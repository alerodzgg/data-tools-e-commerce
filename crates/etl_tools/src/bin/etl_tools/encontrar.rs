//! Modo «Encontrar»: marca las filas que contienen alguna de las palabras
//! de una lista (escrita a mano o traída de un XLSX).

use std::path::Path;

use commerce_core::{columnas_union, total_filas};
use etl_tools::constantes::COL_ENCONTRADA;

use crate::comunes::{
    abortar_si_reservadas, excluir_refs, preguntar_hojas_excluir_de, seleccionar_archivo, Etiquetada,
};
use crate::cruce::{cerrar_cruce, cruzar_y_escribir, preguntar_accion, AccionCruce, Entrada, Salida};
use etl_tools::Busqueda;

use crate::AppResult;

// ════════════════════════════════════════════════════════════════════════
// Modo — Encontrar
// ════════════════════════════════════════════════════════════════════════

fn pedir_palabras_encontrar() -> AppResult<Option<Vec<String>>> {
    /// De dónde salen las palabras a buscar.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum FuentePalabras {
        Escritas,
        DesdeXlsx,
    }
    impl std::fmt::Display for FuentePalabras {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                FuentePalabras::Escritas => write!(f, "Escribirlas aquí (separadas por coma)"),
                FuentePalabras::DesdeXlsx => {
                    write!(f, "Un XLSX con las palabras en su PRIMERA columna")
                }
            }
        }
    }
    let Some(fuente) = app_shell::menu_seleccionar_nav(
        "¿De dónde vienen las palabras a buscar?",
        vec![FuentePalabras::Escritas, FuentePalabras::DesdeXlsx],
    )?
    else {
        return Ok(None);
    };

    if fuente == FuentePalabras::Escritas {
        let texto =
            app_shell::pedir_texto("Palabras a buscar (separadas por coma, ej: 'honda,depo,faro led'):")?
                .unwrap_or_default();
        let palabras: Vec<String> = texto
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
        return Ok(Some(palabras));
    }

    let Some(archivo) = seleccionar_archivo("Selecciona el XLSX con las palabras (1ª columna)")? else {
        return Ok(None);
    };
    let Some(excluir) = preguntar_hojas_excluir_de(&archivo, "las palabras")? else {
        return Ok(None);
    };
    let primera_es_palabra = app_shell::menu_seleccionar(
        &format!(
            "En '{}', ¿la PRIMERA fila es una cabecera o ya es una palabra?",
            archivo.file_name().unwrap_or_default().to_string_lossy()
        ),
        vec![
            Etiquetada::nueva(false, "Es una cabecera/título (ignorarla)"),
            Etiquetada::nueva(true, "Ya es una palabra (incluirla en la búsqueda)"),
        ],
    )?
    .is_some_and(|o| o.valor);

    Ok(Some(etl_tools::palabras_de_xlsx(
        &archivo,
        Some(&excluir_refs(&excluir)),
        primera_es_palabra,
        app_shell::warn,
    )?))
}

pub(crate) fn encontrar_modo(ruta_salida: &Path) -> AppResult<()> {
    app_shell::mostrar_subcabecera("ENCONTRAR — marcar filas que contienen palabras");
    let Some(palabras) = pedir_palabras_encontrar()? else {
        return Ok(());
    };
    if palabras.is_empty() {
        app_shell::warn("No hay palabras usables. Operación cancelada.");
        return Ok(());
    }
    let lookup = etl_tools::lookup_palabras(&palabras)?;
    if lookup.height() == 0 {
        app_shell::warn("Ninguna palabra quedó usable tras normalizar (¿solo símbolos?).");
        return Ok(());
    }

    let Some(base) = seleccionar_archivo("Selecciona el archivo donde BUSCAR")? else {
        return Ok(());
    };
    let Some(excluir_base) = preguntar_hojas_excluir_de(&base, "la Base")? else {
        return Ok(());
    };
    let refs_base = excluir_refs(&excluir_base);
    let cols_base = columnas_union(std::slice::from_ref(&base), Some(&refs_base), app_shell::warn);
    if cols_base.is_empty() {
        app_shell::error(&format!(
            "No se pudieron leer columnas de '{}'.",
            base.file_name().unwrap_or_default().to_string_lossy()
        ));
        return Ok(());
    }
    if abortar_si_reservadas(&cols_base) {
        return Ok(());
    }
    if cols_base.contains(&COL_ENCONTRADA.to_string()) {
        app_shell::error(&format!(
            "El archivo ya tiene una columna '{COL_ENCONTRADA}'; renómbrala y reintenta."
        ));
        return Ok(());
    }

    let Some(clave_base) =
        app_shell::menu_seleccionar_nav("Columna donde buscar las palabras:", cols_base.clone())?
    else {
        return Ok(());
    };
    let Some(opcion) = app_shell::menu_seleccionar_nav(
        "Si VARIAS palabras coinciden en la misma celda:",
        vec![
            Etiquetada::nueva(
                etl_tools::OpcionMultiple::Primera,
                "Solo la primera (el orden de tu lista marca la prioridad)",
            ),
            Etiquetada::nueva(
                etl_tools::OpcionMultiple::Todas,
                "Todas, unidas con salto de línea",
            ),
        ],
    )?
    .map(|o| o.valor) else {
        return Ok(());
    };

    let stem = base.file_stem().unwrap_or_default().to_string_lossy();
    let accion = preguntar_accion(
        &base,
        Some(&format!(
            "Añadir la columna al archivo base ('{}') (sobrescribir)",
            base.file_name().unwrap_or_default().to_string_lossy()
        )),
    )?;

    app_shell::info(&format!(
        "Lista: {} palabras distintas. Construyendo el buscador (Aho-Corasick)...",
        lookup.height()
    ));
    let (ac, claves_originales) = etl_tools::construir_automata(&lookup)?;

    let columnas_salida: Vec<String> = std::iter::once(COL_ENCONTRADA.to_string())
        .chain(cols_base.iter().cloned())
        .collect();
    let solo_match = accion == AccionCruce::SoloReporte;
    let ruta = match accion {
        AccionCruce::SobrescribirBase => base.with_file_name(format!("{stem}__tmp_encontrar.xlsx")),
        AccionCruce::ArchivoNuevo => {
            commerce_core::ruta_unica(ruta_salida.join(format!("encontrar_{stem}.xlsx")))
        }
        AccionCruce::SoloReporte => {
            commerce_core::ruta_unica(ruta_salida.join(format!("encontradas_{stem}.xlsx")))
        }
    };

    let total = total_filas(std::slice::from_ref(&base), Some(&refs_base), app_shell::warn).unwrap_or(0);
    let col_encontrada = vec![COL_ENCONTRADA.to_string()];

    let (escritor_total, filas_match, ruta_real) = cruzar_y_escribir(
        &Entrada {
            archivo: &base,
            refs: &refs_base,
            columnas: &cols_base,
            clave: &clave_base,
        },
        &Busqueda {
            lookup: &lookup,
            ac: &ac,
            claves_originales: &claves_originales,
            salida_traer: &col_encontrada,
            opcion,
        },
        &Salida {
            columnas: &columnas_salida,
            solo_match,
            ruta: &ruta,
            total,
            etiqueta_barra: &format!(
                "Buscando en {}",
                base.file_name().unwrap_or_default().to_string_lossy()
            ),
        },
    )?;

    cerrar_cruce(accion, &ruta_real, &base, escritor_total, filas_match, total)
}
