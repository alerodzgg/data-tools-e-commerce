use std::path::Path;

use commerce_core::CoreResult;
use polars::prelude::*;

use crate::comunes::{
    agregar_caracteristicas_y_cantidades, aplicar_transformaciones_comunes, columna_precio2,
    extraer_precio_entero, separar_por_categorias,
};
use crate::constantes::{verificar_columnas_reservadas, COL_OPCION, COL_PRECIO, COL_TITULO};

/// Procesa una hoja completa en modo 'Unicas': limpia imágenes/precios,
/// genera OEM/SKU/Caracteristicas/Cantidades. Vectorizado a nivel de
/// DataFrame (sin loop de filas en el llamador).
///
/// `contador_jitter` (si `Some`) PERSISTE entre llamadas (una por hoja): sin
/// esto, el jitter de `Precio2` colisionaba entre hojas (ver
/// [`crate::comunes::columna_precio2`]).
pub fn procesar_dataframe_unicas(
    df: &DataFrame,
    modificar_oem: bool,
    contador_jitter: Option<&mut u64>,
) -> CoreResult<DataFrame> {
    let mut df = df.clone();
    if df.column("web-scraper-order").is_ok() {
        df = df.drop("web-scraper-order")?;
    }

    if df.column(COL_PRECIO).is_ok() {
        df = extraer_precio_entero(&df, COL_PRECIO)?;
        let precio2 = columna_precio2(&df, COL_PRECIO, contador_jitter)?;
        df.with_column(Column::new("Precio2".into(), precio2))?;
    }

    df = aplicar_transformaciones_comunes(&df, modificar_oem, &[])?;

    if df.column(COL_TITULO).is_ok() {
        df = agregar_caracteristicas_y_cantidades(&df)?;
    }

    Ok(df)
}

/// Separa `df_procesado` en (válidas, eliminadas-con-motivo) aplicando los
/// filtros en orden: primero "Tiene contenido en Opcion", luego (para las
/// que sobreviven ese filtro) "Sin imágenes".
fn separar_validas_y_eliminadas(df_procesado: &DataFrame) -> CoreResult<(DataFrame, DataFrame)> {
    let mask_opcion = crate::comunes::mask_opcion(df_procesado)?;
    let mask_sin_imagenes = crate::comunes::mask_sin_imagenes(df_procesado)?;

    let (mut df_validas, df_eliminadas) = separar_por_categorias(
        df_procesado,
        &[
            (&mask_opcion, "Tiene contenido en Opcion"),
            (&mask_sin_imagenes, "Sin imágenes"),
        ],
    )?;

    // `Caracteristicas`/`Cantidades` (derivadas de `Titulo` en
    // `procesar_dataframe_unicas`) sí sirven de diagnóstico en `Eliminadas`
    // (por eso se calculan siempre, incluso para filas que van a terminar
    // acá) pero no forman parte de la plantilla de publicación de eBay: se
    // descartan solo de `df_validas`, no de `df_eliminadas`. (El modo Amazon
    // ni siquiera las calcula — su parseo de título es distinto — así que no
    // hay equivalente que perder ahí.)
    let cols_a_borrar = [COL_PRECIO, COL_OPCION, "Caracteristicas", "Cantidades"];
    let existentes: Vec<&str> = cols_a_borrar
        .iter()
        .copied()
        .filter(|c| df_validas.column(c).is_ok())
        .collect();
    if !existentes.is_empty() {
        df_validas = df_validas.drop_many(existentes);
    }

    Ok((df_validas, df_eliminadas))
}

