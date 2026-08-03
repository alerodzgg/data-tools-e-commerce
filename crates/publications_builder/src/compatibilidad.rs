//! Orquestación de 'Compatibilidades'/'Comprimidas': JOIN INVERTIDO en
//! streaming entre Hoja1 y las hojas de compatibilidad, más el motor de I/O
//! particionado (dedup GLOBAL de 'Combinada' vía
//! `commerce_core::AcumuladorParticionado`). El parsing puro vive en
//! [`parsing`] y las reglas de negocio de filtrado en [`filtros`].

use std::collections::{HashMap, HashSet};
use std::ops::Not;
use std::path::Path;

use commerce_core::{AcumuladorParticionado, CoreResult, EscritorXlsx};
use polars::prelude::*;

use crate::comunes::columna_texto;
use crate::constantes::{verificar_columnas_reservadas, COL_PRECIO, COL_START_URL};

mod filtros;
mod parsing;

pub use filtros::aplicar_filtros_combinada;
pub use parsing::{
    aplicar_sku_secuencial, columnas_a_combinar, limpiar_hoja_compat, limpiar_precio_hoja1,
    preprocesar_hoja1, procesar_dataframe_compatibilidad,
};

use filtros::{escribir_bucket_procesadas, explotar_coincidencia};

/// Modo de 'Compatibilidades' (el menú lo llama "Compatibilidades") vs
/// 'Comprimidas'. Es un enum y no literales de texto sueltos porque esos
/// literales viajan por 6 funciones distintas y el compilador no detecta un
/// typo en ninguna: un literal mal escrito cae en silencio en la rama por
/// defecto de cada `if`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModoCompatibilidad {
    /// Menú "Compatibilidades": conserva 'Linea' salvo que se pida borrarla.
    Repetidas,
    /// Menú "Comprimidas": agrupa coincidencias en una sola fila con
    /// columna 'Coincidencia'.
    Comprimidas,
}

/// Cuántas particiones de disco usa el dedup GLOBAL de 'Combinada' (ver
/// [`commerce_core::AcumuladorParticionado`]). Compatibilidades puede sumar
/// decenas de millones de filas repartidas en muchas hojas/bloques: sin
/// particionar, el dedup por menor 'Precio2' era solo LOCAL a cada
/// sub-bloque de 250k filas — dos filas con la MISMA 'Combinada' que caían
/// en bloques u hojas distintas nunca se comparaban entre sí y ambas
/// sobrevivían.
const PARTICIONES_DEDUP_COMBINADA: usize = 16;
const FILAS_BUFFER_PARTICION_COMBINADA: usize = 1_000_000;

const FILAS_POR_SUBBLOQUE: usize = 250_000;
const FILAS_PRE_EXPLODE: usize = 20_000;

/// Contexto compartido por `procesar_unido`/`despachar_unido`: agrupa lo que
/// siempre viaja junto entre sub-bloques de un mismo archivo. Como struct y
/// no como 6 parámetros sueltos por función: silenciar
/// `clippy::too_many_arguments` con un `allow` tapa la señal en vez de
/// resolverla.
struct ContextoUnido<'a> {
    columnas_combinar: &'a [String],
    modo: ModoCompatibilidad,
    modificar_oem: bool,
    escritor: &'a mut EscritorXlsx,
    contador_sku: &'a mut HashMap<String, u64>,
    acumulador: &'a mut AcumuladorParticionado,
}

