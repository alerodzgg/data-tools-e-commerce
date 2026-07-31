use std::collections::{HashMap, HashSet};
use std::path::Path;

use commerce_core::{concatenar_vertical, CoreResult, RutaEscritaReal};
use polars::prelude::*;

use crate::coincidencia_parcial::rellenar_texto;
use crate::duplicados::clave_limpia_de;
use crate::escritor::nuevo_escritor;
use crate::lectura::{
    columna_texto_o_vacia, con_clave_normalizada, iter_hojas_valores, preparar_chunk_clave,
};

fn reducir_exacta(t: &DataFrame, columnas_traer: &[String], unir_multiples: bool) -> CoreResult<DataFrame> {
    if unir_multiples {
        let mut aggs = Vec::new();
        for c in columnas_traer {
            aggs.push(
                col(c.as_str())
                    .fill_null(lit(""))
                    .str()
                    .join("\n", true)
                    .alias(c.as_str()),
            );
        }
        Ok(t.clone()
            .lazy()
            .group_by_stable([col("clave_limpia")])
            .agg(aggs)
            .collect()?)
    } else {
        Ok(t.unique_stable(
            Some(&["clave_limpia".to_string()]),
            UniqueKeepStrategy::First,
            None,
        )?)
    }
}

/// Carga la Tabla en memoria como mapa `[clave_limpia, ...columnas_traer]`.
///
/// `unir_multiples=false` conserva la PRIMERA coincidencia de cada clave
/// (como el BUSCARV de Excel); `unir_multiples=true` junta TODAS las
/// coincidencias con salto de línea. Se reduce por hoja ANTES de concatenar
/// (memoria O(claves distintas), no O(filas totales) de la Tabla).
pub fn cargar_tabla_busqueda(
    ruta_tabla: &Path,
    clave_tabla: &str,
    columnas_traer: &[String],
    unir_multiples: bool,
    excluir: Option<&[&str]>,
    avisar: impl FnMut(&str),
) -> CoreResult<DataFrame> {
    let mut columnas_necesarias = vec![clave_tabla.to_string()];
    columnas_necesarias.extend(columnas_traer.iter().cloned());

    let chunks = iter_hojas_valores(std::slice::from_ref(&ruta_tabla.to_path_buf()), excluir, avisar)?;
    let mut partes = Vec::new();
    for chunk in chunks {
        let preparado = preparar_chunk_clave(&chunk, &columnas_necesarias, clave_tabla)?;
        let Some(sub) = con_clave_normalizada(
            &preparado,
            clave_tabla,
            columnas_traer,
            "clave_limpia",
            clave_limpia_de,
        )?
        else {
            continue;
        };
        partes.push(reducir_exacta(&sub, columnas_traer, unir_multiples)?);
    }

    if partes.is_empty() {
        let mut cols = vec![Column::new("clave_limpia".into(), Vec::<String>::new())];
        for c in columnas_traer {
            cols.push(Column::new(c.as_str().into(), Vec::<Option<String>>::new()));
        }
        return Ok(DataFrame::new_infer_height(cols)?);
    }
    if partes.len() == 1 {
        return Ok(partes.into_iter().next().unwrap());
    }
    reducir_exacta(&concatenar_vertical(partes)?, columnas_traer, unir_multiples)
}

/// Nombre de salida de cada columna traída (colisión con la Base → sufijo
/// `_tabla`, `_tabla2`, …).
pub fn renombrar_traidas(
    cols_base: &[String],
    columnas_traer: &[String],
) -> (Vec<String>, HashMap<String, String>) {
    let mut usados: HashSet<String> = cols_base.iter().cloned().collect();
    let mut renombrar = HashMap::new();
    let mut salida_traer = Vec::new();
    for c in columnas_traer {
        let mut nombre = c.clone();
        if usados.contains(&nombre) {
            let mut k = 2u32;
            nombre = format!("{c}_tabla");
            while usados.contains(&nombre) {
                nombre = format!("{c}_tabla{k}");
                k += 1;
            }
        }
        usados.insert(nombre.clone());
        salida_traer.push(nombre.clone());
        renombrar.insert(c.clone(), nombre);
    }
    (salida_traer, renombrar)
}

