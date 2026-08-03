//! Binario interactivo de `etl_tools`: borrar por palabra, gestionar
//! duplicados, ordenar columnas, dividir por caracteres, BUSCARV
//! (exacto/parcial), Encontrar.
//!
//! "Borrar por color de fuente" no está en el menú: requeriría leer el
//! color de fuente de cada celda, algo que ningún lector de XLSX en el
//! ecosistema Rust expone hoy (ver el comentario de alcance en `lib.rs`).

// Cada `src/bin/*` es un crate root propio: NO hereda los lints de `lib.rs`,
// asi que la politica se repite aca. Un panic en produccion aborta el proceso
// que ve el usuario; en tests `.unwrap()` es la forma normal de fallar rapido.
#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]

use app_shell::FlujoError;
use commerce_core::CoreError;

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Flujo(#[from] FlujoError),
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error(transparent)]
    Polars(#[from] polars::error::PolarsError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

type AppResult<T> = Result<T, AppError>;

/// Tamaño de bloque para las escrituras en streaming de este binario: evita
/// materializar el DataFrame completo de una sola vez.
const FILAS_POR_BLOQUE_ESCRITURA: usize = 200_000;

mod buscarv;
mod caracteres;
mod comunes;
mod cruce;
mod duplicados;
mod encontrar;
mod ordenar;
mod palabra;

use buscarv::{buscarv_modo, buscarv_parcial_modo};
use caracteres::dividir_por_caracteres;
use comunes::seleccionar_archivo;
use duplicados::gestionar_duplicados;
use encontrar::encontrar_modo;
use ordenar::ordenar_columna;
use palabra::procesar_por_palabra;
// ════════════════════════════════════════════════════════════════════════
// main()
// ════════════════════════════════════════════════════════════════════════

/// Los modos del menú principal. El menú los ofrece POR VALOR (vía
/// `menu_seleccionar_nav<Modo>`) y `ejecutar` los resuelve con un `match`
/// exhaustivo: agregar o quitar una variante no compila hasta contemplarla
/// en el despacho. Con una lista de etiquetas y comparación posicional
/// (`modo == opciones[N]`), en cambio, reordenar el menú desviaba
/// silenciosamente cada rama posterior sin que el compilador lo notara.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modo {
    BorrarPorPalabra,
    Duplicados,
    OrdenarColumnas,
    ContarCaracteres,
    Buscarv,
    BuscarvParcial,
    Encontrar,
}

impl Modo {
    const TODOS: [Modo; 7] = [
        Modo::BorrarPorPalabra,
        Modo::Duplicados,
        Modo::OrdenarColumnas,
        Modo::ContarCaracteres,
        Modo::Buscarv,
        Modo::BuscarvParcial,
        Modo::Encontrar,
    ];
}

impl std::fmt::Display for Modo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let etiqueta = match self {
            Modo::BorrarPorPalabra => "Borrar por palabra clave",
            Modo::Duplicados => "Gestionar duplicados (por columna clave)",
            Modo::OrdenarColumnas => "Ordenar columnas (A→Z / 0→9, estilo Excel)",
            Modo::ContarCaracteres => "Contar caracteres de una columna (columna, hojas o reporte)",
            Modo::Buscarv => "BUSCARV — traer columnas de otro archivo (VLOOKUP)",
            Modo::BuscarvParcial => "BUSCARV parcial — cruce por coincidencia parcial (palabra completa)",
            Modo::Encontrar => "Encontrar — marcar filas que contienen palabras (terminal o XLSX)",
        };
        write!(f, "{etiqueta}")
    }
}

fn ejecutar() -> AppResult<()> {
    app_shell::mostrar_cabecera("ETL TOOLS — BUSCARV, duplicados, colores, Encontrar");
    let ruta_salida = app_shell::ruta_salida();

    loop {
        let modo = match app_shell::menu_seleccionar_nav("¿Qué quieres hacer?", Modo::TODOS.to_vec()) {
            Ok(Some(m)) => m,
            Ok(None) => {
                app_shell::info("Hasta luego.");
                break;
            }
            Err(FlujoError::VolverAlMenu) => continue,
            Err(e) => return Err(e.into()),
        };

        let resultado = match modo {
            Modo::BorrarPorPalabra => match seleccionar_archivo("Selecciona el archivo a procesar:") {
                Ok(Some(archivo)) => procesar_por_palabra(&archivo, &ruta_salida),
                Ok(None) => {
                    app_shell::warn("No se seleccionó archivo. Volviendo al menú.");
                    Ok(())
                }
                Err(e) => Err(e),
            },
            Modo::Duplicados => gestionar_duplicados(&ruta_salida),
            Modo::OrdenarColumnas => ordenar_columna(&ruta_salida),
            Modo::ContarCaracteres => dividir_por_caracteres(&ruta_salida),
            Modo::Buscarv => buscarv_modo(&ruta_salida),
            Modo::BuscarvParcial => buscarv_parcial_modo(&ruta_salida),
            Modo::Encontrar => encontrar_modo(&ruta_salida),
        };

        if let Err(e) = resultado {
            match e {
                AppError::Flujo(FlujoError::VolverAlMenu) => continue,
                e => app_shell::error(&format!("Error: {e}")),
            }
        }
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
