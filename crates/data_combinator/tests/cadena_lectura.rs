//! Frontera entre los escritores de `commerce_core` y el lector de
//! `data_combinator`.
//!
//! `data_combinator` lee CSV con el crate `csv` — un tercer motor, distinto
//! del `LazyCsvReader` de `etl_tools` y del `calamine` de `commerce_core`.
//! Combinar es además la operación que MÁS encadena: su entrada son siempre
//! salidas de otras herramientas.

use commerce_core::escritor_xlsx::OpcionesEscritorXlsx;
use commerce_core::{EscritorCsv, EscritorXlsx};
use data_combinator::{combinar, Division, Formato, OpcionesCombinar, UmbralesLoteCsv, UmbralesOrden};
use polars::prelude::*;

/// Códigos que un lector descuidado reinterpretaría como números.
const CODIGOS: [&str; 5] = ["007", "0012", "1.50", "1e5", "0000"];

fn df_codigos() -> DataFrame {
    df!("Sku" => CODIGOS.to_vec(), "Nombre" => vec!["pieza"; CODIGOS.len()]).unwrap()
}

fn columna_de(ruta: &std::path::Path) -> Vec<String> {
    let mut lector = csv::ReaderBuilder::new().from_path(ruta).unwrap();
    let idx = lector
        .headers()
        .unwrap()
        .iter()
        .position(|c| c == "Sku")
        .expect("la salida debe conservar la columna");
    lector
        .records()
        .map(|r| r.unwrap().get(idx).unwrap().to_string())
        .collect()
}

/// Combina `entrada` en un CSV y devuelve la columna `Sku` resultante.
fn combinar_a_csv(entrada: &std::path::Path, salida: &std::path::Path, nombre: &str) -> Vec<String> {
    let archivos = vec![entrada.to_path_buf()];
    let columnas = vec!["Sku".to_string(), "Nombre".to_string()];
    let excluir = Vec::new();
    let (rutas, _) = combinar(
        &OpcionesCombinar {
            archivos: &archivos,
            columnas: &columnas,
            hojas_excluir: &excluir,
            formato: Formato::Csv,
            columna_orden: None,
            ascendente: true,
            nombre_salida: nombre,
            ruta_salida: salida,
            division: Division::Ninguna,
            umbrales_orden: UmbralesOrden::default(),
            umbrales_lote_csv: UmbralesLoteCsv::default(),
        },
        |_| {},
        |_| {},
    )
    .expect("combinar debe poder leer lo que escribe el workspace");
    columna_de(&rutas[0])
}

#[test]
fn combinar_no_deforma_los_codigos_de_un_xlsx_del_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let mut escritor =
        EscritorXlsx::nuevo(tmp.path().join("origen.xlsx"), OpcionesEscritorXlsx::default()).unwrap();
    escritor.escribir(&df_codigos(), Some("Datos")).unwrap();
    escritor.cerrar().unwrap();

    assert_eq!(combinar_a_csv(&escritor.ruta, tmp.path(), "desde_xlsx"), CODIGOS);
}

#[test]
fn combinar_no_deforma_los_codigos_de_un_csv_del_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let columnas = vec!["Sku".to_string(), "Nombre".to_string()];
    let mut escritor = EscritorCsv::nuevo(tmp.path().join("origen.csv"), columnas).unwrap();
    escritor.escribir(&df_codigos(), None).unwrap();
    escritor.cerrar().unwrap();

    assert_eq!(combinar_a_csv(&escritor.ruta, tmp.path(), "desde_csv"), CODIGOS);
}

