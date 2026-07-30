//! El pipeline (`ort` + modelos ONNX) debe reconocer exactamente el texto
//! esperado en `tests/fixture_ocr.json` para cada imagen de
//! `tests/fixtures/`. El fixture se generó una vez con los pesos SIN
//! cuantizar (los mismos que se exportaron a ONNX), para que la comparación
//! aísle la fidelidad del reconocedor del ruido que metería comparar contra
//! un modelo cuantizado.

use std::collections::HashMap;
use std::path::PathBuf;

use ocr_tools::reader::Reader;
use serde::Deserialize;

#[derive(Deserialize)]
struct FixtureResultado {
    text: String,
    #[allow(dead_code)]
    confidence: f32,
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn readtext_reconoce_el_texto_esperado_del_fixture() {
    let fixture_path = manifest_dir().join("tests").join("fixture_ocr.json");
    let fixture_json = std::fs::read_to_string(&fixture_path).expect("no se encontró fixture_ocr.json");
    let fixture: HashMap<String, Vec<FixtureResultado>> =
        serde_json::from_str(&fixture_json).expect("fixture_ocr.json invalido");

    let mut reader = Reader::new(
        manifest_dir().join("models").join("craft_detector.onnx"),
        manifest_dir().join("models").join("recognizer_latin_g2.onnx"),
    )
    .expect("no se pudieron cargar los modelos ONNX");

    let mut nombres: Vec<&String> = fixture.keys().collect();
    nombres.sort();

    for nombre in nombres {
        let esperados = &fixture[nombre];
        let ruta_imagen = manifest_dir().join("tests").join("fixtures").join(nombre);
        let rgb = image::open(&ruta_imagen)
            .unwrap_or_else(|e| panic!("no se pudo abrir {nombre}: {e}"))
            .to_rgb8();

        let resultados = reader
            .readtext(&rgb)
            .unwrap_or_else(|e| panic!("readtext fallo en {nombre}: {e}"));

        let textos_obtenidos: Vec<&str> = resultados.iter().map(|r| r.text.as_str()).collect();
        let textos_esperados: Vec<&str> = esperados.iter().map(|r| r.text.as_str()).collect();

        assert_eq!(
            textos_obtenidos, textos_esperados,
            "texto reconocido no coincide para {nombre}"
        );
    }
}
