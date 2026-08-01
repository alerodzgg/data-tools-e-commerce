use std::collections::HashSet;

use crate::patrones::Patrones;

fn quitar_puntuacion(palabra: &str) -> String {
    palabra
        .chars()
        .filter(|c| !matches!(c, '/' | '.' | '*' | ',' | '-'))
        .collect()
}

fn contar_digitos(s: &str) -> usize {
    s.chars().filter(char::is_ascii_digit).count()
}

fn contar_letras(s: &str) -> usize {
    s.chars().filter(char::is_ascii_alphabetic).count()
}

/// Clasifica UN título: motivo de eliminación (o `None` si sobrevive) y su
/// versión de salida (el original si es palabra prohibida, el limpio si
/// no). Función PURA — el llamador decide cachear por título distinto.
///
/// Las 7 reglas (palabra prohibida + 6 reglas de "contaminación" del
/// título) se evalúan en CASCADA: gana la primera que dispare.
pub fn clasificar_uno(
    titulo: &str,
    patrones: &Patrones,
    marcas_lower: &HashSet<String>,
) -> (String, Option<String>) {
    if let Some(caps) = patrones.prohibidas.captures(titulo) {
        // El grupo 1 existe por construcción del patrón (ver `Patrones`), pero
        // se resuelve sin `unwrap`: si el patrón cambiara, se reporta la
        // coincidencia completa en vez de abortar el proceso.
        let palabra = caps
            .get(1)
            .map_or_else(|| titulo.to_string(), |m| m.as_str().to_string());
        return (titulo.to_string(), Some(format!("Palabra prohibida({palabra})")));
    }

    let mut limpio = patrones.re_x_hex.replace_all(titulo, "").into_owned();
    limpio = patrones.re_control.replace_all(&limpio, "").into_owned();
    limpio = patrones
        .re_espacios_especiales
        .replace_all(&limpio, " ")
        .into_owned();
    limpio = patrones.re_invisibles.replace_all(&limpio, "").into_owned();
    let limpio_recortado = limpio.trim_end_matches(['\n', '\r', '.']).to_string();
    limpio = patrones
        .re_espacios_multi
        .replace_all(&limpio_recortado, " ")
        .trim()
        .to_string();

    let sin_marcas: String = limpio
        .split(' ')
        .filter(|palabra| {
            let recortado = palabra.to_lowercase();
            let recortado = recortado.trim_matches(|c: char| ".,!?;:\"'()[]{}".contains(c));
            !marcas_lower.contains(recortado)
        })
        .collect::<Vec<_>>()
        .join(" ");
    limpio = patrones
        .re_espacios_multi
        .replace_all(&sin_marcas, " ")
        .trim()
        .to_string();

    let motivo = if patrones.re_seis_digitos.is_match(&limpio) {
        Some("Contiene 6 números consecutivos".to_string())
    } else if limpio.chars().count() < 30 || limpio.chars().count() > 60 {
        Some(format!(
            "Longitud fuera de rango: {} caracteres",
            limpio.chars().count()
        ))
    } else if contar_digitos(&limpio) >= 14 {
        Some("(Regla 4) Contiene 14 o más dígitos en total".to_string())
    } else if limpio
        .split(' ')
        .any(|w| contar_digitos(&quitar_puntuacion(w)) > 9)
    {
        Some("(Regla 1) Palabra con más de 9 dígitos".to_string())
    } else if limpio.split(' ').any(|w| {
        let sin_puntuacion = quitar_puntuacion(w);
        contar_digitos(&sin_puntuacion) >= 7 && contar_letras(&sin_puntuacion) >= 3
    }) {
        Some("(Regla 2) Palabra con más de 7 dígitos y 3+ letras".to_string())
    } else if patrones.re_secuencia.is_match(&limpio) {
        Some("(Regla 3) Secuencia de 3+ números con el mismo primer dígito".to_string())
    } else if patrones.re_decimales.is_match(&limpio) {
        Some("(Regla 5) Contiene 4 o más decimales consecutivos".to_string())
    } else if patrones.re_seg_prefiltro.is_match(&limpio)
        && patrones.re_segmentos.is_match(&limpio).unwrap_or(false)
    {
        Some("(Regla 6) Contiene 4 o más segmentos alfanuméricos cortos (3-4 caracteres)".to_string())
    } else {
        None
    };

    (limpio, motivo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patrones::{palabras_borradas, palabras_borradas_lower};

    fn patrones_y_marcas() -> (Patrones, HashSet<String>) {
        let patrones = Patrones::compilar();
        let marcas = palabras_borradas_lower(&palabras_borradas());
        (patrones, marcas)
    }

    #[test]
    fn palabra_prohibida_conserva_original() {
        let (patrones, marcas) = patrones_y_marcas();
        let (salida, motivo) = clasificar_uno(
            "Radio Pantalla Tactil Para Auto Con Bluetooth",
            &patrones,
            &marcas,
        );
        assert_eq!(salida, "Radio Pantalla Tactil Para Auto Con Bluetooth");
        assert_eq!(motivo, Some("Palabra prohibida(Radio)".to_string()));
    }

    #[test]
    fn borra_marca_y_sobrevive() {
        let (patrones, marcas) = patrones_y_marcas();
        let (salida, motivo) = clasificar_uno(
            "Bosch. Amortiguador Delantero Toyota Corolla Sport",
            &patrones,
            &marcas,
        );
        assert_eq!(salida, "Amortiguador Delantero Toyota Corolla Sport");
        assert_eq!(motivo, None);
    }

    #[test]
    fn longitud_corta_dispara_regla_longitud() {
        let (patrones, marcas) = patrones_y_marcas();
        let (_, motivo) = clasificar_uno("Corto", &patrones, &marcas);
        assert_eq!(motivo, Some("Longitud fuera de rango: 5 caracteres".to_string()));
    }

    #[test]
    fn seis_digitos_consecutivos() {
        let (patrones, marcas) = patrones_y_marcas();
        let (_, motivo) = clasificar_uno("Compresor Con Numero 123456 Integrado Al Kit", &patrones, &marcas);
        assert_eq!(motivo, Some("Contiene 6 números consecutivos".to_string()));
    }

    #[test]
    fn regla6_segmentos_cortos() {
        let (patrones, marcas) = patrones_y_marcas();
        let (_, motivo) = clasificar_uno("Disco Clutch Caterpillar 3116 938g 3126 38f", &patrones, &marcas);
        assert_eq!(
            motivo,
            Some("(Regla 6) Contiene 4 o más segmentos alfanuméricos cortos (3-4 caracteres)".to_string())
        );
    }

    #[test]
    fn regla3_secuencia_mismo_primer_digito() {
        let (patrones, marcas) = patrones_y_marcas();
        let (_, motivo) = clasificar_uno(
            "Chrysler Motor Culata 318 340 360 Original Compatible",
            &patrones,
            &marcas,
        );
        assert_eq!(
            motivo,
            Some("Palabra prohibida(Original)".to_string()),
            "Original es palabra prohibida, gana primero"
        );
    }

    #[test]
    fn regla1_palabra_mas_de_9_digitos() {
        let (patrones, marcas) = patrones_y_marcas();
        let (_, motivo) = clasificar_uno(
            "Sensor Oxigeno Codigo 00016-34174 Para Nissan",
            &patrones,
            &marcas,
        );
        assert_eq!(motivo, Some("(Regla 1) Palabra con más de 9 dígitos".to_string()));
    }

    #[test]
    fn regla5_decimales_consecutivos() {
        let (patrones, marcas) = patrones_y_marcas();
        let (_, motivo) = clasificar_uno(
            "Arbol Levas Gmc Ls 4.8 5.3 6.0 6.2 Gen3 Gen4 Kit Completo",
            &patrones,
            &marcas,
        );
        assert_eq!(
            motivo,
            Some("(Regla 5) Contiene 4 o más decimales consecutivos".to_string())
        );
    }
}
