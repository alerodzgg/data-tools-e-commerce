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

    let opciones = vec![
        "Carpeta de ENTRADA (de dónde se leen los archivos)".to_string(),
        "Carpeta de SALIDA (dónde se guardan los resultados)".to_string(),
        "Las dos".to_string(),
    ];
    let Some(cual) = app_shell::menu_seleccionar_nav("¿Qué quieres cambiar?", opciones.clone())? else {
        return Ok(());
    };

    let mut nueva_entrada = None;
    let mut nueva_salida = None;
    if cual == opciones[0] || cual == opciones[2] {
        let texto = app_shell::pedir_texto("Nueva carpeta de ENTRADA (vacío = dejarla como está):")?
            .unwrap_or_default();
        if !texto.is_empty() {
            nueva_entrada = Some(PathBuf::from(texto));
        }
    }
    if cual == opciones[1] || cual == opciones[2] {
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
/// como la cancelación normal (ESC). Antes, un `Err` real se trataba igual
/// que ESC (salía en silencio, sin avisar), a diferencia de los otros 5
/// binarios del workspace, que sí propagan y reportan ese error.
fn ejecutar() -> app_shell::FlujoResult<()> {
    app_shell::mostrar_cabecera("DATA TOOLS E-COMMERCE");

    loop {
        let mut opciones: Vec<String> = HERRAMIENTAS.iter().map(|h| h.descripcion.to_string()).collect();
        let idx_rutas = opciones.len();
        opciones.push(format!(
            "⚙  Cambiar rutas  (entrada: {}, salida: {})",
            app_shell::ruta_entrada().display(),
            app_shell::ruta_salida().display()
        ));
        let idx_salir = opciones.len();
        opciones.push("Salir".to_string());

        let Some(eleccion) = app_shell::menu_seleccionar("¿Qué quieres hacer?", opciones.clone())? else {
            app_shell::info("Hasta luego.");
            return Ok(());
        };

        if eleccion == opciones[idx_salir] {
            app_shell::info("Hasta luego.");
            return Ok(());
        }
        if eleccion == opciones[idx_rutas] {
            if let Err(error) = configurar_rutas() {
                if !matches!(error, app_shell::FlujoError::VolverAlMenu) {
                    app_shell::warn(&format!("No se pudo configurar las rutas: {error}"));
                }
            }
            continue;
        }
        if let Some(herramienta) = HERRAMIENTAS.iter().find(|h| h.descripcion == eleccion) {
            ejecutar_herramienta(herramienta);
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
        // `ejecutar()` selecciona la herramienta buscando por IGUALDAD de
        // `descripcion` contra la opción elegida en el menú (`HERRAMIENTAS
        // .iter().find(|h| h.descripcion == eleccion)`): si dos entradas
        // compartieran la misma descripción, `find` siempre devolvería la
        // PRIMERA sin importar cuál eligió el usuario, igual que el bug ya
        // corregido en `elegir_archivo` (ver `app_shell::menus`). `binario`
        // también debe ser único: de lo contrario dos entradas lanzarían el
        // mismo ejecutable.
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
