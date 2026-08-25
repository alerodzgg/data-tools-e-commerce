//! Piezas puras de `FileWorkflow`: alinear un bloque al esquema de columnas
//! de la unión, y repartir el resultado materializado entre el escritor de
//! aprobadas y el de rechazadas. La orquestación completa (selección
//! interactiva de columnas URL, descarga async, `run`) vive en el binario
//! (`bin/ocr_tools.rs::ejecutar_archivo`), que combina estas piezas puras
//! con `AsyncBatchProcessor` y `app_shell`.

use std::collections::HashMap;

use commerce_core::{CoreResult, EscritorXlsx, OpcionesEscritorXlsx};
use polars::prelude::*;

use crate::batch::columna_texto;
use crate::url_compactor;

/// Límite de filas por hoja al guardar (Excel admite 1 048 576, cabecera
/// incluida).
const EXCEL_FILAS_POR_HOJA: usize = 1_000_000;

/// El escritor de `commerce_core`, con las hojas `Parte_N` que usa esta
/// herramienta.
pub fn escritor(
    ruta: impl AsRef<std::path::Path>,
    columnas: Vec<String>,
    avisar: impl FnMut(&str) + 'static,
) -> CoreResult<EscritorXlsx> {
    EscritorXlsx::con_avisador(
        ruta,
        OpcionesEscritorXlsx {
            columnas: Some(columnas),
            filas_por_hoja: EXCEL_FILAS_POR_HOJA,
            hoja_por_defecto: "Parte".to_string(),
            numerar_siempre: true,
            recortar_vacias: true,
        },
        avisar,
    )
}

/// Añade como nulas las columnas de la unión que falten en este bloque
/// (hoja): así todos los bloques de un archivo comparten el mismo esquema.
pub fn alinear(mut bloque: DataFrame, columnas: &[String]) -> PolarsResult<DataFrame> {
    for c in columnas {
        if bloque.column(c).is_err() {
            let nulos: Vec<Option<String>> = vec![None; bloque.height()];
            bloque.with_column(Column::new(c.as_str().into(), nulos))?;
        }
    }
    Ok(bloque)
}

/// Nombres de columna que el flujo usa internamente (auxiliares o de
/// salida). Si el archivo del usuario trajera una así, se sobrescribiría o
/// duplicaría en silencio: se comprueba al inicio y se salta el archivo.
const COLUMNAS_RESERVADAS: [&str; 7] = [
    "Motivo_Rechazo",
    "_compacto",
    "_ri",
    "_source_sheet",
    "_imagen_aprobada",
    "_tiene_rechazada",
    "_imagen_motivo",
];

/// Columnas de `columnas` que chocan con nombres reservados por la
/// herramienta (incluye los dinámicos `_<col>_aprobada` / `_<col>_motivo`).
/// Vacío si no hay colisión.
pub fn columnas_reservadas_en_choque(columnas: &[String]) -> Vec<String> {
    columnas
        .iter()
        .filter(|c| {
            COLUMNAS_RESERVADAS.contains(&c.as_str())
                || (c.starts_with('_') && (c.ends_with("_aprobada") || c.ends_with("_motivo")))
        })
        .cloned()
        .collect()
}

fn columna_bool(df: &DataFrame, nombre: &str) -> PolarsResult<Vec<bool>> {
    Ok(df
        .column(nombre)?
        .bool()?
        .iter()
        .map(|v| v.unwrap_or(false))
        .collect())
}

fn tomar_filas(df: &DataFrame, indices: &[usize]) -> PolarsResult<DataFrame> {
    let idx_ca = IdxCa::from_vec(PlSmallStr::EMPTY, indices.iter().map(|&i| i as IdxSize).collect());
    df.take(&idx_ca)
}

pub struct ResultadoBloque {
    pub aprobadas: usize,
    pub rechazadas: usize,
}

