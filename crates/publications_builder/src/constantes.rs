use std::collections::HashMap;
use std::sync::LazyLock;

pub const CANTIDAD_CARACTERES_OEM3: usize = 250;
pub const MAX_ROWS_PER_SHEET: usize = 1_000_000;
/// Poner en `true` solo si el texto tiene símbolos rotos (mojibake): el
/// arreglo es más lento que dejarlo desactivado.
pub const USAR_REPARACION_MOJIBAKE: bool = false;

pub const COL_OEM: &str = "OEM";
pub const COL_START_URL: &str = "web-scraper-start-url";
pub const COL_PRECIO: &str = "Precio";
pub const COL_OPCION: &str = "Opcion";
pub const COL_TITULO: &str = "Titulo";
pub const COL_TIENDA: &str = "Tienda";
pub const COL_IMAGENES: [&str; 4] = ["Imagen 1", "Imagen 2", "Imagen 3", "Imagen 4"];
pub const COL_MOTIVO_ELIMINACION: &str = "Motivo_Eliminacion";

/// `https?` (no solo `https`): una imagen con `http://` se descartaba en
/// silencio (quedaba celda vacía). eBay usa https, pero así no se pierde
/// ninguna.
pub const URL_REGEX: &str = r#"(https?[^\s"]+)"#;
pub const URL_REPLACE_REGEX: &str = r"/[^/]*$";
pub const LINEA_EN_MODELO_PATTERN: &str = r"\s*\b\d\.\d[T]?\b\s*";

/// Columnas que este builder GENERA (reservadas): si el archivo de entrada
/// trajera una con estos nombres, el proceso la sobrescribiría EN SILENCIO.
pub const COLUMNAS_RESERVADAS: &[&str] = &[
    COL_MOTIVO_ELIMINACION,
    "Sku",
    "Precio2",
    "OEM3",
    "Combinada",
    "Caracteristicas",
    "Cantidades",
];

pub fn columnas_reservadas_en<'a>(columnas: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    commerce_core::columnas_reservadas_en(columnas, COLUMNAS_RESERVADAS.iter().copied())
}

/// `Err` si `columnas` trae nombres reservados del builder; `contexto`
/// identifica de dónde vienen para el mensaje (p. ej. "La hoja 'X'",
/// "'Hoja1'"). Centraliza acá un chequeo que, de otro modo, se repetiría en
/// unicas/amazon/compatibilidad y podría desincronizarse entre copias.
pub fn verificar_columnas_reservadas<'a>(
    columnas: impl IntoIterator<Item = &'a str>,
    contexto: &str,
) -> commerce_core::CoreResult<()> {
    let reservadas = columnas_reservadas_en(columnas);
    if reservadas.is_empty() {
        return Ok(());
    }
    Err(commerce_core::CoreError::Io(std::io::Error::other(format!(
        "{contexto} trae columnas reservadas del builder: {}. Renómbralas en el archivo y reintenta.",
        reservadas.join(", ")
    ))))
}

/// Rangos de precio (en USD, límites inclusive) → precio de lista asignado.
/// Se recorren en orden; el primer rango que contiene el precio gana.
///
/// ATENCIÓN — tramo no monotónico sin explicar, pendiente de confirmación de
/// negocio: `(175, 179) → 16980` es seguido por `(180, 184) → 16195` (BAJA)
/// y luego `(185, 189) → 16680` (vuelve a subir). En todo otro tramo el
/// precio de lista sube o se mantiene al subir el rango de compra; este es
/// el único que no. Puede ser un ajuste puntual de margen o un error de
/// captura: no se modifica sin confirmación. El test
/// `tramo_175_189_mantiene_su_forma_no_monotonica_conocida` fija el
/// comportamiento vigente para que cambiarlo sea deliberado.
pub static RANGOS_PRECIOS: &[((i64, i64), i64)] = &[
    ((1, 14), 5749),
    ((15, 19), 6472),
    ((20, 24), 7021),
    ((25, 29), 7252),
    ((30, 39), 7483),
    ((40, 44), 7882),
    ((45, 49), 8113),
    ((50, 54), 8344),
    ((55, 59), 8575),
    ((60, 64), 8806),
    ((65, 69), 9037),
    ((70, 74), 9268),
    ((75, 79), 9500),
    ((80, 84), 9731),
    ((85, 89), 9962),
    ((90, 94), 10193),
    ((95, 99), 10424),
    ((100, 109), 10719),
    ((110, 119), 11181),
    ((120, 129), 11643),
    ((130, 139), 12296),
    ((140, 149), 12949),
    ((150, 159), 13919),
    ((160, 164), 14889),
    ((165, 169), 15692),
    ((170, 174), 16495),
    ((175, 179), 16980),
    ((180, 184), 16195),
    ((185, 189), 16680),
    ((190, 199), 17165),
    ((200, 224), 18136),
    ((225, 249), 19291),
    ((250, 274), 20447),
    ((275, 299), 21603),
    ((300, 324), 22758),
    ((325, 349), 24232),
    ((350, 374), 25705),
    ((375, 399), 27178),
    ((400, 424), 28651),
    ((425, 449), 30759),
    ((450, 474), 32868),
    ((475, 499), 34976),
    ((500, 524), 37084),
    ((525, 549), 39192),
    ((550, 574), 41300),
    ((575, 599), 43409),
    ((600, 624), 45517),
    ((625, 649), 47625),
    ((650, 699), 49733),
    ((700, 724), 52680),
    ((725, 749), 54788),
    ((750, 799), 56896),
    ((800, 824), 59842),
    ((825, 849), 61316),
    ((850, 874), 62789),
    ((875, 899), 64262),
    ((900, 924), 65735),
    ((925, 949), 67843),
    ((950, 999), 69317),
    ((1000, 1025), 71628),
];

