//! Helpers de interfaz compartidos por los modos de `etl_tools`: selección
//! de archivos/columnas/hojas y escritura por bloques.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use commerce_core::EscritorXlsx;
use etl_tools::constantes::SOPORTADOS_ARCHIVOS;
use polars::prelude::*;

use crate::{AppResult, FILAS_POR_BLOQUE_ESCRITURA};

// ════════════════════════════════════════════════════════════════════════
// Helpers compartidos
// ════════════════════════════════════════════════════════════════════════

fn listar_archivos_soportados(entrada: &Path) -> Vec<PathBuf> {
    let mut archivos: Vec<PathBuf> = std::fs::read_dir(entrada)
        .into_iter()
        .flatten()
        .filter_map(|r| r.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| SOPORTADOS_ARCHIVOS.contains(&e.to_lowercase().as_str()))
                    .unwrap_or(false)
        })
        .collect();
    archivos.sort();
    archivos
}

pub(crate) fn seleccionar_archivo(mensaje: &str) -> AppResult<Option<PathBuf>> {
    let entrada = app_shell::ruta_entrada();
    let archivos = listar_archivos_soportados(&entrada);
    if archivos.is_empty() {
        app_shell::error(&format!("No se encontraron archivos en '{}'", entrada.display()));
        return Ok(None);
    }
    Ok(app_shell::elegir_archivo(mensaje, archivos)?)
}

pub(crate) fn excluir_refs(excluir: &HashSet<String>) -> Vec<&str> {
    excluir.iter().map(String::as_str).collect()
}

/// Renombra `origen` a `destino` con un mensaje de error claro si falla, en
/// vez de propagar el `io::Error` genérico del SO. En Windows, sobrescribir
/// un `.xlsx` que el usuario tiene abierto en Excel es el caso típico de
/// fallo acá: sin este mensaje, el usuario ve un error críptico y no sabe
/// que sus datos ya procesados quedaron a salvo en `origen`, sin renombrar.
pub(crate) fn renombrar_o_avisar(origen: &commerce_core::RutaEscritaReal, destino: &Path) -> AppResult<()> {
    std::fs::rename(origen, destino).map_err(|e| {
        std::io::Error::other(format!(
            "No se pudo sobrescribir '{}' (¿está abierto en Excel u otro programa? \
             ciérralo e intenta de nuevo): {e}. Los datos procesados quedaron en '{}'.",
            destino.display(),
            origen.display()
        ))
        .into()
    })
}

/// Gate Sí/No + checkbox sobre las hojas reales. `None` si se canceló o se
/// excluyó todo. Envuelve `app_shell::preguntar_hojas_excluir` para un solo
/// archivo (uso más común en este binario).
pub(crate) fn preguntar_hojas_excluir_de(
    archivo: &Path,
    etiqueta: &str,
) -> AppResult<Option<HashSet<String>>> {
    Ok(app_shell::preguntar_hojas_excluir(
        std::slice::from_ref(&archivo.to_path_buf()),
        etiqueta,
    )?)
}

/// A diferencia de `app_shell::abortar_si_reservadas` (chequeo exacto), acá
/// hace falta además el matching por prefijo de `_font_color_*` — por eso
/// esta versión propia, pero reusando el mismo mensaje vía
/// `avisar_si_hay_reservadas` en vez de duplicarlo a mano.
pub(crate) fn abortar_si_reservadas(columnas: &[String]) -> bool {
    let chocan = etl_tools::columnas_reservadas_presentes(columnas);
    app_shell::avisar_si_hay_reservadas(&chocan)
}

pub(crate) fn seleccionar_columnas(columnas: &[String], para: &str) -> AppResult<Vec<String>> {
    if columnas.is_empty() {
        app_shell::warn("No hay columnas disponibles.");
        return Ok(Vec::new());
    }
    Ok(app_shell::menu_multiple(
        &format!("Columnas para {para}:"),
        columnas.to_vec(),
    )?)
}

