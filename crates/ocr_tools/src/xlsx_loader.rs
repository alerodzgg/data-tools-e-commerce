//! Lectura de XLSX multi-hoja → Polars: `XlsxLoader`.
//!
//! Se reutiliza el motor de lectura compartido de `commerce_core`
//! (calamine + nombres de cabecera canónicos + reparación de mojibake), el
//! mismo que usan el resto de las herramientas del proyecto — solo se agrega
//! lo específico de este módulo: filtrar filas 100% vacías y etiquetar
//! `_source_sheet` por fila.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use commerce_core::{
    abrir_libro, columnas_union, leer_hoja_por_nombre, limpiar_mojibake, nombres_hojas_libro, CoreResult,
    LibroXlsx,
};
use polars::prelude::*;

pub struct HojaLeida {
    pub nombre: String,
    pub df: DataFrame,
}

/// Itera las hojas de un XLSX, hoja por hoja: filas 100% vacías descartadas,
/// mojibake reparado en las columnas de texto, `_source_sheet` agregada.
///
/// Nunca aborta: si el libro no se puede abrir, o una hoja individual falla,
/// se avisa vía `avisar` y se sigue (con lo que quede, o con una lista vacía).
pub fn iter_sheets(path: &Path, mut avisar: impl FnMut(&str)) -> Vec<HojaLeida> {
    let mut libro = match abrir_libro(path) {
        Ok(l) => l,
        Err(error) => {
            avisar(&format!("Error al abrir '{}': {error}", path.display()));
            return Vec::new();
        }
    };

    let mut salida = Vec::new();
    for nombre in nombres_hojas_libro(&libro) {
        match cargar_hoja(&mut libro, path, &nombre) {
            Ok(Some(df)) => salida.push(HojaLeida { nombre, df }),
            Ok(None) => {}
            Err(error) => avisar(&format!("Error leyendo hoja '{nombre}': {error}")),
        }
    }
    salida
}

fn cargar_hoja(libro: &mut LibroXlsx, path: &Path, nombre: &str) -> CoreResult<Option<DataFrame>> {
    let df = leer_hoja_por_nombre(libro, path, nombre)?;
    let mut df = quitar_filas_totalmente_vacias(df)?;
    if df.height() == 0 {
        return Ok(None);
    }

    let alto = df.height();
    df.with_column(Column::new(
        "_source_sheet".into(),
        vec![nombre.to_string(); alto],
    ))?;

    let columnas_texto: Vec<String> = df
        .get_column_names()
        .iter()
        .map(|s| s.to_string())
        .filter(|c| !c.starts_with('_'))
        .collect();
    let refs: Vec<&str> = columnas_texto.iter().map(String::as_str).collect();
    Ok(Some(limpiar_mojibake(df, Some(&refs))?))
}

fn quitar_filas_totalmente_vacias(df: DataFrame) -> PolarsResult<DataFrame> {
    let alto = df.height();
    if alto == 0 {
        return Ok(df);
    }
    let mut todas_vacias = vec![true; alto];
    for columna in df.columns() {
        let serie = columna.as_materialized_series();
        for (i, es_null) in serie.is_null().iter().enumerate() {
            if !es_null.unwrap_or(false) {
                todas_vacias[i] = false;
            }
        }
    }
    let mascara = BooleanChunked::from_iter_values(PlSmallStr::EMPTY, todas_vacias.iter().map(|v| !v));
    df.filter(&mascara)
}

/// Unión (orden de primera aparición) de las columnas de todas las hojas del
/// archivo, con los mismos nombres que produce la lectura.
pub fn column_union(path: &Path, avisar: impl FnMut(&str)) -> Vec<String> {
    let archivos = [path.to_path_buf()];
    columnas_union(&archivos, None, avisar)
}

/// Lee hasta `n` filas (muestreando todas las hojas que haga falta) para
/// autodetectar columnas de URL. `None` si el libro no aporta ninguna fila.
pub fn load_sample(path: &Path, n: usize) -> Option<DataFrame> {
    let hojas = iter_sheets(path, |_| {});
    if hojas.is_empty() {
        return None;
    }

    let mut partes = Vec::new();
    let mut faltan = n;
    for hoja in hojas {
        if faltan == 0 {
            break;
        }
        let tomado = hoja.df.head(Some(faltan));
        faltan -= tomado.height();
        if tomado.height() > 0 {
            partes.push(tomado);
        }
    }
    if partes.is_empty() {
        return None;
    }
    concat_diagonal(partes).ok()
}

/// Concatena `dfs` alineando a la unión de columnas (rellena faltantes con
/// null), igual que `pl.concat(..., how="diagonal_relaxed")`.
fn concat_diagonal(dfs: Vec<DataFrame>) -> PolarsResult<DataFrame> {
    let mut columnas: Vec<String> = Vec::new();
    let mut vistas: HashSet<String> = HashSet::new();
    for df in &dfs {
        for c in df.get_column_names() {
            if vistas.insert(c.to_string()) {
                columnas.push(c.to_string());
            }
        }
    }
    let mut resultado: Option<DataFrame> = None;
    for mut df in dfs {
        for c in &columnas {
            if df.column(c).is_err() {
                let nulos: Vec<Option<String>> = vec![None; df.height()];
                df.with_column(Column::new(c.as_str().into(), nulos))?;
            }
        }
        let df = df.select(columnas.iter().map(String::as_str))?;
        resultado = Some(match resultado {
            None => df,
            Some(acc) => acc.vstack(&df)?,
        });
    }
    Ok(resultado.unwrap_or_else(DataFrame::empty))
}

