use std::path::Path;
use std::sync::LazyLock;

use commerce_core::CoreResult;
use polars::prelude::*;
use regex::Regex;

use crate::comunes::{
    aplicar_oem3, columna_precio2, columna_texto, extraer_precio_entero, generar_sku, separar_por_categorias,
};
use crate::constantes::{verificar_columnas_reservadas, COL_IMAGENES, COL_OPCION, COL_PRECIO, COL_START_URL};

static RE_REF_FINAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/ref=.*$").unwrap());
static RE_AC_VARIANTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\._AC_.*?_\.").unwrap());
static RE_ID_AMAZON: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/dp/([A-Z0-9]+)(?:[/?]|$)").unwrap());
static RE_VISIT_THE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^visit\s+the\s*").unwrap());

/// Tratamiento de imágenes de Amazon (alta resolución): reemplaza variantes
/// como `._AC_SX466_.` por `._AC_SL1500_.`; si se pidió, vacía Imagen 2/3/4.
fn limpiar_imagenes_amazon(df: &DataFrame, borrar_imagenes_extra: bool) -> CoreResult<DataFrame> {
    let mut df = df.clone();
    for &c in COL_IMAGENES.iter() {
        if df.column(c).is_err() {
            continue;
        }
        if borrar_imagenes_extra && matches!(c, "Imagen 2" | "Imagen 3" | "Imagen 4") {
            df.with_column(Column::new(c.into(), vec![String::new(); df.height()]))?;
            continue;
        }
        let valores: Vec<String> = columna_texto(&df, c)?
            .into_iter()
            .map(|v| {
                RE_AC_VARIANTE
                    .replace(v.unwrap_or_default().as_str(), "._AC_SL1500_.")
                    .into_owned()
            })
            .collect();
        df.with_column(Column::new(c.into(), valores))?;
    }
    Ok(df)
}

/// Lógica específica para Amazon: elimina columnas innecesarias, genera
/// SKU con ASIN, limpia imágenes para alta resolución.
///
/// `contador_jitter` (si `Some`) PERSISTE entre llamadas (una por hoja): sin
/// esto, el jitter de `Precio2` colisionaba entre hojas (ver
/// [`crate::comunes::columna_precio2`]).
pub fn procesar_dataframe_amazon(
    df: &DataFrame,
    modificar_oem: bool,
    borrar_imagenes_extra: bool,
    contador_jitter: Option<&mut u64>,
) -> CoreResult<DataFrame> {
    let mut df = df.clone();

    let cols_a_borrar = ["pagination", "product-link", COL_START_URL, "web-scraper-order"];
    let existentes: Vec<&str> = cols_a_borrar
        .iter()
        .copied()
        .filter(|c| df.column(c).is_ok())
        .collect();
    if !existentes.is_empty() {
        df = df.drop_many(existentes);
    }
    if df.column("product-link-href").is_ok() {
        df = df.rename("product-link-href", COL_START_URL.into())?.clone();
    }
    if df.column(COL_START_URL).is_ok() {
        let valores: Vec<String> = columna_texto(&df, COL_START_URL)?
            .into_iter()
            .map(|v| {
                RE_REF_FINAL
                    .replace(v.unwrap_or_default().as_str(), "")
                    .into_owned()
            })
            .collect();
        df.with_column(Column::new(COL_START_URL.into(), valores))?;
    }

    if df.column(COL_PRECIO).is_ok() {
        df = extraer_precio_entero(&df, COL_PRECIO)?;
        let precio2 = columna_precio2(&df, COL_PRECIO, contador_jitter)?;
        df.with_column(Column::new("Precio2".into(), precio2))?;
    }

    df = limpiar_imagenes_amazon(&df, borrar_imagenes_extra)?;
    df = generar_sku(&df, &RE_ID_AMAZON, Some(&RE_VISIT_THE))?;

    if modificar_oem {
        df = aplicar_oem3(&df)?;
    }

    Ok(df)
}

