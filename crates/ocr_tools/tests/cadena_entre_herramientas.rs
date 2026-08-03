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
use ocr_tools::image_embedder::{ImageEmbedConfig, ImageEmbedder, MotivoSinProcesar};
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

    assert!(
        !matches!(resultado, Err(MotivoSinProcesar::ArchivoCorrupto)),
        "ocr_tools reporta como CORRUPTO un archivo que escribió el propio workspace"
    );
}
