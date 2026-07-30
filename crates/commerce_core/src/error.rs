use std::path::PathBuf;

/// Errores del motor. `core` nunca decide cómo mostrarlos: los devuelve
/// (`Result`) y quien llama decide si abortar, avisar o ignorar.
#[derive(thiserror::Error, Debug)]
pub enum CoreError {
    #[error("no se pudo abrir '{ruta}': {fuente}")]
    AperturaLibro {
        ruta: PathBuf,
        #[source]
        fuente: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("error al leer '{archivo}' / '{hoja}': {fuente}")]
    LecturaHoja {
        archivo: PathBuf,
        hoja: String,
        #[source]
        fuente: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error(transparent)]
    Polars(#[from] polars::error::PolarsError),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
}

pub type CoreResult<T> = Result<T, CoreError>;
