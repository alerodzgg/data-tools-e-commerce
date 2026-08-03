//! Acumulación y despacho por lotes de los contextos que requieren OCR.
//!
//! El bucle de `run` y el consumo de resultados corren CONCURRENTES vía
//! `tokio::join!` en la misma función async (no como tareas separadas): así
//! `&mut Reader` —no clonable ni `Send` de forma barata— se toma prestado sin
//! envolverlo en un `Mutex` que serializaría igual toda la inferencia.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{oneshot, Mutex as AsyncMutex, Notify};

use crate::detectors::ImageContext;
use crate::pipeline::PipelineVerdict;

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
        // batch_size=1 + batch_timeout LARGO: `run` debe chequear si el
        // buffer ya tiene ítems ANTES de esperar notify/timeout. Sin eso,
        // cada envío de la ráfaga más allá del primero agotaría los 10s
        // completos y el test tardaría ~50s de tiempo virtual.
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
}
