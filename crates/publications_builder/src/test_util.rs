//! Helpers de test compartidos por `amazon`, `compatibilidad` y `unicas`,
//! para no repetir el mismo andamiaje en los tres.

use std::collections::HashMap;
use std::path::Path;

use polars::prelude::*;

pub(crate) fn escribir_libro(ruta: &Path, hojas: &[(&str, Vec<Vec<&str>>)]) {
    use rust_xlsxwriter::Workbook;
    let mut wb = Workbook::new();
    for (nombre, filas) in hojas {
        let hoja = wb.add_worksheet().set_name(*nombre).unwrap();
        for (f, fila) in filas.iter().enumerate() {
            for (c, valor) in fila.iter().enumerate() {
                hoja.write(f as u32, c as u16, *valor).unwrap();
            }
        }
    }
    wb.save(ruta).unwrap();
}

pub(crate) fn leer_hojas(ruta: &Path) -> HashMap<String, DataFrame> {
    let mut libro = commerce_core::abrir_libro(ruta).unwrap();
    let nombres = commerce_core::nombres_hojas_libro(&libro);
    nombres
        .into_iter()
        .map(|n| {
            let df = commerce_core::leer_hoja_por_nombre(&mut libro, ruta, &n).unwrap();
            (n, df)
        })
        .collect()
}
