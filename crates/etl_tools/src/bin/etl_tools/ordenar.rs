//! Modo «Ordenar columnas» (A→Z / 0→9, estilo Excel).

use std::path::Path;

use crate::comunes::{
    abortar_si_reservadas, armar_desde_bloques, escribir_por_bloques, excluir_refs,
    preguntar_hojas_excluir_de, seleccionar_archivo,
};
use crate::AppResult;

// ════════════════════════════════════════════════════════════════════════
// Modo — Ordenar columnas
// ════════════════════════════════════════════════════════════════════════

pub(crate) fn ordenar_columna(ruta_salida: &Path) -> AppResult<()> {
    let Some(archivo) = seleccionar_archivo("Selecciona el archivo a ordenar")? else {
        app_shell::warn("No se seleccionó archivo. Volviendo al menú.");
        return Ok(());
    };
    let Some(hojas_excluir) = preguntar_hojas_excluir_de(&archivo, "")? else {
        return Ok(());
    };
    // Una sola apertura del archivo para columnas + datos: pedir las
    // columnas aparte con `columnas_union` abriría el mismo archivo dos
    // veces para lo mismo.
    let bloques = etl_tools::iter_hojas_valores(
        std::slice::from_ref(&archivo),
        Some(&excluir_refs(&hojas_excluir)),
        app_shell::warn,
    )?;
    let columnas = commerce_core::columnas_union_de_bloques(&bloques);
    if columnas.is_empty() {
        app_shell::error(&format!(
            "No se pudieron leer columnas de '{}'.",
            archivo.file_name().unwrap_or_default().to_string_lossy()
        ));
        return Ok(());
    }
    if abortar_si_reservadas(&columnas) {
        return Ok(());
    }

    let Some(columna) = app_shell::menu_seleccionar_nav("¿Qué columna quieres ordenar?", columnas.clone())?
    else {
        app_shell::warn("Operación cancelada.");
        return Ok(());
    };
    let ascendente =
        app_shell::menu_confirmar("Sentido del orden: ¿ascendente (A→Z, 0→9)?", true)?.unwrap_or(true);

    app_shell::info(&format!("Ordenando por '{columna}'..."));
    let Some(df) = armar_desde_bloques(bloques, &columnas)? else {
        app_shell::warn("El archivo está vacío.");
        return Ok(());
    };
    let df = commerce_core::ordenar_excel_df(&df, &columna, ascendente)?;

    let stem = archivo.file_stem().unwrap_or_default().to_string_lossy();
    let ruta = commerce_core::ruta_unica(ruta_salida.join(format!("ordenado_{stem}.xlsx")));
    let mut escritor = etl_tools::nuevo_escritor(&ruta, columnas)?;
    let barra = app_shell::barra_progreso(
        &format!(
            "Escribiendo {}",
            ruta.file_name().unwrap_or_default().to_string_lossy()
        ),
        df.height() as u64,
    );
    escribir_por_bloques(&mut escritor, &df, None, &barra)?;
    barra.finish_and_clear();

    let sentido = if ascendente {
        "ascendente (A→Z, 0→9)"
    } else {
        "descendente (Z→A, 9→0)"
    };
    app_shell::success(&format!(
        "Guardado: '{}' ({} filas, ordenado por '{columna}' {sentido}).",
        ruta.file_name().unwrap_or_default().to_string_lossy(),
        escritor.total
    ));
    Ok(())
}
