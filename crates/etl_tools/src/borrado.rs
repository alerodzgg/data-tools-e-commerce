use std::ops::Not;

use commerce_core::CoreResult;
use polars::prelude::*;

/// Aplica la lógica multi-columna de borrado con desplazamiento:
///
/// - Si TODAS las celdas seleccionadas coinciden        → se elimina la fila.
/// - Si ALGUNAS coinciden                                → se vacían esas
///   celdas y los valores supervivientes se compactan hacia la izquierda.
/// - Si tras compactar no queda nada                     → se elimina la fila.
/// - Si ninguna coincide                                 → la fila se
///   conserva intacta (sin recortar ni tocar nada).
///
/// `coincide(columna, valor_de_esa_celda)` decide si la celda debe borrarse
/// (palabra encontrada, color objetivo, etc.).
///
/// Aquí se recorre cada fila directamente en vez de usar expresiones
/// vectorizadas de Polars (listas, `concat_list`, `list.get`): un bucle
/// nativo en Rust no paga el coste que la vectorización evita en otros
/// entornos, y un recorrido por filas es más simple de leer y auditar que
/// la maquinaria de listas de Polars para esta lógica.
#[allow(clippy::needless_range_loop)]
pub fn procesar_columnas_con_desplazamiento(
    df: &DataFrame,
    columnas: &[String],
    coincide: impl Fn(&str, Option<&str>) -> bool,
    motivo_todas: &str,
    motivo_vacias: &str,
) -> CoreResult<(DataFrame, DataFrame)> {
    let columnas: Vec<String> = columnas
        .iter()
        .filter(|c| df.column(c.as_str()).is_ok())
        .cloned()
        .collect();
    if columnas.is_empty() {
        return Ok((df.clone(), DataFrame::empty()));
    }
    let n_cols = columnas.len();
    let n = df.height();

    let valores: Vec<Vec<Option<String>>> = columnas
        .iter()
        .map(|c| -> CoreResult<Vec<Option<String>>> {
            let serie = df.column(c)?.as_materialized_series().clone();
            let serie = if serie.dtype() == &DataType::String {
                serie
            } else {
                serie.cast(&DataType::String)?
            };
            Ok(serie.str()?.iter().map(|v| v.map(str::to_string)).collect())
        })
        .collect::<CoreResult<_>>()?;

    let mut es_valida = Vec::with_capacity(n);
    let mut motivo_por_fila: Vec<Option<String>> = Vec::with_capacity(n);
    // Solo se rellena para las filas VÁLIDAS y TOCADAS (con alguna coincidencia).
    let mut desplazado_por_fila: Vec<Option<Vec<String>>> = Vec::with_capacity(n);

    for fila in 0..n {
        let mut n_coincidencias = 0usize;
        let mut supervivientes: Vec<String> = Vec::with_capacity(n_cols);
        for (ci, nombre) in columnas.iter().enumerate() {
            let valor = valores[ci][fila].as_deref();
            if coincide(nombre, valor) {
                n_coincidencias += 1;
            } else if let Some(texto) = valor.map(str::trim).filter(|v| !v.is_empty()) {
                supervivientes.push(texto.to_string());
            }
        }

        if n_coincidencias > 0 && supervivientes.is_empty() {
            let motivo = if n_coincidencias == n_cols {
                motivo_todas
            } else {
                motivo_vacias
            };
            es_valida.push(false);
            motivo_por_fila.push(Some(motivo.to_string()));
            desplazado_por_fila.push(None);
        } else {
            es_valida.push(true);
            motivo_por_fila.push(None);
            if n_coincidencias > 0 {
                supervivientes.resize(n_cols, String::new());
                desplazado_por_fila.push(Some(supervivientes));
            } else {
                desplazado_por_fila.push(None); // fila intacta: se conserva el valor original
            }
        }
    }

    let mascara_validas = BooleanChunked::from_iter_values("m".into(), es_valida.iter().copied());
    let mascara_borradas: BooleanChunked = mascara_validas.clone().not();

    let mut df_validas = df.filter(&mascara_validas)?;
    for (ci, nombre) in columnas.iter().enumerate() {
        let valores_finales: Vec<Option<String>> = (0..n)
            .filter(|&fila| es_valida[fila])
            .map(|fila| match &desplazado_por_fila[fila] {
                Some(vals) => Some(vals[ci].clone()),
                None => valores[ci][fila].clone(),
            })
            .collect();
        df_validas.with_column(Column::new(nombre.as_str().into(), valores_finales))?;
    }

    let mut df_borradas = df.filter(&mascara_borradas)?;
    let motivos: Vec<String> = (0..n)
        .filter(|&fila| !es_valida[fila])
        .filter_map(|fila| motivo_por_fila[fila].clone())
        .collect();
    df_borradas.with_column(Column::new("Motivo_Borrado".into(), motivos))?;

    Ok((df_validas, df_borradas))
}

