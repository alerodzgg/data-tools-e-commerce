//! Binario interactivo de `publications_validator`: elegir archivo, confirmar
//! OEM, procesar.

use app_shell::FlujoError;
use publications_validator::Procesador;

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Flujo(#[from] FlujoError),
    #[error(transparent)]
    Core(#[from] commerce_core::CoreError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn ejecutar() -> Result<(), AppError> {
    app_shell::mostrar_cabecera("PUBLICATIONS VALIDATOR");

    // Solo .xlsx: el .xls antiguo no lo abre este lector (elegirlo abortaba
    // con error); si aparece uno, convertirlo a .xlsx antes de procesar.
    let entrada = app_shell::ruta_entrada();
    let archivos = app_shell::listar_xlsx(&entrada)?;
    if archivos.is_empty() {
        app_shell::error(&format!(
            "No se encontraron archivos .xlsx en '{}'.",
            entrada.display()
        ));
        return Ok(());
    }

    let Some(archivo_entrada) = app_shell::elegir_archivo("Selecciona el archivo a procesar:", archivos)?
    else {
        app_shell::info("Hasta luego.");
        return Ok(());
    };

    let modificar_oem = app_shell::menu_confirmar(
        "¿Modificar la columna OEM (rellenar vacíos con US123400 y cortar)?",
        false,
    )?
    .unwrap_or(false);

    let stem = archivo_entrada.file_stem().unwrap_or_default().to_string_lossy();
    let archivo_salida = app_shell::ruta_salida().join(format!("{stem}_procesado.xlsx"));

    let procesador = Procesador::nuevo(&archivo_entrada, &archivo_salida, 50_000, modificar_oem);
    let total = commerce_core::total_filas(std::slice::from_ref(&archivo_entrada), None, app_shell::warn)
        .unwrap_or(0);
    let barra = app_shell::barra_progreso("Procesando", total);
    match procesador.procesar_masivo(app_shell::warn, |avance| barra.inc(avance)) {
        Ok(stats) => {
            barra.finish_and_clear();
            app_shell::success(&format!(
                "Procesamiento completado — {} procesadas, {} eliminadas ({} lotes).",
                stats.procesadas, stats.eliminadas, stats.chunks
            ));
        }
        Err(e) => {
            barra.finish_and_clear();
            app_shell::error(&format!("Error: {e}"));
        }
    }
    Ok(())
}

fn main() {
    if let Err(e) = ejecutar() {
        match e {
            AppError::Flujo(FlujoError::VolverAlMenu) => {}
            e => app_shell::error(&format!("Error fatal: {e}")),
        }
    }
}
