//! Una hoja física del libro y las reglas de nombre que impone Excel.
//!
//! Las reglas viven acá y no dispersas en el escritor porque son de Excel, no
//! de este programa: 31 caracteres, sin `[]:*?/\`, sin caracteres de control,
//! y sin nombres repetidos dentro del mismo libro.

use std::collections::HashSet;
use std::io;

use super::spool::SpoolTemp;

/// Caracteres que Excel prohíbe en un nombre de hoja.
const CARS_INVALIDOS: &[char] = &['[', ']', ':', '*', '?', '/', '\\'];

/// Largo máximo de un nombre de hoja en Excel.
const MAX_NOMBRE: usize = 31;

/// Una hoja física del libro. Su XML se acumula en un temporal (RAM→disco):
/// el elemento `<dimension>` va ANTES de los datos pero solo se conoce al
/// terminar la hoja (necesita el nº de filas).
pub(super) struct Hoja {
    pub(super) nombre: String,
    pub(super) col_final: String,
    pub(super) tmp: SpoolTemp,
    pub(super) filas: usize,
}

impl Hoja {
    pub(super) fn nueva(nombre: String, col_final: String, cabecera_xml: &str) -> io::Result<Self> {
        let mut tmp = SpoolTemp::nuevo();
        tmp.escribir(cabecera_xml.as_bytes())?; // cabecera = fila 1
        Ok(Self {
            nombre,
            col_final,
            tmp,
            filas: 0,
        })
    }
}

/// Deja `nombre` en un nombre de hoja que Excel acepta.
///
/// Además de los caracteres prohibidos, borra los de control ilegales en XML
/// 1.0 (0x00-0x1F): un nombre de hoja de un archivo de ORIGEN externo con un
/// byte de control crudo (p. ej. `\u{0}`) genera un `xl/workbook.xml`
/// inválido — el mismo caso que `escapar_texto_xml` cubre para el CONTENIDO
/// de las celdas.
pub(super) fn sanear(nombre: &str) -> String {
    let limpio: String = nombre
        .chars()
        .filter(|c| !CARS_INVALIDOS.contains(c) && (*c as u32) >= 0x20)
        .collect();
    let recortado = limpio.chars().take(MAX_NOMBRE).collect::<String>();
    let recortado = recortado.trim();
    if recortado.is_empty() {
        "Hoja".to_string()
    } else {
        recortado.to_string()
    }
}

/// Devuelve `candidato` si no está en `usados`, o la primera variante
/// `candidato_N` que no lo esté.
///
/// Cada variante se trunca siempre desde el nombre ORIGINAL, no desde la
/// anterior: así una segunda colisión da `Base_2` y no `Base_1_2`.
pub(super) fn nombre_unico(candidato: String, usados: &HashSet<String>) -> String {
    if !usados.contains(&candidato) {
        return candidato;
    }
    let mut k = 1u64;
    loop {
        let sufijo = format!("_{k}");
        let corte = MAX_NOMBRE.saturating_sub(sufijo.chars().count());
        let variante = candidato.chars().take(corte).collect::<String>() + &sufijo;
        if !usados.contains(&variante) {
            return variante;
        }
        k += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usados(nombres: &[&str]) -> HashSet<String> {
        nombres.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn sanear_quita_los_prohibidos_por_excel_y_los_de_control() {
        assert_eq!(sanear("Ven[tas]:2024*?/\\"), "Ventas2024");
        assert_eq!(sanear("con\u{0}nulo"), "connulo");
    }

    #[test]
    fn sanear_recorta_a_31_y_nunca_devuelve_vacio() {
        assert_eq!(sanear(&"a".repeat(50)).chars().count(), MAX_NOMBRE);
        assert_eq!(sanear("///"), "Hoja");
        assert_eq!(sanear("   "), "Hoja");
    }

    #[test]
    fn una_segunda_colision_numera_desde_el_original_no_desde_la_variante() {
        // Truncar desde la variante anterior daría "Base_1_2": el sufijo se
        // acumularía y a los pocos choques el nombre dejaría de ser legible.
        let ya = usados(&["Base", "Base_1"]);
        assert_eq!(nombre_unico("Base".to_string(), &ya), "Base_2");
    }

    #[test]
    fn el_nombre_desambiguado_sigue_cabiendo_en_31_caracteres() {
        let largo = "a".repeat(MAX_NOMBRE);
        let ya = usados(&[&largo]);
        let unico = nombre_unico(largo.clone(), &ya);
        assert_ne!(unico, largo);
        assert!(unico.chars().count() <= MAX_NOMBRE);
    }

    #[test]
    fn sin_colision_el_nombre_pasa_intacto() {
        assert_eq!(nombre_unico("Ventas".to_string(), &usados(&["Otra"])), "Ventas");
    }
}
