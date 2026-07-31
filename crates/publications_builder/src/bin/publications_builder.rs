//! Binario interactivo de `publications_builder`: Únicas, Compatibilidades,
//! Comprimidas y Amazon.

use std::path::{Path, PathBuf};

use app_shell::FlujoError;
use commerce_core::{total_filas, CoreError};

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Flujo(#[from] FlujoError),
    #[error(transparent)]
    Core(#[from] CoreError),
}

type AppResult<T> = Result<T, AppError>;

fn seleccionar_archivo() -> AppResult<Option<PathBuf>> {
    let entrada = app_shell::ruta_entrada();
    let archivos = app_shell::listar_xlsx(&entrada).unwrap_or_default();
    if archivos.is_empty() {
        app_shell::error(&format!(
            "No se encontraron archivos .xlsx en '{}'.",
            entrada.display()
        ));
        return Ok(None);
    }
    Ok(app_shell::elegir_archivo(
        "Elige el archivo a procesar:",
        archivos,
    )?)
}

/// Ejecuta un modo, mostrando la barra de progreso y el resumen final.
/// `correr` recibe `(avisar, progreso)` y hace el trabajo real — mismo
/// patrón que los demás binarios: la interfaz solo envuelve al motor.
///
/// `correr` devuelve `true` si de verdad escribió algo. Con `false` (hoy solo
/// pasa si Compatibilidades no encuentra 'Hoja1') se omite el mensaje de
/// éxito: ya se avisó por qué no se escribió nada, y un "✅ completado"
/// justo después de ese aviso sería contradictorio.
fn ejecutar_modo(
    archivo: &Path,
    correr: impl FnOnce(&mut dyn FnMut(&str), &mut dyn FnMut(u64)) -> Result<bool, CoreError>,
) -> AppResult<()> {
    let total = total_filas(
        std::slice::from_ref(&archivo.to_path_buf()),
        None,
        app_shell::warn,
    )
    .unwrap_or(0);
    let barra = app_shell::barra_progreso(
        &format!(
            "Procesando {}",
            archivo.file_name().unwrap_or_default().to_string_lossy()
        ),
        total,
    );
    let inicio = std::time::Instant::now();
    let mut avisar = |m: &str| app_shell::warn(m);
    let mut progreso = |n: u64| barra.inc(n);
    let resultado = correr(&mut avisar, &mut progreso);
    barra.finish_and_clear();

    if resultado? {
        app_shell::success(&format!(
            "Listo: procesado en {:.2} segundos.",
            inicio.elapsed().as_secs_f64()
        ));
    }
    Ok(())
}

fn ejecutar() -> AppResult<()> {
    app_shell::mostrar_cabecera("PUBLICATIONS BUILDER — eBay / Amazon");
    let ruta_salida = app_shell::ruta_salida();

    loop {
        let opciones = vec![
            "Únicas".to_string(),
            "Compatibilidades".to_string(),
            "Comprimidas".to_string(),
            "Amazon".to_string(),
        ];
        let modo = match app_shell::menu_seleccionar_nav(
            "¿Qué tipo de procesamiento deseas ejecutar?",
            opciones.clone(),
        ) {
            Ok(Some(m)) => m,
            Ok(None) => {
                app_shell::info("Hasta luego.");
                break;
            }
            Err(FlujoError::VolverAlMenu) => continue,
            Err(e) => return Err(e.into()),
        };

        let resultado = (|| -> AppResult<()> {
            let Some(archivo) = seleccionar_archivo()? else {
                app_shell::warn("No se seleccionó archivo. Volviendo al menú.");
                return Ok(());
            };

            let modificar_oem = app_shell::menu_confirmar(
                "¿Modificar la columna OEM (rellenar vacíos con US123400 y cortar)?",
                false,
            )?
            .unwrap_or(false);

            let stem = archivo.file_stem().unwrap_or_default().to_string_lossy();
            let archivo_salida = ruta_salida.join(format!("{stem}_procesado.xlsx"));

            if modo == opciones[0] {
                ejecutar_modo(&archivo, |avisar, progreso| {
                    publications_builder::ejecutar_procesamiento_unicas(
                        &archivo,
                        &archivo_salida,
                        modificar_oem,
                        avisar,
                        progreso,
                    )
                    .map(|()| true)
                })
            } else if modo == opciones[1] || modo == opciones[2] {
                let modo_interno = if modo == opciones[1] {
                    publications_builder::ModoCompatibilidad::Repetidas
                } else {
                    publications_builder::ModoCompatibilidad::Comprimidas
                };
                let borrar_linea = modo_interno == publications_builder::ModoCompatibilidad::Repetidas
                    && app_shell::menu_confirmar("¿Borrar la columna Y los valores de 'Linea'?", false)?
                        .unwrap_or(false);
                ejecutar_modo(&archivo, |avisar, progreso| {
                    publications_builder::ejecutar_procesamiento_compatibilidad(
                        &archivo,
                        &archivo_salida,
                        modo_interno,
                        modificar_oem,
                        borrar_linea,
                        avisar,
                        progreso,
                    )
                })
            } else {
                let borrar_imagenes_extra = app_shell::menu_confirmar(
                    "¿Borrar el contenido de las columnas Imagen 2, 3 y 4?",
                    false,
                )?
                .unwrap_or(false);
                ejecutar_modo(&archivo, |avisar, progreso| {
                    publications_builder::ejecutar_procesamiento_amazon(
                        &archivo,
                        &archivo_salida,
                        modificar_oem,
                        borrar_imagenes_extra,
                        avisar,
                        progreso,
                    )
                    .map(|()| true)
                })
            }
        })();

        if let Err(e) = resultado {
            match e {
                AppError::Flujo(FlujoError::VolverAlMenu) => continue,
                e => app_shell::error(&format!("Error: {e}")),
            }
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
