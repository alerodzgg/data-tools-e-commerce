use commerce_core::{como_numero_finito, recortar_espacios_orden};

// El orden estilo Excel en memoria (`ordenar_excel_df`) vive en `commerce_core`
// junto con las funciones de recorte/parseo: es la MISMA lógica que usa ETL
// tools para su modo "Ordenar columnas", así que se comparte en un solo
// sitio en vez de mantener dos copias que puedan divergir.
pub use commerce_core::ordenar_excel_df;

/// Grupo de ordenamiento de una clave: vacíos siempre al final (grupo más
/// alto), luego números, luego texto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Grupo {
    Numero,
    Texto,
    Vacio,
}

/// Clave de orden Excel para la mezcla externa (fusión k-vías), equivalente
/// a `ordenar_excel_df`: usa los MISMOS helpers de recorte/parseo de
/// `commerce_core`, así que ambos caminos (memoria y disco) no pueden
/// divergir por una diferencia de implementación entre uno y otro.
#[derive(Debug, Clone)]
pub struct ClaveExcel {
    grupo: Grupo,
    numero: f64,
    texto: String,
    ascendente: bool,
}

impl ClaveExcel {
    pub fn nueva(valor: &str, ascendente: bool) -> Self {
        let limpio = recortar_espacios_orden(valor);
        if limpio.is_empty() {
            return Self {
                grupo: Grupo::Vacio,
                numero: 0.0,
                texto: String::new(),
                ascendente,
            };
        }
        match como_numero_finito(limpio) {
            Some(f) => Self {
                grupo: Grupo::Numero,
                numero: f,
                texto: limpio.to_lowercase(),
                ascendente,
            },
            None => Self {
                grupo: Grupo::Texto,
                numero: 0.0,
                texto: limpio.to_lowercase(),
                ascendente,
            },
        }
    }
}

// `PartialEq` derivada por campo incluiría `ascendente`, pero `cmp` NO lo usa
// para decidir igualdad (solo invierte el resultado ya calculado): dos
// claves con `grupo`/`numero`/`texto` iguales pero `ascendente` distinto
// daban `cmp() == Equal` y a la vez `derived_eq() == false`, violando el
// contrato de que `Eq`/`Ord` deben ser consistentes entre sí (puede romper
// invariantes de estructuras que asumen esa consistencia, como `dedup()`
// tras un `sort()`). Definir `eq` en términos de `cmp` lo garantiza siempre,
// sin depender de mantener ambos en sincro a mano.
impl PartialEq for ClaveExcel {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}
impl Eq for ClaveExcel {}

impl PartialOrd for ClaveExcel {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ClaveExcel {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        // Los vacíos van SIEMPRE al final, sin importar el sentido.
        if self.grupo == Grupo::Vacio || other.grupo == Grupo::Vacio {
            return self.grupo.cmp(&other.grupo);
        }
        let orden = self
            .grupo
            .cmp(&other.grupo)
            .then_with(|| self.numero.partial_cmp(&other.numero).unwrap_or(Ordering::Equal))
            .then_with(|| self.texto.cmp(&other.texto));
        if self.ascendente {
            orden
        } else {
            orden.reverse()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clave_excel_vacios_siempre_al_final() {
        let mut valores = vec!["b", "", "a", "10", "2"];
        valores.sort_by(|a, b| ClaveExcel::nueva(a, true).cmp(&ClaveExcel::nueva(b, true)));
        assert_eq!(valores.last(), Some(&""));
        let (numeros, _): (Vec<_>, Vec<_>) = valores.into_iter().partition(|v| *v == "2" || *v == "10");
        assert_eq!(numeros, vec!["2", "10"]);
    }

    #[test]
    fn clave_excel_eq_consistente_con_ord_sin_importar_ascendente() {
        // Antes: `PartialEq` derivada comparaba también `ascendente`, pero
        // `cmp` lo ignora para decidir igualdad (solo invierte el resultado
        // ya calculado) — dos claves iguales en todo menos `ascendente` daban
        // `cmp() == Equal` y `derived_eq() == false` a la vez, violando el
        // contrato Eq/Ord.
        let asc = ClaveExcel::nueva("Honda", true);
        let desc = ClaveExcel::nueva("Honda", false);
        assert_eq!(asc.cmp(&desc), std::cmp::Ordering::Equal);
        assert_eq!(asc, desc, "eq debe ser consistente con cmp==Equal");
    }

    #[test]
    fn clave_excel_desempata_numeros_iguales_por_texto() {
        let a = ClaveExcel::nueva("2.0", true);
        let b = ClaveExcel::nueva("2", true);
        assert_eq!(
            a.cmp(&b),
            std::cmp::Ordering::Greater,
            "'2' ordena antes que '2.0'"
        );
    }
}
