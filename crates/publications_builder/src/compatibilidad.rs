use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::ops::Not;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use commerce_core::{concat_diagonal, tomar_filas, CoreResult, EscritorXlsx};
use polars::prelude::*;
use regex::Regex;

use crate::comunes::{columna_precio2, columna_texto, RE_VIDEO};
use crate::constantes::{
    verificar_columnas_reservadas, COL_IMAGENES, COL_PRECIO, COL_START_URL, COL_TITULO,
    LINEA_EN_MODELO_PATTERN,
};

/// Modo de 'Compatibilidades' (el menú lo llama "Compatibilidades") vs
/// 'Comprimidas' — antes viajaban como literales de texto sueltos por 6
/// funciones distintas, sin que el compilador detectara un typo en ninguna
/// de ellas (un literal mal escrito caía en silencio en la rama por defecto
/// de cada `if`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModoCompatibilidad {
    /// Menú "Compatibilidades": conserva 'Linea' salvo que se pida borrarla.
    Repetidas,
    /// Menú "Comprimidas": agrupa coincidencias en una sola fila con
    /// columna 'Coincidencia'.
    Comprimidas,
}

static RE_HASTA_DOLAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^.*?\$").unwrap());
static RE_DESDE_PUNTO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\..*$").unwrap());
static RE_LINEA_EN_MODELO: LazyLock<Regex> = LazyLock::new(|| Regex::new(LINEA_EN_MODELO_PATTERN).unwrap());
static RE_PRIMER_TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\S+)").unwrap());
static RE_PUNTO_CERO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.0$").unwrap());
static RE_ESPACIOS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static RE_NA_FINAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-NA$").unwrap());
static RE_GUION_DIGITOS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d)-(\d)").unwrap());

/// Precio vectorizado de Hoja1: lo que queda tras el primer '$' y antes del
/// primer '.'; luego a entero (los no numéricos caen a 0). Distinto de
/// [`crate::comunes::extraer_precio_entero`] (usado por 'unicas'/'amazon'):
/// aquí el precio ya viene con formato `"$85.99"`, no dígitos sueltos.
///
/// Se descarta cualquier carácter no numérico ANTES de parsear (no solo
/// `trim`): un separador de miles como `"$1,234.56"` dejaría `"1,234"` tras
/// los dos recortes de arriba, que `"1,234".parse::<i64>()` rechazaría y
/// caería en silencio a 0 en vez de 1234.
pub fn limpiar_precio_hoja1(df: &DataFrame) -> CoreResult<DataFrame> {
    let precios: Vec<i64> = columna_texto(df, COL_PRECIO)?
        .iter()
        .map(|v| {
            let s = v.as_deref().unwrap_or("0");
            let s = RE_HASTA_DOLAR.replace(s, "");
            let s = RE_DESDE_PUNTO.replace(&s, "");
            let solo_digitos: String = s.chars().filter(char::is_ascii_digit).collect();
            solo_digitos.parse::<i64>().unwrap_or(0)
        })
        .collect();
    let mut df = df.clone();
    df.with_column(Column::new(COL_PRECIO.into(), precios))?;
    Ok(df)
}

