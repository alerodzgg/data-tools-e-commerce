use commerce_core::CoreResult;
use polars::prelude::*;

use crate::constantes::TOKENS_NULOS;

fn es_token_nulo(valor: &str) -> bool {
    TOKENS_NULOS.contains(&valor.trim().to_lowercase().as_str())
}

/// Alinea el chunk a `columnas` (rellena las faltantes con vacío), normaliza
/// a texto, mapea los tokens nulos ('nan', 'null', …) y los nulos reales a
/// cadena vacía, y repara mojibake.
///
/// Nunca se tocan las columnas que no están en `columnas`: se construyen
/// únicamente las de salida leyendo del `df` de origen, sin necesidad de un
/// paso previo de descarte.
pub fn normalizar(df: &DataFrame, columnas: &[String]) -> CoreResult<DataFrame> {
    let alto = df.height();
    let mut salida: Vec<Column> = Vec::with_capacity(columnas.len());

    for nombre in columnas {
        let valores: Vec<String> = match df.column(nombre) {
            Ok(col) => {
                let serie = col.as_materialized_series().clone();
                let serie = if serie.dtype() == &DataType::String {
                    serie
                } else {
                    serie.cast(&DataType::String)?
                };
                serie
                    .str()?
                    .iter()
                    .map(|v| match v {
                        Some(s) if !es_token_nulo(s) => s.to_string(),
                        _ => String::new(),
                    })
                    .collect()
            }
            // Columna ausente en este chunk: vacía (no nula).
            Err(_) => vec![String::new(); alto],
        };
        salida.push(Column::new(nombre.as_str().into(), valores));
    }

    let df_salida = DataFrame::new_infer_height(salida)?;
    commerce_core::limpiar_mojibake(df_salida, None).map_err(Into::into)
}

#[cfg(test)]
#[allow(clippy::invisible_characters)] // el soft-hyphen U+00AD es el mojibake real que se está probando
mod tests {
    use super::*;

    #[test]
    fn normalizar_rellena_faltantes_y_mapea_nulos() -> CoreResult<()> {
        let df = df!("A" => ["1", "NaN", "null"], "B" => ["x", "y", "z"])?;
        let salida = normalizar(&df, &["A".to_string(), "B".to_string(), "C".to_string()])?;
        assert_eq!(
            salida.column("A")?.str()?.iter().collect::<Vec<_>>(),
            vec![Some("1"), Some(""), Some("")]
        );
        assert_eq!(
            salida.column("C")?.str()?.iter().collect::<Vec<_>>(),
            vec![Some(""), Some(""), Some("")]
        );
        Ok(())
    }

    #[test]
    fn normalizar_repara_mojibake() -> CoreResult<()> {
        let df = df!("A" => ["BujÃ­a"])?;
        let salida = normalizar(&df, &["A".to_string()])?;
        assert_eq!(salida.column("A")?.str()?.get(0), Some("Bujía"));
        Ok(())
    }
}
