//! D1-D2 · Detectores CPU del pipeline de rechazo de imágenes: banner de
//! color sólido y fondo no neutro en las esquinas. Corren antes que el OCR
//! (D3-D5) porque son órdenes de magnitud más baratos: medidos sobre 461
//! imágenes reales dieron 0,011 s y 0,001 s por imagen, contra 4,4 s del OCR.
//!
//! Hubo un D3 (diagrama técnico, por Transformada de Hough) que se eliminó:
//! costaba 6,5 s por imagen —el 58% del cómputo, más que el OCR entero— y no
//! detectaba nada. Aprobó las 461 imágenes del lote y también los diagramas
//! de despiece que el usuario señaló a mano. Buscaba líneas RECTAS largas, y
//! una ilustración de autopartes son contornos curvos sobre fondo blanco.
//!
//! No hay fixture externa contra la cual validar equivalencia exacta para
//! D1-D2, así que la verificación aquí es por casos sintéticos directos.

use image::{GrayImage, Luma, RgbImage};

/// Saturación y valor de HSV en convención 8-bit (0-255), a partir de un
/// pixel RGB. El matiz no se calcula: ningún detector lo usa, y da igual el
/// orden RGB/BGR porque max/min de canales es simétrico.
pub(crate) fn sat_val(rgb: [u8; 3]) -> (u8, u8) {
    let [r, g, b] = rgb;
    let maxc = r.max(g).max(b);
    let minc = r.min(g).min(b);
    let s = if maxc == 0 {
        0
    } else {
        (((maxc - minc) as f32 / maxc as f32) * 255.0).round() as u8
    };
    (s, maxc)
}

/// Resultado de un detector: `detected=true` significa "rechazar la imagen".
pub struct DetectionResult {
    pub detected: bool,
    pub reason: String,
}

impl DetectionResult {
    pub fn reject(reason: impl Into<String>) -> Self {
        Self {
            detected: true,
            reason: reason.into(),
        }
    }

    pub fn accept() -> Self {
        Self {
            detected: false,
            reason: String::new(),
        }
    }
}

/// Cache de espacios de color derivados de una imagen RGB, compartidos entre
/// D1-D2. `sat`/`val` son la saturación y el valor de HSV en convención
/// 8-bit (0-255, no 0-1); no se calcula el matiz porque ningún detector lo
/// necesita. `gray` usa deliberadamente los pesos ITU-R BT.601
/// (0.299/0.587/0.114), distintos de los BT.709 que usa
/// `image::Pixel::to_luma`.
pub struct ImageContext {
    pub width: u32,
    pub height: u32,
    /// Imagen original (color): la necesita `TextDetector` (D3-D5) para su
    /// propio resize + OCR, igual que `ImageContext.bgr` en el original.
    pub rgb: RgbImage,
    pub gray: GrayImage,
    pub sat: GrayImage,
    pub val: GrayImage,
}

impl ImageContext {
    pub fn new(rgb: &RgbImage) -> Self {
        let (width, height) = rgb.dimensions();
        let mut gray = GrayImage::new(width, height);
        let mut sat = GrayImage::new(width, height);
        let mut val = GrayImage::new(width, height);

        for (x, y, px) in rgb.enumerate_pixels() {
            let [r, g, b] = px.0;
            let luma = (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32).round();
            gray.put_pixel(x, y, Luma([luma.clamp(0.0, 255.0) as u8]));

            let (s, v) = sat_val([r, g, b]);
            sat.put_pixel(x, y, Luma([s]));
            val.put_pixel(x, y, Luma([v]));
        }

        Self {
            width,
            height,
            rgb: rgb.clone(),
            gray,
            sat,
            val,
        }
    }
}

pub struct BannerConfig {
    pub band_pct: f32,
    pub min_saturation: u8,
    pub min_pixel_pct: f32,
}

impl Default for BannerConfig {
    fn default() -> Self {
        Self {
            band_pct: 0.15,
            min_saturation: 60,
            min_pixel_pct: 0.55,
        }
    }
}

/// D1 — Franjas horizontales (superior/inferior) con alta saturación de color.
pub struct SolidBannerDetector {
    cfg: BannerConfig,
}

impl SolidBannerDetector {
    pub fn new(cfg: BannerConfig) -> Self {
        Self { cfg }
    }

    pub fn detect(&self, ctx: &ImageContext) -> DetectionResult {
        let c = &self.cfg;
        let h = ctx.height;
        let band_h = ((h as f32 * c.band_pct) as u32).max(10).min(h);

        for (name, y0, y1) in [("superior", 0, band_h), ("inferior", h - band_h, h)] {
            let mut hit = 0u32;
            let mut total = 0u32;
            for y in y0..y1 {
                for x in 0..ctx.width {
                    total += 1;
                    if ctx.sat.get_pixel(x, y).0[0] > c.min_saturation {
                        hit += 1;
                    }
                }
            }
            let pct = if total == 0 {
                0.0
            } else {
                hit as f32 / total as f32
            };
            if pct >= c.min_pixel_pct {
                return DetectionResult::reject(format!(
                    "D1·Banner de color en zona {name} ({:.0}% píxeles saturados)",
                    pct * 100.0
                ));
            }
        }
        DetectionResult::accept()
    }
}

