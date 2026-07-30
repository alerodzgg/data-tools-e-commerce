//! Elimina huecos horizontales: en cada fila, mueve las URLs no vacías hacia
//! la izquierda y rellena el resto con `""`. `UrlCompactor::compact`.
//!
//! Esto podría vectorizarse con `concat_list().list.drop_nulls()` en Polars,
//! pero un recorrido por filas es más simple de leer y auditar que la
//! maquinaria de listas de Polars para esta lógica (mismo criterio ya usado
//! en `etl_tools::borrado`).

use polars::prelude::*;

/// Tokens que cuentan como "celda vacía" (tras trim + lowercase). Única
/// fuente de verdad: `compact` y `tiene_valor` leen de aquí.
const TOKENS_VACIOS: [&str; 2] = ["", "nan"];

/// Decide si una celda tiene un valor "real" (no vacía, no el string
/// literal "nan").
pub fn tiene_valor(v: &str) -> bool {
    !TOKENS_VACIOS.contains(&v.trim().to_lowercase().as_str())
}

/// Compacta `url_columns` hacia la izquierda, fila por fila.
pub fn compact(df: &DataFrame, url_columns: &[String]) -> PolarsResult<DataFrame> {
    if url_columns.is_empty() {
        return Ok(df.clone());
    }

    let n = df.height();
    let valores: Vec<Vec<Option<String>>> = url_columns
        .iter()
        .map(|c| -> PolarsResult<Vec<Option<String>>> {
            let serie = df.column(c)?.as_materialized_series().clone();
            let serie = if serie.dtype() == &DataType::String {
                serie
            } else {
                serie.cast(&DataType::String)?
            };
            Ok(serie.str()?.iter().map(|v| v.map(str::to_string)).collect())
        })
        .collect::<PolarsResult<_>>()?;

    let mut compactadas: Vec<Vec<String>> = vec![Vec::with_capacity(n); url_columns.len()];
    for fila in 0..n {
        let supervivientes: Vec<&str> = valores
            .iter()
            .map(|col| col[fila].as_deref().unwrap_or(""))
            .filter(|v| tiene_valor(v))
            .collect();
        for (j, columna) in compactadas.iter_mut().enumerate() {
            columna.push(supervivientes.get(j).copied().unwrap_or("").to_string());
        }
    }

    let mut resultado = df.clone();
    for (col, valores) in url_columns.iter().zip(compactadas) {
        resultado.with_column(Column::new(col.as_str().into(), valores))?;
    }
    Ok(resultado)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiene_valor_descarta_vacio_y_literal_nan() {
        assert!(!tiene_valor(""));
        assert!(!tiene_valor("  "));
        assert!(!tiene_valor("nan"));
        assert!(!tiene_valor("NaN"));
        assert!(tiene_valor("http://x.com/1.jpg"));
    }

    #[test]
    fn compact_mueve_urls_no_vacias_a_la_izquierda() {
        let df = df! {
            "Sku" => ["A1", "A2"],
            "Imagen 1" => ["", "urlA"],
            "Imagen 2" => ["urlB", "nan"],
            "Imagen 3" => ["urlC", "urlD"],
        }
        .unwrap();
        let out = compact(
            &df,
            &[
                "Imagen 1".to_string(),
                "Imagen 2".to_string(),
                "Imagen 3".to_string(),
            ],
        )
        .unwrap();

        assert_eq!(
            out.column("Imagen 1").unwrap().str().unwrap().get(0),
            Some("urlB")
        );
        assert_eq!(
            out.column("Imagen 2").unwrap().str().unwrap().get(0),
            Some("urlC")
        );
        assert_eq!(out.column("Imagen 3").unwrap().str().unwrap().get(0), Some(""));

        assert_eq!(
            out.column("Imagen 1").unwrap().str().unwrap().get(1),
            Some("urlA")
        );
        assert_eq!(
            out.column("Imagen 2").unwrap().str().unwrap().get(1),
            Some("urlD")
        );
        assert_eq!(out.column("Imagen 3").unwrap().str().unwrap().get(1), Some(""));
    }

    #[test]
    fn compact_sin_columnas_url_devuelve_el_df_intacto() {
        let df = df! { "Sku" => ["A1"] }.unwrap();
        let out = compact(&df, &[]).unwrap();
        assert_eq!(out.height(), 1);
        assert_eq!(out.get_column_names(), df.get_column_names());
    }
}
