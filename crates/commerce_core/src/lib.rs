//! commerce_core — motor compartido por las herramientas de data-tools-e-commerce.
//!
//! Qué vive aquí: todo lo que NO es lógica de negocio ni interfaz.
//!   · Serialización XML/OOXML (escritor XLSX propio y rápido)
//!   · Reparación de mojibake (patrón UTF-8 leído como Windows-1252)
//!   · Nombres canónicos de cabecera y de archivo
//!   · Guard de columnas reservadas
//!
//! REGLA DE ORO: este crate no sabe nada de la interfaz (ni menús, ni barras
//! de progreso, ni `println!`). Cuando necesita avisar de algo, lo devuelve
//! (`Result`) o lo pasa a un callback; quien llama decide cómo mostrarlo.

// Política de la Ronda 9 de auditoría (ver docs/decisiones/0006-tests-reactivos-vs-invariantes.md):
// un panic en código de producción de este crate tumba el proceso completo
// de CUALQUIER binario que lo use — `commerce_core` es el motor compartido
// por los 5. `cfg(test)` desactiva el lint para el propio código de test
// (donde un `.unwrap()`/`.expect()` es la forma normal de fallar rápido), y
// solo se activa cuando se compila el `--lib` real (no bajo `--cfg test`),
// que es el único caso donde el código compilado es 100% de producción.
#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]

mod alineacion;
pub mod cabeceras;
pub mod columnas;
pub mod error;
pub mod escritor_csv;
pub mod escritor_salida;
pub mod escritor_xlsx;
pub mod lector;
pub mod mojibake;
pub mod orden_excel;
pub mod particionado;
pub mod rutas;
mod xml;

pub use cabeceras::{columnas_reservadas_en, nombres_cabecera, renombrar_canonico};
pub use columnas::{columna_texto, concat_diagonal, concatenar_vertical, tomar_filas};
pub use error::{CoreError, CoreResult};
pub use escritor_csv::EscritorCsv;
pub use escritor_salida::EscritorSalida;
pub use escritor_xlsx::{EscritorXlsx, OpcionesEscritorXlsx};
pub use lector::{
    abrir_libro, columnas_union, columnas_union_de_bloques, contar_filas_hoja, iter_hojas_xlsx,
    leer_hoja_por_nombre, nombres_hojas_libro, total_filas, LibroXlsx,
};
pub use mojibake::limpiar_mojibake;
pub use orden_excel::{como_numero_finito, ordenar_excel_df, recortar_espacios_orden};
pub use particionado::AcumuladorParticionado;
pub use rutas::{ruta_unica, RutaEscritaReal};
pub use xml::MAX_FILAS_EXCEL;
