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
        // Camino rápido: si la columna YA es texto, no trae nulos y ninguna
        // celda es un token nulo, entonces normalizarla la dejaría idéntica.
        // Reusarla evita recorrer y copiar todas sus celdas — en datos reales
        // es el caso mayoritario, y era el 73 % del tiempo del pipeline.
        //
        // La verificación es una sola pasada de LECTURA; el camino largo hace
        // esa pasada más una de escritura más la asignación del buffer.
        if let Ok(col) = df.column(nombre) {
            if col.dtype() == &DataType::String {
                let serie = col.as_materialized_series();
                let texto = serie.str()?;
                if texto.null_count() == 0 && !texto.iter().flatten().any(es_token_nulo) {
                    salida.push(col.clone());
                    continue;
                }
            }
        }

        // Se escribe directo al buffer contiguo de Arrow. La versión anterior
        // asignaba un `String` por celda y armaba un `Vec<String>` que polars
        // volvía a copiar entero al convertirlo: tres pasadas sobre los mismos
        // datos. Medido, esa reconstrucción era el 92 % del costo de
        // `normalizar`, que a su vez era el 84 % del pipeline completo.
        let mut constructor = StringChunkedBuilder::new(nombre.as_str().into(), alto);
        match df.column(nombre) {
            Ok(col) => {
                let serie = col.as_materialized_series().clone();
                let serie = if serie.dtype() == &DataType::String {
                    serie
                } else {
                    serie.cast(&DataType::String)?
                };
                for valor in serie.str()?.iter() {
                    // El texto válido se copia del origen al destino sin pasar
                    // por un `String` propio.
                    match valor {
                        Some(s) if !es_token_nulo(s) => constructor.append_value(s),
                        _ => constructor.append_value(""),
                    }
                }
            }
            // Columna ausente en este chunk: vacía (no nula).
            Err(_) => {
                for _ in 0..alto {
                    constructor.append_value("");
                }
            }
        }
        salida.push(constructor.finish().into_series().into());
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