/// Limpia el precio de Hoja1, calcula `Precio2` y separa (válidas,
/// eliminadas): se eliminan las filas con 'Opcion' no vacía o sin imágenes
/// (todas vacías o con 'video'). A propósito NO usa
/// [`crate::comunes::mask_sin_imagenes`]: aquí 'video' TAMBIÉN cuenta como
/// "sin imagen", y sin columnas de imagen el default es NO descartar (los
/// otros dos modos SÍ descartan).
pub fn preprocesar_hoja1(df: &DataFrame) -> CoreResult<(DataFrame, DataFrame)> {
    let mut df = df.clone();
    if df.column(COL_PRECIO).is_ok() {
        df = limpiar_precio_hoja1(&df)?;
        // Se llama UNA sola vez sobre toda 'Hoja1' (no una vez por chunk):
        // no hay riesgo de colisión entre llamadas, así que no hace falta
        // un contador persistente acá.
        let precio2 = columna_precio2(&df, COL_PRECIO, None)?;
        df.with_column(Column::new("Precio2".into(), precio2))?;
    }

    let mask_opcion = crate::comunes::mask_opcion(&df)?;

    let cols_img: Vec<&str> = COL_IMAGENES
        .iter()
        .copied()
        .filter(|c| df.column(c).is_ok())
        .collect();
    let n = df.height();
    let mask_img: Vec<bool> = if cols_img.is_empty() {
        vec![false; n]
    } else {
        let mut todas_vacias = vec![true; n];
        for c in cols_img {
            for (i, v) in columna_texto(&df, c)?.iter().enumerate() {
                let vacio_o_video = match v.as_deref() {
                    None => true,
                    Some(s) => s.is_empty() || RE_VIDEO.is_match(s),
                };
                if !vacio_o_video {
                    todas_vacias[i] = false;
                }
            }
        }
        todas_vacias
    };

    let descartar: Vec<bool> = (0..n).map(|i| mask_opcion[i] || mask_img[i]).collect();
    let mask_descartar = BooleanChunked::from_iter_values("m".into(), descartar.iter().copied());
    let mask_validas: BooleanChunked = mask_descartar.clone().not();
    Ok((df.filter(&mask_validas)?, df.filter(&mask_descartar)?))
}

/// Limpia UNA hoja de compatibilidad y la pasa toda a texto. En modo
/// 'repetidas' limpia 'Linea' (quita '--') y, si se pidió borrarla, la
/// elimina y depura los litros embebidos en 'Modelo'.
pub fn limpiar_hoja_compat(
    df: &DataFrame,
    modo: ModoCompatibilidad,
    borrar_linea: bool,
) -> CoreResult<DataFrame> {
    let mut df = df.clone();
    if modo == ModoCompatibilidad::Repetidas {
        let linea_col = df
            .get_column_names()
            .iter()
            .find(|c| c.as_str().trim().eq_ignore_ascii_case("linea"))
            .map(|s| s.to_string());
        if let Some(ref col) = linea_col {
            let valores: Vec<String> = columna_texto(&df, col)?
                .into_iter()
                .map(|v| v.unwrap_or_default().replace("--", ""))
                .collect();
            df.with_column(Column::new(col.as_str().into(), valores))?;
        }
        if borrar_linea {
            if let Some(col) = &linea_col {
                df = df.drop(col)?;
            }
            if df.column("Modelo").is_ok() {
                let valores: Vec<String> = columna_texto(&df, "Modelo")?
                    .into_iter()
                    .map(|v| {
                        RE_LINEA_EN_MODELO
                            .replace_all(v.unwrap_or_default().as_str(), " ")
                            .trim()
                            .to_string()
                    })
                    .collect();
                df.with_column(Column::new("Modelo".into(), valores))?;
            }
        }
    }

    for nombre in df.get_column_names_owned() {
        let col = df.column(nombre.as_str())?;
        if col.dtype() != &DataType::String {
            let serie = col.as_materialized_series().cast(&DataType::String)?;
            df.with_column(serie.with_name(nombre.clone()).into())?;
        }
    }
    Ok(df)
}

/// Columnas que se concatenan en 'Combinada', según el modo.
pub fn columnas_a_combinar(modo: ModoCompatibilidad, borrar_linea: bool) -> Vec<String> {
    let mut columnas = vec![
        "Cantidades".to_string(),
        "Traducido".to_string(),
        "Caracteristicas".to_string(),
    ];
    if modo == ModoCompatibilidad::Repetidas {
        columnas.extend(["Marca", "Chasis", "Modelo"].map(String::from));
        if !borrar_linea {
            columnas.push("Linea".to_string());
        }
        columnas.push("Litros".to_string());
    } else if modo == ModoCompatibilidad::Comprimidas {
        columnas.push("Coincidencia".to_string());
    }
    columnas
}

