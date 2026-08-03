//! Las herramientas del workspace tienen que poder encadenarse: lo que
//! escribe una es la entrada de la siguiente.
//!
//! `EscritorXlsx` (usado por builder, validator, etl_tools y data_combinator)
//! y `ImageEmbedder` (que lee con `umya-spreadsheet`) son los dos extremos de
//! esa cadena, y viven en crates distintos: sin un test acá, nada verifica
//! que el formato que produce uno sea el que acepta el otro.

use std::collections::HashSet;

use commerce_core::escritor_xlsx::OpcionesEscritorXlsx;
use commerce_core::EscritorXlsx;
use ocr_tools::downloader::DownloadConfig;
use ocr_tools::image_embedder::{ImageEmbedConfig, ImageEmbedder};
use polars::prelude::*;

#[tokio::test]
async fn ocr_tools_puede_abrir_lo_que_escribe_el_escritor_del_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let origen = tmp.path().join("salida_de_otra_herramienta.xlsx");

    let df = df!("Sku" => ["A1"], "Fotos" => ["http://127.0.0.1:1/x.png"]).unwrap();
    let mut escritor = EscritorXlsx::nuevo(&origen, OpcionesEscritorXlsx::default()).unwrap();
    escritor.escribir(&df, Some("Datos")).unwrap();
    escritor.cerrar().unwrap();
    let origen = escritor.ruta.clone();

    let embedder = ImageEmbedder::new(ImageEmbedConfig::default(), DownloadConfig::default(), 4);
    let resultado = embedder
        .procesar_archivo(&origen, &HashSet::new(), &tmp.path().join("con_img.xlsx"), |_| {})
        .await;

    // Se exige que PROCESE, no solo que no diga "corrupto": `SinColumnasDeUrl`
    // también significa que no vio los datos, y satisfacía la versión débil de
    // esta aserción mientras el archivo se leía como una hoja vacía.
    let (_, _, fallos, hojas) = resultado.expect("debe procesar el archivo, no rechazarlo");
    assert_eq!(hojas, 1, "no vio la hoja con datos");
    assert_eq!(fallos, 1, "no vio la URL de la columna de fotos");
}

/// La dirección inversa: lo que escribe `ocr_tools` tiene que poder leerlo el
/// resto del workspace, SIN perder el contrato central del producto.
///
/// `ImageEmbedder` lee y reescribe con `umya-spreadsheet`, no con
/// `EscritorXlsx`. Ese escritor es el único del workspace que no controlamos,
/// así que es el único punto donde un código como `007` podría volverse el
/// número 7 sin que nada lo impida.
#[tokio::test]
async fn insertar_imagenes_no_convierte_los_codigos_en_numeros() {
    let tmp = tempfile::tempdir().unwrap();
    let origen = tmp.path().join("skus.xlsx");

    let codigos = ["007", "0012", "1.50", "1e5", "0000"];
    let urls = vec!["http://127.0.0.1:1/x.png"; codigos.len()];
    let df = df!("Sku" => codigos.to_vec(), "Fotos" => urls).unwrap();
    let mut escritor = EscritorXlsx::nuevo(&origen, OpcionesEscritorXlsx::default()).unwrap();
    escritor.escribir(&df, Some("Datos")).unwrap();
    escritor.cerrar().unwrap();
    let origen = escritor.ruta.clone();

    let embedder = ImageEmbedder::new(ImageEmbedConfig::default(), DownloadConfig::default(), 4);
    let (salida, ..) = embedder
        .procesar_archivo(&origen, &HashSet::new(), &tmp.path().join("con_img.xlsx"), |_| {})
        .await
        .expect("debe procesar el archivo");

    // Se relee con el lector del workspace (calamine), que es el que usarían
    // etl_tools, data_combinator o el validator sobre este mismo archivo.
    let mut libro = commerce_core::abrir_libro(&salida).unwrap();
    let hoja = commerce_core::nombres_hojas_libro(&libro)[0].clone();
    let df = commerce_core::leer_hoja_por_nombre(&mut libro, &salida, &hoja).unwrap();
    let leidos: Vec<String> = df
        .column("Sku")
        .unwrap()
        .str()
        .unwrap()
        .iter()
        .map(|v| v.unwrap_or("").to_string())
        .collect();

    assert_eq!(
        leidos, codigos,
        "un código perdió su forma al pasar por el escritor de ocr_tools"
    );
}