pub static MAPEO_CARACTERISTICAS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    HashMap::from([
        ("front", "frontal"),
        ("rear", "trasero"),
        ("lh", "izq"),
        ("left", "izq"),
        ("driver", "izq"),
        ("rh", "der"),
        ("right", "der"),
        ("passenger", "der"),
        ("intake", "admision"),
        ("exhaust", "escape"),
        ("upper", "superior"),
        ("lower", "inferior"),
    ])
});

pub static MAPEO_CANTIDADES: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| HashMap::from([("kit", "kit"), ("pair", "par"), ("set", "set"), ("pcs", "set")]));

pub static PARES_OPUESTOS: &[(&str, &str)] = &[
    ("frontal", "trasero"),
    ("izq", "der"),
    ("admision", "escape"),
    ("superior", "inferior"),
];

/// Precio de lista para `precio` según [`RANGOS_PRECIOS`] (`None` fuera de rango).
pub fn precio_de_rango(precio: i64) -> Option<i64> {
    RANGOS_PRECIOS
        .iter()
        .find(|((min, max), _)| precio >= *min && precio <= *max)
        .map(|(_, valor)| *valor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detecta_columnas_reservadas() {
        assert_eq!(columnas_reservadas_en(["Sku", "Titulo"]), vec!["Sku".to_string()]);
        assert!(columnas_reservadas_en(["Titulo"]).is_empty());
    }

    #[test]
    fn verificar_columnas_reservadas_incluye_el_contexto_en_el_mensaje() {
        assert!(verificar_columnas_reservadas(["Titulo"], "La hoja 'X'").is_ok());
        let error = verificar_columnas_reservadas(["Sku", "Titulo"], "La hoja 'X'").unwrap_err();
        let mensaje = error.to_string();
        assert!(mensaje.contains("La hoja 'X'"));
        assert!(mensaje.contains("Sku"));
    }

    #[test]
    fn precio_de_rango_primer_y_ultimo_bucket() {
        assert_eq!(precio_de_rango(1), Some(5749));
        assert_eq!(precio_de_rango(1025), Some(71628));
        assert_eq!(precio_de_rango(1026), None);
        assert_eq!(precio_de_rango(0), None);
    }

    #[test]
    fn tramo_175_189_mantiene_su_forma_no_monotonica_conocida() {
        // Ver `RANGOS_PRECIOS`: este tramo sube-baja-sube sin explicación,
        // pendiente de confirmación de negocio. El test no valida que sea
        // CORRECTO, solo que no cambie por accidente. Si empieza a fallar
        // porque la tabla se corrigió a monotónica, actualizarlo (no
        // borrarlo) con la forma confirmada.
        assert_eq!(precio_de_rango(177), Some(16980));
        assert_eq!(precio_de_rango(182), Some(16195));
        assert_eq!(precio_de_rango(187), Some(16680));
    }

    #[test]
    fn rangos_precios_es_monotonico_no_decreciente_salvo_el_tramo_175_189_ya_documentado() {
        // Guarda de regresión para el RESTO de la tabla: si un futuro cambio
        // introdujera otra baja no explicada en cualquier otro tramo, este
        // test debe fallar y forzar la misma pregunta que ya se hizo para
        // 175-189 (¿regla de negocio real o error de captura?), en vez de
        // que quede sin detectar hasta que alguien lo note en producción.
        let excepcion_conocida = ((180, 184), 16195i64);
        let mut anterior: Option<i64> = None;
        for &(rango, valor) in RANGOS_PRECIOS {
            if (rango, valor) == excepcion_conocida {
                anterior = Some(valor);
                continue;
            }
            if let Some(prev) = anterior {
                assert!(
                    valor >= prev,
                    "tramo {rango:?} (valor {valor}) baja respecto al anterior ({prev}) \
                     y no es la excepción ya documentada (180,184) — ¿nueva anomalía sin confirmar?"
                );
            }
            anterior = Some(valor);
        }
    }
}
