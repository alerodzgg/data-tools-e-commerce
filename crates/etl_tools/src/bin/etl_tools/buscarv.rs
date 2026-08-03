//! Modos «BUSCARV» (exacto) y «BUSCARV parcial» (por palabra completa).

use std::path::Path;

use commerce_core::total_filas;
use polars::prelude::*;

use crate::comunes::{excluir_refs, Etiquetada};
use crate::cruce::{
    cerrar_cruce, cruzar_y_escribir, preguntar_accion, preparar_cruce, ruta_cruce, AccionCruce, Entrada,
    Salida,
};
use etl_tools::Busqueda;

use crate::AppResult;

// ════════════════════════════════════════════════════════════════════════
// Modo — BUSCARV
// ════════════════════════════════════════════════════════════════════════

pub(crate) fn buscarv_modo(ruta_salida: &Path) -> AppResult<()> {
    app_shell::mostrar_subcabecera("BUSCARV — traer columnas de otro archivo (VLOOKUP)");
    let Some(prep) = preparar_cruce(
        "Columna CLAVE en el archivo BASE (por dónde cruzar):",
        "Columna CLAVE en el archivo TABLA (la que se compara contra la Base):",
    )?
    else {
        return Ok(());
    };

    let unir_multiples = app_shell::menu_seleccionar(
        "Si una clave tiene VARIAS coincidencias en la Tabla:",
        vec![
            Etiquetada::nueva(false, "Solo la primera (como Excel BUSCARV)"),
            Etiquetada::nueva(true, "Unir todas con salto de línea"),
        ],
    )?
    .is_some_and(|o| o.valor);

    let accion = preguntar_accion(&prep.base, None)?;

    let (salida_traer, renombrar) = etl_tools::renombrar_traidas(&prep.cols_base, &prep.columnas_traer);
    let (viejos, nuevos) = etl_tools::pares_de_renombre(&renombrar);

    app_shell::info("Cargando la tabla de búsqueda...");
    let mut lookup = etl_tools::cargar_tabla_busqueda(
        &prep.tabla,
        &prep.clave_tabla,
        &prep.columnas_traer,
        unir_multiples,
        Some(&excluir_refs(&prep.excluir_tabla)),
        app_shell::warn,
    )?
    .lazy()
    .rename(viejos, nuevos, true)
    .collect()?;
    if lookup.height() == 0 {
        app_shell::warn("La Tabla no tiene claves usables. Nada que cruzar.");
        return Ok(());
    }
    app_shell::info(&format!("Tabla: {} claves distintas cargadas.", lookup.height()));
    let alto = lookup.height();
    lookup.with_column(Column::new("_hit".into(), vec![true; alto]))?;

    let solo_match = accion == AccionCruce::SoloReporte;
    let ruta = ruta_cruce(accion, &prep.base, ruta_salida, "");
    let refs_base = excluir_refs(&prep.excluir_base);
    let total = total_filas(
        std::slice::from_ref(&prep.base),
        Some(&refs_base),
        app_shell::warn,
    )
    .unwrap_or(0);
    let barra = app_shell::barra_progreso(
        &format!(
            "Cruzando {}",
            prep.base.file_name().unwrap_or_default().to_string_lossy()
        ),
        total,
    );

    let resultado = etl_tools::buscarv(
        &etl_tools::Fuente {
            archivo: &prep.base,
            columnas: &prep.cols_base,
            columna_clave: &prep.clave_base,
            excluir: Some(&refs_base),
        },
        &lookup,
        &salida_traer,
        solo_match,
        &ruta,
        app_shell::warn,
        |n| barra.inc(n),
    );
    barra.finish_and_clear();
    // `buscarv` ya abortó su propio escritor (contra su ruta REAL) si falló.
    let (total_escrito, filas_match, filas_total, ruta_real) = resultado?;

    cerrar_cruce(
        accion,
        &ruta_real,
        &prep.base,
        total_escrito as u64,
        filas_match,
        filas_total,
    )
}

