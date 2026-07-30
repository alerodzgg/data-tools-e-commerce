use polars::prelude::*;

use crate::error::CoreResult;

/// Espacios que se recortan de la clave de orden antes de decidir si es
/// número o texto.
pub const ESPACIOS_ORDEN: &[char] = &[
    '\u{09}', '\u{0a}', '\u{0b}', '\u{0c}', '\u{0d}', '\u{20}', '\u{85}', '\u{a0}', '\u{1680}', '\u{2000}',
    '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}', '\u{2008}',
    '\u{2009}', '\u{200a}', '\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}', '\u{3000}',
];

/// Recorta `s` con el mismo set de espacios que usa el orden estilo Excel en
/// todo el proyecto (motor en memoria y mezcla externa).
pub fn recortar_espacios_orden(s: &str) -> &str {
    s.trim_matches(|c: char| ESPACIOS_ORDEN.contains(&c))
}

/// Intenta interpretar `s` (ya recortado) como número finito (excluye
/// 'nan'/'inf'/'-inf', que Excel trata como texto).
pub fn como_numero_finito(s: &str) -> Option<f64> {
    s.parse::<f64>().ok().filter(|f| f.is_finite())
}

/// Ordena `df` por `columna` al estilo Excel:
///
/// - Los NÚMEROS se ordenan por su VALOR (0→9; '2' antes que '10', no
///   lexicográficamente).
/// - El TEXTO se ordena alfabéticamente (A→Z), sin distinguir mayúsculas.
/// - Las celdas VACÍAS quedan SIEMPRE al final, en ambos sentidos.
///
/// Usada tanto por el modo "Ordenar columnas" de ETL tools como por la
/// mezcla externa de DATA combinator: es la MISMA lógica para ambos casos,
/// vive una sola vez para que no puedan divergir.
pub fn ordenar_excel_df(df: &DataFrame, columna: &str, ascendente: bool) -> CoreResult<DataFrame> {
    let serie = df.column(columna)?.as_materialized_series().clone();
    let serie = if serie.dtype() == &DataType::String {
        serie
    } else {
        serie.cast(&DataType::String)?
    };
    let ca = serie.str()?;

    let n = ca.len();
    let mut o_vac = Vec::with_capacity(n);
    let mut o_grp = Vec::with_capacity(n);
    let mut o_num = Vec::with_capacity(n);
    let mut o_txt = Vec::with_capacity(n);

    for valor in ca.iter() {
        let limpio = recortar_espacios_orden(valor.unwrap_or(""));
        let vacio = valor.is_none() || limpio.is_empty();
        let numero = como_numero_finito(limpio);
        o_vac.push(vacio);
        o_grp.push(if numero.is_some() { 0i32 } else { 1i32 });
        o_num.push(numero);
        o_txt.push(limpio.to_lowercase());
    }

    let mut df = df.clone();
    df.with_column(Column::new("_o_vac".into(), o_vac))?;
    df.with_column(Column::new("_o_grp".into(), o_grp))?;
    df.with_column(Column::new("_o_num".into(), o_num))?;
    df.with_column(Column::new("_o_txt".into(), o_txt))?;

    let inv = !ascendente;
    let opciones = SortMultipleOptions::default()
        .with_maintain_order(true)
        .with_nulls_last(true)
        .with_order_descending_multi([false, inv, inv, inv]);
    let ordenado = df.sort(["_o_vac", "_o_grp", "_o_num", "_o_txt"], opciones)?;
    Ok(ordenado.drop_many(["_o_vac", "_o_grp", "_o_num", "_o_txt"]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vacios_al_final_y_orden_numerico() -> CoreResult<()> {
        for ascendente in [true, false] {
            let df = df!("K" => ["b", "", "a", "10", "2"])?;
            let ordenado = ordenar_excel_df(&df, "K", ascendente)?;
            let claves: Vec<Option<String>> = ordenado
                .column("K")?
                .str()?
                .iter()
                .map(|v| v.map(str::to_string))
                .collect();
            assert_eq!(
                claves.last().unwrap().as_deref(),
                Some(""),
                "vacíos siempre al final"
            );
            let numeros: Vec<&str> = claves
                .iter()
                .filter_map(|v| v.as_deref())
                .filter(|v| *v == "2" || *v == "10")
                .collect();
            let esperado: Vec<&str> = if ascendente {
                vec!["2", "10"]
            } else {
                vec!["10", "2"]
            };
            assert_eq!(numeros, esperado);
        }
        Ok(())
    }

    #[test]
    fn texto_alfabetico_sin_distinguir_mayusculas() -> CoreResult<()> {
        let df = df!("K" => ["banana", "Arbol", "cereza"])?;
        let ordenado = ordenar_excel_df(&df, "K", true)?;
        assert_eq!(
            ordenado
                .column("K")?
                .str()?
                .iter()
                .map(|v| v.unwrap())
                .collect::<Vec<_>>(),
            vec!["Arbol", "banana", "cereza"]
        );
        Ok(())
    }
}
