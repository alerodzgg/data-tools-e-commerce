//! Dónde va cada imagen: cuántas columnas nuevas se insertan, en qué
//! posición final quedan, y cómo se traducen los píxeles a las unidades que
//! usa Excel.
//!
//! Geometría pura sobre la hoja. No descarga nada ni sabe qué hay dentro de
//! una imagen.

use std::collections::HashMap;

use umya_spreadsheet::Worksheet;

use super::MAX_COLUMNAS_HOJA;

/// Alto de fila: Excel lo mide en puntos. Conversión estándar a 96 DPI
/// (1 pt = 1/72", 1 px = 1/96") → pt = px × 0.75.
pub(super) fn px_a_puntos(px: u32) -> f64 {
    px as f64 * 0.75
}

/// Ancho de columna: Excel lo mide en "caracteres de la fuente por defecto"
/// (Calibri 11), no en píxeles. Aproximación estándar de la industria:
/// unidades ≈ píxeles / 7.
pub(super) fn px_a_ancho_columna(px: u32) -> f64 {
    px as f64 / 7.0
}

/// Una URL de imagen ya con su celda destino calculada en la hoja (1-based,
/// como en Excel).
pub(super) struct TareaImagen {
    pub(super) fila_excel: u32,
    pub(super) col_destino: u32,
    pub(super) url: String,
}

/// Por columna URL, `k` = máximo de URLs en cualquiera de sus celdas: ese es
/// el número de columnas nuevas que se le insertan a la derecha, de una sola
/// vez para TODA la columna. La posición final de cada columna nueva se
/// calcula con una suma de prefijos ANTES de tocar la hoja (válida sin
/// importar el orden real de inserción); las inserciones reales se hacen de
/// derecha a izquierda con los índices ORIGINALES.
pub(super) fn insertar_columnas(
    ws: &mut Worksheet,
    recolectado: &HashMap<u32, HashMap<u32, Vec<String>>>,
    avisar: &mut dyn FnMut(&str),
) -> HashMap<u32, Vec<u32>> {
    insertar_columnas_con_limite(ws, recolectado, MAX_COLUMNAS_HOJA, avisar)
}

/// El límite entra por parámetro para poder ejercer el recorte en un test
/// sin construir una hoja de 16 384 columnas (mismo criterio que
/// [`verificar_tamano_xlsx_con_limites`]).
pub(super) fn insertar_columnas_con_limite(
    ws: &mut Worksheet,
    recolectado: &HashMap<u32, HashMap<u32, Vec<String>>>,
    max_columnas: u32,
    avisar: &mut dyn FnMut(&str),
) -> HashMap<u32, Vec<u32>> {
    let mut columnas: Vec<u32> = recolectado.keys().copied().collect();
    columnas.sort_unstable();

    let pedidas: HashMap<u32, u32> = columnas
        .iter()
        .map(|&c| {
            let k = recolectado[&c].values().map(Vec::len).max().unwrap_or(0) as u32;
            (c, k)
        })
        .collect();

    let mut destino_final: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut concedidas: HashMap<u32, u32> = HashMap::new();
    let mut insertados_antes = 0u32;
    let mut recortadas = 0u32;
    for &c in &columnas {
        let col_final = c + insertados_antes;
        // Una sola celda puede traer muchas URLs, así que `k` lo fija el
        // dato del usuario: sin este tope, un archivo con una lista larga
        // de imágenes empuja la hoja más allá de las columnas que Excel
        // admite y el .xlsx resultante no abre — sin ningún aviso.
        let k = pedidas[&c].min(max_columnas.saturating_sub(col_final));
        recortadas += pedidas[&c] - k;
        destino_final.insert(c, ((col_final + 1)..=(col_final + k)).collect());
        concedidas.insert(c, k);
        insertados_antes += k;
    }

    if recortadas > 0 {
        avisar(&format!(
            "Se llegó al límite de {max_columnas} columnas de Excel: {recortadas} imagen(es) \
                 por fila quedaron sin insertar. El resto sí se insertó."
        ));
    }

    for &c in columnas.iter().rev() {
        let k = concedidas[&c];
        if k > 0 {
            ws.insert_new_column_by_index(&(c + 1), &k);
        }
    }

    destino_final
}

pub(super) fn generar_tareas(
    recolectado: &HashMap<u32, HashMap<u32, Vec<String>>>,
    destino_final: &HashMap<u32, Vec<u32>>,
) -> Vec<TareaImagen> {
    let mut tareas = Vec::new();
    for (col_original, filas) in recolectado {
        let Some(columnas_destino) = destino_final.get(col_original) else {
            continue;
        };
        for (&fila, urls) in filas {
            // `zip` corta solo cuando la columna se recortó por el límite
            // de Excel: las URLs que sobran no tienen dónde ir.
            for (url, &col_destino) in urls.iter().zip(columnas_destino) {
                tareas.push(TareaImagen {
                    fila_excel: fila,
                    col_destino,
                    url: url.clone(),
                });
            }
        }
    }
    // `recolectado` es un `HashMap` y Rust aleatoriza su orden de iteración
    // por proceso. El orden de las tareas decide qué nombre recibe cada
    // imagen dentro del paquete (`xl/media/img_N.png`), así que sin ordenar
    // acá dos corridas sobre el mismo archivo dan .xlsx distintos byte a byte.
    tareas.sort_by_key(|t| (t.fila_excel, t.col_destino));
    tareas
}

