//! Orquestador de detectores: encadena D1-D3 (CPU, cortan en el primer
//! rechazo) y luego D4-D6 (OCR) solo si hace falta. `ImagePipeline` expone
//! la vía síncrona de una sola imagen; el batching/paralelismo async vive
//! aparte, en `async_processor::AsyncBatchProcessor` (que llama a
//! `run_fast`/`run_slow` de acá por cada imagen del lote).
//!
//! `run_fast` no envuelve cada detector en manejo de errores individual:
//! D1-D3 son funciones puras sobre buffers ya decodificados y no tienen
//! ninguna vía de fallo en tiempo de ejecución.

use crate::detectors::{
    BackgroundConfig, BannerConfig, ImageContext, NonNeutralBackgroundDetector, SolidBannerDetector,
};
use crate::reader::Reader;
use crate::text_detector::{TextDetector, TextDetectorConfig};

#[derive(Default)]
pub struct PipelineConfig {
    pub banner: BannerConfig,
    pub background: BackgroundConfig,
    pub text: TextDetectorConfig,
}

/// Qué detectores están activos (equivalente al dict `enabled` que arma la
/// CLI a partir de flags; todos activos por defecto).
pub struct DetectorToggles {
    pub d1: bool,
    pub d2: bool,
    pub d4_d5_d6: bool,
}

impl Default for DetectorToggles {
    fn default() -> Self {
        Self {
            d1: true,
            d2: true,
            d4_d5_d6: true,
        }
    }
}

pub struct PipelineVerdict {
    pub approved: bool,
    pub reasons: Vec<String>,
}

pub struct ImagePipeline {
    d1: Option<SolidBannerDetector>,
    d2: Option<NonNeutralBackgroundDetector>,
    slow: Option<TextDetector>,
}

impl ImagePipeline {
    pub fn from_config(cfg: PipelineConfig, toggles: DetectorToggles) -> Self {
        let PipelineConfig {
            banner,
            background,
            text,
        } = cfg;
        Self {
            d1: toggles.d1.then(|| SolidBannerDetector::new(banner)),
            d2: toggles.d2.then(|| NonNeutralBackgroundDetector::new(background)),
            slow: toggles.d4_d5_d6.then(|| TextDetector::new(text)),
        }
    }

    pub fn has_slow_stage(&self) -> bool {
        self.slow.is_some()
    }

    /// Etapa CPU. `Some(veredicto)` es definitivo (rechazo, o aprobación sin
    /// etapa lenta configurada); `None` significa "pasó, seguir con OCR".
    pub fn run_fast(&self, ctx: &ImageContext) -> Option<PipelineVerdict> {
        if let Some(d1) = &self.d1 {
            let r = d1.detect(ctx);
            if r.detected {
                return Some(PipelineVerdict {
                    approved: false,
                    reasons: vec![r.reason],
                });
            }
        }
        if let Some(d2) = &self.d2 {
            let r = d2.detect(ctx);
            if r.detected {
                return Some(PipelineVerdict {
                    approved: false,
                    reasons: vec![r.reason],
                });
            }
        }

        if self.slow.is_none() {
            return Some(PipelineVerdict {
                approved: true,
                reasons: Vec::new(),
            });
        }
        None
    }

    /// Etapa OCR. Solo debe llamarse cuando `run_fast` devolvió `None`. Hoy el
    /// único caller (`async_processor`) respeta esa invariante, pero
    /// `run_slow` es `pub` (este crate es una librería): un llamador nuevo
    /// que la viole no debe panickear, sino recibir un `Err` — mismo
    /// criterio que `salida_output` en `session.rs` ya aplica para "modelo
    /// con forma de salida inesperada".
    pub fn run_slow(&self, ctx: &ImageContext, reader: &mut Reader) -> ort::Result<PipelineVerdict> {
        let Some(slow) = self.slow.as_ref() else {
            return Err(ort::Error::new(
                "run_slow llamado sin etapa lenta (OCR) configurada en este pipeline",
            ));
        };
        let res = slow.detect(ctx, reader)?;
        Ok(if res.detected {
            PipelineVerdict {
                approved: false,
                reasons: vec![res.reason],
            }
        } else {
            PipelineVerdict {
                approved: true,
                reasons: Vec::new(),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn imagen_neutra(w: u32, h: u32) -> RgbImage {
        RgbImage::from_pixel(w, h, Rgb([180, 180, 180]))
    }

    #[test]
    fn run_fast_rechaza_en_d1_sin_llegar_a_d2() {
        let mut img = imagen_neutra(100, 100);
        for y in 0..20 {
            for x in 0..100 {
                img.put_pixel(x, y, Rgb([220, 20, 20]));
            }
        }
        let ctx = ImageContext::new(&img);
        let pipeline = ImagePipeline::from_config(PipelineConfig::default(), DetectorToggles::default());
        let verdict = pipeline.run_fast(&ctx).expect("debe ser definitivo");
        assert!(!verdict.approved);
        assert!(verdict.reasons[0].contains("D1"));
    }

    #[test]
    fn run_fast_aprueba_sin_ocr_si_la_etapa_lenta_esta_deshabilitada() {
        let ctx = ImageContext::new(&imagen_neutra(100, 100));
        let toggles = DetectorToggles {
            d4_d5_d6: false,
            ..DetectorToggles::default()
        };
        let pipeline = ImagePipeline::from_config(PipelineConfig::default(), toggles);
        let verdict = pipeline
            .run_fast(&ctx)
            .expect("sin etapa lenta debe resolver aqui");
        assert!(verdict.approved);
    }

    #[test]
    fn run_fast_devuelve_none_cuando_pasa_cpu_y_hay_etapa_lenta_pendiente() {
        let ctx = ImageContext::new(&imagen_neutra(100, 100));
        let pipeline = ImagePipeline::from_config(PipelineConfig::default(), DetectorToggles::default());
        assert!(pipeline.run_fast(&ctx).is_none());
        assert!(pipeline.has_slow_stage());
    }

    #[test]
    fn detectores_deshabilitados_no_se_construyen() {
        let toggles = DetectorToggles {
            d1: false,
            d2: false,
            d4_d5_d6: false,
        };
        let pipeline = ImagePipeline::from_config(PipelineConfig::default(), toggles);
        assert!(!pipeline.has_slow_stage());
        // Sin ningun detector activo, la etapa rapida aprueba de inmediato.
        let ctx = ImageContext::new(&imagen_neutra(50, 50));
        let verdict = pipeline.run_fast(&ctx).unwrap();
        assert!(verdict.approved);
    }

    #[test]
    fn run_slow_devuelve_error_en_vez_de_panic_si_no_hay_etapa_lenta_configurada() {
        // `run_slow` devuelve `ort::Result`: violar su invariante (llamarlo
        // sin etapa lenta) debe dar `Err`, no tumbar el proceso.
        use std::path::PathBuf;

        let toggles = DetectorToggles {
            d4_d5_d6: false,
            ..DetectorToggles::default()
        };
        let pipeline = ImagePipeline::from_config(PipelineConfig::default(), toggles);
        assert!(!pipeline.has_slow_stage());

        let modelo = |nombre: &str| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("models")
                .join(nombre)
        };
        let mut reader =
            crate::reader::Reader::new(modelo("craft_detector.onnx"), modelo("recognizer_latin_g2.onnx"))
                .expect("carga los modelos empaquetados del repo");

        let ctx = ImageContext::new(&imagen_neutra(50, 50));
        let resultado = pipeline.run_slow(&ctx, &mut reader);
        assert!(resultado.is_err(), "debe ser Err, no panickear");
    }
}
