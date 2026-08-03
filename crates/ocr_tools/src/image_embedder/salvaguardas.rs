//! Qué archivos se rechazan ANTES de abrirlos.
//!
//! `xlsx::reader::xlsx::read` materializa el libro entero en memoria sin
//! ningún límite propio, así que el filtro tiene que estar acá: se mira el
//! directorio central del zip (barato, no descomprime nada) y se decide.

use std::path::Path;

use super::MotivoSinProcesar;

/// Techo de bytes DESCOMPRIMIDOS que se acepta sumar entre todas las
/// entradas del .xlsx (que es un zip). Mismo espíritu que
/// `MAX_IMAGE_DIM`/`MAX_IMAGE_ALLOC` en `downloader.rs` para imágenes: un
/// .xlsx pequeño en disco puede descomprimir a un tamaño desproporcionado
/// ("zip bomb"), y `xlsx::reader::xlsx::read` materializa el libro entero en
/// memoria de una sola vez, sin ningún límite propio.
const MAX_XLSX_UNCOMPRESSED: u64 = 512 * 1024 * 1024;
/// Techo de cantidad de entradas dentro del zip — protege contra el caso de
/// muchísimas entradas diminutas (que no pesarían por tamaño total pero sí
/// por la sola cantidad de metadata/archivos a procesar).
const MAX_XLSX_ENTRIES: usize = 50_000;

/// Revisa el directorio central del zip (barato: no descomprime contenido)
/// antes de pasarle el archivo a `xlsx::reader::xlsx::read`, que sí lo
/// descomprime entero en memoria sin ningún límite propio. Devuelve `Err` si
/// el archivo no es un zip válido o excede los límites de arriba.
pub(super) fn verificar_tamano_xlsx_seguro(ruta: &Path) -> Result<(), MotivoSinProcesar> {
    verificar_tamano_xlsx_con_limites(ruta, MAX_XLSX_UNCOMPRESSED, MAX_XLSX_ENTRIES)
}

/// Misma lógica que [`verificar_tamano_xlsx_seguro`], con los límites como
/// parámetro para poder testear el rechazo sin construir un zip de cientos
/// de MB reales.
fn verificar_tamano_xlsx_con_limites(
    ruta: &Path,
    max_uncompressed: u64,
    max_entries: usize,
) -> Result<(), MotivoSinProcesar> {
    let archivo = std::fs::File::open(ruta).map_err(|_| MotivoSinProcesar::ArchivoCorrupto)?;
    let mut zip = zip::ZipArchive::new(archivo).map_err(|_| MotivoSinProcesar::ArchivoCorrupto)?;
    if zip.len() > max_entries {
        return Err(MotivoSinProcesar::ArchivoDemasiadoGrande);
    }
    let mut total: u64 = 0;
    for i in 0..zip.len() {
        let entrada = zip
            .by_index_raw(i)
            .map_err(|_| MotivoSinProcesar::ArchivoCorrupto)?;
        total = total.saturating_add(entrada.size());
        if total > max_uncompressed {
            return Err(MotivoSinProcesar::ArchivoDemasiadoGrande);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xlsx_minimo(ruta: &Path) {
        use rust_xlsxwriter::Workbook;
        let mut wb = Workbook::new();
        let hoja = wb.add_worksheet();
        hoja.write(0, 0, "Sku").unwrap();
        hoja.write(1, 0, "A1").unwrap();
        wb.save(ruta).unwrap();
    }

    #[test]
    fn verificar_tamano_xlsx_seguro_acepta_un_archivo_normal() {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("normal.xlsx");
        xlsx_minimo(&ruta);
        assert!(verificar_tamano_xlsx_seguro(&ruta).is_ok());
    }
    #[test]
    fn verificar_tamano_xlsx_con_limites_rechaza_si_excede_bytes_descomprimidos() {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("normal.xlsx");
        xlsx_minimo(&ruta);
        // Límite de 1 byte descomprimido: cualquier .xlsx real lo excede en
        // la primera entrada — simula, sin fabricar un archivo real de
        // cientos de MB, el rechazo que dispararía una zip-bomb real.
        let error = verificar_tamano_xlsx_con_limites(&ruta, 1, usize::MAX)
            .expect_err("debe rechazar por bytes descomprimidos");
        assert!(matches!(error, MotivoSinProcesar::ArchivoDemasiadoGrande));
    }
    #[test]
    fn verificar_tamano_xlsx_con_limites_rechaza_si_excede_cantidad_de_entradas() {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("normal.xlsx");
        xlsx_minimo(&ruta);
        let error = verificar_tamano_xlsx_con_limites(&ruta, u64::MAX, 0)
            .expect_err("debe rechazar por cantidad de entradas");
        assert!(matches!(error, MotivoSinProcesar::ArchivoDemasiadoGrande));
    }
}
