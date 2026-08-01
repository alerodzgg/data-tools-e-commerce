//! Preprocesado de imagen para el detector CRAFT: `resize_aspect_ratio`,
//! `normalize_mean_variance`.

use image::{imageops::FilterType, RgbImage};
use ndarray::Array3;

/// Media/varianza usadas por CRAFT (ImageNet, orden RGB).
pub const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
pub const VARIANCE: [f32; 3] = [0.229, 0.224, 0.225];

/// Redimensiona el lado mayor a `max_dim` preservando la relación de aspecto
/// (sin lienzo/relleno, a diferencia de [`resize_aspect_ratio`]); `img` tal
/// cual si ya está dentro de la cota. Compartida por
/// `downloader::decode_and_resize` y `text_detector::prepare_for_ocr`.
pub fn resize_max_dim(img: &RgbImage, max_dim: u32) -> RgbImage {
    let (w, h) = img.dimensions();
    let side = w.max(h);
    if side <= max_dim {
        return img.clone();
    }
    let scale = max_dim as f32 / side as f32;
    let new_w = ((w as f32 * scale) as u32).max(1);
    let new_h = ((h as f32 * scale) as u32).max(1);
    image::imageops::resize(img, new_w, new_h, FilterType::Triangle)
}

/// Redimensiona manteniendo la relación de aspecto y rellena a un lienzo
/// múltiplo de 32 (requisito del backbone de CRAFT). Devuelve el lienzo
/// (con el resto en negro) y el `ratio` aplicado (necesario para reescalar
/// las cajas detectadas de vuelta a coordenadas de la imagen original).
pub fn resize_aspect_ratio(img: &RgbImage, square_size: u32, mag_ratio: f32) -> (RgbImage, f32) {
    let (width, height) = img.dimensions();
    let max_side = height.max(width) as f32;

    let target_size = (mag_ratio * max_side).min(square_size as f32);
    let ratio = target_size / max_side;

    let target_h = (height as f32 * ratio) as u32;
    let target_w = (width as f32 * ratio) as u32;
    let proc = image::imageops::resize(img, target_w, target_h, FilterType::Triangle);

    let target_h32 = if target_h % 32 != 0 {
        target_h + (32 - target_h % 32)
    } else {
        target_h
    };
    let target_w32 = if target_w % 32 != 0 {
        target_w + (32 - target_w % 32)
    } else {
        target_w
    };

    let mut canvas = RgbImage::new(target_w32, target_h32);
    image::imageops::replace(&mut canvas, &proc, 0, 0);

    (canvas, ratio)
}

/// Normaliza una imagen RGB u8 a `(pixel - mean*255) / (var*255)`, devuelta
/// como array HWC f32 (listo para transponer a CHW antes de la inferencia).
pub fn normalize_mean_variance(img: &RgbImage) -> Array3<f32> {
    let (w, h) = img.dimensions();
    let mut out = Array3::<f32>::zeros((h as usize, w as usize, 3));
    for y in 0..h {
        for x in 0..w {
            let p = img.get_pixel(x, y);
            for c in 0..3 {
                out[[y as usize, x as usize, c]] = (p[c] as f32 - MEAN[c] * 255.0) / (VARIANCE[c] * 255.0);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_aspect_ratio_respeta_el_tope_y_rellena_a_multiplo_de_32() {
        let img = RgbImage::new(1000, 500);
        let (canvas, ratio) = resize_aspect_ratio(&img, 2560, 1.0);
        // target_size = min(1.0*1000, 2560) = 1000 → ratio = 1000/1000 = 1.0
        assert_eq!(ratio, 1.0);
        // target_h=500 → pad a 512; target_w=1000 → pad a 1024
        assert_eq!(canvas.dimensions(), (1024, 512));
    }

    #[test]
    fn resize_aspect_ratio_reduce_cuando_excede_canvas_size() {
        let img = RgbImage::new(4000, 2000);
        let (canvas, ratio) = resize_aspect_ratio(&img, 2560, 1.0);
        // target_size = min(4000, 2560) = 2560 → ratio = 2560/4000 = 0.64
        assert!((ratio - 0.64).abs() < 1e-6);
        // target_h = 2000*0.64 = 1280 (ya múltiplo de 32); target_w = 4000*0.64=2560
        assert_eq!(canvas.dimensions(), (2560, 1280));
    }

    #[test]
    fn normalize_mean_variance_valor_conocido() {
        let mut img = RgbImage::new(1, 1);
        img.put_pixel(0, 0, image::Rgb([255, 0, 128]));
        let out = normalize_mean_variance(&img);
        let esperado_r = (255.0 - MEAN[0] * 255.0) / (VARIANCE[0] * 255.0);
        let esperado_g = (0.0 - MEAN[1] * 255.0) / (VARIANCE[1] * 255.0);
        let esperado_b = (128.0 - MEAN[2] * 255.0) / (VARIANCE[2] * 255.0);
        assert!((out[[0, 0, 0]] - esperado_r).abs() < 1e-5);
        assert!((out[[0, 0, 1]] - esperado_g).abs() < 1e-5);
        assert!((out[[0, 0, 2]] - esperado_b).abs() < 1e-5);
    }
}