/// Numera el SKU secuencial de forma GLOBAL y única en TODA la corrida.
/// El SKU final es `base-N`, N = nº de aparición de esa base contando todas
/// las filas ya procesadas (no solo el sub-bloque actual). `contador`
/// (si `Some`) PERSISTE entre bloques: comparte el conteo global; si es
/// `None`, numera 1-based solo dentro de `df`.
pub fn aplicar_sku_secuencial(
    df: &DataFrame,
    contador: Option<&mut HashMap<String, u64>>,
) -> CoreResult<DataFrame> {
    if df.column("Sku").is_err() {
        return Ok(df.clone());
    }
    let bases: Vec<String> = columna_texto(df, "Sku")?
        .into_iter()
        .map(|v| v.unwrap_or_default())
        .collect();
    let n = bases.len();

    let mut vistos: HashMap<&str, u64> = HashMap::new();
    let mut rank: Vec<u64> = Vec::with_capacity(n);
    for b in &bases {
        let c = vistos.entry(b.as_str()).or_insert(0);
        *c += 1;
        rank.push(*c);
    }

    let offsets: Vec<i64> = match &contador {
        Some(c) => bases
            .iter()
            .map(|b| *c.get(b.as_str()).unwrap_or(&0) as i64)
            .collect(),
        None => vec![0; n],
    };

    if let Some(contador) = contador {
        for (b, c) in vistos {
            *contador.entry(b.to_string()).or_insert(0) += c;
        }
    }

    let sku: Vec<String> = (0..n)
        .map(|i| format!("{}-{}", bases[i], offsets[i] + rank[i] as i64))
        .collect();
    let mut df = df.clone();
    df.with_column(Column::new("Sku".into(), sku))?;
    Ok(df)
}

/// Procesa un lote para 'Compatibilidades'/'Comprimidas': Caracteristicas +
/// Cantidades (a partir de Titulo), transformaciones comunes, SKU secuencial
/// GLOBAL, limpieza de 'Litros' y generación de 'Combinada'.
pub fn procesar_dataframe_compatibilidad(
    df: &DataFrame,
    columnas_a_combinar: &[String],
    modificar_oem: bool,
    contador_sku: Option<&mut HashMap<String, u64>>,
) -> CoreResult<DataFrame> {
    if df.height() == 0 {
        return Ok(df.clone());
    }
    let mut df = df.clone();

    if df.column(COL_TITULO).is_ok() {
        df = crate::comunes::agregar_caracteristicas_y_cantidades(&df)?;
    }

    let extra_mojibake = ["Marca", "Chasis", "Modelo", "Linea", "Coincidencia", "Traducido"];
    df = crate::comunes::aplicar_transformaciones_comunes(&df, modificar_oem, &extra_mojibake)?;

    if df.column("Sku").is_ok() {
        df = aplicar_sku_secuencial(&df, contador_sku)?;
    }

    if df.column("Litros").is_ok() {
        let valores: Vec<String> = columna_texto(&df, "Litros")?
            .into_iter()
            .map(|v| {
                let s = v.unwrap_or_default();
                RE_PRIMER_TOKEN
                    .find(s.trim())
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default()
            })
            .collect();
        df.with_column(Column::new("Litros".into(), valores))?;
    }

    let existentes: Vec<&str> = columnas_a_combinar
        .iter()
        .map(String::as_str)
        .filter(|c| df.column(c).is_ok())
        .collect();
    let combinada: Vec<String> = if existentes.is_empty() {
        vec![String::new(); df.height()]
    } else {
        let columnas_vals: Vec<Vec<Option<String>>> = existentes
            .iter()
            .map(|c| columna_texto(&df, c))
            .collect::<CoreResult<_>>()?;
        (0..df.height())
            .map(|i| {
                let partes: Vec<String> = columnas_vals
                    .iter()
                    .map(|col| {
                        RE_PUNTO_CERO
                            .replace(col[i].as_deref().unwrap_or(""), "")
                            .trim()
                            .to_string()
                    })
                    .collect();
                let unido = partes.join(" ");
                RE_ESPACIOS.replace_all(&unido, " ").trim().to_string()
            })
            .collect()
    };
    df.with_column(Column::new("Combinada".into(), combinada))?;

    Ok(df)
}

fn dedup_por_combinada_menor_precio2(df: &DataFrame) -> CoreResult<DataFrame> {
    let opciones = SortMultipleOptions::default().with_nulls_last(true);
    let ordenado = df.sort(["Precio2"], opciones)?;
    Ok(ordenado.unique::<(), ()>(Some(&["Combinada".to_string()]), UniqueKeepStrategy::First, None)?)
}

