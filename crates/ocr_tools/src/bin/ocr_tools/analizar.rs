//! El modo de ANÁLISIS: filtrar las imágenes de un archivo por su contenido.
//!
//! Una hoja a la vez (lee → analiza → escribe → libera) para que el pico de
//! memoria no dependa del tamaño del archivo.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ocr_tools::async_processor::{AsyncBatchProcessor, Bloque, Motor, Persistencia};
use ocr_tools::checkpoint_store::CheckpointStore;
use ocr_tools::downloader::DownloadConfig;
use ocr_tools::file_workflow::{self, columnas_reservadas_en_choque};
use ocr_tools::pipeline::{DetectorToggles, ImagePipeline, PipelineConfig};
use ocr_tools::reader::Reader;
use ocr_tools::report::build_report;
use ocr_tools::xlsx_loader;

use crate::dialogos::{resolver_columnas_url, ModoColumnas};
use crate::{modelo, AppError, AppResult};
use app_shell::FlujoError;

/// Procesa UN archivo de punta a punta: unión de columnas, checkpoint,
/// resolución de columnas URL, y una hoja a la vez (lee → analiza → escribe
/// → libera), igual que `FileWorkflow.run`.
/// Descargas simultáneas contra el servidor de imágenes.
///
/// Vale 32 porque es lo que usaba la versión en Python de esta herramienta,
/// que funcionaba contra las mismas URLs. Se probó bajarlo a 6 sospechando
/// del throttling de los CDN: no cambió nada, así que la hipótesis quedó
/// descartada y el valor volvió al original.
const DESCARGAS_SIMULTANEAS: usize = 32;

/// Cuántos motores OCR levantar en paralelo.
///
/// Cada motor carga su propia copia de los modelos ONNX (~94 MB), así que
/// más motores compran velocidad con memoria. El tope de 8 evita que una
/// máquina de muchos núcleos se quede sin RAM antes de empezar; una corrida
/// grande en un servidor puede subirlo con `OCR_TOOLS_MOTORES`.
fn motores_ocr() -> usize {
    if let Some(n) = std::env::var("OCR_TOOLS_MOTORES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        return n.max(1);
    }
    std::thread::available_parallelism().map_or(1, |n| n.get().min(8))
}

async fn ejecutar_archivo(
    xlsx_path: &Path,
    pipeline: Arc<ImagePipeline>,
    mut readers: Option<&mut [Reader]>,
    rechazadas_solo: bool,
    modo_columnas: ModoColumnas<'_>,
) -> AppResult<()> {
    app_shell::info(&format!(
        "\nProcesando: {}",
        xlsx_path.file_name().unwrap_or_default().to_string_lossy()
    ));

    let columnas = xlsx_loader::column_union(xlsx_path, app_shell::warn);
    if columnas.is_empty() {
        app_shell::error("Archivo vacío o ilegible. Saltando.");
        return Ok(());
    }
    let chocan = columnas_reservadas_en_choque(&columnas);
    if !chocan.is_empty() {
        app_shell::error(&format!(
            "El archivo tiene columnas con nombres reservados por la herramienta: {}. Renómbralas y reintenta.",
            chocan.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(", ")
        ));
        return Ok(());
    }

    let url_columns = resolver_columnas_url(xlsx_path, &columnas, modo_columnas)?;
    if url_columns.is_empty() {
        app_shell::error("Sin columnas URL. Saltando archivo.");
        return Ok(());
    }

    let out_dir = app_shell::ruta_salida();
    let stem = xlsx_path.file_stem().unwrap_or_default().to_string_lossy();
    let checkpoint_path = out_dir.join(format!("_checkpoint_{stem}.jsonl"));
    let checkpoint = Arc::new(CheckpointStore::new(checkpoint_path));
    let cached = checkpoint.load();

    let ruta_aprob = commerce_core::ruta_unica(out_dir.join(format!("procesado_{stem}.xlsx")));
    let ruta_rech = commerce_core::ruta_unica(out_dir.join(format!("rechazadas_{stem}.xlsx")));

    let mut columnas_rech = columnas.clone();
    columnas_rech.push("Motivo_Rechazo".to_string());
    let mut escritor_aprob = file_workflow::escritor(&ruta_aprob, columnas.clone(), app_shell::warn)?;
    let mut escritor_rech = file_workflow::escritor(&ruta_rech, columnas_rech, app_shell::warn)?;

    let procesador = AsyncBatchProcessor::new(
        DownloadConfig::default(),
        1,
        std::time::Duration::from_millis(250),
        DESCARGAS_SIMULTANEAS,
        // Cada 50 y no cada 500: el buffer se vuelca a disco cada ~10s a
        // 0,19 s/imagen, y una interrupción de spot cuesta 50 imágenes en vez
        // de 500. La escritura corre en `spawn_blocking` y son unos pocos KB,
        // así que no frena el análisis.
        //
        // Con 500 había un caso peor: un archivo de menos de 500 imágenes
        // NUNCA llegaba al umbral, así que el checkpoint no se escribía hasta
        // el final y un corte a mitad perdía todo el trabajo.
        50,
    );

    let mut filas_total = 0u64;
    let mut imgs_analizadas = 0u64;
    let mut imgs_rechazadas = 0u64;
    let mut offset: i64 = 0;

    let resultado: AppResult<()> = async {
        for hoja in xlsx_loader::iter_sheets(xlsx_path, app_shell::warn) {
            let bloque = file_workflow::alinear(hoja.df, &columnas)?;
            app_shell::info(&format!("Hoja '{}': {} filas.", hoja.nombre, bloque.height()));

            let (pendientes, desde_cache) =
                ocr_tools::async_processor::contar_trabajo(&bloque, &url_columns, &cached, offset)?;
            let barra = app_shell::barra_progreso(
                &format!("Analizando '{}'", hoja.nombre),
                (pendientes + desde_cache) as u64,
            );
            let outcome = procesador
                .process(
                    Bloque {
                        df: &bloque,
                        url_columns: &url_columns,
                        idx_offset: offset,
                    },
                    Motor {
                        pipeline: pipeline.clone(),
                        readers: readers.as_deref_mut(),
                    },
                    Persistencia {
                        checkpoint: checkpoint.clone(),
                        cached: &cached,
                    },
                    |n| barra.inc(n),
                    app_shell::warn,
                )
                .await?;
            barra.finish_and_clear();

            file_workflow::escribir_bloque(
                &outcome.df,
                &bloque,
                &url_columns,
                rechazadas_solo,
                &mut escritor_aprob,
                &mut escritor_rech,
            )?;

            filas_total += bloque.height() as u64;
            imgs_analizadas += outcome.imagenes_analizadas as u64;
            imgs_rechazadas += outcome.imagenes_rechazadas as u64;
            offset += bloque.height() as i64;
        }
        Ok(())
    }
    .await;

    if let Err(e) = resultado {
        // Corrida a medias: fuera las salidas parciales. El checkpoint NO se
        // toca — el resume retoma exactamente donde iba.
        escritor_aprob.abortar().ok();
        escritor_rech.abortar().ok();
        let _ = std::fs::remove_file(&ruta_aprob);
        let _ = std::fs::remove_file(&ruta_rech);
        return Err(e);
    }

    escritor_aprob.cerrar()?;
    escritor_rech.cerrar()?;

    app_shell::success(&format!(
        "Guardado: {} ({} filas)",
        ruta_aprob.file_name().unwrap_or_default().to_string_lossy(),
        escritor_aprob.total
    ));
    if escritor_rech.total > 0 {
        app_shell::success(&format!(
            "Rechazadas: {} ({} filas)",
            ruta_rech.file_name().unwrap_or_default().to_string_lossy(),
            escritor_rech.total
        ));
    } else {
        let _ = std::fs::remove_file(&ruta_rech);
        app_shell::info("No hubo imágenes rechazadas.");
    }

    app_shell::info(&build_report(
        imgs_analizadas,
        imgs_rechazadas,
        filas_total,
        &xlsx_path.file_name().unwrap_or_default().to_string_lossy(),
    ));
    checkpoint.delete()?;
    app_shell::info("Checkpoint eliminado.");
    Ok(())
}

