//! Binario interactivo de `ocr_tools`: `CLI`/`FileWorkflow::run`/`main()`
//! para la selección de archivos/columnas/detectores por menú, análisis
//! (D1-D6) o inserción de imágenes en el Excel.
//!
//! Vive en `src/bin/` (no en la librería) a propósito: `ocr_tools` (el
//! motor) no depende de `app_shell` (la interfaz) — misma regla que separa
//! `commerce_core` de este binario en el resto del workspace.

// Cada `src/bin/*` es un crate root propio: NO hereda los lints de `lib.rs`,
// asi que la politica se repite aca. Un panic en produccion aborta el proceso
// que ve el usuario; en tests `.unwrap()` es la forma normal de fallar rapido.
#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]

use std::path::PathBuf;

use app_shell::FlujoError;
use ocr_tools::pipeline::DetectorToggles;
use ocr_tools::xlsx_loader;

mod analizar;
mod desatendido;
mod dialogos;
mod insertar;

use analizar::analizar_archivos;
use dialogos::{configurar_detectores, elegir_modo_rechazadas};
use insertar::insertar_imagenes_en_archivos;

/// Errores del binario: la unión de lo que puede fallar en cada capa
/// (interfaz, motor, I/O). No vive en `app_shell` porque `ort`/`polars` son
/// específicos de este tool, no algo que compartan las demás herramientas.
#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Flujo(#[from] FlujoError),
    #[error(transparent)]
    Core(#[from] commerce_core::CoreError),
    #[error(transparent)]
    Polars(#[from] polars::error::PolarsError),
    #[error(transparent)]
    Ort(#[from] ort::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

type AppResult<T> = Result<T, AppError>;

fn modelo(nombre: &str) -> PathBuf {
    ocr_tools::assets_dir().join("models").join(nombre)
}

/// Los modos del menú principal. El menú los ofrece por VALOR y
/// `ciclo_principal` los resuelve con un `match` exhaustivo: agregar una
/// variante no compila hasta contemplarla en el despacho.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modo {
    Analizar,
    Configurar,
    Insertar,
}

impl Modo {
    const TODOS: [Modo; 3] = [Modo::Analizar, Modo::Configurar, Modo::Insertar];
}

impl std::fmt::Display for Modo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let etiqueta = match self {
            Modo::Analizar => "Analizar imágenes (todos los detectores activos)",
            Modo::Configurar => "Configurar detectores y analizar",
            Modo::Insertar => "Insertar imágenes de URL en el Excel (columna nueva a la derecha)",
        };
        write!(f, "{etiqueta}")
    }
}

async fn ciclo_principal() -> AppResult<()> {
    loop {
        let eleccion = match app_shell::menu_seleccionar_nav("¿Qué quieres hacer?", Modo::TODOS.to_vec()) {
            Ok(Some(e)) => e,
            Ok(None) => {
                app_shell::info("Hasta luego.");
                return Ok(());
            }
            Err(FlujoError::VolverAlMenu) => continue,
            Err(e) => return Err(e.into()),
        };

        let files = xlsx_loader::list_xlsx_files(&app_shell::ruta_entrada())?;
        if files.is_empty() {
            app_shell::warn("No se encontraron archivos. Volviendo al menú.");
            continue;
        }

        let seleccionados =
            match app_shell::elegir_archivos("Archivos a analizar (Enter sin marcar = cancelar):", files) {
                Ok(v) => v,
                Err(FlujoError::VolverAlMenu) => continue,
                Err(e) => return Err(e.into()),
            };
        if seleccionados.is_empty() {
            app_shell::warn("No se seleccionaron archivos. Volviendo al menú.");
            continue;
        }

        if eleccion == Modo::Insertar {
            if let Err(e) = insertar_imagenes_en_archivos(&seleccionados).await {
                match e {
                    AppError::Flujo(FlujoError::VolverAlMenu) => continue,
                    e => {
                        app_shell::error(&format!("La herramienta falló: {e}"));
                        app_shell::warn("Vuelvo al menú.");
                    }
                }
            }
            continue;
        }

        let toggles = if eleccion == Modo::Configurar {
            match configurar_detectores() {
                Ok(t) => t,
                Err(FlujoError::VolverAlMenu) => continue,
                Err(e) => return Err(e.into()),
            }
        } else {
            DetectorToggles::default()
        };
        let rechazadas_solo = match elegir_modo_rechazadas() {
            Ok(v) => v,
            Err(FlujoError::VolverAlMenu) => continue,
            Err(e) => return Err(e.into()),
        };

        analizar_archivos(&seleccionados, toggles, rechazadas_solo, None, false).await?;
    }
}

/// Analiza sin preguntar nada, para correr bajo systemd o nohup.
///
/// No imprime la cabecera decorativa ni el diagnóstico del sistema: en un
/// servidor eso solo ensucia el log que alguien va a leer buscando por qué
/// se cortó una corrida de días.
async fn correr_desatendido(opciones: desatendido::Opciones) -> i32 {
    if let Some(salida) = opciones.salida {
        if let Err(e) = std::fs::create_dir_all(&salida) {
            app_shell::error(&format!("No se pudo crear '{}': {e}", salida.display()));
            return 1;
        }
        app_shell::fijar_rutas(None, Some(salida));
    }
    // Validar ANTES de arrancar: una ruta mal escrita salía con código 0, y
    // para systemd eso es "terminó bien". La corrida no procesaba nada, no
    // se reintentaba, y el silencio duraba hasta que alguien mirara la
    // carpeta de salida vacía.
    let faltantes: Vec<&std::path::PathBuf> = opciones.archivos.iter().filter(|r| !r.is_file()).collect();
    if !faltantes.is_empty() {
        for ruta in faltantes {
            app_shell::error(&format!("No existe el archivo: {}", ruta.display()));
        }
        return 2;
    }

    let resultado = analizar_archivos(
        &opciones.archivos,
        DetectorToggles::default(),
        opciones.rechazadas_solo,
        opciones.columnas.as_deref(),
        true,
    )
    .await;
    match resultado {
        Ok(()) => 0,
        Err(e) => {
            // Código distinto de cero para que systemd sepa que hay que
            // reintentar: con `Restart=always` esto es lo que reanuda la
            // corrida tras una interrupción de spot.
            app_shell::error(&format!("Error fatal: {e}"));
            1
        }
    }
}

#[tokio::main]
async fn main() {
    let codigo = match desatendido::parsear(std::env::args().skip(1)) {
        Ok(Some(opciones)) => correr_desatendido(opciones).await,
        Ok(None) => {
            app_shell::mostrar_cabecera("OCR TOOLS — filtrar imágenes por su contenido");
            app_shell::mostrar_diagnostico_sistema();
            match ciclo_principal().await {
                Ok(()) => 0,
                Err(e) => {
                    app_shell::error(&format!("Error fatal: {e}"));
                    1
                }
            }
        }
        Err(desatendido::FalloArgumentos::PidioAyuda) => {
            println!("{}", desatendido::AYUDA);
            0
        }
        Err(desatendido::FalloArgumentos::Invalido(detalle)) => {
            app_shell::error(&detalle);
            println!("{}", desatendido::AYUDA);
            2
        }
    };
    std::process::exit(codigo);
}
