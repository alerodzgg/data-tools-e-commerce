//! Parseo y reconstrucción de los enlaces de tienda de eBay.
//!
//! Un enlace de tienda termina siempre en el mismo bloque de parámetros: el
//! rango de precio (`_udlo`/`_udhi`), el orden (`_sop`) y el tamaño de página
//! (`_ipg`). Fragmentar la tienda es reescribir ESE bloque muchas veces
//! cambiando solo el rango, dejando intacto todo lo que venga antes.
//!
//! Por qué esto no se hace con un `replace` de texto: el bloque aparece en el
//! archivo real con `_sop` en DOS posiciones distintas (antes del rango en
//! unos enlaces, después de `_ipg` en otros). Un `replace` ciego sobre
//! `_udlo=10&_udhi=500` funcionaría en los dos, pero también "funcionaría"
//! sobre un enlace que no es de tienda —una búsqueda cualquiera, una URL
//! pegada a mano, una celda con basura— y generaría decenas de enlaces rotos
//! sin un solo aviso. Acá la forma del sufijo se RECONOCE, y lo que no encaja
//! se rechaza con un motivo concreto.

/// Marca dónde empieza el bloque de parámetros que este módulo reescribe.
/// Todo lo anterior es la base de la tienda y se copia tal cual.
pub const ANCLA: &str = "&isRefine=true";

/// Dónde va `_sop` dentro del sufijo.
///
/// Un enum y no un `bool`: las dos formas se reconstruyen distinto, y un
/// `match` exhaustivo obliga a que agregar una tercera forma en el futuro
/// rompa la compilación en vez de caer en silencio en una rama `else`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormaSufijo {
    /// `&isRefine=true&_sop=15&_udlo=10&_udhi=500&_ipg=240`
    SopAntesDelRango,
    /// `&isRefine=true&_udlo=10&_udhi=500&_ipg=240&_sop=16`
    SopAlFinal,
}

/// Por qué un enlace no se pudo fragmentar.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ErrorEnlace {
    #[error("la celda del enlace está vacía")]
    Vacio,
    #[error("no contiene '{ANCLA}': no parece un enlace de tienda de eBay")]
    SinAncla,
    #[error(
        "el bloque de parámetros tras '{ANCLA}' no es ninguna de las dos formas conocidas \
         (_sop antes o después del rango); se leyó: '{0}'"
    )]
    SufijoDesconocido(String),
}

/// Un enlace de tienda ya descompuesto: la base, la forma del sufijo, y los
/// dos parámetros que se conservan al fragmentar.
///
/// Solo se construye vía [`EnlaceTienda::parsear`], así que un valor de este
/// tipo YA es un enlace fragmentable: el resto del módulo no vuelve a
/// preguntarse si la URL era válida.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnlaceTienda {
    base: String,
    forma: FormaSufijo,
    sop: String,
    ipg: String,
}

impl EnlaceTienda {
    pub fn parsear(url: &str) -> Result<Self, ErrorEnlace> {
        let url = url.trim();
        if url.is_empty() {
            return Err(ErrorEnlace::Vacio);
        }
        // `rfind` y no `find`: el ancla marca el comienzo del bloque FINAL. Si
        // un enlace la trajera repetida antes, partir por la primera dejaría
        // el resto dentro del "sufijo" y lo volvería irreconocible.
        let corte = url.rfind(ANCLA).ok_or(ErrorEnlace::SinAncla)?;
        let base = url[..corte].to_string();
        let cola = &url[corte + ANCLA.len()..];

        let mut pares: Vec<(&str, &str)> = Vec::new();
        for par in cola.split('&').filter(|p| !p.is_empty()) {
            let (clave, valor) = par
                .split_once('=')
                .ok_or_else(|| ErrorEnlace::SufijoDesconocido(cola.to_string()))?;
            pares.push((clave, valor));
        }

        let claves: Vec<&str> = pares.iter().map(|(c, _)| *c).collect();
        let forma = match claves.as_slice() {
            ["_sop", "_udlo", "_udhi", "_ipg"] => FormaSufijo::SopAntesDelRango,
            ["_udlo", "_udhi", "_ipg", "_sop"] => FormaSufijo::SopAlFinal,
            _ => return Err(ErrorEnlace::SufijoDesconocido(cola.to_string())),
        };

        // Los valores se copian tal cual, sin parsearlos a número: `_sop` e
        // `_ipg` viajan de la entrada a la salida sin que este módulo los
        // interprete, así que exigirles ser numéricos rechazaría enlaces que
        // eBay acepta sin ganar nada a cambio.
        let buscar = |clave: &str| {
            pares
                .iter()
                .find(|(c, _)| *c == clave)
                .map(|(_, v)| (*v).to_string())
                .unwrap_or_default()
        };
        Ok(Self {
            base,
            forma,
            sop: buscar("_sop"),
            ipg: buscar("_ipg"),
        })
    }

