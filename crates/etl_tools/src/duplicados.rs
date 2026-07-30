use std::collections::{HashMap, HashSet};
use std::path::Path;

use commerce_core::CoreResult;
use polars::prelude::*;

use crate::escritor::nuevo_escritor;
use crate::lectura::{columna_texto_o_vacia, iter_hojas_valores, preparar_chunk_clave};

/// Reemplaza vocales acentuadas por su versión simple (á→a, é→e, í→i, ó→o,
/// ú/ü→u), para que la misma palabra escrita con o sin tilde normalice a la
/// misma clave (`"Camión"` == `"Camion"`). La `ñ` se conserva TAL CUAL: no es
/// una vocal con acento sino una letra distinta del alfabeto español —
/// transliterarla a `n` fusionaría palabras realmente distintas (`"año"` /
/// `"ano"`, `"campaña"` / `"campana"`). Espera texto ya en minúsculas.
pub(crate) fn quitar_tildes(texto: &str) -> String {
    texto
        .chars()
        .map(|c| match c {
            'á' => 'a',
            'é' => 'e',
            'í' => 'i',
            'ó' => 'o',
            'ú' | 'ü' => 'u',
            otro => otro,
        })
        .collect()
}

/// Clave normalizada: minúsculas, tildes transliteradas (`ñ` conservada,
/// ver [`quitar_tildes`]) y solo caracteres alfanuméricos + `ñ` (igual que
/// `str.to_lowercase().str.replace_all(r"[^a-z0-9ñ]", "")` en Polars, tras
/// quitar tildes).
pub fn clave_limpia_de(valor: Option<&str>) -> String {
    quitar_tildes(&valor.unwrap_or("").to_lowercase())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == 'ñ')
        .collect()
}

/// Pasada barata: claves limpias únicas del archivo (solo la columna clave),
/// excluyendo la clave vacía (celdas en blanco o solo símbolos no cruzan
/// entre archivos).
pub fn claves_unicas(
    archivo: &Path,
    columna_clave: &str,
    excluir: Option<&[&str]>,
    avisar: impl FnMut(&str),
) -> CoreResult<HashSet<String>> {
    let chunks = iter_hojas_valores(std::slice::from_ref(&archivo.to_path_buf()), excluir, avisar)?;
    let mut claves = HashSet::new();
    for chunk in chunks {
        for valor in columna_texto_o_vacia(&chunk, columna_clave)? {
            let k = clave_limpia_de(valor.as_deref());
            if !k.is_empty() {
                claves.insert(k);
            }
        }
    }
    Ok(claves)
}

/// Devuelve (claves limpias que aparecen 2+ veces, nº total de filas
/// involucradas). El conteo de filas sale GRATIS de esta misma pasada.
pub fn claves_repetidas(
    archivo: &Path,
    columna_clave: &str,
    excluir: Option<&[&str]>,
    avisar: impl FnMut(&str),
) -> CoreResult<(HashSet<String>, u64)> {
    let chunks = iter_hojas_valores(std::slice::from_ref(&archivo.to_path_buf()), excluir, avisar)?;
    let mut conteo: HashMap<String, u64> = HashMap::new();
    for chunk in chunks {
        for valor in columna_texto_o_vacia(&chunk, columna_clave)? {
            let k = clave_limpia_de(valor.as_deref());
            if !k.is_empty() {
                *conteo.entry(k).or_insert(0) += 1;
            }
        }
    }
    let repetidas: Vec<(String, u64)> = conteo.into_iter().filter(|(_, n)| *n > 1).collect();
    let n_filas: u64 = repetidas.iter().map(|(_, n)| n).sum();
    Ok((repetidas.into_iter().map(|(k, _)| k).collect(), n_filas))
}

