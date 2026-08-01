use std::collections::HashSet;

use commerce_core::CoreResult;
use polars::prelude::*;

/// Itera (hoja por hoja, ya en `String`) todos los `archivos`, sin detección
/// de color. Un CSV se lee ENTERO como un único chunk (a diferencia de DATA
/// combinator, que lo batchea).
///
/// `excluir`: nombres de hoja (minúsculas/recortadas) a saltar (solo XLSX).
pub fn iter_hojas_valores(
    archivos: &[std::path::PathBuf],
    excluir: Option<&[&str]>,
    mut avisar: impl FnMut(&str),
) -> CoreResult<Vec<DataFrame>> {
    let mut salida = Vec::new();
    for archivo in archivos {
        let extension = archivo
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if extension == "csv" {
            let Some(ruta_str) = archivo.to_str() else {
                continue;
            };
            match LazyCsvReader::new(PlRefPath::from(ruta_str))
                .with_infer_schema_length(Some(0))
                .with_ignore_errors(true)
                .finish()
                .and_then(polars::prelude::LazyFrame::collect)
            {
                Ok(df) if df.height() > 0 => salida.push(df),
                Ok(_) => {}
                Err(error) => avisar(&format!(
                    "Error al leer '{}': {error}",
                    archivo
                        .file_name()
                        .map(|n| n.to_string_lossy())
                        .unwrap_or_default()
                )),
            }
            continue;
        }
        let hojas = commerce_core::iter_hojas_xlsx(archivo, excluir, &mut avisar);
        salida.extend(hojas);
    }
    Ok(salida)
}

/// Alinea el chunk a la unión de columnas (rellena faltantes), CONSERVA las
/// columnas extra que ya traiga (p. ej. auxiliares `_font_color_*`),
/// normaliza a texto y repara mojibake.
pub fn preparar_chunk(chunk: &DataFrame, columnas_union: &[String]) -> CoreResult<DataFrame> {
    let existentes: HashSet<&str> = chunk.get_column_names().iter().map(|s| s.as_str()).collect();
    let mut df = chunk.clone();
    for c in columnas_union {
        if !existentes.contains(c.as_str()) {
            let nulos: Vec<Option<String>> = vec![None; df.height()];
            df.with_column(Column::new(c.as_str().into(), nulos))?;
        }
    }
    let extras: Vec<String> = df
        .get_column_names()
        .iter()
        .map(|s| s.to_string())
        .filter(|c| !columnas_union.contains(c))
        .collect();
    let orden: Vec<&str> = columnas_union
        .iter()
        .map(|s| s.as_str())
        .chain(extras.iter().map(|s| s.as_str()))
        .collect();
    let mut df = df.select(orden)?;

    for nombre in df.get_column_names_owned() {
        let col = df.column(nombre.as_str())?;
        if col.dtype() != &DataType::String {
            let serie = col.as_materialized_series().cast(&DataType::String)?;
            df.with_column(serie.with_name(nombre.clone()).into())?;
        }
    }
    commerce_core::limpiar_mojibake(df, None).map_err(Into::into)
}

/// Igual que [`preparar_chunk`], pero para los modos que trabajan por
/// columna CLAVE: alinea a `columnas` (sin conservar extras) y garantiza que
/// `columna_clave` exista, aunque no esté en `columnas`.
pub fn preparar_chunk_clave(
    chunk: &DataFrame,
    columnas: &[String],
    columna_clave: &str,
) -> CoreResult<DataFrame> {
    let existentes: HashSet<&str> = chunk.get_column_names().iter().map(|s| s.as_str()).collect();
    let mut df = chunk.clone();
    for c in columnas {
        if !existentes.contains(c.as_str()) {
            let nulos: Vec<Option<String>> = vec![None; df.height()];
            df.with_column(Column::new(c.as_str().into(), nulos))?;
        }
    }
    if !columnas.iter().any(|c| c == columna_clave) && !existentes.contains(columna_clave) {
        let nulos: Vec<Option<String>> = vec![None; df.height()];
        df.with_column(Column::new(columna_clave.into(), nulos))?;
    }
    let df = df.select(columnas.iter().map(|s| s.as_str()))?;
    commerce_core::limpiar_mojibake(df, None).map_err(Into::into)
}

/// Agrega la columna de clave normalizada `nombre_clave` a `preparado` (ya
/// pasado por [`preparar_chunk_clave`]) y descarta las filas cuya clave
/// normalizada quedó vacía (celdas vacías o solo símbolos no cruzan).
/// `None` si no queda ninguna fila. Compartido entre BUSCARV exacto
/// (`clave_limpia_de`) y BUSCARV parcial (`norm_parcial`), que antes
/// repetían este mismo patrón (calcular clave → agregar columna → enmascarar
/// vacíos → filtrar) de forma casi idéntica, divergiendo solo en el nombre
/// de columna y la función de normalización.
pub(crate) fn con_clave_normalizada(
    preparado: &DataFrame,
    clave_tabla: &str,
    columnas_traer: &[String],
    nombre_clave: &str,
    normalizar: impl Fn(Option<&str>) -> String,
) -> CoreResult<Option<DataFrame>> {
    let claves: Vec<String> = columna_texto_o_vacia(preparado, clave_tabla)?
        .iter()
        .map(|v| normalizar(v.as_deref()))
        .collect();
    let mut sub = preparado.select(columnas_traer.iter().map(|s| s.as_str()))?;
    sub.with_column(Column::new(nombre_clave.into(), claves))?;
    let mascara = BooleanChunked::from_iter_values(
        "m".into(),
        sub.column(nombre_clave)?
            .str()?
            .iter()
            .map(|v| !v.unwrap_or("").is_empty()),
    );
    let sub = sub.filter(&mascara)?;
    Ok((sub.height() > 0).then_some(sub))
}

/// Valores de `nombre` como texto (o todo `None` si la columna no existe en
/// este chunk): evita que los modos revienten cuando una hoja no trae la
/// columna clave/objetivo.
pub fn columna_texto_o_vacia(df: &DataFrame, nombre: &str) -> CoreResult<Vec<Option<String>>> {
    match df.column(nombre) {
        Ok(col) => {
            let serie = col.as_materialized_series().clone();
            let serie = if serie.dtype() == &DataType::String {
                serie
            } else {
                serie.cast(&DataType::String)?
            };
            Ok(serie.str()?.iter().map(|v| v.map(str::to_string)).collect())
        }
        Err(_) => Ok(vec![None; df.height()]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparar_chunk_rellena_faltantes_y_conserva_extras() -> CoreResult<()> {
        let chunk = df!("A" => ["1"], "Extra" => ["x"])?;
        let alineado = preparar_chunk(&chunk, &["A".to_string(), "B".to_string()])?;
        assert_eq!(
            alineado
                .get_column_names()
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "B", "Extra"]
        );
        assert_eq!(alineado.column("B")?.str()?.get(0), None);
        assert_eq!(alineado.column("Extra")?.str()?.get(0), Some("x"));
        Ok(())
    }

    #[test]
    fn preparar_chunk_clave_asegura_columna_clave() -> CoreResult<()> {
        let chunk = df!("A" => ["1"])?;
        let alineado = preparar_chunk_clave(&chunk, &["A".to_string()], "Sku")?;
        // "Sku" no pedida en `columnas`, así que no aparece en la salida (se
        // seleccionan solo `columnas`); solo se garantiza que EXISTA antes del
        // select para no reventar si el llamador la referencia después.
        assert_eq!(
            alineado
                .get_column_names()
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
            vec!["A"]
        );
        Ok(())
    }
}
