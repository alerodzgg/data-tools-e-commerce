//! Compilación de patrones regex LITERALES (constantes del propio código).

use regex::{Regex, RegexBuilder};

/// Compila un patrón que es una CONSTANTE del código, nunca entrada de
/// usuario ni contenido de un archivo. Un patrón inválido acá es un error de
/// programación que debe aparecer en la primera corrida, no un `Result` que
/// cada llamador propague sin poder hacer nada útil con él.
///
/// Centraliza la única excepción legítima a la política de "sin `unwrap`/
/// `expect` en producción": sin esta función, la misma justificación habría
/// que repetirla en cada `static` de regex del workspace.
///
/// # Panics
/// Si `patron` no es un regex válido.
#[allow(clippy::expect_used)]
pub fn regex_literal(patron: &str) -> Regex {
    Regex::new(patron).expect("patrón regex literal inválido (constante del código)")
}

/// Como [`regex_literal`], pero sin distinguir mayúsculas de minúsculas.
///
/// # Panics
/// Si `patron` no es un regex válido.
#[allow(clippy::expect_used)]
pub fn regex_literal_sin_may(patron: &str) -> Regex {
    RegexBuilder::new(patron)
        .case_insensitive(true)
        .build()
        .expect("patrón regex literal inválido (constante del código)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compila_y_respeta_la_insensibilidad_a_mayusculas() {
        assert!(regex_literal(r"^\d+$").is_match("123"));
        assert!(!regex_literal("video").is_match("VIDEO"));
        assert!(regex_literal_sin_may("video").is_match("VIDEO"));
    }
}
