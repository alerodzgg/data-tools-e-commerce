//! Diálogo y plomería compartidos por BUSCARV, BUSCARV parcial y Encontrar:
//! qué archivos/columnas cruzar, qué hacer con las coincidencias, y el
//! núcleo de cruce por lotes acotados.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use commerce_core::{columnas_union, EscritorXlsx};
use polars::prelude::*;

use crate::comunes::{
    abortar_si_reservadas, excluir_refs, preguntar_hojas_excluir_de, renombrar_o_avisar, seleccionar_archivo,
    seleccionar_columnas, Etiquetada,
};
use etl_tools::Busqueda;

use crate::AppResult;

const FILAS_MIN_LOTE_PARCIAL: usize = 500_000;
const CELDAS_POR_LOTE_PARCIAL: usize = 20_000_000;

// ════════════════════════════════════════════════════════════════════════
// Diálogo y plomería compartidos por BUSCARV / BUSCARV parcial / Encontrar
// ════════════════════════════════════════════════════════════════════════

pub(crate) struct ParametrosCruce {
    pub(crate) base: PathBuf,
    pub(crate) tabla: PathBuf,
    pub(crate) excluir_base: HashSet<String>,
    pub(crate) excluir_tabla: HashSet<String>,
    pub(crate) cols_base: Vec<String>,
    pub(crate) clave_base: String,
    pub(crate) clave_tabla: String,
    pub(crate) columnas_traer: Vec<String>,
}

pub(crate) fn preparar_cruce(prompt_base: &str, prompt_tabla: &str) -> AppResult<Option<ParametrosCruce>> {
    let Some(base) = seleccionar_archivo("Selecciona el archivo BASE (el que se va a enriquecer)")? else {
        return Ok(None);
    };
    let Some(tabla) = seleccionar_archivo("Selecciona el archivo TABLA (de donde se traen los datos)")?
    else {
        return Ok(None);
    };
    let Some(excluir_base) = preguntar_hojas_excluir_de(&base, "la Base")? else {
        return Ok(None);
    };
    let Some(excluir_tabla) = preguntar_hojas_excluir_de(&tabla, "la Tabla")? else {
        return Ok(None);
    };

    let cols_base = columnas_union(
        std::slice::from_ref(&base),
        Some(&excluir_refs(&excluir_base)),
        app_shell::warn,
    );
    if cols_base.is_empty() {
        app_shell::error(&format!(
            "No se pudieron leer columnas de '{}'.",
            base.file_name().unwrap_or_default().to_string_lossy()
        ));
        return Ok(None);
    }
    let cols_tabla = columnas_union(
        std::slice::from_ref(&tabla),
        Some(&excluir_refs(&excluir_tabla)),
        app_shell::warn,
    );
    if cols_tabla.is_empty() {
        app_shell::error(&format!(
            "No se pudieron leer columnas de '{}'.",
            tabla.file_name().unwrap_or_default().to_string_lossy()
        ));
        return Ok(None);
    }
    if abortar_si_reservadas(&cols_base) || abortar_si_reservadas(&cols_tabla) {
        return Ok(None);
    }

    let Some(clave_base) = app_shell::menu_seleccionar_nav(prompt_base, cols_base.clone())? else {
        return Ok(None);
    };
    let Some(clave_tabla) = app_shell::menu_seleccionar_nav(prompt_tabla, cols_tabla.clone())? else {
        return Ok(None);
    };

    let candidatas: Vec<String> = cols_tabla
        .iter()
        .filter(|c| **c != clave_tabla)
        .cloned()
        .collect();
    if candidatas.is_empty() {
        app_shell::warn("La Tabla no tiene otras columnas para traer.");
        return Ok(None);
    }
    let columnas_traer = seleccionar_columnas(&candidatas, "TRAER de la Tabla")?;
    if columnas_traer.is_empty() {
        app_shell::warn("Sin columnas a traer. Operación cancelada.");
        return Ok(None);
    }

    Ok(Some(ParametrosCruce {
        base,
        tabla,
        excluir_base,
        excluir_tabla,
        cols_base,
        clave_base,
        clave_tabla,
        columnas_traer,
    }))
}

/// Qué hacer con las filas que cruzaron. Viaja como valor entre
/// `preguntar_accion`, `ruta_cruce` y `cerrar_cruce`, que deben coincidir en
/// su interpretación: como índice numérico, esas tres lecturas podían
/// divergir sin que nada lo detectara.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccionCruce {
    /// Sobrescribir el archivo base (se escribe a un temporal y se renombra).
    SobrescribirBase,
    /// Dejar el resultado en un archivo nuevo.
    ArchivoNuevo,
    /// Solo el reporte, con las filas que cruzaron.
    SoloReporte,
}

