//! Binario interactivo de `data_combinator`. Dos modos:
//!
//! - COMBINAR: elegir archivos, columnas, formato, división y orden.
//! - FRAGMENTAR: partir los enlaces de tienda de eBay en tramos de precio
//!   para Web Scraper Cloud.
//!
//! Los dos comparten la carpeta de entrada/salida y el estilo de menús, pero
//! no se cruzan en ningún otro punto: el modo se elige una vez al arrancar.

// Cada `src/bin/*` es un crate root propio: NO hereda los lints de `lib.rs`,
// asi que la politica se repite aca. Un panic en produccion aborta el proceso
// que ve el usuario; en tests `.unwrap()` es la forma normal de fallar rapido.
#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]

use std::fmt;
use std::path::PathBuf;

use app_shell::{FlujoError, FlujoResult};
use commerce_core::{columnas_union, total_filas, CoreError};
use data_combinator::fragmentar_ebay::ErrorFragmentar;
use data_combinator::{
    combinar, fragmentar_archivo, Division, Formato, OpcionesCombinar, OpcionesFragmentar,
    UmbralesLoteCsv, UmbralesOrden, COLUMNAS_RESERVADAS, UMBRAL_POR_DEFECTO,
};

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Flujo(#[from] FlujoError),
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error(transparent)]
    Fragmentar(#[from] ErrorFragmentar),
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
    /// Cómo se reparte la salida. El menú devuelve la variante por VALOR:
    /// comparar contra el texto mostrado dejaba la decisión atada a una
    /// etiqueta que puede cambiar sin que nada lo advierta.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ModoDivision {
        Ninguna,
        PorHojas,
        PorArchivos,
    }
    impl std::fmt::Display for ModoDivision {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                ModoDivision::Ninguna => write!(f, "No dividir (una sola salida)"),
                ModoDivision::PorHojas => {
                    write!(f, "En hojas de N filas (dentro del mismo archivo)")
                }
                ModoDivision::PorArchivos => write!(f, "En archivos separados de N filas"),
            }
        }
    }

    let mut opciones = vec![ModoDivision::Ninguna];
    // Dividir en hojas solo tiene sentido en un formato con hojas.
    if formato == Formato::Xlsx {
        opciones.push(ModoDivision::PorHojas);
    }
    opciones.push(ModoDivision::PorArchivos);

    // `None`/"no dividir" ya es un valor de negocio legítimo: no se ofrece
    // "cancelar" aparte (colisionaría), solo "volver al menú".
    let Some(modo) = app_shell::menu_seleccionar_nav("¿Dividir la salida?", opciones)? else {
        return Ok(Division::Ninguna);
    };
    if modo == ModoDivision::Ninguna {
        return Ok(Division::Ninguna);
    }
    let es_hojas = modo == ModoDivision::PorHojas;
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

fn combinar_archivos() -> AppResult<()> {
    app_shell::mostrar_subcabecera("Combinar varios archivos en uno");

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

    /// Qué columnas entran en la salida.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum SeleccionColumnas {
        Todas,
        Elegir,
    }
    struct OpcionColumnas {
        cual: SeleccionColumnas,
        etiqueta: String,
    }
    impl std::fmt::Display for OpcionColumnas {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.etiqueta)
        }
    }

    let opciones = vec![
        OpcionColumnas {
            cual: SeleccionColumnas::Todas,
            etiqueta: format!("Todas las columnas ({})", columnas_disp.len()),
        },
        OpcionColumnas {
            cual: SeleccionColumnas::Elegir,
            etiqueta: "Elegir columnas específicas…".to_string(),
        },
    ];
    let Some(elegido) = app_shell::menu_seleccionar_nav("¿Qué columnas incluir?", opciones)? else {
        app_shell::info("Hasta luego.");
        return Ok(());
    };
    let columnas = match elegido.cual {
        SeleccionColumnas::Todas => columnas_disp,
        SeleccionColumnas::Elegir => {
            app_shell::menu_multiple("Columnas a incluir (Enter sin marcar = cancelar):", columnas_disp)?
        }
    };
    // Sin ninguna columna marcada hay que cancelar: el resto del flujo
    // seguiría sin error y produciría "0 filas combinadas" con un archivo
    // vacío, en vez del mismo aviso que da la selección de archivos.
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

/// Pide el umbral de publicaciones por terminal.
///
/// Repreguntar en vez de aceptar cualquier cosa: un umbral mal escrito no da
/// ningún error visible, solo fragmenta las tiendas equivocadas —o ninguna— y
/// el archivo de salida parece correcto.
fn pedir_umbral() -> FlujoResult<u64> {
    loop {
        let texto = app_shell::pedir_texto(&format!(
            "Umbral de publicaciones a fragmentar (Enter = {UMBRAL_POR_DEFECTO}):"
        ))?
        .unwrap_or_default();
        if texto.is_empty() {
            return Ok(UMBRAL_POR_DEFECTO);
        }
        // Se toleran los separadores con los que un humano escribe "9.000" o
        // "9,000": rechazarlos sería exigirle un formato que no aporta nada.
        let limpio: String = texto.chars().filter(|c| !matches!(c, '.' | ',' | ' ')).collect();
        match limpio.parse::<u64>() {
            Ok(n) if n >= 1 => return Ok(n),
            _ => app_shell::warn("Escribe un número entero mayor que 0."),
        }
    }
}

