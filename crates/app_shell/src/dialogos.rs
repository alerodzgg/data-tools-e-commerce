//! Diálogos compartidos entre herramientas.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use commerce_core::{abrir_libro, nombres_hojas_libro};

use crate::mensajes::{info, warn};
use crate::menus::{menu_multiple, menu_seleccionar};
use crate::navegacion::FlujoResult;

/// Pregunta (Sí/No) si excluir hojas; si sí, deja elegirlas entre las hojas
/// REALES de los XLSX (`archivos`; los CSV se ignoran, no tienen hojas).
///
/// Devuelve el set de nombres a saltar (minúsculas/recortados; vacío =
/// ninguna), o `None` si se excluyó todo y no queda nada que leer. Con un
/// solo XLSX de ≤1 hoja, o solo CSVs, no pregunta nada.
pub fn preguntar_hojas_excluir(archivos: &[PathBuf], etiqueta: &str) -> FlujoResult<Option<HashSet<String>>> {
    let mut hojas: Vec<String> = Vec::new();
    let mut vistas: HashSet<String> = HashSet::new();
    let mut total_hojas = 0usize;

    for archivo in archivos {
        if !es_xlsx(archivo) {
            continue;
        }
        let Ok(libro) = abrir_libro(archivo) else {
            continue;
        };
        let nombres = nombres_hojas_libro(&libro);
        total_hojas += nombres.len();
        for hoja in nombres {
            let clave = hoja.trim().to_lowercase();
            if vistas.insert(clave) {
                hojas.push(hoja);
            }
        }
    }

    if total_hojas <= 1 || hojas.is_empty() {
        return Ok(Some(HashSet::new()));
    }

    let sufijo = if etiqueta.is_empty() {
        String::new()
    } else {
        format!(" de {etiqueta}")
    };
    let nombre_archivo = if archivos.len() == 1 {
        format!(
            " ('{}')",
            archivos[0].file_name().unwrap_or_default().to_string_lossy()
        )
    } else {
        String::new()
    };

    const SI: &str = "Sí, elegir hoja(s) a excluir";
    let opciones = vec!["No, usar todas las hojas".to_string(), SI.to_string()];
    let pregunta = format!("¿Hay alguna hoja que excluir{sufijo}{nombre_archivo}?");
    let eleccion = menu_seleccionar(&pregunta, opciones)?;
    if eleccion.as_deref() != Some(SI) {
        return Ok(Some(HashSet::new()));
    }

    let marcadas = menu_multiple(&format!("Hojas a excluir{sufijo}:"), hojas)?;
    let excluir: HashSet<String> = marcadas.iter().map(|h| h.trim().to_lowercase()).collect();
    let hay_csv = archivos.iter().any(|a| !es_xlsx(a));

    if !excluir.is_empty() && excluir.len() >= vistas.len() && !hay_csv {
        warn(&format!(
            "Excluiste todas las hojas{sufijo}. Operación cancelada."
        ));
        return Ok(None);
    }
    if !excluir.is_empty() {
        info(&format!("Hojas excluidas{sufijo}: {}", marcadas.join(", ")));
    }
    Ok(Some(excluir))
}

fn es_xlsx(archivo: &Path) -> bool {
    archivo
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("xlsx"))
        .unwrap_or(false)
}

/// `True` (y avisa) si `columnas` trae nombres reservados por la
/// herramienta: el flujo debe cancelarse en vez de corromperlos en silencio.
pub fn abortar_si_reservadas(columnas: &[String], reservadas: &[&str]) -> bool {
    let chocan = commerce_core::columnas_reservadas_en(
        columnas.iter().map(String::as_str),
        reservadas.iter().copied(),
    );
    avisar_si_hay_reservadas(&chocan)
}

/// Si `chocan` no está vacío, avisa cuáles son (mismo mensaje que
/// [`abortar_si_reservadas`]) y devuelve `true`. Extraído para que
/// llamadores con un chequeo de reservadas más rico que el exacto de acá
/// (p. ej. `etl_tools`, que además matchea por prefijo para las columnas de
/// color de fuente) puedan reusar el mensaje sin duplicarlo a mano.
pub fn avisar_si_hay_reservadas(chocan: &[String]) -> bool {
    if !chocan.is_empty() {
        crate::mensajes::error(&format!(
            "El archivo tiene columnas con nombres reservados por la herramienta: {}. Renómbralas y vuelve a intentarlo.",
            chocan.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(", ")
        ));
    }
    !chocan.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{con_guion, Respuesta};

    /// Escribe un XLSX real con una hoja vacía por cada nombre de `hojas`
    /// (contenido no importa: `preguntar_hojas_excluir` solo mira los
    /// nombres).
    fn xlsx_con_hojas(ruta: &Path, hojas: &[&str]) {
        use rust_xlsxwriter::Workbook;
        let mut wb = Workbook::new();
        for nombre in hojas {
            let hoja = wb.add_worksheet().set_name(*nombre).unwrap();
            hoja.write(0, 0, "x").unwrap();
        }
        wb.save(ruta).unwrap();
    }

    #[test]
    fn con_una_sola_hoja_no_pregunta_nada() {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("uno.xlsx");
        xlsx_con_hojas(&archivo, &["Hoja1"]);

        // Sin guion: si esto llamara a un menú real, colgaría/fallaría sin
        // terminal — el hecho de que la función devuelva sin necesitar
        // ninguna respuesta programada demuestra que no pregunta nada.
        let resultado = preguntar_hojas_excluir(&[archivo], "").unwrap();
        assert_eq!(resultado, Some(HashSet::new()));
    }

    #[test]
    fn el_usuario_puede_decir_que_no_hay_nada_que_excluir() {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("dos.xlsx");
        xlsx_con_hojas(&archivo, &["Hoja1", "Hoja2"]);

        let resultado = con_guion(vec![Respuesta::Elegir(0)], || {
            preguntar_hojas_excluir(&[archivo], "").unwrap()
        });
        assert_eq!(resultado, Some(HashSet::new()));
    }

    #[test]
    fn el_usuario_puede_excluir_una_hoja_puntual() {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("dos.xlsx");
        xlsx_con_hojas(&archivo, &["Hoja1", "Hoja2"]);

        let resultado = con_guion(vec![Respuesta::Elegir(1), Respuesta::Marcar(vec![0])], || {
            preguntar_hojas_excluir(&[archivo], "").unwrap()
        });
        assert_eq!(resultado, Some(HashSet::from(["hoja1".to_string()])));
    }

    #[test]
    fn excluir_todas_las_hojas_sin_csv_de_por_medio_cancela_el_flujo() {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("dos.xlsx");
        xlsx_con_hojas(&archivo, &["Hoja1", "Hoja2"]);

        let resultado = con_guion(vec![Respuesta::Elegir(1), Respuesta::Marcar(vec![0, 1])], || {
            preguntar_hojas_excluir(&[archivo], "").unwrap()
        });
        assert_eq!(
            resultado, None,
            "excluir todas las hojas sin ningún CSV en la mezcla debe cancelar (None)"
        );
    }

    #[test]
    fn abortar_si_reservadas_avisa_solo_cuando_hay_choque() {
        assert!(!abortar_si_reservadas(
            &["Titulo".to_string(), "Precio".to_string()],
            &["Motivo_Eliminacion"]
        ));
        assert!(abortar_si_reservadas(
            &["Titulo".to_string(), "Motivo_Eliminacion".to_string()],
            &["Motivo_Eliminacion"]
        ));
    }
}
