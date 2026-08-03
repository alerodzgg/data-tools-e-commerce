//! Menús interactivos (flechas + Enter, filtro en vivo) sobre `inquire`.
//!
//! `inquire::MultiSelect` resuelve nativamente tanto el checkbox con
//! preselección como sin ella, además de la navegación con filtro, sin
//! necesidad de acumular selecciones manualmente. Cancelar (ESC) usa
//! `prompt_skippable()`, que da `None` de forma nativa.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use inquire::{Confirm, MultiSelect, Select, Text};

use crate::navegacion::{FlujoError, FlujoResult};

const VOLVER: &str = "\u{ab} Volver al menú principal \u{bb}";

/// Bajo un guion de test, resuelve un menú de selección ÚNICA consumiendo el
/// próximo índice programado. Compartido por `menu_seleccionar` y
/// `menu_seleccionar_nav`.
fn tomar_eleccion_de_guion<T>(mensaje: &str, opciones: Vec<T>) -> Option<T> {
    crate::testing::tomar_eleccion(mensaje).map(|indice| {
        opciones
            .into_iter()
            .nth(indice)
            .unwrap_or_else(|| panic!("guion de test: índice {indice} fuera de rango para '{mensaje}'"))
    })
}

/// Bajo un guion de test, resuelve un menú de selección MÚLTIPLE consumiendo
/// los próximos índices marcados. Compartido por `menu_multiple` y
/// `menu_multiple_preseleccionado`.
fn tomar_marcadas_de_guion<T>(mensaje: &str, opciones: Vec<T>) -> Vec<T> {
    let marcadas = crate::testing::tomar_marcadas(mensaje);
    let mut opciones: Vec<Option<T>> = opciones.into_iter().map(Some).collect();
    marcadas
        .into_iter()
        .map(|indice| {
            opciones
                .get_mut(indice)
                .and_then(Option::take)
                .unwrap_or_else(|| panic!("guion de test: índice {indice} fuera de rango para '{mensaje}'"))
        })
        .collect()
}

/// Envoltorio que muestra el nombre de archivo (no la ruta completa) en los
/// menús, pero conserva la `PathBuf` para devolverla. Cuando el nombre se
/// repite entre las opciones (mismo archivo en carpetas distintas), suma la
/// carpeta padre: sin eso, dos opciones se verían idénticas y solo podrían
/// distinguirse por su posición en la lista.
struct ArchivoOpcion {
    ruta: PathBuf,
    mostrar_carpeta: bool,
    // Mostrar solo el NOMBRE de la carpeta padre no alcanza cuando dos
    // archivos comparten nombre de archivo Y nombre de carpeta padre pero
    // están en rutas completas distintas (p. ej. ".../ProyectoA/lote/x.xlsx"
    // y ".../ProyectoB/lote/x.xlsx"): ambas opciones se verían idénticas en
    // el menú. En ese caso se muestra la ruta del padre completa en vez de
    // solo su nombre.
    ruta_padre_ambigua: bool,
}

impl fmt::Display for ArchivoOpcion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nombre = self.ruta.file_name().unwrap_or_default().to_string_lossy();
        if self.ruta_padre_ambigua {
            let carpeta = self
                .ruta
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            write!(f, "{nombre}  ({carpeta})")
        } else if self.mostrar_carpeta {
            let carpeta = self
                .ruta
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            write!(f, "{nombre}  ({carpeta})")
        } else {
            write!(f, "{nombre}")
        }
    }
}

fn nombre_padre(ruta: &std::path::Path) -> std::ffi::OsString {
    ruta.parent()
        .and_then(|p| p.file_name())
        .unwrap_or_default()
        .to_os_string()
}

