//! Binario interactivo de `ocr_tools`: `CLI`/`FileWorkflow::run`/`main()`
//! para la selección de archivos/columnas/detectores por menú, análisis
//! (D1-D6) o inserción de imágenes en el Excel.
//!
//! Vive en `src/bin/` (no en la librería) a propósito: `ocr_tools` (el
//! motor) no depende de `app_shell` (la interfaz) — misma regla que separa
//! `commerce_core` de este binario en el resto del workspace.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use app_shell::{FlujoError, FlujoResult};
use ocr_tools::async_processor::AsyncBatchProcessor;
use ocr_tools::checkpoint_store::CheckpointStore;
use ocr_tools::downloader::DownloadConfig;
use ocr_tools::file_workflow::{self, columnas_reservadas_en_choque};
use ocr_tools::image_embedder::{ImageEmbedConfig, ImageEmbedder, MotivoSinProcesar};
use ocr_tools::pipeline::{DetectorToggles, ImagePipeline, PipelineConfig};
use ocr_tools::reader::Reader;
use ocr_tools::report::build_report;
use ocr_tools::url_helper;
use ocr_tools::xlsx_loader;

/// Errores del binario: la unión de lo que puede fallar en cada capa
/// (interfaz, motor, I/O). No vive en `app_shell` porque `ort`/`polars` son
/// específicos de este tool, no algo que compartan las demás herramientas.
#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Flujo(#[from] FlujoError),
    #[error(transparent)]
    Core(#[from] commerce_core::CoreError),
    #[error(transparent)]
    Polars(#[from] polars::error::PolarsError),
    #[error(transparent)]
    Ort(#[from] ort::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

type AppResult<T> = Result<T, AppError>;

fn modelo(nombre: &str) -> PathBuf {
    ocr_tools::assets_dir().join("models").join(nombre)
}

const OPCION_ANALIZAR: &str = "Analizar imágenes (todos los detectores activos)";
const OPCION_CONFIGURAR: &str = "Configurar detectores y analizar";
const OPCION_INSERTAR: &str = "Insertar imágenes de URL en el Excel (columna nueva a la derecha)";

fn configurar_detectores() -> FlujoResult<DetectorToggles> {
    let etiquetas = [
        "D1 · Banner de color",
        "D2 · Fondo no neutro",
        "D3 · Diagrama/infografía",
        "D4·Texto / D5·Logo / D6·Placeholder (OCR)",
    ];
    let activos = app_shell::menu_multiple_preseleccionado(
        "Detectores a activar (todos marcados por defecto):",
        etiquetas.to_vec(),
        &[0, 1, 2, 3],
    )?;
    let toggles = DetectorToggles {
        d1: activos.contains(&etiquetas[0]),
        d2: activos.contains(&etiquetas[1]),
        d3: activos.contains(&etiquetas[2]),
        d4_d5_d6: activos.contains(&etiquetas[3]),
    };
    app_shell::info(&format!(
        "Detectores activos: {}",
        [
            toggles.d1.then_some("d1"),
            toggles.d2.then_some("d2"),
            toggles.d3.then_some("d3"),
            toggles.d4_d5_d6.then_some("d4_d5_d6"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ")
    ));
    Ok(toggles)
}

fn elegir_modo_rechazadas() -> FlujoResult<bool> {
    let opciones = vec![
        "Todas las imágenes de la fila (como hasta ahora)".to_string(),
        "Solo las imágenes rechazadas (compactadas a la izquierda)".to_string(),
    ];
    let eleccion =
        app_shell::menu_seleccionar("En el archivo de RECHAZADAS, ¿qué imágenes incluir?", opciones)?;
    Ok(eleccion.as_deref() == Some("Solo las imágenes rechazadas (compactadas a la izquierda)"))
}

/// Detecta columnas URL automáticamente; si hay candidatas, confirma con el
/// usuario. Sin candidatas, o si no las quiere, deja elegir manualmente.
fn resolver_columnas_url(xlsx_path: &Path, columnas: &[String]) -> AppResult<Vec<String>> {
    let sample = xlsx_loader::load_sample(xlsx_path, 50);
    let candidatas = match &sample {
        Some(df) => url_helper::auto_detect_columns(df, 10)?,
        None => Vec::new(),
    };
    let publicas: Vec<String> = columnas.iter().filter(|c| !c.starts_with('_')).cloned().collect();

    if !candidatas.is_empty() {
        app_shell::success(&format!("Columnas URL detectadas: {}", candidatas.join(", ")));
        let usar = app_shell::menu_confirmar(
            &format!(
                "¿Usar las columnas detectadas automáticamente? ({})",
                candidatas.join(", ")
            ),
            true,
        )?
        .unwrap_or(true);
        if usar {
            return Ok(candidatas);
        }
        return Ok(app_shell::menu_multiple(
            "Columnas con URLs de imágenes (Enter sin marcar = cancelar):",
            publicas,
        )?);
    }

    app_shell::warn("No se detectaron columnas URL automáticamente.");
    Ok(app_shell::menu_multiple(
        "Columnas con URLs de imágenes (Enter sin marcar = cancelar):",
        publicas,
    )?)
}

/// Procesa UN archivo de punta a punta: unión de columnas, checkpoint,
/// resolución de columnas URL, y una hoja a la vez (lee → analiza → escribe
/// → libera), igual que `FileWorkflow.run`.
async fn ejecutar_archivo(
    xlsx_path: &Path,
    pipeline: Arc<ImagePipeline>,
    mut reader: Option<&mut Reader>,
    rechazadas_solo: bool,
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

    let url_columns = resolver_columnas_url(xlsx_path, &columnas)?;
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
        32,
        500,
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
                    &bloque,
                    &url_columns,
                    pipeline.clone(),
                    reader.as_deref_mut(),
                    checkpoint.clone(),
                    &cached,
                    offset,
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

    println!(
        "{}",
        build_report(
            imgs_analizadas,
            imgs_rechazadas,
            filas_total,
            &xlsx_path.file_name().unwrap_or_default().to_string_lossy()
        )
    );
    checkpoint.delete()?;
    app_shell::info("Checkpoint eliminado.");
    Ok(())
}

async fn insertar_imagenes_en_archivos(archivos: &[PathBuf]) -> AppResult<()> {
    let hojas_excluir = app_shell::preguntar_hojas_excluir(archivos, "")?.unwrap_or_default();
    let embedder = ImageEmbedder::new(ImageEmbedConfig::default(), DownloadConfig::default(), 32);
    let out_dir = app_shell::ruta_salida();

    for archivo in archivos {
        let stem = archivo.file_stem().unwrap_or_default().to_string_lossy();
        let destino = commerce_core::ruta_unica(out_dir.join(format!("con_imagenes_{stem}.xlsx")));
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
            Err(MotivoSinProcesar::FalloEscritura) => app_shell::error(&format!(
                "'{nombre}': se procesó pero no se pudo guardar el resultado en '{}'.",
                destino.file_name().unwrap_or_default().to_string_lossy()
            )),
        }
    }
    Ok(())
}

async fn ciclo_principal() -> AppResult<()> {
    loop {
        let opciones = vec![
            OPCION_ANALIZAR.to_string(),
            OPCION_CONFIGURAR.to_string(),
            OPCION_INSERTAR.to_string(),
        ];
        let eleccion = match app_shell::menu_seleccionar_nav("¿Qué quieres hacer?", opciones) {
            Ok(Some(e)) => e,
            Ok(None) => {
                app_shell::info("Hasta luego.");
                return Ok(());
            }
            Err(FlujoError::VolverAlMenu) => continue,
            Err(e) => return Err(e.into()),
        };

        let files = xlsx_loader::list_xlsx_files(&app_shell::ruta_entrada())?;
        if files.is_empty() {
            app_shell::warn("No se encontraron archivos. Volviendo al menú.");
            continue;
        }

        let seleccionados =
            match app_shell::elegir_archivos("Archivos a analizar (Enter sin marcar = cancelar):", files) {
                Ok(v) => v,
                Err(FlujoError::VolverAlMenu) => continue,
                Err(e) => return Err(e.into()),
            };
        if seleccionados.is_empty() {
            app_shell::warn("No se seleccionaron archivos. Volviendo al menú.");
            continue;
        }

        if eleccion == OPCION_INSERTAR {
            if let Err(e) = insertar_imagenes_en_archivos(&seleccionados).await {
                match e {
                    AppError::Flujo(FlujoError::VolverAlMenu) => continue,
                    e => {
                        app_shell::error(&format!("La herramienta falló: {e}"));
                        app_shell::warn("Vuelvo al menú.");
                    }
                }
            }
            continue;
        }

        let toggles = if eleccion == OPCION_CONFIGURAR {
            match configurar_detectores() {
                Ok(t) => t,
                Err(FlujoError::VolverAlMenu) => continue,
                Err(e) => return Err(e.into()),
            }
        } else {
            DetectorToggles::default()
        };
        let rechazadas_solo = match elegir_modo_rechazadas() {
            Ok(v) => v,
            Err(FlujoError::VolverAlMenu) => continue,
            Err(e) => return Err(e.into()),
        };

        let pipeline = Arc::new(ImagePipeline::from_config(PipelineConfig::default(), toggles));
        let mut reader = if pipeline.has_slow_stage() {
            match Reader::new(modelo("craft_detector.onnx"), modelo("recognizer_latin_g2.onnx")) {
                Ok(r) => Some(r),
                Err(e) => {
                    app_shell::error(&format!("No se pudo cargar el motor OCR: {e}"));
                    continue;
                }
            }
        } else {
            None
        };

        for xlsx_path in &seleccionados {
            let resultado =
                ejecutar_archivo(xlsx_path, pipeline.clone(), reader.as_mut(), rechazadas_solo).await;
            if let Err(e) = resultado {
                match e {
                    AppError::Flujo(FlujoError::VolverAlMenu) => break,
                    e => app_shell::error(&format!("'{}' falló: {e}", xlsx_path.display())),
                }
            }
        }
    }
}

#[tokio::main]
async fn main() {
    app_shell::mostrar_cabecera("OCR TOOLS — filtrar imágenes por su contenido");
    app_shell::mostrar_diagnostico_sistema();

    if let Err(e) = ciclo_principal().await {
        app_shell::error(&format!("Error fatal: {e}"));
    }
}
