//! Recorte y preprocesado de cada caja de texto antes del reconocedor:
//! `get_image_list` (+ `four_point_transform`, `calculate_ratio`,
//! `compute_ratio_and_resize`) y `AlignCollate`/`NormalizePAD`.
//!
//! Nota de diseño: el primer resize usa deliberadamente interpolación
//! bilineal (`FilterType::Triangle`), no Lanczos, para que el resultado
//! coincida con el comportamiento esperado del pipeline de reconocimiento.

use image::{imageops::FilterType, GrayImage, Luma};
use imageproc::geometric_transformations::{warp_into, Interpolation, Projection};
use ndarray::Array3;

use crate::grouping::{FreeBox, HorizontalBox};

/// Aspect ratio siempre >= 1 (invierte si el recorte es más alto que ancho).
pub fn calculate_ratio(width: f32, height: f32) -> f32 {
    let r = width / height;
    if r < 1.0 {
        1.0 / r
    } else {
        r
    }
}

/// Ancho del "balde" de cuantización para el reconocedor: múltiplo de
/// `model_height` que acomoda el aspect ratio del recorte (con margen de
/// `ceil`, luego rellenado por `align_collate_normalize`).
pub fn bucket_width(ratio: f32, model_height: u32) -> u32 {
    (ratio.max(1.0).ceil() as u32).max(1) * model_height
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// Recorte rectangular simple (cajas casi-horizontales), acotado a los
/// límites de la imagen.
pub fn crop_horizontal(img: &GrayImage, hbox: HorizontalBox) -> GrayImage {
    let (w, h) = img.dimensions();
    let x_min = hbox[0].max(0.0).round() as u32;
    let x_max = hbox[1].min(w as f32).round().max(x_min as f32) as u32;
    let y_min = hbox[2].max(0.0).round() as u32;
    let y_max = hbox[3].min(h as f32).round().max(y_min as f32) as u32;
    image::imageops::crop_imm(img, x_min, y_min, (x_max - x_min).max(1), (y_max - y_min).max(1)).to_image()
}

/// Recorte por transformación de perspectiva (cajas inclinadas), equivalente
/// a `four_point_transform`: `quad` = [top-left, top-right, bottom-right, bottom-left].
pub fn crop_free(img: &GrayImage, quad: FreeBox) -> GrayImage {
    let [tl, tr, br, bl] = quad;
    let max_width = (dist(br, bl) as u32).max(dist(tr, tl) as u32).max(1);
    let max_height = (dist(tr, br) as u32).max(dist(tl, bl) as u32).max(1);

    let from = [(tl[0], tl[1]), (tr[0], tr[1]), (br[0], br[1]), (bl[0], bl[1])];
    let to = [
        (0.0, 0.0),
        ((max_width - 1) as f32, 0.0),
        ((max_width - 1) as f32, (max_height - 1) as f32),
        (0.0, (max_height - 1) as f32),
    ];

    let mut out = GrayImage::new(max_width, max_height);
    if let Some(proj) = Projection::from_control_points(from, to) {
        warp_into(img, &proj, Interpolation::Bilinear, Luma([0u8]), &mut out);
    }
    out
}

/// Primer resize (`compute_ratio_and_resize`): preserva aspecto, altura fija
/// `model_height`. Devuelve `None` si la caja es demasiado angosta/corta
/// (mismo guardia que `get_image_list`: `new_width == 0` → se descarta).
pub fn resize_for_recognition(crop: &GrayImage, model_height: u32) -> Option<(GrayImage, f32)> {
    let (w, h) = crop.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let guard_ratio = calculate_ratio(w as f32, h as f32);
    if ((model_height as f32 * guard_ratio) as u32) == 0 {
        return None;
    }

    let raw_ratio = w as f32 / h as f32;
    if raw_ratio < 1.0 {
        let ratio = 1.0 / raw_ratio;
        let new_h = ((model_height as f32 * ratio) as u32).max(1);
        let resized = image::imageops::resize(crop, model_height.max(1), new_h, FilterType::Triangle);
        Some((resized, ratio))
    } else {
        let new_w = ((model_height as f32 * raw_ratio) as u32).max(1);
        let resized = image::imageops::resize(crop, new_w, model_height.max(1), FilterType::Triangle);
        Some((resized, raw_ratio))
    }
}

/// `AlignCollate` + `NormalizePAD`: segundo resize (bicúbico, el que PIL sí
/// aplica de verdad) a lo sumo hasta `bucket_w`, normaliza a `[-1,1]` y
/// rellena el resto del ancho repitiendo la última columna.
pub fn align_collate_normalize(resized: &GrayImage, model_height: u32, bucket_w: u32) -> Array3<f32> {
    let (w1, h1) = resized.dimensions();
    let ratio = w1 as f32 / h1.max(1) as f32;
    let mut resized_w = (model_height as f32 * ratio).ceil() as u32;
    if resized_w > bucket_w {
        resized_w = bucket_w;
    }
    resized_w = resized_w.max(1);
    let step2 = image::imageops::resize(resized, resized_w, model_height, FilterType::CatmullRom);

    let mut out = Array3::<f32>::zeros((1, model_height as usize, bucket_w as usize));
    for y in 0..model_height as usize {
        for x in 0..resized_w as usize {
            let v = step2.get_pixel(x as u32, y as u32).0[0] as f32 / 255.0;
            out[[0, y, x]] = (v - 0.5) / 0.5;
        }
        if resized_w < bucket_w {
            let last = out[[0, y, (resized_w - 1) as usize]];
            for x in resized_w as usize..bucket_w as usize {
                out[[0, y, x]] = last;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imagen_prueba(w: u32, h: u32) -> GrayImage {
        GrayImage::from_fn(w, h, |x, _y| Luma([((x * 255) / w.max(1)) as u8]))
    }

    #[test]
    fn calculate_ratio_invierte_si_es_menor_a_uno() {
        assert_eq!(calculate_ratio(100.0, 50.0), 2.0);
        assert_eq!(calculate_ratio(50.0, 100.0), 2.0);
    }

    #[test]
    fn bucket_width_cuantiza_a_multiplos_de_model_height() {
        assert_eq!(bucket_width(2.3, 64), 3 * 64); // ceil(2.3)=3
        assert_eq!(bucket_width(0.5, 64), 64); // max(0.5,1)=1
    }

    #[test]
    fn crop_horizontal_recorta_y_clampa_a_bordes() {
        let img = imagen_prueba(100, 50);
        let recorte = crop_horizontal(&img, [-10.0, 60.0, 5.0, 200.0]);
        assert_eq!(recorte.dimensions(), (60, 45));
    }

    #[test]
    fn resize_for_recognition_preserva_altura_64() {
        let img = imagen_prueba(200, 40);
        let (resized, ratio) = resize_for_recognition(&img, 64).expect("no deberia saltarse");
        assert_eq!(resized.dimensions().1, 64);
        assert!((ratio - 5.0).abs() < 1e-4);
        assert_eq!(resized.dimensions().0, (64.0 * 5.0) as u32);
    }

    #[test]
    fn align_collate_normalize_rellena_y_normaliza() {
        let img = imagen_prueba(200, 40); // ratio 5 -> bucket = ceil(5)*64 = 320
        let (resized, ratio) = resize_for_recognition(&img, 64).unwrap();
        let bucket = bucket_width(ratio, 64);
        assert_eq!(bucket, 320);
        let tensor = align_collate_normalize(&resized, 64, bucket);
        assert_eq!(tensor.dim(), (1, 64, 320));
        // normalizado a [-1,1]
        for v in tensor.iter() {
            assert!(*v >= -1.0001 && *v <= 1.0001);
        }
        // la columna de relleno replica la ultima columna real
        let resized_w = resized.dimensions().0.min(bucket) as usize;
        if resized_w < 320 {
            assert_eq!(tensor[[0, 0, resized_w]], tensor[[0, 0, resized_w - 1]]);
        }
    }

    #[test]
    fn crop_free_sobre_un_cuadrilatero_axis_aligned_equivale_a_un_recorte_normal() {
        let img = imagen_prueba(100, 50);
        let quad = [[10.0, 10.0], [50.0, 10.0], [50.0, 30.0], [10.0, 30.0]];
        let recorte = crop_free(&img, quad);
        assert_eq!(recorte.dimensions(), (40, 20));
        let directo = crop_horizontal(&img, [10.0, 50.0, 10.0, 30.0]);
        // deben coincidir aprox. pixel a pixel (misma region, sin rotacion)
        for y in 0..20 {
            for x in 0..40 {
                let a = recorte.get_pixel(x, y).0[0] as i32;
                let b = directo.get_pixel(x, y).0[0] as i32;
                assert!((a - b).abs() <= 2, "difieren en ({x},{y}): {a} vs {b}");
            }
        }
    }
}
