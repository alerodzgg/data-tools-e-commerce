//! Orquestador async del pipeline de imágenes: descarga → detectores CPU →
//! OCR batched → checkpoint. Implementa `OcrBatcher` + `AsyncBatchProcessor`
//! sobre `tokio`.
//!
//! Decisiones deliberadas de arquitectura:
//!   · Se usa `futures::stream::buffer_unordered` para limitar la
//!     concurrencia, lo que da backpressure sin necesidad de una cola
//!     acotada explícita ni centinelas de cierre a mano.
//!   · El bucle del `OcrBatcher` (`OcrBatcher::run`) y el consumo de los
//!     resultados corren CONCURRENTES vía `tokio::join!` dentro de la misma
//!     función async, no como tareas separadas (`tokio::spawn`): así
//!     `&mut Reader` (no clonable ni Send de forma barata) se puede tomar
//!     prestado sin envolverlo en un `Mutex` que serializaría igual toda la
//!     inferencia.
//!   · El motor ONNX corre un tensor a la vez (no hay inferencia por lotes
//!     real): `OcrBatcher` sigue acumulando y despachando en grupo por
//!     fidelidad estructural con el diseño del pipeline, pero el cierre que
//!     arma el lote internamente llama a `detect`/`run_slow` una vez por
//!     imagen (`ocr_batch_size=1` por defecto).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use polars::prelude::*;
use tokio::sync::{oneshot, Mutex as AsyncMutex, Notify};

use crate::batch::{columna_texto, materialize, CellResult, CellTask};
use crate::checkpoint_store::{CheckpointEntry, CheckpointStore};
use crate::detectors::ImageContext;
use crate::downloader::{decode_and_resize, AsyncImageDownloader, DownloadConfig};
use crate::pipeline::{ImagePipeline, PipelineVerdict};
use crate::reader::Reader;
use crate::url_helper;
use commerce_core::CoreResult;

// ════════════════════════════════════════════════════════════════════════
// OcrBatcher
// ════════════════════════════════════════════════════════════════════════

struct BatchItem {
    ctx: ImageContext,
    tx: oneshot::Sender<PipelineVerdict>,
}

/// Acumula contextos que requieren OCR y los despacha en lotes. `run` es
/// genérico sobre el cierre que decide qué hacer con cada lote, para poder
/// probar la mecánica de acumulación/despacho sin necesidad de un modelo
/// ONNX real cargado.
#[derive(Clone)]
pub struct OcrBatcher {
    items: Arc<AsyncMutex<VecDeque<BatchItem>>>,
    notify: Arc<Notify>,
    closed: Arc<AtomicBool>,
    batch_size: usize,
    batch_timeout: Duration,
}

impl OcrBatcher {
    pub fn new(batch_size: usize, batch_timeout: Duration) -> Self {
        Self {
            items: Arc::new(AsyncMutex::new(VecDeque::new())),
            notify: Arc::new(Notify::new()),
            closed: Arc::new(AtomicBool::new(false)),
            batch_size: batch_size.max(1),
            batch_timeout,
        }
    }

    pub async fn submit(&self, ctx: ImageContext) -> PipelineVerdict {
        let (tx, rx) = oneshot::channel();
        {
            let mut guard = self.items.lock().await;
            guard.push_back(BatchItem { ctx, tx });
        }
        self.notify.notify_one();
        // El `tx` correspondiente solo se dropea sin enviar si `procesar_lote`
        // devuelve menos veredictos que contextos recibió (hoy no ocurre: el
        // único cierre real es 1:1) o panickea a mitad de lote. En cualquiera
        // de los dos casos esta tarea nunca se evaluó — igual que las otras
        // rutas de fail-open ya corregidas, se rechaza explícitamente en vez
        // de aprobarse por defecto.
        rx.await.unwrap_or_else(|_| PipelineVerdict {
            approved: false,
            reasons: vec!["Fallo interno de lote OCR: el resultado nunca llegó".to_string()],
        })
    }

