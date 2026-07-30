//! data_combinator — combina varios archivos (CSV/XLSX) en una sola salida,
//! con orden global opcional (mezcla externa en disco para 10M+ filas) y
//! división por hojas o por archivos.
//!
//! Solo el MOTOR: selección de archivos/columnas, menús y barra de progreso
//! son responsabilidad de una futura capa de interfaz interactiva, que
//! llamará a [`combinar`] pasándole callbacks de aviso/progreso.

pub mod combinar;
pub mod constantes;
pub mod escritor_particionado;
pub mod lectura;
pub mod normalizar;
pub mod orden;

pub use combinar::{combinar, Division, Formato, OpcionesCombinar};
pub use constantes::{
    UmbralesLoteCsv, UmbralesOrden, COLUMNAS_RESERVADAS, FILAS_POR_HOJA, SOPORTADOS_ARCHIVOS,
};
pub use escritor_particionado::EscritorParticionado;
pub use lectura::{iter_chunks, listar_archivos, LotesCsv};
pub use normalizar::normalizar;
pub use orden::{ordenar_excel_df, ClaveExcel};
