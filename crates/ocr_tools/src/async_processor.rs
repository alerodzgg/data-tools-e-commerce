//! Orquestador async del pipeline de imágenes: descarga → detectores CPU →
//! OCR batched → checkpoint, sobre `tokio`.
//!
//! Este archivo es el orquestador propiamente dicho ([`AsyncBatchProcessor`]:
//! qué hay que hacer, en qué orden y con qué concurrencia). Las dos piezas que
//! usa viven aparte porque se razonan y se prueban solas:
//!   · [`batcher`] — cómo se acumulan y despachan los lotes de OCR.
//!   · [`veredicto`] — cómo cada resultado o fallo se vuelve un veredicto.
//!
//! Decisiones deliberadas de arquitectura:
//!   · Se usa `futures::stream::buffer_unordered` para limitar la
//!     concurrencia, lo que da backpressure sin necesidad de una cola
//!     acotada explícita ni centinelas de cierre a mano.
//!   · El bucle del `OcrBatcher` y el consumo de los resultados corren
//!     CONCURRENTES vía `tokio::join!` dentro de la misma función async, no
//!     como tareas separadas (`tokio::spawn`): así `&mut Reader` (no clonable
//!     ni Send de forma barata) se puede tomar prestado sin envolverlo en un
//!     `Mutex` que serializaría igual toda la inferencia.
//!   · El motor ONNX corre un tensor a la vez (no hay inferencia por lotes
//!     real): `OcrBatcher` sigue acumulando y despachando en grupo por
//!     fidelidad estructural con el diseño del pipeline, pero el cierre que
//!     arma el lote internamente llama a `detect`/`run_slow` una vez por
//!     imagen (`ocr_batch_size=1` por defecto).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use polars::prelude::*;

use crate::batch::{columna_texto, materialize, CellResult, CellTask};
use crate::checkpoint_store::{CheckpointEntry, CheckpointStore};
use crate::downloader::{AsyncImageDownloader, DownloadConfig};
use crate::pipeline::{ImagePipeline, PipelineVerdict};
use crate::reader::Reader;
use crate::url_helper;
use commerce_core::CoreResult;

mod batcher;
mod veredicto;

pub use batcher::OcrBatcher;
use veredicto::{decode_and_run_fast, rechazar_pendientes, veredicto_aislando_panic};

/// El tramo de datos a analizar y su posición dentro del archivo completo.
///
/// Las hojas se procesan de a una, pero el checkpoint indexa por fila del
/// archivo entero: `idx_offset` es lo que traduce entre ambas numeraciones.
/// Va junto a `df`/`url_columns` porque desacoplarlos permite pasar el
/// offset de otra hoja sin que nada lo detecte.
pub struct Bloque<'a> {
    pub df: &'a DataFrame,
    pub url_columns: &'a [String],
    pub idx_offset: i64,
}

/// El motor de análisis: detectores CPU y, si está configurada, la etapa OCR.
///
/// `reader` debe venir `Some` cuando `pipeline.has_slow_stage()`; van juntos
/// para que esa correlación quede a la vista en un solo lugar.
pub struct Motor<'a> {
    pub pipeline: Arc<ImagePipeline>,
    /// Motores OCR disponibles. Uno por hilo: la inferencia ONNX es
    /// CPU-bound y `run_slow` toma `&mut Reader`, así que con un solo motor
    /// el OCR queda serializado por más núcleos que tenga la máquina — y es
    /// la etapa más lenta de todo el pipeline.
    pub readers: Option<&'a mut [Reader]>,
}

/// Qué se sabe ya de corridas anteriores y dónde se persiste lo nuevo.
pub struct Persistencia<'a> {
    pub checkpoint: Arc<CheckpointStore>,
    pub cached: &'a HashMap<(i64, String), CheckpointEntry>,
}

// ════════════════════════════════════════════════════════════════════════
// AsyncBatchProcessor
// ════════════════════════════════════════════════════════════════════════

