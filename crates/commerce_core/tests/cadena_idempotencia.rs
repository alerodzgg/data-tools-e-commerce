//! Lo que el camino de LECTURA transforma, y si esas transformaciones
//! sobreviven al encadenamiento.
//!
//! Leer no es neutro en este workspace: las cabeceras se deduplican, las
//! vacías se nombran `Columna_N` y las `__UNNAMED__N` de calamine se
//! canonizan. Cada una es correcta por separado; el riesgo es que al pasar un
//! archivo por varias herramientas se apliquen de nuevo y deformen los
//! nombres un poco más en cada vuelta.

use commerce_core::escritor_xlsx::OpcionesEscritorXlsx;
use commerce_core::EscritorXlsx;
use polars::prelude::*;

fn escribir(ruta: &std::path::Path, df: &DataFrame, filas_por_hoja: usize) -> std::path::PathBuf {
    let mut escritor = EscritorXlsx::nuevo(
        ruta,
        OpcionesEscritorXlsx {
            filas_por_hoja,
            ..Default::default()
        },
    )
    .unwrap();
    escritor.escribir(df, Some("Datos")).unwrap();
    escritor.cerrar().unwrap();
    escritor.ruta.clone()
}

fn releer(ruta: &std::path::Path) -> Vec<DataFrame> {
    commerce_core::iter_hojas_xlsx(ruta, None, |_: &str| {})
}

/// Escribe una hoja con las cabeceras EXACTAS que se le pasen, sin pasar por
/// un `DataFrame` (que no admite nombres repetidos, justo el caso a probar).
fn escribir_cabeceras(ruta: &std::path::Path, cabeceras: &[&str]) {
    use rust_xlsxwriter::Workbook;
    let mut wb = Workbook::new();
    let hoja = wb.add_worksheet();
    for (i, nombre) in cabeceras.iter().enumerate() {
        hoja.write(0, i as u16, *nombre).unwrap();
        hoja.write(1, i as u16, "v").unwrap();
    }
    wb.save(ruta).unwrap();
}

#[test]
fn las_cabeceras_dejan_de_cambiar_despues_de_la_primera_lectura() {
    // Repetidas, vacías y una que YA parece desambiguada (`Precio_1`): la
    // primera lectura las resuelve, y a partir de ahí el nombre tiene que
    // quedar fijo. Si no, encadenar herramientas le suma un sufijo por vuelta
    // hasta volver la cabecera ilegible.
    let tmp = tempfile::tempdir().unwrap();
    let r1 = tmp.path().join("v1.xlsx");
    escribir_cabeceras(&r1, &["Precio", "Precio", "", "Precio_1", "Sku"]);
    let nombres1: Vec<String> = releer(&r1)[0]
        .get_column_names_owned()
        .iter()
        .map(ToString::to_string)
        .collect();

    let r2 = tmp.path().join("v2.xlsx");
    let refs: Vec<&str> = nombres1.iter().map(String::as_str).collect();
    escribir_cabeceras(&r2, &refs);
    let nombres2: Vec<String> = releer(&r2)[0]
        .get_column_names_owned()
        .iter()
        .map(ToString::to_string)
        .collect();

    assert_eq!(
        nombres1, nombres2,
        "las cabeceras siguieron cambiando en la segunda pasada"
    );
    assert_eq!(nombres1.len(), 5, "no se perdió ninguna columna");
}

#[test]
fn un_archivo_partido_en_varias_hojas_conserva_todas_sus_filas() {
    // `filas_por_hoja` parte el libro en `Datos`, `Datos_2`, … Ese split lo
    // hace el escritor y lo deshace el lector: si alguno se equivoca, se
    // pierden filas en silencio al encadenar.
    let tmp = tempfile::tempdir().unwrap();
    let codigos: Vec<String> = (0..25).map(|i| format!("{i:04}")).collect();
    let df = df!("Sku" => codigos.clone()).unwrap();

    let ruta = escribir(&tmp.path().join("partido.xlsx"), &df, 10);
    let bloques = releer(&ruta);

    assert!(bloques.len() > 1, "el archivo debía partirse en varias hojas");
    let recuperados: Vec<String> = bloques
        .iter()
        .flat_map(|b| {
            b.column("Sku")
                .unwrap()
                .str()
                .unwrap()
                .iter()
                .map(|v| v.unwrap_or("").to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(recuperados, codigos, "el split perdió o deformó filas");
}