/// Re-lee `ruta_entrada` por hojas y escribe en `ruta_salida` (streaming) las
/// filas cuya clave limpia esté (o no, según `mantener_si_en`) en `claves`.
/// Devuelve cuántas filas se escribieron.
#[allow(clippy::too_many_arguments)]
pub fn escribir_filtrado(
    ruta_entrada: &Path,
    ruta_salida: &Path,
    columnas: &[String],
    columna_clave: &str,
    claves: &HashSet<String>,
    mantener_si_en: bool,
    excluir: Option<&[&str]>,
    avisar: impl FnMut(&str),
    mut progreso: impl FnMut(u64),
) -> CoreResult<usize> {
    let mut escritor = nuevo_escritor(ruta_salida, columnas.to_vec())?;
    let chunks = iter_hojas_valores(std::slice::from_ref(&ruta_entrada.to_path_buf()), excluir, avisar)?;
    for chunk in chunks {
        let filas_entrada = chunk.height();
        let preparado = preparar_chunk_clave(&chunk, columnas, columna_clave)?;
        let pertenece: Vec<bool> = columna_texto_o_vacia(&preparado, columna_clave)?
            .into_iter()
            .map(|v| {
                let en_claves = claves.contains(&clave_limpia_de(v.as_deref()));
                if mantener_si_en {
                    en_claves
                } else {
                    !en_claves
                }
            })
            .collect();
        let mascara = BooleanChunked::from_iter_values("m".into(), pertenece.into_iter());
        let seleccion = preparado.filter(&mascara)?;
        if seleccion.height() > 0 {
            escritor.escribir(&seleccion, None)?;
        }
        progreso(filas_entrada as u64);
    }
    escritor.cerrar()?;
    Ok(escritor.total)
}

