//! El modo INSERTAR: incrustar en el propio Excel las imágenes de una
//! columna de URLs, en una columna nueva a la derecha.

use std::path::PathBuf;

use ocr_tools::downloader::DownloadConfig;
use ocr_tools::image_embedder::{ImageEmbedConfig, ImageEmbedder, MotivoSinProcesar};

use crate::AppResult;

pub(crate) async fn insertar_imagenes_en_archivos(archivos: &[PathBuf]) -> AppResult<()> {
    let hojas_excluir = app_shell::preguntar_hojas_excluir(archivos, "")?.unwrap_or_default();
    let embedder = ImageEmbedder::new(ImageEmbedConfig::default(), DownloadConfig::default(), 32);
    let out_dir = app_shell::ruta_salida();

    for archivo in archivos {
        let stem = archivo.file_stem().unwrap_or_default().to_string_lossy();
        let destino = out_dir.join(format!("con_imagenes_{stem}.xlsx"));
        let nombre = archivo.file_name().unwrap_or_default().to_string_lossy();
        match embedder
            .procesar_archivo(archivo, &hojas_excluir, &destino, app_shell::warn)
            .await
        {
            Ok((ruta, exitos, fallos, hojas)) => {
                let extra = if fallos > 0 {
                    format!(
                        ", {fallos} con error ('{}')",
                        ImageEmbedConfig::default().texto_error
                    )
                } else {
                    String::new()
                };
                app_shell::success(&format!(
                    "Guardado: '{}' — {exitos} imagen(es) insertadas{extra} en {hojas} hoja(s).",
                    ruta.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
            Err(MotivoSinProcesar::SinColumnasDeUrl) => app_shell::warn(&format!(
                "'{nombre}': no se detectaron columnas con URLs de imagen. Nada que hacer."
            )),
            Err(MotivoSinProcesar::ArchivoCorrupto) => app_shell::error(&format!(
                "'{nombre}': no se pudo abrir (¿está corrupto o no es un .xlsx válido?)."
            )),
            Err(MotivoSinProcesar::ArchivoDemasiadoGrande) => app_shell::error(&format!(
                "'{nombre}': excede el límite de tamaño/descompresión permitido para insertar imágenes. \
                 No se procesa."
            )),
            Err(MotivoSinProcesar::FalloEscritura) => app_shell::error(&format!(
                "'{nombre}': se procesó pero no se pudo guardar el resultado en '{}'.",
                destino.file_name().unwrap_or_default().to_string_lossy()
            )),
        }
    }
    Ok(())
}
