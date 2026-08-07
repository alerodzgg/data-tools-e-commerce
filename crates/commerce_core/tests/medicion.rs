//! Medición de los caminos calientes. No es un test de corrección: imprime
//! tiempos para comparar antes/después de un cambio.
//!
//! Cada medición se repite y se informa la MEDIANA. Una sola corrida en una
//! máquina de escritorio varía hasta un 50 % entre ejecuciones idénticas
//! —otros procesos, caché, frecuencia del CPU— y con esa dispersión no se
//! puede distinguir una mejora del 30 % del ruido.
//! Se corre con `cargo test -p commerce_core --test medicion --release -- --ignored --nocapture`.

use commerce_core::escritor_xlsx::OpcionesEscritorXlsx;
use commerce_core::EscritorXlsx;
use polars::prelude::*;

/// Corre `f` `REPETICIONES` veces y devuelve la mediana. Descarta la primera
/// corrida: calienta cachés y páginas, y siempre sale peor que el resto.
fn mediana(f: impl Fn() -> std::time::Duration) -> std::time::Duration {
    const REPETICIONES: usize = 5;
    let mut tiempos: Vec<std::time::Duration> = (0..=REPETICIONES).map(|_| f()).skip(1).collect();
    tiempos.sort_unstable();
    tiempos[tiempos.len() / 2]
}

#[test]
#[ignore = "medición, no corrección"]
fn tiempo_de_escritura_xlsx() {
    const FILAS: usize = 500_000;
    const COLUMNAS: usize = 12;

    let columnas: Vec<Column> = (0..COLUMNAS)
        .map(|c| {
            let valores: Vec<String> = (0..FILAS).map(|f| format!("v{c}_{f}")).collect();
            Column::new(format!("Col{c}").into(), valores)
        })
        .collect();
    let df = DataFrame::new_infer_height(columnas).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let transcurrido = mediana(|| {
        let inicio = std::time::Instant::now();
        let mut escritor =
            EscritorXlsx::nuevo(tmp.path().join("bench.xlsx"), OpcionesEscritorXlsx::default()).unwrap();
        escritor.escribir(&df, Some("Datos")).unwrap();
        escritor.cerrar().unwrap();
        inicio.elapsed()
    });

    let celdas = FILAS * COLUMNAS;
    eprintln!(
        "XLSX (mediana de 5): {FILAS} filas x {COLUMNAS} col = {celdas} celdas en {:.2?} ({:.1} M celdas/s)",
        transcurrido,
        celdas as f64 / transcurrido.as_secs_f64() / 1e6
    );
}

#[test]
#[ignore = "medición, no corrección"]
fn tiempo_de_lectura_xlsx() {
    // Leer es sospechoso por construcción: `calamine` materializa la hoja
    // entera como `Range<Data>` y después `hoja_a_dataframe` la vuelve a
    // materializar como `Vec<Vec<Option<String>>>`. Son DOS copias completas
    // más una asignación de `String` por celda.
    const FILAS: usize = 500_000;
    const COLUMNAS: usize = 12;

    let columnas: Vec<Column> = (0..COLUMNAS)
        .map(|c| {
            let valores: Vec<String> = (0..FILAS).map(|f| format!("v{c}_{f}")).collect();
            Column::new(format!("Col{c}").into(), valores)
        })
        .collect();
    let df = DataFrame::new_infer_height(columnas).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let mut escritor =
        EscritorXlsx::nuevo(tmp.path().join("leer.xlsx"), OpcionesEscritorXlsx::default()).unwrap();
    escritor.escribir(&df, Some("Datos")).unwrap();
    escritor.cerrar().unwrap();
    let ruta = escritor.ruta.clone();
    let bytes = std::fs::metadata(&ruta).unwrap().len();

    let transcurrido = mediana(|| {
        let inicio = std::time::Instant::now();
        let bloques = commerce_core::iter_hojas_xlsx(&ruta, None, |_: &str| {});
        let t = inicio.elapsed();
        assert!(!bloques.is_empty());
        t
    });

    let bloques = commerce_core::iter_hojas_xlsx(&ruta, None, |_: &str| {});
    let filas: usize = bloques.iter().map(polars::prelude::DataFrame::height).sum();
    let celdas = filas * COLUMNAS;
    eprintln!(
        "LECTURA (mediana de 5): {celdas} celdas desde {:.1} MB en {:.2?} ({:.1} M celdas/s)",
        bytes as f64 / 1048576.0,
        transcurrido,
        celdas as f64 / transcurrido.as_secs_f64() / 1e6
    );
}

