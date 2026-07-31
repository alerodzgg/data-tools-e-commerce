//! Validación y detección de URLs de imagen: `UrlHelper`.
//!
//! Nota de tipos: el llamador ya extrae `Option<&str>` de la celda tipada
//! (las celdas de un `DataFrame` de Polars pueden traer cualquier tipo
//! dinámico), así que el caso "no es string" se resuelve en el propio
//! sistema de tipos: `is_image`/`split_image_urls` no necesitan una rama
//! explícita para eso.

use std::sync::LazyLock;

use polars::prelude::*;
use regex::Regex;

const IMAGE_EXT_SUFFIXES: [&str; 7] = [".jpg", ".jpeg", ".png", ".webp", ".bmp", ".tiff", ".gif"];

// Corta en el primer separador/espacio, nunca a mitad de la siguiente URL.
// El esquema es case-insensitive (solo esa parte, vía el grupo `(?i:...)`):
// un origen que traiga "Http://" o "HTTPS://" (p. ej. autocorrección de
// Excel) es tan válido como en minúsculas.
#[allow(clippy::unwrap_used)] // patrón regex literal fijo, válido por construcción
static URL_PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i:https?)://[^\s,;]+").unwrap());

/// `True` si `value` es una URL http(s) cuyo path (sin query string) termina
/// en una extensión de imagen conocida. El esquema se compara sin distinguir
/// mayúsculas/minúsculas (ver nota en [`URL_PATTERN`]).
pub fn is_image(value: Option<&str>) -> bool {
    let Some(text) = value.map(str::trim) else {
        return false;
    };
    let esquema = text.get(..8).unwrap_or(text).to_lowercase();
    if !(esquema.starts_with("http://") || esquema.starts_with("https://")) {
        return false;
    }
    let path = text.split('?').next().unwrap_or("").to_lowercase();
    IMAGE_EXT_SUFFIXES.iter().any(|ext| path.ends_with(ext))
}

/// Extrae, en orden de aparición, las URLs de imagen de una celda que puede
/// traer una o varias (separadas por coma, punto y coma, salto de línea, con
/// o sin espacio). Lista vacía si no hay ninguna.
///
/// La detección sigue siendo por extensión conocida (vía `is_image`): URLs de
/// servidores de imágenes sin extensión en el path no se reconocen a propósito,
/// para no arriesgar falsos positivos sobre URLs genéricas.
pub fn split_image_urls(value: Option<&str>) -> Vec<String> {
    let Some(text) = value else {
        return Vec::new();
    };
    if text.trim().is_empty() {
        return Vec::new();
    }
    URL_PATTERN
        .find_iter(text)
        .map(|m| m.as_str().to_string())
        .filter(|u| is_image(Some(u)))
        .collect()
}

/// Detecta columnas que mayoritariamente contienen URLs de imagen, mirando
/// una muestra de hasta `sample_size` valores no nulos por columna. Columnas
/// que no son de tipo string se descartan sin error (nunca pueden contener
/// una URL).
pub fn auto_detect_columns(df: &DataFrame, sample_size: usize) -> PolarsResult<Vec<String>> {
    let n = sample_size.min(df.height());
    if n == 0 {
        return Ok(Vec::new());
    }
    let threshold = ((n as f32 * 0.3) as usize).max(1);

    let mut detected = Vec::new();
    for name in df.get_column_names() {
        if name.as_str().starts_with('_') {
            continue;
        }
        let column = df.column(name)?;
        if column.dtype() != &DataType::String {
            continue;
        }
        let hits = column
            .str()?
            .iter()
            .flatten()
            .take(n)
            .filter(|v| is_image(Some(v)))
            .count();
        if hits >= threshold {
            detected.push(name.to_string());
        }
    }
    Ok(detected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_image_exige_esquema_http_y_extension_conocida() {
        assert!(is_image(Some("http://x.com/1.jpg")));
        assert!(is_image(Some("https://x.com/1.JPG?w=100"))); // case-insensitive, ignora query
        assert!(!is_image(Some("http://x.com/pagina.html")));
        assert!(!is_image(Some("ftp://x.com/1.jpg")));
        assert!(!is_image(None));
    }

    #[test]
    fn is_image_no_confunde_gifted_com_con_extension_gif() {
        // `.endswith` ancla al final del path, no es un substring en cualquier parte.
        assert!(!is_image(Some("http://foo.gifted.com/x")));
    }

    #[test]
    fn is_image_acepta_esquema_en_mayusculas() {
        assert!(is_image(Some("HTTP://x.com/1.jpg")));
        assert!(is_image(Some("Https://x.com/1.jpg")));
    }

    #[test]
    fn split_image_urls_reconoce_esquema_en_mayusculas() {
        assert_eq!(
            split_image_urls(Some("HTTPS://x.com/1.jpg")),
            vec!["HTTPS://x.com/1.jpg".to_string()]
        );
    }

    #[test]
    fn split_image_urls_varias_en_una_celda() {
        let celda = "http://x.com/1.jpg,http://x.com/2.png otro texto http://x.com/3.gif";
        assert_eq!(
            split_image_urls(Some(celda)),
            vec![
                "http://x.com/1.jpg".to_string(),
                "http://x.com/2.png".to_string(),
                "http://x.com/3.gif".to_string(),
            ]
        );
    }

    #[test]
    fn split_image_urls_sin_urls_de_imagen() {
        assert_eq!(split_image_urls(Some("sin urls aqui")), Vec::<String>::new());
        assert_eq!(
            split_image_urls(Some("http://x.com/pagina.html")),
            Vec::<String>::new()
        );
        assert_eq!(split_image_urls(None), Vec::<String>::new());
    }

    #[test]
    fn auto_detect_columns_con_pocas_filas_escala_el_umbral_a_la_muestra_real() {
        let df = df! {
            "Sku" => ["A1", "A2"],
            "Fotos" => ["http://x.com/1.jpg", "http://x.com/2.jpg"],
        }
        .unwrap();
        assert_eq!(auto_detect_columns(&df, 10).unwrap(), vec!["Fotos".to_string()]);
    }

    #[test]
    fn auto_detect_columns_ignora_columnas_privadas_y_no_string() {
        let df = df! {
            "_interno" => ["http://x.com/1.jpg", "http://x.com/2.jpg"],
            "Precio" => [10i64, 20i64],
            "Notas" => ["hola", "sin urls"],
        }
        .unwrap();
        assert_eq!(auto_detect_columns(&df, 10).unwrap(), Vec::<String>::new());
    }
}
