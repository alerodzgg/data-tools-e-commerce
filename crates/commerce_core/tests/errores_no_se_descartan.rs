//! Centinela contra el defecto más caro de este workspace: tirar la causa
//! real de un fallo y reportar una genérica.
//!
//! Apareció tres veces en producción. La peor costó dos días de diagnóstico:
//! `xlsx::reader::read(...).map_err(|_| ArchivoCorrupto)` convertía un
//! `Zip(FileNotFound)` —que señalaba con el dedo el `styles.xml` que
//! faltaba— en "archivo corrupto", y mandaba a revisar el archivo del
//! usuario en vez del escritor propio.
//!
//! No prohíbe descartar: a veces el error de origen no agrega nada (una URL
//! que no parsea ya está descrita por `UrlInvalida`). Prohíbe descartar EN
//! SILENCIO. Quien lo haga tiene que escribir por qué, y esa línea la lee el
//! próximo que investigue un fallo.

use std::fs;
use std::path::{Path, PathBuf};

/// Marca que autoriza un descarte concreto. Va en la misma línea o en alguna
/// de las anteriores del bloque de comentario.
const JUSTIFICACION: &str = "causa-descartada:";

/// Patrones que destruyen la causa de un error.
const DESCARTES: [&str; 2] = ["map_err(|_|", "map_err(|_e|"];

fn raiz_workspace() -> PathBuf {
    // CARGO_MANIFEST_DIR = crates/commerce_core
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("el manifiesto siempre está dos niveles bajo la raíz")
        .to_path_buf()
}

fn fuentes(dir: &Path, encontradas: &mut Vec<PathBuf>) {
    let Ok(entradas) = fs::read_dir(dir) else {
        return;
    };
    for entrada in entradas.flatten() {
        let ruta = entrada.path();
        if ruta.is_dir() {
            fuentes(&ruta, encontradas);
        } else if ruta.extension().is_some_and(|e| e == "rs") {
            // Este archivo NOMBRA los patrones que persigue, en su comentario
            // y en `DESCARTES`. Sin excluirlo se acusa a sí mismo, y el
            // centinela quedaría rojo para siempre por su propia definición.
            let es_el_centinela = ruta
                .file_name()
                .is_some_and(|n| n == "errores_no_se_descartan.rs");
            if !es_el_centinela {
                encontradas.push(ruta);
            }
        }
    }
}

#[test]
fn todo_descarte_de_causa_esta_justificado_por_escrito() {
    let raiz = raiz_workspace();
    let mut archivos = Vec::new();
    fuentes(&raiz.join("crates"), &mut archivos);
    assert!(!archivos.is_empty(), "no se encontró ninguna fuente que revisar");

    let mut sin_justificar: Vec<String> = Vec::new();
    for archivo in &archivos {
        let Ok(texto) = fs::read_to_string(archivo) else {
            continue;
        };
        let lineas: Vec<&str> = texto.lines().collect();
        for (i, linea) in lineas.iter().enumerate() {
            if !DESCARTES.iter().any(|p| linea.contains(p)) {
                continue;
            }
            // La justificación puede estar en la línea o en el comentario que
            // la precede: se mira hacia atrás mientras siga siendo comentario.
            let mut justificado = linea.contains(JUSTIFICACION);
            let mut j = i;
            while !justificado && j > 0 {
                let previa = lineas[j - 1].trim();
                if !previa.starts_with("//") {
                    break;
                }
                justificado = previa.contains(JUSTIFICACION);
                j -= 1;
            }
            if !justificado {
                let relativa = archivo.strip_prefix(&raiz).unwrap_or(archivo);
                sin_justificar.push(format!("{}:{} → {}", relativa.display(), i + 1, linea.trim()));
            }
        }
    }

    assert!(
        sin_justificar.is_empty(),
        "Se descarta la causa de un error sin explicar por qué.\n\
         Si el error de origen no agrega nada, escribilo con `{JUSTIFICACION} <motivo>`\n\
         en la línea o en el comentario de arriba. Si sí agrega, conservalo.\n\n{}",
        sin_justificar.join("\n")
    );
}
