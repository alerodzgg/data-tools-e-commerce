//! Orquestación de punta a punta: `Reader::readtext(rgb, detail=1,
//! paragraph=false)`, usado por `read_bgr`/`read_bgr_batch`.
//!
//! Parámetros de umbral/geometría fijados a valores por defecto estables;
//! nada en la app los sobreescribe.

use std::path::Path;

use image::{GrayImage, Luma, RgbImage};
use ndarray::Array3;

use crate::craft_utils::{adjust_result_coordinates, get_det_boxes_core, BoxPoint, TextBox};
use crate::ctc::Converter;
use crate::grouping::{group_text_box, FreeBox, HorizontalBox};
use crate::imgproc::{normalize_mean_variance, resize_aspect_ratio};
use crate::recognize_prep::{
    align_collate_normalize, bucket_width, crop_free, crop_horizontal, resize_for_recognition,
};
use crate::session::{Detector, Recognizer};

const CANVAS_SIZE: u32 = 2560;
const MAG_RATIO: f32 = 1.0;
const TEXT_THRESHOLD: f32 = 0.7;
const LINK_THRESHOLD: f32 = 0.4;
const LOW_TEXT: f32 = 0.4;
const SLOPE_THS: f32 = 0.1;
const YCENTER_THS: f32 = 0.5;
const HEIGHT_THS: f32 = 0.5;
const WIDTH_THS: f32 = 0.5;
const ADD_MARGIN: f32 = 0.1;
const MIN_SIZE: f32 = 20.0;
const MODEL_HEIGHT: u32 = 64;

/// Un fragmento de texto reconocido, con su caja (4 esquinas) y confianza.
#[derive(Debug, Clone)]
pub struct OcrResult {
    pub bbox: [[f32; 2]; 4],
    pub text: String,
    pub confidence: f32,
}

pub struct Reader {
    detector: Detector,
    recognizer: Recognizer,
    converter: Converter,
}

impl Reader {
    pub fn new(detector_path: impl AsRef<Path>, recognizer_path: impl AsRef<Path>) -> ort::Result<Self> {
        Ok(Self {
            detector: Detector::new(detector_path)?,
            recognizer: Recognizer::new(recognizer_path)?,
            converter: Converter::default(),
        })
    }

    /// `rgb`: imagen en orden RGB. La escala de grises usada para reconocer
    /// se calcula deliberadamente con la fórmula de luminancia estándar
    /// aplicada como si el array estuviera en orden BGR, lo que intercambia
    /// el peso de R y B. Es un quirk intencional de este reader, no un error
    /// de lectura: ver [`swapped_luma_gray`] para el detalle exacto.
    pub fn readtext(&mut self, rgb: &RgbImage) -> ort::Result<Vec<OcrResult>> {
        let gray = swapped_luma_gray(rgb);

        let (horizontal, free) = self.detect(rgb)?;
        let mut results = Vec::new();

        for hbox in horizontal {
            if let Some(r) = self.recognize_one(&gray, RecogBox::Horizontal(hbox))? {
                results.push(r);
            }
        }
        for quad in free {
            if let Some(r) = self.recognize_one(&gray, RecogBox::Free(quad))? {
                results.push(r);
            }
        }

        Ok(results)
    }

    fn detect(&mut self, rgb: &RgbImage) -> ort::Result<(Vec<HorizontalBox>, Vec<FreeBox>)> {
        let (canvas, ratio) = resize_aspect_ratio(rgb, CANVAS_SIZE, MAG_RATIO);
        let normalized = normalize_mean_variance(&canvas);
        let (h, w, _) = normalized.dim();
        let mut chw = Array3::<f32>::zeros((3, h, w));
        for y in 0..h {
            for x in 0..w {
                for c in 0..3 {
                    chw[[c, y, x]] = normalized[[y, x, c]];
                }
            }
        }

        let (text_score, link_score) = self.detector.forward(chw)?;
        let boxes = get_det_boxes_core(&text_score, &link_score, TEXT_THRESHOLD, LINK_THRESHOLD, LOW_TEXT);
        let ratio_w_h = 1.0 / ratio;
        let boxes = adjust_result_coordinates(&boxes, ratio_w_h, ratio_w_h, 2.0);
        // `get_textbox` castea a int32 (trunca) antes de agrupar.
        let boxes: Vec<TextBox> = boxes
            .iter()
            .map(|b| {
                b.map(|p| BoxPoint {
                    x: p.x.trunc(),
                    y: p.y.trunc(),
                })
            })
            .collect();

        let (mut horizontal, mut free) = group_text_box(
            &boxes,
            SLOPE_THS,
            YCENTER_THS,
            HEIGHT_THS,
            WIDTH_THS,
            ADD_MARGIN,
            true,
        );

        horizontal.retain(|b| (b[1] - b[0]).max(b[3] - b[2]) > MIN_SIZE);
        free.retain(|q| {
            let xs = [q[0][0], q[1][0], q[2][0], q[3][0]];
            let ys = [q[0][1], q[1][1], q[2][1], q[3][1]];
            let dx = xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max)
                - xs.iter().cloned().fold(f32::INFINITY, f32::min);
            let dy = ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max)
                - ys.iter().cloned().fold(f32::INFINITY, f32::min);
            dx.max(dy) > MIN_SIZE
        });

        Ok((horizontal, free))
    }

    fn recognize_one(&mut self, gray: &GrayImage, caja: RecogBox) -> ort::Result<Option<OcrResult>> {
        let crop = match caja {
            RecogBox::Horizontal(hbox) => crop_horizontal(gray, hbox),
            RecogBox::Free(quad) => crop_free(gray, quad),
        };

        let Some((resized, ratio)) = resize_for_recognition(&crop, MODEL_HEIGHT) else {
            return Ok(None);
        };
        let bucket = bucket_width(ratio, MODEL_HEIGHT);
        let tensor = align_collate_normalize(&resized, MODEL_HEIGHT, bucket);

        let mut probs = self.recognizer.forward(tensor)?;
        self.converter.apply_language_mask(&mut probs);
        let (text, confidence) = self.converter.decode_greedy(&probs);

        let bbox = match caja {
            RecogBox::Horizontal(h) => [[h[0], h[2]], [h[1], h[2]], [h[1], h[3]], [h[0], h[3]]],
            RecogBox::Free(q) => q,
        };

        Ok(Some(OcrResult {
            bbox,
            text,
            confidence,
        }))
    }
}

enum RecogBox {
    Horizontal(HorizontalBox),
    Free(FreeBox),
}

/// Escala de grises intencionalmente "cruzada": aplica la fórmula de
/// luminancia estándar como si el array estuviera en orden BGR aunque en
/// realidad está en RGB, lo que da `0.299*B + 0.587*G + 0.114*R` en vez de
/// la fórmula habitual `0.299*R + 0.587*G + 0.114*B`. Comportamiento
/// deliberado de este reader, no un bug.
fn swapped_luma_gray(rgb: &RgbImage) -> GrayImage {
    GrayImage::from_fn(rgb.width(), rgb.height(), |x, y| {
        let p = rgb.get_pixel(x, y);
        let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
        let v = 0.299 * b + 0.587 * g + 0.114 * r;
        Luma([v.round().clamp(0.0, 255.0) as u8])
    })
}
