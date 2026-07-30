//! Mensajes e impresión con estilo: mismos niveles/iconos y misma cabecera
//! estándar para todas las herramientas, sin el banner ASCII grande (adorno
//! visual sin lógica: se deja para cuando haga falta, no es parte del
//! "motor").

use owo_colors::OwoColorize;

use crate::rutas::{ruta_entrada, ruta_salida};

enum Nivel {
    Info,
    Success,
    Warning,
    Error,
}

fn icono(nivel: &Nivel) -> &'static str {
    match nivel {
        Nivel::Info => "\u{2139}\u{fe0f}",
        Nivel::Success => "\u{2705}",
        Nivel::Warning => "\u{26a0}\u{fe0f}",
        Nivel::Error => "\u{274c}",
    }
}

fn imprimir_usuario(mensaje: &str, nivel: Nivel) {
    println!("{}  {mensaje}", icono(&nivel));
}

pub fn info(mensaje: &str) {
    imprimir_usuario(mensaje, Nivel::Info);
}
pub fn success(mensaje: &str) {
    imprimir_usuario(mensaje, Nivel::Success);
}
pub fn warn(mensaje: &str) {
    imprimir_usuario(mensaje, Nivel::Warning);
}
pub fn error(mensaje: &str) {
    imprimir_usuario(mensaje, Nivel::Error);
}

/// Cabecera de arranque de una herramienta: siempre igual en todas.
pub fn mostrar_cabecera(titulo: &str) {
    println!("\n{}", format!("── {titulo} ──").bold().truecolor(0, 175, 255));
    info(&format!("Entrada: {}", ruta_entrada().display()));
    info(&format!("Salida:  {}", ruta_salida().display()));
}

/// Cabecera de un sub-flujo dentro de una herramienta (sin repetir
/// Entrada/Salida, ya mostradas al entrar).
pub fn mostrar_subcabecera(titulo: &str) {
    println!("\n{}", format!("── {titulo} ──").bold().truecolor(0, 175, 255));
}

#[cfg(test)]
mod tests {
    use super::*;

    // El resto del módulo es `println!` puro sin ramas ni operaciones
    // falibles (nada que un test pudiera atrapar más allá de "compila y no
    // entra en pánico"); `icono` es la única lógica real: un mapeo que un
    // copy-paste podría romper en silencio (dos niveles con el mismo ícono).
    #[test]
    fn icono_es_distinto_para_cada_nivel() {
        let iconos = [
            icono(&Nivel::Info),
            icono(&Nivel::Success),
            icono(&Nivel::Warning),
            icono(&Nivel::Error),
        ];
        let distintos: std::collections::HashSet<_> = iconos.iter().collect();
        assert_eq!(
            distintos.len(),
            iconos.len(),
            "cada nivel debe tener su propio ícono"
        );
    }
}
