//! D4+D5+D6 · Clasificación del texto detectado por OCR en un solo pase:
//!   D4 — texto comercial   (palabras sueltas + frases multi-palabra)
//!   D5 — logo en esquina   (texto de color dentro de las esquinas)
//!   D6 — imagen placeholder ("coming soon", "no image"…)
//!
//! `TextDetector::analyze_ocr_results`. El pre-filtro morfológico de
//! candidatos a texto no está portado a propósito: en el original Python
//! vivía permanentemente deshabilitado por config (dejaba pasar texto
//! diagonal, así que nunca se ejecutaba en producción) — mismo criterio que
//! la exclusión de la variante de polígonos en el resto de este crate.

use std::collections::HashSet;

use image::RgbImage;
use regex::{Regex, RegexBuilder};

use crate::detectors::{sat_val, DetectionResult, ImageContext};
use crate::reader::{OcrResult, Reader};

pub struct TextDetectorConfig {
    pub min_confidence: f32,
    pub min_text_length: usize,
    pub corner_radius_pct: f32,
    pub ignore_patterns: Vec<String>,
    pub commercial_words: Vec<String>,
    pub placeholder_phrases: Vec<String>,
    pub watermark_sat_max: u8,
    pub ocr_max_dim: u32,
}

impl Default for TextDetectorConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.40,
            min_text_length: 3,
            corner_radius_pct: 0.22,
            ignore_patterns: vec![
                r"^\d{1,12}$".to_string(),
                r"^[A-Z]{1,4}-?\d{1,6}$".to_string(),
                r"^[A-Z]{1,3}\d{1,4}[A-Z]?$".to_string(),
            ],
            commercial_words: [
                "free",
                "shipping",
                "warranty",
                "guarantee",
                "sale",
                "discount",
                "stock",
                "delivery",
                "offer",
                "quality",
                "new",
                "brand",
                "seller",
                "envío",
                "garantía",
                "gratis",
                "oferta",
                "nuevo",
                "venta",
                "calidad",
                "worry",
                "returns",
                "satisfaction",
                "postal",
                "service",
                "fast",
                "2pcs",
                "3pcs",
                "4pcs",
                "pcs",
                "kit",
                "set",
                "pack",
                "combo",
                "hot",
                "best",
                "buy",
                "shop",
                "store",
                "parts",
                "auto",
                "coming soon",
                "thank you",
                "business",
                "appreciate",
                "reliable",
                "aftermarket",
                "diagram",
                "assembly",
                "exploded",
                "view",
                "package list",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            placeholder_phrases: [
                "coming soon",
                "image coming",
                "no image",
                "not available",
                "photo coming",
                "image not available",
                "no photo",
                "awaiting image",
                "sin imagen",
                "imagen no disponible",
                "proximamente",
                "próximamente",
                "foto no disponible",
                "no disponible",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            watermark_sat_max: 25,
            ocr_max_dim: 900,
        }
    }
}

pub struct TextDetector {
    cfg: TextDetectorConfig,
    ignore_patterns: Vec<Regex>,
    commercial_single: HashSet<String>,
    commercial_phrases: Vec<String>,
}

impl TextDetector {
    pub fn new(cfg: TextDetectorConfig) -> Self {
        let ignore_patterns = cfg
            .ignore_patterns
            .iter()
            .map(|p| {
                // `ignore_patterns` solo se construye hoy vía el `Default`
                // de `TextDetectorConfig` (patrones literales fijos, no
                // datos de usuario/archivo) — siempre compilan.
                #[allow(clippy::expect_used)]
                RegexBuilder::new(p)
                    .case_insensitive(true)
                    .build()
                    .expect("patron ignore_patterns invalido")
            })
            .collect();
        let commercial_single = cfg
            .commercial_words
            .iter()
            .filter(|w| !w.contains(' '))
            .cloned()
            .collect();
        let commercial_phrases = cfg
            .commercial_words
            .iter()
            .filter(|w| w.contains(' '))
            .cloned()
            .collect();
        Self {
            cfg,
            ignore_patterns,
            commercial_single,
            commercial_phrases,
        }
    }

    /// Vía single-image: redimensiona a `ocr_max_dim`, corre OCR y clasifica.
    pub fn detect(&self, ctx: &ImageContext, reader: &mut Reader) -> ort::Result<DetectionResult> {
        let prepared = self.prepare_for_ocr(&ctx.rgb);
        let results = reader.readtext(&prepared)?;
        Ok(self.analyze_ocr_results(&prepared, &results))
    }

