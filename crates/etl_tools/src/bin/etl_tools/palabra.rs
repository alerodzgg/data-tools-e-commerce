//! Modo «Borrar por palabra clave».

use std::path::Path;

use crate::comunes::{abortar_si_reservadas, excluir_refs, preguntar_hojas_excluir_de, seleccionar_columnas};
use crate::AppResult;

// ════════════════════════════════════════════════════════════════════════
// Modo — Borrar por palabra
// ════════════════════════════════════════════════════════════════════════

pub(crate) fn procesar_por_palabra(archivo: &Path, ruta_salida: &Path) -> AppResult<()> {
    let Some(excluir) = preguntar_hojas_excluir_de(archivo, "")? else {
        return Ok(());
    };
    let refs = excluir_refs(&excluir);

    // Una sola apertura del archivo para todo lo que sigue (columnas, total
    // de filas y los datos a procesar) — antes `columnas_union`,
    // `total_filas` e `iter_hojas_valores` abrían el mismo archivo por
    // separado, 3 veces, para lo mismo.
    let bloques = etl_tools::iter_hojas_valores(
        std::slice::from_ref(&archivo.to_path_buf()),
        Some(&refs),
        app_shell::warn,
    )?;
    let columnas = commerce_core::columnas_union_de_bloques(&bloques);
    if columnas.is_empty() {
        app_shell::error("No se pudieron leer columnas de los archivos.");
        return Ok(());
    }
    if abortar_si_reservadas(&columnas) {
        return Ok(());
    }

    let columnas_obj = seleccionar_columnas(&columnas, "borrado por palabra")?;
    if columnas_obj.is_empty() {
        app_shell::warn("Sin columnas. Operación cancelada.");
        return Ok(());
    }

    let entrada = app_shell::pedir_texto("Palabras a borrar (separadas por coma, ej: 'rojo,azul'):")?
        .unwrap_or_default();
    let palabras: Vec<String> = entrada
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(regex::escape)
        .collect();
    if palabras.is_empty() {
        app_shell::warn("Sin palabras válidas. Operación cancelada.");
        return Ok(());
    }
    let patron = regex::RegexBuilder::new(&format!("(?:{})", palabras.join("|")))
        .case_insensitive(true)
        .build()
        .expect("patron valido: palabras escapadas");

    let stem = archivo.file_stem().unwrap_or_default().to_string_lossy();
    let total: u64 = bloques.iter().map(|df| df.height() as u64).sum();

    let ruta_proc = commerce_core::ruta_unica(ruta_salida.join(format!("procesado_{stem}.xlsx")));
    let ruta_borr = commerce_core::ruta_unica(ruta_salida.join(format!("borradas_{stem}.xlsx")));
    let cols_borradas: Vec<String> = columnas
        .iter()
        .cloned()
        .chain(["Motivo_Borrado".to_string()])
        .collect();

    let mut escritor_validas = etl_tools::nuevo_escritor(&ruta_proc, columnas.clone())?;
    let mut escritor_borradas = etl_tools::nuevo_escritor(&ruta_borr, cols_borradas.clone())?;
    let barra = app_shell::barra_progreso(&format!("Procesando {stem}"), total);

    for chunk in bloques {
        let filas_entrada = chunk.height() as u64;
        let chunk = etl_tools::preparar_chunk(&chunk, &columnas)?;
        let (validas, borradas) = etl_tools::procesar_columnas_con_desplazamiento(
            &chunk,
            &columnas_obj,
            |_col, valor| valor.map(|v| patron.is_match(v)).unwrap_or(false),
            "Todas las celdas seleccionadas contienen la palabra a eliminar",
            "Todas las celdas seleccionadas quedaron vacías tras el borrado",
        )?;
        if validas.height() > 0 {
            escritor_validas.escribir(&validas.select(&columnas)?, None)?;
        }
        if borradas.height() > 0 {
            escritor_borradas.escribir(&borradas.select(&cols_borradas)?, None)?;
        }
        barra.inc(filas_entrada);
    }
    barra.finish_and_clear();

    if escritor_borradas.total == 0 {
        let _ = std::fs::remove_file(&ruta_borr);
    }
    let (n_validas, n_borradas) = (escritor_validas.total as u64, escritor_borradas.total as u64);

    print!(
        "{}",
        etl_tools::generar_reporte_cambios(n_validas + n_borradas, n_borradas, n_validas)
    );
    app_shell::success(&format!(
        "Guardado: '{}' ({n_validas} filas).",
        ruta_proc.file_name().unwrap_or_default().to_string_lossy()
    ));
    if n_borradas > 0 {
        app_shell::success(&format!(
            "Guardado: '{}' ({n_borradas} filas).",
            ruta_borr.file_name().unwrap_or_default().to_string_lossy()
        ));
    } else {
        app_shell::info("No se borró ninguna fila en esta ejecución.");
    }
    Ok(())
}
