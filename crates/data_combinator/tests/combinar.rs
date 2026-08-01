//! Pruebas E2E de `combinar`.
//!
//! TRAMPA METODOLÓGICA (crítica, no la olvides al añadir pruebas aquí): para
//! ejercitar la MEZCLA EXTERNA hacen falta VARIOS ARCHIVOS de entrada. Un
//! archivo = un chunk = un run, y entonces la fusión k-vías nunca interleavea
//! y `ClaveExcel` no se ejerce de verdad. Un fuzzer con un solo archivo de
//! entrada daría 0 divergencias incluso con un bug real (falso negativo).

use std::path::{Path, PathBuf};

use data_combinator::{combinar, Division, Formato, OpcionesCombinar, UmbralesLoteCsv, UmbralesOrden};
use polars::prelude::*;
use rand::rngs::StdRng;
use rand::seq::IndexedRandom;
use rand::SeedableRng;

fn escribir_csv(ruta: &Path, df: &DataFrame) {
    let mut archivo = std::fs::File::create(ruta).unwrap();
    CsvWriter::new(&mut archivo).finish(&mut df.clone()).unwrap();
}

#[allow(clippy::too_many_arguments)]
fn combinar_una(
    archivos: &[PathBuf],
    columnas: &[String],
    hojas_excluir: &[String],
    formato: Formato,
    columna_orden: Option<&str>,
    ascendente: bool,
    nombre_salida: &str,
    ruta_salida: &Path,
    umbrales_orden: UmbralesOrden,
) -> (PathBuf, usize) {
    let opciones = OpcionesCombinar {
        archivos,
        columnas,
        hojas_excluir,
        formato,
        columna_orden,
        ascendente,
        nombre_salida,
        ruta_salida,
        division: Division::Ninguna,
        umbrales_orden,
        umbrales_lote_csv: UmbralesLoteCsv::default(),
    };
    let (rutas, n) = combinar(&opciones, |_| {}, |_| {}).unwrap();
    assert_eq!(rutas.len(), 1, "se esperaba 1 ruta, hay {}", rutas.len());
    (rutas[0].clone(), n)
}

fn leer_columna_csv(ruta: &Path, columna: &str) -> Vec<String> {
    let mut lector = csv::ReaderBuilder::new().from_path(ruta).unwrap();
    let cabecera = lector.headers().unwrap().clone();
    let idx = cabecera.iter().position(|c| c == columna).unwrap();
    lector
        .records()
        .map(|r| r.unwrap().get(idx).unwrap().to_string())
        .collect()
}

fn columnas(nombres: &[&str]) -> Vec<String> {
    nombres.iter().map(|s| s.to_string()).collect()
}

// ------------------------------------------------------ caso mínimo de orden
#[test]
fn orden_igual_en_memoria_y_en_disco() {
    let tmp = tempfile::tempdir().unwrap();
    escribir_csv(
        &tmp.path().join("a.csv"),
        &df!("Clave" => ["\x1f5\x1f"], "Id" => ["A"]).unwrap(),
    );
    escribir_csv(
        &tmp.path().join("b.csv"),
        &df!("Clave" => ["9"], "Id" => ["B"]).unwrap(),
    );
    let fuentes = vec![tmp.path().join("a.csv"), tmp.path().join("b.csv")];
    let cols = columnas(&["Clave", "Id"]);
    let excluir = Vec::new();

    let (ruta_mem, _) = combinar_una(
        &fuentes,
        &cols,
        &excluir,
        Formato::Csv,
        Some("Clave"),
        true,
        "o_memoria",
        tmp.path(),
        UmbralesOrden::forzar_memoria(),
    );
    let (ruta_disco, _) = combinar_una(
        &fuentes,
        &cols,
        &excluir,
        Formato::Csv,
        Some("Clave"),
        true,
        "o_disco",
        tmp.path(),
        UmbralesOrden::forzar_disco(),
    );

    assert_eq!(
        leer_columna_csv(&ruta_mem, "Id"),
        leer_columna_csv(&ruta_disco, "Id")
    );
}

