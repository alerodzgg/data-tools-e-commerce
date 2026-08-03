//! `hub` — lanzador de las 5 herramientas.
//!
//! Cada herramienta es su propio binario compilado, con su propio runtime
//! (`ocr_tools` usa `tokio`, por ejemplo) — mezclarlos todos en un solo
//! binario forzaría un único runtime async compartido entre las 5. En
//! cambio, el hub las lanza como PROCESO HIJO, heredando la consola (así
//! los menús interactivos de cada herramienta funcionan igual que si se
//! ejecutaran sueltas).
//!
//! "Cambiar rutas" cambia la entrada/salida solo para esta sesión del hub,
//! y se propaga al hijo por variables de entorno
//! (`PUBLICACIONES_ENTRADA`/`PUBLICACIONES_SALIDA`) para que el hijo "vea"
//! el cambio.

// Cada `src/bin/*` es un crate root propio: NO hereda los lints de `lib.rs`,
// asi que la politica se repite aca. Un panic en produccion aborta el proceso
// que ve el usuario; en tests `.unwrap()` es la forma normal de fallar rapido.
#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]

use std::path::{Path, PathBuf};
use std::process::Command;

struct Herramienta {
    binario: &'static str,
    descripcion: &'static str,
}

const HERRAMIENTAS: &[Herramienta] = &[
    Herramienta {
        binario: "publications_builder",
        descripcion: "Publications builder — construir publicaciones (eBay / Amazon)",
    },
    Herramienta {
        binario: "publications_validator",
        descripcion: "Publications validator — validar y limpiar títulos",
    },
    Herramienta {
        binario: "ocr_tools",
        descripcion: "OCR tools — filtrar imágenes por su contenido",
    },
    Herramienta {
        binario: "etl_tools",
        descripcion: "ETL tools — BUSCARV, duplicados, colores, Encontrar",
    },
    Herramienta {
        binario: "data_combinator",
        descripcion: "DATA combinator — combinar varios archivos en uno",
    },
];

/// Ruta del binario hermano `nombre`, en el mismo directorio que este propio
/// ejecutable (cargo pone TODOS los binarios del workspace en el mismo
/// `target/<perfil>/`, sin importar de qué crate vienen).
///
/// `None` si no se pudo resolver la ruta del propio ejecutable (en vez de
/// entrar en pánico): el llamador degrada mostrando un error y sigue en el
/// menú, en lugar de tirar abajo todo el hub.
fn ruta_binario(nombre: &str) -> Option<PathBuf> {
    let mut ruta = std::env::current_exe().ok()?;
    ruta.pop();
    ruta.push(nombre);
    if cfg!(windows) {
        ruta.set_extension("exe");
    }
    Some(ruta)
}

fn ejecutar_herramienta(herramienta: &Herramienta) {
    let Some(ruta) = ruta_binario(herramienta.binario) else {
        app_shell::error("No se pudo resolver la ruta del propio ejecutable del hub.");
        return;
    };
    if !ruta.exists() {
        app_shell::error(&format!(
            "No se encuentra '{}' (¿compilaste el workspace completo con 'cargo build --workspace'?).",
            ruta.display()
        ));
        return;
    }
    let estado = Command::new(&ruta)
        .env("PUBLICACIONES_ENTRADA", app_shell::ruta_entrada())
        .env("PUBLICACIONES_SALIDA", app_shell::ruta_salida())
        .status();
    match estado {
        Ok(estado) if estado.success() => {}
        Ok(estado) => app_shell::warn(&format!(
            "'{}' terminó con un error ({estado}).",
            herramienta.descripcion
        )),
        Err(error) => app_shell::error(&format!(
            "No se pudo ejecutar '{}': {error}",
            herramienta.descripcion
        )),
    }
}

/// Muestra el resultado de fijar una ruta: éxito si la carpeta quedó creada,
/// aviso si `asegurar_carpetas` no pudo crearla (permisos, disco, etc.) — así
/// el mensaje no miente cuando la carpeta en realidad no existe.
fn reportar_ruta(etiqueta: &str, ruta: &Path) {
    if ruta.is_dir() {
        app_shell::success(&format!("{etiqueta}: {}", ruta.display()));
    } else {
        app_shell::warn(&format!(
            "{etiqueta}: {} (no se pudo crear la carpeta; revisá permisos)",
            ruta.display()
        ));
    }
}