// ════════════════════════════════════════════════════════════════════════
// Modo — BUSCARV parcial
// ════════════════════════════════════════════════════════════════════════

pub(crate) fn buscarv_parcial_modo(ruta_salida: &Path) -> AppResult<()> {
    app_shell::mostrar_subcabecera("BUSCARV PARCIAL — por contención de palabra completa");
    let Some(prep) = preparar_cruce(
        "Columna de la BASE donde buscar (el texto largo):",
        "Columna CLAVE de la TABLA (el término a buscar dentro de la Base):",
    )?
    else {
        return Ok(());
    };

    let Some(opcion) = app_shell::menu_seleccionar_nav(
        "Si VARIAS claves de la Tabla aparecen en el mismo texto de la Base:",
        vec![
            Etiquetada::nueva(
                etl_tools::OpcionMultiple::Larga,
                "Traer la más larga (más específica)",
            ),
            Etiquetada::nueva(
                etl_tools::OpcionMultiple::Primera,
                "Traer la primera (orden de la Tabla)",
            ),
            Etiquetada::nueva(
                etl_tools::OpcionMultiple::Todas,
                "Traer todas, unidas con salto de línea",
            ),
        ],
    )?
    .map(|o| o.valor) else {
        return Ok(());
    };

    let unir_multiples = app_shell::menu_seleccionar(
        "Si una MISMA clave está repetida en la Tabla (varias filas):",
        vec![
            Etiquetada::nueva(false, "Solo la primera"),
            Etiquetada::nueva(true, "Unir sus valores con salto de línea"),
        ],
    )?
    .is_some_and(|o| o.valor);

    let accion = preguntar_accion(&prep.base, None)?;
    let (salida_traer, renombrar) = etl_tools::renombrar_traidas(&prep.cols_base, &prep.columnas_traer);
    let (viejos, nuevos) = etl_tools::pares_de_renombre(&renombrar);

    app_shell::info("Cargando la tabla y construyendo el buscador (Aho-Corasick)...");
    let lookup = etl_tools::cargar_tabla_parcial(
        &prep.tabla,
        &prep.clave_tabla,
        &prep.columnas_traer,
        unir_multiples,
        Some(&excluir_refs(&prep.excluir_tabla)),
        app_shell::warn,
    )?
    .lazy()
    .rename(viejos, nuevos, true)
    .collect()?;
    if lookup.height() == 0 {
        app_shell::warn("La Tabla no tiene claves usables. Nada que cruzar.");
        return Ok(());
    }
    let (ac, claves_originales) = etl_tools::construir_automata(&lookup)?;
    app_shell::info(&format!("Tabla: {} claves distintas.", lookup.height()));

    let solo_match = accion == AccionCruce::SoloReporte;
    let ruta = ruta_cruce(accion, &prep.base, ruta_salida, "parcial_");
    let refs_base = excluir_refs(&prep.excluir_base);
    let total = total_filas(
        std::slice::from_ref(&prep.base),
        Some(&refs_base),
        app_shell::warn,
    )
    .unwrap_or(0);
    let columnas_salida: Vec<String> = salida_traer
        .iter()
        .cloned()
        .chain(prep.cols_base.iter().cloned())
        .collect();

    let (escritor_total, filas_match, ruta_real) = cruzar_y_escribir(
        &Entrada {
            archivo: &prep.base,
            refs: &refs_base,
            columnas: &prep.cols_base,
            clave: &prep.clave_base,
        },
        &Busqueda {
            lookup: &lookup,
            ac: &ac,
            claves_originales: &claves_originales,
            salida_traer: &salida_traer,
            opcion,
        },
        &Salida {
            columnas: &columnas_salida,
            solo_match,
            ruta: &ruta,
            total,
            etiqueta_barra: &format!(
                "Cruzando {}",
                prep.base.file_name().unwrap_or_default().to_string_lossy()
            ),
        },
    )?;

    cerrar_cruce(accion, &ruta_real, &prep.base, escritor_total, filas_match, total)
}