/// Menú de las tres [`AccionCruce`] con etiquetas a medida del modo que
/// pregunta. Devuelve la variante por VALOR; cancelar equivale a
/// `SoloReporte`, la opción que no toca ningún archivo existente.
pub(crate) fn elegir_accion(mensaje: &str, etiquetas: [String; 3]) -> AppResult<AccionCruce> {
    let [sobre, nuevo, reporte] = etiquetas;
    let opciones = vec![
        Etiquetada::nueva(AccionCruce::SobrescribirBase, sobre),
        Etiquetada::nueva(AccionCruce::ArchivoNuevo, nuevo),
        Etiquetada::nueva(AccionCruce::SoloReporte, reporte),
    ];
    Ok(app_shell::menu_seleccionar_nav(mensaje, opciones)?.map_or(AccionCruce::SoloReporte, |o| o.valor))
}

/// Pregunta qué hacer con las coincidencias de un cruce (BUSCARV/Encontrar).
pub(crate) fn preguntar_accion(base: &Path, sobrescribir_como: Option<&str>) -> AppResult<AccionCruce> {
    let texto_sobre = sobrescribir_como.map(str::to_string).unwrap_or_else(|| {
        format!(
            "Limpiar el archivo base ('{}') (sobrescribir)",
            base.file_name().unwrap_or_default().to_string_lossy()
        )
    });
    elegir_accion(
        "¿Qué quieres hacer con las coincidencias?",
        [
            texto_sobre,
            "Crear un archivo nuevo con el resultado".to_string(),
            "Solo generar el reporte de coincidencias (solo las que cruzaron)".to_string(),
        ],
    )
}

/// Ruta de salida para BUSCARV según la acción elegida. `prefijo` distingue
/// exacto ("") de parcial ("parcial_") en el nombre, incluido el temporal de
/// `SobrescribirBase`: sin él, BUSCARV exacto y parcial sobre la MISMA base
/// se pisarían el mismo archivo temporal.
pub(crate) fn ruta_cruce(accion: AccionCruce, base: &Path, ruta_salida: &Path, prefijo: &str) -> PathBuf {
    let stem = base.file_stem().unwrap_or_default().to_string_lossy();
    match accion {
        AccionCruce::SobrescribirBase => base.with_file_name(format!("{stem}__tmp_{prefijo}buscarv.xlsx")),
        AccionCruce::ArchivoNuevo => {
            commerce_core::ruta_unica(ruta_salida.join(format!("buscarv_{prefijo}{stem}.xlsx")))
        }
        AccionCruce::SoloReporte => {
            commerce_core::ruta_unica(ruta_salida.join(format!("coincidencias_{prefijo}{stem}.xlsx")))
        }
    }
}

pub(crate) fn cerrar_cruce(
    accion: AccionCruce,
    ruta: &commerce_core::RutaEscritaReal,
    base: &Path,
    total_escrito: u64,
    filas_match: u64,
    filas_total: u64,
) -> AppResult<()> {
    let sin_match = filas_total.saturating_sub(filas_match);
    let resumen = format!("Con coincidencia: {filas_match} · sin coincidencia: {sin_match}.");
    match accion {
        AccionCruce::SobrescribirBase => {
            renombrar_o_avisar(ruta, base)?;
            app_shell::success(&format!(
                "Archivo base sobrescrito: '{}' ({total_escrito} filas). {resumen}",
                base.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        AccionCruce::ArchivoNuevo => app_shell::success(&format!(
            "Guardado: '{}' ({total_escrito} filas). {resumen}",
            ruta.file_name().unwrap_or_default().to_string_lossy()
        )),
        AccionCruce::SoloReporte => app_shell::success(&format!(
            "Reporte de coincidencias: '{}' ({total_escrito} filas que cruzaron). {resumen}",
            ruta.file_name().unwrap_or_default().to_string_lossy()
        )),
    }
    Ok(())
}

/// Núcleo compartido de BUSCARV parcial y Encontrar: cruza `archivo` contra
/// el autómata Aho-Corasick por lotes acotados (`FILAS_MIN_LOTE_PARCIAL`/
/// `CELDAS_POR_LOTE_PARCIAL`), escribe el resultado y limpia la salida a
/// medias si algo falla. Lo único que difiere entre ambos modos son estos
/// parámetros.
///
/// Devuelve `(filas_escritas, filas_con_match, ruta_real)` para que el
/// llamador arme su propio `cerrar_cruce`. `ruta_real` es la ruta EFECTIVA
/// usada por `EscritorXlsx`, que puede diferir de `ruta` si la redirigió por
/// una colisión: sobrescribir el archivo base con `ruta` movería un archivo
/// que esta llamada nunca escribió.
/// De dónde salen las filas a cruzar.
pub(crate) struct Entrada<'a> {
    pub archivo: &'a Path,
    pub refs: &'a [&'a str],
    pub columnas: &'a [String],
    pub clave: &'a str,
}

/// Qué se escribe y dónde.
pub(crate) struct Salida<'a> {
    pub columnas: &'a [String],
    pub solo_match: bool,
    pub ruta: &'a Path,
    pub total: u64,
    pub etiqueta_barra: &'a str,
}

