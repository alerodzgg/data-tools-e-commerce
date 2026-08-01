//! app_shell — capa de interfaz compartida por los binarios interactivos de
//! las herramientas de data-tools-e-commerce: rutas, mensajes, menús, barra de
//! progreso, navegación y diálogos comunes.
//!
//! REGLA DE ORO (misma que `commerce_core`): este crate es la interfaz, así
//! que es el único lugar del workspace donde `println!`/menús/barras son
//! aceptables. Los motores (`commerce_core`, `ocr_tools`, etc.) no dependen
//! de este crate.

// Un panic en produccion aborta el proceso completo; en tests, en cambio,
// `.unwrap()`/`.expect()` es la forma normal de fallar rapido. El lint se
// activa solo fuera de `cfg(test)`. Las excepciones legitimas se anotan caso
// por caso con `#[allow(...)]` y su justificacion.
#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]

pub mod diagnostico;
pub mod dialogos;
pub mod mensajes;
pub mod menus;
pub mod navegacion;
pub mod progreso;
pub mod rutas;
pub mod testing;

pub use diagnostico::mostrar_diagnostico_sistema;
pub use dialogos::{abortar_si_reservadas, avisar_si_hay_reservadas, listar_xlsx, preguntar_hojas_excluir};
pub use mensajes::{error, info, mostrar_cabecera, mostrar_subcabecera, success, warn};
pub use menus::{
    elegir_archivo, elegir_archivos, menu_confirmar, menu_multiple, menu_multiple_preseleccionado,
    menu_seleccionar, menu_seleccionar_nav, pedir_texto,
};
pub use navegacion::{FlujoError, FlujoResult};
pub use progreso::barra_progreso;
pub use rutas::{fijar_rutas, ruta_entrada, ruta_salida};
