use std::collections::HashSet;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use calamine::{open_workbook_auto, Data, Range, Reader, Sheets};
use polars::prelude::*;

use crate::cabeceras::{nombres_cabecera, renombrar_canonico};
use crate::error::{CoreError, CoreResult};

fn normalizar_excluir(excluir: Option<&[&str]>) -> HashSet<String> {
    excluir
        .unwrap_or(&[])
        .iter()
        .map(|h| h.trim().to_lowercase())
        .collect()
}

/// Convierte una celda de calamine a su representación de texto canónica.
/// Todo se trata como texto: los SKUs y códigos como '007' nunca se
/// reinterpretan como números.
fn celda_a_texto(valor: &Data) -> Option<String> {
    match valor {
        Data::Empty => None,
        Data::String(s) => Some(s.clone()),
        Data::Int(i) => Some(i.to_string()),
        Data::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                Some(format!("{}", *f as i64))
            } else {
                Some(f.to_string())
            }
        }
        Data::Bool(b) => Some(if *b {
            "TRUE".to_string()
        } else {
            "FALSE".to_string()
        }),
        Data::DateTime(dt) => Some(dt.to_string()),
        Data::DateTimeIso(s) => Some(s.clone()),
        Data::DurationIso(s) => Some(s.clone()),
        Data::Error(e) => Some(format!("{e:?}")),
    }
}

/// Una hoja completa (cabecera + filas) → DataFrame con nombres canónicos,
/// todo en columnas `String`.
pub(crate) fn hoja_a_dataframe(rango: &Range<Data>) -> CoreResult<DataFrame> {
    let mut filas = rango.rows();
    let Some(cabecera) = filas.next() else {
        return Ok(DataFrame::empty());
    };
    let nombres = nombres_cabecera(cabecera.iter().map(celda_a_texto));

    let ancho = nombres.len();
    let mut columnas: Vec<Vec<Option<String>>> = vec![Vec::new(); ancho];
    for fila in filas {
        for (i, columna) in columnas.iter_mut().enumerate() {
            columna.push(fila.get(i).and_then(celda_a_texto));
        }
    }

    let series: Vec<Column> = nombres
        .into_iter()
        .zip(columnas)
        .map(|(nombre, valores)| Column::new(nombre.into(), valores))
        .collect();
    let df = DataFrame::new_infer_height(series)?;
    renombrar_canonico(df).map_err(Into::into)
}

/// Itera las hojas de un XLSX (todo en columnas `String`, nombres
/// canónicos), abriendo el libro UNA sola vez con calamine.
///
/// `avisar`: se llama con un mensaje si el libro o una hoja no se pueden
/// leer (y se salta). Nunca aborta: un archivo corrupto/truncado deja de
/// aportar hojas, no tumba el procesamiento del resto de archivos.
pub fn iter_hojas_xlsx(
    archivo: &Path,
    excluir: Option<&[&str]>,
    mut avisar: impl FnMut(&str),
) -> Vec<DataFrame> {
    let excluir = normalizar_excluir(excluir);
    let mut salida = Vec::new();

    let mut libro = match open_workbook_auto(archivo) {
        Ok(l) => l,
        Err(error) => {
            avisar(&format!(
                "No se pudo abrir '{}': {error}",
                archivo.file_name().unwrap_or_default().to_string_lossy()
            ));
            return salida;
        }
    };

    let nombres_hojas: Vec<String> = libro.sheet_names().to_vec();
    for nombre in nombres_hojas {
        if excluir.contains(&nombre.trim().to_lowercase()) {
            continue;
        }
        match libro.worksheet_range(&nombre) {
            Ok(rango) => match hoja_a_dataframe(&rango) {
                Ok(df) if df.height() > 0 => salida.push(df),
                Ok(_) => {}
                Err(error) => avisar(&format!(
                    "Error al leer '{}' / '{nombre}': {error}",
                    archivo.file_name().unwrap_or_default().to_string_lossy()
                )),
            },
            Err(error) => avisar(&format!(
                "Error al leer '{}' / '{nombre}': {error}",
                archivo.file_name().unwrap_or_default().to_string_lossy()
            )),
        }
    }
    salida
}

/// Libro XLSX abierto, para streamear hoja por hoja sin tener todo el
/// workbook en memoria a la vez (necesario cuando una sola hoja no cabe
/// cómodamente, o cuando el llamador necesita leer hojas por NOMBRE en un
/// orden concreto, p. ej. "Hoja1" primero y el resto después).
pub type LibroXlsx = Sheets<BufReader<File>>;