    /// No hay más `submit()` por venir. `run` termina en cuanto el buffer
    /// quede vacío.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    /// Corre hasta que `close()` se haya llamado y el buffer quede vacío.
    /// `procesar_lote` recibe hasta `batch_size` contextos y devuelve un
    /// veredicto por cada uno, EN EL MISMO ORDEN.
    pub async fn run(&self, mut procesar_lote: impl FnMut(&[ImageContext]) -> Vec<PipelineVerdict>) {
        loop {
            // Solo esperar notify/timeout si el buffer está VACÍO: si una
            // ráfaga de `submit()` ya dejó varios ítems encolados mientras
            // este lote se procesaba, `notify_one()` colapsa esas llamadas en
            // un único permiso — sin este chequeo, cada ítem restante de la
            // ráfaga esperaría el `batch_timeout` COMPLETO antes de
            // despacharse, en vez de drenarse de inmediato.
            let buffer_vacio = {
                let guard = self.items.lock().await;
                if guard.is_empty() && self.closed.load(Ordering::SeqCst) {
                    return;
                }
                guard.is_empty()
            };
            if buffer_vacio {
                let _ = tokio::time::timeout(self.batch_timeout, self.notify.notified()).await;
            }

            let lote: Vec<BatchItem> = {
                let mut guard = self.items.lock().await;
                if guard.is_empty() {
                    if self.closed.load(Ordering::SeqCst) {
                        return;
                    }
                    continue;
                }
                let n = self.batch_size.min(guard.len());
                guard.drain(..n).collect()
            };

            let mut ctxs = Vec::with_capacity(lote.len());
            let mut txs = Vec::with_capacity(lote.len());
            for item in lote {
                ctxs.push(item.ctx);
                txs.push(item.tx);
            }

            let resultados = procesar_lote(&ctxs);
            for (tx, resultado) in txs.into_iter().zip(resultados) {
                let _ = tx.send(resultado);
            }
        }
    }
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
    /// archivo, no uno nuevo por hoja: `process` se llama una vez por hoja y
    /// antes `run_async` reconstruía el cliente en cada llamada, perdiendo el
    /// reuso de conexiones entre hojas del mismo libro.
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

const SIN_DESCARGA: &str = "No se pudo descargar (timeout o URL inválida)";

/// Decode + detectores CPU en un solo salto a un thread de bloqueo (igual
/// que `_decode_and_run_fast`: decodificar y correr D1-D3 juntos evita un
/// segundo round-trip y deja el runtime async libre para I/O de red).
async fn decode_and_run_fast(
    pipeline: Arc<ImagePipeline>,
    resize_max_dim: Option<u32>,
    content: Option<Vec<u8>>,
) -> (Option<PipelineVerdict>, Option<ImageContext>) {
    let Some(content) = content else {
        return (
            Some(PipelineVerdict {
                approved: false,
                reasons: vec![SIN_DESCARGA.to_string()],
            }),
            None,
        );
    };

    let resultado = tokio::task::spawn_blocking(move || {
        let rgb = decode_and_resize(&content, resize_max_dim)?;
        let ctx = ImageContext::new(&rgb);
        let veredicto = pipeline.run_fast(&ctx);
        Some((veredicto, ctx))
    })
    .await;

    match resultado {
        Ok(Some((Some(v), _ctx))) => (Some(v), None),
        Ok(Some((None, ctx))) => (None, Some(ctx)),
        _ => (
            Some(PipelineVerdict {
                approved: false,
                reasons: vec![SIN_DESCARGA.to_string()],
            }),
            None,
        ),
    }
}

/// Marca todas las `tasks` como rechazadas con `motivo` (en `results`, para
/// que `materialize` las vuelque, y devuelve las `CheckpointEntry` para
/// persistirlas) — usado por los dos casos de "no se puede evaluar esta
/// tarea por una falla de infraestructura, no de los datos": no deben quedar
/// aprobadas por el default de `materialize` (correcto solo para celdas que
/// nunca fueron tareas), sino explícitamente rechazadas con un motivo real.
fn rechazar_pendientes(
    tasks: &[CellTask],
    results: &mut HashMap<(i64, String), CellResult>,
    motivo: &str,
) -> Vec<CheckpointEntry> {
    for t in tasks {
        results.insert(
            (t.idx, t.col.clone()),
            CellResult {
                idx: t.idx,
                col: t.col.clone(),
                approved: false,
                reason: motivo.to_string(),
            },
        );
    }
    tasks
        .iter()
        .map(|t| CheckpointEntry {
            idx: t.idx,
            col: t.col.clone(),
            approved: false,
            reason: motivo.to_string(),
        })
        .collect()
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
    #[allow(clippy::too_many_arguments)]
    pub async fn process(
        &self,
        df: &DataFrame,
        url_columns: &[String],
        pipeline: Arc<ImagePipeline>,
        reader: Option<&mut Reader>,
        checkpoint: Arc<CheckpointStore>,
        cached: &HashMap<(i64, String), CheckpointEntry>,
        idx_offset: i64,
        mut progreso: impl FnMut(u64),
        mut avisar: impl FnMut(&str),
    ) -> CoreResult<ProcessOutcome> {
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
            self.run_async(
                tasks,
                &mut results,
                pipeline,
                reader,
                checkpoint,
                &mut progreso,
                &mut avisar,
            )
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

    #[allow(clippy::too_many_arguments)]
    async fn run_async(
        &self,
        tasks: Vec<CellTask>,
        results: &mut HashMap<(i64, String), CellResult>,
        pipeline: Arc<ImagePipeline>,
        reader: Option<&mut Reader>,
        checkpoint: Arc<CheckpointStore>,
        progreso: &mut dyn FnMut(u64),
        avisar: &mut dyn FnMut(&str),
    ) {
        if pipeline.has_slow_stage() && reader.is_none() {
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

        match (batcher.as_ref(), reader) {
            (Some(batcher), Some(reader)) => {
                let pipeline_ref = &*pipeline;
                let despacho = batcher.run(|ctxs| {
                    // `run_slow` es CPU-bound (inferencia ONNX real) y puede
                    // panickear ante un modelo/dato inesperado (p. ej. una
                    // forma de tensor que `session.rs` no anticipó). Antes
                    // corría inline sin aislamiento: un panic tumbaba el
                    // proceso completo y perdía hasta `checkpoint_every`
                    // resultados aún no volcados a disco. `block_in_place`
                    // libera al scheduler de tokio mientras corre (requiere
                    // el runtime multi-thread que usa `#[tokio::main]` en
                    // `bin/ocr_tools.rs`); `catch_unwind` limita el daño de un
                    // panic a la imagen que lo causó — el resto del lote y el
                    // checkpoint siguen su curso normal.
                    tokio::task::block_in_place(|| {
                        ctxs.iter()
                            .map(|ctx| {
                                veredicto_aislando_panic(std::panic::AssertUnwindSafe(|| {
                                    pipeline_ref.run_slow(ctx, reader)
                                }))
                            })
                            .collect()
                    })
                });
                tokio::join!(consumo, despacho);
            }
            _ => consumo.await,
        }

        volcar_checkpoint(&checkpoint, &mut buffer_checkpoint, avisar).await;
    }
}

/// Traduce el resultado de `run_slow` a un veredicto. Un fallo de inferencia
/// OCR es tan "no evaluado" como uno de descarga: aprobar por defecto (como
/// hacía este código antes) dejaría pasar en silencio imágenes que nunca
/// llegaron a analizarse.
fn veredicto_de_resultado_ocr(resultado: ort::Result<PipelineVerdict>) -> PipelineVerdict {
    resultado.unwrap_or_else(|error| PipelineVerdict {
        approved: false,
        reasons: vec![format!("Fallo de inferencia OCR: {error}")],
    })
}

/// Corre `inferencia` (una llamada a `run_slow` para una sola imagen) tras
/// `catch_unwind`: un panic durante la inferencia OCR (p. ej. un modelo con
/// una forma de tensor inesperada que se cuele más allá de la validación de
/// `session.rs`) se traduce en un rechazo para ESA imagen, en vez de
/// propagarse y tumbar el proceso completo — perdiendo, de paso, el resto
/// del lote y cualquier resultado del checkpoint aún no volcado a disco.
fn veredicto_aislando_panic(
    inferencia: impl FnOnce() -> ort::Result<PipelineVerdict> + std::panic::UnwindSafe,
) -> PipelineVerdict {
    match std::panic::catch_unwind(inferencia) {
        Ok(resultado) => veredicto_de_resultado_ocr(resultado),
        Err(_panic) => PipelineVerdict {
            approved: false,
            reasons: vec!["Fallo interno (panic) durante la inferencia OCR".to_string()],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdicto(approved: bool) -> PipelineVerdict {
        PipelineVerdict {
            approved,
            reasons: if approved {
                Vec::new()
            } else {
                vec!["motivo".to_string()]
            },
        }
    }

    fn ctx_de_prueba() -> ImageContext {
        ImageContext::new(&image::RgbImage::from_pixel(4, 4, image::Rgb([200, 200, 200])))
    }

    #[tokio::test]
    async fn submit_se_resuelve_cuando_run_procesa_el_lote() {
        let batcher = OcrBatcher::new(4, Duration::from_millis(20));
        let batcher_run = batcher.clone();

        let tarea_run = tokio::spawn(async move {
            batcher_run
                .run(|ctxs| ctxs.iter().map(|_| verdicto(true)).collect())
                .await;
        });

        let resultado = batcher.submit(ctx_de_prueba()).await;
        assert!(resultado.approved);

        batcher.close();
        tarea_run.await.unwrap();
    }

    #[tokio::test]
    async fn agrupa_en_lotes_de_a_lo_sumo_batch_size() {
        let batcher = OcrBatcher::new(2, Duration::from_millis(20));
        let batcher_run = batcher.clone();
        let tamanos_vistos = Arc::new(std::sync::Mutex::new(Vec::new()));
        let tamanos_run = tamanos_vistos.clone();

        let tarea_run = tokio::spawn(async move {
            batcher_run
                .run(|ctxs| {
                    tamanos_run.lock().unwrap().push(ctxs.len());
                    ctxs.iter().map(|_| verdicto(true)).collect()
                })
                .await;
        });

        let mut envios = Vec::new();
        for _ in 0..5 {
            let b = batcher.clone();
            envios.push(tokio::spawn(async move { b.submit(ctx_de_prueba()).await }));
        }
        for envio in envios {
            assert!(envio.await.unwrap().approved);
        }

        batcher.close();
        tarea_run.await.unwrap();

        let vistos = tamanos_vistos.lock().unwrap();
        assert_eq!(vistos.iter().sum::<usize>(), 5);
        assert!(
            vistos.iter().all(|&n| n <= 2),
            "ningun lote debe superar batch_size=2: {vistos:?}"
        );
    }

    #[tokio::test]
    async fn un_lote_parcial_se_despacha_por_timeout_sin_esperar_a_llenarse() {
        let batcher = OcrBatcher::new(10, Duration::from_millis(15));
        let batcher_run = batcher.clone();
        let tarea_run = tokio::spawn(async move {
            batcher_run
                .run(|ctxs| ctxs.iter().map(|_| verdicto(true)).collect())
                .await;
        });

        // Solo 1 item, muy por debajo de batch_size=10: debe resolverse por
        // el timeout, no quedarse esperando a que se llene el lote.
        let resultado =
            tokio::time::timeout(Duration::from_millis(200), batcher.submit(ctx_de_prueba())).await;
        assert!(resultado.is_ok(), "submit no debe colgarse esperando batch_size");

        batcher.close();
        tarea_run.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn una_rafaga_de_envios_no_espera_el_timeout_completo_por_cada_uno() {
        // batch_size=1 + batch_timeout LARGO: si `run` esperara notify/timeout
        // sin chequear primero si el buffer ya tiene ítems (el bug que este
        // test cubre), cada envío de la ráfaga más allá del primero tendría
        // que agotar los 10s completos antes de despacharse — con tiempo
        // pausado, esto haría que el test tardara ~50s de tiempo VIRTUAL en
        // vez de resolverse casi de inmediato.
        let batcher = OcrBatcher::new(1, Duration::from_secs(10));
        let batcher_run = batcher.clone();
        let tarea_run = tokio::spawn(async move {
            batcher_run
                .run(|ctxs| ctxs.iter().map(|_| verdicto(true)).collect())
                .await;
        });

        let inicio = tokio::time::Instant::now();
        let envios: Vec<_> = (0..5).map(|_| batcher.submit(ctx_de_prueba())).collect();
        let resultados = futures::future::join_all(envios).await;
        let transcurrido = inicio.elapsed();

        assert!(resultados.iter().all(|v| v.approved));
        assert!(
            transcurrido < Duration::from_secs(1),
            "no debería necesitar agotar el batch_timeout (10s) por cada ítem de la ráfaga; tardó {transcurrido:?}"
        );

        batcher.close();
        tarea_run.await.unwrap();
    }

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
                &df,
                &url_columns,
                pipeline,
                None,
                checkpoint,
                &cached,
                0,
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
                &df,
                &url_columns,
                pipeline,
                None,
                checkpoint,
                &HashMap::new(),
                0,
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
                &df,
                &url_columns,
                pipeline,
                None,
                checkpoint,
                &HashMap::new(),
                0,
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

    #[test]
    fn si_falla_la_inferencia_ocr_el_veredicto_es_rechazo_no_aprobacion_por_defecto() {
        // Antes de este fix, un `Err` de `run_slow` (fallo real del motor
        // ONNX, no solo de descarga) se traducía en `approved: true` por
        // defecto — el mismo bug de fail-open que ya se había corregido para
        // la inicialización del downloader, pero en otro punto de entrada.
        let error = ort::Error::new("fallo de inferencia simulado");
        let veredicto = veredicto_de_resultado_ocr(Err(error));
        assert!(!veredicto.approved);
        assert!(veredicto.reasons[0].contains("fallo de inferencia simulado"));
    }

    #[test]
    fn si_la_inferencia_ocr_no_falla_el_veredicto_se_propaga_intacto() {
        let veredicto = veredicto_de_resultado_ocr(Ok(verdicto(true)));
        assert!(veredicto.approved);
    }

    #[test]
    fn un_panic_durante_la_inferencia_se_traduce_en_rechazo_en_vez_de_propagarse() {
        // Antes de este fix, un panic dentro de `run_slow` (p. ej. un modelo
        // ONNX con una forma de tensor inesperada) tumbaba el proceso
        // completo — perdiendo el resto del lote y cualquier resultado del
        // checkpoint aún no volcado a disco. `veredicto_aislando_panic` debe
        // contener el panic y devolver un rechazo, sin propagarlo.
        let veredicto = veredicto_aislando_panic(std::panic::AssertUnwindSafe(
            || -> ort::Result<PipelineVerdict> { panic!("panic simulado de inferencia OCR") },
        ));
        assert!(!veredicto.approved);
        assert!(veredicto.reasons[0].contains("panic"));
    }

    #[test]
    fn sin_panic_veredicto_aislando_panic_se_comporta_como_veredicto_de_resultado_ocr() {
        let veredicto = veredicto_aislando_panic(std::panic::AssertUnwindSafe(|| Ok(verdicto(true))));
        assert!(veredicto.approved);

        let veredicto = veredicto_aislando_panic(std::panic::AssertUnwindSafe(|| {
            Err(ort::Error::new("fallo real, no panic"))
        }));
        assert!(!veredicto.approved);
        assert!(veredicto.reasons[0].contains("fallo real, no panic"));
    }
}
