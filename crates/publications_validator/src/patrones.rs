use std::collections::HashSet;

use fancy_regex::Regex as FancyRegex;
use regex::Regex;

use crate::constantes::{PALABRAS_BASE, PALABRAS_CRITICAS, PALABRAS_PROHIBIDAS};

fn titulo_simple(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(primera) => primera.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
    }
}

/// Lista final de palabras "borradas" (se quitan del título, no descartan la
/// fila): base curada + variantes upper/lower/title de las críticas, sin
/// duplicados, preservando el orden de primera aparición.
pub fn palabras_borradas() -> Vec<String> {
    let mut vistas: HashSet<String> = HashSet::new();
    let mut salida = Vec::new();
    let mut agregar = |palabra: String| {
        if vistas.insert(palabra.clone()) {
            salida.push(palabra);
        }
    };
    for &p in PALABRAS_BASE {
        agregar(p.to_string());
    }
    for &p in PALABRAS_CRITICAS {
        agregar(p.to_uppercase());
        agregar(p.to_lowercase());
        agregar(titulo_simple(p));
    }
    salida
}

/// El mismo conjunto que [`palabras_borradas`], en minúsculas y como
/// `HashSet` para el borrado por token (búsqueda O(1)).
pub fn palabras_borradas_lower(palabras: &[String]) -> HashSet<String> {
    palabras.iter().map(|p| p.to_lowercase()).collect()
}

/// Todos los regex/patrones compilados UNA sola vez (equivalente a los
/// atributos precompilados de `ProcesadorPublicacionesGrandes.__init__`).
pub struct Patrones {
    pub prohibidas: Regex,
    pub re_x_hex: Regex,
    pub re_control: Regex,
    pub re_espacios_especiales: Regex,
    pub re_invisibles: Regex,
    pub re_espacios_multi: Regex,
    pub re_seis_digitos: Regex,
    pub re_secuencia: Regex,
    pub re_decimales: Regex,
    /// Núcleo de la Regla 6 SIN el lookahead: compatible con `regex` (Rust).
    /// Superset seguro del patrón completo — todo lo que casa el patrón
    /// completo casa este núcleo, así que sirve de prefiltro vectorizable.
    pub re_seg_prefiltro: Regex,
    /// Patrón COMPLETO de la Regla 6, con el lookahead original: necesita
    /// `fancy-regex` (el crate `regex` no soporta lookaround). Solo se
    /// evalúa sobre los pocos candidatos que pasan `re_seg_prefiltro`.
    pub re_segmentos: FancyRegex,
}

impl Patrones {
    pub fn compilar() -> Self {
        let mut escapadas: Vec<String> = PALABRAS_PROHIBIDAS.iter().map(|p| regex::escape(p)).collect();
        escapadas.sort_by_key(|b| std::cmp::Reverse(b.len()));
        let patron_prohibidas = format!(r"(?i)\b({})\b", escapadas.join("|"));

        let patron_secuencia: String = (1..=9)
            .map(|d| format!(r"\b{d}\d{{2,3}}(?:\s+{d}\d{{2,3}}){{2,}}\b"))
            .collect::<Vec<_>>()
            .join("|");

        Self {
            prohibidas: Regex::new(&patron_prohibidas).unwrap(),
            re_x_hex: Regex::new(r"_x[0-9A-Fa-f]{4}_").unwrap(),
            re_control: Regex::new(r"[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]").unwrap(),
            re_espacios_especiales: Regex::new(r"[\u{00A0}\u{2000}-\u{200A}\u{202F}\u{205F}\u{3000}]").unwrap(),
            re_invisibles: Regex::new(r"[\u{200B}\u{200C}\u{200D}\u{FEFF}]").unwrap(),
            re_espacios_multi: Regex::new(r"\s+").unwrap(),
            re_seis_digitos: Regex::new(r"\d{6}").unwrap(),
            re_secuencia: Regex::new(&patron_secuencia).unwrap(),
            re_decimales: Regex::new(r"(?:\d+\.\d+[\s/]){3,}\d+\.\d+").unwrap(),
            re_seg_prefiltro: Regex::new(r"(?:\b[a-zA-Z0-9]{3,4}[\s/]){3,}\b[a-zA-Z0-9]{3,4}\b").unwrap(),
            re_segmentos: FancyRegex::new(
                r"(?=(?:.*?\b(?:\w*\d\w*\d\w*\d\w*)\b){3,})(?:\b[a-zA-Z0-9]{3,4}[\s/]){3,}\b[a-zA-Z0-9]{3,4}\b",
            )
            .unwrap(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palabras_borradas_incluye_variantes_de_criticas_sin_duplicar() {
        let palabras = palabras_borradas();
        assert!(palabras.contains(&"MOPAR".to_string()));
        assert!(palabras.contains(&"mopar".to_string()));
        assert!(palabras.contains(&"Mopar".to_string()));
        assert!(palabras.contains(&"ACDelco".to_string()));
        assert!(palabras.contains(&"Acdelco".to_string()));
        let vistas: HashSet<&String> = palabras.iter().collect();
        assert_eq!(vistas.len(), palabras.len(), "sin duplicados");
    }

    #[test]
    fn prohibidas_detecta_palabra_completa_preservando_mayusculas() {
        let patrones = Patrones::compilar();
        let caps = patrones.prohibidas.captures("Radio Pantalla Tactil").unwrap();
        assert_eq!(&caps[1], "Radio");
        assert!(
            patrones.prohibidas.captures("Radiofrecuencia").is_none(),
            "solo palabra completa"
        );
    }

    #[test]
    fn segmentos_regla6_dispara_con_lookahead() {
        let patrones = Patrones::compilar();
        assert!(patrones
            .re_segmentos
            .is_match("Disco Clutch Caterpillar 3116 938g 3126 38f")
            .unwrap());
        assert!(!patrones
            .re_segmentos
            .is_match("Rueda Aluminio Foose Legend One 17x7 BMW 328i 09-14")
            .unwrap());
    }
}