/// Procesa un lote YA UNIDO (producto×compatibilidad): explota, filtra y
/// escribe. El explode corre por REBANADAS (`FILAS_PRE_EXPLODE`) y su
/// resultado se procesa en SUB-BLOQUES (`FILAS_POR_SUBBLOQUE`): pico de RAM
/// acotado con independencia del factor de explosión.
fn procesar_unido(chunk_unido: &DataFrame, ctx: &mut ContextoUnido) -> CoreResult<()> {
    if ctx.modo == ModoCompatibilidad::Comprimidas && chunk_unido.column("Coincidencia").is_ok() {
        let n = chunk_unido.height();
        let mut ini = 0;
        while ini < n {
            let tomar = FILAS_PRE_EXPLODE.min(n - ini);
            let sub = chunk_unido.slice(ini as i64, tomar);
            let sub = explotar_coincidencia(&sub)?;
            let m = sub.height();
            let mut i2 = 0;
            while i2 < m {
                let tomar2 = FILAS_POR_SUBBLOQUE.min(m - i2);
                let bloque = sub.slice(i2 as i64, tomar2);
                despachar_unido(&bloque, ctx)?;
                i2 += tomar2;
            }
            ini += tomar;
        }
        return Ok(());
    }

    let n = chunk_unido.height();
    let mut ini = 0;
    while ini < n {
        let tomar = FILAS_POR_SUBBLOQUE.min(n - ini);
        let bloque = chunk_unido.slice(ini as i64, tomar);
        despachar_unido(&bloque, ctx)?;
        ini += tomar;
    }
    Ok(())
}

/// Procesa un sub-bloque ya unido+explotado. Filas con 'Coincidencia' vacía
/// → `No_Procesadas` (sin compatibilidad real, se escribe directo). Las
/// demás pasan los filtros y se acumulan en `acumulador` (dedup GLOBAL por
/// 'Combinada' al final, no una escritura directa a "Procesadas").
fn despachar_unido(sub: &DataFrame, ctx: &mut ContextoUnido) -> CoreResult<()> {
    let mut sub = sub.clone();
    if sub.column("Coincidencia").is_ok() {
        let coincidencia = columna_texto(&sub, "Coincidencia")?;
        let mask_vacia: Vec<bool> = coincidencia
            .iter()
            .map(|v| v.as_deref().map(|s| s.trim().is_empty()).unwrap_or(true))
            .collect();
        let mask_vacia_ca = BooleanChunked::from_iter_values("m".into(), mask_vacia.iter().copied());
        let sub_vacias = sub.filter(&mask_vacia_ca)?;
        let mask_no_vacia: BooleanChunked = mask_vacia_ca.not();
        sub = sub.filter(&mask_no_vacia)?;

        if sub_vacias.height() > 0 {
            let mut df_vacias = procesar_dataframe_compatibilidad(
                &sub_vacias,
                ctx.columnas_combinar,
                ctx.modificar_oem,
                Some(ctx.contador_sku),
            )?;
            if df_vacias.column(COL_PRECIO).is_ok() {
                df_vacias = df_vacias.drop(COL_PRECIO)?;
            }
            ctx.escritor.escribir(&df_vacias, Some("No_Procesadas"))?;
        }
    }

    let mut df_proc = procesar_dataframe_compatibilidad(
        &sub,
        ctx.columnas_combinar,
        ctx.modificar_oem,
        Some(ctx.contador_sku),
    )?;
    df_proc = aplicar_filtros_combinada(&df_proc, ctx.modo, ctx.escritor)?;
    if df_proc.column(COL_PRECIO).is_ok() {
        df_proc = df_proc.drop(COL_PRECIO)?;
    }
    ctx.acumulador.agregar(&df_proc, "Combinada")?;
    Ok(())
}

/// `true` si `hoja` tiene el formato `HojaN` (N = uno o más dígitos), sin
/// distinguir mayúsculas/minúsculas ni espacios — el nombre que genera el
/// scraper para las hojas de compatibilidad (Hoja2, Hoja3…). Debe ser un
/// match de forma completa: `contains("Hoja")` aceptaría "HojaResumen".
fn es_hoja_de_compatibilidad(hoja: &str) -> bool {
    let normalizada = hoja.trim().to_lowercase();
    normalizada
        .strip_prefix("hoja")
        .is_some_and(|resto| !resto.is_empty() && resto.chars().all(|c| c.is_ascii_digit()))
}

/// Agrupa la config/recursos de `iter_bloques_compat` que viajan juntos de
/// una sola vez — mismo criterio que [`ContextoUnido`], que ya resolvió esta
/// misma señal (`clippy::too_many_arguments`) para `procesar_unido`/
/// `despachar_unido`.
struct ContextoIterCompat<'a> {
    archivo_entrada: &'a Path,
    sheet_names: &'a [String],
    modo: ModoCompatibilidad,
    borrar_linea: bool,
    chunk_size: usize,
    libro: &'a mut commerce_core::LibroXlsx,
}

