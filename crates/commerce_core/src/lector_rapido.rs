//! Camino rápido de lectura para los `.xlsx` que escribe este mismo workspace.
//!
//! `calamine` es un lector genérico: soporta xls, xlsb, ods, estilos, fórmulas
//! y fechas, y paga ese costo en cada celda. Medido sobre 6M de celdas, tarda
//! 10,2 s donde descomprimir y recorrer el XML cuesta 3,9 s — el resto es
//! generalidad que acá no se usa.
//!
//! Este módulo lee ÚNICAMENTE la forma exacta que produce [`EscritorXlsx`]:
//! celdas `<c r=".." t="inlineStr"><is><t>texto</t></is></c>`, sin tabla de
//! cadenas compartidas ni estilos. Ante cualquier cosa que no encaje devuelve
//! `None` y el llamador cae a `calamine` — **nunca adivina**. El costo de
//! equivocarse es leer lento, no leer mal.
//!
//! El resultado debe ser IDÉNTICO al del camino con `calamine`: por eso
//! comparte con él `nombres_cabecera` y `renombrar_canonico` en vez de
//! reimplementar el nombrado de columnas.

use std::io::Read;
use std::path::Path;

use polars::prelude::*;
use quick_xml::events::Event;

use crate::cabeceras::{nombres_cabecera, renombrar_canonico};

/// Techo de bytes descomprimidos por hoja. Sin esto, un `.xlsx` manipulado
/// haría reservar memoria sin límite antes de mirar su contenido.
const MAX_XML_HOJA: u64 = 512 * 1024 * 1024;

/// Lee todas las hojas de `archivo`, o `None` si el libro no tiene la forma
/// que este módulo sabe leer.
///
/// `excluir` son nombres de hoja ya normalizados (recortados y en minúsculas),
/// igual que en el camino con `calamine`.
pub(crate) fn leer_hojas(
    archivo: &Path,
    excluir: &std::collections::HashSet<String>,
) -> Option<Vec<DataFrame>> {
    let mut zip = ::zip::ZipArchive::new(std::fs::File::open(archivo).ok()?).ok()?;

    // Una tabla de cadenas compartidas significa que el libro lo escribió otro
    // programa: este módulo no la resuelve y no va a intentarlo.
    if zip.by_name("xl/sharedStrings.xml").is_ok() {
        return None;
    }

    let nombres = nombres_de_hojas(&mut zip)?;
    let mut salida = Vec::new();
    for (indice, nombre) in nombres.iter().enumerate() {
        if excluir.contains(&nombre.trim().to_lowercase()) {
            continue;
        }
        let xml = leer_entrada(&mut zip, &format!("xl/worksheets/sheet{}.xml", indice + 1))?;
        let df = hoja_a_dataframe(&xml)?;
        if df.height() > 0 || df.width() > 0 {
            salida.push(df);
        }
    }
    Some(salida)
}