#[test]
#[ignore = "medición, no corrección"]
fn cuanto_del_tiempo_de_lectura_es_calamine() {
    // Separa el costo del parser XML (calamine) del de nuestra construcción
    // del DataFrame. Si casi todo es calamine, optimizar `hoja_a_dataframe`
    // no puede dar mucho más, y el techo lo pone la librería.
    const FILAS: usize = 500_000;
    const COLUMNAS: usize = 12;

    let columnas: Vec<Column> = (0..COLUMNAS)
        .map(|c| {
            let valores: Vec<String> = (0..FILAS).map(|f| format!("v{c}_{f}")).collect();
            Column::new(format!("Col{c}").into(), valores)
        })
        .collect();
    let df = DataFrame::new_infer_height(columnas).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let mut escritor =
        EscritorXlsx::nuevo(tmp.path().join("solo.xlsx"), OpcionesEscritorXlsx::default()).unwrap();
    escritor.escribir(&df, Some("Datos")).unwrap();
    escritor.cerrar().unwrap();
    let ruta = escritor.ruta.clone();

    // Solo calamine: abrir y materializar el rango, sin construir nada nuestro.
    let inicio = std::time::Instant::now();
    let mut libro = commerce_core::abrir_libro(&ruta).unwrap();
    let hoja = commerce_core::nombres_hojas_libro(&libro)[0].clone();
    let filas = commerce_core::contar_filas_hoja(&mut libro, &ruta, &hoja).unwrap();
    let solo_parser = inicio.elapsed();

    // Camino completo, para comparar.
    let inicio = std::time::Instant::now();
    let _ = commerce_core::iter_hojas_xlsx(&ruta, None, |_: &str| {});
    let completo = inicio.elapsed();

    eprintln!(
        "SOLO CALAMINE: {filas} filas en {solo_parser:.2?}  |  CAMINO COMPLETO: {completo:.2?}  |  \
         parser = {:.0}% del total",
        solo_parser.as_secs_f64() / completo.as_secs_f64() * 100.0
    );
}