/// Itera las hojas de compatibilidad (Hoja2, Hoja3…) por BLOQUES ya
/// limpios, deduplicados dentro de cada bloque. Solo una hoja vive en RAM a
/// la vez: las compatibilidades pueden sumar decenas de millones de filas
/// repartidas en muchas hojas sin saturar la memoria.
fn iter_bloques_compat(
    ctx: &mut ContextoIterCompat,
    avisar: &mut (impl FnMut(&str) + ?Sized),
    mut on_chunk: impl FnMut(DataFrame) -> CoreResult<()>,
) -> CoreResult<()> {
    for hoja in ctx.sheet_names {
        if hoja.trim().eq_ignore_ascii_case("hoja1") {
            continue;
        }
        if !es_hoja_de_compatibilidad(hoja) {
            avisar(&format!(
                "Se ignora la hoja '{hoja}': no tiene el formato esperado 'HojaN' de compatibilidad."
            ));
            continue;
        }
        let df_compat = match commerce_core::leer_hoja_por_nombre(ctx.libro, ctx.archivo_entrada, hoja) {
            Ok(df) => df,
            Err(error) => {
                avisar(&format!("Error leyendo compatibilidad '{hoja}': {error}"));
                continue;
            }
        };
        verificar_columnas_reservadas(
            df_compat.get_column_names().iter().map(|s| s.as_str()),
            &format!("La hoja '{hoja}'"),
        )?;
        let df_compat = limpiar_hoja_compat(&df_compat, ctx.modo, ctx.borrar_linea)?;
        let n = df_compat.height();
        let mut inicio = 0;
        while inicio < n {
            let tomar = ctx.chunk_size.min(n - inicio);
            let bloque = df_compat.slice(inicio as i64, tomar);
            let bloque = bloque.unique::<(), ()>(None, UniqueKeepStrategy::Any, None)?;
            on_chunk(bloque)?;
            inicio += tomar;
        }
    }
    Ok(())
}