fn fragmentar_enlaces() -> AppResult<()> {
    app_shell::mostrar_subcabecera("Fragmentar enlaces de tienda de eBay por rango de precio");

    let entrada = app_shell::ruta_entrada();
    let disponibles = app_shell::listar_xlsx(&entrada).unwrap_or_default();
    if disponibles.is_empty() {
        app_shell::error(&format!("No se encontraron archivos .xlsx en '{}'.", entrada.display()));
        return Ok(());
    }
    let Some(archivo) = app_shell::elegir_archivo("Archivo a fragmentar:", disponibles)? else {
        app_shell::info("Hasta luego.");
        return Ok(());
    };

    let umbral = pedir_umbral()?;
    app_shell::info(&format!(
        "Se fragmentarán las tiendas con {umbral} publicaciones o más, en todas las hojas del archivo."
    ));

    let nombre_texto =
        app_shell::pedir_texto("Nombre del archivo de salida (Enter = 'fragmentado'):")?.unwrap_or_default();
    let nombre_salida = if nombre_texto.is_empty() {
        "fragmentado".to_string()
    } else {
        nombre_texto
    };

    let (destino, resumen) = fragmentar_archivo(
        &OpcionesFragmentar {
            archivo: &archivo,
            umbral,
            nombre_salida: &nombre_salida,
            ruta_salida: &app_shell::ruta_salida(),
        },
        app_shell::warn,
    )?;

    app_shell::success(&format!(
        "Listo: {} enlaces generados en '{}' ({} filas de entrada → {} de salida).",
        resumen.filas_generadas,
        destino.file_name().unwrap_or_default().to_string_lossy(),
        resumen.filas_entrada,
        resumen.filas_salida,
    ));

    // El total y el reparto van SIEMPRE, incluso valiendo cero. Antes solo se
    // imprimía lo fragmentado y las categorías con valor: el silencio era
    // ambiguo —no se distinguía "no hubo errores" de "esto no revisa
    // errores"— y no había forma de ver cuántas tiendas se consideraron en
    // total sin sumar a mano.
    app_shell::info(&format!(
        "Tiendas: {} en total → {} fragmentadas, {} sin fragmentar.",
        resumen.tiendas_totales(),
        resumen.tiendas_fragmentadas,
        resumen.sin_fragmentar(),
    ));
    app_shell::info(&format!(
        "  · {} por debajo del umbral ({umbral} publicaciones). No son un error.",
        resumen.filas_bajo_umbral
    ));
    if resumen.errores() == 0 {
        app_shell::info("  · 0 con error: todas las que llegaban al umbral se fragmentaron.");
    } else {
        // Una tienda que se quedó sin tramos es trabajo que el scraper no va
        // a hacer, y enterarse al final de la corrida de Web Scraper Cloud
        // sale mucho más caro que leerlo acá.
        app_shell::warn(&format!(
            "  · {} llegaban al umbral y NO se pudieron fragmentar:",
            resumen.errores()
        ));
        if resumen.enlaces_invalidos > 0 {
            app_shell::warn(&format!(
                "      {} con un enlace irreconocible.",
                resumen.enlaces_invalidos
            ));
        }
        if resumen.publicaciones_ilegibles > 0 {
            app_shell::warn(&format!(
                "      {} con un valor no numérico en '{}'.",
                resumen.publicaciones_ilegibles,
                data_combinator::fragmentar_ebay::COLUMNA_PUBLICACIONES
            ));
        }
    }
    if resumen.hojas_sin_columnas > 0 {
        app_shell::warn(&format!(
            "{} hojas se copiaron sin cambios por no tener las dos columnas obligatorias.",
            resumen.hojas_sin_columnas
        ));
    }
    Ok(())
}

/// Qué hace esta corrida. El menú devuelve la variante por VALOR (nunca el
/// texto mostrado) para que renombrar una etiqueta no pueda cambiar en
/// silencio qué modo se ejecuta.
#[derive(Clone, Copy)]
enum Modo {
    Combinar,
    FragmentarEbay,
}

impl fmt::Display for Modo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Modo::Combinar => write!(f, "Combinar varios archivos en uno"),
            Modo::FragmentarEbay => {
                write!(f, "Fragmentar enlaces de tienda de eBay por rango de precio")
            }
        }
    }
}

fn ejecutar() -> AppResult<()> {
    app_shell::mostrar_cabecera("DATA COMBINATOR");
    let Some(modo) = app_shell::menu_seleccionar_nav("¿Qué querés hacer?", vec![Modo::Combinar, Modo::FragmentarEbay])?
    else {
        app_shell::info("Hasta luego.");
        return Ok(());
    };
    match modo {
        Modo::Combinar => combinar_archivos(),
        Modo::FragmentarEbay => fragmentar_enlaces(),
    }
}

fn main() {
    if let Err(e) = ejecutar() {
        match e {
            AppError::Flujo(FlujoError::VolverAlMenu) => {}
            e => app_shell::error(&format!("Error fatal: {e}")),
        }
    }
}
