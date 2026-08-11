//! Inserta, para cada columna con URLs de imagen, una o más columnas nuevas
//! inmediatamente a su derecha con la imagen descargada. `ImageEmbedder` usa
//! `umya-spreadsheet` — el único crate de este workspace que sabe leer,
//! MODIFICAR (insertar columnas desplazando lo existente) y volver a
//! escribir un XLSX ya existente con imágenes incrustadas; `calamine` es
//! solo-lectura y `rust_xlsxwriter` solo escribe libros nuevos desde cero.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use futures::stream::{self, StreamExt};
use image::{imageops::FilterType, RgbImage};
use umya_spreadsheet::structs::drawing::spreadsheet::MarkerType;
use umya_spreadsheet::{self as xlsx, Worksheet};

use crate::downloader::{AsyncImageDownloader, DownloadConfig};
use crate::url_helper;

/// Por qué [`ImageEmbedder::procesar_archivo`] no generó una salida —
/// distingue las 3 causas en vez de colapsarlas en un mismo `None`, para que
/// el mensaje al usuario no asuma siempre "sin columnas de URL" cuando en
/// realidad el archivo está corrupto o falló la escritura del destino.
#[derive(Debug)]
pub enum MotivoSinProcesar {
    /// El archivo de origen no se pudo abrir (corrupto, no es un .xlsx válido, etc.).
    ArchivoCorrupto,
    /// El archivo excede los límites de tamaño/descompresión de
    /// [`verificar_tamano_xlsx_seguro`] — se rechaza sin llegar a
    /// materializarlo completo en memoria.
    ArchivoDemasiadoGrande,
    /// Se abrió bien, pero ninguna hoja tenía columnas de URL de imagen.
    SinColumnasDeUrl,
    /// Se procesó al menos una hoja, pero no se pudo escribir el destino.
    FalloEscritura,
}

/// Cota real de Excel (16 384 columnas `A..XFD`, 1 048 576 filas): protege
/// `detectar_columnas_url`/`recolectar` de una hoja con `<dimension>`
/// manipulado que declare una geometría mucho mayor que la real —
/// `get_highest_column_and_row()` solo LEE ese atributo del XML, no lo
/// valida contra los datos reales de la hoja.
const MAX_COLUMNAS_HOJA: u32 = 16_384;
const MAX_FILAS_HOJA: u32 = 1_048_576;

/// Filas escaneadas como máximo POR COLUMNA en `detectar_columnas_url` antes
/// de rendirse: sin esto, una columna vacía (o casi) nunca dispara el
/// `break` por `vistos >= n` y fuerza un escaneo completo de la hoja — el
/// verdadero multiplicador de costo (hasta `max_col` veces), más que
/// `max_col`/`max_row` en sí mismos.
const MAX_FILAS_MUESTREADAS_POR_COLUMNA: u32 = 10_000;

/// Lecturas de celda TOTALES (sumando TODAS las columnas) que tolera
/// `detectar_columnas_url` antes de rendirse con lo que ya detectó. El cap
/// por columna de arriba acota el costo de CADA columna, pero no el TOTAL:
/// con `MAX_COLUMNAS_HOJA` columnas todas dispersas/vacías, el peor caso
/// seguía siendo `MAX_COLUMNAS_HOJA × MAX_FILAS_MUESTREADAS_POR_COLUMNA` ≈
/// 164M lecturas ante una hoja con geometría manipulada.
const MAX_LECTURAS_TOTALES_DETECCION: u64 = 2_000_000;

mod layout;
mod salvaguardas;

use layout::{generar_tareas, insertar_columnas, px_a_ancho_columna, px_a_puntos, TareaImagen};
use salvaguardas::verificar_tamano_xlsx_seguro;

pub struct ImageEmbedConfig {
    pub ancho_px: u32,
    pub alto_px: u32,
    pub intentos_descarga: u32,
    pub texto_error: String,
    pub max_muestra_deteccion: usize,
    pub umbral_deteccion: f32,
}

impl Default for ImageEmbedConfig {
    fn default() -> Self {
        Self {
            ancho_px: 118,
            alto_px: 168,
            intentos_descarga: 2,
            texto_error: "Error".to_string(),
            max_muestra_deteccion: 10,
            umbral_deteccion: 0.30,
        }
    }
}

pub struct ImageEmbedder {
    cfg: ImageEmbedConfig,
    download_cfg: DownloadConfig,
    max_concurrency: usize,
    /// Un solo `reqwest::Client` (con su pool de conexiones) para todo el
    /// archivo, no uno nuevo por hoja: `descargar_todas` se llama una vez por
    /// hoja (vía `procesar_hoja`), y reconstruir el cliente en cada llamada
    /// perdería el reuso de conexiones entre hojas del mismo libro.
    downloader_cache: tokio::sync::OnceCell<AsyncImageDownloader>,
    /// Correlativo para el nombre de archivo de cada imagen incrustada, único
    /// en TODO el libro (no por hoja: todas comparten el mismo `xl/media/`).
    /// Ver [`Self::nombre_imagen_unico`].
    siguiente_imagen: std::sync::atomic::AtomicUsize,
}

