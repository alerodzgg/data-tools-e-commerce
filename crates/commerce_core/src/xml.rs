use polars::prelude::*;
use std::fmt::Write;

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
    escapar_texto_xml_en(&mut salida, texto);
    salida
}

/// Igual que [`escapar_texto_xml`], pero escribiendo en `salida`.
///
/// La versión que devuelve `String` asigna una vez por celda, y en un archivo
/// de millones de filas eso es la mayor parte del trabajo del serializador.
/// Además toma un camino rápido: si el texto no tiene nada que escapar —el
/// caso de la enorme mayoría de las celdas— se copia de una sola vez en vez
/// de carácter por carácter.
pub(crate) fn escapar_texto_xml_en(salida: &mut String, texto: &str) {
    if !texto
        .bytes()
        .any(|b| b == b'&' || b == b'<' || b == b'>' || (b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r')))
    {
        salida.push_str(texto);
        return;
    }
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
}

/// Serializa UNA celda como inline string (todo texto: los SKUs y códigos
/// nunca se reinterpretan como números). Solo para la cabecera y filas
/// vacías: las filas de datos van por `serializar_bloque_xml`, mucho más
/// rápido al evitar reasignaciones de `String` por celda.
pub(crate) fn celda_xml(valor: Option<&str>, columna: usize, fila: usize) -> String {
    match valor {
        // Una celda vacía no se emite: sin valor, su única función sería
        // ocupar una posición, y para eso ya está la referencia `r` de las
        // celdas que sí vienen después.
        None | Some("") => String::new(),
        Some(texto) => {
            let referencia = format!("{}{fila}", col_letra(columna));
            let escapado = escapar_texto_xml(texto);
            if texto.starts_with(' ') || texto.ends_with(' ') {
                format!(
                    r#"<c r="{referencia}" t="inlineStr"><is><t xml:space="preserve">{escapado}</t></is></c>"#
                )
            } else {
                format!(r#"<c r="{referencia}" t="inlineStr"><is><t>{escapado}</t></is></c>"#)
            }
        }
    }
}

/// Escribe UNA celda no vacía directamente en `salida`.
///
/// Camino caliente del serializador: todo se escribe en el búfer destino, sin
/// `String` intermedios para la referencia ni para el texto escapado.
fn escribir_celda_en(salida: &mut String, texto: &str, letra: &str, fila: usize) {
    let espacio_al_borde = texto.starts_with(' ') || texto.ends_with(' ');
    let _ = write!(salida, r#"<c r="{letra}{fila}" t="inlineStr"><is><t"#);
    if espacio_al_borde {
        salida.push_str(r#" xml:space="preserve""#);
    }
    salida.push('>');
    escapar_texto_xml_en(salida, texto);
    salida.push_str("</t></is></c>");
}

/// Serializa una fila a partir de valores en memoria (cabecera, filas vacías).
///
/// `fila` es el número de fila 1-based dentro de la hoja. Va en el atributo
/// `r`, igual que la referencia de cada celda: ECMA-376 los hace opcionales
/// —Excel infiere la posición por orden de aparición— pero cualquier lector
/// que indexe por referencia recibe una hoja mal armada sin ellos.
pub(crate) fn fila_xml<'a, I>(valores: I, fila: usize) -> String
where
    I: IntoIterator<Item = Option<&'a str>>,
{
    let mut salida = format!(r#"<row r="{fila}">"#);
    for (i, valor) in valores.into_iter().enumerate() {
        salida.push_str(&celda_xml(valor, i + 1, fila));
    }
    salida.push_str("</row>");
    salida
}

/// XML de TODAS las filas de `df` (columnas ya alineadas) en un solo string.
///
/// El bucle ya compila a código nativo, así que aquí se recorre cada columna
/// como `StringChunked` y se ensambla cada fila directamente: sin necesidad
/// de expresiones de Polars para evitar coste de iteración celda a celda.
pub(crate) fn serializar_bloque_xml(
    df: &DataFrame,
    columnas: &[String],
    fila_inicial: usize,
) -> CoreResult<String> {
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

    // La letra de cada columna no cambia entre filas: calcularla una vez por
    // bloque en vez de una vez por celda ahorra una asignación por celda, que
    // a millones de filas es el grueso del trabajo.
    let letras: Vec<String> = (1..=columnas.len()).map(col_letra).collect();

    for desplazamiento in 0..n {
        let fila = fila_inicial + desplazamiento;
        let _ = write!(salida, r#"<row r="{fila}">"#);
        for (i, it) in iters.iter_mut().enumerate() {
            if let Some(texto) = it.next().flatten().filter(|t| !t.is_empty()) {
                escribir_celda_en(&mut salida, texto, &letras[i], fila);
            }
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
        // Ambos caminos deben numerar las filas igual: `fila_xml` se usa para
        // la cabecera y `serializar_bloque_xml` para los datos, y una
        // discrepancia entre ellos daría referencias `r` incoherentes.
        let esperado: String = valores
            .iter()
            .enumerate()
            .map(|(i, v)| fila_xml([Some(*v)], i + 2))
            .collect();
        let obtenido = serializar_bloque_xml(&df, &["A".to_string()], 2)?;
        assert_eq!(obtenido, esperado);
        Ok(())
    }
}
