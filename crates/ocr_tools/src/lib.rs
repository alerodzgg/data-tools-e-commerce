//! ocr_tools — motor OCR (CRAFT + reconocedor VGG-BiLSTM-CTC) vía ONNX Runtime.

// Política de la Ronda 9 de auditoría (ver docs/decisiones/0006-tests-reactivos-vs-invariantes.md):
// este crate procesa entrada no confiable (imágenes descargadas de red,
// .xlsx de terceros, salidas de un modelo ONNX) en un pipeline async de
// larga duración — un panic ahí tumba el proceso completo y pierde trabajo
// ya hecho. `cfg(test)` desactiva el lint para el propio código de test.
#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]

use std::path::{Path, PathBuf};

/// Carpeta donde viven `models/` (pesos ONNX) y `runtime/` (motor de
/// inferencia dinámico). Se resuelve en tiempo de EJECUCIÓN, no de
/// compilación, para que el binario funcione instalado en cualquier ruta,
/// probando en este orden (el primero que resulte válido gana):
///
/// 1. La variable de entorno `OCR_TOOLS_ASSETS_DIR`, si apunta a una carpeta
///    que de verdad existe — es una señal EXPLÍCITA del usuario/instalador,
///    así que gana por delante de cualquier heurística automática. Si la
///    variable está seteada pero la ruta no existe (typo, instalación a
///    medias), se ignora y se sigue probando — no se usa a ciegas.
/// 2. Junto al propio ejecutable (`current_exe()`), si ahí existen TANTO
///    `models/` COMO `runtime/` (las dos, no alcanza con una: una carpeta a
///    medio instalar no debe aceptarse como válida) — el layout normal de
///    una instalación empaquetada.
/// 3. La raíz del crate (`CARGO_MANIFEST_DIR`), solo como conveniencia para
///    `cargo run`/`cargo test` dentro del checkout — en la práctica nunca se
///    cumple en un binario ya compilado y movido, aunque no está garantizado
///    (si por coincidencia existiera ese path exacto en la máquina destino).
pub fn assets_dir() -> PathBuf {
    resolver_assets_dir(
        std::env::var("OCR_TOOLS_ASSETS_DIR").ok().map(PathBuf::from),
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf)),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")),
    )
}

/// Lógica pura de `assets_dir()`, separada para poder testear el orden de
/// prioridad sin depender del `current_exe()` real del proceso de test (que
/// no tiene `models/`/`runtime/` al lado) ni de variables de entorno
/// compartidas entre tests que corren en paralelo.
fn resolver_assets_dir(
    env_var: Option<PathBuf>,
    dir_ejecutable: Option<PathBuf>,
    fallback: PathBuf,
) -> PathBuf {
    if let Some(dir) = env_var {
        if dir.is_dir() {
            return dir;
        }
    }
    if let Some(dir) = dir_ejecutable {
        if dir.join("models").is_dir() && dir.join("runtime").is_dir() {
            return dir;
        }
    }
    fallback
}

pub mod async_processor;
pub mod batch;
pub mod checkpoint_store;
pub mod craft_utils;
pub mod ctc;
pub mod detectors;
pub mod downloader;
pub mod file_workflow;
pub mod grouping;
pub mod image_embedder;
pub mod imgproc;
pub mod pipeline;
pub mod reader;
pub mod recognize_prep;
pub mod report;
pub mod session;
pub mod text_detector;
pub mod url_compactor;
pub mod url_helper;
pub mod xlsx_loader;

#[cfg(test)]
mod tests {
    use super::*;

    /// Carpeta con `models/` y `runtime/` reales (ambas), simulando el
    /// directorio de un binario ya instalado.
    fn dir_instalado() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("models")).unwrap();
        std::fs::create_dir(tmp.path().join("runtime")).unwrap();
        tmp
    }

    #[test]
    fn env_var_valida_gana_aunque_el_directorio_del_ejecutable_tambien_sea_valido() {
        let env_dir = tempfile::tempdir().unwrap();
        let exe_dir = dir_instalado();
        let resultado = resolver_assets_dir(
            Some(env_dir.path().to_path_buf()),
            Some(exe_dir.path().to_path_buf()),
            PathBuf::from("/fallback"),
        );
        assert_eq!(resultado, env_dir.path());
    }

    #[test]
    fn env_var_invalida_no_bloquea_las_siguientes_estrategias() {
        let exe_dir = dir_instalado();
        let resultado = resolver_assets_dir(
            Some(PathBuf::from("/esta/ruta/no/existe/seguro")),
            Some(exe_dir.path().to_path_buf()),
            PathBuf::from("/fallback"),
        );
        assert_eq!(
            resultado,
            exe_dir.path(),
            "una env var seteada pero inválida debe ignorarse, no aceptarse a ciegas"
        );
    }

    #[test]
    fn directorio_del_ejecutable_a_medio_instalar_no_se_acepta() {
        // Solo `models/`, sin `runtime/`: no alcanza con una de las dos.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("models")).unwrap();
        let resultado = resolver_assets_dir(None, Some(tmp.path().to_path_buf()), PathBuf::from("/fallback"));
        assert_eq!(
            resultado,
            PathBuf::from("/fallback"),
            "una carpeta con solo `models/` (sin `runtime/`) no debe aceptarse como válida"
        );
    }

    #[test]
    fn sin_env_var_ni_directorio_de_ejecutable_valido_usa_el_fallback() {
        let resultado = resolver_assets_dir(None, None, PathBuf::from("/fallback"));
        assert_eq!(resultado, PathBuf::from("/fallback"));
    }

    #[test]
    fn directorio_del_ejecutable_completo_gana_sobre_el_fallback() {
        let exe_dir = dir_instalado();
        let resultado = resolver_assets_dir(
            None,
            Some(exe_dir.path().to_path_buf()),
            PathBuf::from("/fallback"),
        );
        assert_eq!(resultado, exe_dir.path());
    }
}
