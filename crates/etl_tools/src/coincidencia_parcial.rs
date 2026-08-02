use std::collections::HashSet;
use std::path::Path;

use aho_corasick::{AhoCorasick, MatchKind};
use commerce_core::{concatenar_vertical, CoreResult};
use polars::prelude::*;
use regex::Regex;
use std::sync::LazyLock;

use crate::duplicados::quitar_tildes;
use crate::lectura::{
    columna_texto_o_vacia, con_clave_normalizada, iter_hojas_valores, preparar_chunk_clave,
};

/// Umbral de deduplicación adaptativa: si los textos DISTINTOS de un lote son
/// ≤ este % de las filas (títulos repetidos, lo normal en publicaciones),
/// sale más barato normalizar/cruzar/resolver SOLO los distintos y repartir
/// el resultado con un join.
pub const UMBRAL_DEDUP_PARCIAL: f64 = 0.6;

/// Qué hacer cuando VARIAS claves de la Tabla coinciden en el mismo texto de
/// la Base — antes viajaba como `&str` ("larga"/"primera"/"todas") por 3
/// funciones, con cualquier string no reconocido cayendo en silencio en
/// "primera" (el brazo `_` del match, sin que el compilador pudiera avisar
/// de un typo en ningún llamador).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpcionMultiple {
    /// La coincidencia más larga (más específica).
    Larga,
    /// La primera en el orden de la Tabla.
    Primera,
    /// Todas, unidas con salto de línea.
    Todas,
}

static RE_NO_ALFANUM_PARCIAL: LazyLock<Regex> =
    LazyLock::new(|| commerce_core::regex_literal(r"[^0-9a-zñ]+"));

/// Texto normalizado para match parcial: minúsculas, tildes transliteradas
/// (`ñ` conservada, ver [`quitar_tildes`]), símbolos→espacio (colapsado), sin
/// padding. A diferencia del exacto (que quita TODO lo no alfanumérico),
/// aquí los espacios se CONSERVAN: son los límites de palabra que usa el
/// motor Aho-Corasick para no casar dentro de otra palabra.
pub fn norm_parcial(texto: &str) -> String {
    let minuscula = quitar_tildes(&texto.to_lowercase());
    RE_NO_ALFANUM_PARCIAL
        .replace_all(&minuscula, " ")
        .trim()
        .to_string()
}

/// Construye una columna `List[String]` a partir de un `Vec` de filas (cada
/// una, sus valores de lista). `Series: NamedFrom<_, ListType>` exige
/// `Vec<Series>`, no `Vec<Vec<String>>` directamente.
fn columna_lista_de_texto(nombre: &str, filas: Vec<Vec<String>>) -> Column {
    let series_por_fila: Vec<Series> = filas
        .into_iter()
        .map(|fila| Series::new(PlSmallStr::EMPTY, fila))
        .collect();
    Series::new(nombre.into(), series_por_fila).into()
}

fn reducir_parcial(t: &DataFrame, columnas_traer: &[String], unir_multiples: bool) -> CoreResult<DataFrame> {
    if unir_multiples {
        let mut aggs = vec![col("_rank").min()];
        for c in columnas_traer {
            aggs.push(
                col(c.as_str())
                    .fill_null(lit(""))
                    .str()
                    .join("\n", true)
                    .alias(c.as_str()),
            );
        }
        Ok(t.clone()
            .lazy()
            .group_by_stable([col("clave_norm")])
            .agg(aggs)
            .collect()?)
    } else {
        Ok(t.unique_stable(Some(&["clave_norm".to_string()]), UniqueKeepStrategy::First, None)?)
    }
}

