//! Empaquetado OOXML: qué partes lleva el `.xlsx` y cómo se escribe cada una.
//!
//! Es el único módulo que conoce el formato del archivo. No sabe cuándo hay
//! que rotar de hoja ni qué filas van diferidas — eso es la máquina de estados
//! del escritor. Acá solo: dado un zip y unas hojas ya llenas, producir un
//! libro que Excel abra.

use std::fs::File;
use std::io::{self, Write};

use ::zip::write::{FileOptions, SimpleFileOptions};
use ::zip::{CompressionMethod, ZipWriter};

use super::hoja::Hoja;
use crate::error::CoreResult;
use crate::xml::XML_DECL;

/// El `ZipWriter` mientras el libro sigue abierto.
///
/// Es `None` solo después de `cerrar()`/`abortar()`, y ninguna ruta de
/// escritura llega hasta acá en ese estado (`escribir()` corta antes por
/// `cerrado`). Si alguna llegara sería un bug de este módulo — pero se
/// propaga como error igual: nada acá amerita tumbar el proceso del usuario.
pub(super) fn zip_abierto(zip: &mut Option<ZipWriter<File>>) -> CoreResult<&mut ZipWriter<File>> {
    match zip.as_mut() {
        Some(zip) => Ok(zip),
        None => Err(io::Error::other("EscritorXlsx: zip ya finalizado").into()),
    }
}

/// Escapa un valor que va DENTRO de un atributo XML (p. ej. el nombre de hoja
/// en `xl/workbook.xml`).
///
/// Defensa en profundidad: `hoja::sanear` ya borra los caracteres de control
/// del nombre antes de llegar acá, pero esta función no depende de eso para
/// ser segura por sí misma.
fn escapar_atributo(texto: &str) -> String {
    let sin_control: String = texto.chars().filter(|c| (*c as u32) >= 0x20).collect();
    sin_control
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Vuelca una hoja terminada como `xl/worksheets/sheet{indice}.xml`.
///
/// `indice` es 1-based, como los nombra OOXML. `<dimension>` se emite acá y no
/// antes porque recién ahora se sabe cuántas filas tiene la hoja.
pub(super) fn volcar_hoja(
    zip: &mut Option<ZipWriter<File>>,
    hoja: &mut Hoja,
    indice: usize,
) -> CoreResult<()> {
    let filas_totales = hoja.filas + 1; // +1 por la cabecera
    let dimension = format!(r#"<dimension ref="A1:{}{filas_totales}"/>"#, hoja.col_final);
    let opciones: SimpleFileOptions = FileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(1))
        .large_file(true);

    let zip = zip_abierto(zip)?;
    zip.start_file(format!("xl/worksheets/sheet{indice}.xml"), opciones)?;
    zip.write_all(XML_DECL.as_bytes())?;
    zip.write_all(br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">"#)?;
    zip.write_all(dimension.as_bytes())?;
    zip.write_all(b"<sheetData>")?;
    hoja.tmp.volcar_en(zip)?;
    zip.write_all(b"</sheetData></worksheet>")?;
    Ok(())
}

/// Escribe las cuatro partes que enmarcan a las hojas: tipos de contenido,
/// relaciones raíz, `workbook.xml` y sus relaciones.
///
/// Recibe solo los NOMBRES de las hojas (en orden) porque es todo lo que el
/// empaquetado necesita saber de ellas.
pub(super) fn escribir_estructura(zip: &mut Option<ZipWriter<File>>, nombres: &[String]) -> CoreResult<()> {
    let n = nombres.len();
    let opciones = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let overrides: String = (1..=n)
        .map(|k| {
            format!(
                r#"<Override PartName="/xl/worksheets/sheet{k}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#
            )
        })
        .collect();

    let hojas_xml: String = nombres
        .iter()
        .enumerate()
        .map(|(k, nombre)| {
            let k = k + 1;
            format!(
                r#"<sheet name="{}" sheetId="{k}" r:id="rId{k}"/>"#,
                escapar_atributo(nombre)
            )
        })
        .collect();

    let rels: String = (1..=n)
        .map(|k| {
            format!(
                r#"<Relationship Id="rId{k}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{k}.xml"/>"#
            )
        })
        .collect();

    // Todo el XML queda armado arriba: acá abajo solo se vuelca, con el
    // `Option` del zip resuelto una vez para las cuatro partes.
    let zip = zip_abierto(zip)?;

    zip.start_file("[Content_Types].xml", opciones)?;
    write!(
        zip,
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">"#,
            r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
            r#"<Default Extension="xml" ContentType="application/xml"/>"#,
            r#"<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>"#,
            "{overrides}</Types>",
        ),
        overrides = overrides
    )?;

    zip.start_file("_rels/.rels", opciones)?;
    write!(
        zip,
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
        )
    )?;

    zip.start_file("xl/workbook.xml", opciones)?;
    write!(
        zip,
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">"#,
            "<sheets>{hojas_xml}</sheets></workbook>",
        ),
        hojas_xml = hojas_xml
    )?;

    zip.start_file("xl/_rels/workbook.xml.rels", opciones)?;
    write!(
        zip,
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            "{rels}</Relationships>",
        ),
        rels = rels
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapar_atributo_neutraliza_lo_que_rompe_el_xml() {
        assert_eq!(escapar_atributo(r#"A&B<C>D"E"#), "A&amp;B&lt;C&gt;D&quot;E");
    }

    #[test]
    fn escapar_atributo_borra_controles_aunque_sanear_ya_los_haya_quitado() {
        // Defensa en profundidad: un control crudo en un atributo produce un
        // workbook.xml que Excel rechaza entero, no una celda fea.
        assert_eq!(escapar_atributo("a\u{0}b\u{1f}c"), "abc");
    }

    #[test]
    fn sin_zip_abierto_se_devuelve_error_en_vez_de_panic() {
        let mut cerrado: Option<ZipWriter<File>> = None;
        assert!(zip_abierto(&mut cerrado).is_err());
        assert!(escribir_estructura(&mut cerrado, &["Hoja1".to_string()]).is_err());
    }
}