/// Cuántas particiones de disco usa el dedup GLOBAL de 'Combinada' (ver
/// [`AcumuladorProcesadas`]). Compatibilidades puede sumar decenas de
/// millones de filas repartidas en muchas hojas/bloques: sin particionar, el
/// dedup por menor 'Precio2' era solo LOCAL a cada sub-bloque de 250k filas —
/// dos filas con la MISMA 'Combinada' que caían en bloques u hojas distintas
/// nunca se comparaban entre sí y ambas sobrevivían.
const PARTICIONES_DEDUP_COMBINADA: usize = 16;
const FILAS_BUFFER_PARTICION_COMBINADA: usize = 1_000_000;

/// Acumula en disco (particionado por hash de 'Combinada') las filas que
/// pasaron los filtros y esperan el dedup por menor 'Precio2'. `agregar` se
/// llama una vez por sub-bloque durante el streaming; `finalizar` relee cada
/// partición COMPLETA, deduplica de verdad (ahora sí, entre TODOS los
/// bloques/hojas) y escribe "Procesadas". Memoria acotada: cada partición
/// vive en RAM una a la vez, nunca el archivo completo.
struct AcumuladorProcesadas {
    tmpdir: tempfile::TempDir,
    n_part: usize,
    buffer_part: Vec<Vec<DataFrame>>,
    archivos_part: Vec<Vec<PathBuf>>,
    contador_archivos: Vec<usize>,
    buffer_filas: usize,
    // Semilla aleatoria por CORRIDA (no por valor: todos los valores de esta
    // misma corrida deben hashear de forma CONSISTENTE entre sí para caer
    // siempre en la misma partición). `DefaultHasher::new()` a secas usa
    // claves fijas (SipHash con clave (0,0)) — un archivo de entrada
    // diseñado a propósito podría forzar que todo caiga en una sola
    // partición, anulando el acotado de memoria que este particionado
    // existe para garantizar. `RandomState::new()` sortea una semilla nueva
    // una sola vez acá; `build_hasher()` la reusa para cada valor.
    hasher_state: std::collections::hash_map::RandomState,
}

impl AcumuladorProcesadas {
    fn nuevo(n_part: usize) -> CoreResult<Self> {
        Ok(Self {
            tmpdir: tempfile::tempdir()?,
            n_part,
            buffer_part: vec![Vec::new(); n_part],
            archivos_part: vec![Vec::new(); n_part],
            contador_archivos: vec![0; n_part],
            buffer_filas: 0,
            hasher_state: std::collections::hash_map::RandomState::new(),
        })
    }

