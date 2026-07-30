//! Utilidades genéricas sobre columnas de un `DataFrame` ya cargado.

use std::collections::HashSet;

use polars::prelude::*;

use crate::error::CoreResult;

/// Devuelve la columna `nombre` como texto, casteando si hace falta (nunca
/// falla por tipo: un `i64`/`f64`/etc. se convierte, solo un nombre de
/// columna inexistente da error). Duplicada antes de forma idéntica en
/// `publications_builder`, `publications_validator` y `ocr_tools`.
pub fn columna_texto(df: &DataFrame, nombre: &str) -> PolarsResult<Vec<Option<String>>> {
    let serie = df.column(nombre)?.as_materialized_series().clone();
    let serie = if serie.dtype() == &DataType::String {
        serie
    } else {
        serie.cast(&DataType::String)?
    };
    Ok(serie.str()?.iter().map(|v| v.map(str::to_string)).collect())
}

/// Apila verticalmente varios `DataFrame` ya con el mismo esquema. Vacío
/// (`DataFrame::empty()`) si `dfs` está vacío. Duplicada antes de forma
/// idéntica en `etl_tools` y `data_combinator`.
pub fn concatenar_vertical(dfs: Vec<DataFrame>) -> CoreResult<DataFrame> {
    let mut iter = dfs.into_iter();
    let Some(mut base) = iter.next() else {
        return Ok(DataFrame::empty());
    };
    for df in iter {
        base.vstack_mut_owned(df)?;
    }
    Ok(base)
}

/// Selecciona las filas en `indices` (orden preservado, puede repetir).
/// Duplicada antes de forma idéntica en `publications_builder` y
/// `publications_validator`.
pub fn tomar_filas(df: &DataFrame, indices: &[usize]) -> CoreResult<DataFrame> {
    let idx_ca = IdxCa::from_vec(PlSmallStr::EMPTY, indices.iter().map(|&i| i as IdxSize).collect());
    Ok(df.take(&idx_ca)?)
}

/// Concatena `dfs` alineando a la UNIÓN de columnas (rellena faltantes con
/// null), igual que `pl.concat(..., how="diagonal")` — a diferencia de
/// [`concatenar_vertical`], que asume el mismo esquema en todos. Duplicada
/// antes de forma idéntica en `publications_builder` y
/// `publications_validator`.
pub fn concat_diagonal(dfs: Vec<DataFrame>) -> CoreResult<DataFrame> {
    if dfs.is_empty() {
        return Ok(DataFrame::empty());
    }
    let mut columnas: Vec<String> = Vec::new();
    let mut vistas: HashSet<String> = HashSet::new();
    for df in &dfs {
        for c in df.get_column_names() {
            if vistas.insert(c.to_string()) {
                columnas.push(c.to_string());
            }
        }
    }
    let mut resultado: Option<DataFrame> = None;
    for df in dfs {
        let mut df = df;
        for c in &columnas {
            if df.column(c).is_err() {
                let nulos: Vec<Option<String>> = vec![None; df.height()];
                df.with_column(Column::new(c.as_str().into(), nulos))?;
            }
        }
        let df = df.select(columnas.iter().map(String::as_str))?;
        resultado = Some(match resultado {
            None => df,
            Some(acc) => acc.vstack(&df)?,
        });
    }
    Ok(resultado.unwrap_or_else(DataFrame::empty))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columna_texto_castea_numericos_y_preserva_nulos() {
        let df = df!("A" => [Some(1i64), None, Some(3)]).unwrap();
        assert_eq!(
            columna_texto(&df, "A").unwrap(),
            vec![Some("1".to_string()), None, Some("3".to_string())]
        );
    }

    #[test]
    fn columna_texto_columna_inexistente_da_error() {
        let df = df!("A" => [1i64]).unwrap();
        assert!(columna_texto(&df, "NoExiste").is_err());
    }
}