/// Restaura el texto de las celdas que `umya-spreadsheet` convirtió a número
/// al leer.
///
/// `umya` preserva el tipo de las celdas `t="str"`, pero para las
/// `t="inlineStr"` —que es lo que escribe `EscritorXlsx`— llama a su
/// `guess_typed_data`, que convierte a número todo lo que parsee como `f64`.
/// Al reescribir el libro, `007` sale como `7`, `1e5` como `100000` y `0000`
/// como `0`: los códigos del usuario destruidos en silencio.
///
/// La pérdida ocurre al LEER, así que no se puede reparar mirando lo que
/// quedó en memoria: para entonces el texto original ya no existe. Por eso se
/// relee el archivo con el lector del workspace (calamine, que sí respeta
/// `inlineStr`) y se restaura desde ahí.
///
/// Solo se tocan las celdas que `umya` tipó como numéricas: las que leyó bien
/// se dejan intactas, y una celda que en el origen era de verdad un número
/// sigue siéndolo, porque el lector la habría devuelto igual.
fn restaurar_texto_original(libro: &mut xlsx::Spreadsheet, ruta: &Path) {
    let Ok(mut origen) = commerce_core::abrir_libro(ruta) else {
        return;
    };
    for nombre in commerce_core::nombres_hojas_libro(&origen) {
        let Ok(df) = commerce_core::leer_hoja_por_nombre(&mut origen, ruta, &nombre) else {
            continue;
        };
        let Some(hoja) = libro.get_sheet_by_name_mut(&nombre) else {
            continue;
        };
        for (j, columna) in df.get_column_names_owned().into_iter().enumerate() {
            let col = j as u32 + 1;
            // Fila 1 = cabecera; los datos empiezan en la 2.
            restaurar_celda(hoja, col, 1, columna.as_str());
            let Ok(serie) = df.column(columna.as_str()) else {
                continue;
            };
            let Ok(textos) = serie.as_materialized_series().str().cloned() else {
                continue;
            };
            for (i, valor) in textos.iter().enumerate() {
                if let Some(texto) = valor {
                    restaurar_celda(hoja, col, i as u32 + 2, texto);
                }
            }
        }
    }
}

/// Reescribe `(col, fila)` como texto solo si `umya` la tipó numérica (`"n"`).
fn restaurar_celda(hoja: &mut Worksheet, col: u32, fila: u32, texto: &str) {
    let era_numero = hoja
        .get_cell((col, fila))
        .is_some_and(|celda| celda.get_data_type() == "n");
    if era_numero {
        hoja.get_cell_mut((col, fila)).set_value_string(texto);
    }
}