/// Genera un reporte textual de los cambios.
pub fn generar_reporte_cambios(total_original: u64, total_borrado: u64, total_final: u64) -> String {
    let pct = if total_original > 0 {
        total_borrado as f64 / total_original as f64 * 100.0
    } else {
        0.0
    };
    format!(
        "\n{sep}\nREPORTE DE CAMBIOS - PROCESAMIENTO COMPLETADO\n{sep}\n\
         Filas originales:    {total_original}\n\
         Filas borradas:      {total_borrado} ({pct:.2}%)\n\
         Filas válidas:       {total_final}\n\
         {sep}\n",
        sep = "=".repeat(60),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coincide_palabra<'a>(palabras: &'a [&'a str]) -> impl Fn(&str, Option<&str>) -> bool + 'a {
        move |_col, valor| match valor {
            Some(v) => palabras
                .iter()
                .any(|p| v.to_lowercase().contains(&p.to_lowercase())),
            None => false,
        }
    }

    #[test]
    fn fila_intacta_si_ninguna_columna_coincide() -> CoreResult<()> {
        let df = df!("A" => ["hola", "manzana"], "B" => ["mundo", "pera"])?;
        let (validas, borradas) = procesar_columnas_con_desplazamiento(
            &df,
            &["A".into(), "B".into()],
            coincide_palabra(&["rojo"]),
            "todas",
            "vacias",
        )?;
        assert_eq!(validas.height(), 2);
        assert_eq!(borradas.height(), 0);
        assert_eq!(
            validas.column("A")?.str()?.iter().collect::<Vec<_>>(),
            vec![Some("hola"), Some("manzana")]
        );
        Ok(())
    }

    #[test]
    fn fila_se_borra_si_todas_las_celdas_coinciden() -> CoreResult<()> {
        let df = df!("A" => ["rojo"], "B" => ["ROJO intenso"])?;
        let (validas, borradas) = procesar_columnas_con_desplazamiento(
            &df,
            &["A".into(), "B".into()],
            coincide_palabra(&["rojo"]),
            "TODAS",
            "VACIAS",
        )?;
        assert_eq!(validas.height(), 0);
        assert_eq!(borradas.height(), 1);
        assert_eq!(borradas.column("Motivo_Borrado")?.str()?.get(0), Some("TODAS"));
        Ok(())
    }

    #[test]
    fn celdas_supervivientes_se_compactan_a_la_izquierda() -> CoreResult<()> {
        // "A" coincide (se vacía), "B" y "C" sobreviven → se compactan a B,C→A,B y C queda "".
        let df = df!("A" => ["rojo"], "B" => ["manzana"], "C" => ["pera"])?;
        let (validas, borradas) = procesar_columnas_con_desplazamiento(
            &df,
            &["A".into(), "B".into(), "C".into()],
            coincide_palabra(&["rojo"]),
            "todas",
            "vacias",
        )?;
        assert_eq!(borradas.height(), 0);
        assert_eq!(validas.height(), 1);
        assert_eq!(validas.column("A")?.str()?.get(0), Some("manzana"));
        assert_eq!(validas.column("B")?.str()?.get(0), Some("pera"));
        assert_eq!(validas.column("C")?.str()?.get(0), Some(""));
        Ok(())
    }

    #[test]
    fn fila_se_borra_si_tras_compactar_no_queda_nada_aunque_no_todas_coincidan() -> CoreResult<()> {
        // "A" coincide, "B" ya estaba vacía: ninguna sobrevive → se borra con motivo_vacias.
        let df = df!("A" => ["rojo"], "B" => [""])?;
        let (validas, borradas) = procesar_columnas_con_desplazamiento(
            &df,
            &["A".into(), "B".into()],
            coincide_palabra(&["rojo"]),
            "todas",
            "vacias",
        )?;
        assert_eq!(validas.height(), 0);
        assert_eq!(borradas.height(), 1);
        assert_eq!(borradas.column("Motivo_Borrado")?.str()?.get(0), Some("vacias"));
        Ok(())
    }

    #[test]
    fn reporte_de_cambios_formatea_porcentaje() {
        let reporte = generar_reporte_cambios(100, 25, 75);
        assert!(reporte.contains("25 (25.00%)"));
        assert!(reporte.contains("Filas válidas:       75"));
    }
}