#[test]
fn desempate_numerico_por_texto() {
    let tmp = tempfile::tempdir().unwrap();
    escribir_csv(
        &tmp.path().join("a.csv"),
        &df!("Clave" => ["2.0"], "Id" => ["A"]).unwrap(),
    );
    escribir_csv(
        &tmp.path().join("b.csv"),
        &df!("Clave" => ["2"], "Id" => ["B"]).unwrap(),
    );
    let fuentes = vec![tmp.path().join("a.csv"), tmp.path().join("b.csv")];
    let cols = columnas(&["Clave", "Id"]);
    let excluir = Vec::new();

    let (ruta_mem, _) = combinar_una(
        &fuentes,
        &cols,
        &excluir,
        Formato::Csv,
        Some("Clave"),
        true,
        "d_memoria",
        tmp.path(),
        UmbralesOrden::forzar_memoria(),
    );
    let (ruta_disco, _) = combinar_una(
        &fuentes,
        &cols,
        &excluir,
        Formato::Csv,
        Some("Clave"),
        true,
        "d_disco",
        tmp.path(),
        UmbralesOrden::forzar_disco(),
    );

    assert_eq!(
        leer_columna_csv(&ruta_mem, "Id"),
        leer_columna_csv(&ruta_disco, "Id")
    );
}

// ------------------------------------------------------------------ FUZZER
const VALORES: &[&str] = &[
    "10",
    "2",
    "100",
    "9",
    "1.5",
    "1,000",
    "1_000",
    "0x10",
    "1e5",
    "nan",
    "inf",
    "-inf",
    "NULL",
    "n/a",
    "",
    "  ",
    "manzana",
    "Manzana",
    "ZEBRA",
    "arbol",
    "5.",
    " 7 ",
    "\x1f5\x1f",
    "\x1e8",
    "\x1c3\x1c",
    "\u{a0}4\u{a0}",
    "000123",
    "1.50",
    "+4",
    "-3",
    "2.0",
    "abc123",
    "123abc",
    "\tzeta\t",
    "MANGO",
    "mango ",
    " mango",
    " 5 ",
];

#[test]
fn fuzzer_memoria_vs_mezcla_externa() {
    // El MISMO dato debe salir en el MISMO orden quepa en RAM o desborde a
    // disco. UN ARCHIVO POR FILA → un run por archivo → la fusión k-vías se
    // ejerce de verdad. Esta prueba está diseñada para cazar bugs de orden
    // en la fusión k-vías.
    for semilla in 0u64..15 {
        for ascendente in [true, false] {
            let tmp = tempfile::tempdir().unwrap();
            let mut rng = StdRng::seed_from_u64(semilla);
            let mut fuentes = Vec::new();
            for i in 0..20 {
                let v = *VALORES.choose(&mut rng).unwrap();
                let ruta = tmp.path().join(format!("f{i}.csv"));
                escribir_csv(&ruta, &df!("Clave" => [v], "Id" => [i.to_string()]).unwrap());
                fuentes.push(ruta);
            }
            let cols = columnas(&["Clave", "Id"]);
            let excluir = Vec::new();

            let (r1, _) = combinar_una(
                &fuentes,
                &cols,
                &excluir,
                Formato::Csv,
                Some("Clave"),
                ascendente,
                "mem",
                tmp.path(),
                UmbralesOrden::forzar_memoria(),
            );
            let (r2, _) = combinar_una(
                &fuentes,
                &cols,
                &excluir,
                Formato::Csv,
                Some("Clave"),
                ascendente,
                "dis",
                tmp.path(),
                UmbralesOrden::forzar_disco(),
            );

            let en_memoria = leer_columna_csv(&r1, "Id");
            let en_disco = leer_columna_csv(&r2, "Id");
            assert_eq!(en_memoria, en_disco, "semilla={semilla} ascendente={ascendente}");
        }
    }
}

// --------------------------------------------------------- memoria acotada
#[test]
fn presupuesto_trocea_el_chunk() {
    // El presupuesto de run debe PARTIR el chunk, no solo evaluarse tras
    // acumularlo entero: presupuesto de 1 fila + chunk de 5000 → runs de
    // 1000 filas cada uno (no un único run de 5000).
    let tmp = tempfile::tempdir().unwrap();
    let origen = tmp.path().join("grande.csv");
    let claves: Vec<String> = (0..5000).map(|i: i32| (i % 97).to_string()).collect();
    let ids: Vec<String> = (0..5000).map(|i: i32| i.to_string()).collect();
    escribir_csv(&origen, &df!("Clave" => claves.clone(), "Id" => ids).unwrap());

    let cols = columnas(&["Clave", "Id"]);
    let excluir = Vec::new();
    let umbrales = UmbralesOrden {
        celdas_por_run: 2000,
        filas_min_run: 1000,
    };
    let (ruta, _) = combinar_una(
        &[origen],
        &cols,
        &excluir,
        Formato::Csv,
        Some("Clave"),
        true,
        "runs",
        tmp.path(),
        umbrales,
    );

    let salida = leer_columna_csv(&ruta, "Clave");
    let mut esperado = claves;
    esperado.sort_by(|a, b| {
        a.parse::<f64>()
            .unwrap()
            .partial_cmp(&b.parse::<f64>().unwrap())
            .unwrap()
    });
    assert_eq!(salida, esperado);
}

