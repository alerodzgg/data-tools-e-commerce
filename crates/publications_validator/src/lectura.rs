use std::path::Path;

use commerce_core::{CoreError, CoreResult};
use polars::prelude::*;

use crate::constantes::HOJA_RESERVADA;

/// Suma de filas de datos de todas las hojas (para estimar si el dedup
/// cabe en memoria).
pub fn estimar_total_filas(archivo_entrada: &Path) -> u64 {
    let Ok(mut libro) = commerce_core::abrir_libro(archivo_entrada) else {
        return 0;
    };
    let sheet_names = commerce_core::nombres_hojas_libro(&libro);
    sheet_names
        .iter()
        .filter_map(|h| commerce_core::contar_filas_hoja(&mut libro, archivo_entrada, h).ok())
        .sum()
}

/// Itera hoja por hoja, en bloques de `chunk_size` filas si la hoja es más
/// grande. Aborta (sin llamar a `on_chunk` ni una vez) si el archivo no
/// existe, está corrupto, o trae una hoja llamada [`HOJA_RESERVADA`]
/// (nombre reservado para el reporte). Un error de lectura de UNA hoja se
/// avisa y se salta (no aborta el resto del archivo).
pub fn iter_hojas_streaming(
    archivo_entrada: &Path,
    chunk_size: usize,
    mut avisar: impl FnMut(&str),
    mut on_chunk: impl FnMut(&str, DataFrame) -> CoreResult<()>,
) -> CoreResult<()> {
    if !archivo_entrada.exists() {
        return Err(CoreError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("No se encuentra el archivo {}", archivo_entrada.display()),
        )));
    }

    let mut libro = commerce_core::abrir_libro(archivo_entrada)?;
    let sheet_names = commerce_core::nombres_hojas_libro(&libro);

    for hoja in &sheet_names {
        if hoja.trim().eq_ignore_ascii_case(HOJA_RESERVADA) {
            return Err(CoreError::Io(std::io::Error::other(format!(
                "El archivo trae una hoja llamada '{hoja}' (nombre reservado para el reporte de eliminadas). \
                 Renómbrala y reintenta."
            ))));
        }
    }

    for hoja in &sheet_names {
        let df = match commerce_core::leer_hoja_por_nombre(&mut libro, archivo_entrada, hoja) {
            Ok(df) => df,
            Err(error) => {
                avisar(&format!("Error cargando hoja {hoja}: {error}"));
                continue;
            }
        };
        let n = df.height();
        if n > chunk_size {
            let mut inicio = 0;
            while inicio < n {
                let tomar = chunk_size.min(n - inicio);
                on_chunk(hoja, df.slice(inicio as i64, tomar))?;
                inicio += tomar;
            }
        } else {
            on_chunk(hoja, df)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escribir_libro(ruta: &Path, hojas: &[(&str, usize)]) {
        use rust_xlsxwriter::Workbook;
        let mut wb = Workbook::new();
        for (nombre, filas) in hojas {
            let hoja = wb.add_worksheet().set_name(*nombre).unwrap();
            hoja.write(0, 0, "Col").unwrap();
            for f in 0..*filas {
                hoja.write((f + 1) as u32, 0, format!("v{f}")).unwrap();
            }
        }
        wb.save(ruta).unwrap();
    }

    #[test]
    fn estimar_total_filas_archivo_inexistente_devuelve_cero() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(estimar_total_filas(&tmp.path().join("no_existe.xlsx")), 0);
    }

    #[test]
    fn estimar_total_filas_suma_todas_las_hojas() {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("multi.xlsx");
        escribir_libro(&archivo, &[("Hoja1", 3), ("Hoja2", 5)]);
        assert_eq!(estimar_total_filas(&archivo), 8);
    }
}
