//! Barra de progreso, usando `indicatif`.

use indicatif::{ProgressBar, ProgressStyle};

pub fn barra_progreso(descripcion: &str, total: u64) -> ProgressBar {
    let barra = ProgressBar::new(total);
    barra.set_style(
        ProgressStyle::with_template("{spinner:.green} {msg} [{bar:40.green}] {pos}/{len} ({eta})")
            .unwrap()
            .progress_chars("=> "),
    );
    barra.set_message(descripcion.to_string());
    barra
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn barra_progreso_no_entra_en_panico_con_el_template() {
        // El `.unwrap()` sobre `ProgressStyle::with_template(...)` puede
        // entrar en pánico si el template se rompe en un futuro cambio; sin
        // este test, nada lo cubría.
        let barra = barra_progreso("Procesando", 100);
        assert_eq!(barra.length(), Some(100));
        assert_eq!(barra.position(), 0);
    }

    #[test]
    fn barra_progreso_con_total_cero_no_entra_en_panico() {
        let barra = barra_progreso("Sin filas", 0);
        assert_eq!(barra.length(), Some(0));
    }

    #[test]
    fn barra_progreso_avanza_con_inc() {
        let barra = barra_progreso("Procesando", 10);
        barra.inc(3);
        assert_eq!(barra.position(), 3);
    }
}
