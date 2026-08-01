//! Postprocesado de CRAFT: de los mapas de texto/enlace a cajas de texto
//! (`get_det_boxes_core`, `adjust_result_coordinates`).
//!
//! Este pipeline corre siempre con `poly=false` (sin ajuste de polígono),
//! así que la variante que arma polígonos no está implementada: nunca se
//! ejecuta en el camino real de este crate.

use image::{GrayImage, Luma};
use imageproc::distance_transform::Norm;
use imageproc::geometry::min_area_rect;
use imageproc::morphology::dilate;
use imageproc::point::Point;
use imageproc::region_labelling::{connected_components, Connectivity};
use ndarray::Array2;

/// Una esquina de una caja de texto detectada, en coordenadas de imagen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxPoint {
    pub x: f32,
    pub y: f32,
}

/// Caja de texto: 4 esquinas en orden horario, empezando por la de menor `x+y`
/// (misma convención que `box = np.roll(box, 4-startidx, 0)` en el original).
pub type TextBox = [BoxPoint; 4];

/// Extrae las cajas de texto de los mapas de score de CRAFT.
///
/// `textmap`/`linkmap`: salida cruda del detector (probabilidades, no aún
/// binarizadas), forma `(H, W)`.
pub fn get_det_boxes_core(
    textmap: &Array2<f32>,
    linkmap: &Array2<f32>,
    text_threshold: f32,
    link_threshold: f32,
    low_text: f32,
) -> Vec<TextBox> {
    let (h, w) = textmap.dim();

    // cv2.threshold(map, umbral, 1, THRESH_BINARY) → 1 si > umbral, si no 0.
    let text_score = textmap.mapv(|v| v > low_text);
    let link_score = linkmap.mapv(|v| v > link_threshold);

    let mut comb = GrayImage::new(w as u32, h as u32);
    for y in 0..h {
        for x in 0..w {
            if text_score[[y, x]] || link_score[[y, x]] {
                comb.put_pixel(x as u32, y as u32, Luma([255]));
            }
        }
    }

    let labels = connected_components(&comb, Connectivity::Four, Luma([0u8]));
    let n_labels = labels.pixels().map(|p| p.0[0]).max().unwrap_or(0) + 1;

    struct Stats {
        left: u32,
        top: u32,
        right: u32,
        bottom: u32,
        area: u32,
    }
    let mut stats: Vec<Option<Stats>> = (0..n_labels).map(|_| None).collect();
    let mut max_text: Vec<f32> = vec![f32::NEG_INFINITY; n_labels as usize];
    for y in 0..h {
        for x in 0..w {
            let l = labels.get_pixel(x as u32, y as u32).0[0];
            if l == 0 {
                continue;
            }
            let e = stats[l as usize].get_or_insert(Stats {
                left: x as u32,
                top: y as u32,
                right: x as u32,
                bottom: y as u32,
                area: 0,
            });
            e.left = e.left.min(x as u32);
            e.top = e.top.min(y as u32);
            e.right = e.right.max(x as u32);
            e.bottom = e.bottom.max(y as u32);
            e.area += 1;
            if textmap[[y, x]] > max_text[l as usize] {
                max_text[l as usize] = textmap[[y, x]];
            }
        }
    }

    let mut det = Vec::new();

    for k in 1..n_labels {
        let Some(st) = &stats[k as usize] else {
            continue;
        };
        if st.area < 10 {
            continue;
        }
        if max_text[k as usize] < text_threshold {
            continue;
        }

        let bbox_w = (st.right - st.left + 1) as f64;
        let bbox_h = (st.bottom - st.top + 1) as f64;
        let size = st.area as f64;
        let niter = ((size * bbox_w.min(bbox_h) / (bbox_w * bbox_h)).sqrt() * 2.0) as i64;

        // segmap: 255 donde labels==k, salvo los píxeles que son SOLO de enlace
        // (sin texto) — se descartan antes de dilatar, igual que el original.
        let mut segmap = GrayImage::new(w as u32, h as u32);
        for y in 0..h {
            for x in 0..w {
                if labels.get_pixel(x as u32, y as u32).0[0] == k {
                    let solo_enlace = link_score[[y, x]] && !text_score[[y, x]];
                    if !solo_enlace {
                        segmap.put_pixel(x as u32, y as u32, Luma([255]));
                    }
                }
            }
        }

        // El kernel original es un cuadrado MORPH_RECT de lado (1+niter); con
        // ancla por defecto (ksize/2) eso equivale, en distancia Chebyshev
        // (norma L-infinito), a un radio de dilatación de ksize/2 = (1+niter)/2.
        // El margen de recorte del original (±niter) es generoso a propósito
        // (mayor que el radio real necesario), así que dilatar sobre la imagen
        // completa da el mismo resultado que recortar y pegar de vuelta.
        let radio = ((1 + niter).max(0) / 2) as u8;
        let segmap = if radio > 0 {
            dilate(&segmap, Norm::LInf, radio)
        } else {
            segmap
        };

        let mut points: Vec<Point<i32>> = Vec::new();
        for y in 0..h {
            for x in 0..w {
                if segmap.get_pixel(x as u32, y as u32).0[0] != 0 {
                    points.push(Point::new(x as i32, y as i32));
                }
            }
        }
        if points.is_empty() {
            continue;
        }

        let rect = min_area_rect(&points);
        let mut corners: [(f32, f32); 4] = [
            (rect[0].x as f32, rect[0].y as f32),
            (rect[1].x as f32, rect[1].y as f32),
            (rect[2].x as f32, rect[2].y as f32),
            (rect[3].x as f32, rect[3].y as f32),
        ];

        let side01 = dist(corners[0], corners[1]);
        let side12 = dist(corners[1], corners[2]);
        let box_ratio = side01.max(side12) / (side01.min(side12) + 1e-5);
        if (1.0 - box_ratio).abs() <= 0.1 {
            // El `continue` de la línea 143 ya garantiza `points` no vacío.
            #[allow(clippy::unwrap_used)]
            let (l, r, t, b) = (
                points.iter().map(|p| p.x).min().unwrap() as f32,
                points.iter().map(|p| p.x).max().unwrap() as f32,
                points.iter().map(|p| p.y).min().unwrap() as f32,
                points.iter().map(|p| p.y).max().unwrap() as f32,
            );
            corners = [(l, t), (r, t), (r, b), (l, b)];
        }

        // `corners` es un array de 4 elementos: `min_by` sobre su iterador
        // nunca da `None`.
        #[allow(clippy::unwrap_used)]
        let start_idx = corners
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                // `partial_cmp` puede dar `None` si algún corner llegó con
                // coordenadas NaN (geometría derivada de detecciones sobre
                // imágenes de terceros, no confiables): tratarlo como
                // "iguales" en vez de entrar en pánico.
                (a.0 + a.1)
                    .partial_cmp(&(b.0 + b.1))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .unwrap();
        let mut ordered = [BoxPoint { x: 0.0, y: 0.0 }; 4];
        for (i, slot) in ordered.iter_mut().enumerate() {
            let src = (i + start_idx) % 4;
            *slot = BoxPoint {
                x: corners[src].0,
                y: corners[src].1,
            };
        }
        det.push(ordered);
    }

    det
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Reescala las cajas (en coordenadas del mapa de heatmap, resolución mitad
/// de la imagen redimensionada) de vuelta a coordenadas de la imagen original.
pub fn adjust_result_coordinates(
    boxes: &[TextBox],
    ratio_w: f32,
    ratio_h: f32,
    ratio_net: f32,
) -> Vec<TextBox> {
    boxes
        .iter()
        .map(|b| {
            b.map(|p| BoxPoint {
                x: p.x * ratio_w * ratio_net,
                y: p.y * ratio_h * ratio_net,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cuadro_de_texto(h: usize, w: usize, y0: usize, y1: usize, x0: usize, x1: usize) -> Array2<f32> {
        let mut m = Array2::<f32>::zeros((h, w));
        for y in y0..y1 {
            for x in x0..x1 {
                m[[y, x]] = 0.9;
            }
        }
        m
    }

    #[test]
    fn detecta_una_caja_rectangular_simple() {
        let textmap = cuadro_de_texto(50, 100, 10, 30, 10, 60);
        let linkmap = Array2::<f32>::zeros((50, 100));
        let boxes = get_det_boxes_core(&textmap, &linkmap, 0.7, 0.4, 0.4);
        assert_eq!(boxes.len(), 1);
        let xs: Vec<f32> = boxes[0].iter().map(|p| p.x).collect();
        let ys: Vec<f32> = boxes[0].iter().map(|p| p.y).collect();
        assert!(xs.iter().copied().fold(f32::INFINITY, f32::min) <= 10.5);
        assert!(xs.iter().copied().fold(f32::NEG_INFINITY, f32::max) >= 59.0);
        assert!(ys.iter().copied().fold(f32::INFINITY, f32::min) <= 10.5);
        assert!(ys.iter().copied().fold(f32::NEG_INFINITY, f32::max) >= 29.0);
    }

    #[test]
    fn sin_texto_no_detecta_nada() {
        let textmap = Array2::<f32>::zeros((50, 100));
        let linkmap = Array2::<f32>::zeros((50, 100));
        let boxes = get_det_boxes_core(&textmap, &linkmap, 0.7, 0.4, 0.4);
        assert!(boxes.is_empty());
    }

    #[test]
    fn componente_demasiado_pequena_se_descarta() {
        let textmap = cuadro_de_texto(50, 100, 10, 12, 10, 12); // area 4 < 10
        let linkmap = Array2::<f32>::zeros((50, 100));
        let boxes = get_det_boxes_core(&textmap, &linkmap, 0.7, 0.4, 0.4);
        assert!(boxes.is_empty());
    }

    #[test]
    fn adjust_result_coordinates_escala_por_ratio_y_red() {
        let boxes = vec![[
            BoxPoint { x: 1.0, y: 2.0 },
            BoxPoint { x: 3.0, y: 4.0 },
            BoxPoint { x: 5.0, y: 6.0 },
            BoxPoint { x: 7.0, y: 8.0 },
        ]];
        let ajustadas = adjust_result_coordinates(&boxes, 2.0, 0.5, 2.0);
        assert_eq!(ajustadas[0][0], BoxPoint { x: 4.0, y: 2.0 });
        assert_eq!(ajustadas[0][3], BoxPoint { x: 28.0, y: 8.0 });
    }
}
