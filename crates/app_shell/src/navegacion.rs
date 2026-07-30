//! Señal de "volver al menú principal desde cualquier profundidad": una
//! variante de error propagada con `?` a través de `FlujoResult`, sin
//! necesidad de desandar cada nivel a mano.

#[derive(Debug, thiserror::Error)]
pub enum FlujoError {
    /// El usuario eligió « Volver al menú principal » en algún punto: sale
    /// de la herramienta entera, sin importar cuán anidado esté el menú.
    #[error("volver al menú principal")]
    VolverAlMenu,
    #[error(transparent)]
    Core(#[from] commerce_core::CoreError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Inquire(#[from] inquire::InquireError),
}

pub type FlujoResult<T> = Result<T, FlujoError>;
