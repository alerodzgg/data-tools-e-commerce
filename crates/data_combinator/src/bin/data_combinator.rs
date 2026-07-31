//! Binario interactivo de `data_combinator`: elegir archivos, columnas,
//! formato, división y orden; combinar.

use std::fmt;
use std::path::PathBuf;

use app_shell::{FlujoError, FlujoResult};
use commerce_core::{columnas_union, total_filas, CoreError};
use data_combinator::{
    combinar, Division, Formato, OpcionesCombinar, UmbralesLoteCsv, UmbralesOrden, COLUMNAS_RESERVADAS,
};

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Flujo(#[from] FlujoError),
    #[error(transparent)]
    Core(#[from] CoreError),
}

type AppResult<T> = Result<T, AppError>;

struct ArchivoOpcion(PathBuf);
impl fmt::Display for ArchivoOpcion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.file_name().unwrap_or_default().to_string_lossy())
    }
}

fn listar_archivos(entrada: &std::path::Path) -> Vec<PathBuf> {
    let mut archivos: Vec<PathBuf> = data_combinator::listar_archivos(entrada);
    archivos.sort();
    archivos
}

fn elegir_division(formato: Formato) -> FlujoResult<Division> {
    let mut opciones = vec!["No dividir (una sola salida)".to_string()];
    if formato == Formato::Xlsx {
        opciones.push("En hojas de N filas (dentro del mismo archivo)".to_string());
    }
    opciones.push("En archivos separados de N filas".to_string());

    // `None`/"no dividir" ya es un valor de negocio legítimo: no se ofrece
    // "cancelar" aparte (colisionaría), solo "volver al menú".
    let modo = app_shell::menu_seleccionar_nav("¿Dividir la salida?", opciones)?;
    let Some(modo) = modo else {
        return Ok(Division::Ninguna);
    };
    if modo == "No dividir (una sola salida)" {
        return Ok(Division::Ninguna);
    }
    let es_hojas = modo == "En hojas de N filas (dentro del mismo archivo)";
    let unidad = if es_hojas { "hoja" } else { "archivo" };

    let filas = loop {
        let texto = app_shell::pedir_texto(&format!(
            "¿Cuántas filas por {unidad}? (Enter = {}):",
            data_combinator::FILAS_POR_HOJA
        ))?
        .unwrap_or_default();
        if texto.is_empty() {
            break data_combinator::FILAS_POR_HOJA;
        }
        let limpio: String = texto.chars().filter(|c| !matches!(c, '.' | ',' | ' ')).collect();
        match limpio.parse::<usize>() {
            Ok(n) if n >= 1 => break n,
            _ => app_shell::warn("Escribe un número entero mayor que 0."),
        }
    };

    let filas = if es_hojas && filas > commerce_core::MAX_FILAS_EXCEL {
        app_shell::warn(&format!(
            "Excel admite como máximo {} filas por hoja; se usará ese valor.",
            commerce_core::MAX_FILAS_EXCEL
        ));
        commerce_core::MAX_FILAS_EXCEL
    } else {
        filas
    };

    app_shell::info(&format!("División: {filas} filas por {unidad}."));
    Ok(if es_hojas {
        Division::Hojas(filas)
    } else {
        Division::Archivos(filas)
    })
}