fn renombrar_imagenes_src(df: DataFrame) -> CoreResult<DataFrame> {
    let mut df = df;
    for i in 1..=4 {
        let origen = format!("Imagen {i}-src");
        let destino = format!("Imagen {i}");
        if df.column(&origen).is_ok() && df.column(&destino).is_err() {
            df = df.rename(&origen, destino.as_str().into())?.clone();
        }
    }
    Ok(df)
}

/// Orquesta el modo 'Amazon': lee el archivo hoja por hoja, procesa y
/// escribe en streaming (dos hojas de salida: `Hoja1` y `Eliminadas`).
///
/// `borrar_imagenes_extra`: si se debe vaciar Imagen 2/3/4 (la pregunta al
/// usuario es responsabilidad de la interfaz; aquí llega ya resuelta).
pub fn ejecutar_procesamiento_amazon(
    archivo_entrada: &Path,
    archivo_salida: &Path,
    modificar_oem: bool,
    borrar_imagenes_extra: bool,
    mut avisar: impl FnMut(&str),
    mut progreso: impl FnMut(u64),
) -> CoreResult<()> {
    let mut libro = commerce_core::abrir_libro(archivo_entrada)?;
    let sheet_names = commerce_core::nombres_hojas_libro(&libro);
    let mut contador_jitter: u64 = 0;

    crate::escritor::ejecutar_con_escritor(archivo_salida, &mut avisar, |escritor, avisar| {
        for hoja in &sheet_names {
            let df_temp = match commerce_core::leer_hoja_por_nombre(&mut libro, archivo_entrada, hoja) {
                Ok(df) => df,
                Err(error) => {
                    avisar(&format!("Error leyendo hoja '{hoja}': {error}"));
                    continue;
                }
            };
            let df_temp = renombrar_imagenes_src(df_temp)?;

            verificar_columnas_reservadas(
                df_temp.get_column_names().iter().map(|s| s.as_str()),
                &format!("La hoja '{hoja}'"),
            )?;

            let filas_leidas = df_temp.height() as u64;
            let df_procesado = procesar_dataframe_amazon(
                &df_temp,
                modificar_oem,
                borrar_imagenes_extra,
                Some(&mut contador_jitter),
            )?;
            drop(df_temp);

            let mask_opcion = crate::comunes::mask_opcion(&df_procesado)?;
            let mask_sin_imagenes = crate::comunes::mask_sin_imagenes(&df_procesado)?;
            let n = df_procesado.height();
            let mask_precio_cero: Vec<bool> = if df_procesado.column(COL_PRECIO).is_ok() {
                df_procesado
                    .column(COL_PRECIO)?
                    .as_materialized_series()
                    .i64()?
                    .iter()
                    .map(|v| v == Some(0))
                    .collect()
            } else {
                vec![false; n]
            };

            let (mut df_validas, df_eliminadas) = separar_por_categorias(
                &df_procesado,
                &[
                    (&mask_opcion, "Tiene contenido en Opcion"),
                    (&mask_sin_imagenes, "Sin imágenes"),
                    (&mask_precio_cero, "Precio es 0"),
                ],
            )?;
            if df_validas.column(COL_OPCION).is_ok() {
                df_validas = df_validas.drop(COL_OPCION)?;
            }
            if df_validas.column(COL_PRECIO).is_ok() {
                df_validas = df_validas.drop(COL_PRECIO)?;
            }

            escritor.escribir(&df_validas, Some("Hoja1"))?;
            escritor.escribir(&df_eliminadas, Some("Eliminadas"))?;

            progreso(filas_leidas);
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{escribir_libro, leer_hojas};

    #[test]
    fn sku_usa_asin_y_quita_visit_the() -> CoreResult<()> {
        let df = df!(
            "Precio" => ["$85.00"],
            "Tienda" => ["Visit the Bosch Store"],
            "product-link-href" => ["https://amazon.com/dp/B08X1234?ref=abc"],
        )?;
        let salida = procesar_dataframe_amazon(&df, false, false, None)?;
        assert_eq!(
            salida.column("Sku")?.str()?.get(0),
            Some("u-85-boschstore-B08X1234")
        );
        Ok(())
    }

    #[test]
    fn imagen_alta_resolucion_y_borrado_de_extras() -> CoreResult<()> {
        let df = df!(
            "Imagen 1" => ["https://m.media-amazon.com/images/I/x._AC_SX466_.jpg"],
            "Imagen 2" => ["https://m.media-amazon.com/images/I/y._AC_SX300_.jpg"],
        )?;
        let sin_borrar = procesar_dataframe_amazon(&df, false, false, None)?;
        assert_eq!(
            sin_borrar.column("Imagen 1")?.str()?.get(0),
            Some("https://m.media-amazon.com/images/I/x._AC_SL1500_.jpg")
        );
        assert_eq!(
            sin_borrar.column("Imagen 2")?.str()?.get(0),
            Some("https://m.media-amazon.com/images/I/y._AC_SL1500_.jpg")
        );

        let con_borrado = procesar_dataframe_amazon(&df, false, true, None)?;
        assert_eq!(con_borrado.column("Imagen 2")?.str()?.get(0), Some(""));
        Ok(())
    }

    #[test]
    fn e2e_renombra_imagen_src_y_filtra_precio_cero() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("amz.xlsx");
        escribir_libro(
            &entrada,
            &[(
                "Hoja1",
                vec![
                    vec!["product-link-href", "Precio", "Tienda", "Imagen 1-src"],
                    vec![
                        "https://amazon.com/dp/B001AAA/ref=x",
                        "$85.00",
                        "TiendaX",
                        "https://x.com/1._AC_SX300_.jpg",
                    ],
                    vec![
                        "https://amazon.com/dp/B002BBB/ref=y",
                        "$0.00",
                        "TiendaX",
                        "https://x.com/2._AC_SX300_.jpg",
                    ],
                ],
            )],
        );

        let salida = tmp.path().join("amz_out.xlsx");
        ejecutar_procesamiento_amazon(&entrada, &salida, false, false, |_| {}, |_| {})?;

        let hojas = leer_hojas(&salida);
        let validas = &hojas["Hoja1"];
        assert_eq!(validas.height(), 1, "la de precio 0 se descarta");
        assert_eq!(
            validas.column("Imagen 1")?.str()?.get(0),
            Some("https://x.com/1._AC_SL1500_.jpg"),
            "Imagen 1-src se renombra"
        );
        assert_eq!(validas.column("Sku")?.str()?.get(0), Some("u-85-tiendax-B001AAA"));

        let eliminadas = &hojas["Eliminadas"];
        assert_eq!(eliminadas.height(), 1);
        assert_eq!(
            eliminadas.column("Motivo_Eliminacion")?.str()?.get(0),
            Some("Precio es 0")
        );
        Ok(())
    }

    #[test]
    fn columnas_reservadas_abortan_sin_dejar_salida() {
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("res.xlsx");
        escribir_libro(
            &entrada,
            &[("Hoja1", vec![vec!["Sku", "Precio"], vec!["s1", "$1.00"]])],
        );

        let salida = tmp.path().join("res_out.xlsx");
        let resultado = ejecutar_procesamiento_amazon(&entrada, &salida, false, false, |_| {}, |_| {});
        assert!(
            resultado.is_err(),
            "una columna reservada debe abortar el procesamiento"
        );
        assert!(!salida.exists(), "no debe quedar una salida a medias");
    }

    #[test]
    fn archivo_corrupto_no_deja_salida_a_medias() {
        let tmp = tempfile::tempdir().unwrap();
        let bueno = tmp.path().join("bueno.xlsx");
        escribir_libro(&bueno, &[("Hoja1", vec![vec!["Precio"]])]);
        let bytes = std::fs::read(&bueno).unwrap();
        let truncado = tmp.path().join("truncado.xlsx");
        std::fs::write(&truncado, &bytes[..bytes.len() / 2]).unwrap();

        let salida = tmp.path().join("salida.xlsx");
        let resultado = ejecutar_procesamiento_amazon(&truncado, &salida, false, false, |_| {}, |_| {});
        assert!(resultado.is_err());
        assert!(!salida.exists());
    }
}
