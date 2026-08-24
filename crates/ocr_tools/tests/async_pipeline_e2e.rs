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

use ocr_tools::async_processor::{AsyncBatchProcessor, Bloque, Motor, Persistencia};
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

// `flavor = "multi_thread"`: la inferencia OCR real usa
// `tokio::task::block_in_place` (ver el comentario en
// `async_processor::run_async`), que entra en pánico bajo el runtime
// `current_thread` que usa `#[tokio::test]` por defecto — el mismo
// requisito que ya cumple `#[tokio::main]` en `bin/ocr_tools.rs` (multi-hilo
// por defecto).
#[tokio::test(flavor = "multi_thread")]
async fn pipeline_async_completo_rechaza_el_placeholder_via_ocr_real() {
    let bytes = std::fs::read(manifest_dir().join("tests/fixtures/coming_soon.png")).unwrap();
    let url = servir_imagen(bytes);

    let df = df! { "Imagen 1" => [url.as_str()] }.unwrap();
    let url_columns = vec!["Imagen 1".to_string()];

    // Solo D4-D6 (OCR): el objetivo es probar el cableado async + motor OCR
    // real, no las heurísticas de color/bordes de D1-D2.
    let toggles = DetectorToggles {
        d1: false,
        d2: false,
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
            Bloque {
                df: &df,
                url_columns: &url_columns,
                idx_offset: 0,
            },
            Motor {
                pipeline,
                readers: Some(std::slice::from_mut(&mut reader)),
            },
            Persistencia {
                checkpoint,
                cached: &cached,
            },
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

/// Sirve una imagen DISTINTA según la ruta pedida, para que cada fila del
/// bloque tenga un veredicto propio y el orden sea observable.
fn servir_varias(imagenes: Vec<Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let puerto = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap_or(0);
            let pedido = String::from_utf8_lossy(&buf[..n]).to_string();
            // "GET /3.png HTTP/1.1" -> 3
            let indice = pedido
                .split_whitespace()
                .nth(1)
                .and_then(|r| r.trim_start_matches('/').split('.').next().map(str::to_string))
                .and_then(|d| d.parse::<usize>().ok())
                .unwrap_or(0);
            let bytes = &imagenes[indice % imagenes.len()];
            let cabecera = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            let _ = stream.write_all(cabecera.as_bytes());
            let _ = stream.write_all(bytes);
        }
    });
    format!("http://127.0.0.1:{puerto}")
}

/// Con varios motores OCR el resultado tiene que ser EL MISMO que con uno.
///
/// El pool reparte el lote en trozos, un hilo por motor, y después rearma la
/// salida. Si ese rearmado se desordenara, cada veredicto quedaría pegado a
/// la fila equivocada: no habría error ni panic, solo imágenes buenas
/// marcadas como rechazadas y viceversa. Es el fallo más caro posible acá,
/// porque es silencioso.
#[tokio::test(flavor = "multi_thread")]
async fn el_pool_de_motores_no_altera_el_orden_de_los_veredictos() {
    let fixtures: Vec<Vec<u8>> = [
        "coming_soon.png",
        "sale.png",
        "free_shipping.png",
        "hello_world.png",
    ]
    .iter()
    .map(|f| std::fs::read(manifest_dir().join("tests/fixtures").join(f)).unwrap())
    .collect();
    let base = servir_varias(fixtures);

    // 12 filas ciclando las 4 fixturas: mas filas que motores, para que el
    // reparto en trozos sea real y no un caso degenerado.
    let urls: Vec<String> = (0..12).map(|i| format!("{base}/{i}.png")).collect();
    let df = df! { "Imagen 1" => urls.iter().map(String::as_str).collect::<Vec<_>>() }.unwrap();
    let url_columns = vec!["Imagen 1".to_string()];
    let toggles = DetectorToggles {
        d1: false,
        d2: false,
        d4_d5_d6: true,
    };
    let pipeline = Arc::new(ImagePipeline::from_config(PipelineConfig::default(), toggles));

    let mut motivos_por_pool: Vec<Vec<String>> = Vec::new();
    for cantidad_motores in [1usize, 4] {
        let mut pool: Vec<Reader> = (0..cantidad_motores)
            .map(|_| {
                Reader::new(
                    manifest_dir().join("models/craft_detector.onnx"),
                    manifest_dir().join("models/recognizer_latin_g2.onnx"),
                )
                .expect("modelos ONNX")
            })
            .collect();

        let procesador =
            AsyncBatchProcessor::new(DownloadConfig::default(), 4, Duration::from_millis(100), 4, 100);
        let tmp = tempfile::tempdir().unwrap();
        let checkpoint = Arc::new(CheckpointStore::new(tmp.path().join("cp.jsonl")));
        let cached = HashMap::new();

        let outcome = procesador
            .process(
                Bloque {
                    df: &df,
                    url_columns: &url_columns,
                    idx_offset: 0,
                },
                Motor {
                    pipeline: pipeline.clone(),
                    readers: Some(&mut pool),
                },
                Persistencia {
                    checkpoint,
                    cached: &cached,
                },
                |_| {},
                |_| {},
            )
            .await
            .expect("process no debe fallar");

        let columna = outcome.df.column("_imagen_motivo").unwrap().str().unwrap();
        motivos_por_pool.push(
            (0..12)
                .map(|i| columna.get(i).unwrap_or("").to_string())
                .collect(),
        );
    }

    assert_eq!(
        motivos_por_pool[0], motivos_por_pool[1],
        "con 4 motores los veredictos quedaron en otro orden que con 1: cada fila recibiria el veredicto de otra imagen"
    );
}
