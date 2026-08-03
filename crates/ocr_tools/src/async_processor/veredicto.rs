//! Cómo se convierte cada resultado (o fallo) en un veredicto por imagen.
//!
//! Regla común a todas las rutas: una imagen que NUNCA llegó a evaluarse se
//! rechaza con el motivo real, no se aprueba por defecto. Aprobarla dejaría
//! pasar en silencio justo lo que el análisis existe para filtrar.

use std::collections::HashMap;
use std::sync::Arc;

use crate::batch::{CellResult, CellTask};
use crate::checkpoint_store::CheckpointEntry;
use crate::detectors::ImageContext;
use crate::downloader::{decode_and_resize, FalloDescarga};
use crate::pipeline::{ImagePipeline, PipelineVerdict};

/// Veredicto de rechazo con un motivo concreto. Las tres causas —fallo de
/// descarga, imagen ilegible, y panic interno— se distinguen en el reporte:
/// un texto único para las tres apuntaría al dato del usuario incluso cuando
/// el problema es del servidor remoto o del propio programa.
fn rechazo(motivo: String) -> (Option<PipelineVerdict>, Option<ImageContext>) {
    (
        Some(PipelineVerdict {
            approved: false,
            reasons: vec![motivo],
        }),
        None,
    )
}

/// Decode + detectores CPU en un solo salto a un thread de bloqueo (igual
/// que `_decode_and_run_fast`: decodificar y correr D1-D3 juntos evita un
/// segundo round-trip y deja el runtime async libre para I/O de red).
pub(super) async fn decode_and_run_fast(
    pipeline: Arc<ImagePipeline>,
    resize_max_dim: Option<u32>,
    content: Result<Vec<u8>, FalloDescarga>,
) -> (Option<PipelineVerdict>, Option<ImageContext>) {
    let content = match content {
        Ok(bytes) => bytes,
        Err(fallo) => return rechazo(format!("No se pudo descargar: {fallo}")),
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
        // Descargó bien, pero los bytes no son una imagen decodificable (o
        // exceden los límites anti-bomba de `decode_and_resize`): es un
        // problema del CONTENIDO, no de la descarga.
        Ok(None) => rechazo("Descargada, pero no es una imagen válida o supera los límites".to_string()),
        // Panic dentro de `spawn_blocking`: un bug de este programa, no del
        // dato ni de la red. Se reporta aparte para no mandar a revisar la
        // URL o la conexión.
        Err(_join_error) => rechazo("Fallo interno al procesar la imagen".to_string()),
    }
}

/// Marca todas las `tasks` como rechazadas con `motivo` (en `results`, para
/// que `materialize` las vuelque, y devuelve las `CheckpointEntry` para
/// persistirlas) — usado por los dos casos de "no se puede evaluar esta
/// tarea por una falla de infraestructura, no de los datos": no deben quedar
/// aprobadas por el default de `materialize` (correcto solo para celdas que
/// nunca fueron tareas), sino explícitamente rechazadas con un motivo real.
pub(super) fn rechazar_pendientes(
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

/// Traduce el resultado de `run_slow` a un veredicto. Un fallo de inferencia
/// OCR es tan "no evaluado" como uno de descarga: aprobar por defecto
/// dejaría pasar en silencio imágenes que nunca llegaron a analizarse.
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
pub(super) fn veredicto_aislando_panic(
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

    #[test]
    fn si_falla_la_inferencia_ocr_el_veredicto_es_rechazo_no_aprobacion_por_defecto() {
        // Un `Err` de `run_slow` (fallo real del motor ONNX) no puede
        // traducirse en `approved: true`: una imagen que nunca se evaluó
        // debe quedar rechazada, no colarse por defecto.
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
        // Un panic dentro de `run_slow` (p. ej. un modelo ONNX con forma de
        // tensor inesperada) tumbaría el proceso y perdería el resto del lote
        // más lo aún no volcado al checkpoint: debe contenerse y devolver un
        // rechazo.
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
