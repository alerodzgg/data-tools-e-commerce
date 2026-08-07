//! El camino rápido y el de `calamine` tienen que dar EXACTAMENTE lo mismo.
//!
//! Tener dos lectores solo es admisible si son indistinguibles desde afuera.
//! Estos tests comparan ambos sobre los casos donde es más fácil que
//! diverjan: celdas vacías, cabeceras repetidas, texto con metacaracteres XML
//! y códigos que un lector descuidado convertiría en números.

use commerce_core::escritor_xlsx::OpcionesEscritorXlsx;
use commerce_core::EscritorXlsx;
use polars::prelude::*;

/// Lee con el camino público (que usa el rápido si reconoce el archivo).
fn leer(ruta: &std::path::Path) -> Vec<DataFrame> {
    commerce_core::iter_hojas_xlsx(ruta, None, |_: &str| {})
}

/// Lee saltando el camino rápido: `calamine` no reconoce un `.xlsx` cuyo
/// `xl/workbook.xml` se le esconde, así que se compara contra una copia leída
/// por la ruta de siempre a través del lector de hojas por nombre.
fn leer_con_calamine(ruta: &std::path::Path) -> Vec<DataFrame> {
    let mut libro = commerce_core::abrir_libro(ruta).expect("calamine debe abrirlo");
    commerce_core::nombres_hojas_libro(&libro)
        .into_iter()
        .map(|hoja| commerce_core::leer_hoja_por_nombre(&mut libro, ruta, &hoja).unwrap())
        .collect()
}

fn escribir(ruta: &std::path::Path, df: &DataFrame) -> std::path::PathBuf {
    let mut e = EscritorXlsx::nuevo(ruta, OpcionesEscritorXlsx::default()).unwrap();
    e.escribir(df, Some("Datos")).unwrap();
    e.cerrar().unwrap();
    e.ruta.clone()
}

fn comparar(nombre: &str, df: &DataFrame) {
    let tmp = tempfile::tempdir().unwrap();
    let ruta = escribir(&tmp.path().join(format!("{nombre}.xlsx")), df);

    let rapido = leer(&ruta);
    let lento = leer_con_calamine(&ruta);

    assert_eq!(rapido.len(), lento.len(), "[{nombre}] distinta cantidad de hojas");
    for (r, l) in rapido.iter().zip(&lento) {
        assert_eq!(
            r.get_column_names_owned(),
            l.get_column_names_owned(),
            "[{nombre}] distintas columnas"
        );
        assert_eq!(r, l, "[{nombre}] distinto contenido");
    }
}

#[test]
fn ambos_lectores_coinciden_en_codigos_que_parecen_numeros() {
    comparar(
        "codigos",
        &df!("Sku" => ["007", "0012", "1.50", "1e5", "0000"]).unwrap(),
    );
}

#[test]
fn ambos_lectores_coinciden_con_celdas_vacias_intercaladas() {
    comparar(
        "vacias",
        &df!(
            "A" => [Some("x"), None, Some("y"), None],
            "B" => [None, Some("1"), None, Some("2")],
        )
        .unwrap(),
    );
}

#[test]
fn ambos_lectores_coinciden_con_metacaracteres_xml() {
    comparar(
        "xml",
        &df!("Texto" => ["a&b", "<tag>", "\"comillas\"", "  espacios  ", "ñ中🚗"]).unwrap(),
    );
}

#[test]
fn ambos_lectores_coinciden_con_una_columna_entera_vacia() {
    comparar(
        "columna_vacia",
        &df!("Llena" => ["a", "b"], "Vacia" => [None::<&str>, None]).unwrap(),
    );
}

#[test]
fn con_una_hoja_grande_el_camino_paralelo_da_lo_mismo_que_el_secuencial() {
    // Los demás tests usan archivos por debajo del umbral de paralelismo, así
    // que nunca tocan el reparto entre hilos. Este lo cruza a propósito: si
    // un corte partiera una fila, o si los fragmentos se concatenaran fuera
    // de orden, acá se ve.
    let filas = 60_000;
    let sku: Vec<String> = (0..filas).map(|i| format!("{i:07}")).collect();
    let texto: Vec<String> = (0..filas).map(|i| format!("pieza & <{i}>")).collect();
    let df = df!("Sku" => sku.clone(), "Nombre" => texto).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let ruta = escribir(&tmp.path().join("grande.xlsx"), &df);
    let bytes = std::fs::metadata(&ruta).unwrap().len();
    assert!(bytes > 0);

    let rapido = leer(&ruta);
    let lento = leer_con_calamine(&ruta);
    assert_eq!(rapido.len(), 1);
    assert_eq!(rapido[0].height(), filas, "se perdieron filas al repartir");
    assert_eq!(rapido[0], lento[0], "el camino paralelo divergió del secuencial");

    // El orden importa: concatenar los fragmentos mal daría las mismas filas
    // en otra secuencia y la comparación de arriba lo detectaría, pero se
    // afirma explícito para que el motivo quede claro al leer el test.
    let leidos: Vec<String> = rapido[0]
        .column("Sku")
        .unwrap()
        .str()
        .unwrap()
        .iter()
        .map(|v| v.unwrap_or("").to_string())
        .collect();
    assert_eq!(leidos, sku, "las filas salieron desordenadas");
}
