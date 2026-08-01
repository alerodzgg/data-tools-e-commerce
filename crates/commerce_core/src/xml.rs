use polars::prelude::*;

use crate::error::CoreResult;

pub(crate) const XML_DECL: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;

/// Límite REAL de Excel: 1 048 576 filas por hoja, cabecera incluida.
pub const MAX_FILAS_EXCEL: usize = 1_048_575;

/// Índice de columna (1-based) → letra Excel: 1→A, 27→AA, 52→AZ.
pub(crate) fn col_letra(n: usize) -> String {
    let mut n = n.max(1);
    let mut letra = Vec::new();
    while n > 0 {
        let resto = (n - 1) % 26;
        letra.push((b'A' + resto as u8) as char);
        n = (n - 1) / 26;
    }
    letra.iter().rev().collect()
}

/// Escapa texto para XML (entidades de `& < >`) y borra los caracteres de
/// control ilegales en XML 1.0 (0x00-0x1F salvo tab/LF/CR), que de colarse
/// corromperían el .xlsx. Un único barrido, así que a diferencia de una
/// secuencia de `replace_all` no hay riesgo de reescapar el `&` de `&lt;`.
pub(crate) fn escapar_texto_xml(texto: &str) -> String {
    let mut salida = String::with_capacity(texto.len());
    for c in texto.chars() {
        match c {
            '&' => salida.push_str("&amp;"),
            '<' => salida.push_str("&lt;"),
            '>' => salida.push_str("&gt;"),
            '\t' | '\n' | '\r' => salida.push(c),
            c if (c as u32) < 0x20 => {} // caracter de control ilegal: se borra
            c => salida.push(c),
        }
    }
    salida
}

/// Serializa UNA celda como inline string (todo texto: los SKUs y códigos
/// nunca se reinterpretan como números). Solo para la cabecera y filas
/// vacías: las filas de datos van por `serializar_bloque_xml`, mucho más
/// rápido al evitar reasignaciones de `String` por celda.
pub(crate) fn celda_xml(valor: Option<&str>) -> String {
    match valor {
        None => "<c/>".to_string(),
        Some("") => "<c/>".to_string(),
        Some(texto) => {
            let escapado = escapar_texto_xml(texto);
            if texto.starts_with(' ') || texto.ends_with(' ') {
                format!(r#"<c t="inlineStr"><is><t xml:space="preserve">{escapado}</t></is></c>"#)
            } else {
                format!(r#"<c t="inlineStr"><is><t>{escapado}</t></is></c>"#)
            }
        }
    }
}

/// Serializa una fila a partir de valores en memoria (cabecera, filas vacías).
pub(crate) fn fila_xml<'a, I>(valores: I) -> String
where
    I: IntoIterator<Item = Option<&'a str>>,
{
    let mut salida = String::from("<row>");
    for valor in valores {
        salida.push_str(&celda_xml(valor));
    }
    salida.push_str("</row>");
    salida
}

/// XML de TODAS las filas de `df` (columnas ya alineadas) en un solo string.
///
/// El bucle ya compila a código nativo, así que aquí se recorre cada columna
/// como `StringChunked` y se ensambla cada fila directamente: sin necesidad
/// de expresiones de Polars para evitar coste de iteración celda a celda.
pub(crate) fn serializar_bloque_xml(df: &DataFrame, columnas: &[String]) -> CoreResult<String> {
    if df.height() == 0 {
        return Ok(String::new());
    }

    let mut chunked: Vec<StringChunked> = Vec::with_capacity(columnas.len());
    for nombre in columnas {
        let serie = df.column(nombre)?.as_materialized_series().clone();
        let serie = if serie.dtype() == &DataType::String {
            serie
        } else {
            serie.cast(&DataType::String)?
        };
        chunked.push(serie.str()?.clone());
    }

    let n = df.height();
    let mut salida = String::with_capacity(n * 32);
    let mut iters: Vec<_> = chunked.iter().map(polars::prelude::ChunkedArray::iter).collect();

    for _ in 0..n {
        salida.push_str("<row>");
        for it in iters.iter_mut() {
            let valor = it.next().flatten();
            salida.push_str(&celda_xml(valor));
        }
        salida.push_str("</row>");
    }
    Ok(salida)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn col_letra_basica() {
        assert_eq!(col_letra(1), "A");
        assert_eq!(col_letra(27), "AA");
        assert_eq!(col_letra(52), "AZ");
    }

    #[test]
    fn bloque_xml_identico_a_fila_xml() -> CoreResult<()> {
        let valores = ["a&b", "<tag>", " espacios ", "", "x\u{1}y", "normal"];
        let df = df!("A" => valores)?;
        let esperado: String = valores.iter().map(|v| fila_xml([Some(*v)])).collect();
        let obtenido = serializar_bloque_xml(&df, &["A".to_string()])?;
        assert_eq!(obtenido, esperado);
        Ok(())
    }
}