impl ImageEmbedder {
    pub fn new(cfg: ImageEmbedConfig, mut download_cfg: DownloadConfig, max_concurrency: usize) -> Self {
        // "se intentará doble vez" = 2 intentos TOTALES = 1 reintento.
        download_cfg.retries = cfg.intentos_descarga.saturating_sub(1);
        Self {
            cfg,
            download_cfg,
            max_concurrency,
            downloader_cache: tokio::sync::OnceCell::new(),
            siguiente_imagen: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    // ── Paso 1: ¿qué columnas de esta hoja son de URL de imagen? ─────────
    fn detectar_columnas_url(&self, ws: &Worksheet, avisar: &mut dyn FnMut(&str)) -> Vec<u32> {
        let (max_col, max_row) = ws.get_highest_column_and_row();
        let max_col = max_col.min(MAX_COLUMNAS_HOJA);
        let max_row = max_row.min(MAX_FILAS_HOJA);
        let n = self
            .cfg
            .max_muestra_deteccion
            .min(max_row.saturating_sub(1) as usize);
        if n == 0 {
            return Vec::new();
        }
        let limite_fila = max_row.min(MAX_FILAS_MUESTREADAS_POR_COLUMNA.saturating_add(1));

        let mut detectadas = Vec::new();
        let mut lecturas_totales: u64 = 0;
        for col in 1..=max_col {
            if lecturas_totales >= MAX_LECTURAS_TOTALES_DETECCION {
                avisar(&format!(
                    "Se alcanzó el límite de lecturas al detectar columnas de imagen (hoja muy grande o \
                     con geometría inusual): se analizaron las primeras {} de {max_col} columnas.",
                    col - 1
                ));
                break;
            }
            let mut vistos = 0usize;
            let mut aciertos = 0usize;
            for fila in 2..=limite_fila {
                lecturas_totales += 1;
                let valor = ws.get_value((col, fila));
                if valor.trim().is_empty() {
                    continue;
                }
                vistos += 1;
                if !url_helper::split_image_urls(Some(&valor)).is_empty() {
                    aciertos += 1;
                }
                if vistos >= n {
                    break;
                }
            }
            // El umbral se mide contra las celdas REALMENTE muestreadas, no
            // contra el tamaño de muestra pretendido (`n`): una columna con
            // pocas celdas llenas pero todas URLs es inequívocamente de
            // imágenes, y medirla contra `n` la descartaba en silencio — sin
            // insertar sus imágenes ni decir por qué.
            let minimo = ((vistos as f32 * self.cfg.umbral_deteccion) as usize).max(1);
            if vistos > 0 && aciertos >= minimo {
                detectadas.push(col);
            }
        }
        detectadas
    }

    // ── Paso 2: recolectar las URLs de cada celda de esas columnas ───────
    fn recolectar(ws: &Worksheet, columnas_url: &[u32]) -> HashMap<u32, HashMap<u32, Vec<String>>> {
        let (_, max_row) = ws.get_highest_column_and_row();
        let max_row = max_row.min(MAX_FILAS_HOJA);
        let mut recolectado: HashMap<u32, HashMap<u32, Vec<String>>> =
            columnas_url.iter().map(|&c| (c, HashMap::new())).collect();
        for &col in columnas_url {
            for fila in 2..=max_row {
                let valor = ws.get_value((col, fila));
                let urls = url_helper::split_image_urls(Some(&valor));
                if !urls.is_empty() {
                    recolectado.entry(col).or_default().insert(fila, urls);
                }
            }
        }
        recolectado
    }

    /// Decodifica vía `downloader::decode_and_resize` (con `resize_max_dim:
    /// None`, o sea sin su propio resize: el de acá redimensiona exacto a
    /// `ancho_px`/`alto_px` después). Reusarla, en vez de decodificar por
    /// cuenta propia, es lo que hace que los límites anti bomba-de-
    /// descompresión (`image::Limits`) sean los mismos acá y en el modo de
    /// análisis, sin una segunda implementación que pueda divergir en dureza.
    fn decodificar_y_redimensionar(&self, content: &[u8]) -> Option<RgbImage> {
        let img = crate::downloader::decode_and_resize(content, None)?;
        Some(image::imageops::resize(
            &img,
            self.cfg.ancho_px,
            self.cfg.alto_px,
            FilterType::Triangle,
        ))
    }

    // ── Paso 4: descargar + redimensionar, concurrencia acotada ──────────
    /// Devuelve, por índice de tarea, la imagen ya redimensionada o `None` si
    /// falló tras agotar los intentos (esa tarea escribirá `texto_error`).
    async fn descargar_todas(
        &self,
        tareas: &[TareaImagen],
        avisar: &mut dyn FnMut(&str),
    ) -> Vec<Option<RgbImage>> {
        // Bajo test (de esta librería o de tests/, vía la feature
        // `test-support`), el servidor mock corre en 127.0.0.1: el
        // downloader de producción lo rechazaría por el filtro anti-SSRF
        // (ver `downloader::tests`), así que los tests usan un constructor
        // que permite hosts privados. En release ese constructor ni siquiera
        // existe.
        #[cfg(not(any(test, feature = "test-support")))]
        let downloader = self
            .downloader_cache
            .get_or_try_init(|| async { AsyncImageDownloader::new(self.download_cfg.clone()) })
            .await;
        #[cfg(any(test, feature = "test-support"))]
        let downloader = self
            .downloader_cache
            .get_or_try_init(|| async {
                AsyncImageDownloader::nuevo_para_test_con_hosts_privados_permitidos(self.download_cfg.clone())
            })
            .await;
        let downloader = match downloader {
            Ok(d) => d,
            Err(error) => {
                // Se avisa en vez de contarlo como error por URL: un fallo
                // de infraestructura (p. ej. backend TLS no disponible) no es
                // culpa de las URLs, y "N con error" a secas lo ocultaría.
                avisar(&format!("No se pudo inicializar el descargador: {error}"));
                return tareas.iter().map(|_| None).collect();
            }
        };

        let indexados: Vec<(usize, Result<RgbImage, String>)> = stream::iter(tareas.iter().enumerate())
            .map(|(i, tarea)| async move {
                // Este modo INSERTA imágenes en el xlsx: una que no se pudo
                // traer simplemente queda sin insertar (se reporta agregada
                // como "N con error" al final), así que acá solo interesa
                // si hay bytes o no — la causa concreta del fallo la usa el
                // modo de ANÁLISIS, que sí escribe un motivo por fila.
                // El motivo se conserva: descartarlo dejaba al usuario con
                // una columna de celdas que dicen "Error" y nada más, sin
                // forma de saber si el problema es la red, el servidor o el
                // contenido — que se arreglan de maneras distintas.
                let resultado = match downloader.fetch(&tarea.url).await {
                    Ok(bytes) => match self.decodificar_y_redimensionar(&bytes) {
                        Some(imagen) => Ok(imagen),
                        None => Err("descargada, pero no es una imagen válida".to_string()),
                    },
                    Err(fallo) => Err(fallo.to_string()),
                };
                (i, resultado)
            })
            .buffer_unordered(self.max_concurrency.max(1))
            .collect()
            .await;

        let mut resultados: Vec<Option<RgbImage>> = (0..tareas.len()).map(|_| None).collect();
        // Los motivos se agrupan y se informan UNA vez: un aviso por imagen
        // fallida ahogaría la consola, y lo que el usuario necesita para
        // decidir qué hacer es el reparto de causas, no el detalle de cada
        // fila.
        let mut motivos: HashMap<String, usize> = HashMap::new();
        for (i, resultado) in indexados {
            match resultado {
                Ok(imagen) => resultados[i] = Some(imagen),
                Err(motivo) => *motivos.entry(motivo).or_insert(0) += 1,
            }
        }
        if !motivos.is_empty() {
            let mut reparto: Vec<(String, usize)> = motivos.into_iter().collect();
            reparto.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let detalle = reparto
                .iter()
                .map(|(motivo, n)| format!("{n} × {motivo}"))
                .collect::<Vec<_>>()
                .join("; ");
            avisar(&format!("Imágenes que no se pudieron insertar — {detalle}"));
        }
        resultados
    }

    /// Nombre de archivo distinto para cada imagen incrustada.
    ///
    /// NO es cosmético: `umya-spreadsheet` usa este nombre como la ruta real
    /// dentro del .xlsx (`xl/media/<nombre>`), y su escritor **saltea en
    /// silencio** cualquier entrada cuyo nombre ya exista en el paquete
    /// (`WriterManager::add_bin` chequea `check_file_exist` y no sobrescribe).
    /// Con un nombre fijo, entonces, solo se guardaba la PRIMERA imagen del
    /// libro y todas las celdas terminaban apuntando a ese único archivo —
    /// el resultado visible era "todas las filas muestran la misma imagen"
    /// aunque cada URL se hubiera descargado bien y fuera distinta.
    ///
    /// El correlativo es de todo el libro, no por hoja: `xl/media/` es
    /// compartido, así que dos hojas con la misma numeración volverían a
    /// colisionar.
    fn nombre_imagen_unico(&self) -> String {
        let n = self
            .siguiente_imagen
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("img_{n}.png")
    }

    // ── Paso 5: insertar las imágenes descargadas + ajustar dimensiones ──
    fn insertar_imagenes(
        &self,
        ws: &mut Worksheet,
        tareas: &[TareaImagen],
        imagenes: &[Option<RgbImage>],
    ) -> (usize, usize) {
        let mut filas_afectadas: HashSet<u32> = HashSet::new();
        let mut columnas_afectadas: HashSet<u32> = HashSet::new();
        let mut exitos = 0usize;
        let mut fallos = 0usize;

        for (tarea, img) in tareas.iter().zip(imagenes) {
            let Some(rgb) = img else {
                ws.get_cell_mut((tarea.col_destino, tarea.fila_excel))
                    .set_value(self.cfg.texto_error.clone());
                fallos += 1;
                continue;
            };

            let mut buffer = std::io::Cursor::new(Vec::new());
            if rgb.write_to(&mut buffer, image::ImageFormat::Png).is_err() {
                ws.get_cell_mut((tarea.col_destino, tarea.fila_excel))
                    .set_value(self.cfg.texto_error.clone());
                fallos += 1;
                continue;
            }

            let mut marker = MarkerType::default();
            marker.set_coordinate(xlsx::helper::coordinate::coordinate_from_index(
                &tarea.col_destino,
                &tarea.fila_excel,
            ));
            let mut xl_img = xlsx::Image::default();
            xl_img.new_image_with_dimensions(
                self.cfg.alto_px,
                self.cfg.ancho_px,
                &self.nombre_imagen_unico(),
                buffer.into_inner(),
                marker,
            );
            ws.add_image(xl_img);
            // Recién ACÁ, después de confirmar que la imagen se insertó: si
            // se marcara para toda tarea, una celda con "Error" quedaría con
            // el alto/ancho de una imagen que nunca llegó a incrustarse.
            filas_afectadas.insert(tarea.fila_excel);
            columnas_afectadas.insert(tarea.col_destino);
            exitos += 1;
        }

        for fila in filas_afectadas {
            ws.get_row_dimension_mut(&fila)
                .set_height(px_a_puntos(self.cfg.alto_px));
        }
        for col in columnas_afectadas {
            ws.get_column_dimension_by_number_mut(&col)
                .set_width(px_a_ancho_columna(self.cfg.ancho_px));
        }

        (exitos, fallos)
    }

    // ── Orquestador de UNA hoja ───────────────────────────────────────────
    async fn procesar_hoja(&self, ws: &mut Worksheet, avisar: &mut dyn FnMut(&str)) -> (usize, usize) {
        let columnas_url = self.detectar_columnas_url(ws, avisar);
        if columnas_url.is_empty() {
            return (0, 0);
        }
        let recolectado = Self::recolectar(ws, &columnas_url);
        if recolectado.values().all(HashMap::is_empty) {
            return (0, 0);
        }
        let destino_final = insertar_columnas(ws, &recolectado, avisar);
        let tareas = generar_tareas(&recolectado, &destino_final);
        let imagenes = self.descargar_todas(&tareas, avisar).await;
        self.insertar_imagenes(ws, &tareas, &imagenes)
    }

    // ── API pública: UN archivo, todas sus hojas (salvo excluidas) ───────
    /// `None` si el archivo no se pudo abrir, o si ninguna hoja tenía
    /// columnas con URLs de imagen (nada que insertar).
    pub async fn procesar_archivo(
        &self,
        ruta: &Path,
        hojas_excluir: &HashSet<String>,
        destino: &Path,
        mut avisar: impl FnMut(&str),
    ) -> Result<(PathBuf, usize, usize, usize), MotivoSinProcesar> {
        verificar_tamano_xlsx_seguro(ruta)?;
        let mut libro = xlsx::reader::xlsx::read(ruta).map_err(|_| MotivoSinProcesar::ArchivoCorrupto)?;
        restaurar_texto_original(&mut libro, ruta);

        let mut total_exitos = 0usize;
        let mut total_fallos = 0usize;
        let mut hojas_procesadas = 0usize;

        for ws in libro.get_sheet_collection_mut() {
            if hojas_excluir.contains(&ws.get_name().trim().to_lowercase()) {
                continue;
            }
            let (exitos, fallos) = self.procesar_hoja(ws, &mut avisar).await;
            if exitos > 0 || fallos > 0 {
                hojas_procesadas += 1;
                total_exitos += exitos;
                total_fallos += fallos;
            }
        }

        if hojas_procesadas == 0 {
            return Err(MotivoSinProcesar::SinColumnasDeUrl);
        }

        // La ruta ÚNICA se resuelve acá, no en el llamador: `umya-spreadsheet`
        // escribe donde le digan y pisaría una salida anterior. El resto de
        // los escritores del workspace lo resuelven adentro (ADR 0001) y
        // devuelven la ruta real; esta función ya devolvía un `PathBuf`, así
        // que hacerlo acá la alinea sin cambiarle la firma.
        let destino = commerce_core::ruta_unica(destino);
        if xlsx::writer::xlsx::write(&libro, &destino).is_err() {
            // Un fallo a mitad de escritura deja un .xlsx corrupto: se borra,
            // igual que hace `abortar()` en los escritores propios.
            let _ = std::fs::remove_file(&destino);
            return Err(MotivoSinProcesar::FalloEscritura);
        }
        Ok((destino, total_exitos, total_fallos, hojas_procesadas))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn libro_de_prueba() -> xlsx::Spreadsheet {
        xlsx::new_file()
    }

    fn servidor_mock_imagen() -> String {
        servidor_mock_imagen_color([5, 5, 5])
    }

    /// Como [`servidor_mock_imagen`], pero con un color a elección: dos
    /// servidores con colores distintos dan imágenes con bytes distintos,
    /// que es lo que permite detectar si el .xlsx terminó reusando una sola.
    fn servidor_mock_imagen_color(color: [u8; 3]) -> String {
        let bytes = {
            let img = RgbImage::from_pixel(20, 20, image::Rgb(color));
            let mut buf = std::io::Cursor::new(Vec::new());
            img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
            buf.into_inner()
        };
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
        format!("http://127.0.0.1:{puerto}/img.png")
    }

    #[tokio::test]
    async fn procesar_archivo_no_pisa_una_salida_anterior() {
        // `umya-spreadsheet` escribe donde le digan: sin resolver la ruta
        // única acá, una segunda corrida machacaba el resultado de la primera.
        let tmp = tempfile::tempdir().unwrap();
        let origen = tmp.path().join("origen.xlsx");
        {
            let mut libro = libro_de_prueba();
            let ws = libro.get_sheet_by_name_mut("Sheet1").unwrap();
            ws.get_cell_mut((1, 1)).set_value("Fotos");
            ws.get_cell_mut((1, 2))
                .set_value("http://127.0.0.1:1/no-existe.png");
            xlsx::writer::xlsx::write(&libro, &origen).unwrap();
        }
        let destino = tmp.path().join("salida.xlsx");
        let embedder = ImageEmbedder::new(ImageEmbedConfig::default(), DownloadConfig::default(), 4);
        let excluir = HashSet::new();

        let (primera, ..) = embedder
            .procesar_archivo(&origen, &excluir, &destino, |_| {})
            .await
            .expect("primera corrida");
        let (segunda, ..) = embedder
            .procesar_archivo(&origen, &excluir, &destino, |_| {})
            .await
            .expect("segunda corrida");

        assert_ne!(primera, segunda, "la segunda corrida pisó la primera");
        assert!(primera.exists() && segunda.exists());
    }
    #[test]
    fn decodificar_y_redimensionar_rechaza_dimensiones_que_exceden_el_limite() {
        // Los límites anti bomba-de-descompresión se heredan de
        // `downloader::decode_and_resize` (MAX_IMAGE_DIM=20_000): decodificar
        // sin ellos deja que un archivo chico declare dimensiones enormes.
        let img = RgbImage::from_pixel(20_001, 2, image::Rgb([1, 2, 3]));
        let mut bytes = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();

        let embedder = ImageEmbedder::new(ImageEmbedConfig::default(), DownloadConfig::default(), 4);
        assert!(embedder.decodificar_y_redimensionar(&bytes).is_none());
    }
    #[test]
    fn detectar_columnas_url_con_pocas_filas_escala_el_umbral() {
        let mut libro = libro_de_prueba();
        let ws = libro.get_sheet_by_name_mut("Sheet1").unwrap();
        ws.get_cell_mut((1, 1)).set_value("Sku");
        ws.get_cell_mut((2, 1)).set_value("Fotos");
        ws.get_cell_mut((1, 2)).set_value("A1");
        ws.get_cell_mut((2, 2)).set_value("http://x.com/1.jpg");
        ws.get_cell_mut((1, 3)).set_value("A2");
        ws.get_cell_mut((2, 3)).set_value("http://x.com/2.jpg");

        let embedder = ImageEmbedder::new(ImageEmbedConfig::default(), DownloadConfig::default(), 4);
        assert_eq!(embedder.detectar_columnas_url(ws, &mut |_| {}), vec![2]);
    }
    #[test]
    fn detectar_columnas_url_no_escanea_mas_alla_del_limite_por_columna() {
        // Una columna sin valores no-vacíos en las primeras filas nunca
        // dispara el `break` por `vistos >= n`: sin el tope por columna, un
        // `.xlsx` con `<dimension>` inflado haría escanear hasta `max_row` y
        // colgaría el proceso. Acá la URL real está MÁS ALLÁ del tope: debe
        // quedar sin detectar y la llamada debe volver rápido.
        let mut libro = libro_de_prueba();
        let ws = libro.get_sheet_by_name_mut("Sheet1").unwrap();
        ws.get_cell_mut((1, 1)).set_value("Sku");
        ws.get_cell_mut((2, 1)).set_value("Fotos");
        let fila_lejana = MAX_FILAS_MUESTREADAS_POR_COLUMNA + 100;
        ws.get_cell_mut((1, fila_lejana)).set_value("A1");
        ws.get_cell_mut((2, fila_lejana)).set_value("http://x.com/1.jpg");

        let embedder = ImageEmbedder::new(ImageEmbedConfig::default(), DownloadConfig::default(), 4);
        let inicio = std::time::Instant::now();
        let detectadas = embedder.detectar_columnas_url(ws, &mut |_| {});
        assert!(
            inicio.elapsed() < std::time::Duration::from_secs(5),
            "debe rendirse dentro del límite por columna, no escanear hasta max_row"
        );
        assert!(
            detectadas.is_empty(),
            "la URL está más allá del límite de muestreo por columna: no debería detectarse"
        );
    }
    #[test]
    fn detectar_columnas_url_avisa_y_se_rinde_si_excede_el_limite_total_de_lecturas() {
        // El límite por columna acota el costo de CADA columna, pero no el
        // TOTAL: con muchas columnas vacías (cada una cuesta ~10 000
        // lecturas antes de rendirse), el análisis podía tardar minutos ante
        // una hoja con geometría manipulada. Acá se registra una geometría
        // grande (max_row alto, vía una celda lejana que no cae dentro del
        // rango muestreado) y una URL bien pasada la columna 200 (donde el
        // presupuesto total ya se agotó con columnas vacías anteriores):
        // debe rendirse ANTES de llegar a ella, avisando, y sin colgarse.
        let mut libro = libro_de_prueba();
        let ws = libro.get_sheet_by_name_mut("Sheet1").unwrap();
        let fila_lejana = MAX_FILAS_MUESTREADAS_POR_COLUMNA + 100;
        ws.get_cell_mut((1, fila_lejana)).set_value("x");
        ws.get_cell_mut((300, 2)).set_value("http://x.com/1.jpg");

        let embedder = ImageEmbedder::new(ImageEmbedConfig::default(), DownloadConfig::default(), 4);
        let avisos = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let avisos_clon = avisos.clone();
        let inicio = std::time::Instant::now();
        let detectadas =
            embedder.detectar_columnas_url(ws, &mut |m: &str| avisos_clon.borrow_mut().push(m.to_string()));
        assert!(
            inicio.elapsed() < std::time::Duration::from_secs(30),
            "debe rendirse dentro del presupuesto total de lecturas, no escanear todas las columnas"
        );
        assert!(
            detectadas.is_empty(),
            "la columna con URL está más allá del presupuesto total: no debería detectarse"
        );
        assert!(
            !avisos.borrow().is_empty(),
            "debe avisar que se alcanzó el límite de lecturas, no rendirse en silencio"
        );
    }
    #[test]
    fn insertar_imagenes_escribe_texto_de_error_cuando_la_descarga_fallo() {
        let mut libro = libro_de_prueba();
        let ws = libro.get_sheet_by_name_mut("Sheet1").unwrap();
        let cfg = ImageEmbedConfig::default();
        let embedder = ImageEmbedder::new(ImageEmbedConfig::default(), DownloadConfig::default(), 4);

        let tareas = vec![TareaImagen {
            fila_excel: 2,
            col_destino: 3,
            url: "http://x.com/no-existe.jpg".to_string(),
        }];
        let imagenes: Vec<Option<RgbImage>> = vec![None];

        let (exitos, fallos) = embedder.insertar_imagenes(ws, &tareas, &imagenes);
        assert_eq!((exitos, fallos), (0, 1));
        assert_eq!(ws.get_value((3, 2)), cfg.texto_error);
        // Una tarea fallida no debe dejar la fila/columna con el alto/ancho
        // de la imagen que nunca se insertó. La comparación es contra el
        // valor derivado de la imagen y no contra 0.0 porque `get_cell_mut`
        // crea la entrada con el default interno de `umya-spreadsheet` como
        // efecto secundario de tocar la celda.
        let alto_de_imagen = px_a_puntos(cfg.alto_px);
        let ancho_de_imagen = px_a_ancho_columna(cfg.ancho_px);
        assert_ne!(
            *ws.get_row_dimension(&2).unwrap().get_height(),
            alto_de_imagen,
            "una fila sin imagen insertada no debe tomar el alto pensado para una imagen"
        );
        assert_ne!(
            *ws.get_column_dimension_by_number(&3).unwrap().get_width(),
            ancho_de_imagen,
            "una columna sin imagen insertada no debe tomar el ancho pensado para una imagen"
        );
    }
    #[test]
    fn insertar_imagenes_incrusta_y_ajusta_dimensiones() {
        let mut libro = libro_de_prueba();
        let ws = libro.get_sheet_by_name_mut("Sheet1").unwrap();
        let embedder = ImageEmbedder::new(ImageEmbedConfig::default(), DownloadConfig::default(), 4);

        let tareas = vec![TareaImagen {
            fila_excel: 3,
            col_destino: 4,
            url: "http://x.com/1.jpg".to_string(),
        }];
        let imagenes = vec![Some(RgbImage::from_pixel(118, 168, image::Rgb([10, 20, 30])))];

        let (exitos, fallos) = embedder.insertar_imagenes(ws, &tareas, &imagenes);
        assert_eq!((exitos, fallos), (1, 0));
        assert_eq!(ws.get_image_collection().len(), 1);
        assert_eq!(*ws.get_row_dimension(&3).unwrap().get_height(), 126.0);
        assert!((ws.get_column_dimension_by_number(&4).unwrap().get_width() - (118.0 / 7.0)).abs() < 1e-9);
    }
    #[tokio::test]
    async fn procesar_archivo_con_archivo_corrupto_devuelve_ese_motivo() {
        let tmp = tempfile::tempdir().unwrap();
        let origen = tmp.path().join("corrupto.xlsx");
        std::fs::write(&origen, b"esto no es un xlsx").unwrap();

        let embedder = ImageEmbedder::new(ImageEmbedConfig::default(), DownloadConfig::default(), 4);
        let destino = tmp.path().join("con_imagenes.xlsx");
        let error = embedder
            .procesar_archivo(&origen, &HashSet::new(), &destino, |_| {})
            .await
            .expect_err("un archivo corrupto no debe procesarse");
        assert!(matches!(error, MotivoSinProcesar::ArchivoCorrupto));
    }
    #[tokio::test]
    async fn procesar_archivo_sin_columnas_de_url_devuelve_ese_motivo() {
        let tmp = tempfile::tempdir().unwrap();
        let origen = tmp.path().join("sin_urls.xlsx");
        {
            use rust_xlsxwriter::Workbook;
            let mut wb = Workbook::new();
            let hoja = wb.add_worksheet();
            hoja.write(0, 0, "Sku").unwrap();
            hoja.write(1, 0, "A1").unwrap();
            wb.save(&origen).unwrap();
        }

        let embedder = ImageEmbedder::new(ImageEmbedConfig::default(), DownloadConfig::default(), 4);
        let destino = tmp.path().join("con_imagenes.xlsx");
        let error = embedder
            .procesar_archivo(&origen, &HashSet::new(), &destino, |_| {})
            .await
            .expect_err("sin columnas de URL no debe procesarse");
        assert!(matches!(error, MotivoSinProcesar::SinColumnasDeUrl));
    }
    #[tokio::test]
    async fn procesar_archivo_e2e_descarga_e_inserta_una_imagen_real() {
        let tmp = tempfile::tempdir().unwrap();
        let origen = tmp.path().join("origen.xlsx");
        {
            use rust_xlsxwriter::Workbook;
            let mut wb = Workbook::new();
            let hoja = wb.add_worksheet();
            hoja.write(0, 0, "Sku").unwrap();
            hoja.write(0, 1, "Fotos").unwrap();
            hoja.write(1, 0, "A1").unwrap();
            hoja.write(1, 1, servidor_mock_imagen()).unwrap();
            wb.save(&origen).unwrap();
        }

        let embedder = ImageEmbedder::new(ImageEmbedConfig::default(), DownloadConfig::default(), 4);
        let destino = tmp.path().join("con_imagenes.xlsx");
        let (ruta, exitos, fallos, hojas) = embedder
            .procesar_archivo(&origen, &HashSet::new(), &destino, |_| {})
            .await
            .expect("debe procesar al menos una hoja");

        assert_eq!((exitos, fallos, hojas), (1, 0, 1));

        let releido = xlsx::reader::xlsx::read(&ruta).unwrap();
        let hoja = releido.get_sheet(&0).unwrap();
        assert_eq!(hoja.get_image_collection().len(), 1);
    }
    #[tokio::test]
    async fn dos_urls_distintas_producen_dos_imagenes_distintas_en_el_xlsx() {
        // Regresión: todas las imágenes se incrustaban con el nombre fijo
        // "img.png", que `umya-spreadsheet` usa como ruta real dentro del
        // paquete (`xl/media/img.png`). Su escritor NO sobrescribe una
        // entrada ya existente (`add_bin` → `check_file_exist`), así que solo
        // se guardaba la PRIMERA imagen y todas las celdas apuntaban a ella:
        // el usuario veía la misma foto repetida pese a tener URLs distintas
        // y descargas correctas.
        let tmp = tempfile::tempdir().unwrap();
        let origen = tmp.path().join("origen.xlsx");
        {
            use rust_xlsxwriter::Workbook;
            let mut wb = Workbook::new();
            let hoja = wb.add_worksheet();
            hoja.write(0, 0, "Sku").unwrap();
            hoja.write(0, 1, "Fotos").unwrap();
            hoja.write(1, 0, "A1").unwrap();
            hoja.write(1, 1, servidor_mock_imagen_color([255, 0, 0])).unwrap();
            hoja.write(2, 0, "A2").unwrap();
            hoja.write(2, 1, servidor_mock_imagen_color([0, 0, 255])).unwrap();
            wb.save(&origen).unwrap();
        }

        let embedder = ImageEmbedder::new(ImageEmbedConfig::default(), DownloadConfig::default(), 4);
        let destino = tmp.path().join("con_imagenes.xlsx");
        let (ruta, exitos, fallos, _hojas) = embedder
            .procesar_archivo(&origen, &HashSet::new(), &destino, |_| {})
            .await
            .expect("debe procesar la hoja");
        assert_eq!((exitos, fallos), (2, 0), "ambas descargas deben salir bien");

        // Se inspecciona el .xlsx como zip, que es lo que abre Excel:
        // releerlo con la misma librería que lo escribió podría ocultar un
        // defecto que esté justo en la escritura del paquete.
        let archivo = std::fs::File::open(&ruta).unwrap();
        let mut zip = zip::ZipArchive::new(archivo).unwrap();
        let medias: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .filter(|n| n.starts_with("xl/media/"))
            .collect();
        assert_eq!(
            medias.len(),
            2,
            "deben quedar DOS archivos de imagen en el paquete, uno por URL: {medias:?}"
        );

        let mut contenidos: Vec<Vec<u8>> = Vec::new();
        for nombre in &medias {
            let mut entrada = zip.by_name(nombre).unwrap();
            let mut bytes = Vec::new();
            entrada.read_to_end(&mut bytes).unwrap();
            contenidos.push(bytes);
        }
        assert_ne!(
            contenidos[0], contenidos[1],
            "las dos imágenes deben tener contenido distinto (una roja y una azul), no la misma repetida"
        );
    }
}