/// Lista los `.xlsx` de un directorio, en orden alfabético.
pub fn list_xlsx_files(directory: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut archivos: Vec<PathBuf> = std::fs::read_dir(directory)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("xlsx"))
                    .unwrap_or(false)
        })
        .collect();
    archivos.sort();
    Ok(archivos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_xlsxwriter::Workbook;

    fn escribir(ruta: &Path, hojas: &[(&str, Vec<Vec<&str>>)]) {
        let mut wb = Workbook::new();
        for (nombre, filas) in hojas {
            let hoja = wb.add_worksheet().set_name(*nombre).unwrap();
            for (r, fila) in filas.iter().enumerate() {
                for (c, valor) in fila.iter().enumerate() {
                    if !valor.is_empty() {
                        hoja.write(r as u32, c as u16, *valor).unwrap();
                    }
                }
            }
        }
        wb.save(ruta).unwrap();
    }

    #[test]
    fn iter_sheets_descarta_filas_totalmente_vacias_y_marca_la_hoja_origen() {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("cab.xlsx");
        escribir(
            &ruta,
            &[(
                "Datos",
                vec![
                    vec!["Sku", "Precio"],
                    vec!["A1", "10"],
                    vec!["", ""], // fila 100% vacia
                    vec!["A2", "11"],
                ],
            )],
        );

        let hojas = iter_sheets(&ruta, |_| {});
        assert_eq!(hojas.len(), 1);
        assert_eq!(hojas[0].nombre, "Datos");
        assert_eq!(hojas[0].df.height(), 2, "la fila 100% vacia se descarta");
        assert_eq!(
            hojas[0].df.column("_source_sheet").unwrap().str().unwrap().get(0),
            Some("Datos")
        );
    }

    #[test]
    fn iter_sheets_ante_archivo_corrupto_no_revienta_y_avisa() {
        let tmp = tempfile::tempdir().unwrap();
        let bueno = tmp.path().join("bueno.xlsx");
        escribir(&bueno, &[("H", vec![vec!["A"], vec!["1"]])]);
        let bytes = std::fs::read(&bueno).unwrap();
        let truncado = tmp.path().join("truncado.xlsx");
        std::fs::write(&truncado, &bytes[..bytes.len() / 2]).unwrap();

        let mut avisos = Vec::new();
        let hojas = iter_sheets(&truncado, |m| avisos.push(m.to_string()));
        assert!(hojas.is_empty());
        assert!(!avisos.is_empty());
    }

    #[test]
    fn column_union_mira_todas_las_hojas_en_orden_de_aparicion() {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("multi.xlsx");
        escribir(
            &ruta,
            &[
                ("Meta", vec![vec!["Nota"], vec!["algo"]]),
                (
                    "Datos",
                    vec![vec!["Sku", "Img"], vec!["S0", "http://x.com/a.jpg"]],
                ),
            ],
        );
        assert_eq!(column_union(&ruta, |_| {}), vec!["Nota", "Sku", "Img"]);
    }

    #[test]
    fn load_sample_muestrea_todas_las_hojas_hasta_completar_n() {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("multi.xlsx");
        escribir(
            &ruta,
            &[
                ("Meta", vec![vec!["Nota"], vec!["sin urls aqui"]]),
                (
                    "Datos",
                    vec![
                        vec!["Sku", "Img"],
                        vec!["S0", "http://cdn.com/0.jpg"],
                        vec!["S1", "http://cdn.com/1.jpg"],
                    ],
                ),
            ],
        );
        let muestra = load_sample(&ruta, 50).expect("debe traer datos");
        assert_eq!(muestra.height(), 3); // 1 de Meta + 2 de Datos
        assert!(muestra.column("Img").is_ok());
        assert!(muestra.column("Nota").is_ok());
    }

    #[test]
    fn list_xlsx_files_ordena_alfabeticamente_y_solo_toma_xlsx() {
        let tmp = tempfile::tempdir().unwrap();
        escribir(&tmp.path().join("b.xlsx"), &[("H", vec![vec!["A"]])]);
        escribir(&tmp.path().join("a.xlsx"), &[("H", vec![vec!["A"]])]);
        std::fs::write(tmp.path().join("c.txt"), "no importa").unwrap();

        let archivos = list_xlsx_files(tmp.path()).unwrap();
        let nombres: Vec<String> = archivos
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(nombres, vec!["a.xlsx".to_string(), "b.xlsx".to_string()]);
    }

    #[test]
    fn list_xlsx_files_no_ignora_extension_en_mayusculas() {
        // Antes: la comparación era case-sensitive (`== Some("xlsx")`), así
        // que un archivo "Reporte.XLSX" desaparecía en silencio del listado.
        let tmp = tempfile::tempdir().unwrap();
        escribir(&tmp.path().join("Reporte.XLSX"), &[("H", vec![vec!["A"]])]);

        let archivos = list_xlsx_files(tmp.path()).unwrap();
        assert_eq!(archivos.len(), 1);
    }
}