/// Orquesta el modo 'Unicas': lee el archivo hoja por hoja, procesa y
/// escribe en streaming (dos hojas de salida: `Hoja1` y `Eliminadas`).
///
/// `avisar`: mensajes no críticos (hoja no legible, columnas descartadas…).
/// `progreso`: filas procesadas, para que el llamador actualice su barra.
///
/// Un archivo corrupto o con columnas reservadas no deja una salida a
/// medias: en error, se aborta el escritor (que borra el .xlsx parcial).
pub fn ejecutar_procesamiento_unicas(
    archivo_entrada: &Path,
    archivo_salida: &Path,
    modificar_oem: bool,
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

            verificar_columnas_reservadas(
                df_temp.get_column_names().iter().map(|s| s.as_str()),
                &format!("La hoja '{hoja}'"),
            )?;

            let filas_leidas = df_temp.height() as u64;
            let df_procesado =
                procesar_dataframe_unicas(&df_temp, modificar_oem, Some(&mut contador_jitter))?;
            drop(df_temp);

            let (df_validas, df_eliminadas) = separar_validas_y_eliminadas(&df_procesado)?;
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
    fn e2e_dos_hojas_distinto_orden_y_sin_imagen_4() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("in.xlsx");
        escribir_libro(
            &entrada,
            &[
                (
                    "HojaA",
                    vec![
                        vec![
                            "web-scraper-start-url",
                            "Precio",
                            "Opcion",
                            "Titulo",
                            "Tienda",
                            "Imagen 1",
                            "Imagen 2",
                            "Imagen 3",
                        ],
                        vec![
                            "https://ebay.com/itm/12345",
                            "85",
                            "",
                            "Pair Kit Bumper Front LH",
                            "Mi Tienda: SA",
                            "x \"https://c.com/a/b/c.jpg\" y",
                            "",
                            "",
                        ],
                        vec![
                            "https://ebay.com/itm/22222",
                            "20",
                            "con-opcion",
                            "T2",
                            "T",
                            "https://c.com/i/j.jpg",
                            "",
                            "",
                        ],
                        vec!["https://ebay.com/itm/33333", "30", "", "T3", "T", "", "", ""],
                    ],
                ),
                (
                    "HojaB",
                    vec![
                        vec![
                            "Titulo",
                            "web-scraper-start-url",
                            "Precio",
                            "Opcion",
                            "Tienda",
                            "Imagen 1",
                            "Imagen 2",
                            "Imagen 3",
                        ],
                        vec![
                            "Set Grille",
                            "https://ebay.com/itm/98765",
                            "150",
                            "",
                            "OtraTienda",
                            "https://c.com/z/w.jpg",
                            "",
                            "",
                        ],
                    ],
                ),
            ],
        );

        let salida = tmp.path().join("out.xlsx");
        ejecutar_procesamiento_unicas(&entrada, &salida, false, |_| {}, |_| {})?;

        let hojas = leer_hojas(&salida);
        let validas = &hojas["Hoja1"];
        let eliminadas = &hojas["Eliminadas"];

        assert_eq!(
            validas.height(),
            2,
            "una válida por hoja, sin crash por 'Imagen 4'"
        );
        assert_eq!(eliminadas.height(), 2);
        assert!(eliminadas.column("Motivo_Eliminacion").is_ok());

        let sku: Vec<_> = validas.column("Sku")?.str()?.iter().map(|v| v.unwrap()).collect();
        assert_eq!(sku[0], "u-85-mitiendasa-12345");
        let img1: Vec<_> = validas
            .column("Imagen 1")?
            .str()?
            .iter()
            .map(|v| v.unwrap())
            .collect();
        assert_eq!(img1[0], "https://c.com/a/b/s-l1600.jpg");
        assert!(validas.column("Precio").is_err() && validas.column("Opcion").is_err());

        let titulo: Vec<_> = validas
            .column("Titulo")?
            .str()?
            .iter()
            .map(|v| v.unwrap())
            .collect();
        assert_eq!(titulo[1], "Set Grille", "la hoja B se realinea");
        assert_eq!(sku[1], "u-150-otratienda-98765");
        Ok(())
    }

    #[test]
    fn caracteristicas_y_cantidades_solo_sobreviven_en_eliminadas_no_en_hoja1() {
        // `agregar_caracteristicas_y_cantidades` corre siempre (antes de
        // separar válidas/eliminadas), pero `separar_validas_y_eliminadas`
        // las descarta solo de `Hoja1` (no es parte de la plantilla de eBay);
        // en `Eliminadas` quedan como diagnóstico. Sin este test, un cambio
        // que las dejara filtrar a `Hoja1` (o que las perdiera también de
        // `Eliminadas`) pasaría desapercibido.
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("in.xlsx");
        escribir_libro(
            &entrada,
            &[(
                "Hoja1",
                vec![
                    vec![
                        "web-scraper-start-url",
                        "Precio",
                        "Opcion",
                        "Titulo",
                        "Tienda",
                        "Imagen 1",
                    ],
                    vec![
                        "https://ebay.com/itm/12345",
                        "85",
                        "",
                        "Pair Kit Bumper Front LH",
                        "Mi Tienda: SA",
                        "https://c.com/a.jpg",
                    ],
                    vec![
                        "https://ebay.com/itm/22222",
                        "20",
                        "con-opcion",
                        "T2",
                        "T",
                        "https://c.com/b.jpg",
                    ],
                ],
            )],
        );

        let salida = tmp.path().join("out.xlsx");
        ejecutar_procesamiento_unicas(&entrada, &salida, false, |_| {}, |_| {}).unwrap();

        let hojas = leer_hojas(&salida);
        let validas = &hojas["Hoja1"];
        let eliminadas = &hojas["Eliminadas"];

        assert!(
            validas.column("Caracteristicas").is_err() && validas.column("Cantidades").is_err(),
            "Hoja1 (plantilla de eBay) no debe traer estas columnas internas"
        );
        assert!(
            eliminadas.column("Caracteristicas").is_ok() && eliminadas.column("Cantidades").is_ok(),
            "Eliminadas sí debe conservarlas: son diagnóstico de por qué se descartó la fila"
        );
    }

    #[test]
    fn columnas_reservadas_abortan_sin_dejar_salida() {
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("res.xlsx");
        escribir_libro(
            &entrada,
            &[("Hoja1", vec![vec!["Sku", "Titulo"], vec!["s1", "t1"]])],
        );

        let salida = tmp.path().join("res_out.xlsx");
        let resultado = ejecutar_procesamiento_unicas(&entrada, &salida, false, |_| {}, |_| {});
        assert!(resultado.is_err());
        assert!(!salida.exists(), "no debe quedar una salida a medias");
    }

    #[test]
    fn archivo_corrupto_no_deja_salida_a_medias() {
        let tmp = tempfile::tempdir().unwrap();
        let bueno = tmp.path().join("bueno.xlsx");
        escribir_libro(&bueno, &[("Hoja1", vec![vec!["Sku", "Titulo"]])]);
        let bytes = std::fs::read(&bueno).unwrap();
        let truncado = tmp.path().join("truncado.xlsx");
        std::fs::write(&truncado, &bytes[..bytes.len() / 2]).unwrap();

        let salida = tmp.path().join("salida.xlsx");
        let resultado = ejecutar_procesamiento_unicas(&truncado, &salida, false, |_| {}, |_| {});
        assert!(resultado.is_err());
        assert!(!salida.exists());
    }
}
