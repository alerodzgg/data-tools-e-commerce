//! Agrupación/fusión de cajas de texto detectadas por CRAFT.
//!
//! `group_text_box` separa cajas casi-horizontales (`horizontal_list`) de
//! las inclinadas (`free_list`), funde las horizontales cercanas en la
//! misma línea y les agrega un margen proporcional.

use crate::craft_utils::TextBox;

/// Caja horizontal fusionada: `[x_min, x_max, y_min, y_max]` (con margen ya aplicado).
pub type HorizontalBox = [f32; 4];

/// Cuadrilátero libre (texto inclinado), 4 esquinas `[x, y]` con margen aplicado.
pub type FreeBox = [[f32; 2]; 4];

#[derive(Debug, Clone, Copy)]
struct RawBox {
    x_min: f32,
    x_max: f32,
    y_min: f32,
    y_max: f32,
    y_center: f32,
    height: f32,
}

#[allow(clippy::too_many_arguments)]
pub fn group_text_box(
    polys: &[TextBox],
    slope_ths: f32,
    ycenter_ths: f32,
    height_ths: f32,
    width_ths: f32,
    add_margin: f32,
    sort_output: bool,
) -> (Vec<HorizontalBox>, Vec<FreeBox>) {
    let mut horizontal_list: Vec<RawBox> = Vec::new();
    let mut free_list: Vec<FreeBox> = Vec::new();

    for poly in polys {
        let (x0, y0) = (poly[0].x, poly[0].y);
        let (x1, y1) = (poly[1].x, poly[1].y);
        let (x2, y2) = (poly[2].x, poly[2].y);
        let (x3, y3) = (poly[3].x, poly[3].y);

        let slope_up = (y1 - y0) / (x1 - x0).max(10.0);
        let slope_down = (y2 - y3) / (x2 - x3).max(10.0);

        if slope_up.abs().max(slope_down.abs()) < slope_ths {
            let x_max = x0.max(x1).max(x2).max(x3);
            let x_min = x0.min(x1).min(x2).min(x3);
            let y_max = y0.max(y1).max(y2).max(y3);
            let y_min = y0.min(y1).min(y2).min(y3);
            horizontal_list.push(RawBox {
                x_min,
                x_max,
                y_min,
                y_max,
                y_center: 0.5 * (y_min + y_max),
                height: y_max - y_min,
            });
        } else {
            let height = ((x3 - x0).powi(2) + (y3 - y0).powi(2)).sqrt();
            let width = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
            let margin = (1.44 * add_margin * width.min(height)) as i32 as f32;

            let theta13 = ((y0 - y2) / (x0 - x2).max(10.0)).atan().abs();
            let theta24 = ((y1 - y3) / (x1 - x3).max(10.0)).atan().abs();

            let nx0 = x0 - theta13.cos() * margin;
            let ny0 = y0 - theta13.sin() * margin;
            let nx1 = x1 + theta24.cos() * margin;
            let ny1 = y1 - theta24.sin() * margin;
            let nx2 = x2 + theta13.cos() * margin;
            let ny2 = y2 + theta13.sin() * margin;
            let nx3 = x3 - theta24.cos() * margin;
            let ny3 = y3 + theta24.sin() * margin;
            free_list.push([[nx0, ny0], [nx1, ny1], [nx2, ny2], [nx3, ny3]]);
        }
    }

    if sort_output {
        // `unwrap_or(Equal)`: geometría derivada de detecciones sobre
        // imágenes de terceros (no confiables) podría dar NaN en algún caso
        // extremo; tratarlo como "iguales" en vez de entrar en pánico.
        horizontal_list.sort_by(|a, b| {
            a.y_center
                .partial_cmp(&b.y_center)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    // Combinar en grupos (líneas) de altura/y_center comparable.
    let mut combined_list: Vec<Vec<RawBox>> = Vec::new();
    let mut new_box: Vec<RawBox> = Vec::new();
    let mut b_height: Vec<f32> = Vec::new();
    let mut b_ycenter: Vec<f32> = Vec::new();
    for poly in horizontal_list {
        if new_box.is_empty() {
            b_height = vec![poly.height];
            b_ycenter = vec![poly.y_center];
            new_box.push(poly);
        } else {
            let mean_height = mean(&b_height);
            let mean_ycenter = mean(&b_ycenter);
            if (mean_ycenter - poly.y_center).abs() < ycenter_ths * mean_height {
                b_height.push(poly.height);
                b_ycenter.push(poly.y_center);
                new_box.push(poly);
            } else {
                combined_list.push(std::mem::take(&mut new_box));
                b_height = vec![poly.height];
                b_ycenter = vec![poly.y_center];
                new_box.push(poly);
            }
        }
    }
    combined_list.push(new_box);

    let mut merged_list: Vec<HorizontalBox> = Vec::new();

    for boxes in combined_list {
        if boxes.len() == 1 {
            let b = boxes[0];
            let margin = (add_margin * (b.x_max - b.x_min).min(b.height)) as i32 as f32;
            merged_list.push([
                b.x_min - margin,
                b.x_max + margin,
                b.y_min - margin,
                b.y_max + margin,
            ]);
        } else if boxes.len() > 1 {
            let mut boxes = boxes;
            boxes.sort_by(|a, b| a.x_min.partial_cmp(&b.x_min).unwrap_or(std::cmp::Ordering::Equal));

            let mut merged_box: Vec<Vec<RawBox>> = Vec::new();
            let mut new_box: Vec<RawBox> = Vec::new();
            let mut b_height: Vec<f32> = Vec::new();
            let mut x_max = 0.0f32;
            for b in boxes {
                if new_box.is_empty() {
                    b_height = vec![b.height];
                    x_max = b.x_max;
                    new_box.push(b);
                } else {
                    let mean_height = mean(&b_height);
                    if (mean_height - b.height).abs() < height_ths * mean_height
                        && (b.x_min - x_max) < width_ths * (b.y_max - b.y_min)
                    {
                        b_height.push(b.height);
                        x_max = b.x_max;
                        new_box.push(b);
                    } else {
                        b_height = vec![b.height];
                        x_max = b.x_max;
                        merged_box.push(std::mem::take(&mut new_box));
                        new_box.push(b);
                    }
                }
            }
            if !new_box.is_empty() {
                merged_box.push(new_box);
            }

            for mbox in merged_box {
                if mbox.len() != 1 {
                    let x_min = mbox.iter().map(|b| b.x_min).fold(f32::INFINITY, f32::min);
                    let x_max = mbox.iter().map(|b| b.x_max).fold(f32::NEG_INFINITY, f32::max);
                    let y_min = mbox.iter().map(|b| b.y_min).fold(f32::INFINITY, f32::min);
                    let y_max = mbox.iter().map(|b| b.y_max).fold(f32::NEG_INFINITY, f32::max);
                    let margin = (add_margin * (x_max - x_min).min(y_max - y_min)) as i32 as f32;
                    merged_list.push([x_min - margin, x_max + margin, y_min - margin, y_max + margin]);
                } else {
                    let b = mbox[0];
                    let margin = (add_margin * (b.x_max - b.x_min).min(b.y_max - b.y_min)) as i32 as f32;
                    merged_list.push([
                        b.x_min - margin,
                        b.x_max + margin,
                        b.y_min - margin,
                        b.y_max + margin,
                    ]);
                }
            }
        }
    }

    (merged_list, free_list)
}

fn mean(xs: &[f32]) -> f32 {
    xs.iter().sum::<f32>() / xs.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::craft_utils::BoxPoint;

    fn caja(x0: f32, y0: f32, x1: f32, y1: f32) -> TextBox {
        // rectángulo horizontal: top-left, top-right, bottom-right, bottom-left
        [
            BoxPoint { x: x0, y: y0 },
            BoxPoint { x: x1, y: y0 },
            BoxPoint { x: x1, y: y1 },
            BoxPoint { x: x0, y: y1 },
        ]
    }

    #[test]
    fn una_caja_horizontal_simple_recibe_margen() {
        let polys = vec![caja(10.0, 10.0, 60.0, 30.0)];
        let (horiz, free) = group_text_box(&polys, 0.1, 0.5, 0.5, 0.5, 0.1, true);
        assert!(free.is_empty());
        assert_eq!(horiz.len(), 1);
        // margen = int(0.1*min(50,20)) = int(2.0) = 2
        assert_eq!(horiz[0], [8.0, 62.0, 8.0, 32.0]);
    }

    #[test]
    fn dos_cajas_en_la_misma_linea_se_funden() {
        let polys = vec![caja(0.0, 10.0, 40.0, 30.0), caja(45.0, 10.0, 90.0, 30.0)];
        let (horiz, _free) = group_text_box(&polys, 0.1, 0.5, 0.5, 0.5, 0.1, true);
        assert_eq!(horiz.len(), 1);
        assert_eq!(horiz[0][0], -2.0); // x_min=0, margen=int(0.1*min(90,20))=2
        assert_eq!(horiz[0][1], 92.0); // x_max=90+2
    }

    #[test]
    fn dos_cajas_en_lineas_distintas_no_se_funden() {
        let polys = vec![caja(0.0, 10.0, 40.0, 30.0), caja(0.0, 200.0, 40.0, 220.0)];
        let (horiz, _free) = group_text_box(&polys, 0.1, 0.5, 0.5, 0.5, 0.1, true);
        assert_eq!(horiz.len(), 2);
    }

    #[test]
    fn coordenada_nan_no_entra_en_panico_al_ordenar() {
        // Antes: `.partial_cmp(...).unwrap()` entraba en pánico si una caja
        // traía NaN (geometría derivada de detecciones sobre imágenes de
        // terceros, no confiables). Dos cajas en la misma línea horizontal
        // ejercitan el `sort_by` de `y_center` y, al fusionarse, el de
        // `x_min` también.
        let polys = vec![caja(f32::NAN, 10.0, 40.0, 30.0), caja(45.0, 10.0, 90.0, 30.0)];
        let (_horiz, _free) = group_text_box(&polys, 0.1, 0.5, 0.5, 0.5, 0.1, true);
    }

    #[test]
    fn caja_muy_inclinada_va_a_free_list() {
        // pendiente pronunciada: sube 50px en 40px de ancho (slope_up = 50/40 = 1.25 >> 0.1)
        let polys = vec![[
            BoxPoint { x: 0.0, y: 50.0 },
            BoxPoint { x: 40.0, y: 0.0 },
            BoxPoint { x: 45.0, y: 5.0 },
            BoxPoint { x: 5.0, y: 55.0 },
        ]];
        let (horiz, free) = group_text_box(&polys, 0.1, 0.5, 0.5, 0.5, 0.1, true);
        assert!(horiz.is_empty());
        assert_eq!(free.len(), 1);
    }
}
