//! Deja `models/` y `runtime/` junto al ejecutable compilado.
//!
//! `ocr_tools` los busca en tiempo de EJECUCIÓN al lado de su propio binario
//! (`assets_dir()`), no en el árbol de fuentes: así el `.exe` se puede mover o
//! empaquetar sin quedar atado al checkout. El precio es que alguien tiene que
//! ponerlos ahí, y hacerlo a mano se pierde en el primer `cargo clean` —
//! dejando un binario que compila bien y falla al cargar el motor OCR.
//!
//! Copiar solo lo que falta o cambió: son ~134 MB y rehacerlos en cada
//! compilación sería más caro que compilar.

use std::path::{Path, PathBuf};
use std::{env, fs, io};

fn main() {
    println!("cargo:rerun-if-changed=models");
    println!("cargo:rerun-if-changed=runtime");

    let Some(destino) = directorio_del_binario() else {
        // Sin un destino identificable no se puede hacer nada útil, pero
        // tampoco tiene sentido romper la compilación: el binario funciona,
        // solo hay que copiar los recursos a mano.
        println!("cargo:warning=No se pudo ubicar el directorio de salida: copiá models/ y runtime/ junto al ejecutable a mano.");
        return;
    };

    let origen = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    for carpeta in ["models", "runtime"] {
        if let Err(error) = copiar_si_hace_falta(&origen.join(carpeta), &destino.join(carpeta)) {
            println!("cargo:warning=No se pudo copiar {carpeta}/ junto al ejecutable ({error}); copiala a mano o `ocr_tools` no va a encontrar el motor OCR.");
        }
    }
}

/// El directorio donde cargo deja los ejecutables (`target/<perfil>/`).
///
/// `OUT_DIR` apunta a `target/<perfil>/build/<crate>-<hash>/out`, así que el
/// directorio del binario está tres niveles más arriba.
fn directorio_del_binario() -> Option<PathBuf> {
    let out = PathBuf::from(env::var("OUT_DIR").ok()?);
    out.ancestors().nth(3).map(Path::to_path_buf)
}

/// Copia el contenido de `origen` en `destino`, salteando los archivos que ya
/// están y tienen el mismo tamaño.
fn copiar_si_hace_falta(origen: &Path, destino: &Path) -> io::Result<()> {
    if !origen.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(destino)?;
    for entrada in fs::read_dir(origen)? {
        let entrada = entrada?;
        let desde = entrada.path();
        if !desde.is_file() {
            continue;
        }
        let hacia = destino.join(entrada.file_name());
        let mismo_tamano = fs::metadata(&hacia)
            .ok()
            .zip(entrada.metadata().ok())
            .is_some_and(|(a, b)| a.len() == b.len());
        if !mismo_tamano {
            fs::copy(&desde, &hacia)?;
        }
    }
    Ok(())
}