    fn prepare_for_ocr(&self, rgb: &RgbImage) -> RgbImage {
        crate::imgproc::resize_max_dim(rgb, self.cfg.ocr_max_dim)
    }

    /// Lógica de clasificación pura: dada la imagen ya preparada para OCR y
    /// sus resultados, decide D4/D5/D6. Separado de `detect` para poder
    /// testear la clasificación sin depender de inferencia real.
    pub fn analyze_ocr_results(&self, prepared: &RgbImage, ocr_results: &[OcrResult]) -> DetectionResult {
        let (w, h) = prepared.dimensions();
        let corner_radius = (w.min(h) as f32 * self.cfg.corner_radius_pct) as f64;
        let corner_centers = [
            (0f64, 0f64),
            (w as f64, 0.0),
            (0.0, h as f64),
            (w as f64, h as f64),
        ];
        let mut reasons: Vec<String> = Vec::new();

        let valid: Vec<(&OcrResult, String)> = ocr_results
            .iter()
            .map(|r| (r, r.text.trim().to_string()))
            .filter(|(r, t)| {
                r.confidence >= self.cfg.min_confidence && t.chars().count() >= self.cfg.min_text_length
            })
            .collect();

        let joined = valid
            .iter()
            .map(|(_, t)| t.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        let joined_words: HashSet<&str> = joined.split_whitespace().collect();

        // D6 · imagen placeholder: coincidencia por subconjunto de palabras
        // (orden-independiente, ya que el reader puede devolver fragmentos
        // desordenados).
        if let Some(ph) = self
            .cfg
            .placeholder_phrases
            .iter()
            .find(|p| p.split_whitespace().all(|w| joined_words.contains(w)))
        {
            return DetectionResult::reject(format!("D6·Imagen placeholder: contiene '{ph}'"));
        }

        // D4 · frase comercial multi-palabra (substring sobre el texto unido).
        if let Some(cphrase) = self
            .commercial_phrases
            .iter()
            .find(|p| joined.contains(p.as_str()))
        {
            reasons.push(format!("D4·Texto comercial (frase): '{cphrase}'"));
        }

        for (r, text) in &valid {
            if self.ignore_patterns.iter().any(|re| re.is_match(text)) {
                continue;
            }

            let text_lower = text.to_lowercase();
            let words: HashSet<&str> = text_lower.split_whitespace().collect();
            let hits: Vec<&str> = words
                .iter()
                .filter(|w| self.commercial_single.contains(**w))
                .copied()
                .collect();

            if !hits.is_empty() {
                reasons.push(format!(
                    "D4·Texto comercial: '{text}' (palabras: {}, conf={:.2})",
                    hits.join(", "),
                    r.confidence
                ));
                continue;
            }

            // D5 · logo/texto de color situado dentro de una esquina.
            let cx = r.bbox.iter().map(|p| p[0]).sum::<f32>() / 4.0;
            let cy = r.bbox.iter().map(|p| p[1]).sum::<f32>() / 4.0;
            let in_corner = corner_centers.iter().any(|(ex, ey)| {
                let dx = cx as f64 - ex;
                let dy = cy as f64 - ey;
                dx * dx + dy * dy <= corner_radius * corner_radius
            });
            if in_corner && self.is_colored_watermark(prepared, &r.bbox) {
                reasons.push(format!(
                    "D5·Logo/texto en esquina: '{text}' (conf={:.2})",
                    r.confidence
                ));
                continue;
            }
        }

        if !reasons.is_empty() {
            reasons.truncate(5);
            return DetectionResult::reject(reasons.join(" | "));
        }
        DetectionResult::accept()
    }

    fn is_colored_watermark(&self, rgb: &RgbImage, bbox: &[[f32; 2]; 4]) -> bool {
        let (w, h) = rgb.dimensions();
        let xs = bbox.iter().map(|p| p[0]);
        let ys = bbox.iter().map(|p| p[1]);
        let x1 = xs.clone().fold(f32::INFINITY, f32::min).max(0.0) as u32;
        let y1 = ys.clone().fold(f32::INFINITY, f32::min).max(0.0) as u32;
        let x2 = (xs.fold(f32::NEG_INFINITY, f32::max) as u32).min(w);
        let y2 = (ys.fold(f32::NEG_INFINITY, f32::max) as u32).min(h);
        if x2 <= x1 || y2 <= y1 {
            return false;
        }

        let mut sum_s = 0f64;
        let mut count = 0u32;
        for y in y1..y2 {
            for x in x1..x2 {
                let px = rgb.get_pixel(x, y).0;
                let (s, _v) = sat_val(px);
                sum_s += s as f64;
                count += 1;
            }
        }
        let mean_s = if count == 0 { 0.0 } else { sum_s / count as f64 };
        mean_s > self.cfg.watermark_sat_max as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn ocr(bbox: [[f32; 2]; 4], text: &str, confidence: f32) -> OcrResult {
        OcrResult {
            bbox,
            text: text.to_string(),
            confidence,
        }
    }

    fn lienzo(w: u32, h: u32) -> RgbImage {
        RgbImage::from_pixel(w, h, Rgb([200, 200, 200]))
    }

    #[test]
    fn descarta_fragmentos_de_baja_confianza_o_muy_cortos() {
        let det = TextDetector::new(TextDetectorConfig::default());
        let results = vec![
            ocr([[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]], "ab", 0.99),
            ocr([[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]], "abc", 0.10),
        ];
        let res = det.analyze_ocr_results(&lienzo(200, 200), &results);
        assert!(!res.detected);
    }

    #[test]
    fn d6_detecta_placeholder_aunque_los_fragmentos_esten_desordenados() {
        let det = TextDetector::new(TextDetectorConfig::default());
        let results = vec![
            ocr(
                [[50.0, 50.0], [90.0, 50.0], [90.0, 70.0], [50.0, 70.0]],
                "SOON",
                0.9,
            ),
            ocr(
                [[10.0, 10.0], [40.0, 10.0], [40.0, 30.0], [10.0, 30.0]],
                "COMING",
                0.9,
            ),
        ];
        let res = det.analyze_ocr_results(&lienzo(200, 200), &results);
        assert!(res.detected);
        assert!(res.reason.contains("D6"));
    }

    #[test]
    fn d4_detecta_palabra_comercial_suelta() {
        let det = TextDetector::new(TextDetectorConfig::default());
        let results = vec![ocr(
            [[10.0, 10.0], [80.0, 10.0], [80.0, 30.0], [10.0, 30.0]],
            "FREE SHIPPING",
            0.9,
        )];
        let res = det.analyze_ocr_results(&lienzo(200, 200), &results);
        assert!(res.detected);
        assert!(res.reason.contains("D4"));
    }

    #[test]
    fn d4_no_dispara_con_palabra_que_solo_contiene_la_comercial_como_substring() {
        // "renew" no debe disparar por contener "new": el split es por palabra
        // completa, no por substring (a diferencia de las frases D4).
        let det = TextDetector::new(TextDetectorConfig::default());
        let results = vec![ocr(
            [[10.0, 10.0], [80.0, 10.0], [80.0, 30.0], [10.0, 30.0]],
            "renew today",
            0.9,
        )];
        let res = det.analyze_ocr_results(&lienzo(200, 200), &results);
        assert!(!res.detected);
    }

    #[test]
    fn ignora_texto_que_matchea_un_patron_de_codigo_de_parte() {
        let det = TextDetector::new(TextDetectorConfig::default());
        let results = vec![ocr(
            [[10.0, 10.0], [80.0, 10.0], [80.0, 30.0], [10.0, 30.0]],
            "AB-12345",
            0.9,
        )];
        let res = det.analyze_ocr_results(&lienzo(200, 200), &results);
        assert!(!res.detected);
    }

    #[test]
    fn d5_detecta_logo_de_color_en_esquina() {
        let det = TextDetector::new(TextDetectorConfig::default());
        let mut img = lienzo(200, 200);
        // Mancha saturada en la esquina superior-izquierda, bajo el radio D5.
        for y in 0..20 {
            for x in 0..20 {
                img.put_pixel(x, y, Rgb([220, 30, 30]));
            }
        }
        let results = vec![ocr(
            [[2.0, 2.0], [18.0, 2.0], [18.0, 18.0], [2.0, 18.0]],
            "acme",
            0.9,
        )];
        let res = det.analyze_ocr_results(&img, &results);
        assert!(res.detected);
        assert!(res.reason.contains("D5"));
    }

    #[test]
    fn d5_no_dispara_si_el_texto_en_la_esquina_no_es_de_color() {
        let det = TextDetector::new(TextDetectorConfig::default());
        let img = lienzo(200, 200); // gris uniforme: saturacion 0 en toda la imagen
        let results = vec![ocr(
            [[2.0, 2.0], [18.0, 2.0], [18.0, 18.0], [2.0, 18.0]],
            "acme",
            0.9,
        )];
        let res = det.analyze_ocr_results(&img, &results);
        assert!(!res.detected);
    }
}