/// Arma las opciones de menú marcando `mostrar_carpeta` solo para los
/// archivos cuyo nombre se repite en `archivos`, y `ruta_padre_ambigua`
/// cuando además el nombre de la carpeta padre también se repite entre esos
/// mismos archivos (ver comentario de [`ArchivoOpcion`]).
fn opciones_de_archivos(archivos: Vec<PathBuf>) -> Vec<ArchivoOpcion> {
    let mut conteo: HashMap<std::ffi::OsString, usize> = HashMap::new();
    for ruta in &archivos {
        *conteo
            .entry(ruta.file_name().unwrap_or_default().to_os_string())
            .or_insert(0) += 1;
    }
    let mut conteo_padre: HashMap<(std::ffi::OsString, std::ffi::OsString), usize> = HashMap::new();
    for ruta in &archivos {
        let clave = (
            ruta.file_name().unwrap_or_default().to_os_string(),
            nombre_padre(ruta),
        );
        *conteo_padre.entry(clave).or_insert(0) += 1;
    }
    archivos
        .into_iter()
        .map(|ruta| {
            let nombre = ruta.file_name().unwrap_or_default().to_os_string();
            let repetido = conteo.get(&nombre).copied().unwrap_or(0) > 1;
            let ruta_padre_ambigua = repetido
                && conteo_padre
                    .get(&(nombre, nombre_padre(&ruta)))
                    .copied()
                    .unwrap_or(0)
                    > 1;
            ArchivoOpcion {
                ruta,
                mostrar_carpeta: repetido,
                ruta_padre_ambigua,
            }
        })
        .collect()
}

/// Elige UN archivo de `archivos`, mostrando solo el nombre. `None` si
/// cancela o si `archivos` está vacío (uso: listar antes de llamar y avisar
/// aparte si hace falta un mensaje específico de "no hay archivos").
pub fn elegir_archivo(mensaje: &str, archivos: Vec<PathBuf>) -> FlujoResult<Option<PathBuf>> {
    if archivos.is_empty() {
        return Ok(None);
    }
    let opciones = opciones_de_archivos(archivos);
    Ok(menu_seleccionar_nav(mensaje, opciones)?.map(|a| a.ruta))
}

/// Elige varios archivos de `archivos`, mostrando solo el nombre. Lista
/// vacía si cancela o no marca ninguno.
pub fn elegir_archivos(mensaje: &str, archivos: Vec<PathBuf>) -> FlujoResult<Vec<PathBuf>> {
    let opciones = opciones_de_archivos(archivos);
    Ok(menu_multiple(mensaje, opciones)?
        .into_iter()
        .map(|a| a.ruta)
        .collect())
}

/// Menú de UNA opción. `None` si el usuario cancela (ESC).
///
/// Si hay un guion de test activo ([`crate::testing::con_guion`]), consume la
/// próxima respuesta programada en vez de leer la terminal real.
pub fn menu_seleccionar<T: fmt::Display>(mensaje: &str, opciones: Vec<T>) -> FlujoResult<Option<T>> {
    if crate::testing::activo() {
        return Ok(tomar_eleccion_de_guion(mensaje, opciones));
    }
    Ok(Select::new(mensaje, opciones).prompt_skippable()?)
}

/// Envuelve cada opción real más una centinela para "« Volver »", para que
/// `menu_seleccionar_nav` pueda devolver el valor `T` ORIGINAL (por
/// identidad de posición, vía `inquire`) en vez de tener que re-buscarlo por
/// igualdad de su texto mostrado — eso último es lo que causaba que, con dos
/// opciones que se MUESTRAN igual (p. ej. dos archivos de mismo nombre en
/// carpetas distintas), se devolviera siempre la primera en vez de la
/// realmente elegida.
enum ConVolver<T> {
    Opcion(T),
    Volver,
}

impl<T: fmt::Display> fmt::Display for ConVolver<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConVolver::Opcion(t) => write!(f, "{t}"),
            ConVolver::Volver => write!(f, "{VOLVER}"),
        }
    }
}