/// Prepara el motor UNA vez y corre con él todos los archivos elegidos.
///
/// Cargar los modelos ONNX es de lejos lo más caro del modo y no depende del
/// archivo: hacerlo por archivo multiplicaría ese costo por el tamaño del
/// lote sin ganar nada.
pub(crate) async fn analizar_archivos(
    archivos: &[PathBuf],
    toggles: DetectorToggles,
    rechazadas_solo: bool,
    columnas_fijas: Option<&[String]>,
    desatendido: bool,
) -> AppResult<()> {
    let pipeline = Arc::new(ImagePipeline::from_config(PipelineConfig::default(), toggles));
    let mut readers = if pipeline.has_slow_stage() {
        let n = motores_ocr();
        let gpu = ocr_tools::session::hay_aceleracion_gpu();
        app_shell::info(&format!(
            "Motor OCR: {} ({n} en paralelo). Ajustable con OCR_TOOLS_MOTORES.",
            if gpu { "GPU (CUDA)" } else { "CPU" }
        ));
        let mut pool = Vec::with_capacity(n);
        for _ in 0..n {
            match Reader::new(modelo("craft_detector.onnx"), modelo("recognizer_latin_g2.onnx")) {
                Ok(r) => pool.push(r),
                Err(e) => {
                    // Con al menos uno se puede seguir, más lento: abortar
                    // una corrida de horas por no poder abrir el motor Nº 6
                    // sería peor que correr con 5.
                    if pool.is_empty() {
                        app_shell::error(&format!("No se pudo cargar el motor OCR: {e}"));
                        return Ok(());
                    }
                    app_shell::warn(&format!(
                        "Solo se pudieron cargar {} motores OCR de {n}: {e}",
                        pool.len()
                    ));
                    break;
                }
            }
        }
        Some(pool)
    } else {
        None
    };

    for xlsx_path in archivos {
        let resultado = ejecutar_archivo(
            xlsx_path,
            pipeline.clone(),
            readers.as_deref_mut(),
            rechazadas_solo,
            match columnas_fijas {
                Some(c) => ModoColumnas::Fijas(c),
                // Sin argumentos hay un humano al teclado; con ellos, no.
                None if desatendido => ModoColumnas::DetectarSinPreguntar,
                None => ModoColumnas::Preguntar,
            },
        )
        .await;
        if let Err(e) = resultado {
            match e {
                // Cancelar con ESC aborta el LOTE, no solo el archivo en curso.
                AppError::Flujo(FlujoError::VolverAlMenu) => break,
                e => app_shell::error(&format!("'{}' falló: {e}", xlsx_path.display())),
            }
        }
    }
    Ok(())
}