/// Reparte `df_result` (salida de [`crate::batch::materialize`]) entre
/// aprobadas y rechazadas, restaura las URLs originales en el archivo de
/// rechazadas, y escribe ambos bloques.
///
/// `rechazadas_solo`:
///   - `false` → cada fila rechazada trae TODAS sus imágenes originales.
///   - `true`  → solo las imágenes rechazadas (las aprobadas se vacían) y se
///     compactan a la izquierda; el motivo se renumera para seguir el nuevo
///     orden (invariante: `url_columns` debe respetar el orden físico
///     izquierda-a-derecha de la hoja, igual que en el original).
pub fn escribir_bloque(
    df_result: &DataFrame,
    bloque_original: &DataFrame,
    url_columns: &[String],
    rechazadas_solo: bool,
    escritor_aprob: &mut EscritorXlsx,
    escritor_rech: &mut EscritorXlsx,
) -> CoreResult<ResultadoBloque> {
    let n = df_result.height();
    let imagen_aprobada = columna_bool(df_result, "_imagen_aprobada")?;
    let tiene_rechazada = columna_bool(df_result, "_tiene_rechazada")?;

    let idx_aprobadas: Vec<usize> = (0..n).filter(|&i| imagen_aprobada[i]).collect();
    let aprobadas_df = tomar_filas(df_result, &idx_aprobadas)?;
    let aprobadas_df = url_compactor::compact(&aprobadas_df, url_columns)?;
    escritor_aprob.escribir(&aprobadas_df, None)?;

    let idx_rech: Vec<usize> = (0..n).filter(|&i| tiene_rechazada[i]).collect();
    let n_rech = idx_rech.len();

    if n_rech > 0 {
        let mut rechazadas_df = tomar_filas(df_result, &idx_rech)?;

        let originales: HashMap<&str, Vec<Option<String>>> = url_columns
            .iter()
            .map(|c| Ok::<_, PolarsError>((c.as_str(), columna_texto(bloque_original, c)?)))
            .collect::<PolarsResult<_>>()?;

        if !rechazadas_solo {
            for col in url_columns {
                let orig = &originales[col.as_str()];
                let vals: Vec<Option<String>> = idx_rech.iter().map(|&i| orig[i].clone()).collect();
                rechazadas_df.with_column(Column::new(col.as_str().into(), vals))?;
            }
            rechazadas_df.rename("_imagen_motivo", "Motivo_Rechazo".into())?;
        } else {
            let mut restaurada: HashMap<&str, Vec<String>> = HashMap::new();
            for col in url_columns {
                let aprob_col = columna_bool(&rechazadas_df, &format!("_{col}_aprobada"))?;
                let orig = &originales[col.as_str()];
                let vals: Vec<String> = idx_rech
                    .iter()
                    .enumerate()
                    .map(|(pos, &i)| {
                        if aprob_col[pos] {
                            String::new()
                        } else {
                            orig[i].clone().unwrap_or_default()
                        }
                    })
                    .collect();
                restaurada.insert(col.as_str(), vals);
            }

            let motivo_por_col: HashMap<&str, Vec<String>> = url_columns
                .iter()
                .map(|c| {
                    let valores = columna_texto(&rechazadas_df, &format!("_{c}_motivo"))?
                        .into_iter()
                        .map(|v| v.unwrap_or_default())
                        .collect();
                    Ok::<_, PolarsError>((c.as_str(), valores))
                })
                .collect::<PolarsResult<_>>()?;

            let mut nuevos_motivos = Vec::with_capacity(n_rech);
            for pos in 0..n_rech {
                let mut partes: Vec<String> = Vec::new();
                let mut k = 0;
                for col in url_columns {
                    let valor = &restaurada[col.as_str()][pos];
                    if url_compactor::tiene_valor(valor) {
                        k += 1;
                        let razon = &motivo_por_col[col.as_str()][pos];
                        partes.push(format!("[Imagen {k}] {razon}").trim_end().to_string());
                    }
                }
                nuevos_motivos.push(partes.join(" | "));
            }

            for col in url_columns {
                rechazadas_df
                    .with_column(Column::new(col.as_str().into(), restaurada[col.as_str()].clone()))?;
            }
            rechazadas_df = url_compactor::compact(&rechazadas_df, url_columns)?;
            rechazadas_df.with_column(Column::new("_imagen_motivo".into(), nuevos_motivos))?;
            rechazadas_df.rename("_imagen_motivo", "Motivo_Rechazo".into())?;
        }

        escritor_rech.escribir(&rechazadas_df, None)?;
    }

    Ok(ResultadoBloque {
        aprobadas: aprobadas_df.height(),
        rechazadas: n_rech,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch::{materialize, CellResult};

    fn fixture() -> (DataFrame, DataFrame, Vec<String>) {
        let url_columns: Vec<String> = ["Imagen 1", "Imagen 2", "Imagen 3", "Imagen 4"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let bloque = df! {
            "Sku" => ["S0", "S1"],
            "Imagen 1" => ["urlA", "urlW"],
            "Imagen 2" => ["urlB", "urlX"],
            "Imagen 3" => ["urlC", "urlY"],
            "Imagen 4" => ["urlD", "urlZ"],
        }
        .unwrap();

        let cr = |idx: i64, col: &str, approved: bool, reason: &str| CellResult {
            idx,
            col: col.to_string(),
            approved,
            reason: reason.to_string(),
        };
        let mut resultados = HashMap::new();
        resultados.insert((0, "Imagen 1".to_string()), cr(0, "Imagen 1", false, "D1·Banner"));
        resultados.insert((0, "Imagen 3".to_string()), cr(0, "Imagen 3", false, "D2·Fondo"));
        resultados.insert((1, "Imagen 2".to_string()), cr(1, "Imagen 2", false, "D4·Logo"));
        resultados.insert(
            (1, "Imagen 4".to_string()),
            cr(1, "Imagen 4", false, "D5·Placeholder"),
        );

        let df_result = materialize(&bloque, &url_columns, &resultados, 0).unwrap();
        (df_result, bloque, url_columns)
    }

    fn leer_rechazadas(ruta: &std::path::Path) -> DataFrame {
        let mut libro = commerce_core::abrir_libro(ruta).unwrap();
        commerce_core::leer_hoja_por_nombre(&mut libro, ruta, "Parte_1").unwrap()
    }

    #[test]
    fn rechazadas_modo_todas_las_imagenes_restaura_todas_las_originales() {
        let (df_result, bloque, url_columns) = fixture();
        let tmp = tempfile::tempdir().unwrap();
        let columnas = {
            let mut c = vec!["Sku".to_string()];
            c.extend(url_columns.clone());
            c
        };
        let mut columnas_rech = columnas.clone();
        columnas_rech.push("Motivo_Rechazo".to_string());

        let mut ap = escritor(tmp.path().join("ap.xlsx"), columnas, |_| {}).unwrap();
        let ruta_re = tmp.path().join("re.xlsx");
        let mut re = escritor(&ruta_re, columnas_rech, |_| {}).unwrap();

        escribir_bloque(&df_result, &bloque, &url_columns, false, &mut ap, &mut re).unwrap();
        ap.cerrar().unwrap();
        re.cerrar().unwrap();

        let salida = leer_rechazadas(&ruta_re);
        let fila0 = |c: &str| {
            salida
                .column(c)
                .unwrap()
                .str()
                .unwrap()
                .get(0)
                .map(str::to_string)
        };
        assert_eq!(fila0("Imagen 1").as_deref(), Some("urlA"));
        assert_eq!(fila0("Imagen 2").as_deref(), Some("urlB"));
        assert_eq!(fila0("Imagen 3").as_deref(), Some("urlC"));
        assert_eq!(fila0("Imagen 4").as_deref(), Some("urlD"));
        assert_eq!(
            fila0("Motivo_Rechazo").as_deref(),
            Some("[Imagen 1] D1·Banner | [Imagen 3] D2·Fondo")
        );
    }

    #[test]
    fn rechazadas_solo_las_rechazadas_renumera_el_motivo() {
        let (df_result, bloque, url_columns) = fixture();
        let tmp = tempfile::tempdir().unwrap();
        let mut columnas = vec!["Sku".to_string()];
        columnas.extend(url_columns.clone());
        let mut columnas_rech = columnas.clone();
        columnas_rech.push("Motivo_Rechazo".to_string());

        let mut ap = escritor(tmp.path().join("ap.xlsx"), columnas, |_| {}).unwrap();
        let ruta_re = tmp.path().join("re.xlsx");
        let mut re = escritor(&ruta_re, columnas_rech, |_| {}).unwrap();

        escribir_bloque(&df_result, &bloque, &url_columns, true, &mut ap, &mut re).unwrap();
        ap.cerrar().unwrap();
        re.cerrar().unwrap();

        let salida = leer_rechazadas(&ruta_re);
        let fila = |i: usize, c: &str| {
            salida
                .column(c)
                .unwrap()
                .str()
                .unwrap()
                .get(i)
                .map(str::to_string)
        };

        assert_eq!(fila(0, "Imagen 1").as_deref(), Some("urlA"));
        assert_eq!(fila(0, "Imagen 2").as_deref(), Some("urlC"));
        assert_eq!(fila(0, "Imagen 3"), None);
        assert_eq!(
            fila(0, "Motivo_Rechazo").as_deref(),
            Some("[Imagen 1] D1·Banner | [Imagen 2] D2·Fondo")
        );

        assert_eq!(fila(1, "Imagen 1").as_deref(), Some("urlX"));
        assert_eq!(fila(1, "Imagen 2").as_deref(), Some("urlZ"));
        assert_eq!(fila(1, "Imagen 3"), None);
        assert_eq!(
            fila(1, "Motivo_Rechazo").as_deref(),
            Some("[Imagen 1] D4·Logo | [Imagen 2] D5·Placeholder")
        );
    }

    #[test]
    fn devuelve_conteos_de_aprobadas_y_rechazadas() {
        let (df_result, bloque, url_columns) = fixture();
        let tmp = tempfile::tempdir().unwrap();
        let mut columnas = vec!["Sku".to_string()];
        columnas.extend(url_columns.clone());
        let mut columnas_rech = columnas.clone();
        columnas_rech.push("Motivo_Rechazo".to_string());

        let mut ap = escritor(tmp.path().join("ap.xlsx"), columnas, |_| {}).unwrap();
        let mut re = escritor(tmp.path().join("re.xlsx"), columnas_rech, |_| {}).unwrap();

        let resultado = escribir_bloque(&df_result, &bloque, &url_columns, false, &mut ap, &mut re).unwrap();
        ap.cerrar().unwrap();
        re.cerrar().unwrap();

        // Ambas filas tienen al menos una rechazada; ambas tienen al menos una aprobada.
        assert_eq!(resultado.aprobadas, 2);
        assert_eq!(resultado.rechazadas, 2);
    }

    #[test]
    fn alinear_agrega_como_nulas_las_columnas_faltantes() {
        let bloque = df! { "Sku" => ["A1"] }.unwrap();
        let out = alinear(bloque, &["Sku".to_string(), "Imagen 1".to_string()]).unwrap();
        assert!(out.column("Imagen 1").is_ok());
        assert_eq!(out.column("Imagen 1").unwrap().null_count(), 1);
    }

    #[test]
    fn columnas_reservadas_en_choque_detecta_fijas_y_dinamicas() {
        let columnas = vec![
            "Sku".to_string(),
            "Motivo_Rechazo".to_string(),
            "_Imagen 1_aprobada".to_string(),
            "_Imagen 1_motivo".to_string(),
            "Precio".to_string(),
        ];
        let chocan = columnas_reservadas_en_choque(&columnas);
        assert_eq!(
            chocan,
            vec![
                "Motivo_Rechazo".to_string(),
                "_Imagen 1_aprobada".to_string(),
                "_Imagen 1_motivo".to_string(),
            ]
        );
    }

    #[test]
    fn columnas_reservadas_en_choque_vacio_sin_colisiones() {
        let columnas = vec!["Sku".to_string(), "Precio".to_string()];
        assert!(columnas_reservadas_en_choque(&columnas).is_empty());
    }
}