#[test]
fn combinar_con_orden_no_deforma_los_codigos_ni_pierde_filas() {
    // Con `columna_orden` se activa la MEZCLA EXTERNA: los datos se vuelcan a
    // CSV temporales y se refunden con un montículo k-vías. Es un camino de
    // código distinto del de arriba, y el que más manos le pone a los valores.
    // Dos archivos de entrada para que la fusión llegue a interleavear.
    let tmp = tempfile::tempdir().unwrap();
    let mut rutas = Vec::new();
    for (i, mitad) in [&CODIGOS[..2], &CODIGOS[2..]].iter().enumerate() {
        let df = df!("Sku" => mitad.to_vec(), "Nombre" => vec!["pieza"; mitad.len()]).unwrap();
        let mut e = EscritorXlsx::nuevo(
            tmp.path().join(format!("parte{i}.xlsx")),
            OpcionesEscritorXlsx::default(),
        )
        .unwrap();
        e.escribir(&df, Some("Datos")).unwrap();
        e.cerrar().unwrap();
        rutas.push(e.ruta.clone());
    }

    let columnas = vec!["Sku".to_string(), "Nombre".to_string()];
    let excluir = Vec::new();
    let (salidas, _) = combinar(
        &OpcionesCombinar {
            archivos: &rutas,
            columnas: &columnas,
            hojas_excluir: &excluir,
            formato: Formato::Csv,
            columna_orden: Some("Sku"),
            ascendente: true,
            nombre_salida: "ordenado",
            ruta_salida: tmp.path(),
            division: Division::Ninguna,
            umbrales_orden: UmbralesOrden::default(),
            umbrales_lote_csv: UmbralesLoteCsv::default(),
        },
        |_| {},
        |_| {},
    )
    .expect("combinar con orden debe funcionar");

    let mut obtenidos = columna_de(&salidas[0]);
    obtenidos.sort();
    let mut esperados: Vec<String> = CODIGOS.iter().map(|c| (*c).to_string()).collect();
    esperados.sort();
    assert_eq!(obtenidos, esperados, "la mezcla externa deformó o perdió códigos");
}

#[test]
fn el_formato_de_intercambio_cierra_el_circulo_sin_deformar_nada() {
    // El caso que justifica toda la migración: una herramienta escribe IPC y
    // la siguiente lo lee. Se verifica sobre el ciclo COMPLETO de `combinar`
    // —no sobre el escritor aislado— porque es ahí donde el formato tiene que
    // ser transparente.
    let tmp = tempfile::tempdir().unwrap();
    let origen = tmp.path().join("origen.xlsx");
    let mut e = EscritorXlsx::nuevo(&origen, OpcionesEscritorXlsx::default()).unwrap();
    e.escribir(&df_codigos(), Some("Datos")).unwrap();
    e.cerrar().unwrap();

    let columnas = vec!["Sku".to_string(), "Nombre".to_string()];
    let excluir = Vec::new();
    let opciones = |archivos: &[std::path::PathBuf], formato, nombre: &'static str| {
        let (rutas, _) = combinar(
            &OpcionesCombinar {
                archivos,
                columnas: &columnas,
                hojas_excluir: &excluir,
                formato,
                columna_orden: None,
                ascendente: true,
                nombre_salida: nombre,
                ruta_salida: tmp.path(),
                division: Division::Ninguna,
                umbrales_orden: UmbralesOrden::default(),
                umbrales_lote_csv: UmbralesLoteCsv::default(),
            },
            |_| {},
            |_| {},
        )
        .expect("combinar debe funcionar");
        rutas
    };

    // Paso 1: XLSX → IPC.
    let intermedio = opciones(&[e.ruta.clone()], Formato::Ipc, "paso1");
    assert_eq!(intermedio[0].extension().unwrap(), "ipc");

    // Paso 2: ese IPC vuelve a entrar y sale como CSV, para poder inspeccionarlo.
    let final_csv = opciones(&intermedio, Formato::Csv, "paso2");

    assert_eq!(
        columna_de(&final_csv[0]),
        CODIGOS,
        "los códigos no sobrevivieron el viaje por el formato de intercambio"
    );
}

#[test]
fn dividir_en_hojas_tampoco_tiene_sentido_en_ipc() {
    // El guard se escribió para CSV; al sumar un tercer formato sin hojas
    // tenía que cubrirlo también, y no por mencionar CSV explícitamente.
    let tmp = tempfile::tempdir().unwrap();
    let origen = tmp.path().join("o.xlsx");
    let mut e = EscritorXlsx::nuevo(&origen, OpcionesEscritorXlsx::default()).unwrap();
    e.escribir(&df_codigos(), Some("Datos")).unwrap();
    e.cerrar().unwrap();

    let columnas = vec!["Sku".to_string(), "Nombre".to_string()];
    let excluir = Vec::new();
    let archivos = vec![e.ruta.clone()];
    let resultado = combinar(
        &OpcionesCombinar {
            archivos: &archivos,
            columnas: &columnas,
            hojas_excluir: &excluir,
            formato: Formato::Ipc,
            columna_orden: None,
            ascendente: true,
            nombre_salida: "ipc_por_hojas",
            ruta_salida: tmp.path(),
            division: Division::Hojas(2),
            umbrales_orden: UmbralesOrden::default(),
            umbrales_lote_csv: UmbralesLoteCsv::default(),
        },
        |_| {},
        |_| {},
    );
    assert!(resultado.is_err(), "debe rechazarse, no ignorarse");
}
