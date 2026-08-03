use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;

use commerce_core::CoreResult;
use polars::prelude::*;
use regex::Regex;

use crate::constantes::{
    precio_de_rango, CANTIDAD_CARACTERES_OEM3, COL_IMAGENES, COL_MOTIVO_ELIMINACION, COL_OEM, COL_PRECIO,
    COL_START_URL, COL_TIENDA, COL_TITULO, MAPEO_CANTIDADES, MAPEO_CARACTERISTICAS, PARES_OPUESTOS,
    URL_REGEX, URL_REPLACE_REGEX,
};

static PALABRA: LazyLock<Regex> = LazyLock::new(|| commerce_core::regex_literal(r"\b\w+\b"));
static RE_URL: LazyLock<Regex> = LazyLock::new(|| commerce_core::regex_literal(URL_REGEX));
static RE_URL_REPLACE: LazyLock<Regex> = LazyLock::new(|| commerce_core::regex_literal(URL_REPLACE_REGEX));
pub(crate) static RE_VIDEO: LazyLock<Regex> = LazyLock::new(|| commerce_core::regex_literal_sin_may("video"));
static RE_TIENDA_SIMBOLOS: LazyLock<Regex> =
    LazyLock::new(|| commerce_core::regex_literal(r"[\s:_\-.,*/\\]"));