pub struct AsyncBatchProcessor {
    download_cfg: DownloadConfig,
    ocr_batch_size: usize,
    ocr_batch_timeout: Duration,
    max_concurrency: usize,
    checkpoint_every: usize,
    /// Un solo `reqwest::Client` (con su pool de conexiones) para todo el
    /// archivo, no uno nuevo por hoja: `process` se llama una vez por hoja, y
    /// reconstruir el cliente en cada llamada perdería el reuso de conexiones
    /// entre hojas del mismo libro.
    downloader_cache: tokio::sync::OnceCell<AsyncImageDownloader>,
}

impl AsyncBatchProcessor {
    pub fn new(
        download_cfg: DownloadConfig,
        ocr_batch_size: usize,
        ocr_batch_timeout: Duration,
        max_concurrency: usize,
        checkpoint_every: usize,
    ) -> Self {
        Self {
            download_cfg,
            ocr_batch_size,
            ocr_batch_timeout,
            max_concurrency,
            checkpoint_every,
            downloader_cache: tokio::sync::OnceCell::new(),
        }
    }
}

pub struct ProcessOutcome {
    pub df: DataFrame,
    pub imagenes_analizadas: usize,
    pub imagenes_rechazadas: usize,
}

/// Tareas pendientes: celdas-URL de `url_columns` que no están en `cached` y
/// cuyo valor es efectivamente una URL de imagen.
fn pending_tasks(
    df: &DataFrame,
    url_columns: &[String],
    cached: &HashMap<(i64, String), CheckpointEntry>,
    idx_offset: i64,
) -> PolarsResult<Vec<CellTask>> {
    let n = df.height();
    let mut columnas: HashMap<&str, Vec<Option<String>>> = HashMap::new();
    for col in url_columns {
        columnas.insert(col.as_str(), columna_texto(df, col)?);
    }

    let mut tasks = Vec::new();
    // `local` indexa varias columnas distintas por fila (no una sola lista):
    // no hay iterador único con `.enumerate()` que reemplace este patrón.
    #[allow(clippy::needless_range_loop)]
    for local in 0..n {
        let gidx = idx_offset + local as i64;
        for col in url_columns {
            if cached.contains_key(&(gidx, col.clone())) {
                continue;
            }
            let url = columnas[col.as_str()][local]
                .as_deref()
                .unwrap_or("")
                .trim()
                .to_string();
            if url_helper::is_image(Some(&url)) {
                tasks.push(CellTask {
                    idx: gidx,
                    col: col.clone(),
                    url,
                });
            }
        }
    }
    Ok(tasks)
}

/// Cuántas celdas-imagen hay por evaluar (`pendientes`, red/OCR) y cuántas ya
/// resueltas por el checkpoint (`desde_cache`, instantáneas) para este bloque.
/// Pensado para dimensionar una barra de progreso ANTES de llamar a
/// [`AsyncBatchProcessor::process`] (que hace exactamente este mismo cálculo
/// puro sobre un `DataFrame` ya en memoria — sin I/O — así que repetirlo acá
/// no tiene el costo que sí tendría, por ejemplo, releer un archivo).
pub fn contar_trabajo(
    df: &DataFrame,
    url_columns: &[String],
    cached: &HashMap<(i64, String), CheckpointEntry>,
    idx_offset: i64,
) -> PolarsResult<(usize, usize)> {
    let n = df.height() as i64;
    let pendientes = pending_tasks(df, url_columns, cached, idx_offset)?.len();
    let desde_cache = cached
        .keys()
        .filter(|(gidx, col)| {
            *gidx >= idx_offset && *gidx < idx_offset + n && url_columns.iter().any(|c| c == col)
        })
        .count();
    Ok((pendientes, desde_cache))
}

