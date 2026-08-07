//! Medición de la operación REAL de `combinar`, no de las primitivas.
//!
//! `commerce_core` mide escribir y leer un `DataFrame` aislado. El pipeline
//! completo agrega normalización de columnas, alineación y troceado, y esa
//! diferencia ya mordió antes en esta base: un lector que daba 2,6x aislado
//! rindió 1,57x integrado. Acá se mide lo que el usuario espera de verdad.

use commerce_core::escritor_xlsx::OpcionesEscritorXlsx;
use commerce_core::EscritorXlsx;
use data_combinator::{combinar, Division, Formato, OpcionesCombinar, UmbralesLoteCsv, UmbralesOrden};
use polars::prelude::*;

/// Mediana de 5 corridas, descartando la primera (calienta cachés).
fn mediana(f: impl Fn() -> std::time::Duration) -> std::time::Duration {
    let mut t: Vec<_> = (0..=5).map(|_| f()).skip(1).collect();
    t.sort_unstable();
    t[t.len() / 2]
}

#[test]
#[ignore = "medición, no corrección"]
fn combinar_completo_xlsx_contra_columnar() {
    const FILAS: usize = 300_000;
    const COLUMNAS: usize = 12;

    let cols: Vec<Column> = (0..COLUMNAS)
        .map(|c| {
            let v: Vec<String> = (0..FILAS).map(|f| format!("v{c}_{f}")).collect();
            Column::new(format!("Col{c}").into(), v)
        })
        .collect();
    let df = DataFrame::new_infer_height(cols).unwrap();
    let columnas: Vec<String> = (0..COLUMNAS).map(|c| format!("Col{c}")).collect();
    let excluir: Vec<String> = Vec::new();
    let tmp = tempfile::tempdir().unwrap();

    // Fuentes equivalentes en los dos formatos.
    let mut e = EscritorXlsx::nuevo(tmp.path().join("f.xlsx"), OpcionesEscritorXlsx::default()).unwrap();
    e.escribir(&df, Some("Datos")).unwrap();
    e.cerrar().unwrap();
    let fuente_xlsx = vec![e.ruta.clone()];

    let mut e = commerce_core::EscritorIpc::nuevo(tmp.path().join("f.ipc")).unwrap();
    e.escribir(&df, None).unwrap();
    e.cerrar().unwrap();
    let fuente_ipc = vec![e.ruta.clone()];

    let correr = |archivos: &[std::path::PathBuf], formato, nombre: &'static str| {
        let inicio = std::time::Instant::now();
        combinar(
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
        inicio.elapsed()
    };

    let texto = mediana(|| correr(&fuente_xlsx, Formato::Xlsx, "todo_xlsx"));
    let columnar = mediana(|| correr(&fuente_ipc, Formato::Ipc, "todo_ipc"));

    let celdas = FILAS * COLUMNAS;
    eprintln!(
        "\n=== PIPELINE `combinar` COMPLETO ({FILAS}x{COLUMNAS} = {celdas} celdas, mediana de 5) ===\n\
         XLSX → XLSX   {texto:.2?}  ({:.2} M celdas/s)\n\
         IPC  → IPC    {columnar:.2?}  ({:.2} M celdas/s)\n\
         GANANCIA REAL DEL PIPELINE: {:.1}x\n",
        celdas as f64 / texto.as_secs_f64() / 1e6,
        celdas as f64 / columnar.as_secs_f64() / 1e6,
        texto.as_secs_f64() / columnar.as_secs_f64(),
    );
}