/// Orquesta 'Compatibilidades'/'Comprimidas' con un **JOIN INVERTIDO en
/// streaming**: Hoja1 (≤1M por el límite de Excel) vive en RAM como lado
/// del join, y las compatibilidades se streamean por bloques uniéndose
/// contra Hoja1 — memoria plana, listo para 20M+ filas de compatibilidad.
///
/// `borrar_linea`: en modo 'repetidas', si se debe borrar la columna Y los
/// valores de 'Linea' (la pregunta al usuario es responsabilidad de la
/// interfaz; aquí llega ya resuelta).
///
/// Si el archivo no tiene 'Hoja1', avisa, no escribe nada y devuelve `false`
/// (no `true`: no hay ningún "procesamiento completado" que anunciar). Un
/// error a mitad de proceso no deja una salida a medias: se aborta el
/// escritor.
pub fn ejecutar_procesamiento_compatibilidad(
    archivo_entrada: &Path,
    archivo_salida: &Path,
    modo: ModoCompatibilidad,
    modificar_oem: bool,
    borrar_linea: bool,
    mut avisar: impl FnMut(&str),
    mut progreso: impl FnMut(u64),
) -> CoreResult<bool> {
    let mut libro = commerce_core::abrir_libro(archivo_entrada)?;
    let sheet_names = commerce_core::nombres_hojas_libro(&libro);

    // Case/espacio-insensible, igual que `es_hoja_de_compatibilidad` para
    // Hoja2/Hoja3…: el scraper puede emitir "hoja1" en minúscula, y sin esta
    // normalización el archivo entero quedaría sin procesar.
    let Some(nombre_hoja1) = sheet_names
        .iter()
        .find(|h| h.trim().eq_ignore_ascii_case("hoja1"))
    else {
        avisar("No se encontró la 'Hoja1' en el archivo.");
        return Ok(false);
    };

    let df_hoja1_crudo = commerce_core::leer_hoja_por_nombre(&mut libro, archivo_entrada, nombre_hoja1)?;
    verificar_columnas_reservadas(
        df_hoja1_crudo.get_column_names().iter().map(|s| s.as_str()),
        "'Hoja1'",
    )?;
    let (df_hoja1, df_eliminadas) = preprocesar_hoja1(&df_hoja1_crudo)?;
    let hoja1_cols: Vec<String> = df_hoja1
        .get_column_names()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let columnas_combinar = columnas_a_combinar(modo, borrar_linea);

    crate::escritor::ejecutar_con_escritor(archivo_salida, &mut avisar, |escritor, avisar| {
        const CHUNK_SIZE: usize = 100_000;
        let mut urls_matcheadas: HashSet<String> = HashSet::new();
        let tiene_url = df_hoja1.column(COL_START_URL).is_ok();
        let mut contador_sku: HashMap<String, u64> = HashMap::new();
        let mut acumulador =
            AcumuladorParticionado::nuevo(PARTICIONES_DEDUP_COMBINADA, FILAS_BUFFER_PARTICION_COMBINADA)?;

        if tiene_url {
            iter_bloques_compat(
                &mut ContextoIterCompat {
                    archivo_entrada,
                    sheet_names: &sheet_names,
                    modo,
                    borrar_linea,
                    chunk_size: CHUNK_SIZE,
                    libro: &mut libro,
                },
                &mut *avisar,
                |bloque| {
                    let filas = bloque.height() as u64;
                    if bloque.column(COL_START_URL).is_ok() {
                        let unido = bloque.join(
                            &df_hoja1,
                            [COL_START_URL],
                            [COL_START_URL],
                            JoinArgs::new(JoinType::Inner),
                            None,
                        )?;
                        if unido.height() > 0 {
                            for v in columna_texto(&unido, COL_START_URL)?.into_iter().flatten() {
                                urls_matcheadas.insert(v);
                            }
                            let extras: Vec<String> = unido
                                .get_column_names()
                                .iter()
                                .map(|s| s.to_string())
                                .filter(|c| !hoja1_cols.contains(c))
                                .collect();
                            let orden: Vec<&str> = hoja1_cols
                                .iter()
                                .map(String::as_str)
                                .filter(|c| unido.column(c).is_ok())
                                .chain(extras.iter().map(String::as_str))
                                .collect();
                            let unido = unido.select(orden)?;
                            procesar_unido(
                                &unido,
                                &mut ContextoUnido {
                                    columnas_combinar: &columnas_combinar,
                                    modo,
                                    modificar_oem,
                                    escritor,
                                    contador_sku: &mut contador_sku,
                                    acumulador: &mut acumulador,
                                },
                            )?;
                        }
                    }
                    progreso(filas);
                    Ok(())
                },
            )?;
        }

        let n = df_hoja1.height();
        let mut inicio = 0;
        while inicio < n {
            let tomar = CHUNK_SIZE.min(n - inicio);
            let mut sub = df_hoja1.slice(inicio as i64, tomar);
            if tiene_url && !urls_matcheadas.is_empty() {
                let urls = columna_texto(&sub, COL_START_URL)?;
                let mask: Vec<bool> = urls
                    .iter()
                    .map(|v| v.as_deref().map(|s| !urls_matcheadas.contains(s)).unwrap_or(true))
                    .collect();
                let mask_ca = BooleanChunked::from_iter_values("m".into(), mask.iter().copied());
                sub = sub.filter(&mask_ca)?;
            }
            if sub.height() > 0 {
                let mut df_sin = procesar_dataframe_compatibilidad(
                    &sub,
                    &columnas_combinar,
                    modificar_oem,
                    Some(&mut contador_sku),
                )?;
                if df_sin.column(COL_PRECIO).is_ok() {
                    df_sin = df_sin.drop(COL_PRECIO)?;
                }
                escritor.escribir(&df_sin, Some("No_Procesadas"))?;
            }
            progreso(tomar as u64);
            inicio += tomar;
        }

        acumulador.finalizar(|bucket| escribir_bucket_procesadas(bucket, escritor, &mut *avisar))?;
        escritor.escribir(&df_eliminadas, Some("Eliminadas"))?;
        Ok(())
    })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{escribir_libro, leer_hojas};

    #[test]
    fn e2e_comprimidas_explode_y_no_procesadas() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("comp.xlsx");
        escribir_libro(
            &entrada,
            &[
                (
                    "Hoja1",
                    vec![
                        vec![
                            "web-scraper-start-url",
                            "Precio",
                            "Opcion",
                            "Titulo",
                            "Tienda",
                            "Imagen 1",
                            "Imagen 2",
                            "Imagen 3",
                            "Imagen 4",
                        ],
                        vec![
                            "https://e.com/itm/111",
                            "$85.99",
                            "",
                            "Pair Kit Bumper",
                            "TiendaX",
                            "https://c.com/1.jpg",
                            "",
                            "",
                            "",
                        ],
                        vec![
                            "https://e.com/itm/222",
                            "$20.00",
                            "",
                            "Set Grille",
                            "TiendaX",
                            "https://c.com/2.jpg",
                            "",
                            "",
                            "",
                        ],
                        vec![
                            "https://e.com/itm/333",
                            "$30.00",
                            "opcion-llena",
                            "T3",
                            "TiendaX",
                            "https://c.com/3.jpg",
                            "",
                            "",
                            "",
                        ],
                    ],
                ),
                (
                    "Hoja2",
                    vec![
                        vec!["web-scraper-start-url", "Coincidencia", "Traducido"],
                        vec!["https://e.com/itm/111", "CIVIC 2001@CIVIC 2002", "Defensa"],
                    ],
                ),
            ],
        );

        let salida = tmp.path().join("comp_out.xlsx");
        ejecutar_procesamiento_compatibilidad(
            &entrada,
            &salida,
            ModoCompatibilidad::Comprimidas,
            false,
            false,
            |_| {},
            |_| {},
        )?;

        let hojas = leer_hojas(&salida);
        let procesadas = &hojas["Procesadas"];
        assert_eq!(
            procesadas.height(),
            2,
            "el explode genera una fila por coincidencia"
        );

        let mut combinadas: Vec<_> = procesadas
            .column("Combinada")?
            .str()?
            .iter()
            .map(|v| v.unwrap().to_string())
            .collect();
        combinadas.sort();
        assert_eq!(
            combinadas,
            vec!["Kit Defensa CIVIC 2001", "Kit Defensa CIVIC 2002"]
        );

        let mut skus: Vec<_> = procesadas
            .column("Sku")?
            .str()?
            .iter()
            .map(|v| v.unwrap().to_string())
            .collect();
        skus.sort();
        assert_eq!(
            skus,
            vec!["u-85-tiendax-111-1", "u-85-tiendax-111-2"],
            "SKU secuencial único"
        );

        assert_eq!(hojas["No_Procesadas"].height(), 1, "la fila sin compatibilidad");
        assert_eq!(hojas["Eliminadas"].height(), 1, "la fila con Opcion llena");
        Ok(())
    }

    #[test]
    fn e2e_repetidas_borra_linea_y_depura_litros_en_modelo() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("rep.xlsx");
        escribir_libro(
            &entrada,
            &[
                (
                    "Hoja1",
                    vec![
                        vec!["web-scraper-start-url", "Precio", "Titulo", "Tienda"],
                        vec!["https://e.com/itm/111", "$50.00", "Product X", "TiendaY"],
                    ],
                ),
                (
                    "Hoja2",
                    vec![
                        vec!["web-scraper-start-url", "Linea", "Modelo", "Marca", "Chasis"],
                        vec![
                            "https://e.com/itm/111",
                            "L1--L2",
                            "CIVIC 2.0T SEDAN",
                            "Honda",
                            "ChasisX",
                        ],
                    ],
                ),
            ],
        );

        let salida = tmp.path().join("rep_out.xlsx");
        ejecutar_procesamiento_compatibilidad(
            &entrada,
            &salida,
            ModoCompatibilidad::Repetidas,
            false,
            true,
            |_| {},
            |_| {},
        )?;

        let hojas = leer_hojas(&salida);
        let procesadas = &hojas["Procesadas"];
        assert_eq!(procesadas.height(), 1);
        assert!(
            procesadas.column("Linea").is_err(),
            "borrar_linea=true debe quitar la columna 'Linea' del resultado"
        );
        assert_eq!(
            procesadas.column("Modelo")?.str()?.get(0),
            Some("CIVIC SEDAN"),
            "el patrón de litros embebido en Modelo (p. ej. '2.0T') se depura al borrar Linea"
        );
        Ok(())
    }

    #[test]
    fn dedup_de_combinada_es_global_entre_hojas_no_solo_local_por_sub_bloque() -> CoreResult<()> {
        // El dedup de 'Combinada' es GLOBAL: cada hoja de compatibilidad se
        // procesa en su propia invocación, así que dos productos distintos
        // que resuelven a la MISMA 'Combinada' por hojas distintas deben
        // compararse igual entre sí. `AcumuladorParticionado` los reúne por
        // hash y deduplica al final; sobrevive solo el de menor Precio2.
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("dup_entre_hojas.xlsx");
        escribir_libro(
            &entrada,
            &[
                (
                    "Hoja1",
                    vec![
                        vec!["web-scraper-start-url", "Precio", "Titulo", "Tienda"],
                        vec!["https://e.com/itm/barato", "$10.00", "Product X", "TiendaY"],
                        vec!["https://e.com/itm/caro", "$40.00", "Product X", "TiendaY"],
                    ],
                ),
                (
                    "Hoja2",
                    vec![
                        vec!["web-scraper-start-url", "Marca", "Chasis", "Modelo", "Linea"],
                        vec!["https://e.com/itm/barato", "Honda", "ChasisX", "Civic", "L1"],
                    ],
                ),
                (
                    "Hoja3",
                    vec![
                        vec!["web-scraper-start-url", "Marca", "Chasis", "Modelo", "Linea"],
                        vec!["https://e.com/itm/caro", "Honda", "ChasisX", "Civic", "L1"],
                    ],
                ),
            ],
        );

        let salida = tmp.path().join("dup_entre_hojas_out.xlsx");
        ejecutar_procesamiento_compatibilidad(
            &entrada,
            &salida,
            ModoCompatibilidad::Repetidas,
            false,
            false,
            |_| {},
            |_| {},
        )?;

        let hojas = leer_hojas(&salida);
        let procesadas = &hojas["Procesadas"];
        assert_eq!(
            procesadas.height(),
            1,
            "las dos filas resuelven a la misma 'Combinada' vía hojas distintas: debe sobrevivir solo 1"
        );
        assert_eq!(
            procesadas.column("web-scraper-start-url")?.str()?.get(0),
            Some("https://e.com/itm/barato"),
            "debe sobrevivir la de menor Precio2 (el producto más barato)"
        );
        Ok(())
    }

    #[test]
    fn hoja_de_compatibilidad_en_minuscula_se_procesa_igual() -> CoreResult<()> {
        // El scraper puede emitir los nombres de hoja en minúscula.
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("minuscula.xlsx");
        escribir_libro(
            &entrada,
            &[
                (
                    "Hoja1",
                    vec![
                        vec!["web-scraper-start-url", "Precio", "Titulo", "Tienda"],
                        vec!["https://e.com/itm/111", "$50.00", "Product X", "TiendaY"],
                    ],
                ),
                (
                    "hoja2",
                    vec![
                        vec!["web-scraper-start-url", "Marca", "Chasis", "Modelo"],
                        vec!["https://e.com/itm/111", "Honda", "ChasisX", "CIVIC"],
                    ],
                ),
            ],
        );

        let salida = tmp.path().join("minuscula_out.xlsx");
        ejecutar_procesamiento_compatibilidad(
            &entrada,
            &salida,
            ModoCompatibilidad::Repetidas,
            false,
            false,
            |_| {},
            |_| {},
        )?;

        let hojas = leer_hojas(&salida);
        assert_eq!(
            hojas["Procesadas"].height(),
            1,
            "'hoja2' en minúscula debe tratarse igual que 'Hoja2'"
        );
        Ok(())
    }

    #[test]
    fn hoja1_en_minuscula_se_procesa_igual() -> CoreResult<()> {
        // La hoja principal en minúscula debe encontrarse igual: si no, el
        // archivo no se procesa y el aviso culpa a una hoja que sí existe.
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("hoja1_minuscula.xlsx");
        escribir_libro(
            &entrada,
            &[(
                "hoja1",
                vec![
                    vec!["web-scraper-start-url", "Precio", "Titulo", "Tienda"],
                    vec!["https://e.com/itm/111", "$50.00", "Product X", "TiendaY"],
                ],
            )],
        );

        let salida = tmp.path().join("hoja1_minuscula_out.xlsx");
        let hizo_algo = ejecutar_procesamiento_compatibilidad(
            &entrada,
            &salida,
            ModoCompatibilidad::Repetidas,
            false,
            false,
            |_| {},
            |_| {},
        )?;

        assert!(hizo_algo, "'hoja1' en minúscula debe tratarse igual que 'Hoja1'");
        let hojas = leer_hojas(&salida);
        // Sin ninguna hoja de compatibilidad (Hoja2, Hoja3…) que matchee,
        // la fila de Hoja1 va a "No_Procesadas" — lo relevante acá es que
        // `hizo_algo` sea `true` y que exista una salida real, no `false`
        // con el aviso engañoso de "no se encontró Hoja1".
        assert_eq!(hojas["No_Procesadas"].height(), 1);
        Ok(())
    }

    #[test]
    fn hoja_con_nombre_no_estandar_se_ignora_con_aviso() -> CoreResult<()> {
        // Una hoja cuyo nombre solo CONTIENE "Hoja" no es de compatibilidad:
        // unirla contra Hoja1 mezclaría datos ajenos en el resultado.
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("no_estandar.xlsx");
        escribir_libro(
            &entrada,
            &[
                (
                    "Hoja1",
                    vec![
                        vec!["web-scraper-start-url", "Precio", "Titulo", "Tienda"],
                        vec!["https://e.com/itm/111", "$50.00", "Product X", "TiendaY"],
                    ],
                ),
                (
                    "HojaResumen",
                    vec![
                        vec!["web-scraper-start-url", "Marca"],
                        vec!["https://e.com/itm/111", "Honda"],
                    ],
                ),
            ],
        );

        let salida = tmp.path().join("no_estandar_out.xlsx");
        let avisos = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let avisos_clon = avisos.clone();
        ejecutar_procesamiento_compatibilidad(
            &entrada,
            &salida,
            ModoCompatibilidad::Repetidas,
            false,
            false,
            move |m: &str| avisos_clon.borrow_mut().push(m.to_string()),
            |_| {},
        )?;

        assert!(
            avisos.borrow().iter().any(|m| m.contains("HojaResumen")),
            "debe avisar que 'HojaResumen' se ignoró: {:?}",
            avisos.borrow()
        );
        let hojas = leer_hojas(&salida);
        assert_eq!(
            hojas["No_Procesadas"].height(),
            1,
            "sin datos de compatibilidad real, la fila de Hoja1 va a No_Procesadas"
        );
        Ok(())
    }

    #[test]
    fn hoja1_ausente_avisa_y_no_escribe_nada() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("sin_hoja1.xlsx");
        escribir_libro(
            &entrada,
            &[(
                "Hoja2",
                vec![vec!["web-scraper-start-url"], vec!["https://e.com/itm/1"]],
            )],
        );
        let salida = tmp.path().join("sin_hoja1_out.xlsx");

        let avisos = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let avisos_clon = avisos.clone();
        let hizo_algo = ejecutar_procesamiento_compatibilidad(
            &entrada,
            &salida,
            ModoCompatibilidad::Repetidas,
            false,
            false,
            move |m: &str| avisos_clon.borrow_mut().push(m.to_string()),
            |_| {},
        )?;

        assert!(
            !hizo_algo,
            "debe devolver false: no hay ningún 'procesamiento completado' que anunciar"
        );
        assert!(
            avisos.borrow().iter().any(|m| m.contains("Hoja1")),
            "debe avisar que falta 'Hoja1': {:?}",
            avisos.borrow()
        );
        assert!(
            !salida.exists(),
            "no debe crear ningún archivo de salida si falta 'Hoja1'"
        );
        Ok(())
    }

    #[test]
    fn columna_reservada_en_hoja_de_compatibilidad_aborta_sin_dejar_salida() {
        // El chequeo de columnas reservadas se aplicaba a 'Hoja1' pero no a
        // las hojas de compatibilidad (Hoja2, Hoja3…): una columna llamada
        // 'Combinada' ahí se sobrescribía en silencio en vez de avisar.
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("res_compat.xlsx");
        escribir_libro(
            &entrada,
            &[
                (
                    "Hoja1",
                    vec![
                        vec!["web-scraper-start-url", "Precio", "Titulo", "Tienda"],
                        vec!["https://e.com/itm/111", "$50.00", "Product X", "TiendaY"],
                    ],
                ),
                (
                    "Hoja2",
                    vec![
                        vec!["web-scraper-start-url", "Marca", "Combinada"],
                        vec!["https://e.com/itm/111", "Honda", "algo"],
                    ],
                ),
            ],
        );
        let salida = tmp.path().join("res_compat_out.xlsx");

        let resultado = ejecutar_procesamiento_compatibilidad(
            &entrada,
            &salida,
            ModoCompatibilidad::Repetidas,
            false,
            false,
            |_| {},
            |_| {},
        );

        assert!(
            resultado.is_err(),
            "una columna reservada en Hoja2 debe abortar el procesamiento"
        );
        assert!(!salida.exists(), "no debe quedar una salida a medias");
    }

    #[test]
    fn progreso_suma_el_total_de_filas_de_todas_las_hojas_con_url() {
        // El progreso debe contar TAMBIÉN las filas de Hoja1 que van a
        // "No_Procesadas", no solo las de las hojas de compatibilidad.
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("progreso.xlsx");
        escribir_libro(
            &entrada,
            &[
                (
                    "Hoja1",
                    vec![
                        vec!["web-scraper-start-url", "Precio", "Titulo", "Tienda"],
                        vec!["https://e.com/itm/111", "$50.00", "Product X", "TiendaY"],
                        vec!["https://e.com/itm/222", "$60.00", "Product Y", "TiendaY"],
                    ],
                ),
                (
                    "Hoja2",
                    vec![
                        vec!["web-scraper-start-url", "Marca"],
                        vec!["https://e.com/itm/111", "Honda"],
                    ],
                ),
            ],
        );
        let salida = tmp.path().join("progreso_out.xlsx");

        let total = std::cell::Cell::new(0u64);
        ejecutar_procesamiento_compatibilidad(
            &entrada,
            &salida,
            ModoCompatibilidad::Repetidas,
            false,
            false,
            |_| {},
            |n| total.set(total.get() + n),
        )
        .unwrap();

        assert_eq!(
            total.get(),
            3,
            "2 filas de Hoja1 + 1 fila de Hoja2 = 3 avances de progreso en total"
        );
    }

    #[test]
    fn progreso_avanza_aunque_hoja1_no_tenga_columna_de_url() {
        // Sin `web-scraper-start-url` el loop de compatibilidad no corre:
        // el progreso debe avanzar igual con las filas de Hoja1.
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("sin_url.xlsx");
        escribir_libro(
            &entrada,
            &[(
                "Hoja1",
                vec![
                    vec!["Precio", "Titulo", "Tienda"],
                    vec!["$50.00", "Product X", "TiendaY"],
                    vec!["$60.00", "Product Y", "TiendaY"],
                ],
            )],
        );
        let salida = tmp.path().join("sin_url_out.xlsx");

        let total = std::cell::Cell::new(0u64);
        ejecutar_procesamiento_compatibilidad(
            &entrada,
            &salida,
            ModoCompatibilidad::Repetidas,
            false,
            false,
            |_| {},
            |n| total.set(total.get() + n),
        )
        .unwrap();

        assert_eq!(
            total.get(),
            2,
            "las 2 filas de Hoja1 deben contar igual sin columna de URL"
        );
    }
}
