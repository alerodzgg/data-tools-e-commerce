//! Modo «Gestionar duplicados»: dentro de un archivo, o entre dos.

use std::path::Path;

use commerce_core::columnas_union;

use crate::comunes::{
    abortar_si_reservadas, excluir_refs, preguntar_hojas_excluir_de, renombrar_o_avisar, seleccionar_archivo,
    seleccionar_columna_clave, Etiquetada,
};
use crate::cruce::{elegir_accion, AccionCruce};
use crate::AppResult;

// ════════════════════════════════════════════════════════════════════════
// Modo — Duplicados
// ════════════════════════════════════════════════════════════════════════

fn buscar_duplicados(ruta_salida: &Path) -> AppResult<()> {
    app_shell::mostrar_subcabecera("BÚSQUEDA DE DUPLICADOS EN UN ARCHIVO");
    let Some(archivo) = seleccionar_archivo("Selecciona el archivo para buscar duplicados")? else {
        app_shell::warn("No se seleccionó archivo. Volviendo al menú.");
        return Ok(());
    };
    let Some(excluir) = preguntar_hojas_excluir_de(&archivo, "")? else {
        return Ok(());
    };
    let refs = excluir_refs(&excluir);

    let columnas = columnas_union(std::slice::from_ref(&archivo), Some(&refs), app_shell::warn);
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
    let Some(columna_clave) = seleccionar_columna_clave(&columnas)? else {
        return Ok(());
    };

    app_shell::info("Detectando claves repetidas...");
    let (claves_repetidas, num_duplicados) =
        etl_tools::claves_repetidas(&archivo, &columna_clave, Some(&refs), app_shell::warn)?;
    app_shell::success(&format!(
        "Análisis completado. {num_duplicados} filas con clave repetida."
    ));
    if num_duplicados == 0 {
        app_shell::info("No se encontraron duplicados internos en el archivo.");
        return Ok(());
    }

    let stem = archivo.file_stem().unwrap_or_default().to_string_lossy();
    let accion = elegir_accion(
        "¿Qué quieres hacer con la versión limpia?",
        [
            format!(
                "Limpiar el archivo actual ('{}') (sobrescribir)",
                archivo.file_name().unwrap_or_default().to_string_lossy()
            ),
            "Crear un nuevo archivo con las filas limpias".to_string(),
            "Solo generar el reporte de duplicados (no limpiar)".to_string(),
        ],
    )?;

    let ruta_dup = commerce_core::ruta_unica(ruta_salida.join(format!("duplicados_internos_{stem}.xlsx")));
    let ruta_limpia = match accion {
        AccionCruce::SobrescribirBase => Some(archivo.with_file_name(format!("{stem}__tmp_limpio.xlsx"))),
        AccionCruce::ArchivoNuevo => Some(commerce_core::ruta_unica(
            ruta_salida.join(format!("{stem}_sin_duplicados.xlsx")),
        )),
        AccionCruce::SoloReporte => None,
    };

    app_shell::info(if ruta_limpia.is_some() {
        "Escribiendo duplicados y versión limpia en una sola pasada..."
    } else {
        "Escribiendo reporte de duplicados..."
    });
    let barra = app_shell::barra_progreso("Escribiendo", 0);
    let resultado = etl_tools::escribir_reporte_y_limpio(
        &etl_tools::Fuente {
            archivo: &archivo,
            columnas: &columnas,
            columna_clave: &columna_clave,
            excluir: Some(&refs),
        },
        &ruta_dup,
        ruta_limpia.as_deref(),
        &claves_repetidas,
        app_shell::warn,
        |n| barra.inc(n),
    );
    barra.finish_and_clear();
    // Ante un error, `escribir_reporte_y_limpio` ya abortó sus propios
    // escritores (contra su ruta REAL, no la que se le pidió) — no hace
    // falta que este llamador adivine qué archivo limpiar.
    let (n_dup, n_limpio, ruta_limpia_real) = resultado?;

    app_shell::success(&format!(
        "Reporte de duplicados: '{}' ({n_dup} filas).",
        ruta_dup.file_name().unwrap_or_default().to_string_lossy()
    ));
    match accion {
        AccionCruce::SobrescribirBase => {
            // Se renombra `ruta_limpia_real`, la ruta donde el escritor
            // escribió de verdad, no la que se le pidió: si esa ya estaba
            // ocupada, la escritura se redirigió a otra, y renombrar la
            // pedida movería un archivo ajeno sobre los datos del usuario.
            if let Some(rl) = &ruta_limpia_real {
                renombrar_o_avisar(rl, &archivo)?;
            }
            app_shell::success(&format!(
                "Archivo original sobrescrito: '{}' ({n_limpio} filas).",
                archivo.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        AccionCruce::ArchivoNuevo => {
            if let Some(rl) = &ruta_limpia_real {
                app_shell::success(&format!(
                    "Nuevo archivo limpio en '{}' ({n_limpio} filas).",
                    rl.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
        AccionCruce::SoloReporte => {
            app_shell::info("Solo se generó el reporte de duplicados (no se limpió nada).");
        }
    }
    Ok(())
}

fn comparar_duplicados(ruta_salida: &Path) -> AppResult<()> {
    app_shell::mostrar_subcabecera("COMPARACIÓN DE DUPLICADOS ENTRE DOS ARCHIVOS");
    let Some(base) = seleccionar_archivo("Selecciona el archivo 'Base'")? else {
        return Ok(());
    };
    let Some(comp) = seleccionar_archivo("Selecciona el archivo de 'Comparación'")? else {
        return Ok(());
    };
    let Some(excluir_base) = preguntar_hojas_excluir_de(&base, "la Base")? else {
        return Ok(());
    };
    let Some(excluir_comp) = preguntar_hojas_excluir_de(&comp, "la Comparación")? else {
        return Ok(());
    };
    let refs_base = excluir_refs(&excluir_base);
    let refs_comp = excluir_refs(&excluir_comp);

    let columnas_base = columnas_union(std::slice::from_ref(&base), Some(&refs_base), app_shell::warn);
    if columnas_base.is_empty() {
        app_shell::error(&format!(
            "No se pudieron leer columnas de '{}'.",
            base.file_name().unwrap_or_default().to_string_lossy()
        ));
        return Ok(());
    }
    if abortar_si_reservadas(&columnas_base) {
        return Ok(());
    }
    let Some(columna_clave) = seleccionar_columna_clave(&columnas_base)? else {
        return Ok(());
    };

    let columnas_comp = columnas_union(std::slice::from_ref(&comp), Some(&refs_comp), app_shell::warn);
    if !columnas_comp.contains(&columna_clave) {
        app_shell::error("La columna clave no existe en el archivo de Comparación.");
        app_shell::info("Ambos archivos deben tener la columna clave con el mismo nombre.");
        return Ok(());
    }
    if abortar_si_reservadas(&columnas_comp) {
        return Ok(());
    }

    app_shell::info("Leyendo claves del archivo de comparación...");
    let claves_comp = etl_tools::claves_unicas(&comp, &columna_clave, Some(&refs_comp), app_shell::warn)?;

    let base_stem = base.file_stem().unwrap_or_default().to_string_lossy();
    let ruta_dup = commerce_core::ruta_unica(ruta_salida.join(format!("duplicados_{base_stem}.xlsx")));
    app_shell::info(&format!(
        "Detectando y escribiendo duplicados en '{}'...",
        ruta_dup.file_name().unwrap_or_default().to_string_lossy()
    ));
    let barra = app_shell::barra_progreso("Comparando", 0);
    let num_duplicados = etl_tools::escribir_filtrado(
        &etl_tools::Fuente {
            archivo: &base,
            columnas: &columnas_base,
            columna_clave: &columna_clave,
            excluir: Some(&refs_base),
        },
        &ruta_dup,
        &claves_comp,
        true,
        app_shell::warn,
        |n| barra.inc(n),
    )?;
    barra.finish_and_clear();

    app_shell::success(&format!(
        "Análisis completado. Se encontraron {num_duplicados} duplicados en Base."
    ));
    if num_duplicados == 0 {
        app_shell::info("No se encontraron duplicados. El archivo base ya está limpio.");
        return Ok(());
    }

    let comp_stem = comp.file_stem().unwrap_or_default().to_string_lossy();
    /// Cuál de los dos archivos del cruce se limpia (o ninguno).
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ALimpiar {
        Base,
        Comparacion,
        Ninguno,
    }
    let opciones = vec![
        Etiquetada::nueva(
            ALimpiar::Base,
            format!(
                "Limpiar archivo Base ('{}')",
                base.file_name().unwrap_or_default().to_string_lossy()
            ),
        ),
        Etiquetada::nueva(
            ALimpiar::Comparacion,
            format!(
                "Limpiar archivo de Comparación ('{}')",
                comp.file_name().unwrap_or_default().to_string_lossy()
            ),
        ),
        Etiquetada::nueva(ALimpiar::Ninguno, "No limpiar ningún archivo"),
    ];
    let eleccion = app_shell::menu_seleccionar_nav("¿Qué quieres limpiar?", opciones)?
        .map_or(ALimpiar::Ninguno, |o| o.valor);

    if eleccion == ALimpiar::Base {
        let ruta_limpia =
            commerce_core::ruta_unica(ruta_salida.join(format!("base_sin_duplicados_{base_stem}.xlsx")));
        let barra = app_shell::barra_progreso("Limpiando", 0);
        let n = etl_tools::escribir_filtrado(
            &etl_tools::Fuente {
                archivo: &base,
                columnas: &columnas_base,
                columna_clave: &columna_clave,
                excluir: Some(&refs_base),
            },
            &ruta_limpia,
            &claves_comp,
            false,
            app_shell::warn,
            |x| barra.inc(x),
        )?;
        barra.finish_and_clear();
        app_shell::success(&format!(
            "Guardado: '{}' ({n} filas).",
            ruta_limpia.file_name().unwrap_or_default().to_string_lossy()
        ));
    } else if eleccion == ALimpiar::Comparacion {
        app_shell::info("Leyendo claves del archivo Base...");
        let claves_base = etl_tools::claves_unicas(&base, &columna_clave, Some(&refs_base), app_shell::warn)?;
        let ruta_limpia = commerce_core::ruta_unica(
            ruta_salida.join(format!("comparacion_sin_duplicados_{comp_stem}.xlsx")),
        );
        let barra = app_shell::barra_progreso("Limpiando", 0);
        let n = etl_tools::escribir_filtrado(
            &etl_tools::Fuente {
                archivo: &comp,
                columnas: &columnas_comp,
                columna_clave: &columna_clave,
                excluir: Some(&refs_comp),
            },
            &ruta_limpia,
            &claves_base,
            false,
            app_shell::warn,
            |x| barra.inc(x),
        )?;
        barra.finish_and_clear();
        app_shell::success(&format!(
            "Guardado: '{}' ({n} filas).",
            ruta_limpia.file_name().unwrap_or_default().to_string_lossy()
        ));
    } else {
        app_shell::info("Operación finalizada. No se ha limpiado ningún archivo.");
    }
    Ok(())
}

pub(crate) fn gestionar_duplicados(ruta_salida: &Path) -> AppResult<()> {
    #[derive(Clone, Copy)]
    enum Sub {
        DentroDeUno,
        EntreDos,
    }
    impl std::fmt::Display for Sub {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Sub::DentroDeUno => write!(f, "Buscar/quitar duplicados dentro de un archivo"),
                Sub::EntreDos => write!(f, "Comparar duplicados entre dos archivos"),
            }
        }
    }
    match app_shell::menu_seleccionar_nav("Gestión de duplicados:", vec![Sub::DentroDeUno, Sub::EntreDos])? {
        Some(Sub::DentroDeUno) => buscar_duplicados(ruta_salida),
        Some(Sub::EntreDos) => comparar_duplicados(ruta_salida),
        None => {
            app_shell::warn("Operación cancelada.");
            Ok(())
        }
    }
}
