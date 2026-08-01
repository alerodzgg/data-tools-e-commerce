//! publications_validator — limpieza/validación masiva de títulos ya
//! traducidos: 7 reglas de "contaminación" (números de parte, listas de
//! años/modelos/cilindradas) + palabras prohibidas + dedup por título
//! conservando el de menor precio.
//!
//! Solo el MOTOR: selección de archivo y confirmación de OEM son
//! responsabilidad de una futura capa de interfaz.
//!
//! La Regla 6 (segmentos alfanuméricos cortos) usa `fancy-regex` porque su
//! patrón original tiene un lookahead — el crate `regex` (Rust) no soporta
//! lookaround. Se evalúa en dos fases, igual que el original: un prefiltro
//! sin lookahead (`regex`, vectorizable) descarta la mayoría de títulos, y
//! solo los candidatos pasan por el patrón completo (`fancy-regex`).

// Un panic en produccion aborta el proceso completo; en tests, en cambio,
// `.unwrap()`/`.expect()` es la forma normal de fallar rapido. El lint se
// activa solo fuera de `cfg(test)`. Las excepciones legitimas se anotan caso
// por caso con `#[allow(...)]` y su justificacion.
#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]

pub mod clasificacion;
pub mod constantes;
pub mod escritor;
pub mod lectura;
pub mod patrones;
pub mod procesador;

pub use clasificacion::clasificar_uno;
pub use constantes::columnas_reservadas_en;
pub use escritor::nuevo_escritor;
pub use patrones::{palabras_borradas, palabras_borradas_lower, Patrones};
pub use procesador::{Procesador, Stats};