    fn agregar(&mut self, df: DataFrame) -> CoreResult<()> {
        if df.height() == 0 {
            return Ok(());
        }
        let alto = df.height();
        let combinada = columna_texto(&df, "Combinada")?;
        let mut por_particion: Vec<Vec<usize>> = vec![Vec::new(); self.n_part];
        for (i, v) in combinada.iter().enumerate() {
            let pid = (self.hasher_state.hash_one(v) % self.n_part as u64) as usize;
            por_particion[pid].push(i);
        }
        for (pid, indices) in por_particion.into_iter().enumerate() {
            if !indices.is_empty() {
                self.buffer_part[pid].push(tomar_filas(&df, &indices)?);
            }
        }
        self.buffer_filas += alto;
        if self.buffer_filas >= FILAS_BUFFER_PARTICION_COMBINADA {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> CoreResult<()> {
        for p in 0..self.n_part {
            if self.buffer_part[p].is_empty() {
                continue;
            }
            let dfs = std::mem::take(&mut self.buffer_part[p]);
            let mut df = concat_diagonal(dfs)?;
            let ruta = self
                .tmpdir
                .path()
                .join(format!("p{p}_{}.ipc", self.contador_archivos[p]));
            self.contador_archivos[p] += 1;
            let mut archivo = std::fs::File::create(&ruta)?;
            IpcWriter::new(&mut archivo).finish(&mut df)?;
            self.archivos_part[p].push(ruta);
        }
        self.buffer_filas = 0;
        Ok(())
    }

    /// Relee cada partición, deduplica por 'Combinada' (menor 'Precio2') y
    /// escribe el resultado en la hoja "Procesadas".
    ///
    /// Si falta 'Precio2', se avisa en vez de saltear el dedup en silencio
    /// (antes era la única asimetría del crate: `publications_validator`'s
    /// `dedup_bucket` ya avisaba en el mismo escenario).
    fn finalizar(mut self, escritor: &mut EscritorXlsx, avisar: &mut dyn FnMut(&str)) -> CoreResult<()> {
        self.flush()?;
        for rutas in &self.archivos_part {
            if rutas.is_empty() {
                continue;
            }
            let dfs: Vec<DataFrame> = rutas
                .iter()
                .map(|r| -> CoreResult<DataFrame> {
                    let archivo = std::fs::File::open(r)?;
                    Ok(IpcReader::new(archivo).finish()?)
                })
                .collect::<CoreResult<_>>()?;
            let bucket = concat_diagonal(dfs)?;
            let deduped = if bucket.column("Precio2").is_ok() {
                dedup_por_combinada_menor_precio2(&bucket)?
            } else {
                avisar("No se encontró la columna 'Precio2': no se deduplicó por precio en 'Procesadas'.");
                bucket
            };
            escritor.escribir(&deduped, Some("Procesadas"))?;
        }
        Ok(())
    }
}

/// Filtra/limpia sobre 'Combinada'; devuelve las válidas (SIN deduplicar
/// todavía: eso ahora es responsabilidad de [`AcumuladorProcesadas`], que lo
/// hace de forma GLOBAL). Escribe a hojas aparte: 'Exceso_caracteres' (>60)
/// y, en 'comprimidas', 'Eliminadas' (con 'ERROR' o con >=2 'NA'). Luego
/// normaliza guiones.
pub fn aplicar_filtros_combinada(
    df_proc: &DataFrame,
    modo: ModoCompatibilidad,
    escritor: &mut EscritorXlsx,
) -> CoreResult<DataFrame> {
    if df_proc.height() == 0 || df_proc.column("Combinada").is_err() {
        return Ok(df_proc.clone());
    }
    let mut df_proc = df_proc.clone();

    let combinada = columna_texto(&df_proc, "Combinada")?;
    let mask_exceso: Vec<bool> = combinada
        .iter()
        .map(|v| v.as_deref().unwrap_or("").chars().count() > 60)
        .collect();
    let mask_exceso_ca = BooleanChunked::from_iter_values("m".into(), mask_exceso.iter().copied());
    let mut df_exceso = df_proc.filter(&mask_exceso_ca)?;
    let mask_no_exceso: BooleanChunked = mask_exceso_ca.not();
    df_proc = df_proc.filter(&mask_no_exceso)?;
    if df_exceso.column(COL_PRECIO).is_ok() {
        df_exceso = df_exceso.drop(COL_PRECIO)?;
    }
    escritor.escribir(&df_exceso, Some("Exceso_caracteres"))?;

    if modo == ModoCompatibilidad::Comprimidas {
        let combinada = columna_texto(&df_proc, "Combinada")?;
        let mask_error: Vec<bool> = combinada
            .iter()
            .map(|v| v.as_deref().unwrap_or("").contains("ERROR"))
            .collect();
        let mask_error_ca = BooleanChunked::from_iter_values("m".into(), mask_error.iter().copied());
        escritor.escribir(&df_proc.filter(&mask_error_ca)?, Some("Eliminadas"))?;
        let mask_no_error: BooleanChunked = mask_error_ca.not();
        df_proc = df_proc.filter(&mask_no_error)?;

        let combinada = columna_texto(&df_proc, "Combinada")?;
        let mask_na: Vec<bool> = combinada
            .iter()
            .map(|v| v.as_deref().unwrap_or("").matches("NA").count() >= 2)
            .collect();
        let mask_na_ca = BooleanChunked::from_iter_values("m".into(), mask_na.iter().copied());
        escritor.escribir(&df_proc.filter(&mask_na_ca)?, Some("Eliminadas"))?;
        let mask_no_na: BooleanChunked = mask_na_ca.not();
        df_proc = df_proc.filter(&mask_no_na)?;

        let combinada: Vec<String> = columna_texto(&df_proc, "Combinada")?
            .into_iter()
            .map(|v| {
                let s = v.unwrap_or_default();
                let s = RE_NA_FINAL.replace(&s, "");
                let s = RE_GUION_DIGITOS.replace_all(&s, "${1}@@HYPHEN@@${2}");
                let s = s.replace('-', " ");
                s.replace("@@HYPHEN@@", "-")
            })
            .collect();
        df_proc.with_column(Column::new("Combinada".into(), combinada))?;
    }

    Ok(df_proc)
}

const FILAS_POR_SUBBLOQUE: usize = 250_000;
const FILAS_PRE_EXPLODE: usize = 20_000;

/// Explota 'Coincidencia' (separada por '@') en una fila por coincidencia.
/// Implementado como un `take` con índices repetidos (en vez de la
/// maquinaria de listas de Polars): más simple de leer para este caso.
fn explotar_coincidencia(df: &DataFrame) -> CoreResult<DataFrame> {
    let coincidencia = columna_texto(df, "Coincidencia")?;
    let n = df.height();
    let partes_por_fila: Vec<Vec<String>> = coincidencia
        .iter()
        .map(|v| match v.as_deref() {
            Some(s) if !s.is_empty() => s.split('@').map(str::to_string).collect(),
            _ => vec![String::new()],
        })
        .collect();

    let indices: Vec<IdxSize> = (0..n)
        .flat_map(|i| std::iter::repeat(i as IdxSize).take(partes_por_fila[i].len().max(1)))
        .collect();
    let coincidencia_explotada: Vec<String> = partes_por_fila.into_iter().flatten().collect();

    let idx_ca = IdxCa::from_vec(PlSmallStr::EMPTY, indices);
    let mut df_explotado = df.take(&idx_ca)?;
    df_explotado.with_column(Column::new("Coincidencia".into(), coincidencia_explotada))?;
    Ok(df_explotado)
}

/// Procesa un lote YA UNIDO (producto×compatibilidad): explota, filtra y
/// escribe. El explode corre por REBANADAS (`FILAS_PRE_EXPLODE`) y su
/// resultado se procesa en SUB-BLOQUES (`FILAS_POR_SUBBLOQUE`): pico de RAM
/// acotado con independencia del factor de explosión.
/// Contexto compartido por `procesar_unido`/`despachar_unido`: agrupa lo que
/// siempre viaja junto entre sub-bloques de un mismo archivo (antes 6
/// parámetros sueltos por función, con `#[allow(clippy::too_many_arguments)]`
/// en vez de resolver la señal del propio lint).
struct ContextoUnido<'a> {
    columnas_combinar: &'a [String],
    modo: ModoCompatibilidad,
    modificar_oem: bool,
    escritor: &'a mut EscritorXlsx,
    contador_sku: &'a mut HashMap<String, u64>,
    acumulador: &'a mut AcumuladorProcesadas,
}

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
    ctx.acumulador.agregar(df_proc)?;
    Ok(())
}

