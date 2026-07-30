use std::collections::HashSet;

use polars::prelude::*;

use crate::error::CoreResult;

/// Alinea `df` a `columnas`: si ya coincide exactamente (mismo orden, mismos
/// nombres), lo devuelve tal cual sin tocar nada. Si no: columnas SOBRANTES
/// (en `df` pero no en `columnas`) se descartan, avisando la primera vez que
/// se ve cada una (rastreado en `extras_avisadas`, para no repetir el aviso
/// en cada bloque); columnas FALTANTES se rellenan con `None`. Devuelve `df`
/// recortado/reordenado a exactamente `columnas`.
///
/// Compartida entre `EscritorXlsx::alinear` y `EscritorCsv::alinear`, que
/// antes implementaban la MISMA lógica casi línea por línea — divergían
/// solo en el mensaje de aviso (uno menciona la hoja, el otro no), que por
/// eso queda a cargo de `avisar` (recibe los nombres nuevos, no un mensaje
/// ya armado).
pub(crate) fn alinear_columnas(
    df: &DataFrame,
    columnas: &[String],
    extras_avisadas: &mut HashSet<String>,
    avisar: &mut dyn FnMut(&[String]),
) -> CoreResult<DataFrame> {
    let actuales: Vec<String> = df.get_column_names().iter().map(|s| s.to_string()).collect();
    if actuales.as_slice() == columnas {
        return Ok(df.clone());
    }

    let set_columnas: HashSet<&str> = columnas.iter().map(|s| s.as_str()).collect();
    let nuevas: Vec<String> = actuales
        .iter()
        .filter(|c| !set_columnas.contains(c.as_str()) && !extras_avisadas.contains(*c))
        .cloned()
        .collect();
    if !nuevas.is_empty() {
        for c in &nuevas {
            extras_avisadas.insert(c.clone());
        }
        avisar(&nuevas);
    }

    let set_actuales: HashSet<&str> = actuales.iter().map(|s| s.as_str()).collect();
    let mut df = df.clone();
    for faltante in columnas.iter().filter(|c| !set_actuales.contains(c.as_str())) {
        let nula: Vec<Option<&str>> = vec![None; df.height()];
        df.with_column(Column::new(faltante.as_str().into(), nula))?;
    }
    Ok(df.select(columnas.iter().map(|s| s.as_str()))?)
}