// ------------------------------------------------------------------- E2E
#[test]
fn combina_sin_ordenar_rellenando_faltantes() {
    let tmp = tempfile::tempdir().unwrap();
    escribir_csv(
        &tmp.path().join("e1.csv"),
        &df!("A" => ["1", "2"], "B" => ["x", "y"]).unwrap(),
    );
    escribir_csv(
        &tmp.path().join("e2.csv"),
        &df!("A" => ["3"], "C" => ["z"]).unwrap(),
    );
    let fuentes = vec![tmp.path().join("e1.csv"), tmp.path().join("e2.csv")];

    let columnas_union = commerce_core::columnas_union(&fuentes, None, |_| {});
    assert_eq!(
        columnas_union,
        vec!["A".to_string(), "B".to_string(), "C".to_string()]
    );

    let (ruta, n) = combinar_una(
        &fuentes,
        &columnas_union,
        &[],
        Formato::Csv,
        None,
        true,
        "e2e",
        tmp.path(),
        UmbralesOrden::default(),
    );
    assert_eq!(n, 3);
    assert_eq!(leer_columna_csv(&ruta, "C"), vec!["", "", "z"]);
}

#[test]
fn xlsx_vacios_al_final_y_orden_numerico() {
    for ascendente in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let origen = tmp.path().join("s.csv");
        escribir_csv(
            &origen,
            &df!("K" => ["b", "", "a", "10", "2"], "Id" => ["1", "2", "3", "4", "5"]).unwrap(),
        );
        let cols = columnas(&["K", "Id"]);
        let (ruta, _) = combinar_una(
            &[origen],
            &cols,
            &[],
            Formato::Xlsx,
            Some("K"),
            ascendente,
            "x",
            tmp.path(),
            UmbralesOrden::default(),
        );

        let hojas = commerce_core::iter_hojas_xlsx(&ruta, None, |_| {});
        let hoja = &hojas[0];
        let claves: Vec<Option<String>> = hoja
            .column("K")
            .unwrap()
            .str()
            .unwrap()
            .iter()
            .map(|v| v.map(str::to_string))
            .collect();

        let ultimo = claves.last().unwrap();
        assert!(
            ultimo.is_none() || ultimo.as_deref() == Some(""),
            "los vacíos deben quedar al final: {claves:?}"
        );
        let numeros: Vec<&str> = claves
            .iter()
            .filter_map(|k| k.as_deref())
            .filter(|k| *k == "2" || *k == "10")
            .collect();
        let esperado: Vec<&str> = if ascendente {
            vec!["2", "10"]
        } else {
            vec!["10", "2"]
        };
        assert_eq!(numeros, esperado);
    }
}

#[test]
fn error_en_columna_de_orden_no_deja_archivo_de_salida_a_medias() {
    let tmp = tempfile::tempdir().unwrap();
    escribir_csv(
        &tmp.path().join("e1.csv"),
        &df!("A" => ["1", "2"], "B" => ["x", "y"]).unwrap(),
    );
    let fuentes = vec![tmp.path().join("e1.csv")];
    let cols = columnas(&["A", "B"]);

    let opciones = OpcionesCombinar {
        archivos: &fuentes,
        columnas: &cols,
        hojas_excluir: &[],
        formato: Formato::Csv,
        columna_orden: Some("NO_EXISTE"),
        ascendente: true,
        nombre_salida: "err",
        ruta_salida: tmp.path(),
        division: Division::Ninguna,
        umbrales_orden: UmbralesOrden::default(),
        umbrales_lote_csv: UmbralesLoteCsv::default(),
    };

    let resultado = combinar(&opciones, |_| {}, |_| {});
    assert!(resultado.is_err(), "columna de orden inexistente debe fallar");

    let quedaron: Vec<String> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|r| r.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "e1.csv")
        .collect();
    assert!(
        quedaron.is_empty(),
        "no debe quedar ningún archivo de salida a medias: {quedaron:?}"
    );
}

