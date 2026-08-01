//! etl_tools — caja de herramientas CSV/XLSX: borrado por palabra, duplicados,
//! ordenar estilo Excel, dividir por caracteres, BUSCARV (exacto y parcial),
//! Encontrar.
//!
//! Solo el MOTOR: selección de archivos/columnas, menús y barra de progreso
//! son responsabilidad de una futura capa de interfaz. Las funciones de este
//! crate reciben parámetros ya resueltos y devuelven resultados/`Result`;
//! cuando necesitan avisar de algo no crítico, lo hacen vía un callback
//! `avisar`/`progreso`, igual que en `commerce_core` y `data_combinator`.
//!
//! ## Alcance actual
//!
//! El modo "Borrar por color de fuente" (`procesar_por_color` /
//! `_iter_hojas_color`) queda FUERA por ahora: requiere leer el color de
//! fuente de cada celda, algo que ningún lector de XLSX en el ecosistema
//! Rust expone hoy. Implementarlo bien exige un parser OOXML de estilos
//! aparte (fonts + cellXfs + mapeo celda→estilo), un subsistema nuevo del
//! tamaño de un módulo propio, así que se priorizó primero todo lo demás.
//! El núcleo de borrado (`borrado::procesar_columnas_con_desplazamiento`) ya
//! está escrito de forma genérica para poder enchufarle un criterio de
//! color el día que se implemente ese lector.

// Un panic en produccion aborta el proceso completo; en tests, en cambio,
// `.unwrap()`/`.expect()` es la forma normal de fallar rapido. El lint se
// activa solo fuera de `cfg(test)`. Las excepciones legitimas se anotan caso
// por caso con `#[allow(...)]` y su justificacion.
#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]

pub mod borrado;
pub mod buscarv;
pub mod caracteres;
pub mod coincidencia_parcial;
pub mod constantes;
pub mod duplicados;
pub mod encontrar;
pub mod escritor;
pub mod lectura;

pub use borrado::{generar_reporte_cambios, procesar_columnas_con_desplazamiento};
pub use buscarv::{buscarv, cargar_tabla_busqueda, renombrar_traidas};
pub use caracteres::{anadir_columna_y_ordenar, contar_caracteres, dividir_dentro_fuera};
pub use coincidencia_parcial::{
    cargar_tabla_parcial, construir_automata, cruzar_chunk_parcial, norm_parcial, OpcionMultiple,
};
pub use constantes::columnas_reservadas_presentes;
pub use duplicados::{
    clave_limpia_de, claves_repetidas, claves_unicas, escribir_filtrado, escribir_reporte_y_limpio,
};
pub use encontrar::{lookup_palabras, palabras_de_xlsx};
pub use escritor::nuevo_escritor;
pub use lectura::{iter_hojas_valores, preparar_chunk, preparar_chunk_clave};

// Reexportado para que las herramientas que dependan de este crate no
// necesiten depender también de `commerce_core` solo por el orden Excel.
pub use commerce_core::ordenar_excel_df;