/// Carga la Tabla para el cruce parcial: mapa `[clave_norm, _rank, ...columnas_traer]`.
/// `_rank` = orden de aparición en la Tabla (para la opción "la primera").
/// Se reduce por hoja (memoria O(claves distintas)) y luego entre hojas.
/// `unir_multiples` junta con "\n" los valores de una MISMA clave normalizada.
pub fn cargar_tabla_parcial(
    ruta_tabla: &Path,
    clave_tabla: &str,
    columnas_traer: &[String],
    unir_multiples: bool,
    excluir: Option<&[&str]>,
    avisar: impl FnMut(&str),
) -> CoreResult<DataFrame> {
    let mut columnas_necesarias = vec![clave_tabla.to_string()];
    columnas_necesarias.extend(columnas_traer.iter().cloned());

    let chunks = iter_hojas_valores(std::slice::from_ref(&ruta_tabla.to_path_buf()), excluir, avisar)?;
    let mut partes: Vec<DataFrame> = Vec::new();
    let mut n: u64 = 0;

    for chunk in chunks {
        let preparado = preparar_chunk_clave(&chunk, &columnas_necesarias, clave_tabla)?;
        let Some(mut sub) =
            con_clave_normalizada(&preparado, clave_tabla, columnas_traer, "clave_norm", |v| {
                norm_parcial(v.unwrap_or(""))
            })?
        else {
            continue;
        };
        let alto = sub.height() as u64;
        sub.with_column(Column::new("_rank".into(), (n..n + alto).collect::<Vec<u64>>()))?;
        n += alto;
        partes.push(reducir_parcial(&sub, columnas_traer, unir_multiples)?);
    }

    if partes.is_empty() {
        let mut cols = vec![
            Column::new("clave_norm".into(), Vec::<String>::new()),
            Column::new("_rank".into(), Vec::<u64>::new()),
        ];
        for c in columnas_traer {
            cols.push(Column::new(c.as_str().into(), Vec::<Option<String>>::new()));
        }
        return Ok(DataFrame::new_infer_height(cols)?);
    }
    if let [unica] = partes.as_slice() {
        return Ok(unica.clone());
    }
    reducir_parcial(&concatenar_vertical(partes)?, columnas_traer, unir_multiples)
}

/// Construye el patrón (con relleno de espacios = palabra completa) para
/// cada fila de `lookup` (en el MISMO orden: el índice del patrón = índice
/// de fila = lo que devuelve el automáta).
pub fn construir_automata(lookup: &DataFrame) -> CoreResult<(AhoCorasick, Vec<String>)> {
    let claves: Vec<String> = lookup
        .column("clave_norm")?
        .str()?
        .iter()
        .map(|v| v.unwrap_or("").to_string())
        .collect();
    let patrones: Vec<String> = claves.iter().map(|k| format!(" {k} ")).collect();
    let ac = AhoCorasick::builder()
        .match_kind(MatchKind::Standard)
        .build(&patrones)
        .map_err(|e| commerce_core::CoreError::Io(std::io::Error::other(e)))?;
    Ok((ac, claves))
}

fn extraer_hits(texto_normalizado: &str, ac: &AhoCorasick, claves_originales: &[String]) -> Vec<String> {
    let con_padding = format!(" {texto_normalizado} ");
    ac.find_overlapping_iter(&con_padding)
        .map(|m| claves_originales[m.pattern().as_usize()].clone())
        .collect()
}

