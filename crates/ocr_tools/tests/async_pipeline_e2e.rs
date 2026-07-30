//! Extremo a extremo: servidor HTTP local sirviendo una imagen fixture real
//! → `AsyncBatchProcessor` (descarga + detectores CPU + `OcrBatcher` + motor
//! OCR real) → veredicto D6 esperado. Verifica el cableado completo entre
//! `downloader`, `pipeline` y `async_processor`, no solo cada pieza aislada.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ocr_tools::async_processor::AsyncBatchProcessor;
use ocr_tools::checkpoint_store::CheckpointStore;
use ocr_tools::downloader::DownloadConfig;
use ocr_tools::pipeline::{DetectorToggles, ImagePipeline, PipelineConfig};
use ocr_tools::reader::Reader;
use polars::prelude::*;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Levanta un servidor HTTP mínimo que sirve `bytes` como `image/png` en
/// cada petición, en un thread aparte.
fn servir_imagen(bytes: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let puerto = listener.local_addr().unwrap().port();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let cabecera = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            let _ = stream.write_all(cabecera.as_bytes());
            let _ = stream.write_all(&bytes);
        }
    });
    format!("http://127.0.0.1:{puerto}/coming_soon.jpg")
}

#[tokio::test]
async fn pipeline_async_completo_rechaza_el_placeholder_via_ocr_real() {
    let bytes = std::fs::read(manifest_dir().join("tests/fixtures/coming_soon.png")).unwrap();
    let url = servir_imagen(bytes);

    let df = df! { "Imagen 1" => [url.as_str()] }.unwrap();
    let url_columns = vec!["Imagen 1".to_string()];

    // Solo D4-D6 (OCR): el objetivo es probar el cableado async + motor OCR
    // real, no las heurísticas de color/bordes de D1-D3.
    let toggles = DetectorToggles {
        d1: false,
        d2: false,
        d3: false,
        d4_d5_d6: true,
    };
    let pipeline = Arc::new(ImagePipeline::from_config(PipelineConfig::default(), toggles));

    let mut reader = Reader::new(
        manifest_dir().join("models/craft_detector.onnx"),
        manifest_dir().join("models/recognizer_latin_g2.onnx"),
    )
    .expect("no se pudieron cargar los modelos ONNX");

    let procesador =
        AsyncBatchProcessor::new(DownloadConfig::default(), 1, Duration::from_millis(100), 4, 100);

    let tmp = tempfile::tempdir().unwrap();
    let checkpoint = Arc::new(CheckpointStore::new(tmp.path().join("cp.jsonl")));
    let cached = HashMap::new();

    let outcome = procesador
        .process(
            &df,
            &url_columns,
            pipeline,
            Some(&mut reader),
            checkpoint,
            &cached,
            0,
            |_| {},
            |_| {},
        )
        .await
        .expect("process no debe fallar");

    assert_eq!(outcome.imagenes_analizadas, 1);
    assert_eq!(
        outcome.imagenes_rechazadas, 1,
        "coming_soon.png debe rechazarse via D6"
    );
    let motivo = outcome
        .df
        .column("_imagen_motivo")
        .unwrap()
        .str()
        .unwrap()
        .get(0)
        .unwrap()
        .to_string();
    assert!(motivo.contains("D6"), "motivo inesperado: {motivo}");
}