/// Abre el libro una sola vez (para leer hojas individuales después con
/// [`nombres_hojas_libro`]/[`leer_hoja_por_nombre`]). Error claro si el
/// archivo está corrupto o no es un XLSX válido — nunca un pánico crudo.
pub fn abrir_libro(archivo: &Path) -> CoreResult<LibroXlsx> {
    open_workbook_auto(archivo).map_err(|error| CoreError::AperturaLibro {
        ruta: archivo.to_path_buf(),
        fuente: Box::new(error),
    })
}

/// Nombres de hoja del libro, en el orden del archivo.
pub fn nombres_hojas_libro(libro: &LibroXlsx) -> Vec<String> {
    libro.sheet_names()
}

/// Lee UNA hoja del libro (por nombre) como DataFrame de texto, con nombres
/// de columna canónicos. `archivo` solo se usa para el mensaje de error.
pub fn leer_hoja_por_nombre(libro: &mut LibroXlsx, archivo: &Path, hoja: &str) -> CoreResult<DataFrame> {
    let rango = libro
        .worksheet_range(hoja)
        .map_err(|error| CoreError::LecturaHoja {
            archivo: archivo.to_path_buf(),
            hoja: hoja.to_string(),
            fuente: Box::new(error),
        })?;
    hoja_a_dataframe(&rango)
}

/// Filas de datos (sin contar la cabecera) de UNA hoja ya abierta. Cuenta
/// filas físicas sin construir el `DataFrame`: más barato cuando solo hace
/// falta el total para una barra de progreso.
pub fn contar_filas_hoja(libro: &mut LibroXlsx, archivo: &Path, hoja: &str) -> CoreResult<u64> {
    let rango = libro
        .worksheet_range(hoja)
        .map_err(|error| CoreError::LecturaHoja {
            archivo: archivo.to_path_buf(),
            hoja: hoja.to_string(),
            fuente: Box::new(error),
        })?;
    Ok(rango.rows().count().saturating_sub(1) as u64)
}

/// Unión, en orden de primera aparición, de las columnas de todos los
/// archivos (nombres canónicos), saltando las hojas de `excluir`.
///
/// `avisar`: se llama con un mensaje si un archivo/hoja no se puede leer (y
/// se salta), igual que en [`iter_hojas_xlsx`]. Sin ese canal, un archivo
/// ilegible sería indistinguible de uno sin columnas.
pub fn columnas_union(
    archivos: &[PathBuf],
    excluir: Option<&[&str]>,
    mut avisar: impl FnMut(&str),
) -> Vec<String> {
    let excluir = normalizar_excluir(excluir);
    let mut columnas: Vec<String> = Vec::new();
    let mut vistas: HashSet<String> = HashSet::new();

    let agregar = |nombres: Vec<String>, columnas: &mut Vec<String>, vistas: &mut HashSet<String>| {
        for columna in nombres {
            if vistas.insert(columna.clone()) {
                columnas.push(columna);
            }
        }
    };

    for archivo in archivos {
        let nombre_archivo = archivo.file_name().unwrap_or_default().to_string_lossy();
        let extension = archivo
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        if extension == "csv" {
            let Some(ruta_str) = archivo.to_str() else {
                avisar(&format!(
                    "No se pudo leer la ruta de '{nombre_archivo}' (no es UTF-8 válida)."
                ));
                continue;
            };
            match LazyCsvReader::new(PlRefPath::from(ruta_str))
                .with_infer_schema_length(Some(0))
                .with_n_rows(Some(0))
                .finish()
                .and_then(polars::prelude::LazyFrame::collect)
            {
                Ok(df) => agregar(
                    df.get_column_names().iter().map(|s| s.to_string()).collect(),
                    &mut columnas,
                    &mut vistas,
                ),
                Err(error) => avisar(&format!("No se pudo leer '{nombre_archivo}': {error}")),
            }
            continue;
        }

        let mut libro = match open_workbook_auto(archivo) {
            Ok(l) => l,
            Err(error) => {
                avisar(&format!("No se pudo abrir '{nombre_archivo}': {error}"));
                continue;
            }
        };
        for hoja in libro.sheet_names().to_vec() {
            if excluir.contains(&hoja.trim().to_lowercase()) {
                continue;
            }
            match libro.worksheet_range(&hoja) {
                Ok(rango) => {
                    if let Some(cabecera) = rango.rows().next() {
                        let nombres = nombres_cabecera(cabecera.iter().map(celda_a_texto));
                        agregar(nombres, &mut columnas, &mut vistas);
                    }
                }
                Err(error) => avisar(&format!("Error al leer '{nombre_archivo}' / '{hoja}': {error}")),
            }
        }
    }
    columnas
}