impl AsyncBatchProcessor {
    /// Procesa un BLOQUE (una hoja). `idx_offset` desplaza el índice local
    /// al índice GLOBAL del archivo. Devuelve el `DataFrame` con los
    /// veredictos materializados y los conteos a nivel de IMAGEN.
    ///
    /// `progreso` se llama una vez por celda-imagen resuelta (tanto las que
    /// vienen del checkpoint, de inmediato, como las que requieren red/OCR, a
    /// medida que terminan) — quien llame puede usarlo para avanzar una barra
    /// de progreso real, no solo "hoja completada". `avisar` recibe mensajes
    /// no fatales (p. ej. un fallo de escritura del checkpoint no detiene el
    /// procesamiento, pero el usuario debe enterarse).
    pub async fn process(
        &self,
        bloque: Bloque<'_>,
        motor: Motor<'_>,
        persistencia: Persistencia<'_>,
        mut progreso: impl FnMut(u64),
        mut avisar: impl FnMut(&str),
    ) -> CoreResult<ProcessOutcome> {
        let Bloque {
            df,
            url_columns,
            idx_offset,
        } = bloque;
        let Persistencia { checkpoint, cached } = persistencia;
        let n = df.height() as i64;
        let tasks = pending_tasks(df, url_columns, cached, idx_offset)?;

        let mut results: HashMap<(i64, String), CellResult> = cached
            .iter()
            .filter(|((gidx, col), _)| {
                *gidx >= idx_offset && *gidx < idx_offset + n && url_columns.iter().any(|c| c == col)
            })
            .map(|((gidx, col), entry)| {
                (
                    (*gidx, col.clone()),
                    CellResult {
                        idx: *gidx,
                        col: col.clone(),
                        approved: entry.approved,
                        reason: entry.reason.clone(),
                    },
                )
            })
            .collect();
        progreso(results.len() as u64);

        if !tasks.is_empty() {
            self.run_async(tasks, &mut results, motor, checkpoint, &mut progreso, &mut avisar)
                .await;
        }

        let imagenes_analizadas = results.len();
        let imagenes_rechazadas = results.values().filter(|cr| !cr.approved).count();
        let df_result = materialize(df, url_columns, &results, idx_offset)?;
        Ok(ProcessOutcome {
            df: df_result,
            imagenes_analizadas,
            imagenes_rechazadas,
        })
    }

