//! publications_builder — limpieza/preparación de publicaciones eBay/Amazon
//! para republicar: modos Únicas, Compatibilidades/Comprimidas y Amazon.
//!
//! Solo el MOTOR: selección de archivos, preguntas de confirmación
//! (`¿Borrar la columna 'Linea'?`, `¿Borrar imágenes extra?`, `¿Modificar
//! OEM?`) y barra de progreso son responsabilidad de una futura capa de
//! interfaz — llegan aquí ya resueltas como parámetros/callbacks, igual que
//! en `commerce_core`, `data_combinator` y `etl_tools`.

pub mod amazon;
pub mod compatibilidad;
pub mod comunes;
pub mod constantes;
pub mod escritor;
#[cfg(test)]
mod test_util;
pub mod unicas;

pub use amazon::{ejecutar_procesamiento_amazon, procesar_dataframe_amazon};
pub use compatibilidad::{
    ejecutar_procesamiento_compatibilidad, procesar_dataframe_compatibilidad, ModoCompatibilidad,
};
pub use constantes::columnas_reservadas_en;
pub use escritor::nuevo_escritor;
pub use unicas::{ejecutar_procesamiento_unicas, procesar_dataframe_unicas};