/// Unión, en orden de primera aparición, de las columnas de bloques YA
/// CARGADOS en memoria (p. ej. los que devuelve [`iter_hojas_xlsx`]) — sin
/// abrir ningún archivo. Para un llamador que de todos modos va a cargar los
/// datos completos, esto reemplaza a [`columnas_union`] sin la apertura
/// extra que haría falta solo para leer las cabeceras (calamine no
/// soporta leer solo la cabecera sin materializar la hoja completa, así que
/// esa apertura "solo para columnas" costaba lo mismo que la de los datos).
pub fn columnas_union_de_bloques(bloques: &[DataFrame]) -> Vec<String> {
    let mut columnas: Vec<String> = Vec::new();
    let mut vistas: HashSet<String> = HashSet::new();
    for bloque in bloques {
        for nombre in bloque.get_column_names() {
            let nombre = nombre.to_string();
            if vistas.insert(nombre.clone()) {
                columnas.push(nombre);
            }
        }
    }
    columnas
}

/// Filas de datos totales (para el total de las barras de progreso). XLSX
/// vía metadatos de la hoja (sin materializar los datos), saltando las hojas
/// excluidas. Los CSV se cuentan con un `scan` + `len()` perezoso.
///
/// `avisar`: mismo contrato que en [`columnas_union`]/[`iter_hojas_xlsx`].
pub fn total_filas(
    archivos: &[PathBuf],
    excluir: Option<&[&str]>,
    mut avisar: impl FnMut(&str),
) -> Option<u64> {
    let excluir = normalizar_excluir(excluir);
    let mut total: u64 = 0;

    for archivo in archivos {
        let nombre_archivo = archivo.file_name().unwrap_or_default().to_string_lossy();
        let extension = archivo
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        if extension == "csv" {
            match archivo.to_str() {
                Some(ruta_str) => match LazyCsvReader::new(PlRefPath::from(ruta_str))
                    .with_infer_schema_length(Some(0))
                    .with_ignore_errors(true)
                    .finish()
                    .and_then(|lf| lf.select([len()]).collect())
                    .and_then(|df| df.column("len")?.u32().map(|c| c.get(0).unwrap_or(0) as u64))
                {
                    Ok(n) => total += n,
                    Err(error) => avisar(&format!("No se pudo contar filas de '{nombre_archivo}': {error}")),
                },
                None => avisar(&format!(
                    "No se pudo leer la ruta de '{nombre_archivo}' (no es UTF-8 válida)."
                )),
            }
            continue;
        }

        match open_workbook_auto(archivo) {
            Ok(mut libro) => {
                for hoja in libro.sheet_names().to_vec() {
                    if excluir.contains(&hoja.trim().to_lowercase()) {
                        continue;
                    }
                    match libro.worksheet_range(&hoja) {
                        Ok(rango) => {
                            let filas = rango.rows().count();
                            total += filas.saturating_sub(1) as u64;
                        }
                        Err(error) => {
                            avisar(&format!("Error al leer '{nombre_archivo}' / '{hoja}': {error}"))
                        }
                    }
                }
            }
            Err(error) => avisar(&format!("No se pudo abrir '{nombre_archivo}': {error}")),
        }
    }
    if total == 0 {
        None
    } else {
        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escribir_libro_prueba(ruta: &Path) {
        use rust_xlsxwriter::Workbook;
        let mut wb = Workbook::new();
        let hoja = wb.add_worksheet();
        hoja.write(0, 0, "A").unwrap();
        hoja.write(0, 1, "B").unwrap();
        hoja.write(1, 0, "1").unwrap();
        hoja.write(1, 1, "2").unwrap();
        wb.save(ruta).unwrap();
    }

    #[test]
    fn iter_hojas_xlsx_ante_archivo_corrupto_no_revienta_y_avisa() {
        let tmp = tempfile::tempdir().unwrap();
        let bueno = tmp.path().join("bueno.xlsx");
        escribir_libro_prueba(&bueno);
        let bytes = std::fs::read(&bueno).unwrap();
        let truncado_ruta = tmp.path().join("truncado.xlsx");
        std::fs::write(&truncado_ruta, &bytes[..bytes.len() / 2]).unwrap();

        let mut avisos = Vec::new();
        let hojas = iter_hojas_xlsx(&truncado_ruta, None, |m| avisos.push(m.to_string()));
        assert!(hojas.is_empty());
        assert!(!avisos.is_empty());
        assert!(avisos[0].contains("No se pudo abrir"));
    }

    #[test]
    fn columnas_union_ante_archivo_corrupto_no_revienta_y_avisa() {
        let tmp = tempfile::tempdir().unwrap();
        let bueno = tmp.path().join("bueno.xlsx");
        escribir_libro_prueba(&bueno);
        let bytes = std::fs::read(&bueno).unwrap();
        let truncado_ruta = tmp.path().join("truncado.xlsx");
        std::fs::write(&truncado_ruta, &bytes[..bytes.len() / 2]).unwrap();

        let mut avisos = Vec::new();
        let columnas = columnas_union(&[truncado_ruta], None, |m| avisos.push(m.to_string()));
        assert_eq!(columnas, Vec::<String>::new());
        assert!(
            !avisos.is_empty(),
            "debe avisar que el archivo no se pudo abrir, no quedarse en silencio"
        );
    }

    #[test]
    fn total_filas_ante_archivo_corrupto_no_revienta_y_avisa() {
        let tmp = tempfile::tempdir().unwrap();
        let bueno = tmp.path().join("bueno.xlsx");
        escribir_libro_prueba(&bueno);
        let bytes = std::fs::read(&bueno).unwrap();
        let truncado_ruta = tmp.path().join("truncado.xlsx");
        std::fs::write(&truncado_ruta, &bytes[..bytes.len() / 2]).unwrap();

        let mut avisos = Vec::new();
        let total = total_filas(&[truncado_ruta], None, |m| avisos.push(m.to_string()));
        assert_eq!(total, None);
        assert!(
            !avisos.is_empty(),
            "debe avisar que el archivo no se pudo abrir, no quedarse en silencio"
        );
    }

    #[test]
    fn columnas_union_de_bloques_dedup_en_orden_de_primera_aparicion() -> CoreResult<()> {
        let a = df!("A" => ["1"], "B" => ["2"])?;
        let b = df!("B" => ["3"], "C" => ["4"])?;
        assert_eq!(
            columnas_union_de_bloques(&[a, b]),
            vec!["A".to_string(), "B".to_string(), "C".to_string()]
        );
        Ok(())
    }

    #[test]
    fn abrir_libro_permite_leer_hojas_por_nombre_en_cualquier_orden() {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("multi.xlsx");
        {
            use rust_xlsxwriter::Workbook;
            let mut wb = Workbook::new();
            let hoja1 = wb.add_worksheet().set_name("Hoja1").unwrap();
            hoja1.write(0, 0, "A").unwrap();
            hoja1.write(1, 0, "1").unwrap();
            let hoja2 = wb.add_worksheet().set_name("Hoja2").unwrap();
            hoja2.write(0, 0, "B").unwrap();
            hoja2.write(1, 0, "2").unwrap();
            wb.save(&ruta).unwrap();
        }

        let mut libro = abrir_libro(&ruta).unwrap();
        assert_eq!(
            nombres_hojas_libro(&libro),
            vec!["Hoja1".to_string(), "Hoja2".to_string()]
        );

        let hoja2 = leer_hoja_por_nombre(&mut libro, &ruta, "Hoja2").unwrap();
        assert_eq!(hoja2.column("B").unwrap().str().unwrap().get(0), Some("2"));
        let hoja1 = leer_hoja_por_nombre(&mut libro, &ruta, "Hoja1").unwrap();
        assert_eq!(hoja1.column("A").unwrap().str().unwrap().get(0), Some("1"));
    }

    #[test]
    fn contar_filas_hoja_cuenta_filas_de_datos_sin_la_cabecera() {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("conteo.xlsx");
        {
            use rust_xlsxwriter::Workbook;
            let mut wb = Workbook::new();
            let hoja = wb.add_worksheet().set_name("Hoja1").unwrap();
            hoja.write(0, 0, "A").unwrap();
            for fila in 1..=3 {
                hoja.write(fila, 0, fila as f64).unwrap();
            }
            wb.save(&ruta).unwrap();
        }

        let mut libro = abrir_libro(&ruta).unwrap();
        assert_eq!(contar_filas_hoja(&mut libro, &ruta, "Hoja1").unwrap(), 3);
    }

    #[test]
    fn contar_filas_hoja_ante_hoja_inexistente_devuelve_error_claro() {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("sin_esa_hoja.xlsx");
        escribir_libro_prueba(&ruta);

        let mut libro = abrir_libro(&ruta).unwrap();
        let Err(error) = contar_filas_hoja(&mut libro, &ruta, "NoExiste") else {
            panic!("se esperaba un error")
        };
        assert!(error.to_string().contains("NoExiste"));
    }

    #[test]
    fn abrir_libro_ante_archivo_corrupto_devuelve_error_claro() {
        let tmp = tempfile::tempdir().unwrap();
        let bueno = tmp.path().join("bueno.xlsx");
        escribir_libro_prueba(&bueno);
        let bytes = std::fs::read(&bueno).unwrap();
        let truncado_ruta = tmp.path().join("truncado.xlsx");
        std::fs::write(&truncado_ruta, &bytes[..bytes.len() / 2]).unwrap();

        let Err(error) = abrir_libro(&truncado_ruta) else {
            panic!("se esperaba un error")
        };
        assert!(error.to_string().contains("no se pudo abrir"));
    }
}