fn configurar_rutas() -> app_shell::FlujoResult<()> {
    app_shell::mostrar_subcabecera("Configurar rutas");
    app_shell::info(&format!(
        "Entrada actual: {}",
        app_shell::ruta_entrada().display()
    ));
    app_shell::info(&format!("Salida actual:  {}", app_shell::ruta_salida().display()));
    app_shell::warn("Los cambios valen solo para esta sesión.");

    /// Qué carpeta(s) se van a cambiar.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum CualRuta {
        Entrada,
        Salida,
        Ambas,
    }
    impl std::fmt::Display for CualRuta {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                CualRuta::Entrada => write!(f, "Carpeta de ENTRADA (de dónde se leen los archivos)"),
                CualRuta::Salida => write!(f, "Carpeta de SALIDA (dónde se guardan los resultados)"),
                CualRuta::Ambas => write!(f, "Las dos"),
            }
        }
    }
    let Some(cual) = app_shell::menu_seleccionar_nav(
        "¿Qué quieres cambiar?",
        vec![CualRuta::Entrada, CualRuta::Salida, CualRuta::Ambas],
    )?
    else {
        return Ok(());
    };

    let mut nueva_entrada = None;
    let mut nueva_salida = None;
    if matches!(cual, CualRuta::Entrada | CualRuta::Ambas) {
        let texto = app_shell::pedir_texto("Nueva carpeta de ENTRADA (vacío = dejarla como está):")?
            .unwrap_or_default();
        if !texto.is_empty() {
            nueva_entrada = Some(PathBuf::from(texto));
        }
    }
    if matches!(cual, CualRuta::Salida | CualRuta::Ambas) {
        let texto = app_shell::pedir_texto("Nueva carpeta de SALIDA (vacío = dejarla como está):")?
            .unwrap_or_default();
        if !texto.is_empty() {
            nueva_salida = Some(PathBuf::from(texto));
        }
    }

    if nueva_entrada.is_none() && nueva_salida.is_none() {
        app_shell::info("Sin cambios.");
        return Ok(());
    }
    app_shell::fijar_rutas(nueva_entrada, nueva_salida);
    reportar_ruta("Entrada", &app_shell::ruta_entrada());
    reportar_ruta("Salida ", &app_shell::ruta_salida());
    Ok(())
}

/// Bucle principal. Devuelve `Err` solo ante un error REAL de menú/input
/// (`inquire`, etc.) — `main()` lo reporta; un `Ok(())` cubre tanto "Salir"
/// como la cancelación normal (ESC). Un `Err` real NO se trata como ESC
/// (salir en silencio, sin avisar): los otros 5 binarios del workspace
/// propagan y reportan ese error, y el hub no puede ser la excepción.
fn ejecutar() -> app_shell::FlujoResult<()> {
    app_shell::mostrar_cabecera("DATA TOOLS E-COMMERCE");

    /// Entrada del menú principal. Las herramientas se arman desde
    /// [`HERRAMIENTAS`] y llevan la referencia consigo, de modo que elegir
    /// una devuelve la herramienta misma en vez de un texto que haya que
    /// volver a buscar en la lista.
    enum Entrada {
        Lanzar(&'static Herramienta),
        CambiarRutas(String),
        Salir,
    }
    impl std::fmt::Display for Entrada {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Entrada::Lanzar(h) => write!(f, "{}", h.descripcion),
                Entrada::CambiarRutas(etiqueta) => write!(f, "{etiqueta}"),
                Entrada::Salir => write!(f, "Salir"),
            }
        }
    }

    loop {
        let mut opciones: Vec<Entrada> = HERRAMIENTAS.iter().map(Entrada::Lanzar).collect();
        opciones.push(Entrada::CambiarRutas(format!(
            "⚙  Cambiar rutas  (entrada: {}, salida: {})",
            app_shell::ruta_entrada().display(),
            app_shell::ruta_salida().display()
        )));
        opciones.push(Entrada::Salir);

        let Some(eleccion) = app_shell::menu_seleccionar("¿Qué quieres hacer?", opciones)? else {
            app_shell::info("Hasta luego.");
            return Ok(());
        };

        match eleccion {
            Entrada::Salir => {
                app_shell::info("Hasta luego.");
                return Ok(());
            }
            Entrada::CambiarRutas(_) => {
                if let Err(error) = configurar_rutas() {
                    if !matches!(error, app_shell::FlujoError::VolverAlMenu) {
                        app_shell::warn(&format!("No se pudo configurar las rutas: {error}"));
                    }
                }
            }
            Entrada::Lanzar(herramienta) => ejecutar_herramienta(herramienta),
        }
    }
}

