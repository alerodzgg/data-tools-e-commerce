use std::ops::Not;

use commerce_core::CoreResult;
use polars::prelude::*;

use crate::constantes::COL_CARACTERES;
use crate::lectura::columna_texto_o_vacia;

/// Caracteres de cada valor de `columna` (espacios incluidos, sin recortar
/// nada antes de contar: "cuenta los caracteres" es literal).
pub fn contar_caracteres(df: &DataFrame, columna: &str) -> CoreResult<Vec<u32>> {
    let valores = columna_texto_o_vacia(df, columna)?;
    Ok(valores
        .into_iter()
        .map(|v| v.unwrap_or_default().chars().count() as u32)
        .collect())
}

/// Añade `Caracteres` como PRIMERA columna y ordena TODO el archivo por ella
/// (filtro/orden vectorizado por el conteo de cada fila, sin acumular nada
/// entre filas).
pub fn anadir_columna_y_ordenar(df: &DataFrame, columna: &str, ascendente: bool) -> CoreResult<DataFrame> {
    let conteos = contar_caracteres(df, columna)?;
    let mut df = df.clone();
    df.with_column(Column::new(COL_CARACTERES.into(), conteos))?;

    let opciones = SortMultipleOptions::default().with_order_descending(!ascendente);
    let ordenado = df.sort([COL_CARACTERES], opciones)?;

    let mut columnas_orden = vec![COL_CARACTERES.to_string()];
    columnas_orden.extend(
        ordenado
            .get_column_names()
            .iter()
            .map(|s| s.to_string())
            .filter(|c| c != COL_CARACTERES),
    );
    Ok(ordenado.select(columnas_orden)?)
}

/// Divide `df` en (`dentro`, `fuera`) según si `columna` tiene como máximo
/// `limite` caracteres o más. Cada fila se clasifica solo por SU PROPIA
/// longitud: no es empaquetado voraz, es un filtro.
pub fn dividir_dentro_fuera(
    df: &DataFrame,
    columna: &str,
    limite: u32,
) -> CoreResult<(DataFrame, DataFrame)> {
    let conteos = contar_caracteres(df, columna)?;
    let mascara_dentro = BooleanChunked::from_iter_values("m".into(), conteos.iter().map(|c| *c <= limite));
    let mascara_fuera: BooleanChunked = mascara_dentro.clone().not();
    Ok((df.filter(&mascara_dentro)?, df.filter(&mascara_fuera)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn df_de_prueba() -> DataFrame {
        let largos = [
            "corto",
            "medio largo aqui",
            &"z".repeat(60),
            &"y".repeat(5),
            &"w".repeat(40),
            &"x".repeat(3),
            "texto con    espacios",
            &"q".repeat(95),
        ];
        df!("Sku" => (0..8).map(|i| format!("S{i}")).collect::<Vec<_>>(), "Descripcion" => largos).unwrap()
    }

    #[test]
    fn cuenta_espacios_como_caracter() -> CoreResult<()> {
        let df = df!("Texto" => ["texto con    espacios"])?;
        assert_eq!(contar_caracteres(&df, "Texto")?, vec![21]);
        Ok(())
    }

    #[test]
    fn modo_columna_anade_al_inicio_y_ordena() -> CoreResult<()> {
        let df = df_de_prueba();
        let ordenado = anadir_columna_y_ordenar(&df, "Descripcion", true)?;
        assert_eq!(ordenado.get_column_names()[0].as_str(), "Caracteres");
        let conteos: Vec<u32> = ordenado
            .column("Caracteres")?
            .u32()?
            .into_no_null_iter()
            .collect();
        let mut esperado = conteos.clone();
        esperado.sort_unstable();
        assert_eq!(conteos, esperado);
        Ok(())
    }

    #[test]
    fn modo_hojas_ninguna_fila_se_pierde() -> CoreResult<()> {
        let df = df_de_prueba();
        let (dentro, fuera) = dividir_dentro_fuera(&df, "Descripcion", 50)?;
        assert_eq!(dentro.height() + fuera.height(), 8);
        let largos_fuera: Vec<u32> = contar_caracteres(&fuera, "Descripcion")?;
        assert_eq!(
            {
                let mut v = largos_fuera;
                v.sort();
                v
            },
            vec![60, 95]
        );
        Ok(())
    }

    #[test]
    fn modo_reporte_solo_las_que_superan_el_limite() -> CoreResult<()> {
        let df = df_de_prueba();
        let (_dentro, fuera) = dividir_dentro_fuera(&df, "Descripcion", 1000)?;
        assert_eq!(
            fuera.height(),
            0,
            "nada supera un límite altísimo: sin reporte que generar"
        );
        Ok(())
    }
}
