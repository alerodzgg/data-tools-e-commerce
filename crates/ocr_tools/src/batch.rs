//! Tipos de tarea/resultado por celda y reconstrucción vectorial del
//! `DataFrame` tras el análisis: `CellTask`/`CellResult`/
//! `AsyncBatchProcessor::materialize`.
//!
//! `materialize` podría vectorizarse con expresiones Polars
//! (`any_horizontal`, `concat_str`); aquí se recorre por filas, mismo
//! criterio que en `url_compactor` (más simple de leer/auditar en Rust que
//! encadenar expresiones perezosas para esta lógica).

use std::collections::HashMap;

use polars::prelude::*;

#[derive(Debug, Clone)]
pub struct CellTask {
    pub idx: i64,
    pub col: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct CellResult {
    pub idx: i64,
    pub col: String,
    pub approved: bool,
    pub reason: String,
}

pub(crate) use commerce_core::columna_texto;

/// Reconstruye `df` con las columnas de veredicto por imagen: URLs rechazadas
/// vaciadas, `_{col}_aprobada`/`_{col}_motivo` por columna, y los agregados
/// por fila `_imagen_aprobada`/`_tiene_rechazada`/`_imagen_motivo`.
///
/// Conteo a nivel de IMAGEN (celda-URL), no de fila: cada entrada de
/// `results` es una celda-imagen evaluada.
pub fn materialize(
    df: &DataFrame,
    url_columns: &[String],
    results: &HashMap<(i64, String), CellResult>,
    idx_offset: i64,
) -> PolarsResult<DataFrame> {
    let n = df.height();

    let mut aprobada_por_col: HashMap<&str, Vec<bool>> = HashMap::new();
    let mut motivo_por_col: HashMap<&str, Vec<String>> = HashMap::new();
    for col in url_columns {
        aprobada_por_col.insert(col.as_str(), vec![true; n]);
        motivo_por_col.insert(col.as_str(), vec![String::new(); n]);
    }
    for cr in results.values() {
        let Some(aprobada) = aprobada_por_col.get_mut(cr.col.as_str()) else {
            continue;
        };
        let local = cr.idx - idx_offset;
        if local >= 0 && (local as usize) < n {
            let local = local as usize;
            aprobada[local] = cr.approved;
            // `aprobada_por_col`/`motivo_por_col` se poblaron con el MISMO
            // conjunto de claves (`url_columns`, arriba); el `get_mut` de
            // `aprobada` dos líneas arriba ya confirmó que `cr.col` es una
            // clave válida.
            #[allow(clippy::unwrap_used)]
            {
                motivo_por_col.get_mut(cr.col.as_str()).unwrap()[local] = cr.reason.clone();
            }
        }
    }

    let mut out = df.clone();
    let mut limpias: HashMap<&str, Vec<String>> = HashMap::new();
    for col in url_columns {
        let aprobada = &aprobada_por_col[col.as_str()];
        let motivo = &motivo_por_col[col.as_str()];
        out.with_column(Column::new(format!("_{col}_aprobada").into(), aprobada.clone()))?;
        out.with_column(Column::new(format!("_{col}_motivo").into(), motivo.clone()))?;

        let actual = columna_texto(df, col)?;
        let limpia: Vec<String> = (0..n)
            .map(|i| {
                if !aprobada[i] {
                    String::new()
                } else {
                    actual[i].clone().unwrap_or_default()
                }
            })
            .collect();
        limpias.insert(col.as_str(), limpia);
    }
    for col in url_columns {
        out.with_column(Column::new(col.as_str().into(), limpias[col.as_str()].clone()))?;
    }

    let mut imagen_aprobada = vec![false; n];
    let mut tiene_rechazada = vec![false; n];
    let mut imagen_motivo = vec![String::new(); n];
    for i in 0..n {
        let mut partes: Vec<String> = Vec::new();
        for col in url_columns {
            if aprobada_por_col[col.as_str()][i] {
                imagen_aprobada[i] = true;
            } else {
                tiene_rechazada[i] = true;
            }
            let motivo = &motivo_por_col[col.as_str()][i];
            if !motivo.is_empty() {
                partes.push(format!("[{col}] {motivo}"));
            }
        }
        imagen_motivo[i] = partes.join(" | ");
    }

    out.with_column(Column::new("_imagen_aprobada".into(), imagen_aprobada))?;
    out.with_column(Column::new("_tiene_rechazada".into(), tiene_rechazada))?;
    out.with_column(Column::new("_imagen_motivo".into(), imagen_motivo))?;

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (DataFrame, Vec<String>, HashMap<(i64, String), CellResult>) {
        let url_columns: Vec<String> = ["Imagen 1", "Imagen 2", "Imagen 3", "Imagen 4"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let bloque = df! {
            "Sku" => ["S0", "S1"],
            "Imagen 1" => ["urlA", "urlW"],
            "Imagen 2" => ["urlB", "urlX"],
            "Imagen 3" => ["urlC", "urlY"],
            "Imagen 4" => ["urlD", "urlZ"],
        }
        .unwrap();

        let cr = |idx: i64, col: &str, approved: bool, reason: &str| CellResult {
            idx,
            col: col.to_string(),
            approved,
            reason: reason.to_string(),
        };
        let mut resultados = HashMap::new();
        resultados.insert((0, "Imagen 1".to_string()), cr(0, "Imagen 1", false, "D1·Banner"));
        resultados.insert((0, "Imagen 3".to_string()), cr(0, "Imagen 3", false, "D2·Fondo"));
        resultados.insert((1, "Imagen 2".to_string()), cr(1, "Imagen 2", false, "D5·Logo"));
        resultados.insert(
            (1, "Imagen 4".to_string()),
            cr(1, "Imagen 4", false, "D6·Placeholder"),
        );
        (bloque, url_columns, resultados)
    }

    #[test]
    fn motivo_indica_a_que_imagen_corresponde() {
        let (bloque, url_columns, resultados) = fixture();
        let df = materialize(&bloque, &url_columns, &resultados, 0).unwrap();
        let motivos: Vec<Option<&str>> = df
            .column("_imagen_motivo")
            .unwrap()
            .str()
            .unwrap()
            .iter()
            .collect();
        assert_eq!(motivos[0], Some("[Imagen 1] D1·Banner | [Imagen 3] D2·Fondo"));
        assert_eq!(motivos[1], Some("[Imagen 2] D5·Logo | [Imagen 4] D6·Placeholder"));
    }

    #[test]
    fn urls_rechazadas_quedan_vacias_y_las_no_evaluadas_sobreviven() {
        let (bloque, url_columns, resultados) = fixture();
        let df = materialize(&bloque, &url_columns, &resultados, 0).unwrap();
        assert_eq!(df.column("Imagen 1").unwrap().str().unwrap().get(0), Some(""));
        assert_eq!(df.column("Imagen 2").unwrap().str().unwrap().get(0), Some("urlB"));
    }

    #[test]
    fn imagen_aprobada_y_tiene_rechazada_reflejan_el_mix_por_fila() {
        let (bloque, url_columns, resultados) = fixture();
        let df = materialize(&bloque, &url_columns, &resultados, 0).unwrap();
        let aprobada: Vec<Option<bool>> = df
            .column("_imagen_aprobada")
            .unwrap()
            .bool()
            .unwrap()
            .iter()
            .collect();
        let rechazada: Vec<Option<bool>> = df
            .column("_tiene_rechazada")
            .unwrap()
            .bool()
            .unwrap()
            .iter()
            .collect();
        assert_eq!(aprobada, vec![Some(true), Some(true)]); // Imagen 2/4 y 1/3 sobreviven en cada fila
        assert_eq!(rechazada, vec![Some(true), Some(true)]);
    }

    #[test]
    fn idx_offset_desplaza_el_indice_local_al_global() {
        let (bloque, url_columns, _resultados) = fixture();
        let mut resultados = HashMap::new();
        resultados.insert(
            (10, "Imagen 1".to_string()),
            CellResult {
                idx: 10,
                col: "Imagen 1".to_string(),
                approved: false,
                reason: "D1".to_string(),
            },
        );
        // idx global 10, offset 10 -> local 0
        let df = materialize(&bloque, &url_columns, &resultados, 10).unwrap();
        assert_eq!(df.column("Imagen 1").unwrap().str().unwrap().get(0), Some(""));
    }
}