/// Como `menu_seleccionar`, pero con una opción extra « Volver al menú
/// principal »: si se elige, propaga `FlujoError::VolverAlMenu` (equivalente
/// Rust de la excepción de navegación del original).
///
/// Bajo un guion de test, los índices programados refieren a `opciones` TAL
/// COMO SE PASÓ (sin contar el « Volver » agregado acá).
pub fn menu_seleccionar_nav<T: fmt::Display>(mensaje: &str, opciones: Vec<T>) -> FlujoResult<Option<T>> {
    if crate::testing::activo() {
        return Ok(tomar_eleccion_de_guion(mensaje, opciones));
    }
    let mut envueltas: Vec<ConVolver<T>> = opciones.into_iter().map(ConVolver::Opcion).collect();
    envueltas.push(ConVolver::Volver);
    match Select::new(mensaje, envueltas).prompt_skippable()? {
        Some(ConVolver::Volver) => Err(FlujoError::VolverAlMenu),
        Some(ConVolver::Opcion(t)) => Ok(Some(t)),
        None => Ok(None),
    }
}

/// Menú de varias opciones (checkbox: ↑/↓ mover, ESPACIO marcar, Enter
/// confirmar). Lista vacía si el usuario no marca nada, o si cancela (ESC).
pub fn menu_multiple<T: fmt::Display>(mensaje: &str, opciones: Vec<T>) -> FlujoResult<Vec<T>> {
    if crate::testing::activo() {
        return Ok(tomar_marcadas_de_guion(mensaje, opciones));
    }
    Ok(MultiSelect::new(mensaje, opciones)
        .prompt_skippable()?
        .unwrap_or_default())
}

/// Como `menu_multiple`, pero con algunas opciones pre-marcadas (por
/// posición). Único caso del original: "Configurar detectores", que arranca
/// con todo activado y se desmarca lo que no se quiere.
pub fn menu_multiple_preseleccionado<T: fmt::Display>(
    mensaje: &str,
    opciones: Vec<T>,
    preseleccionadas: &[usize],
) -> FlujoResult<Vec<T>> {
    if crate::testing::activo() {
        return Ok(tomar_marcadas_de_guion(mensaje, opciones));
    }
    Ok(MultiSelect::new(mensaje, opciones)
        .with_default(preseleccionadas)
        .prompt_skippable()?
        .unwrap_or_default())
}

/// Pregunta Sí/No con flechas. `None` si el usuario cancela (ESC) — un caso
/// distinto de confirmar el valor por defecto con Enter, que el llamador
/// necesita poder diferenciar. Acepta "s"/"si"/"sí" (y "y"/"yes") para sí;
/// "n"/"no" para no — sin distinguir mayúsculas/acentos.
fn confirmar_parser(respuesta: &str) -> Result<bool, ()> {
    match respuesta.trim().to_lowercase().as_str() {
        "s" | "si" | "sí" | "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => Err(()),
    }
}

/// "(S/n)"/"(s/N)", nunca "(Y/n)"/"(y/N)": el formateador por default de
/// `inquire::Confirm` está en inglés, y el resto de la interfaz está en
/// español — mezclar los dos es la inconsistencia real (la mayúscula en sí
/// misma, marcando cuál es la opción por default al apretar Enter, es una
/// convención válida que se conserva).
fn confirmar_formatter_default(por_defecto: bool) -> String {
    if por_defecto {
        "S/n".to_string()
    } else {
        "s/N".to_string()
    }
}

pub fn menu_confirmar(mensaje: &str, por_defecto: bool) -> FlujoResult<Option<bool>> {
    if crate::testing::activo() {
        return Ok(crate::testing::tomar_confirmacion(mensaje));
    }
    Ok(Confirm::new(mensaje)
        .with_default(por_defecto)
        .with_parser(&confirmar_parser)
        .with_default_value_formatter(&confirmar_formatter_default)
        .prompt_skippable()?)
}

