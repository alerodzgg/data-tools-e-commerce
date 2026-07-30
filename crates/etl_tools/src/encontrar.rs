use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;

use commerce_core::CoreResult;
use polars::prelude::*;
use regex::Regex;

use crate::coincidencia_parcial::norm_parcial;
use crate::lectura::iter_hojas_valores;

/// Nombre canónico que el lector asigna a una cabecera VACÍA (`Columna_N`):
/// si la "primera fila es una palabra" pero la celda estaba vacía, NO es
/// palabra real.
static RE_COLUMNA_CANONICA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Columna_\d+$").unwrap());

/// Convierte la lista de palabras en el lookup `[clave_norm, _rank,
/// Palabra_encontrada]` que consume el motor del BUSCARV parcial. `_rank` se
/// asigna TRAS descartar las entradas inválidas, así que lo que importa (y
/// lo que garantiza) es el ORDEN RELATIVO: palabra anterior en la lista =
/// rank menor = mayor prioridad. Duplicados (tras normalizar) y entradas que
/// quedan vacías (solo símbolos) se descartan conservando la PRIMERA
/// aparición.
pub fn lookup_palabras(palabras: &[String]) -> CoreResult<DataFrame> {
    let mut vistos: HashSet<String> = HashSet::new();
    let mut claves_norm: Vec<String> = Vec::new();
    let mut originales: Vec<String> = Vec::new();

    for palabra in palabras {
        let norm = norm_parcial(palabra);
        if norm.is_empty() || !vistos.insert(norm.clone()) {
            continue;
        }
        claves_norm.push(norm);
        originales.push(palabra.clone());
    }

    let rank: Vec<u64> = (0..claves_norm.len() as u64).collect();
    Ok(df!(
        "clave_norm" => claves_norm,
        "_rank" => rank,
        "Palabra_encontrada" => originales,
    )?)
}

/// Lee las palabras de la PRIMERA columna de cada hoja de `archivo`.
///
/// El lector consume la fila 1 como CABECERA; en una lista de palabras "a
/// pelo" (sin fila de título) eso se tragaría la PRIMERA palabra en
/// silencio. Con `primera_fila_es_palabra=true`, el nombre de la columna se
/// antepone como palabra en cada hoja — salvo que sea un `Columna_N`
/// canónico (venía de una celda vacía: no es una palabra real).
pub fn palabras_de_xlsx(
    archivo: &Path,
    excluir: Option<&[&str]>,
    primera_fila_es_palabra: bool,
    avisar: impl FnMut(&str),
) -> CoreResult<Vec<String>> {
    let chunks = iter_hojas_valores(std::slice::from_ref(&archivo.to_path_buf()), excluir, avisar)?;
    let mut palabras = Vec::new();
    for chunk in chunks {
        let Some(primera) = chunk.get_column_names().first().map(|s| s.to_string()) else {
            continue;
        };
        if primera_fila_es_palabra && !RE_COLUMNA_CANONICA.is_match(&primera) {
            palabras.push(primera.trim().to_string());
        }
        for valor in chunk.column(&primera)?.str()?.iter().flatten() {
            let recortado = valor.trim();
            if !recortado.is_empty() {
                palabras.push(recortado.to_string());
            }
        }
    }
    Ok(palabras)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_dedup_y_prioridad() -> CoreResult<()> {
        let palabras: Vec<String> = ["Honda", "DEPO", "honda", "faro led", "***", "Depo!", "Civic"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let lookup = lookup_palabras(&palabras)?;
        assert_eq!(lookup.height(), 4);
        let encontradas: Vec<_> = lookup
            .column("Palabra_encontrada")?
            .str()?
            .iter()
            .flatten()
            .collect();
        assert_eq!(encontradas, vec!["Honda", "DEPO", "faro led", "Civic"]);
        let ranks: Vec<u64> = lookup.column("_rank")?.u64()?.into_no_null_iter().collect();
        let mut ordenados = ranks.clone();
        ordenados.sort_unstable();
        assert_eq!(ranks, ordenados);
        let distintos: HashSet<_> = ranks.iter().collect();
        assert_eq!(distintos.len(), ranks.len());
        Ok(())
    }

    fn libro_de_una_columna(ruta: &Path, valores: &[Option<&str>]) {
        use rust_xlsxwriter::Workbook;
        let mut wb = Workbook::new();
        let hoja = wb.add_worksheet();
        for (fila, valor) in valores.iter().enumerate() {
            if let Some(v) = valor {
                hoja.write(fila as u32, 0, *v).unwrap();
            }
        }
        wb.save(ruta).unwrap();
    }

    #[test]
    fn palabras_de_xlsx_avisa_si_el_archivo_no_se_pudo_leer() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("corrupto.xlsx");
        std::fs::write(&archivo, b"esto no es un xlsx").unwrap();

        let avisos = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let avisos_clon = avisos.clone();
        let palabras = palabras_de_xlsx(&archivo, None, false, move |m: &str| {
            avisos_clon.borrow_mut().push(m.to_string())
        })?;

        assert!(palabras.is_empty());
        assert!(
            !avisos.borrow().is_empty(),
            "debe avisar que el archivo no se pudo leer, no quedarse en silencio"
        );
        Ok(())
    }

    #[test]
    fn primera_fila_es_palabra() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("palabras.xlsx");
        libro_de_una_columna(&archivo, &[Some("honda"), Some("depo"), Some("faro led")]);

        let con_cabecera = palabras_de_xlsx(&archivo, None, false, |_| {})?;
        assert_eq!(
            con_cabecera,
            vec!["depo", "faro led"],
            "la 1ª fila se trata como título"
        );

        let sin_cabecera = palabras_de_xlsx(&archivo, None, true, |_| {})?;
        assert_eq!(
            sin_cabecera,
            vec!["honda", "depo", "faro led"],
            "'honda' recuperada y primera"
        );
        Ok(())
    }

    #[test]
    fn celda_inicial_vacia_no_es_palabra() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("vacia.xlsx");
        libro_de_una_columna(&archivo, &[None, Some("honda"), Some("depo")]);

        assert_eq!(
            palabras_de_xlsx(&archivo, None, true, |_| {})?,
            vec!["honda", "depo"]
        );
        Ok(())
    }
}