#[test]
#[ignore = "medición, no corrección"]
fn cual_es_el_piso_fisico_de_la_lectura() {
    // Antes de rediseñar nada: ¿cuánto cuesta lo IRREDUCIBLE? Descomprimir el
    // zip y recorrer el XML son trabajo obligatorio para cualquier lector.
    // Si el piso está muy por debajo de lo que tarda calamine, hay margen
    // para un parser propio; si está cerca, no lo hay y no importa cómo se
    // arquitecture.
    use std::io::Read;

    const FILAS: usize = 500_000;
    const COLUMNAS: usize = 12;

    let columnas: Vec<Column> = (0..COLUMNAS)
        .map(|c| {
            let valores: Vec<String> = (0..FILAS).map(|f| format!("v{c}_{f}")).collect();
            Column::new(format!("Col{c}").into(), valores)
        })
        .collect();
    let df = DataFrame::new_infer_height(columnas).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let mut escritor =
        EscritorXlsx::nuevo(tmp.path().join("piso.xlsx"), OpcionesEscritorXlsx::default()).unwrap();
    escritor.escribir(&df, Some("Datos")).unwrap();
    escritor.cerrar().unwrap();
    let ruta = escritor.ruta.clone();

    // (a) Solo descomprimir la hoja a memoria.
    let inicio = std::time::Instant::now();
    let archivo = std::fs::File::open(&ruta).unwrap();
    let mut zip = ::zip::ZipArchive::new(archivo).unwrap();
    let mut xml = Vec::new();
    zip.by_name("xl/worksheets/sheet1.xml")
        .unwrap()
        .read_to_end(&mut xml)
        .unwrap();
    let descompresion = inicio.elapsed();
    let mb = xml.len() as f64 / 1048576.0;

    // (b) Recorrer ese XML con quick-xml, contando celdas y su texto.
    let inicio = std::time::Instant::now();
    let mut lector = quick_xml::Reader::from_reader(xml.as_slice());
    let mut buf = Vec::new();
    let mut celdas = 0usize;
    let mut bytes_texto = 0usize;
    loop {
        match lector.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) => {
                if e.name().into_inner() == b"c" {
                    celdas += 1;
                }
            }
            Ok(quick_xml::events::Event::Text(e)) => bytes_texto += e.len(),
            Ok(quick_xml::events::Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    let parseo = inicio.elapsed();

    eprintln!(
        "PISO: XML {mb:.0} MB | descomprimir {descompresion:.2?} | parsear {parseo:.2?} \
         ({:.0} MB/s) | total {:.2?} | {celdas} celdas, {bytes_texto} bytes de texto",
        mb / parseo.as_secs_f64(),
        descompresion + parseo
    );
}

/// Columna 0-based a partir de una referencia tipo `"BC12"`.
fn columna_de_ref(referencia: &[u8]) -> usize {
    let mut n = 0usize;
    for b in referencia {
        if b.is_ascii_alphabetic() {
            n = n * 26 + (b.to_ascii_uppercase() - b'A' + 1) as usize;
        } else {
            break;
        }
    }
    n.saturating_sub(1)
}

#[test]
#[ignore = "medición, no corrección"]
fn prototipo_de_lector_propio() {
    // Spike: parser de streaming para NUESTRO formato conocido
    // (`<c r=".." t="inlineStr"><is><t>texto</t></is></c>`), escribiendo
    // directo a los builders de Arrow. Compara contra el camino con calamine
    // sobre el mismo archivo. Si la diferencia es chica, no vale rediseñar.
    use quick_xml::events::Event;
    use std::io::Read;

    const FILAS: usize = 500_000;
    const COLUMNAS: usize = 12;

    let columnas: Vec<Column> = (0..COLUMNAS)
        .map(|c| {
            let valores: Vec<String> = (0..FILAS).map(|f| format!("v{c}_{f}")).collect();
            Column::new(format!("Col{c}").into(), valores)
        })
        .collect();
    let df = DataFrame::new_infer_height(columnas).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let mut escritor =
        EscritorXlsx::nuevo(tmp.path().join("proto.xlsx"), OpcionesEscritorXlsx::default()).unwrap();
    escritor.escribir(&df, Some("Datos")).unwrap();
    escritor.cerrar().unwrap();
    let ruta = escritor.ruta.clone();

    // ── camino actual (calamine) ────────────────────────────────────────
    let inicio = std::time::Instant::now();
    let actual = commerce_core::iter_hojas_xlsx(&ruta, None, |_: &str| {});
    let con_calamine = inicio.elapsed();
    let filas_actual = actual[0].height();

    // ── prototipo propio ────────────────────────────────────────────────
    let inicio = std::time::Instant::now();
    let mut zip = ::zip::ZipArchive::new(std::fs::File::open(&ruta).unwrap()).unwrap();
    let mut xml = Vec::new();
    zip.by_name("xl/worksheets/sheet1.xml")
        .unwrap()
        .read_to_end(&mut xml)
        .unwrap();
    let descomprimido = inicio.elapsed();
    let mb = xml.len() as f64 / 1048576.0;

    let inicio_parseo = std::time::Instant::now();
    let mut lector = quick_xml::Reader::from_reader(xml.as_slice());
    let mut buf = Vec::new();
    let mut celdas: Vec<Vec<Option<String>>> = (0..COLUMNAS).map(|_| Vec::with_capacity(FILAS)).collect();
    let mut fila: Vec<Option<String>> = vec![None; COLUMNAS];
    let mut col = 0usize;
    let mut en_texto = false;
    let mut n_filas = 0usize;

    loop {
        match lector.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.name().into_inner() {
                b"c" => {
                    if let Ok(Some(a)) = e.try_get_attribute("r") {
                        col = columna_de_ref(&a.value);
                    }
                }
                b"t" => en_texto = true,
                _ => {}
            },
            Ok(Event::Text(e)) if en_texto => {
                if col < COLUMNAS {
                    fila[col] = Some(String::from_utf8_lossy(e.as_ref()).into_owned());
                }
            }
            Ok(Event::End(e)) => match e.name().into_inner() {
                b"t" => en_texto = false,
                b"row" => {
                    n_filas += 1;
                    if n_filas > 1 {
                        for (c, valor) in fila.iter_mut().enumerate() {
                            celdas[c].push(valor.take());
                        }
                    } else {
                        fila.iter_mut().for_each(|v| *v = None); // cabecera
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    let parseo = inicio_parseo.elapsed();
    let propio = descomprimido + parseo;

    eprintln!(
        "PROTOTIPO: XML {mb:.0} MB\n  calamine   {con_calamine:.2?}  ({filas_actual} filas)\n  \
         propio     {propio:.2?}  (descomprimir {descomprimido:.2?} + parsear {parseo:.2?} = {:.0} MB/s)\n  \
         GANANCIA   {:.1}x   |   piso irreducible = descompresión {:.0}% del tiempo propio",
        mb / parseo.as_secs_f64(),
        con_calamine.as_secs_f64() / propio.as_secs_f64(),
        descomprimido.as_secs_f64() / propio.as_secs_f64() * 100.0
    );
    assert_eq!(celdas[0].len(), FILAS, "el prototipo debe leer todas las filas");
}