pub(crate) fn cruzar_y_escribir(
    entrada: &Entrada,
    busqueda: &Busqueda,
    salida: &Salida,
) -> AppResult<(u64, u64, commerce_core::RutaEscritaReal)> {
    let Entrada {
        archivo,
        refs,
        columnas: columnas_archivo,
        clave,
    } = *entrada;
    let Busqueda {
        lookup,
        ac,
        claves_originales,
        salida_traer: salida_cols,
        opcion,
    } = *busqueda;
    let Salida {
        columnas: columnas_salida,
        solo_match,
        ruta,
        total,
        etiqueta_barra,
    } = *salida;
    let filas_por_lote = FILAS_MIN_LOTE_PARCIAL.max(CELDAS_POR_LOTE_PARCIAL / columnas_archivo.len().max(1));
    let barra = app_shell::barra_progreso(etiqueta_barra, total);

    let procesar =
        |escritor: &mut EscritorXlsx, lote: &mut Vec<DataFrame>, filas_match: &mut u64| -> AppResult<()> {
            if lote.is_empty() {
                return Ok(());
            }
            let mut bloque = lote.remove(0);
            for parte in lote.drain(..) {
                bloque.vstack_mut_owned(parte)?;
            }
            let alto = bloque.height() as u64;
            let (mut out, n_match) = etl_tools::cruzar_chunk_parcial(
                &bloque,
                clave,
                &Busqueda {
                    lookup,
                    ac,
                    claves_originales,
                    salida_traer: salida_cols,
                    opcion,
                },
            )?;
            *filas_match += n_match as u64;
            if solo_match {
                let mascara = out.column("_match")?.bool()?.clone();
                out = out.filter(&mascara)?;
            }
            if out.height() > 0 {
                escritor.escribir(&out.select(columnas_salida)?, None)?;
            }
            barra.inc(alto);
            Ok(())
        };

    let mut escritor = etl_tools::nuevo_escritor(ruta, columnas_salida.to_vec())?;
    let ruta_real = commerce_core::RutaEscritaReal::nueva(escritor.ruta.clone());
    let ejecucion = (|| -> AppResult<u64> {
        let mut lote: Vec<DataFrame> = Vec::new();
        let mut alto_lote = 0usize;
        let mut filas_match = 0u64;
        for chunk in etl_tools::iter_hojas_valores(
            std::slice::from_ref(&archivo.to_path_buf()),
            Some(refs),
            app_shell::warn,
        )? {
            let chunk = etl_tools::preparar_chunk_clave(&chunk, columnas_archivo, clave)?;
            alto_lote += chunk.height();
            lote.push(chunk);
            if alto_lote >= filas_por_lote {
                procesar(&mut escritor, &mut lote, &mut filas_match)?;
                alto_lote = 0;
            }
        }
        procesar(&mut escritor, &mut lote, &mut filas_match)?;
        Ok(filas_match)
    })();
    barra.finish_and_clear();

    let filas_match = match ejecucion {
        Ok(v) => v,
        Err(e) => {
            let _ = escritor.abortar();
            return Err(e);
        }
    };
    Ok((escritor.total as u64, filas_match, ruta_real))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruta_cruce_temporal_distingue_exacto_de_parcial_sobre_la_misma_base() {
        // BUSCARV exacto y parcial pueden correr sobre la MISMA base: si el
        // temporal de `SobrescribirBase` ignorara el prefijo, ambos
        // escribirían en el mismo archivo y se pisarían entre sí.
        let base = Path::new("carpeta/datos.xlsx");
        let salida = Path::new("salida");
        let exacto = ruta_cruce(AccionCruce::SobrescribirBase, base, salida, "");
        let parcial = ruta_cruce(AccionCruce::SobrescribirBase, base, salida, "parcial_");
        assert_ne!(
            exacto, parcial,
            "exacto y parcial no deben compartir el mismo temporal"
        );
    }
}