fn ejecutar() -> AppResult<()> {
    app_shell::mostrar_cabecera("DATA COMBINATOR — combinar varios archivos en uno");

    let disponibles = listar_archivos(&app_shell::ruta_entrada());
    if disponibles.is_empty() {
        app_shell::error(&format!(
            "No se encontraron archivos en '{}'.",
            app_shell::ruta_entrada().display()
        ));
        return Ok(());
    }

    let opciones: Vec<ArchivoOpcion> = disponibles.into_iter().map(ArchivoOpcion).collect();
    let elegidos = app_shell::menu_multiple("Archivos a combinar (Enter sin marcar = cancelar):", opciones)?;
    if elegidos.is_empty() {
        app_shell::warn("Sin archivos seleccionados. Operación cancelada.");
        return Ok(());
    }
    let archivos: Vec<PathBuf> = elegidos.into_iter().map(|a| a.0).collect();

    let hojas_excluir = match app_shell::preguntar_hojas_excluir(&archivos, "")? {
        Some(h) => h,
        None => {
            app_shell::info("Hasta luego.");
            return Ok(());
        }
    };
    let mut hojas_excluir_vec: Vec<String> = hojas_excluir.into_iter().collect();
    hojas_excluir_vec.sort();
    let excluir_refs: Vec<&str> = hojas_excluir_vec.iter().map(String::as_str).collect();

    let columnas_disp = columnas_union(&archivos, Some(&excluir_refs), app_shell::warn);
    if columnas_disp.is_empty() {
        app_shell::error("No se pudieron leer columnas de los archivos.");
        return Ok(());
    }

    let opciones = vec![
        format!("Todas las columnas ({})", columnas_disp.len()),
        "Elegir columnas específicas…".to_string(),
    ];
    // Compara contra `opciones[0]` (el propio texto recién construido, no un
    // prefijo hardcodeado aparte): si la etiqueta cambia acá arriba, la
    // comparación de abajo sigue en sincro sola, sin poder desincronizarse.
    let elegido = match app_shell::menu_seleccionar_nav("¿Qué columnas incluir?", opciones.clone())? {
        Some(v) => v,
        None => {
            app_shell::info("Hasta luego.");
            return Ok(());
        }
    };
    let columnas = if elegido == opciones[0] {
        columnas_disp.clone()
    } else {
        app_shell::menu_multiple(
            "Columnas a incluir (Enter sin marcar = cancelar):",
            columnas_disp.clone(),
        )?
    };
    // Antes: sin marcar ninguna columna, `columnas` quedaba vacío y
    // `alinear_columnas`/`escritor.escribir` seguían de largo sin error —
    // el resultado final era "¡Listo! 0 filas combinadas" con un archivo de
    // salida vacío, en vez de cancelar como ya hace la selección de
    // archivos (arriba) ante el mismo "no marqué nada".
    if columnas.is_empty() {
        app_shell::warn("No se marcó ninguna columna. Operación cancelada.");
        return Ok(());
    }

    let Some(formato) =
        app_shell::menu_seleccionar_nav("Formato de salida:", vec![Formato::Xlsx, Formato::Csv])?
    else {
        app_shell::info("Hasta luego.");
        return Ok(());
    };

    let division = elegir_division(formato)?;

    let opciones_orden: Vec<String> = std::iter::once("No ordenar (más rápido, memoria plana)".to_string())
        .chain(columnas.iter().map(|c| format!("Ordenar por: {c}")))
        .collect();
    // "No ordenar" también es valor de negocio legítimo: sin "cancelar" aparte.
    let columna_orden = match app_shell::menu_seleccionar_nav("¿Ordenar el resultado?", opciones_orden)? {
        Some(v) if v != "No ordenar (más rápido, memoria plana)" => {
            Some(v.trim_start_matches("Ordenar por: ").to_string())
        }
        _ => None,
    };

    let mut ascendente = true;
    if columna_orden.is_some() {
        if app_shell::abortar_si_reservadas(&columnas, COLUMNAS_RESERVADAS) {
            return Ok(());
        }
        ascendente =
            app_shell::menu_confirmar("Sentido del orden: ¿ascendente (A→Z, 0→9)?", true)?.unwrap_or(true);
    }

    let nombre_texto =
        app_shell::pedir_texto("Nombre del archivo de salida (Enter = 'combinado'):")?.unwrap_or_default();
    let nombre_salida = if nombre_texto.is_empty() {
        "combinado".to_string()
    } else {
        nombre_texto
    };

    app_shell::info("Combinando archivos...");
    let ruta_salida = app_shell::ruta_salida();
    let opciones_combinar = OpcionesCombinar {
        archivos: &archivos,
        columnas: &columnas,
        hojas_excluir: &hojas_excluir_vec,
        formato,
        columna_orden: columna_orden.as_deref(),
        ascendente,
        nombre_salida: &nombre_salida,
        ruta_salida: &ruta_salida,
        division,
        umbrales_orden: UmbralesOrden::default(),
        umbrales_lote_csv: UmbralesLoteCsv::default(),
    };

    let total = total_filas(&archivos, Some(&excluir_refs), app_shell::warn);
    let barra = app_shell::barra_progreso("Combinando", total.unwrap_or(0));
    let (rutas, filas) = combinar(&opciones_combinar, app_shell::warn, |avance| barra.inc(avance))?;
    barra.finish_and_clear();

    if rutas.len() == 1 {
        app_shell::success(&format!(
            "Listo: {filas} filas combinadas en '{}'.",
            rutas[0].file_name().unwrap_or_default().to_string_lossy()
        ));
    } else {
        app_shell::success(&format!(
            "Listo: {filas} filas combinadas en {} archivos: '{}' … '{}'.",
            rutas.len(),
            rutas[0].file_name().unwrap_or_default().to_string_lossy(),
            rutas[rutas.len() - 1]
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
        ));
    }
    Ok(())
}

fn main() {
    if let Err(e) = ejecutar() {
        match e {
            AppError::Flujo(FlujoError::VolverAlMenu) => {}
            e => app_shell::error(&format!("Error fatal: {e}")),
        }
    }
}