#[test]
#[ignore = "medición, no corrección"]
fn reparto_del_tiempo_dentro_del_pipeline_columnar() {
    // El pipeline columnar tarda 1,05 s donde el I/O puro cuesta una fracción.
    // Esto separa las tres etapas para saber a qué atacar: si el intermedio
    // domina, optimizar más el formato no puede dar nada.
    const FILAS: usize = 300_000;
    const COLUMNAS: usize = 12;

    let cols: Vec<Column> = (0..COLUMNAS)
        .map(|c| {
            let v: Vec<String> = (0..FILAS).map(|f| format!("v{c}_{f}")).collect();
            Column::new(format!("Col{c}").into(), v)
        })
        .collect();
    let df = DataFrame::new_infer_height(cols).unwrap();
    let columnas: Vec<String> = (0..COLUMNAS).map(|c| format!("Col{c}")).collect();
    let excluir: Vec<String> = Vec::new();
    let tmp = tempfile::tempdir().unwrap();

    let mut e = commerce_core::EscritorIpc::nuevo(tmp.path().join("src.ipc")).unwrap();
    e.escribir(&df, None).unwrap();
    e.cerrar().unwrap();
    let fuente = vec![e.ruta.clone()];

    // (a) Solo leer.
    let leer = mediana(|| {
        let inicio = std::time::Instant::now();
        let b = commerce_core::leer_ipc(&fuente[0]).unwrap();
        let t = inicio.elapsed();
        assert_eq!(b[0].height(), FILAS);
        t
    });

    // (b) Solo escribir.
    let escribir = mediana(|| {
        let inicio = std::time::Instant::now();
        let mut e = commerce_core::EscritorIpc::nuevo(tmp.path().join("dst.ipc")).unwrap();
        e.escribir(&df, None).unwrap();
        e.cerrar().unwrap();
        inicio.elapsed()
    });

    // (c) El pipeline entero.
    let completo = mediana(|| {
        let inicio = std::time::Instant::now();
        combinar(
            &OpcionesCombinar {
                archivos: &fuente,
                columnas: &columnas,
                hojas_excluir: &excluir,
                formato: Formato::Ipc,
                columna_orden: None,
                ascendente: true,
                nombre_salida: "pipe",
                ruta_salida: tmp.path(),
                division: Division::Ninguna,
                umbrales_orden: UmbralesOrden::default(),
                umbrales_lote_csv: UmbralesLoteCsv::default(),
            },
            |_| {},
            |_| {},
        )
        .unwrap();
        inicio.elapsed()
    });

    let intermedio = completo.saturating_sub(leer + escribir);
    eprintln!(
        "\n=== REPARTO DEL PIPELINE COLUMNAR ({FILAS}x{COLUMNAS}, mediana de 5) ===\n\
         leer_ipc          {leer:.2?}   ({:.0}%)\n\
         escribir_ipc      {escribir:.2?}   ({:.0}%)\n\
         TRABAJO INTERMEDIO {intermedio:.2?}   ({:.0}%)  ← normalizar + alinear + trocear\n\
         total             {completo:.2?}\n\
         Techo si el I/O fuera GRATIS: {:.1}x sobre el pipeline actual\n",
        leer.as_secs_f64() / completo.as_secs_f64() * 100.0,
        escribir.as_secs_f64() / completo.as_secs_f64() * 100.0,
        intermedio.as_secs_f64() / completo.as_secs_f64() * 100.0,
        completo.as_secs_f64() / intermedio.as_secs_f64(),
    );
}

#[test]
#[ignore = "medición, no corrección"]
fn dentro_de_normalizar_quien_manda() {
    // `normalizar` hace dos cosas: reconstruir cada columna como texto
    // (asignando un `String` por celda) y después reparar mojibake sobre todo
    // el DataFrame. Antes de optimizar cualquiera de las dos hay que saber
    // cuál pesa.
    const FILAS: usize = 300_000;
    const COLUMNAS: usize = 12;

    let cols: Vec<Column> = (0..COLUMNAS)
        .map(|c| {
            let v: Vec<String> = (0..FILAS).map(|f| format!("v{c}_{f}")).collect();
            Column::new(format!("Col{c}").into(), v)
        })
        .collect();
    let df = DataFrame::new_infer_height(cols).unwrap();
    let columnas: Vec<String> = (0..COLUMNAS).map(|c| format!("Col{c}")).collect();

    let completo = mediana(|| {
        let inicio = std::time::Instant::now();
        let r = data_combinator::normalizar(&df, &columnas).unwrap();
        let t = inicio.elapsed();
        assert_eq!(r.height(), FILAS);
        t
    });

    let solo_mojibake = mediana(|| {
        let entrada = df.clone();
        let inicio = std::time::Instant::now();
        let r = commerce_core::limpiar_mojibake(entrada, None).unwrap();
        let t = inicio.elapsed();
        assert_eq!(r.height(), FILAS);
        t
    });

    let reconstruccion = completo.saturating_sub(solo_mojibake);
    eprintln!(
        "\n=== DENTRO DE `normalizar` ({FILAS}x{COLUMNAS}, mediana de 5) ===\n\
         reconstruir columnas  {reconstruccion:.2?}  ({:.0}%)  ← un String por celda\n\
         limpiar_mojibake      {solo_mojibake:.2?}  ({:.0}%)\n\
         total normalizar      {completo:.2?}\n",
        reconstruccion.as_secs_f64() / completo.as_secs_f64() * 100.0,
        solo_mojibake.as_secs_f64() / completo.as_secs_f64() * 100.0,
    );
}
