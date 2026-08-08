//! Medición de `normalizar`, que es el 84 % del pipeline de `combinar`.

use polars::prelude::*;

fn mediana(f: impl Fn() -> std::time::Duration) -> std::time::Duration {
    let mut t: Vec<_> = (0..=5).map(|_| f()).skip(1).collect();
    t.sort_unstable();
    t[t.len() / 2]
}

#[test]
#[ignore = "medición, no corrección"]
fn tiempo_de_normalizar() {
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

    let t = mediana(|| {
        let inicio = std::time::Instant::now();
        let r = data_combinator::normalizar(&df, &columnas).unwrap();
        let d = inicio.elapsed();
        assert_eq!(r.height(), FILAS);
        d
    });

    let celdas = FILAS * COLUMNAS;
    eprintln!(
        "\nNORMALIZAR (mediana de 5): {celdas} celdas en {t:.2?}  ({:.1} M celdas/s)\n",
        celdas as f64 / t.as_secs_f64() / 1e6
    );
}