/// Resuelve la lista de "hits" (columna `_hits`, lista de `clave_norm`
/// encontradas) de cada fila/texto a las columnas traídas, según `opcion`.
/// Devuelve `[id_col] + salida_traer` (solo ids con coincidencia).
fn resolver_hits(
    df: &DataFrame,
    id_col: &str,
    lookup: &DataFrame,
    salida_traer: &[String],
    opcion: OpcionMultiple,
) -> CoreResult<DataFrame> {
    let opciones_explode = ExplodeOptions {
        empty_as_null: false,
        keep_nulls: false,
    };
    let explotado = df
        .select([id_col, "_hits"])?
        .explode(["_hits"], opciones_explode)?;
    let claves_recortadas: Vec<Option<String>> = explotado
        .column("_hits")?
        .str()?
        .iter()
        .map(|v| v.map(|s| s.trim().to_string()))
        .collect();
    let mut base = explotado.select([id_col])?;
    base.with_column(Column::new("clave_norm".into(), claves_recortadas))?;
    let unido = base.join(
        lookup,
        ["clave_norm"],
        ["clave_norm"],
        JoinArgs::new(JoinType::Left),
        None,
    )?;

    match opcion {
        OpcionMultiple::Todas => {
            let mascara = BooleanChunked::from_iter_values(
                "m".into(),
                unido.column("_rank")?.u64()?.iter().map(|v| v.is_some()),
            );
            let unico = unido.filter(&mascara)?.unique_stable(
                Some(&[id_col.to_string(), "clave_norm".to_string()]),
                UniqueKeepStrategy::First,
                None,
            )?;
            let opciones = SortMultipleOptions::default().with_nulls_last(true);
            let ordenado = unico.sort(["_rank"], opciones)?;
            let mut aggs = Vec::new();
            for c in salida_traer {
                aggs.push(
                    col(c.as_str())
                        .fill_null(lit(""))
                        .str()
                        .join("\n", true)
                        .alias(c.as_str()),
                );
            }
            Ok(ordenado
                .lazy()
                .group_by_stable([col(id_col)])
                .agg(aggs)
                .collect()?)
        }
        OpcionMultiple::Larga => {
            let longitudes: Vec<u32> = unido
                .column("clave_norm")?
                .str()?
                .iter()
                .map(|v| v.unwrap_or("").chars().count() as u32)
                .collect();
            let mut unido = unido;
            unido.with_column(Column::new("_len".into(), longitudes))?;
            // Desempate por `_rank` (menor = antes en la Tabla): el sort NO es
            // estable por defecto, así que sin esta clave dos claves del
            // mismo largo se resolverían de forma no determinista.
            let opciones = SortMultipleOptions::default()
                .with_order_descending_multi([true, false])
                .with_nulls_last(true);
            let ordenado = unido.sort(["_len", "_rank"], opciones)?;
            let unico =
                ordenado.unique_stable(Some(&[id_col.to_string()]), UniqueKeepStrategy::First, None)?;
            let mut cols = vec![id_col.to_string()];
            cols.extend(salida_traer.iter().cloned());
            Ok(unico.select(cols)?)
        }
        OpcionMultiple::Primera => {
            // Menor _rank (orden de la Tabla).
            let opciones = SortMultipleOptions::default().with_nulls_last(true);
            let ordenado = unido.sort(["_rank"], opciones)?;
            let unico =
                ordenado.unique_stable(Some(&[id_col.to_string()]), UniqueKeepStrategy::First, None)?;
            let mut cols = vec![id_col.to_string()];
            cols.extend(salida_traer.iter().cloned());
            Ok(unico.select(cols)?)
        }
    }
}

pub(crate) fn rellenar_texto(df: &mut DataFrame, columnas: &[String]) -> CoreResult<()> {
    for c in columnas {
        let valores: Vec<String> = df
            .column(c)?
            .as_materialized_series()
            .str()?
            .iter()
            .map(|v| v.unwrap_or("").to_string())
            .collect();
        df.with_column(Column::new(c.as_str().into(), valores))?;
    }
    Ok(())
}

/// Cruza un lote de la Base contra la Tabla por CONTENCIÓN de palabra
/// completa (Aho-Corasick). Devuelve (df con las columnas traídas + `_match`,
/// nº de filas con coincidencia).
///
/// Adaptativo: si el lote repite textos, todo el trabajo caro (normalizar,
/// autómata y resolución) corre SOLO sobre los textos distintos y el
/// resultado se reparte con un join; si casi todo es distinto, opera fila a
/// fila. Ambos caminos deben dar EXACTAMENTE el mismo resultado (verificado
/// por test): la deduplicación es solo una optimización.
/// Contra qué se cruza: la tabla de búsqueda ya cargada, su autómata y qué
/// traer de ella. Los cinco viajan siempre juntos y describen una sola cosa,
/// así que van agrupados en vez de sueltos.
pub struct Busqueda<'a> {
    /// Tabla de búsqueda ya normalizada (`cargar_tabla_parcial`).
    pub lookup: &'a DataFrame,
    /// Autómata construido sobre sus claves (`construir_automata`).
    pub ac: &'a AhoCorasick,
    /// Claves en su forma original, para reportar la coincidencia.
    pub claves_originales: &'a [String],
    /// Columnas de la tabla que se traen a la salida.
    pub salida_traer: &'a [String],
    /// Qué hacer si varias claves coinciden en el mismo texto.
    pub opcion: OpcionMultiple,
}

pub fn cruzar_chunk_parcial(
    chunk: &DataFrame,
    clave_base: &str,
    busqueda: &Busqueda,
) -> CoreResult<(DataFrame, usize)> {
    cruzar_chunk_parcial_con_umbral(chunk, clave_base, busqueda, UMBRAL_DEDUP_PARCIAL)
}