/// Entrada de texto libre. `None` si el usuario cancela (ESC) — un caso
/// distinto de confirmar con Enter una cadena vacía, que el llamador
/// necesita poder diferenciar.
pub fn pedir_texto(mensaje: &str) -> FlujoResult<Option<String>> {
    if crate::testing::activo() {
        return Ok(crate::testing::tomar_texto(mensaje).map(|t| t.trim().to_string()));
    }
    Ok(Text::new(mensaje)
        .prompt_skippable()?
        .map(|t| t.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{con_guion, Respuesta};

    #[test]
    fn menu_seleccionar_bajo_guion_devuelve_la_opcion_del_indice_programado() {
        let opciones = vec!["Uno".to_string(), "Dos".to_string(), "Tres".to_string()];
        let elegido = con_guion(vec![Respuesta::Elegir(1)], || {
            menu_seleccionar("¿cuál?", opciones).unwrap()
        });
        assert_eq!(elegido, Some("Dos".to_string()));
    }

    #[test]
    fn menu_seleccionar_bajo_guion_respeta_cancelar() {
        let opciones = vec!["Uno".to_string()];
        let elegido = con_guion(vec![Respuesta::Cancelar], || {
            menu_seleccionar("¿cuál?", opciones).unwrap()
        });
        assert_eq!(elegido, None);
    }

    #[test]
    fn menu_confirmar_distingue_cancelar_de_confirmar_el_valor_por_defecto() {
        // Cancelar y confirmar el valor por defecto deben ser distinguibles:
        // por eso el retorno es `Option<bool>` y no `bool`.
        let confirmado = con_guion(vec![Respuesta::Confirmar(false)], || {
            menu_confirmar("¿ok?", false).unwrap()
        });
        assert_eq!(confirmado, Some(false));

        let cancelado = con_guion(vec![Respuesta::Cancelar], || {
            menu_confirmar("¿ok?", false).unwrap()
        });
        assert_eq!(cancelado, None);
    }

    #[test]
    fn confirmar_parser_acepta_variantes_en_espanol_sin_distinguir_mayusculas_ni_acentos() {
        for si in ["s", "S", "si", "Si", "sí", "SÍ", "  s  "] {
            assert_eq!(
                confirmar_parser(si),
                Ok(true),
                "'{si}' debe interpretarse como sí"
            );
        }
        for no in ["n", "N", "no", "No", "  no  "] {
            assert_eq!(
                confirmar_parser(no),
                Ok(false),
                "'{no}' debe interpretarse como no"
            );
        }
        assert_eq!(confirmar_parser(""), Err(()));
        assert_eq!(confirmar_parser("tal vez"), Err(()));
    }

    #[test]
    fn confirmar_formatter_default_nunca_usa_las_letras_en_ingles() {
        // La inconsistencia real no era la mayúscula (marca la opción por
        // default), sino mostrar "Y/n"/"y/N" en inglés en medio de una
        // interfaz en español.
        assert_eq!(confirmar_formatter_default(true), "S/n");
        assert_eq!(confirmar_formatter_default(false), "s/N");
    }

    #[test]
    fn pedir_texto_distingue_cancelar_de_confirmar_vacio() {
        // Mismo criterio que `menu_confirmar`: cancelar con ESC y confirmar
        // con el campo vacío son dos respuestas distintas, no la misma
        // cadena vacía.
        let vacio = con_guion(vec![Respuesta::Texto(String::new())], || {
            pedir_texto("¿texto?").unwrap()
        });
        assert_eq!(vacio, Some(String::new()));

        let cancelado = con_guion(vec![Respuesta::Cancelar], || pedir_texto("¿texto?").unwrap());
        assert_eq!(cancelado, None);
    }

    #[test]
    fn menu_seleccionar_nav_bajo_guion_no_cuenta_la_opcion_volver_agregada() {
        // El índice programado refiere a `opciones` TAL COMO SE PASÓ, sin el
        // « Volver » que `menu_seleccionar_nav` agrega internamente: esto
        // demuestra que el guion nunca ve esa opción extra.
        let opciones = vec!["A".to_string(), "B".to_string()];
        let elegido = con_guion(vec![Respuesta::Elegir(1)], || {
            menu_seleccionar_nav("¿cuál?", opciones).unwrap()
        });
        assert_eq!(elegido, Some("B".to_string()));
    }

    #[test]
    fn elegir_archivo_devuelve_el_del_indice_elegido_no_el_primero_con_igual_nombre() {
        // La opción elegida se recupera por POSICIÓN, no buscando por el
        // texto mostrado: dos archivos que se muestran igual (mismo nombre en
        // carpetas distintas) devolverían siempre el primero de la lista.
        let archivos = vec![
            PathBuf::from("carpetaA/data.xlsx"),
            PathBuf::from("carpetaB/data.xlsx"),
        ];
        let elegido = con_guion(vec![Respuesta::Elegir(1)], || {
            elegir_archivo("¿cuál?", archivos).unwrap()
        });
        assert_eq!(elegido, Some(PathBuf::from("carpetaB/data.xlsx")));
    }

    #[test]
    fn opciones_de_archivos_solo_muestra_la_carpeta_para_nombres_repetidos() {
        let opciones = opciones_de_archivos(vec![
            PathBuf::from("carpetaA/data.xlsx"),
            PathBuf::from("carpetaB/data.xlsx"),
            PathBuf::from("carpetaA/unico.xlsx"),
        ]);
        assert_eq!(opciones[0].to_string(), "data.xlsx  (carpetaA)");
        assert_eq!(opciones[1].to_string(), "data.xlsx  (carpetaB)");
        assert_eq!(opciones[2].to_string(), "unico.xlsx");
    }

    #[test]
    fn opciones_de_archivos_usa_la_ruta_padre_completa_si_el_nombre_de_la_carpeta_tambien_se_repite() {
        // "lote" se repite como nombre de carpeta padre en ambos: mostrar
        // solo "(lote)" en los dos no los distinguiría en el menú.
        let opciones = opciones_de_archivos(vec![
            PathBuf::from("ProyectoA/lote/data.xlsx"),
            PathBuf::from("ProyectoB/lote/data.xlsx"),
        ]);
        assert_eq!(
            opciones[0].to_string(),
            format!("data.xlsx  ({})", PathBuf::from("ProyectoA/lote").display())
        );
        assert_eq!(
            opciones[1].to_string(),
            format!("data.xlsx  ({})", PathBuf::from("ProyectoB/lote").display())
        );
    }

    #[test]
    fn menu_multiple_bajo_guion_devuelve_las_opciones_marcadas_en_ese_orden() {
        let opciones = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let marcadas = con_guion(vec![Respuesta::Marcar(vec![2, 0])], || {
            menu_multiple("¿cuáles?", opciones).unwrap()
        });
        assert_eq!(marcadas, vec!["C".to_string(), "A".to_string()]);
    }

    #[test]
    fn flujo_encadenado_de_varios_prompts_consume_el_guion_en_orden() {
        // Simula un flujo de CLI real: elegir una opción, confirmar, y
        // escribir texto — sin ninguna terminal real de por medio.
        let resultado = con_guion(
            vec![
                Respuesta::Elegir(0),
                Respuesta::Confirmar(true),
                Respuesta::Texto("hola".to_string()),
            ],
            || -> FlujoResult<(String, bool, String)> {
                let opcion = menu_seleccionar("paso 1", vec!["X".to_string()])?.expect("no debería cancelar");
                let confirmado = menu_confirmar("paso 2", false)?.expect("no debería cancelar");
                let texto = pedir_texto("paso 3")?.expect("no debería cancelar");
                Ok((opcion, confirmado, texto))
            },
        )
        .unwrap();
        assert_eq!(resultado, ("X".to_string(), true, "hola".to_string()));
    }

    #[test]
    fn fuera_de_con_guion_no_interfiere_con_otro_test_que_use_guion() {
        // Verifica el aislamiento por hilo: si este test corriera en el mismo
        // hilo que uno anterior que dejó un guion activo por error, esto
        // fallaría con un panic de "guion no activo" en vez de leer basura.
        assert!(!crate::testing::activo());
    }
}