/// Nombres de hoja, en orden, leídos de `xl/workbook.xml`.
///
/// Se asume la correspondencia posicional con `sheet{N}.xml` que produce
/// nuestro escritor. Si el libro usa otro orden de relaciones, las hojas
/// saldrían cruzadas — por eso cualquier estructura inesperada aborta el
/// camino rápido en vez de arriesgar un resultado mal armado.
fn nombres_de_hojas<R: Read + std::io::Seek>(zip: &mut ::zip::ZipArchive<R>) -> Option<Vec<String>> {
    let xml = leer_entrada(zip, "xl/workbook.xml")?;
    let mut lector = quick_xml::Reader::from_reader(xml.as_slice());
    let mut buf = Vec::new();
    let mut nombres = Vec::new();
    loop {
        match lector.read_event_into(&mut buf) {
            Ok(Event::Empty(e) | Event::Start(e)) if e.name().into_inner() == b"sheet" => {
                let atributo = e.try_get_attribute("name").ok()??;
                let crudo = String::from_utf8_lossy(&atributo.value);
                let valor = quick_xml::escape::unescape(&crudo).ok()?;
                nombres.push(valor.into_owned());
            }
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
    (!nombres.is_empty()).then_some(nombres)
}

fn leer_entrada<R: Read + std::io::Seek>(zip: &mut ::zip::ZipArchive<R>, nombre: &str) -> Option<Vec<u8>> {
    let mut entrada = zip.by_name(nombre).ok()?;
    if entrada.size() > MAX_XML_HOJA {
        return None;
    }
    let mut datos = Vec::with_capacity(entrada.size() as usize);
    entrada.read_to_end(&mut datos).ok()?;
    Some(datos)
}

/// Columna 0-based a partir de una referencia de celda tipo `"BC12"`.
fn columna_de_ref(referencia: &[u8]) -> Option<usize> {
    let mut n = 0usize;
    for b in referencia {
        if b.is_ascii_alphabetic() {
            n = n
                .checked_mul(26)?
                .checked_add((b.to_ascii_uppercase() - b'A' + 1) as usize)?;
        } else {
            break;
        }
    }
    n.checked_sub(1)
}

/// Convierte el XML de UNA hoja en un `DataFrame`, o `None` si encuentra algo
/// que este camino no cubre (una celda que no sea `inlineStr`, por ejemplo).
///
/// Es SECUENCIAL a propósito. Se probó repartir el parseo entre los 12 núcleos
/// cortando el buffer en límites `<row`: dio resultados idénticos pero tardó
/// 10,7 s contra 4,6 s del camino de un solo hilo. El parseo no es el cuello
/// que parecía —lo es la construcción de las columnas, que al repartirse
/// multiplica la memoria y termina peleando con el asignador— así que la
/// versión paralela quedó descartada por medición, no por complejidad.
fn hoja_a_dataframe(xml: &[u8]) -> Option<DataFrame> {
    parsear_fragmento(xml, None)
}

/// Parsea un fragmento de hoja. Con `nombres` en `Some` toma esos nombres de
/// columna y trata TODAS las filas como datos; con `None` toma la primera fila
/// como cabecera.
fn parsear_fragmento(xml: &[u8], nombres: Option<&[String]>) -> Option<DataFrame> {
    let mut lector = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();

    let hay_cabecera_propia = nombres.is_none();
    // Se construye directo sobre el buffer contiguo de Arrow: un
    // `Vec<Option<String>>` intermedio obliga a polars a copiar cada celda una
    // segunda vez al convertirlo a su representación interna.
    let mut columnas: Vec<StringChunkedBuilder> = Vec::new();
    let mut fila: Vec<Option<String>> = Vec::new();
    let mut columna = 0usize;
    let mut en_texto = false;
    let mut filas_vistas = usize::from(!hay_cabecera_propia);
    if let Some(dados) = nombres {
        columnas = dados
            .iter()
            .map(|n| StringChunkedBuilder::new(n.as_str().into(), 0))
            .collect();
    }

    loop {
        match lector.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.name().into_inner() {
                b"c" => {
                    // Sin `t="inlineStr"` el valor vive en otro lado (tabla
                    // compartida, número, fórmula): no es nuestro formato.
                    match e.try_get_attribute("t").ok()? {
                        Some(t) if t.value.as_ref() == b"inlineStr" => {}
                        Some(_) => return None,
                        None => return None,
                    }
                    columna = columna_de_ref(&e.try_get_attribute("r").ok()??.value)?;
                    if fila.len() <= columna {
                        fila.resize(columna + 1, None);
                    }
                }
                b"t" => en_texto = true,
                _ => {}
            },
            Ok(Event::Text(e)) if en_texto => {
                // `unescape` asigna y recorre siempre; la enorme mayoría de
                // las celdas no tiene ninguna entidad que resolver.
                let crudo: &[u8] = e.as_ref();
                let texto = if crudo.contains(&b'&') {
                    e.unescape().ok()?.into_owned()
                } else {
                    String::from_utf8_lossy(crudo).into_owned()
                };
                if let Some(celda) = fila.get_mut(columna) {
                    *celda = Some(texto);
                }
            }
            Ok(Event::End(e)) => match e.name().into_inner() {
                b"t" => en_texto = false,
                b"row" => {
                    if filas_vistas == 0 {
                        columnas = nombres_cabecera(std::mem::take(&mut fila))
                            .into_iter()
                            .map(|nombre| StringChunkedBuilder::new(nombre.as_str().into(), 0))
                            .collect();
                    } else {
                        fila.resize(columnas.len(), None);
                        for (destino, valor) in columnas.iter_mut().zip(fila.iter_mut()) {
                            match valor.take() {
                                Some(texto) => destino.append_value(texto.as_str()),
                                None => destino.append_null(),
                            }
                        }
                        fila.clear();
                    }
                    filas_vistas += 1;
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }

    if columnas.is_empty() {
        return Some(DataFrame::empty());
    }

    let series: Vec<Column> = columnas
        .into_iter()
        .map(|c| c.finish().into_series().into())
        .collect();
    let df = DataFrame::new_infer_height(series).ok()?;
    renombrar_canonico(df).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn una_referencia_de_celda_se_traduce_a_su_columna() {
        assert_eq!(columna_de_ref(b"A1"), Some(0));
        assert_eq!(columna_de_ref(b"B12"), Some(1));
        assert_eq!(columna_de_ref(b"Z1"), Some(25));
        assert_eq!(columna_de_ref(b"AA1"), Some(26));
        assert_eq!(columna_de_ref(b"XFD1"), Some(16_383));
    }

    #[test]
    fn una_referencia_sin_letras_no_da_columna_cero() {
        // Sin el `checked_sub`, `""` daría `0usize.wrapping_sub(1)` y la celda
        // aterrizaría en una columna absurda en vez de abortar el camino.
        assert_eq!(columna_de_ref(b"1"), None);
        assert_eq!(columna_de_ref(b""), None);
    }

    #[test]
    fn una_celda_que_no_es_inline_string_aborta_el_camino_rapido() {
        // Un libro de otro programa usa `t="s"` (tabla compartida) o celdas
        // numéricas: no las sabemos leer, y devolver `None` manda al llamador
        // a `calamine` en vez de producir datos mal.
        let xml =
            br#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row></sheetData></worksheet>"#;
        assert!(hoja_a_dataframe(xml).is_none());
    }

    #[test]
    fn una_hoja_de_nuestro_formato_se_lee_completa() {
        let xml = br#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Sku</t></is></c><c r="B1" t="inlineStr"><is><t>Precio</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>007</t></is></c><c r="B2" t="inlineStr"><is><t>1.50</t></is></c></row></sheetData></worksheet>"#;
        let df = hoja_a_dataframe(xml).expect("es nuestro formato");
        assert_eq!(df.height(), 1);
        assert_eq!(df.get_column_names_owned(), vec!["Sku", "Precio"]);
        assert_eq!(df.column("Sku").unwrap().str().unwrap().get(0), Some("007"));
    }

    #[test]
    fn las_celdas_salteadas_quedan_vacias_en_su_columna() {
        // El escritor omite las celdas sin valor: la referencia `r` es lo que
        // vuelve a ubicar las que sí vienen.
        let xml = br#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>A</t></is></c><c r="B1" t="inlineStr"><is><t>B</t></is></c></row><row r="2"><c r="B2" t="inlineStr"><is><t>solo B</t></is></c></row></sheetData></worksheet>"#;
        let df = hoja_a_dataframe(xml).expect("es nuestro formato");
        assert_eq!(df.column("A").unwrap().str().unwrap().get(0), None);
        assert_eq!(df.column("B").unwrap().str().unwrap().get(0), Some("solo B"));
    }

    #[test]
    fn el_texto_escapado_vuelve_a_su_forma_original() {
        let xml = br#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Col</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>a&amp;b&lt;c</t></is></c></row></sheetData></worksheet>"#;
        let df = hoja_a_dataframe(xml).expect("es nuestro formato");
        assert_eq!(df.column("Col").unwrap().str().unwrap().get(0), Some("a&b<c"));
    }
}