/// BUSCARV / VLOOKUP: enriquece la Base trayéndole columnas de la Tabla
/// (`lookup`, ya con los nombres de salida aplicados y una columna `_hit`
/// booleana a `true`), cruzando por clave normalizada (LEFT JOIN). Sin
/// coincidencia → celdas vacías. Devuelve (filas_escritas, filas_con_match,
/// filas_totales, ruta_real).
///
/// `ruta_real` es la ruta EFECTIVA donde escribió `EscritorXlsx` — puede
/// diferir de `ruta_salida` si la redirigió por una colisión (p. ej. un
/// temporal rancio de una corrida anterior interrumpida). Quien vaya a
/// sobrescribir el archivo original con el resultado DEBE usar esta ruta,
/// nunca `ruta_salida` directamente.
#[allow(clippy::too_many_arguments)]
pub fn buscarv(
    base: &Path,
    cols_base: &[String],
    clave_base: &str,
    lookup: &DataFrame,
    salida_traer: &[String],
    excluir_base: Option<&[&str]>,
    solo_match: bool,
    ruta_salida: &Path,
    avisar: impl FnMut(&str),
    mut progreso: impl FnMut(u64),
) -> CoreResult<(usize, u64, u64, RutaEscritaReal)> {
    let mut columnas_salida = salida_traer.to_vec();
    columnas_salida.extend(cols_base.iter().cloned());

    let mut escritor = nuevo_escritor(ruta_salida, columnas_salida.clone())?;
    let ruta_real = RutaEscritaReal::nueva(escritor.ruta.clone());

    let resultado = (|| -> CoreResult<(usize, u64, u64)> {
        let chunks = iter_hojas_valores(std::slice::from_ref(&base.to_path_buf()), excluir_base, avisar)?;
        let mut filas_total = 0u64;
        let mut filas_match = 0u64;

        for chunk in chunks {
            let n = chunk.height() as u64;
            let preparado = preparar_chunk_clave(&chunk, cols_base, clave_base)?;
            let claves: Vec<String> = columna_texto_o_vacia(&preparado, clave_base)?
                .iter()
                .map(|v| clave_limpia_de(v.as_deref()))
                .collect();
            let mut preparado = preparado;
            preparado.with_column(Column::new("clave_limpia".into(), claves))?;

            let mut unido = preparado.join(
                lookup,
                ["clave_limpia"],
                ["clave_limpia"],
                JoinArgs::new(JoinType::Left),
                None,
            )?;
            let hit: Vec<bool> = unido
                .column("_hit")?
                .bool()?
                .iter()
                .map(|v| v.unwrap_or(false))
                .collect();
            filas_match += hit.iter().filter(|v| **v).count() as u64;
            rellenar_texto(&mut unido, salida_traer)?;

            let salida = if solo_match {
                let mascara = BooleanChunked::from_iter_values("m".into(), hit.into_iter());
                unido.filter(&mascara)?.select(columnas_salida.clone())?
            } else {
                unido.select(columnas_salida.clone())?
            };
            if salida.height() > 0 {
                escritor.escribir(&salida, None)?;
            }
            filas_total += n;
            progreso(n);
        }

        escritor.cerrar()?;
        Ok((escritor.total, filas_match, filas_total))
    })();

    match resultado {
        Ok((total, filas_match, filas_total)) => Ok((total, filas_match, filas_total, ruta_real)),
        Err(error) => {
            let _ = escritor.abortar();
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escribir_libro(ruta: &Path, filas: &[[&str; 2]]) {
        use rust_xlsxwriter::Workbook;
        let mut wb = Workbook::new();
        let hoja = wb.add_worksheet();
        for (f, fila) in filas.iter().enumerate() {
            for (c, valor) in fila.iter().enumerate() {
                hoja.write(f as u32, c as u16, *valor).unwrap();
            }
        }
        wb.save(ruta).unwrap();
    }

    #[test]
    fn cargar_tabla_busqueda_avisa_si_el_archivo_no_se_pudo_leer() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("corrupto.xlsx");
        std::fs::write(&archivo, b"esto no es un xlsx").unwrap();

        let avisos = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let avisos_clon = avisos.clone();
        let tabla = cargar_tabla_busqueda(
            &archivo,
            "Clave",
            &["Info".to_string()],
            false,
            None,
            move |m: &str| avisos_clon.borrow_mut().push(m.to_string()),
        )?;

        assert_eq!(tabla.height(), 0);
        assert!(
            !avisos.borrow().is_empty(),
            "debe avisar que el archivo no se pudo leer, no quedarse en silencio"
        );
        Ok(())
    }

    #[test]
    fn renombrar_traidas_evita_colision_con_la_base() {
        let (salida, mapa) = renombrar_traidas(
            &["Sku".to_string(), "Precio".to_string()],
            &["Precio".to_string(), "Info".to_string()],
        );
        assert_eq!(salida, vec!["Precio_tabla".to_string(), "Info".to_string()]);
        assert_eq!(mapa["Precio"], "Precio_tabla");
        assert_eq!(mapa["Info"], "Info");
    }

    #[test]
    fn cargar_tabla_conserva_primera_coincidencia_por_defecto() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let archivo = tmp.path().join("tabla.xlsx");
        escribir_libro(
            &archivo,
            &[
                ["Clave", "Info"],
                ["K1", "primero"],
                ["K1", "segundo"],
                ["K2", "unico"],
            ],
        );

        let tabla = cargar_tabla_busqueda(&archivo, "Clave", &["Info".to_string()], false, None, |_| {})?;
        assert_eq!(tabla.height(), 2);
        let fila_k1 = tabla
            .clone()
            .lazy()
            .filter(col("clave_limpia").eq(lit("k1")))
            .collect()?;
        assert_eq!(fila_k1.column("Info")?.str()?.get(0), Some("primero"));
        Ok(())
    }

    #[test]
    fn buscarv_sin_coincidencia_deja_celdas_vacias() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let tabla_archivo = tmp.path().join("tabla.xlsx");
        escribir_libro(&tabla_archivo, &[["Clave", "Info"], ["K1", "valorX"]]);
        let base_archivo = tmp.path().join("base.xlsx");
        escribir_libro(&base_archivo, &[["Sku", "Clave"], ["A", "K1"], ["B", "NoExiste"]]);

        let cols_base = vec!["Sku".to_string(), "Clave".to_string()];
        let (salida_traer, renombrar) = renombrar_traidas(&cols_base, &["Info".to_string()]);
        let mut lookup = cargar_tabla_busqueda(
            &tabla_archivo,
            "Clave",
            &["Info".to_string()],
            false,
            None,
            |_| {},
        )?
        .lazy()
        .rename(
            renombrar.keys().cloned().collect::<Vec<_>>(),
            renombrar.values().cloned().collect::<Vec<_>>(),
            true,
        )
        .collect()?;
        lookup.with_column(Column::new("_hit".into(), vec![true; lookup.height()]))?;

        let ruta_salida = tmp.path().join("out.xlsx");
        let (total, filas_match, filas_total, ruta_real) = buscarv(
            &base_archivo,
            &cols_base,
            "Clave",
            &lookup,
            &salida_traer,
            None,
            false,
            &ruta_salida,
            |_| {},
            |_| {},
        )?;
        assert_eq!(total, 2);
        assert_eq!(filas_match, 1);
        assert_eq!(filas_total, 2);
        assert_eq!(ruta_real.as_path(), ruta_salida);

        let hojas = commerce_core::iter_hojas_xlsx(&ruta_salida, None, |_| {});
        let info: Vec<_> = hojas[0]
            .column("Info")?
            .str()?
            .iter()
            .map(|v| v.unwrap_or("").to_string())
            .collect();
        assert_eq!(info, vec!["valorX", ""]);
        Ok(())
    }

    #[test]
    fn buscarv_devuelve_la_ruta_real_cuando_un_temporal_rancio_fuerza_una_redireccion() -> CoreResult<()> {
        // Mismo bug que en `duplicados.rs`: si la ruta pedida ya existe (un
        // temporal `__tmp_buscarv.xlsx` de una corrida anterior
        // interrumpida), `EscritorXlsx` escribe en "(1).xlsx" — el llamador
        // que va a sobrescribir el archivo base con el resultado necesita
        // esa ruta real, no la pedida.
        let tmp = tempfile::tempdir().unwrap();
        let tabla_archivo = tmp.path().join("tabla.xlsx");
        escribir_libro(&tabla_archivo, &[["Clave", "Info"], ["K1", "valorX"]]);
        let base_archivo = tmp.path().join("base.xlsx");
        escribir_libro(&base_archivo, &[["Sku", "Clave"], ["A", "K1"]]);

        let cols_base = vec!["Sku".to_string(), "Clave".to_string()];
        let (salida_traer, renombrar) = renombrar_traidas(&cols_base, &["Info".to_string()]);
        let mut lookup = cargar_tabla_busqueda(
            &tabla_archivo,
            "Clave",
            &["Info".to_string()],
            false,
            None,
            |_| {},
        )?
        .lazy()
        .rename(
            renombrar.keys().cloned().collect::<Vec<_>>(),
            renombrar.values().cloned().collect::<Vec<_>>(),
            true,
        )
        .collect()?;
        lookup.with_column(Column::new("_hit".into(), vec![true; lookup.height()]))?;

        let ruta_pedida = tmp.path().join("base__tmp_buscarv.xlsx");
        std::fs::write(&ruta_pedida, b"basura de una corrida anterior interrumpida").unwrap();

        let (total, _filas_match, _filas_total, ruta_real) = buscarv(
            &base_archivo,
            &cols_base,
            "Clave",
            &lookup,
            &salida_traer,
            None,
            false,
            &ruta_pedida,
            |_| {},
            |_| {},
        )?;

        assert_eq!(total, 1);
        assert_ne!(
            ruta_real.as_path(),
            ruta_pedida,
            "debió redirigir, esquivando el temporal rancio"
        );
        assert_eq!(
            std::fs::read(&ruta_pedida).unwrap(),
            b"basura de una corrida anterior interrumpida",
            "el temporal rancio no debe tocarse"
        );
        Ok(())
    }
}
