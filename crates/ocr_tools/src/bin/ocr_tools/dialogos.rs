//! Los diálogos del binario: lo que se le pregunta al usuario antes de que
//! el motor haga nada.
//!
//! Separados del trabajo real para que cada modo se lea como "pedir datos,
//! después procesar", y para que cambiar una etiqueta no obligue a abrir el
//! archivo donde vive el pipeline.

use std::path::Path;

use app_shell::FlujoResult;
use ocr_tools::pipeline::DetectorToggles;
use ocr_tools::url_helper;
use ocr_tools::xlsx_loader;

use crate::AppResult;

pub(crate) fn configurar_detectores() -> FlujoResult<DetectorToggles> {
    let etiquetas = [
        "D1 · Banner de color",
        "D2 · Fondo no neutro",
        "D4·Texto / D5·Logo / D6·Placeholder (OCR)",
    ];
    let activos = app_shell::menu_multiple_preseleccionado(
        "Detectores a activar (todos marcados por defecto):",
        etiquetas.to_vec(),
        &[0, 1, 2],
    )?;
    let toggles = DetectorToggles {
        d1: activos.contains(&etiquetas[0]),
        d2: activos.contains(&etiquetas[1]),
        d4_d5_d6: activos.contains(&etiquetas[2]),
    };
    app_shell::info(&format!(
        "Detectores activos: {}",
        [
            toggles.d1.then_some("d1"),
            toggles.d2.then_some("d2"),
            toggles.d4_d5_d6.then_some("d4_d5_d6"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ")
    ));
    Ok(toggles)
}

/// Qué imágenes lleva el archivo de RECHAZADAS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImagenesRechazadas {
    TodasLasDeLaFila,
    SoloLasRechazadas,
}

impl std::fmt::Display for ImagenesRechazadas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let etiqueta = match self {
            ImagenesRechazadas::TodasLasDeLaFila => "Todas las imágenes de la fila",
            ImagenesRechazadas::SoloLasRechazadas => {
                "Solo las imágenes rechazadas (compactadas a la izquierda)"
            }
        };
        write!(f, "{etiqueta}")
    }
}

pub(crate) fn elegir_modo_rechazadas() -> FlujoResult<bool> {
    let eleccion = app_shell::menu_seleccionar(
        "En el archivo de RECHAZADAS, ¿qué imágenes incluir?",
        vec![
            ImagenesRechazadas::TodasLasDeLaFila,
            ImagenesRechazadas::SoloLasRechazadas,
        ],
    )?;
    Ok(eleccion == Some(ImagenesRechazadas::SoloLasRechazadas))
}

/// Detecta columnas URL automáticamente; si hay candidatas, confirma con el
/// usuario. Sin candidatas, o si no las quiere, deja elegir manualmente.
/// De dónde salen las columnas de URL.
///
/// Un enum y no un `Option<Vec<String>>` porque son TRES casos, no dos, y el
/// tercero es el que hace falta en un servidor: detectarlas solas SIN
/// preguntar. Con un `Option`, "detectar" y "preguntar" quedaban colapsados
/// y el modo desatendido se trababa esperando una tecla.
pub(crate) enum ModoColumnas<'a> {
    /// Escritorio: detectar y pedir confirmación.
    Preguntar,
    /// Las dijo el usuario por línea de comandos.
    Fijas(&'a [String]),
    /// Desatendido: lo que detecte, sin confirmar.
    DetectarSinPreguntar,
}

pub(crate) fn resolver_columnas_url(
    xlsx_path: &Path,
    columnas: &[String],
    modo: ModoColumnas<'_>,
) -> AppResult<Vec<String>> {
    // Las fijas se validan contra el archivo: un nombre mal escrito en el
    // comando tiene que fallar acá y no producir una corrida de días que no
    // analiza nada.
    if let ModoColumnas::Fijas(pedidas) = modo {
        let faltan: Vec<&String> = pedidas.iter().filter(|c| !columnas.contains(c)).collect();
        if !faltan.is_empty() {
            app_shell::error(&format!(
                "El archivo no tiene estas columnas: {}. Disponibles: {}",
                faltan.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(", "),
                columnas.join(", ")
            ));
            return Ok(Vec::new());
        }
        return Ok(pedidas.to_vec());
    }

    let sample = xlsx_loader::load_sample(xlsx_path, 50);
    let candidatas = match &sample {
        Some(df) => url_helper::auto_detect_columns(df, 10)?,
        None => Vec::new(),
    };
    let publicas: Vec<String> = columnas.iter().filter(|c| !c.starts_with('_')).cloned().collect();

    if !candidatas.is_empty() {
        app_shell::success(&format!("Columnas URL detectadas: {}", candidatas.join(", ")));
        if matches!(modo, ModoColumnas::DetectarSinPreguntar) {
            return Ok(candidatas);
        }
        let usar = app_shell::menu_confirmar(
            &format!(
                "¿Usar las columnas detectadas automáticamente? ({})",
                candidatas.join(", ")
            ),
            true,
        )?
        .unwrap_or(true);
        if usar {
            return Ok(candidatas);
        }
        return Ok(app_shell::menu_multiple(
            "Columnas con URLs de imágenes (Enter sin marcar = cancelar):",
            publicas,
        )?);
    }

    app_shell::warn("No se detectaron columnas URL automáticamente.");
    Ok(app_shell::menu_multiple(
        "Columnas con URLs de imágenes (Enter sin marcar = cancelar):",
        publicas,
    )?)
}