    /// El mismo enlace con el rango de precio `lo`–`hi`.
    pub fn con_rango(&self, lo: u32, hi: u32) -> String {
        let Self { base, sop, ipg, .. } = self;
        match self.forma {
            FormaSufijo::SopAntesDelRango => {
                format!("{base}{ANCLA}&_sop={sop}&_udlo={lo}&_udhi={hi}&_ipg={ipg}")
            }
            FormaSufijo::SopAlFinal => {
                format!("{base}{ANCLA}&_udlo={lo}&_udhi={hi}&_ipg={ipg}&_sop={sop}")
            }
        }
    }

    pub fn forma(&self) -> FormaSufijo {
        self.forma
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CASO_A: &str =
        "https://www.ebay.com/sch/i.html?sid=subarupartsdirect&isRefine=true&_sop=15&_udlo=10&_udhi=500&_ipg=240";
    const CASO_B: &str =
        "https://www.ebay.com/sch/i.html?sid=subarupartsdirect&isRefine=true&_udlo=10&_udhi=500&_ipg=240&_sop=16";

    #[test]
    fn reconoce_las_dos_formas_del_sufijo() {
        assert_eq!(
            EnlaceTienda::parsear(CASO_A).map(|e| e.forma()),
            Ok(FormaSufijo::SopAntesDelRango)
        );
        assert_eq!(
            EnlaceTienda::parsear(CASO_B).map(|e| e.forma()),
            Ok(FormaSufijo::SopAlFinal)
        );
    }

    #[test]
    fn cada_forma_se_reconstruye_con_sus_parametros_en_su_lugar() {
        // Lo que hay que preservar no es solo el rango: el ORDEN de los
        // parámetros y el valor de `_sop` distinguen un enlace del otro, y
        // mezclarlos daría una página de resultados distinta.
        let a = EnlaceTienda::parsear(CASO_A).expect("caso A");
        assert_eq!(
            a.con_rango(21, 30),
            "https://www.ebay.com/sch/i.html?sid=subarupartsdirect&isRefine=true&_sop=15&_udlo=21&_udhi=30&_ipg=240"
        );

        let b = EnlaceTienda::parsear(CASO_B).expect("caso B");
        assert_eq!(
            b.con_rango(21, 30),
            "https://www.ebay.com/sch/i.html?sid=subarupartsdirect&isRefine=true&_udlo=21&_udhi=30&_ipg=240&_sop=16"
        );
    }

    #[test]
    fn la_base_de_la_tienda_se_copia_intacta() {
        // El `sid` es lo que identifica la tienda: perderlo generaría decenas
        // de enlaces a la búsqueda global de eBay en vez de a la tienda.
        let url =
            "https://www.ebay.com/sch/i.html?_nkw=turbo&sid=abc&_fsrp=1&isRefine=true&_sop=15&_udlo=10&_udhi=500&_ipg=240";
        let e = EnlaceTienda::parsear(url).expect("parsea");
        assert!(e
            .con_rango(10, 20)
            .starts_with("https://www.ebay.com/sch/i.html?_nkw=turbo&sid=abc&_fsrp=1&isRefine=true"));
    }

    #[test]
    fn un_enlace_que_no_es_de_tienda_se_rechaza_en_vez_de_fragmentarse() {
        // Sin esto, una celda con una URL cualquiera produciría decenas de
        // filas de basura indistinguibles de las buenas.
        assert_eq!(
            EnlaceTienda::parsear("https://www.ebay.com/itm/123456"),
            Err(ErrorEnlace::SinAncla)
        );
    }

    #[test]
    fn un_sufijo_con_otros_parametros_no_se_da_por_bueno() {
        let url = "https://www.ebay.com/sch/i.html?sid=x&isRefine=true&_udlo=10&_udhi=500";
        assert!(matches!(
            EnlaceTienda::parsear(url),
            Err(ErrorEnlace::SufijoDesconocido(_))
        ));
    }

    #[test]
    fn una_celda_vacia_se_distingue_de_una_url_mal_formada() {
        // El llamador reporta distinto: una celda vacía es un dato que falta,
        // una URL rota es un dato equivocado.
        assert_eq!(EnlaceTienda::parsear("   "), Err(ErrorEnlace::Vacio));
    }

    #[test]
    fn se_parte_por_el_ancla_final_y_no_por_la_primera() {
        let url =
            "https://www.ebay.com/sch/i.html?sid=x&isRefine=true&otro=1&isRefine=true&_sop=15&_udlo=10&_udhi=500&_ipg=240";
        let e = EnlaceTienda::parsear(url).expect("parsea");
        assert_eq!(
            e.con_rango(10, 20),
            "https://www.ebay.com/sch/i.html?sid=x&isRefine=true&otro=1&isRefine=true&_sop=15&_udlo=10&_udhi=20&_ipg=240"
        );
    }

    #[test]
    fn los_valores_de_sop_e_ipg_de_la_entrada_se_conservan() {
        let url = "https://x/?a=1&isRefine=true&_sop=12&_udlo=10&_udhi=500&_ipg=200";
        let e = EnlaceTienda::parsear(url).expect("parsea");
        assert_eq!(
            e.con_rango(10, 20),
            "https://x/?a=1&isRefine=true&_sop=12&_udlo=10&_udhi=20&_ipg=200"
        );
    }
}
