//! DIAGNOSTICO TEMPORAL - una sola medicion, un solo metodo.
//!
//! Corrige el error de mezclar mediciones con y sin paralelismo: TODO se
//! mide con el mismo pool de motores que usa produccion, para que los
//! numeros sean comparables entre si y extrapolables a una instancia.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use futures::stream::{self, StreamExt};
use ocr_tools::detectors::ImageContext;
use ocr_tools::downloader::{decode_and_resize, AsyncImageDownloader, DownloadConfig};
use ocr_tools::pipeline::{DetectorToggles, ImagePipeline, PipelineConfig};
use ocr_tools::reader::Reader;

const ARCHIVO: &str = r"C:\Users\rodri\Downloads\ebay.xlsx";
const MOTORES: usize = 8;

fn modelos() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("models")
}

/// Corre el pipeline sobre todas las imagenes repartiendo el trabajo entre
/// `MOTORES` hilos, igual que el batcher de produccion.
fn medir(
    etiqueta: &str,
    ctxs: &[ImageContext],
    toggles: DetectorToggles,
    n_base: f64,
) -> (f64, Vec<String>) {
    let pipeline = ImagePipeline::from_config(PipelineConfig::default(), toggles);
    let mut pool: Vec<Reader> = (0..MOTORES)
        .map(|_| {
            Reader::new(
                modelos().join("craft_detector.onnx"),
                modelos().join("recognizer_latin_g2.onnx"),
            )
            .expect("modelos ONNX")
        })
        .collect();

    let t0 = Instant::now();
    let por_hilo = ctxs.len().div_ceil(MOTORES).max(1);
    let motivos: Vec<String> = std::thread::scope(|ambito| {
        let mut handles = Vec::new();
        for (trozo, motor) in ctxs.chunks(por_hilo).zip(pool.iter_mut()) {
            let p = &pipeline;
            handles.push(ambito.spawn(move || {
                let mut fuera = Vec::new();
                for ctx in trozo {
                    let v = match p.run_fast(ctx) {
                        Some(v) => v,
                        None => match p.run_slow(ctx, motor) {
                            Ok(v) => v,
                            Err(_) => continue,
                        },
                    };
                    if !v.approved {
                        fuera.extend(v.reasons.clone());
                    }
                }
                fuera
            }));
        }
        handles.into_iter().filter_map(|h| h.join().ok()).flatten().collect()
    });
    let s = t0.elapsed().as_secs_f64();
    println!(
        "{etiqueta:<34} {s:>8.1}s  {:>8.4}s/img   rechaza {:>3}",
        s / n_base,
        motivos.len()
    );
    (s, motivos)
}

#[tokio::test(flavor = "multi_thread")]
async fn medicion_unica_y_consistente() {
    if !Path::new(ARCHIVO).exists() {
        eprintln!("SALTADO");
        return;
    }
    std::env::set_var("OCR_TOOLS_ASSETS_DIR", env!("CARGO_MANIFEST_DIR"));

    let libro = umya_spreadsheet::reader::xlsx::read(Path::new(ARCHIVO)).expect("libro");
    let mut urls = Vec::new();
    for ws in libro.get_sheet_collection() {
        let (mc, mr) = ws.get_highest_column_and_row();
        for c in 1..=mc {
            for f in 1..=mr {
                urls.extend(ocr_tools::url_helper::split_image_urls(Some(&ws.get_value((c, f)))));
            }
        }
    }
    urls.sort();
    let antes = urls.len();
    urls.dedup();
    println!("urls en el archivo: {antes}  |  unicas: {}", urls.len());
    if antes > urls.len() {
        println!("  -> {} duplicadas ({:.1}%)", antes - urls.len(),
                 100.0 * (antes - urls.len()) as f64 / antes as f64);
    }

    let d = AsyncImageDownloader::new(DownloadConfig::default()).expect("cliente");
    let t = Instant::now();
    let imgs: Vec<image::RgbImage> = stream::iter(urls.iter())
        .map(|u| {
            let d = &d;
            async move {
                match d.fetch(u).await {
                    Ok(b) => decode_and_resize(&b, Some(1280)),
                    Err(_) => None,
                }
            }
        })
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect();
    let s_desc = t.elapsed().as_secs_f64();
    let n = imgs.len();
    let nf = n as f64;
    println!("descargadas: {n} en {s_desc:.1}s ({:.4}s/img con 8 en paralelo)\n", s_desc / nf);

    let ctxs: Vec<ImageContext> = imgs.iter().map(ImageContext::new).collect();

    println!("{:<34} {:>9} {:>12} {:>12}", "configuracion", "total", "s/img", "rechazos");
    println!("{}", "-".repeat(72));

    let (s_todo, motivos_todo) = medir(
        "TODO (D1+D2+OCR)",
        &ctxs,
        DetectorToggles { d1: true, d2: true, d3_d4_d5: true },
        nf,
    );
    let (s_rapidos, motivos_rapidos) = medir(
        "solo D1+D2 (sin OCR)",
        &ctxs,
        DetectorToggles { d1: true, d2: true, d3_d4_d5: false },
        nf,
    );

    // Que aporta cada detector, contando por prefijo del motivo.
    let mut por_det: HashMap<String, usize> = HashMap::new();
    for m in &motivos_todo {
        let det = m.split('\u{b7}').next().unwrap_or("?").trim().to_string();
        *por_det.entry(det).or_insert(0) += 1;
    }
    println!("\nAPORTE DE CADA DETECTOR (sobre {n} imagenes):");
    let mut v: Vec<_> = por_det.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    for (det, k) in &v {
        println!("  {det:<4} -> {k:>3} rechazos ({:.1}% del lote)", 100.0 * *k as f64 / nf);
    }

    let alfa = (s_todo - s_rapidos) / s_todo;
    let extra = motivos_todo.len() as i64 - motivos_rapidos.len() as i64;
    println!("\nALFA (fraccion OCR, mismo paralelismo) = {alfa:.3}");
    println!("El OCR cuesta {:.1}x y aporta {extra} rechazos mas", s_todo / s_rapidos);
    println!("\nRENDIMIENTO POR MAQUINA DE 12 vCPU:");
    println!("  con OCR : {:.4}s/img  ({:.0} img/hora)", s_todo / nf, 3600.0 / (s_todo / nf));
    println!("  sin OCR : {:.4}s/img  ({:.0} img/hora)", s_rapidos / nf, 3600.0 / (s_rapidos / nf));
}