fn main() {
    if let Err(error) = ejecutar() {
        app_shell::error(&format!("Error fatal: {error}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_shell::testing::{con_guion, Respuesta};

    /// Índice del "Salir" en las opciones del menú principal (herramientas,
    /// luego "Cambiar rutas", luego "Salir" — mismo cálculo que `ejecutar()`,
    /// para que este test no se desincronice si el número de herramientas
    /// cambia).
    fn idx_salir() -> usize {
        HERRAMIENTAS.len() + 1
    }

    #[test]
    fn elegir_salir_en_el_menu_principal_termina_sin_error() {
        // `con_guion`/`app_shell::testing` está documentado como pensado
        // sobre todo para testear los binarios de cada herramienta, pero
        // hasta ahora ningún `bin/*.rs` lo usaba — ni siquiera `hub`, cuya
        // lógica entera ES ese bucle de menús encadenados.
        let resultado = con_guion(vec![Respuesta::Elegir(idx_salir())], ejecutar);
        assert!(
            resultado.is_ok(),
            "elegir 'Salir' debe terminar en Ok(()), no en error"
        );
    }

    #[test]
    fn cancelar_el_menu_principal_termina_sin_error_igual_que_salir() {
        let resultado = con_guion(vec![Respuesta::Cancelar], ejecutar);
        assert!(
            resultado.is_ok(),
            "cancelar (ESC) debe terminar en Ok(()), igual que 'Salir'"
        );
    }

    #[test]
    fn ruta_binario_construye_un_sibling_del_ejecutable_actual() {
        let actual = std::env::current_exe().unwrap();
        let ruta = ruta_binario("mi_herramienta").unwrap();
        assert_eq!(ruta.parent(), actual.parent());
        let esperado = if cfg!(windows) {
            "mi_herramienta.exe"
        } else {
            "mi_herramienta"
        };
        assert_eq!(ruta.file_name().unwrap().to_str().unwrap(), esperado);
    }

    #[test]
    fn reportar_ruta_no_entra_en_panico_exista_o_no_la_carpeta() {
        let tmp = tempfile::tempdir().unwrap();
        // Solo verifica que no entra en pánico en ninguna de las dos ramas
        // (carpeta existente vs. inexistente); el mensaje exacto lo deciden
        // `app_shell::success`/`warn`, ya cubiertos por sus propios tests.
        reportar_ruta("Prueba", tmp.path());
        reportar_ruta("Prueba", &tmp.path().join("no_existe"));
    }

    #[test]
    fn herramientas_tiene_binario_y_descripcion_unicos() {
        // `binario` único: dos entradas con el mismo lanzarían el mismo
        // ejecutable. `descripcion` única: son las etiquetas del menú, y dos
        // idénticas serían indistinguibles para quien elige.
        let mut binarios: Vec<&str> = HERRAMIENTAS.iter().map(|h| h.binario).collect();
        let mut descripciones: Vec<&str> = HERRAMIENTAS.iter().map(|h| h.descripcion).collect();
        binarios.sort_unstable();
        descripciones.sort_unstable();
        let binarios_unicos: std::collections::HashSet<_> = binarios.iter().collect();
        let descripciones_unicas: std::collections::HashSet<_> = descripciones.iter().collect();
        assert_eq!(
            binarios_unicos.len(),
            binarios.len(),
            "hay binarios repetidos en HERRAMIENTAS: {binarios:?}"
        );
        assert_eq!(
            descripciones_unicas.len(),
            descripciones.len(),
            "hay descripciones repetidas en HERRAMIENTAS: {descripciones:?}"
        );
    }
}
