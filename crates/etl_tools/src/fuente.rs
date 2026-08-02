//! Qué archivo y qué columnas lee una operación de este crate.

use std::path::Path;

/// El archivo de entrada, sus columnas de interés y qué hojas saltar.
///
/// Los cuatro viajan SIEMPRE juntos por las funciones motor (`buscarv`,
/// `escribir_filtrado`, `escribir_reporte_y_limpio`) y describen una sola
/// cosa: de dónde salen las filas. Agruparlos evita que un llamador cruce
/// por error las columnas de un archivo con la ruta de otro, y saca a esas
/// funciones del umbral de `clippy::too_many_arguments` sin silenciarlo.
pub struct Fuente<'a> {
    /// Archivo de entrada (XLSX o CSV).
    pub archivo: &'a Path,
    /// Columnas que se conservan en la salida, en su orden final.
    pub columnas: &'a [String],
    /// Columna sobre la que se compara/cruza.
    pub columna_clave: &'a str,
    /// Hojas a saltar; `None` las procesa todas.
    pub excluir: Option<&'a [&'a str]>,
}