    async fn run_async(
        &self,
        tasks: Vec<CellTask>,
        results: &mut HashMap<(i64, String), CellResult>,
        motor: Motor<'_>,
        checkpoint: Arc<CheckpointStore>,
        progreso: &mut dyn FnMut(u64),
        avisar: &mut dyn FnMut(&str),
    ) {
        let Motor { pipeline, readers } = motor;
        if pipeline.has_slow_stage() && readers.as_deref().map_or(true, <[Reader]>::is_empty) {
            // Invariante violada: si el pipeline tiene etapa OCR configurada,
            // `reader` debe venir `Some` (lo garantiza hoy el único llamador
            // real, `bin/ocr_tools.rs`). Sin este guard, el `match` de más
            // abajo cayendo en la rama sin `reader` nunca corre `despacho`
            // (el bucle que resuelve los `oneshot::Receiver` de `OcrBatcher`),
            // así que cada `batcher.submit(...)` de `consumo` quedaría
            // esperando para siempre — un cuelgue silencioso, sin mensaje ni
            // panic, mucho más difícil de diagnosticar que rechazar temprano.
            let motivo = "Pipeline con etapa OCR configurada pero sin lector: tareas rechazadas \
                          para no colgar el proceso (bug interno del llamador, no de los datos)"
                .to_string();
            let entradas = rechazar_pendientes(&tasks, results, &motivo);
            if let Err(error) = checkpoint.append_many(&entradas) {
                avisar(&format!("No se pudo escribir el checkpoint: {error}"));
            }
            avisar(&motivo);
            progreso(tasks.len() as u64);
            return;
        }

        // Bajo test (de esta librería o de tests/, vía la feature
        // `test-support`), el servidor mock corre en 127.0.0.1: el
        // downloader de producción lo rechazaría por el filtro anti-SSRF
        // (ver `downloader::tests`). En release ese constructor alternativo
        // ni siquiera existe.
        #[cfg(not(any(test, feature = "test-support")))]
        let nuevo_downloader = self
            .downloader_cache
            .get_or_try_init(|| async { AsyncImageDownloader::new(self.download_cfg.clone()) })
            .await;
        #[cfg(any(test, feature = "test-support"))]
        let nuevo_downloader = self
            .downloader_cache
            .get_or_try_init(|| async {
                AsyncImageDownloader::nuevo_para_test_con_hosts_privados_permitidos(self.download_cfg.clone())
            })
            .await;

        let downloader = match nuevo_downloader {
            Ok(d) => d,
            Err(error) => {
                // No se pudo construir el cliente HTTP (p. ej. backend TLS no
                // disponible en este entorno). Las tareas pendientes NO deben
                // quedar como "aprobadas" por el default de `materialize`
                // (ese default es correcto solo para celdas que nunca fueron
                // tareas) — se marcan explícitamente rechazadas, con el motivo
                // real, para que el usuario se entere en vez de recibir
                // imágenes sin evaluar coladas como válidas.
                let motivo = format!("No se pudo inicializar el descargador: {error}");
                let entradas = rechazar_pendientes(&tasks, results, &motivo);
                if let Err(error) = checkpoint.append_many(&entradas) {
                    avisar(&format!("No se pudo escribir el checkpoint: {error}"));
                }
                progreso(tasks.len() as u64);
                return;
            }
        };
        let resize_max_dim = self.download_cfg.resize_max_dim;

        let batcher = pipeline
            .has_slow_stage()
            .then(|| OcrBatcher::new(self.ocr_batch_size, self.ocr_batch_timeout));

        let procesar_una = |task: CellTask| {
            let pipeline = pipeline.clone();
            let batcher = batcher.clone();
            async move {
                let content = downloader.fetch(&task.url).await;
                let (verdict, ctx) = decode_and_run_fast(pipeline, resize_max_dim, content).await;
                let veredicto = match (verdict, ctx, &batcher) {
                    (Some(v), _, _) => v,
                    (None, Some(ctx), Some(batcher)) => batcher.submit(ctx).await,
                    // Inalcanzable hoy: `run_fast` solo devuelve `None` (pide
                    // seguir a OCR) cuando `pipeline.has_slow_stage()` es
                    // `true`, la misma condición que hace `batcher: Some` dos
                    // líneas más arriba — pero si un cambio futuro rompiera
                    // esa correlación, rechazar es más seguro que aprobar por
                    // defecto una imagen que nunca se evaluó.
                    (None, _, _) => PipelineVerdict {
                        approved: false,
                        reasons: vec!["Fallo interno: imagen sin evaluar".to_string()],
                    },
                };
                (task, veredicto)
            }
        };

        let mut flujo = stream::iter(tasks)
            .map(procesar_una)
            .buffer_unordered(self.max_concurrency.max(1));

        let mut buffer_checkpoint: Vec<CheckpointEntry> = Vec::new();

        // Escribir el checkpoint es I/O síncrono (`writeln!` a disco); correrlo
        // inline bloquearía el worker thread de tokio en cada lote. `spawn_blocking`
        // lo mueve al pool de bloqueo y libera el worker mientras se escribe.
        async fn volcar_checkpoint(
            checkpoint: &Arc<CheckpointStore>,
            buffer: &mut Vec<CheckpointEntry>,
            avisar: &mut dyn FnMut(&str),
        ) {
            if buffer.is_empty() {
                return;
            }
            let lote = std::mem::take(buffer);
            let checkpoint = checkpoint.clone();
            match tokio::task::spawn_blocking(move || checkpoint.append_many(&lote)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => avisar(&format!("No se pudo escribir el checkpoint: {error}")),
                Err(_join_error) => {
                    avisar("No se pudo escribir el checkpoint: tarea de escritura interrumpida")
                }
            }
        }

        let consumo = async {
            while let Some((task, veredicto)) = flujo.next().await {
                let cr = CellResult {
                    idx: task.idx,
                    col: task.col.clone(),
                    approved: veredicto.approved,
                    reason: veredicto.reasons.join(" | "),
                };
                buffer_checkpoint.push(CheckpointEntry {
                    idx: cr.idx,
                    col: cr.col.clone(),
                    approved: cr.approved,
                    reason: cr.reason.clone(),
                });
                results.insert((cr.idx, cr.col.clone()), cr);
                progreso(1);
                if buffer_checkpoint.len() >= self.checkpoint_every.max(1) {
                    volcar_checkpoint(&checkpoint, &mut buffer_checkpoint, avisar).await;
                }
            }
            if let Some(batcher) = &batcher {
                batcher.close();
            }
        };

        match (batcher.as_ref(), readers) {
            (Some(batcher), Some(readers)) if !readers.is_empty() => {
                let pipeline_ref = &*pipeline;
                let despacho = batcher.run(|ctxs| {
                    // `run_slow` es CPU-bound (inferencia ONNX real) y puede
                    // panickear ante un modelo/dato inesperado (p. ej. una
                    // forma de tensor que `session.rs` no anticipó). Sin
                    // aislamiento, un panic tumbaría el proceso completo y
                    // perdería hasta `checkpoint_every` resultados aún no
                    // volcados a disco. `block_in_place`
                    // libera al scheduler de tokio mientras corre (requiere
                    // el runtime multi-thread que usa `#[tokio::main]` en
                    // `bin/ocr_tools.rs`); `catch_unwind` limita el daño de un
                    // panic a la imagen que lo causó — el resto del lote y el
                    // checkpoint siguen su curso normal.
                    //
                    // El lote se REPARTE entre los motores del pool, un hilo
                    // por motor: `run_slow` toma `&mut Reader`, así que el
                    // paralelismo real lo da tener varios motores, no varios
                    // hilos sobre uno.
                    tokio::task::block_in_place(|| {
                        let n = readers.len().min(ctxs.len()).max(1);
                        let por_hilo = ctxs.len().div_ceil(n);
                        let mut salida: Vec<_> = std::thread::scope(|ambito| {
                            let mut handles = Vec::new();
                            for (h, (trozo, motor)) in
                                ctxs.chunks(por_hilo.max(1)).zip(readers.iter_mut()).enumerate()
                            {
                                handles.push(ambito.spawn(move || {
                                    let v: Vec<_> = trozo
                                        .iter()
                                        .map(|ctx| {
                                            veredicto_aislando_panic(std::panic::AssertUnwindSafe(|| {
                                                pipeline_ref.run_slow(ctx, motor)
                                            }))
                                        })
                                        .collect();
                                    (h, v)
                                }));
                            }
                            handles.into_iter().filter_map(|h| h.join().ok()).collect()
                        });
                        // El orden de salida DEBE seguir al de entrada: el
                        // llamador aparea veredictos con filas por posición.
                        salida.sort_by_key(|(h, _)| *h);
                        salida.into_iter().flat_map(|(_, v)| v).collect()
                    })
                });
                tokio::join!(consumo, despacho);
            }
            _ => consumo.await,
        }

        volcar_checkpoint(&checkpoint, &mut buffer_checkpoint, avisar).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloader::FalloDescarga;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn pending_tasks_ignora_celdas_cacheadas_y_no_urls() {
        let df = df! {
            "Imagen 1" => ["http://x.com/1.jpg", "no es url", "http://x.com/3.jpg"],
        }
        .unwrap();
        let url_columns = vec!["Imagen 1".to_string()];
        let mut cached = HashMap::new();
        cached.insert(
            (0i64, "Imagen 1".to_string()),
            CheckpointEntry {
                idx: 0,
                col: "Imagen 1".to_string(),
                approved: true,
                reason: String::new(),
            },
        );

        let tasks = pending_tasks(&df, &url_columns, &cached, 0).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].idx, 2);
        assert_eq!(tasks[0].url, "http://x.com/3.jpg");
    }
    #[tokio::test]
    async fn process_sin_tareas_pendientes_reusa_el_cache_y_no_llama_red() {
        let df = df! {
            "Imagen 1" => ["http://x.com/1.jpg"],
        }
        .unwrap();
        let url_columns = vec!["Imagen 1".to_string()];
        let mut cached = HashMap::new();
        cached.insert(
            (0i64, "Imagen 1".to_string()),
            CheckpointEntry {
                idx: 0,
                col: "Imagen 1".to_string(),
                approved: false,
                reason: "D1·Banner".to_string(),
            },
        );

        let procesador =
            AsyncBatchProcessor::new(DownloadConfig::default(), 1, Duration::from_millis(50), 4, 100);
        let pipeline = Arc::new(ImagePipeline::from_config(
            crate::pipeline::PipelineConfig::default(),
            crate::pipeline::DetectorToggles::default(),
        ));
        let tmp = tempfile::tempdir().unwrap();
        let checkpoint = Arc::new(CheckpointStore::new(tmp.path().join("cp.jsonl")));

        let outcome = procesador
            .process(
                Bloque {
                    df: &df,
                    url_columns: &url_columns,
                    idx_offset: 0,
                },
                Motor {
                    pipeline,
                    readers: None,
                },
                Persistencia {
                    checkpoint,
                    cached: &cached,
                },
                |_| {},
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(outcome.imagenes_analizadas, 1);
        assert_eq!(outcome.imagenes_rechazadas, 1);
        assert_eq!(
            outcome.df.column("Imagen 1").unwrap().str().unwrap().get(0),
            Some("")
        );
    }
    #[tokio::test]
    async fn si_el_downloader_no_se_puede_inicializar_las_tareas_pendientes_se_rechazan_en_vez_de_aprobarse_por_defecto(
    ) {
        let df = df! {
            "Imagen 1" => ["http://x.com/1.jpg"],
        }
        .unwrap();
        let url_columns = vec!["Imagen 1".to_string()];

        let procesador = AsyncBatchProcessor::new(
            // `\r\n` en el user-agent es un valor de header HTTP inválido:
            // `reqwest::Client::builder().build()` falla de forma
            // determinista, sin necesidad de tocar red — el mismo camino que
            // dispararía cualquier otro fallo de construcción del cliente.
            DownloadConfig {
                user_agent: "agente\r\ninvalido".to_string(),
                ..DownloadConfig::default()
            },
            1,
            Duration::from_millis(50),
            4,
            100,
        );
        let pipeline = Arc::new(ImagePipeline::from_config(
            crate::pipeline::PipelineConfig::default(),
            crate::pipeline::DetectorToggles::default(),
        ));
        let tmp = tempfile::tempdir().unwrap();
        let checkpoint = Arc::new(CheckpointStore::new(tmp.path().join("cp.jsonl")));

        let avance = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let avance_cb = avance.clone();
        let outcome = procesador
            .process(
                Bloque {
                    df: &df,
                    url_columns: &url_columns,
                    idx_offset: 0,
                },
                Motor {
                    pipeline,
                    readers: None,
                },
                Persistencia {
                    checkpoint,
                    cached: &HashMap::new(),
                },
                move |n| {
                    avance_cb.fetch_add(n, Ordering::SeqCst);
                },
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(
            outcome.imagenes_rechazadas, 1,
            "una tarea pendiente nunca evaluada por un downloader roto debe quedar RECHAZADA, no aprobada por defecto"
        );
        assert_eq!(
            outcome.df.column("Imagen 1").unwrap().str().unwrap().get(0),
            Some(""),
            "la URL no evaluada debe vaciarse igual que cualquier otra imagen rechazada"
        );
        assert_eq!(
            avance.load(Ordering::SeqCst),
            1,
            "el progreso debe avanzar igual aunque el downloader no se haya podido inicializar"
        );
    }
    #[tokio::test]
    async fn si_falla_la_escritura_del_checkpoint_se_avisa_en_vez_de_tragarse_el_error() {
        let df = df! {
            // Puerto sin listener: falla rápido (conexión rechazada), sin
            // esperar el timeout completo.
            "Imagen 1" => ["http://127.0.0.1:1/no-existe.jpg"],
        }
        .unwrap();
        let url_columns = vec!["Imagen 1".to_string()];

        let procesador = AsyncBatchProcessor::new(
            DownloadConfig {
                timeout: Duration::from_millis(200),
                retries: 0,
                backoff: Duration::from_millis(1),
                ..DownloadConfig::default()
            },
            1,
            Duration::from_millis(50),
            4,
            100,
        );
        let pipeline = Arc::new(ImagePipeline::from_config(
            crate::pipeline::PipelineConfig::default(),
            crate::pipeline::DetectorToggles::default(),
        ));
        let tmp = tempfile::tempdir().unwrap();
        // Un DIRECTORIO en vez de un archivo: `append_many` falla al abrir
        // para escribir, de forma determinista y sin tocar permisos del SO.
        let checkpoint = Arc::new(CheckpointStore::new(tmp.path().to_path_buf()));

        let avisos = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let avisos_cb = avisos.clone();
        let _outcome = procesador
            .process(
                Bloque {
                    df: &df,
                    url_columns: &url_columns,
                    idx_offset: 0,
                },
                Motor {
                    pipeline,
                    readers: None,
                },
                Persistencia {
                    checkpoint,
                    cached: &HashMap::new(),
                },
                |_| {},
                move |m: &str| avisos_cb.lock().unwrap().push(m.to_string()),
            )
            .await
            .unwrap();

        let avisos = avisos.lock().unwrap();
        assert!(
            avisos.iter().any(|m| m.contains("checkpoint")),
            "un fallo real de escritura del checkpoint debe avisarse, no descartarse en silencio con `let _ =`: {avisos:?}"
        );
    }
    #[tokio::test]
    async fn el_motivo_de_rechazo_dice_la_causa_real_no_un_texto_fijo() {
        // Un texto fijo para todo fallo de descarga ("No se pudo descargar:
        // timeout o URL inválida") manda a revisar los datos del usuario
        // incluso cuando el problema es del lado del servidor y las URLs son
        // válidas. Acá el fallo real es "conexión
        // rechazada" (puerto sin listener), y el motivo debe decir eso, sin
        // acusar a la URL de estar mal formada.
        let df = df! {
            "Imagen 1" => ["http://127.0.0.1:1/no-existe.jpg"],
        }
        .unwrap();
        let url_columns = vec!["Imagen 1".to_string()];

        let procesador = AsyncBatchProcessor::new(
            DownloadConfig {
                timeout: Duration::from_millis(200),
                retries: 0,
                backoff: Duration::from_millis(1),
                ..DownloadConfig::default()
            },
            1,
            Duration::from_millis(50),
            4,
            100,
        );
        // Sin etapa OCR: con `d4_d5_d6` activo y `reader: None`, `run_async`
        // corta antes por su propio guard de invariante y nunca llega a
        // intentar la descarga, que es justo lo que este test mide.
        let pipeline = Arc::new(ImagePipeline::from_config(
            crate::pipeline::PipelineConfig::default(),
            crate::pipeline::DetectorToggles {
                d4_d5_d6: false,
                ..crate::pipeline::DetectorToggles::default()
            },
        ));
        let tmp = tempfile::tempdir().unwrap();
        let checkpoint = Arc::new(CheckpointStore::new(tmp.path().join("cp.jsonl")));

        let outcome = procesador
            .process(
                Bloque {
                    df: &df,
                    url_columns: &url_columns,
                    idx_offset: 0,
                },
                Motor {
                    pipeline,
                    readers: None,
                },
                Persistencia {
                    checkpoint,
                    cached: &HashMap::new(),
                },
                |_| {},
                |_| {},
            )
            .await
            .unwrap();

        let motivo = outcome
            .df
            .column("_imagen_motivo")
            .unwrap()
            .str()
            .unwrap()
            .get(0)
            .unwrap()
            .to_string();
        // Un puerto sin listener da "conexión rechazada" en Linux pero
        // agota el timeout en Windows: las dos son causas de RED concretas y
        // correctas, así que se acepta cualquiera. Lo que el test fija es
        // que se nombre una causa real, no un texto fijo genérico.
        assert!(
            motivo.contains(&FalloDescarga::ErrorDeRed.to_string())
                || motivo.contains(&FalloDescarga::Timeout.to_string()),
            "el motivo debe nombrar la causa real de red, no una genérica: {motivo:?}"
        );
        assert!(
            !motivo.contains("URL inválida") && !motivo.contains("URL mal formada"),
            "la URL es válida: el motivo no debe acusarla: {motivo:?}"
        );
    }
}
