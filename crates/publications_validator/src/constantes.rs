use std::sync::LazyLock;

pub const COL_TRADUCIDO: &str = "Traducido";
pub const COL_MOTIVO_ELIMINACION: &str = "Motivo_Eliminacion";
pub const COL_HOJA_ORIGEN: &str = "Hoja_origen";
pub const COL_PRECIO2: &str = "Precio2";
pub const COL_OEM: &str = "OEM";
pub const CANTIDAD_CARACTERES_OEM3: usize = 250;
pub const MAX_ROWS_PER_SHEET: usize = 1_000_000;

/// Columnas que el validator genera/usa internamente. Si el archivo de
/// entrada trajera una así, la clasificación quedaría corrupta EN SILENCIO.
pub const COLUMNAS_RESERVADAS: &[&str] = &[
    COL_MOTIVO_ELIMINACION,
    COL_HOJA_ORIGEN,
    "OEM3",
    "_salida",
    "_pid",
    "_prohibida",
    "_motivo_regla",
    "_cond_seg",
    "_preseg",
    "_i",
];

pub fn columnas_reservadas_en<'a>(columnas: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    commerce_core::columnas_reservadas_en(columnas, COLUMNAS_RESERVADAS.iter().copied())
}

/// Nombre de hoja reservado para el reporte (comparación insensible a
/// mayúsculas/espacios): una hoja de ENTRADA llamada así mezclaría sus
/// filas supervivientes dentro del reporte de eliminadas.
pub const HOJA_RESERVADA: &str = "eliminadas";

/// Particiones mínimas del dedup a disco (caso grande).
pub const PARTICIONES_DEDUPLICACION: usize = 8;

/// Umbral del dedup HÍBRIDO: si las filas estimadas caben bajo esto, se
/// deduplica EN MEMORIA (sin disco, más rápido); por encima, se particiona
/// a disco. ~3M es holgado para 8GB; súbelo si hay más RAM disponible.
pub const UMBRAL_RAM_DEDUP: u64 = 3_000_000;

/// Palabras críticas cuya variante upper/lower/title se añade explícitamente
/// (deben coincidir sin importar mayúsculas, con independencia de la forma
/// ya presente en [`PALABRAS_BASE`]).
/// Marcas y modelos de coches, camiones, diésel, 4x4 y motos ya excluidos
/// de esta lista (curada): quedan marcas de repuestos, accesorios,
/// herramientas y químicos.
///
/// Los datos viven en `datos/palabras_base.txt` (una entrada por línea, `#`
/// para comentar): un alta o baja de marca produce un diff de esa lista, no
/// del código fuente.
pub static PALABRAS_BASE: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| lineas_de(include_str!("datos/palabras_base.txt")));

/// Palabras que deben filtrar la fila COMPLETA (marca/modelo prohibido).
///
/// Los datos viven en `datos/palabras_prohibidas.txt` (ver [`PALABRAS_BASE`]).
pub static PALABRAS_PROHIBIDAS: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| lineas_de(include_str!("datos/palabras_prohibidas.txt")));

/// Una entrada por línea; se ignoran las vacías y las que empiezan con `#`.
fn lineas_de(contenido: &'static str) -> Vec<&'static str> {
    contenido
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

pub const PALABRAS_CRITICAS: &[&str] = &["Mopar", "Motorcraft", "ACDelco"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detecta_columnas_reservadas() {
        assert_eq!(
            columnas_reservadas_en(["Traducido", "OEM3"]),
            vec!["OEM3".to_string()]
        );
        assert!(columnas_reservadas_en(["Traducido"]).is_empty());
    }
}