pub struct BackgroundConfig {
    pub sample_pct: f32,
    pub min_value: u8,
    pub max_saturation: u8,
    pub tolerance_pct: f32,
}

impl Default for BackgroundConfig {
    fn default() -> Self {
        Self {
            sample_pct: 0.06,
            min_value: 130,
            max_saturation: 30,
            tolerance_pct: 0.18,
        }
    }
}

/// D2 — Esquinas con tono oscuro (V < umbral) o saturado (S > umbral).
pub struct NonNeutralBackgroundDetector {
    cfg: BackgroundConfig,
}

impl NonNeutralBackgroundDetector {
    pub fn new(cfg: BackgroundConfig) -> Self {
        Self { cfg }
    }

    pub fn detect(&self, ctx: &ImageContext) -> DetectionResult {
        let c = &self.cfg;
        let (h, w) = (ctx.height, ctx.width);
        let size = ((h.min(w) as f32 * c.sample_pct) as u32).max(20).min(h.min(w));

        let mut non_neutral = 0u32;
        let mut total = 0u32;
        let mut s_sum = 0f64;
        let mut v_sum = 0f64;

        for (cx, cy) in [(0, 0), (w - size, 0), (0, h - size), (w - size, h - size)] {
            for y in cy..cy + size {
                for x in cx..cx + size {
                    let s = ctx.sat.get_pixel(x, y).0[0];
                    let v = ctx.val.get_pixel(x, y).0[0];
                    total += 1;
                    s_sum += s as f64;
                    v_sum += v as f64;
                    if v < c.min_value || s > c.max_saturation {
                        non_neutral += 1;
                    }
                }
            }
        }

        let frac = non_neutral as f32 / total.max(1) as f32;
        if frac > c.tolerance_pct {
            return DetectionResult::reject(format!(
                "D2·Fondo no neutro: {:.1}% píxeles no neutros en esquinas (V_media={:.0}, S_media={:.0})",
                frac * 100.0,
                v_sum / total.max(1) as f64,
                s_sum / total.max(1) as f64
            ));
        }
        DetectionResult::accept()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn imagen_neutra(w: u32, h: u32) -> RgbImage {
        RgbImage::from_pixel(w, h, Rgb([180, 180, 180]))
    }

    #[test]
    fn image_context_calcula_saturacion_y_valor_hsv_8bit() {
        let mut img = RgbImage::from_pixel(4, 4, Rgb([0, 0, 0]));
        img.put_pixel(0, 0, Rgb([255, 0, 0])); // rojo puro: S=255, V=255
        let ctx = ImageContext::new(&img);
        assert_eq!(ctx.sat.get_pixel(0, 0).0[0], 255);
        assert_eq!(ctx.val.get_pixel(0, 0).0[0], 255);
        // negro: V=0, S=0 (evita division por cero)
        assert_eq!(ctx.val.get_pixel(1, 1).0[0], 0);
        assert_eq!(ctx.sat.get_pixel(1, 1).0[0], 0);
    }

    #[test]
    fn d1_acepta_imagen_neutra() {
        let ctx = ImageContext::new(&imagen_neutra(100, 100));
        let det = SolidBannerDetector::new(BannerConfig::default());
        assert!(!det.detect(&ctx).detected);
    }

    #[test]
    fn d1_rechaza_banner_saturado_en_franja_superior() {
        let mut img = imagen_neutra(100, 100);
        for y in 0..20 {
            for x in 0..100 {
                img.put_pixel(x, y, Rgb([220, 20, 20])); // rojo saturado
            }
        }
        let ctx = ImageContext::new(&img);
        let det = SolidBannerDetector::new(BannerConfig::default());
        let res = det.detect(&ctx);
        assert!(res.detected);
        assert!(res.reason.contains("D1"));
        assert!(res.reason.contains("superior"));
    }

    #[test]
    fn d2_acepta_esquinas_neutras() {
        let ctx = ImageContext::new(&imagen_neutra(100, 100));
        let det = NonNeutralBackgroundDetector::new(BackgroundConfig::default());
        assert!(!det.detect(&ctx).detected);
    }

    #[test]
    fn d2_rechaza_esquinas_oscuras() {
        let mut img = imagen_neutra(100, 100);
        for (cx, cy) in [(0, 0), (90, 0), (0, 90), (90, 90)] {
            for y in cy..cy + 10 {
                for x in cx..cx + 10 {
                    img.put_pixel(x, y, Rgb([5, 5, 5])); // casi negro: V bajo
                }
            }
        }
        let ctx = ImageContext::new(&img);
        let det = NonNeutralBackgroundDetector::new(BackgroundConfig::default());
        let res = det.detect(&ctx);
        assert!(res.detected);
        assert!(res.reason.contains("D2"));
    }
}