fn capitalizar(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(primera) => primera.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Características del título (pares opuestos se cancelan entre sí, p. ej.
/// "Front" + "Rear" en el mismo título no aportan característica).
fn extraer_caracteristicas(titulo: &str) -> String {
    let minuscula = titulo.to_lowercase();
    let mut caracteristicas: Vec<&'static str> = Vec::new();
    for palabra in PALABRA.find_iter(&minuscula) {
        if let Some(&valor) = MAPEO_CARACTERISTICAS.get(palabra.as_str()) {
            if !caracteristicas.contains(&valor) {
                caracteristicas.push(valor);
            }
        }
    }
    for (par1, par2) in PARES_OPUESTOS {
        if caracteristicas.contains(par1) && caracteristicas.contains(par2) {
            caracteristicas.retain(|c| c != par1 && c != par2);
        }
    }
    let resultado = caracteristicas
        .iter()
        .map(|c| capitalizar(c))
        .collect::<Vec<_>>()
        .join(" ");
    if resultado.is_empty() {
        resultado
    } else {
        format!("{resultado} ")
    }
}

/// Cantidad del título con PRIORIDAD FIJA kit > pair > set/pcs: "Pair Kit"
/// da "Kit" (misma regla en TODOS los modos que llaman a esta función).
fn extraer_cantidades(titulo: &str) -> String {
    let minuscula = titulo.to_lowercase();
    let palabras: HashSet<&str> = PALABRA.find_iter(&minuscula).map(|m| m.as_str()).collect();
    for clave in ["kit", "pair", "set", "pcs"] {
        // `.get()`, no índice directo: las claves de este bucle y las de
        // `MAPEO_CANTIDADES` viven en archivos distintos (comunes.rs /
        // constantes.rs). Si una keyword nueva se agrega acá sin
        // sincronizar el mapa, un título scrapeado real que la contenga no
        // debe hacer panickear todo el proceso de construcción — se omite
        // la cantidad para ese título (igual que si no hubiera match) y se
        // sigue con el resto del lote.
        if palabras.contains(clave) {
            if let Some(valor) = MAPEO_CANTIDADES.get(clave) {
                return capitalizar(valor);
            }
            return String::new();
        }
    }
    String::new()
}

/// Envoltorio fino sobre `commerce_core::columna_texto`: solo adapta el tipo
/// de error a `CoreResult` para que los call sites existentes (varios con
/// `.collect::<CoreResult<_>>()`) no necesiten tocarse.
pub(crate) fn columna_texto(df: &DataFrame, nombre: &str) -> CoreResult<Vec<Option<String>>> {
    Ok(commerce_core::columna_texto(df, nombre)?)
}

/// True si la fila tiene contenido en 'Opcion' (motivo de descarte). Sin esa
/// columna, no hay motivo (`false`).
pub fn mask_opcion(df: &DataFrame) -> CoreResult<Vec<bool>> {
    match df.column(crate::constantes::COL_OPCION) {
        Ok(_) => Ok(columna_texto(df, crate::constantes::COL_OPCION)?
            .iter()
            .map(|v| matches!(v.as_deref(), Some(s) if !s.is_empty()))
            .collect()),
        Err(_) => Ok(vec![false; df.height()]),
    }
}

/// True si TODAS las columnas de imagen presentes están vacías. Sin ninguna
/// columna de imagen, se considera "sin imágenes" (`true`).
pub fn mask_sin_imagenes(df: &DataFrame) -> CoreResult<Vec<bool>> {
    let cols_img: Vec<&str> = COL_IMAGENES
        .iter()
        .copied()
        .filter(|c| df.column(c).is_ok())
        .collect();
    let n = df.height();
    if cols_img.is_empty() {
        return Ok(vec![true; n]);
    }
    let mut vacias = vec![true; n];
    for c in cols_img {
        for (i, v) in columna_texto(df, c)?.iter().enumerate() {
            if matches!(v.as_deref(), Some(s) if !s.is_empty()) {
                vacias[i] = false;
            }
        }
    }
    Ok(vacias)
}

/// Separa `df` en (válidas, eliminadas) aplicando `categorias` EN ORDEN de
/// prioridad: cada fila cae en la PRIMERA máscara de la lista que la marque
/// (`true`) y queda etiquetada con su motivo; lo que no matchea ninguna
/// categoría es "válida". Evita repetir a mano, en cada motivo sucesivo, el
/// patrón "esta condición Y ninguna de las ya categorizadas".
pub fn separar_por_categorias(
    df: &DataFrame,
    categorias: &[(&[bool], &str)],
) -> CoreResult<(DataFrame, DataFrame)> {
    let n = df.height();
    let mut ya_categorizada = vec![false; n];
    let mut eliminadas: Option<DataFrame> = None;

    for (mask, motivo) in categorias {
        let propia: Vec<bool> = (0..n).map(|i| !ya_categorizada[i] && mask[i]).collect();
        for (i, v) in propia.iter().enumerate() {
            if *v {
                ya_categorizada[i] = true;
            }
        }
        let mask_ca = BooleanChunked::from_iter_values("m".into(), propia.iter().copied());
        let mut sub = df.filter(&mask_ca)?;
        if sub.height() > 0 {
            let alto = sub.height();
            sub.with_column(Column::new(COL_MOTIVO_ELIMINACION.into(), vec![*motivo; alto]))?;
            eliminadas = Some(match eliminadas {
                Some(acc) => acc.vstack(&sub)?,
                None => sub,
            });
        }
    }

    let mask_validas: Vec<bool> = (0..n).map(|i| !ya_categorizada[i]).collect();
    let mask_validas_ca = BooleanChunked::from_iter_values("m".into(), mask_validas.iter().copied());
    let validas = df.filter(&mask_validas_ca)?;

    Ok((validas, eliminadas.unwrap_or_else(DataFrame::empty)))
}

/// Limpieza de las columnas de imagen: 'video' (cualquier mayúscula) → vacío;
/// si no, se extrae la URL y se reemplaza el último segmento por
/// `s-l1600.jpg` (alta resolución). Sin URL reconocible → vacío.
fn limpiar_imagenes(df: &DataFrame) -> CoreResult<DataFrame> {
    let mut df = df.clone();
    for &c in COL_IMAGENES.iter() {
        if df.column(c).is_err() {
            continue;
        }
        let valores: Vec<String> = columna_texto(&df, c)?
            .iter()
            .map(|v| {
                let v = v.as_deref().unwrap_or("");
                if RE_VIDEO.is_match(v) {
                    String::new()
                } else if let Some(m) = RE_URL.find(v) {
                    RE_URL_REPLACE.replace(m.as_str(), "/s-l1600.jpg").into_owned()
                } else {
                    String::new()
                }
            })
            .collect();
        df.with_column(Column::new(c.into(), valores))?;
    }
    Ok(df)
}

/// Rellena `OEM` vacío con `US123400` y genera `OEM3` (recortada a
/// [`CANTIDAD_CARACTERES_OEM3`]). No-op si la columna `OEM` no existe.
pub fn aplicar_oem3(df: &DataFrame) -> CoreResult<DataFrame> {
    if df.column(COL_OEM).is_err() {
        return Ok(df.clone());
    }
    let oem: Vec<String> = columna_texto(df, COL_OEM)?
        .into_iter()
        .map(|v| v.unwrap_or_else(|| "US123400".to_string()))
        .collect();
    let mut df = df.clone();
    df.with_column(Column::new(COL_OEM.into(), oem.clone()))?;
    let oem3: Vec<String> = oem
        .iter()
        .map(|v| {
            let base = if v.trim().is_empty() {
                "US123400"
            } else {
                v.as_str()
            };
            base.chars().take(CANTIDAD_CARACTERES_OEM3).collect()
        })
        .collect();
    df.with_column(Column::new("OEM3".into(), oem3))?;
    Ok(df)
}

/// `Precio2`: el precio de rango ([`precio_de_rango`]), con un jitter
/// determinista (1-50, por hash de un índice) para que filas con el mismo
/// rango no salgan con precios idénticos.
///
/// `contador_jitter` (si `Some`) PERSISTE entre llamadas, igual que
/// `aplicar_sku_secuencial`: sin esto, esta función se llama una vez POR
/// HOJA/chunk, así que la fila 0 de CADA hoja recibía el MISMO jitter que la
/// fila 0 de cualquier otra — colisión sistemática entre hojas, no solo
/// aleatoria. `None` numera 1-based solo dentro de `df` (uso en tests
/// aislados o cuando de verdad hay una sola llamada, como Hoja1 en
/// 'compatibilidad', que se procesa entera de una vez).
pub fn columna_precio2(
    df: &DataFrame,
    columna_precio: &str,
    contador_jitter: Option<&mut u64>,
) -> CoreResult<Vec<Option<i64>>> {
    let serie = df.column(columna_precio)?.as_materialized_series().clone();
    let precios: Vec<Option<i64>> = match serie.dtype() {
        DataType::String => serie
            .str()?
            .iter()
            .map(|v| v.and_then(|s| s.trim().parse::<i64>().ok()))
            .collect(),
        _ => serie.cast(&DataType::Int64)?.i64()?.iter().collect(),
    };
    let base: u64 = match &contador_jitter {
        Some(c) => **c,
        None => 0,
    };
    let resultado = precios
        .iter()
        .enumerate()
        .map(|(i, precio)| {
            precio
                .and_then(precio_de_rango)
                .map(|valor| valor - (jitter(base + i as u64) as i64))
        })
        .collect();
    if let Some(contador) = contador_jitter {
        *contador += precios.len() as u64;
    }
    Ok(resultado)
}

fn jitter(indice: u64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    indice.hash(&mut hasher);
    hasher.finish() % 50 + 1
}

/// `Sku = "u-" + precio + "-" + tienda_normalizada + "-" + id_de_url`.
/// No-op si faltan `Precio`, `Tienda` o `web-scraper-start-url`.
///
/// `patron_id_url` extrae el identificador de la URL (grupo 1);
/// `prefijo_a_quitar` (opcional) se aplica al nombre de tienda ya en
/// minúsculas ANTES de despojarlo de símbolos (p. ej. Amazon quita
/// "visit the " de "Visit the Bosch Store").
pub fn generar_sku(
    df: &DataFrame,
    patron_id_url: &Regex,
    prefijo_a_quitar: Option<&Regex>,
) -> CoreResult<DataFrame> {
    if df.column(COL_PRECIO).is_err() || df.column(COL_TIENDA).is_err() || df.column(COL_START_URL).is_err() {
        return Ok(df.clone());
    }
    let precio = columna_texto(df, COL_PRECIO)?;
    let tienda = columna_texto(df, COL_TIENDA)?;
    let url = columna_texto(df, COL_START_URL)?;
    let n = df.height();

    let sku: Vec<String> = (0..n)
        .map(|i| {
            let p = precio[i].as_deref().unwrap_or("0");
            let t = tienda[i].as_deref().unwrap_or("").to_lowercase();
            let t = match prefijo_a_quitar {
                Some(re) => re.replace(&t, "").into_owned(),
                None => t,
            };
            let t_norm = RE_TIENDA_SIMBOLOS.replace_all(&t, "");
            let u = url[i].as_deref().unwrap_or("");
            let id = patron_id_url
                .captures(u)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str())
                .unwrap_or("");
            format!("u-{p}-{t_norm}-{id}")
        })
        .collect();
    let mut df = df.clone();
    df.with_column(Column::new("Sku".into(), sku))?;
    Ok(df)
}

/// Añade `Caracteristicas`/`Cantidades` a partir de `Titulo`. No-op si la
/// columna no existe.
pub fn agregar_caracteristicas_y_cantidades(df: &DataFrame) -> CoreResult<DataFrame> {
    if df.column(COL_TITULO).is_err() {
        return Ok(df.clone());
    }
    let titulos = columna_texto(df, COL_TITULO)?;
    let caracteristicas: Vec<String> = titulos
        .iter()
        .map(|t| extraer_caracteristicas(t.as_deref().unwrap_or("")))
        .collect();
    let cantidades: Vec<String> = titulos
        .iter()
        .map(|t| extraer_cantidades(t.as_deref().unwrap_or("")))
        .collect();
    let mut df = df.clone();
    df.with_column(Column::new("Caracteristicas".into(), caracteristicas))?;
    df.with_column(Column::new("Cantidades".into(), cantidades))?;
    Ok(df)
}

static RE_DIGITOS: LazyLock<Regex> = LazyLock::new(|| commerce_core::regex_literal(r"\d+"));

/// Extrae la primera racha de dígitos de `columna` y la deja como entero
/// (0 si no hay ninguna). Usado por 'unicas' y 'amazon' (compatibilidades
/// tiene su propia limpieza de precio, distinta: ver
/// [`crate::compatibilidad::limpiar_precio_hoja1`]).
pub fn extraer_precio_entero(df: &DataFrame, columna: &str) -> CoreResult<DataFrame> {
    let precios: Vec<i64> = columna_texto(df, columna)?
        .iter()
        .map(|v| {
            v.as_deref()
                .and_then(|s| RE_DIGITOS.find(s))
                .and_then(|m| m.as_str().parse::<i64>().ok())
                .unwrap_or(0)
        })
        .collect();
    let mut df = df.clone();
    df.with_column(Column::new(columna.into(), precios))?;
    Ok(df)
}

/// Regex del identificador de artículo eBay al final de la URL.
pub static RE_ID_EBAY: LazyLock<Regex> = LazyLock::new(|| commerce_core::regex_literal(r"/(\d+)(?:\?|$)"));

/// Limpiezas y transformaciones comunes a todos los modos: normaliza las URLs
/// de imagen y genera el SKU base (estilo eBay); la transformación de OEM
/// solo se aplica si `modificar_oem` es `true`. `columnas_mojibake_extra`
/// solo se usa si [`crate::constantes::USAR_REPARACION_MOJIBAKE`] está activo
/// (reservado, hoy siempre desactivado: el arreglo de mojibake es lento y
/// solo hace falta si el texto trae símbolos rotos).
pub fn aplicar_transformaciones_comunes(
    df: &DataFrame,
    modificar_oem: bool,
    columnas_mojibake_extra: &[&str],
) -> CoreResult<DataFrame> {
    if df.height() == 0 {
        return Ok(df.clone());
    }
    let mut df = df.clone();

    if crate::constantes::USAR_REPARACION_MOJIBAKE {
        let mut cols: Vec<&str> = vec![COL_TITULO, COL_OEM, crate::constantes::COL_OPCION, COL_TIENDA];
        cols.extend(columnas_mojibake_extra);
        let cols: Vec<&str> = cols.into_iter().filter(|c| df.column(c).is_ok()).collect();
        df = commerce_core::limpiar_mojibake(df, Some(&cols))?;
    }

    df = limpiar_imagenes(&df)?;
    if modificar_oem {
        df = aplicar_oem3(&df)?;
    }
    df = generar_sku(&df, &RE_ID_EBAY, None)?;
    Ok(df)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cantidades_prioridad_kit_sobre_par() {
        assert_eq!(extraer_cantidades("Pair Kit Bumper"), "Kit");
        assert_eq!(extraer_cantidades("Pair of mirrors"), "Par");
    }

    #[test]
    fn caracteristicas_cancela_pares_opuestos() {
        assert_eq!(extraer_caracteristicas("Front and Rear Bumper"), "");
        assert_eq!(extraer_caracteristicas("Front Bumper LH"), "Frontal Izq ");
    }

    #[test]
    fn jitter_es_reproducible_y_rompe_empates() -> CoreResult<()> {
        let df = df!("Precio" => [85, 85, 85])?;
        let p1 = columna_precio2(&df, "Precio", None)?;
        let p2 = columna_precio2(&df, "Precio", None)?;
        assert_eq!(p1, p2, "reproducible");
        assert!(
            p1.iter().collect::<HashSet<_>>().len() > 1,
            "y aun así rompe los empates"
        );
        Ok(())
    }

    #[test]
    fn jitter_no_colisiona_entre_llamadas_cuando_el_contador_persiste() -> CoreResult<()> {
        // El índice del jitter debe PERSISTIR entre llamadas (una por
        // hoja/chunk): si cada una arrancara en 0, la fila 0 de dos hojas con
        // el mismo precio recibiría el mismo Precio2, una colisión
        // sistemática. Varias filas por hoja para que una coincidencia de
        // hash 1-en-50 no vuelva flaky el test.
        let df = df!("Precio" => [85, 85, 85, 85, 85])?;
        let mut contador = 0u64;
        let hoja1 = columna_precio2(&df, "Precio", Some(&mut contador))?;
        let hoja2 = columna_precio2(&df, "Precio", Some(&mut contador))?;
        assert_ne!(
            hoja1, hoja2,
            "las filas de la segunda hoja no deben colisionar con las de la primera"
        );
        Ok(())
    }

    #[test]
    fn imagenes_http_y_https_se_conservan() -> CoreResult<()> {
        let df = df!(
            "Precio" => ["85", "85", "85", "85"],
            "Tienda" => ["t", "t", "t", "t"],
            "web-scraper-start-url" => ["x/1", "x/2", "x/3", "x/4"],
            "Imagen 1" => [
                "https://i.ebayimg.com/a/b/f.jpg",
                "http://i.ebayimg.com/a/f.jpg",
                "algo video",
                "texto sin url",
            ],
        )?;
        let salida = aplicar_transformaciones_comunes(&df, false, &[])?;
        let imagen1: Vec<_> = salida
            .column("Imagen 1")?
            .str()?
            .iter()
            .map(|v| v.unwrap())
            .collect();
        assert_eq!(imagen1[0], "https://i.ebayimg.com/a/b/s-l1600.jpg");
        assert_eq!(
            imagen1[1], "http://i.ebayimg.com/a/s-l1600.jpg",
            "http también se conserva"
        );
        assert_eq!(imagen1[2], "");
        assert_eq!(imagen1[3], "");
        Ok(())
    }

    #[test]
    fn oem3_rellena_vacio_y_recorta() -> CoreResult<()> {
        let df = df!("OEM" => [Some("  "), Some("ABC123"), None::<&str>])?;
        let salida = aplicar_oem3(&df)?;
        let oem3: Vec<_> = salida.column("OEM3")?.str()?.iter().map(|v| v.unwrap()).collect();
        assert_eq!(oem3, vec!["US123400", "ABC123", "US123400"]);
        let oem: Vec<_> = salida.column("OEM")?.str()?.iter().map(|v| v.unwrap()).collect();
        assert_eq!(
            oem,
            vec!["  ", "ABC123", "US123400"],
            "OEM solo rellena NULOS, no vacíos"
        );
        Ok(())
    }
}