/// UNA sola pasada que escribe las dos salidas a la vez:
///   - `ruta_dup`: reporte con TODAS las copias de las claves repetidas.
///   - `ruta_limpia` (si `Some`): versión DEDUPLICADA keep-first — conserva
///     la PRIMERA aparición (en orden de lectura) de cada clave repetida.
///
/// Devuelve (filas_reporte, filas_limpio).
#[allow(clippy::too_many_arguments)]
pub fn escribir_reporte_y_limpio(
    ruta_entrada: &Path,
    ruta_dup: &Path,
    ruta_limpia: Option<&Path>,
    columnas: &[String],
    columna_clave: &str,
    claves_repetidas: &HashSet<String>,
    excluir: Option<&[&str]>,
    avisar: impl FnMut(&str),
    mut progreso: impl FnMut(u64),
) -> CoreResult<(usize, usize)> {
    let mut escritor_dup = nuevo_escritor(ruta_dup, columnas.to_vec())?;
    let mut escritor_limpio = match ruta_limpia {
        Some(r) => Some(nuevo_escritor(r, columnas.to_vec())?),
        None => None,
    };
    let mut emitidas: HashSet<String> = HashSet::new();

    let chunks = iter_hojas_valores(std::slice::from_ref(&ruta_entrada.to_path_buf()), excluir, avisar)?;
    for chunk in chunks {
        let filas_entrada = chunk.height();
        let preparado = preparar_chunk_clave(&chunk, columnas, columna_clave)?;
        let claves_col: Vec<String> = columna_texto_o_vacia(&preparado, columna_clave)?
            .iter()
            .map(|v| clave_limpia_de(v.as_deref()))
            .collect();
        let es_repetida: Vec<bool> = claves_col.iter().map(|k| claves_repetidas.contains(k)).collect();

        let mascara_rep = BooleanChunked::from_iter_values("m".into(), es_repetida.iter().copied());
        let duplicados = preparado.filter(&mascara_rep)?;
        if duplicados.height() > 0 {
            escritor_dup.escribir(&duplicados, None)?;
        }

        if let Some(escritor_limpio) = escritor_limpio.as_mut() {
            let mut vistas_en_chunk: HashSet<String> = HashSet::new();
            let mut mantener: Vec<bool> = Vec::with_capacity(claves_col.len());
            let mut nuevas: Vec<String> = Vec::new();
            for (i, k) in claves_col.iter().enumerate() {
                if !es_repetida[i] {
                    mantener.push(true);
                    continue;
                }
                let primera_en_chunk = vistas_en_chunk.insert(k.clone());
                let se_mantiene = primera_en_chunk && !emitidas.contains(k);
                mantener.push(se_mantiene);
                if se_mantiene {
                    nuevas.push(k.clone());
                }
            }
            emitidas.extend(nuevas);
            let mascara_limpio = BooleanChunked::from_iter_values("m".into(), mantener.into_iter());
            let limpio = preparado.filter(&mascara_limpio)?;
            if limpio.height() > 0 {
                escritor_limpio.escribir(&limpio, None)?;
            }
        }
        progreso(filas_entrada as u64);
    }

    escritor_dup.cerrar()?;
    let n_dup = escritor_dup.total;
    let n_limpio = match escritor_limpio {
        Some(mut e) => {
            e.cerrar()?;
            e.total
        }
        None => 0,
    };
    Ok((n_dup, n_limpio))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escribir_libro(ruta: &Path, hojas: &[(&str, Vec<[&str; 2]>)]) {
        use rust_xlsxwriter::Workbook;
        let mut wb = Workbook::new();
        for (nombre, filas) in hojas {
            let hoja = wb.add_worksheet().set_name(*nombre).unwrap();
            for (fila_idx, fila) in filas.iter().enumerate() {
                for (col_idx, valor) in fila.iter().enumerate() {
                    hoja.write(fila_idx as u32, col_idx as u16, *valor).unwrap();
                }
            }
        }
        wb.save(ruta).unwrap();
    }

    #[test]
    fn clave_limpia_de_transcribe_tildes_pero_conserva_la_ene() {
        // Antes: `is_ascii_alphanumeric` BORRABA toda letra acentuada y la
        // `ñ`, así que "Piña" y "Pia" colapsaban a la misma clave "pia" (falso
        // duplicado) Y "Camión"/"Camion" (misma palabra, con/sin tilde —
        // típico entre archivos de distintas fuentes) daban claves distintas
        // (falso "sin coincidencia"). Ahora las vocales acentuadas se
        // transliteran (arregla el segundo caso) pero la `ñ` se conserva tal
        // cual, porque no es una vocal con acento sino una letra distinta
        // ("año" != "ano").
        assert_eq!(clave_limpia_de(Some("Camión")), clave_limpia_de(Some("Camion")));
        assert_ne!(clave_limpia_de(Some("Piña")), clave_limpia_de(Some("Pia")));
        assert_ne!(clave_limpia_de(Some("año")), clave_limpia_de(Some("ano")));
    }

    #[test]
    fn claves_repetidas_avisa_si_el_archivo_no_se_pudo_leer() -> CoreResult<()> {
        // Antes: `claves_repetidas` (como las otras 7 funciones motor
        // hermanas) no exponía `avisar` y pasaba `|_| {}` a
        // `iter_hojas_valores` — un archivo corrupto se trataba como "0
        // filas" en silencio, sin que el usuario se enterara de que en
        // realidad nunca se pudo leer.
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("corrupto.xlsx");
        std::fs::write(&archivo, b"esto no es un xlsx").unwrap();

        let avisos = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let avisos_clon = avisos.clone();
        let (repetidas, n_reps) = claves_repetidas(&archivo, "Sku", None, move |m: &str| {
            avisos_clon.borrow_mut().push(m.to_string())
        })?;

        assert!(repetidas.is_empty());
        assert_eq!(n_reps, 0);
        assert!(
            !avisos.borrow().is_empty(),
            "debe avisar que el archivo no se pudo leer, no quedarse en silencio"
        );
        Ok(())
    }

    #[test]
    fn duplicados_reporte_y_limpio_con_exclusion() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("dup.xlsx");
        escribir_libro(
            &archivo,
            &[
                ("H1", vec![["Sku", "Val"], ["K1", "a"], ["K2", "b"], ["K1", "c"]]),
                ("H2", vec![["Sku", "Val"], ["K1", "d"], ["K3", "e"]]),
                ("Ignorar", vec![["Sku", "Val"], ["K1", "NO-DEBE-SALIR"]]),
            ],
        );

        let excluir = ["ignorar"];
        let (repetidas, n_reps) = claves_repetidas(&archivo, "Sku", Some(&excluir), |_| {})?;
        assert_eq!(repetidas, HashSet::from(["k1".to_string()]));
        assert_eq!(n_reps, 3);

        let rep = tmp.path().join("rep.xlsx");
        let lim = tmp.path().join("lim.xlsx");
        let columnas = vec!["Sku".to_string(), "Val".to_string()];
        let (n_dup, n_lim) = escribir_reporte_y_limpio(
            &archivo,
            &rep,
            Some(&lim),
            &columnas,
            "Sku",
            &repetidas,
            Some(&excluir),
            |_| {},
            |_| {},
        )?;
        assert_eq!(n_dup, 3);
        assert_eq!(n_lim, 3);

        let hojas_rep = commerce_core::iter_hojas_xlsx(&rep, None, |_| {});
        let vals: Vec<_> = hojas_rep[0]
            .column("Val")?
            .str()?
            .iter()
            .map(|v| v.unwrap().to_string())
            .collect();
        assert_eq!(vals, vec!["a", "c", "d"]);

        let hojas_lim = commerce_core::iter_hojas_xlsx(&lim, None, |_| {});
        let skus: Vec<_> = hojas_lim[0]
            .column("Sku")?
            .str()?
            .iter()
            .map(|v| v.unwrap().to_string())
            .collect();
        let vals: Vec<_> = hojas_lim[0]
            .column("Val")?
            .str()?
            .iter()
            .map(|v| v.unwrap().to_string())
            .collect();
        assert_eq!(skus, vec!["K1", "K2", "K3"]);
        assert_eq!(vals, vec!["a", "b", "e"]);
        Ok(())
    }

    #[test]
    fn claves_unicas_normaliza_ignora_vacias_y_respeta_exclusion() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("comp.xlsx");
        escribir_libro(
            &archivo,
            &[
                ("H1", vec![["Sku", "Val"], ["K-1", "a"], ["", "b"], ["k1", "c"]]),
                ("Ignorar", vec![["Sku", "Val"], ["NO-DEBE-ENTRAR", "x"]]),
            ],
        );

        let excluir = ["ignorar"];
        let claves = claves_unicas(&archivo, "Sku", Some(&excluir), |_| {})?;

        assert_eq!(
            claves,
            HashSet::from(["k1".to_string()]),
            "'K-1' y 'k1' normalizan a la misma clave 'k1'; la celda vacía no entra; la hoja excluida no entra"
        );
        Ok(())
    }

    #[test]
    fn escribir_filtrado_mantener_si_en_true_produce_el_reporte_de_coincidencias() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base.xlsx");
        escribir_libro(
            &base,
            &[("H1", vec![["Sku", "Val"], ["K1", "a"], ["K2", "b"], ["K3", "c"]])],
        );

        // Simula "claves de comparación" (el archivo de comparación en
        // `comparar_duplicados`): solo K1 y K3 cruzan.
        let claves_comp = HashSet::from(["k1".to_string(), "k3".to_string()]);
        let columnas = vec!["Sku".to_string(), "Val".to_string()];

        let ruta_dup = tmp.path().join("duplicados.xlsx");
        let n_dup = escribir_filtrado(
            &base,
            &ruta_dup,
            &columnas,
            "Sku",
            &claves_comp,
            true,
            None,
            |_| {},
            |_| {},
        )?;
        assert_eq!(n_dup, 2, "solo las filas cuya clave SÍ está en claves_comp");
        let hojas_dup = commerce_core::iter_hojas_xlsx(&ruta_dup, None, |_| {});
        let skus_dup: Vec<_> = hojas_dup[0]
            .column("Sku")?
            .str()?
            .iter()
            .map(|v| v.unwrap().to_string())
            .collect();
        assert_eq!(skus_dup, vec!["K1", "K3"]);

        let ruta_limpia = tmp.path().join("limpia.xlsx");
        let n_limpia = escribir_filtrado(
            &base,
            &ruta_limpia,
            &columnas,
            "Sku",
            &claves_comp,
            false,
            None,
            |_| {},
            |_| {},
        )?;
        assert_eq!(
            n_limpia, 1,
            "mantener_si_en=false conserva solo las filas SIN coincidencia"
        );
        let hojas_limpia = commerce_core::iter_hojas_xlsx(&ruta_limpia, None, |_| {});
        let skus_limpia: Vec<_> = hojas_limpia[0]
            .column("Sku")?
            .str()?
            .iter()
            .map(|v| v.unwrap().to_string())
            .collect();
        assert_eq!(skus_limpia, vec!["K2"]);
        Ok(())
    }
}
