use std::collections::HashSet;
use std::sync::LazyLock;

use polars::prelude::*;
use regex::Regex;

/// Fila de cabecera → nombres de columna CANÓNICOS.
///
/// · Celda vacía        → `Columna_N`, con N = posición 1-based (como en Excel).
/// · Nombres repetidos  → sufijos `_1`, `_2`, …
///
/// Es la ÚNICA convención del proyecto. Sin ella, una columna con cabecera vacía
/// o duplicada se pierde EN SILENCIO al alinear los datos contra la unión de
/// columnas.
pub fn nombres_cabecera<I, S>(valores: I) -> Vec<String>
where
    I: IntoIterator<Item = Option<S>>,
    S: AsRef<str>,
{
    let mut nombres: Vec<String> = Vec::new();
    let mut vistos: HashSet<String> = HashSet::new();

    for (i, valor) in valores.into_iter().enumerate() {
        let texto = valor.as_ref().map(|s| s.as_ref().trim()).unwrap_or("");
        let base = if texto.is_empty() {
            format!("Columna_{}", i + 1)
        } else {
            texto.to_string()
        };
        let mut nombre = base.clone();
        let mut k = 1u64;
        while vistos.contains(&nombre) {
            nombre = format!("{base}_{k}");
            k += 1;
        }
        vistos.insert(nombre.clone());
        nombres.push(nombre);
    }
    nombres
}

// calamine nombra `__UNNAMED__{idx}` (0-based) las cabeceras vacías.
static RE_SIN_NOMBRE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^__UNNAMED__(\d+)$").unwrap());

/// Renombra las columnas `__UNNAMED__N` (0-based) a `Columna_{N+1}` (1-based).
///
/// Así los nombres que produce el LECTOR coinciden con los de `nombres_cabecera`.
/// Si no coincidieran, los datos de esas columnas se perderían al alinear.
pub fn renombrar_canonico(df: DataFrame) -> PolarsResult<DataFrame> {
    let columnas: Vec<String> = df.get_column_names().iter().map(|s| s.to_string()).collect();
    let existentes: HashSet<&str> = columnas.iter().map(|s| s.as_str()).collect();

    let mut df = df;
    for columna in &columnas {
        if let Some(cap) = RE_SIN_NOMBRE.captures(columna) {
            let n: u64 = cap[1].parse().unwrap();
            let canonico = format!("Columna_{}", n + 1);
            if !existentes.contains(canonico.as_str()) {
                df.rename(columna, canonico.into())?;
            }
        }
    }
    Ok(df)
}

/// Columnas del archivo que chocan con nombres que la herramienta GENERA.
///
/// Quien llama decide qué hacer (abortar, avisar). Sin este guard, la
/// herramienta las sobrescribía en silencio y corrompía los datos.
pub fn columnas_reservadas_en<'a>(
    columnas: impl IntoIterator<Item = &'a str>,
    reservadas: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let reservadas: HashSet<&str> = reservadas.into_iter().collect();
    columnas
        .into_iter()
        .filter(|c| reservadas.contains(c))
        .map(|c| c.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nombres_cabecera_canonicos() {
        let valores: Vec<Option<&str>> = vec![Some("Sku"), None, Some("Precio"), Some("Precio"), Some("")];
        assert_eq!(
            nombres_cabecera(valores),
            vec!["Sku", "Columna_2", "Precio", "Precio_1", "Columna_5"]
        );
    }

    #[test]
    fn renombrar_canonico_de_lector() -> PolarsResult<()> {
        let df = df!(
            "Sku" => ["a"],
            "__UNNAMED__1" => ["b"],
            "__UNNAMED__4" => ["c"],
        )?;
        let df = renombrar_canonico(df)?;
        let nombres: Vec<&str> = df.get_column_names().iter().map(|s| s.as_str()).collect();
        assert_eq!(nombres, vec!["Sku", "Columna_2", "Columna_5"]);
        Ok(())
    }

    #[test]
    fn columnas_reservadas() {
        assert_eq!(
            columnas_reservadas_en(["Sku", "Precio2"], ["Precio2", "Sku"]),
            vec!["Sku", "Precio2"]
        );
        assert_eq!(columnas_reservadas_en(["Sku"], ["Otra"]), Vec::<String>::new());
    }
}