/// `true` si `hoja` tiene el formato `HojaN` (N = uno o más dígitos),
/// sin distinguir mayúsculas/minúsculas ni espacios — el nombre que genera
/// el scraper para las hojas de compatibilidad (Hoja2, Hoja3…). Antes se
/// usaba `contains("Hoja")`, que colaba falsos positivos como "HojaResumen"
/// y descartaba en silencio "hoja2" en minúscula.
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

    // Comparación case/espacio-insensible, igual que `iter_bloques_compat`
    // hace para Hoja2/Hoja3... (`es_hoja_de_compatibilidad`): antes esta
    // búsqueda comparaba contra el literal exacto `"Hoja1"`, así que un
    // archivo con la hoja principal llamada `"hoja1"` (minúscula) hacía
    // fallar esta condición y el modo Compatibilidades/Comprimidas no
    // procesaba nada, sin ninguna pista real de la causa.
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
        let mut acumulador = AcumuladorProcesadas::nuevo(PARTICIONES_DEDUP_COMBINADA)?;

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

        acumulador.finalizar(escritor, &mut *avisar)?;
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
    fn limpiar_precio_hoja1_extrae_el_entero_entre_signo_y_punto() -> CoreResult<()> {
        let df = df!("Precio" => ["$85.99", "$5.00", "sin precio", ""])?;
        let limpio = limpiar_precio_hoja1(&df)?;
        assert_eq!(
            limpio.column("Precio")?.i64()?.iter().collect::<Vec<_>>(),
            vec![Some(85), Some(5), Some(0), Some(0)],
            "no numérico o vacío cae a 0, no aborta"
        );
        Ok(())
    }

    #[test]
    fn limpiar_precio_hoja1_tolera_separador_de_miles() -> CoreResult<()> {
        // "$1,234.56" tras los recortes de '$' y '.' queda en "1,234", que
        // `parse::<i64>()` rechazaría directamente (coma no es dígito) y
        // caería en silencio a 0 en vez de 1234.
        let df = df!("Precio" => ["$1,234.56"])?;
        let limpio = limpiar_precio_hoja1(&df)?;
        assert_eq!(limpio.column("Precio")?.i64()?.get(0), Some(1234));
        Ok(())
    }

    #[test]
    fn preprocesar_hoja1_descarta_opcion_llena_o_sin_imagenes_incluido_video() -> CoreResult<()> {
        let df = df!(
            "web-scraper-start-url" => ["u1", "u2", "u3", "u4"],
            "Precio" => ["$10.00", "$20.00", "$30.00", "$40.00"],
            "Opcion" => ["opcion-llena", "", "", ""],
            "Imagen 1" => ["https://c.com/1.jpg", "", "https://c.com/video.mp4", "https://c.com/2.jpg"],
        )?;
        let (validas, eliminadas) = preprocesar_hoja1(&df)?;
        assert_eq!(
            validas.column("web-scraper-start-url")?.str()?.get(0),
            Some("u4"),
            "solo u4 tiene Opcion vacía Y una imagen real (no video)"
        );
        assert_eq!(validas.height(), 1);
        let mut urls_eliminadas: Vec<_> = eliminadas
            .column("web-scraper-start-url")?
            .str()?
            .iter()
            .map(|v| v.unwrap().to_string())
            .collect();
        urls_eliminadas.sort();
        assert_eq!(
            urls_eliminadas,
            vec!["u1".to_string(), "u2".to_string(), "u3".to_string()],
            "u1: Opcion llena; u2: sin imagen; u3: 'video' cuenta como sin imagen"
        );
        Ok(())
    }

    #[test]
    fn preprocesar_hoja1_sin_columnas_de_imagen_no_descarta_por_defecto() -> CoreResult<()> {
        // A diferencia de 'unicas'/'amazon': sin columnas de imagen, el
        // default acá es NO descartar (documentado en preprocesar_hoja1).
        let df = df!(
            "web-scraper-start-url" => ["u1"],
            "Precio" => ["$10.00"],
            "Opcion" => [""],
        )?;
        let (validas, eliminadas) = preprocesar_hoja1(&df)?;
        assert_eq!(validas.height(), 1);
        assert_eq!(eliminadas.height(), 0);
        Ok(())
    }

    #[test]
    fn sku_secuencial_global_entre_bloques() -> CoreResult<()> {
        let mut contador = HashMap::new();
        let df1 = df!("Sku" => ["u-1-a-1", "u-1-a-1"])?;
        let r1 = aplicar_sku_secuencial(&df1, Some(&mut contador))?;
        assert_eq!(
            r1.column("Sku")?
                .str()?
                .iter()
                .map(|v| v.unwrap())
                .collect::<Vec<_>>(),
            vec!["u-1-a-1-1", "u-1-a-1-2"]
        );

        let df2 = df!("Sku" => ["u-1-a-1"])?;
        let r2 = aplicar_sku_secuencial(&df2, Some(&mut contador))?;
        assert_eq!(
            r2.column("Sku")?.str()?.get(0),
            Some("u-1-a-1-3"),
            "continúa la numeración GLOBAL, no reinicia"
        );
        Ok(())
    }

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
    fn aplicar_filtros_combinada_exceso_de_caracteres_va_a_hoja_aparte() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("filtros.xlsx");
        let mut escritor = crate::escritor::nuevo_escritor(&ruta, |_: &str| {})?;

        let combinada_larga = "x".repeat(61);
        let df = df!(
            "Sku" => ["s1", "s2"],
            "Combinada" => [combinada_larga.as_str(), "corta"],
        )?;
        let resultado = aplicar_filtros_combinada(&df, ModoCompatibilidad::Repetidas, &mut escritor)?;
        escritor.cerrar()?;

        assert_eq!(
            resultado.height(),
            1,
            "la fila con más de 60 caracteres en Combinada se saca"
        );
        assert_eq!(resultado.column("Combinada")?.str()?.get(0), Some("corta"));

        let mut libro = commerce_core::abrir_libro(&ruta).unwrap();
        let hojas = commerce_core::nombres_hojas_libro(&libro);
        assert!(hojas.iter().any(|h| h == "Exceso_caracteres"));
        let exceso = commerce_core::leer_hoja_por_nombre(&mut libro, &ruta, "Exceso_caracteres").unwrap();
        assert_eq!(exceso.height(), 1);
        Ok(())
    }

    #[test]
    fn aplicar_filtros_combinada_modo_comprimidas_filtra_error_na_y_normaliza_guiones() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("filtros_comp.xlsx");
        let mut escritor = crate::escritor::nuevo_escritor(&ruta, |_: &str| {})?;

        let df = df!(
            "Sku" => ["s1", "s2", "s3"],
            "Combinada" => ["Tiene ERROR aqui", "NA parte-NA otra", "Civic 2-3 puerta-lateral"],
        )?;
        let resultado = aplicar_filtros_combinada(&df, ModoCompatibilidad::Comprimidas, &mut escritor)?;
        escritor.cerrar()?;

        assert_eq!(
            resultado.height(),
            1,
            "solo sobrevive la fila sin 'ERROR' y con menos de 2 'NA'"
        );
        assert_eq!(
            resultado.column("Combinada")?.str()?.get(0),
            Some("Civic 2-3 puerta lateral"),
            "el guión ENTRE DÍGITOS se preserva; el resto de guiones se normaliza a espacio"
        );

        let mut libro = commerce_core::abrir_libro(&ruta).unwrap();
        let eliminadas = commerce_core::leer_hoja_por_nombre(&mut libro, &ruta, "Eliminadas").unwrap();
        assert_eq!(
            eliminadas.height(),
            2,
            "la fila con 'ERROR' y la de 2+ 'NA' van a Eliminadas"
        );
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
        // Antes: el dedup por menor 'Precio2' era LOCAL a cada llamada de
        // `despachar_unido` — y cada hoja de compatibilidad (Hoja2, Hoja3…)
        // se procesa en su PROPIA invocación. Dos productos DISTINTOS que
        // resuelven a la MISMA 'Combinada' pero llegan por hojas distintas
        // sobrevivían los dos, cada uno "deduplicado" solo contra sí mismo.
        // Ahora el dedup es GLOBAL (AcumuladorProcesadas particiona por hash
        // de 'Combinada' y recién deduplica al final, entre TODAS las
        // hojas): debe sobrevivir solo el de menor Precio2.
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
        // Antes: el filtro comparaba con `== "Hoja1"` y `contains("Hoja")`
        // sin normalizar mayúsculas — "hoja2" se descartaba en silencio.
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
        // Antes: `sheet_names.iter().any(|h| h == "Hoja1")` y la lectura
        // posterior comparaban con el literal exacto "Hoja1" — un archivo
        // con la hoja principal llamada "hoja1" (minúscula) hacía fallar la
        // condición y el modo Compatibilidades/Comprimidas no procesaba
        // nada, con el aviso engañoso "No se encontró la 'Hoja1'" pese a que
        // el archivo sí la tenía (solo que en minúscula).
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
        // Antes: `contains("Hoja")` aceptaba falsos positivos como
        // "HojaResumen" y los unía contra Hoja1 como si fueran datos de
        // compatibilidad reales.
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
        // Antes de este fix, el loop que procesa las filas de Hoja1
        // ("No_Procesadas") nunca llamaba a `progreso`: la barra nunca
        // llegaba al 100% aunque el proceso terminara bien.
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
        // Sin `web-scraper-start-url`, el loop de compatibilidad (`tiene_url`)
        // ni siquiera corre — antes de este fix eso significaba CERO avances
        // de progreso en toda la corrida, barra congelada en 0%.
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