#[test]
fn nombre_salida_con_separador_de_ruta_se_rechaza_sin_escribir_fuera_de_la_carpeta() {
    let tmp = tempfile::tempdir().unwrap();
    escribir_csv(
        &tmp.path().join("e1.csv"),
        &df!("A" => ["1"], "B" => ["x"]).unwrap(),
    );
    let fuentes = vec![tmp.path().join("e1.csv")];
    let cols = columnas(&["A", "B"]);
    let carpeta_salida = tmp.path().join("salida");
    std::fs::create_dir(&carpeta_salida).unwrap();

    let opciones = OpcionesCombinar {
        archivos: &fuentes,
        columnas: &cols,
        hojas_excluir: &[],
        formato: Formato::Csv,
        columna_orden: None,
        ascendente: true,
        nombre_salida: "..\\escape",
        ruta_salida: &carpeta_salida,
        division: Division::Ninguna,
        umbrales_orden: UmbralesOrden::default(),
        umbrales_lote_csv: UmbralesLoteCsv::default(),
    };

    assert!(
        combinar(&opciones, |_| {}, |_| {}).is_err(),
        "un nombre de salida con separador de ruta debe rechazarse"
    );
    assert!(
        !tmp.path().join("escape.csv").exists(),
        "no debe escapar de la carpeta de salida elegida"
    );
}

#[test]
fn division_por_archivos_reparte_en_varios_archivos_de_n_filas() {
    let tmp = tempfile::tempdir().unwrap();
    let valores: Vec<String> = (0..7).map(|i| i.to_string()).collect();
    escribir_csv(&tmp.path().join("e1.csv"), &df!("A" => valores).unwrap());
    let fuentes = vec![tmp.path().join("e1.csv")];
    let cols = columnas(&["A"]);

    let opciones = OpcionesCombinar {
        archivos: &fuentes,
        columnas: &cols,
        hojas_excluir: &[],
        formato: Formato::Csv,
        columna_orden: None,
        ascendente: true,
        nombre_salida: "particionado",
        ruta_salida: tmp.path(),
        division: Division::Archivos(3),
        umbrales_orden: UmbralesOrden::default(),
        umbrales_lote_csv: UmbralesLoteCsv::default(),
    };

    let (rutas, total) = combinar(&opciones, |_| {}, |_| {}).unwrap();
    assert_eq!(total, 7);
    assert_eq!(rutas.len(), 3, "7 filas / 3 por archivo -> 3 archivos: 3,3,1");

    let nombres: Vec<String> = rutas
        .iter()
        .map(|r| r.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        nombres,
        vec![
            "particionado_1.csv".to_string(),
            "particionado_2.csv".to_string(),
            "particionado_3.csv".to_string(),
        ]
    );

    let mut leidos: Vec<String> = Vec::new();
    for ruta in &rutas {
        leidos.extend(leer_columna_csv(ruta, "A"));
    }
    leidos.sort();
    let mut esperado: Vec<String> = (0..7).map(|i| i.to_string()).collect();
    esperado.sort();
    assert_eq!(leidos, esperado);
}

#[test]
fn division_por_archivos_sin_datos_deja_un_solo_archivo_con_cabecera() {
    let tmp = tempfile::tempdir().unwrap();
    let fuentes: Vec<PathBuf> = Vec::new();
    let cols = columnas(&["A", "B"]);

    let opciones = OpcionesCombinar {
        archivos: &fuentes,
        columnas: &cols,
        hojas_excluir: &[],
        formato: Formato::Csv,
        columna_orden: None,
        ascendente: true,
        nombre_salida: "vacio",
        ruta_salida: tmp.path(),
        division: Division::Archivos(100),
        umbrales_orden: UmbralesOrden::default(),
        umbrales_lote_csv: UmbralesLoteCsv::default(),
    };

    let (rutas, total) = combinar(&opciones, |_| {}, |_| {}).unwrap();
    assert_eq!(total, 0);
    assert_eq!(
        rutas.len(),
        1,
        "sin datos, igual debe dejar un archivo válido con cabecera"
    );
    let leido = std::fs::read_to_string(&rutas[0]).unwrap();
    assert_eq!(leido.trim(), "A,B");
}
