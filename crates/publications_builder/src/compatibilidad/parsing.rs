//! Capa de PARSING/normalización de 'Compatibilidades'/'Comprimidas': solo
//! transforma `DataFrame`s (precio, hoja completa, SKU secuencial, columna
//! 'Combinada') — nada de E/S ni reglas de negocio sobre qué se descarta.
//! Separado del resto de `compatibilidad` (Ronda 9 de auditoría) porque
//! antes vivía en un único archivo de 1552 líneas que mezclaba esta capa con
//! el motor de I/O particionado y las reglas de filtrado.

use std::collections::HashMap;
use std::ops::Not;
use std::sync::LazyLock;

use commerce_core::CoreResult;
use polars::prelude::*;
use regex::Regex;

use super::ModoCompatibilidad;
use crate::comunes::{columna_precio2, columna_texto, RE_VIDEO};
use crate::constantes::{COL_IMAGENES, COL_PRECIO, COL_TITULO, LINEA_EN_MODELO_PATTERN};

static RE_HASTA_DOLAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^.*?\$").unwrap());
static RE_DESDE_PUNTO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\..*$").unwrap());
static RE_LINEA_EN_MODELO: LazyLock<Regex> = LazyLock::new(|| Regex::new(LINEA_EN_MODELO_PATTERN).unwrap());
static RE_PRIMER_TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\S+)").unwrap());
static RE_PUNTO_CERO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.0$").unwrap());
static RE_ESPACIOS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