pub(crate) fn seleccionar_columna_clave(columnas: &[String]) -> AppResult<Option<String>> {
    if columnas.is_empty() {
        app_shell::warn("No hay columnas disponibles.");
        return Ok(None);
    }
    Ok(app_shell::menu_seleccionar(
        "Elige la columna clave:",
        columnas.to_vec(),
    )?)
}

/// Opción de menú que transporta el VALOR elegido y muestra una etiqueta
/// armada aparte. Es lo que permite que un menú devuelva directamente el
/// tipo que el llamador necesita, en lugar de un texto que deba
/// re-interpretarse comparando posiciones.
///
/// Se usa cuando la etiqueta depende del contexto (el nombre del archivo en
/// curso, por ejemplo); si es fija, alcanza con implementar `Display` sobre
/// el propio tipo y pasarlo directo al menú.
pub(crate) struct Etiquetada<T> {
    pub(crate) valor: T,
    etiqueta: String,
}

impl<T> Etiquetada<T> {
    pub(crate) fn nueva(valor: T, etiqueta: impl Into<String>) -> Self {
        Self {
            valor,
            etiqueta: etiqueta.into(),
        }
    }
}

impl<T> std::fmt::Display for Etiquetada<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.etiqueta)
    }
}

/// Junta bloques YA CARGADOS (p. ej. por [`etl_tools::iter_hojas_valores`])
/// en un único `DataFrame`, alineado a `columnas`. No abre ningún archivo:
/// los llamadores que necesitan columnas + datos del mismo archivo cargan
/// los bloques UNA sola vez y los pasan acá, en vez de que esta función
/// volviera a leerlos (esa doble apertura era justo el problema que este
/// cambio corrige).
pub(crate) fn armar_desde_bloques(
    bloques: Vec<DataFrame>,
    columnas: &[String],
) -> AppResult<Option<DataFrame>> {
    let mut partes = Vec::new();
    for chunk in bloques {
        let chunk = etl_tools::preparar_chunk(&chunk, columnas)?;
        partes.push(chunk.select(columnas)?);
    }
    if partes.is_empty() {
        return Ok(None);
    }
    let mut base = partes.remove(0);
    for parte in partes {
        base.vstack_mut_owned(parte)?;
    }
    Ok(Some(base))
}

pub(crate) fn escribir_por_bloques(
    escritor: &mut EscritorXlsx,
    df: &DataFrame,
    hoja: Option<&str>,
    barra: &indicatif::ProgressBar,
) -> AppResult<()> {
    let mut inicio = 0i64;
    while (inicio as usize) < df.height() {
        let bloque = df.slice(inicio, FILAS_POR_BLOQUE_ESCRITURA);
        escritor.escribir(&bloque, hoja)?;
        barra.inc(bloque.height() as u64);
        inicio += FILAS_POR_BLOQUE_ESCRITURA as i64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renombrar_o_avisar_mueve_el_archivo_en_el_camino_feliz() {
        let tmp = tempfile::tempdir().unwrap();
        let origen = tmp.path().join("origen.xlsx");
        let destino = tmp.path().join("destino.xlsx");
        std::fs::write(&origen, b"contenido").unwrap();
        let origen = commerce_core::RutaEscritaReal::nueva(origen);

        renombrar_o_avisar(&origen, &destino).unwrap();

        assert!(!origen.as_path().exists());
        assert_eq!(std::fs::read(&destino).unwrap(), b"contenido");
    }

    #[test]
    fn renombrar_o_avisar_da_un_mensaje_claro_si_falla() {
        let tmp = tempfile::tempdir().unwrap();
        let origen = tmp.path().join("no_existe.xlsx");
        let destino = tmp.path().join("destino.xlsx");
        let origen = commerce_core::RutaEscritaReal::nueva(origen);

        let err = renombrar_o_avisar(&origen, &destino).unwrap_err().to_string();

        assert!(
            err.contains(&destino.display().to_string()) && err.contains(&origen.display().to_string()),
            "el mensaje debe mencionar tanto el destino como dónde quedaron los datos: {err}"
        );
    }
}
