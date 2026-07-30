use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use polars::prelude::*;

/// Caracteres que delatan texto mal decodificado. Si una cadena no contiene
/// ninguno, no hay nada que reparar: nos saltamos el cálculo por completo.
pub const PISTAS_MOJIBAKE: &[char] = &['Ã', 'Â', 'â', 'Ð', 'Ñ', 'Å', '\u{FFFD}', '€', '™'];

fn contiene_pista(s: &str) -> bool {
    s.chars().any(|c| PISTAS_MOJIBAKE.contains(&c))
}

/// Intenta reparar un mojibake de una sola capa: texto UTF-8 que fue
/// mal-decodificado como Windows-1252 (el patrón dominante en la práctica:
/// 'Ã­' en vez de 'í', etc.). Reencodea a bytes Windows-1252 y los vuelve a
/// decodificar como UTF-8; si el resultado es válido y distinto, es la
/// reparación.
///
/// Nota: cubre específicamente el caso "UTF-8 leído como cp1252", que es el
/// patrón real de los datos de este proyecto y el de los tests. No intenta
/// resolver otros pares de códec, entidades HTML, comillas tipográficas, etc.
fn reparar_uno(s: &str) -> Option<String> {
    let (bytes, _encoding, tuvo_no_mapeables) = encoding_rs::WINDOWS_1252.encode(s);
    if tuvo_no_mapeables {
        return None;
    }
    match String::from_utf8(bytes.into_owned()) {
        Ok(reparado) if reparado != s => Some(reparado),
        _ => None,
    }
}

/// Aplica la reparación SOLO a los valores únicos con indicios de mojibake.
pub fn arreglar_serie(serie: &Series) -> PolarsResult<Series> {
    let ca = serie.str()?;

    let mut vistos: HashSet<String> = HashSet::new();
    let mut mapeo: HashMap<String, String> = HashMap::new();
    for valor in ca.iter().flatten() {
        if contiene_pista(valor) && vistos.insert(valor.to_string()) {
            if let Some(reparado) = reparar_uno(valor) {
                mapeo.insert(valor.to_string(), reparado);
            }
        }
    }
    if mapeo.is_empty() {
        return Ok(serie.clone());
    }

    let reparada: StringChunked = ca.apply_values(|valor: &str| -> Cow<'_, str> {
        match mapeo.get(valor) {
            Some(nuevo) => Cow::Owned(nuevo.clone()),
            None => Cow::Borrowed(valor),
        }
    });
    Ok(reparada.into_series())
}

/// Repara mojibake en las columnas de texto indicadas (o en todas).
pub fn limpiar_mojibake(mut df: DataFrame, columnas: Option<&[&str]>) -> PolarsResult<DataFrame> {
    let objetivo: Vec<String> = match columnas {
        None => df
            .columns()
            .iter()
            .filter(|c| c.dtype() == &DataType::String)
            .map(|c| c.name().to_string())
            .collect(),
        Some(cols) => {
            let mut vistos = HashSet::new();
            let mut out = Vec::new();
            for &c in cols {
                if vistos.insert(c) && df.column(c).is_ok_and(|col| col.dtype() == &DataType::String) {
                    out.push(c.to_string());
                }
            }
            out
        }
    };
    if objetivo.is_empty() {
        return Ok(df);
    }
    for nombre in &objetivo {
        let serie = df.column(nombre)?.as_materialized_series().clone();
        let reparada = arreglar_serie(&serie)?.with_name(PlSmallStr::from(nombre.clone()));
        df.with_column(reparada.into())?;
    }
    Ok(df)
}

#[cfg(test)]
#[allow(clippy::invisible_characters)] // el soft-hyphen U+00AD es el mojibake real que se está probando
mod tests {
    use super::*;

    #[test]
    fn arreglar_serie_repara_mojibake() -> PolarsResult<()> {
        let serie = Series::new(
            "s".into(),
            [Some("limpio"), Some("BujÃ­a NGK"), None, Some("BujÃ­a NGK")],
        );
        let reparada = arreglar_serie(&serie)?;
        let ca = reparada.str()?;
        let valores: Vec<Option<&str>> = ca.iter().collect();
        assert_eq!(
            valores,
            vec![Some("limpio"), Some("Bujía NGK"), None, Some("Bujía NGK")]
        );
        Ok(())
    }

    #[test]
    fn limpiar_mojibake_solo_columnas_de_texto() -> PolarsResult<()> {
        let df = df!("T" => ["BujÃ­a"], "N" => [5i32])?;
        let limpio = limpiar_mojibake(df, None)?;
        let ca = limpio.column("T")?.str()?;
        assert_eq!(ca.get(0), Some("Bujía"));
        Ok(())
    }
}
