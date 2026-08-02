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
/// Los datos viven en `datos/rangos_precios.csv` (un tramo por línea,
/// `min,max,precio`; `#` para comentar): un ajuste de precios produce un
/// diff de esa tabla, no del código fuente. La nota de negocio sobre el
/// tramo 175-189, que no es monotónico, está en la cabecera de ese archivo.
pub static RANGOS_PRECIOS: LazyLock<Vec<((i64, i64), i64)>> =
    LazyLock::new(|| tramos_de(include_str!("datos/rangos_precios.csv")));

/// Parsea `min,max,precio` por línea, ignorando vacías y las que empiezan
/// con `#`.
///
/// # Panics
/// Si una línea con datos no tiene los tres campos o alguno no es entero:
/// la tabla se compila DENTRO del binario, así que un formato inválido es un
/// error de programación y debe verse en la primera corrida, no degradar en
/// silencio a un tramo faltante (que daría un precio de lista incorrecto).
#[allow(clippy::expect_used)]
fn tramos_de(contenido: &'static str) -> Vec<((i64, i64), i64)> {
    contenido
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|linea| {
            let mut campos = linea.split(',').map(str::trim);
            let mut entero = |cual: &str| -> i64 {
                campos
                    .next()
                    .and_then(|c| c.parse().ok())
                    .unwrap_or_else(|| panic!("rangos_precios.csv: {cual} inválido en '{linea}'"))
            };
            let (min, max, precio) = (entero("mínimo"), entero("máximo"), entero("precio"));
            ((min, max), precio)
        })
        .collect()
}

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
        for &(rango, valor) in RANGOS_PRECIOS.iter() {
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
