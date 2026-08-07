//! Frontera entre `commerce_core` (escribe) y `etl_tools` (lee).
//!
//! Los dos crates se prueban por separado contra sus propios formatos, así que
//! el contrato ENTRE ellos no lo verifica nadie. Acá se escribe con los
//! escritores del workspace y se relee con el lector real de `etl_tools` —
//! que para CSV NO es el de `commerce_core`, sino `LazyCsvReader` de polars.
//!
//! El invariante que se sostiene es el central del producto: un código como
//! `007` no puede volverse `7` en ningún punto del camino.

use commerce_core::escritor_xlsx::OpcionesEscritorXlsx;
use commerce_core::{EscritorCsv, EscritorXlsx};
use polars::prelude::*;

/// Códigos que un lector descuidado reinterpretaría como números.
const CODIGOS: [&str; 6] = ["007", "0012", "1.50", "1e5", "0000", "+34"];

fn df_codigos() -> DataFrame {
    df!("Sku" => CODIGOS.to_vec(), "Nombre" => vec!["pieza"; CODIGOS.len()]).unwrap()
}

fn leidos(ruta: &std::path::Path) -> Vec<String> {
    let bloques = etl_tools::iter_hojas_valores(std::slice::from_ref(&ruta.to_path_buf()), None, |_| {})
        .expect("etl_tools debe poder leer lo que escribe el workspace");
    bloques[0]
        .column("Sku")
        .unwrap()
        .str()
        .unwrap()
        .iter()
        .map(|v| v.unwrap_or("").to_string())
        .collect()
}

#[test]
fn etl_tools_lee_sin_deformar_los_codigos_de_un_xlsx_del_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let mut escritor =
        EscritorXlsx::nuevo(tmp.path().join("codigos.xlsx"), OpcionesEscritorXlsx::default()).unwrap();
    escritor.escribir(&df_codigos(), Some("Datos")).unwrap();
    escritor.cerrar().unwrap();

    assert_eq!(leidos(&escritor.ruta), CODIGOS);
}

#[test]
fn etl_tools_lee_sin_deformar_los_codigos_de_un_csv_del_workspace() {
    // `etl_tools` lee CSV con `LazyCsvReader`, no con el lector de
    // `commerce_core`: es un motor distinto y necesita su propia verificación.
    //
    // El CSV NO round-trippea idéntico, y es a propósito: `EscritorCsv`
    // antepone `'` a lo que Excel interpretaría como fórmula (`= + - @`),
    // porque un `.csv` abierto en Excel ejecuta esas celdas. Es la única
    // diferencia admitida frente al XLSX, y se fija acá para que deje de ser
    // una sorpresa al encadenar herramientas.
    let tmp = tempfile::tempdir().unwrap();
    let columnas = vec!["Sku".to_string(), "Nombre".to_string()];
    let mut escritor = EscritorCsv::nuevo(tmp.path().join("codigos.csv"), columnas).unwrap();
    escritor.escribir(&df_codigos(), None).unwrap();
    escritor.cerrar().unwrap();

    let esperado: Vec<String> = CODIGOS
        .iter()
        .map(|c| {
            if c.starts_with(['=', '+', '-', '@']) {
                format!("'{c}")
            } else {
                (*c).to_string()
            }
        })
        .collect();
    assert_eq!(leidos(&escritor.ruta), esperado);
}

#[test]
fn pasar_dos_veces_por_csv_no_acumula_apostrofos() {
    // `EscritorCsv` antepone `'` a lo que Excel interpretaría como fórmula
    // (`= + - @`). La defensa es correcta al ESCRIBIR, pero al releer con
    // nuestro propio lector ese apóstrofo ya es dato: si cada pasada agrega
    // uno más, encadenar herramientas deforma el valor sin límite.
    let tmp = tempfile::tempdir().unwrap();
    let columnas = vec!["Sku".to_string(), "Nombre".to_string()];

    let mut primero = EscritorCsv::nuevo(tmp.path().join("p1.csv"), columnas.clone()).unwrap();
    primero.escribir(&df_codigos(), None).unwrap();
    primero.cerrar().unwrap();
    let tras_una = leidos(&primero.ruta);

    let bloques = etl_tools::iter_hojas_valores(std::slice::from_ref(&primero.ruta), None, |_| {}).unwrap();
    let mut segundo = EscritorCsv::nuevo(tmp.path().join("p2.csv"), columnas).unwrap();
    segundo.escribir(&bloques[0], None).unwrap();
    segundo.cerrar().unwrap();
    let tras_dos = leidos(&segundo.ruta);

    assert_eq!(
        tras_dos, tras_una,
        "una segunda pasada por CSV volvió a deformar los valores"
    );
}