/// Igual que [`cruzar_chunk_parcial`], pero con el umbral de deduplicación
/// adaptativa como parámetro explícito (en vez de una constante de módulo
/// mutable): permite forzar cada camino (directo / con dedup) desde los
/// tests sin estado global compartido.
pub fn cruzar_chunk_parcial_con_umbral(
    chunk: &DataFrame,
    clave_base: &str,
    busqueda: &Busqueda,
    umbral_dedup: f64,
) -> CoreResult<(DataFrame, usize)> {
    let Busqueda {
        lookup,
        ac,
        claves_originales,
        salida_traer,
        opcion,
    } = *busqueda;
    let textos_base: Vec<Option<String>> = columna_texto_o_vacia(chunk, clave_base)?;
    let n = textos_base.len();
    let crudos: Vec<String> = textos_base
        .iter()
        .map(|v| v.as_deref().unwrap_or("").to_string())
        .collect();
    let n_dist = crudos.iter().collect::<HashSet<_>>().len();

    let mut salida = if (n_dist as f64) <= (n as f64) * umbral_dedup {
        let mut vistos = HashSet::new();
        let mut distintos: Vec<String> = Vec::new();
        for k in &crudos {
            if vistos.insert(k.clone()) {
                distintos.push(k.clone());
            }
        }
        let hits: Vec<Vec<String>> = distintos
            .iter()
            .map(|k| extraer_hits(&norm_parcial(k), ac, claves_originales))
            .collect();
        let coincide: Vec<bool> = hits.iter().map(|h| !h.is_empty()).collect();

        let mut dist_df = DataFrame::new_infer_height(vec![Column::new("_kraw".into(), distintos.clone())])?;
        dist_df.with_column(columna_lista_de_texto("_hits", hits))?;
        dist_df.with_column(Column::new("_match".into(), coincide))?;

        let resuelto = resolver_hits(&dist_df, "_kraw", lookup, salida_traer, opcion)?;
        let dist_reducido = dist_df.select(["_kraw", "_match"])?.join(
            &resuelto,
            ["_kraw"],
            ["_kraw"],
            JoinArgs::new(JoinType::Left),
            None,
        )?;

        let mut ci = chunk.clone();
        ci.with_column(Column::new("_kraw".into(), crudos))?;
        let out = ci.join(
            &dist_reducido,
            ["_kraw"],
            ["_kraw"],
            JoinArgs::new(JoinType::Left),
            None,
        )?;
        out.drop("_kraw")?
    } else {
        let hits: Vec<Vec<String>> = crudos
            .iter()
            .map(|k| extraer_hits(&norm_parcial(k), ac, claves_originales))
            .collect();
        let coincide: Vec<bool> = hits.iter().map(|h| !h.is_empty()).collect();

        let mut ci = chunk.clone();
        ci.with_column(Column::new("_ri".into(), (0..n as u64).collect::<Vec<u64>>()))?;
        ci.with_column(columna_lista_de_texto("_hits", hits))?;
        ci.with_column(Column::new("_match".into(), coincide))?;

        let resuelto = resolver_hits(&ci, "_ri", lookup, salida_traer, opcion)?;
        let ci2 = ci.drop("_hits")?;
        let out = ci2.join(&resuelto, ["_ri"], ["_ri"], JoinArgs::new(JoinType::Left), None)?;
        out.drop("_ri")?
    };

    rellenar_texto(&mut salida, salida_traer)?;
    // `_match` puede llegar null tras el join (texto sin coincidencia): cuenta como `false`.
    let match_final: Vec<bool> = salida
        .column("_match")?
        .bool()?
        .iter()
        .map(|v| v.unwrap_or(false))
        .collect();
    let n_match = match_final.iter().filter(|v| **v).count();
    salida.with_column(Column::new("_match".into(), match_final))?;

    Ok((salida, n_match))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escribir_libro(ruta: &Path, hojas: &[&[[&str; 2]]]) {
        use rust_xlsxwriter::Workbook;
        let mut wb = Workbook::new();
        for filas in hojas {
            let hoja = wb.add_worksheet();
            for (f, fila) in filas.iter().enumerate() {
                for (c, valor) in fila.iter().enumerate() {
                    hoja.write(f as u32, c as u16, *valor).unwrap();
                }
            }
        }
        wb.save(ruta).unwrap();
    }

    #[test]
    fn norm_parcial_transcribe_tildes_pero_conserva_la_ene() {
        // Mismo criterio que `clave_limpia_de` (ver su test hermano en
        // duplicados.rs): tildes transliteradas para que la misma palabra
        // con/sin tilde matchee en el BUSCARV parcial, pero la `ñ` se
        // conserva porque es una letra distinta, no una vocal acentuada.
        assert_eq!(norm_parcial("Camión"), norm_parcial("Camion"));
        assert_ne!(norm_parcial("Piña"), norm_parcial("Pia"));
        assert_ne!(norm_parcial("año"), norm_parcial("ano"));
    }

    #[test]
    fn cargar_tabla_parcial_avisa_si_el_archivo_no_se_pudo_leer() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("corrupto.xlsx");
        std::fs::write(&archivo, b"esto no es un xlsx").unwrap();

        let avisos = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let avisos_clon = avisos.clone();
        let tabla = cargar_tabla_parcial(
            &archivo,
            "Clave",
            &["Info".to_string()],
            false,
            None,
            move |m: &str| avisos_clon.borrow_mut().push(m.to_string()),
        )?;

        assert_eq!(tabla.height(), 0);
        assert!(
            !avisos.borrow().is_empty(),
            "debe avisar que el archivo no se pudo leer, no quedarse en silencio"
        );
        Ok(())
    }

    #[test]
    fn cargar_tabla_parcial_conserva_primera_coincidencia_por_defecto() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("tabla.xlsx");
        escribir_libro(
            &archivo,
            &[&[
                ["Clave", "Info"],
                ["Hola Mundo", "primero"],
                ["Hola Mundo", "segundo"],
                ["Rojo", "unico"],
            ]],
        );

        let tabla = cargar_tabla_parcial(&archivo, "Clave", &["Info".to_string()], false, None, |_| {})?;
        assert_eq!(tabla.height(), 2);
        let fila = tabla
            .lazy()
            .filter(col("clave_norm").eq(lit("hola mundo")))
            .collect()?;
        assert_eq!(fila.column("Info")?.str()?.get(0), Some("primero"));
        Ok(())
    }

    #[test]
    fn cargar_tabla_parcial_unir_multiples_junta_con_salto_de_linea() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("tabla.xlsx");
        escribir_libro(
            &archivo,
            &[&[["Clave", "Info"], ["Rojo", "uno"], ["Rojo", "dos"]]],
        );

        let tabla = cargar_tabla_parcial(&archivo, "Clave", &["Info".to_string()], true, None, |_| {})?;
        assert_eq!(tabla.height(), 1);
        assert_eq!(tabla.column("Info")?.str()?.get(0), Some("uno\ndos"));
        Ok(())
    }

    #[test]
    fn cargar_tabla_parcial_lee_varias_hojas_y_conserva_orden_de_aparicion_en_rank() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("tabla_multi.xlsx");
        escribir_libro(
            &archivo,
            &[
                &[["Clave", "Info"], ["Rojo", "de_hoja1"]],
                &[["Clave", "Info"], ["Rojo", "de_hoja2"]],
            ],
        );

        // Con "la primera" gana la que aparece antes en orden de lectura
        // (hoja 1), aunque las dos hojas traigan la misma clave normalizada.
        let tabla = cargar_tabla_parcial(&archivo, "Clave", &["Info".to_string()], false, None, |_| {})?;
        assert_eq!(tabla.height(), 1);
        assert_eq!(tabla.column("Info")?.str()?.get(0), Some("de_hoja1"));
        Ok(())
    }

    #[test]
    fn cargar_tabla_parcial_sin_coincidencias_devuelve_tabla_vacia_con_columnas() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("tabla_vacia.xlsx");
        escribir_libro(&archivo, &[&[["Clave", "Info"]]]);

        let tabla = cargar_tabla_parcial(&archivo, "Clave", &["Info".to_string()], false, None, |_| {})?;
        assert_eq!(tabla.height(), 0);
        assert_eq!(
            tabla
                .get_column_names()
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
            vec!["clave_norm", "_rank", "Info"]
        );
        Ok(())
    }

    fn lookup_prueba() -> DataFrame {
        df!(
            "clave_norm" => ["hola mundo", "hola", "arte", "rojo"],
            "_rank" => [0u64, 1, 2, 3],
            "Info" => ["A", "B", "C", "D"],
        )
        .unwrap()
    }

    #[test]
    fn es_por_palabra_completa() -> CoreResult<()> {
        let lookup = lookup_prueba();
        let (ac, claves) = construir_automata(&lookup)?;
        let base = df!("Texto" => ["holanda tulipan", "hola mundo"])?;
        let (salida, _) = cruzar_chunk_parcial(
            &base,
            "Texto",
            &Busqueda {
                lookup: &lookup,
                ac: &ac,
                claves_originales: &claves,
                salida_traer: &["Info".to_string()],
                opcion: OpcionMultiple::Larga,
            },
        )?;
        let info: Vec<_> = salida
            .column("Info")?
            .str()?
            .iter()
            .map(|v| v.unwrap().to_string())
            .collect();
        assert_eq!(info[0], "", "'holanda' no debe casar con 'hola'");
        assert_eq!(info[1], "A", "'hola mundo' debe casar con la clave más larga");
        Ok(())
    }

    #[test]
    fn camino_directo_igual_al_dedup() -> CoreResult<()> {
        let lookup = lookup_prueba();
        let (ac, claves) = construir_automata(&lookup)?;
        let textos = [
            "Hola-Mundo terrenal!",
            "color ROJO intenso",
            "",
            "  ",
            "holanda tulipan",
            "hola y arte juntos",
        ];
        let mut textos_rep = Vec::new();
        let mut ids = Vec::new();
        for _ in 0..200 {
            for t in textos {
                ids.push(ids.len().to_string());
                textos_rep.push(t.to_string());
            }
        }
        let chunk = df!("Texto" => textos_rep, "Id" => ids)?;

        for opcion in [
            OpcionMultiple::Larga,
            OpcionMultiple::Primera,
            OpcionMultiple::Todas,
        ] {
            let (d1, m1) = cruzar_chunk_parcial_con_umbral(
                &chunk,
                "Texto",
                &Busqueda {
                    lookup: &lookup,
                    ac: &ac,
                    claves_originales: &claves,
                    salida_traer: &["Info".to_string()],
                    opcion,
                },
                -1.0, // fuerza camino directo
            )?;
            let (d2, m2) = cruzar_chunk_parcial_con_umbral(
                &chunk,
                "Texto",
                &Busqueda {
                    lookup: &lookup,
                    ac: &ac,
                    claves_originales: &claves,
                    salida_traer: &["Info".to_string()],
                    opcion,
                },
                2.0, // fuerza camino con dedup
            )?;
            assert_eq!(m1, m2, "opcion={opcion:?}");
            let cols = ["Id", "Texto", "Info", "_match"];
            assert_eq!(d1.select(cols)?, d2.select(cols)?, "opcion={opcion:?}");
        }
        Ok(())
    }

    #[test]
    fn larga_desempata_determinista() -> CoreResult<()> {
        let lookup = df!(
            "clave_norm" => ["gato azul", "pato rojo"],
            "_rank" => [0u64, 1],
            "Info" => ["PRIMERA", "SEGUNDA"],
        )?;
        let (ac, claves) = construir_automata(&lookup)?;
        let textos: Vec<&str> = std::iter::repeat_n("pato rojo y gato azul juntos", 300).collect();
        let base = df!("Texto" => textos)?;

        let mut vistos = HashSet::new();
        for _ in 0..5 {
            let (salida, _) = cruzar_chunk_parcial(
                &base,
                "Texto",
                &Busqueda {
                    lookup: &lookup,
                    ac: &ac,
                    claves_originales: &claves,
                    salida_traer: &["Info".to_string()],
                    opcion: OpcionMultiple::Larga,
                },
            )?;
            for v in salida.column("Info")?.str()?.iter().flatten() {
                vistos.insert(v.to_string());
            }
        }
        assert_eq!(vistos, HashSet::from(["PRIMERA".to_string()]));
        Ok(())
    }
}