#[cfg(test)]
mod tests {
    use super::*;
    use umya_spreadsheet as xlsx;

    fn libro_de_prueba() -> xlsx::Spreadsheet {
        xlsx::new_file()
    }

    /// Una celda con `n` URLs para la columna `col`.
    fn recolectado_de(col: u32, n: usize) -> HashMap<u32, HashMap<u32, Vec<String>>> {
        let urls: Vec<String> = (0..n).map(|i| format!("http://x/{i}.png")).collect();
        HashMap::from([(col, HashMap::from([(2u32, urls)]))])
    }

    #[test]
    fn px_a_puntos_y_ancho_columna_convierten_estandar() {
        assert_eq!(px_a_puntos(168), 126.0);
        assert!((px_a_ancho_columna(118) - 16.857142857142858).abs() < 1e-9);
    }
    #[test]
    fn no_se_insertan_mas_columnas_de_las_que_excel_admite() {
        // Una sola celda puede traer una lista larga de URLs, así que `k` lo
        // fija el dato del usuario. Pasarse del tope produce un .xlsx que
        // Excel se niega a abrir, y sin este recorte no habría ni aviso.
        let mut libro = libro_de_prueba();
        let ws = libro.get_sheet_by_name_mut("Sheet1").unwrap();
        let recolectado = recolectado_de(1, 10);

        let mut avisos = Vec::new();
        let destino = insertar_columnas_con_limite(ws, &recolectado, 4, &mut |m| avisos.push(m.to_string()));

        // Con la columna origen en 1 y tope 4, entran 3 columnas nuevas (2..=4).
        assert_eq!(destino[&1], vec![2, 3, 4]);
        assert_eq!(avisos.len(), 1, "el recorte debe avisarse, no ser silencioso");
        assert!(avisos[0].contains('7'), "aviso inesperado: {}", avisos[0]);
    }
    #[test]
    fn las_urls_recortadas_no_generan_tareas_fuera_de_rango() {
        // `generar_tareas` indexaba por posición contra las columnas destino:
        // en cuanto el recorte las hace más cortas que las URLs de la fila,
        // ese indexado sería un panic.
        let recolectado = recolectado_de(1, 10);
        let destino = HashMap::from([(1u32, vec![2, 3, 4])]);

        let tareas = generar_tareas(&recolectado, &destino);

        assert_eq!(tareas.len(), 3);
        assert_eq!(
            tareas.iter().map(|t| t.col_destino).collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
    }
    #[test]
    fn sin_recorte_no_se_avisa_nada() {
        let mut libro = libro_de_prueba();
        let ws = libro.get_sheet_by_name_mut("Sheet1").unwrap();
        let recolectado = recolectado_de(1, 3);

        let mut avisos = Vec::new();
        let destino = insertar_columnas_con_limite(ws, &recolectado, MAX_COLUMNAS_HOJA, &mut |m| {
            avisos.push(m.to_string())
        });

        assert_eq!(destino[&1], vec![2, 3, 4]);
        assert!(avisos.is_empty());
    }
    #[test]
    fn insertar_columnas_calcula_destino_final_y_desplaza_lo_existente() {
        let mut libro = libro_de_prueba();
        let ws = libro.get_sheet_by_name_mut("Sheet1").unwrap();
        // "Fotos" en col 2 con hasta 2 URLs por celda; "Precio" en col 3 (debe
        // desplazarse a la col 5 tras insertar las 2 columnas de "Fotos").
        ws.get_cell_mut((1, 1)).set_value("Sku");
        ws.get_cell_mut((2, 1)).set_value("Fotos");
        ws.get_cell_mut((3, 1)).set_value("Precio");
        ws.get_cell_mut((3, 2)).set_value("100");

        let mut recolectado: HashMap<u32, HashMap<u32, Vec<String>>> = HashMap::new();
        recolectado.insert(
            2,
            HashMap::from([(
                2u32,
                vec!["http://x.com/1.jpg".to_string(), "http://x.com/2.jpg".to_string()],
            )]),
        );

        let destino = insertar_columnas(ws, &recolectado, &mut |_| {});
        assert_eq!(destino[&2], vec![3, 4]);

        // "Precio" (originalmente en 3) debe haberse corrido a la 5.
        assert_eq!(ws.get_value((5, 1)), "Precio");
        assert_eq!(ws.get_value((5, 2)), "100");
    }
}
