//! Descarga concurrente de imágenes vía HTTP: `AsyncImageDownloader` sobre
//! `reqwest`/`tokio`.
//!
//! `fetch` devuelve solo los bytes crudos: el decode/resize se deja fuera del
//! runtime async a propósito, porque decodificar una imagen es CPU-bound y
//! bloquearía el reactor si se hiciera inline — quien orqueste la descarga
//! debe correr [`decode_and_resize`] en un thread aparte (p. ej.
//! `tokio::task::spawn_blocking` o un `rayon`/thread-pool de CPU).
//!
//! Mitigación SSRF: las URLs vienen de un Excel de un tercero, nunca de una
//! fuente confiable. `fetch` resuelve el host ANTES de conectar y rechaza IPs
//! privadas/loopback/link-local (incluye el endpoint de metadata de nube
//! `169.254.169.254`) o reservadas; la política de redirects re-valida cada
//! salto con el mismo criterio. Nota de alcance: la comprobación y la
//! conexión real las hace `reqwest` por separado, así que un DNS rebinding
//! justo entre esos dos pasos no queda cubierto — aceptable para una
//! herramienta batch interna, no para un servicio multi-tenant expuesto.

use std::fmt;
use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use futures::StreamExt;
use image::{ImageReader, Limits, RgbImage};

/// Por qué falló una descarga. Antes `fetch` devolvía `Option<Vec<u8>>` y
/// TODAS estas causas colapsaban en un único `None`, que el llamador
/// reportaba con un texto fijo ("timeout o URL inválida") — apuntando a los
/// datos del usuario incluso cuando la causa real era del servidor remoto
/// (rate-limit, 404) o del propio filtro de seguridad. Con un archivo de
/// URLs perfectamente válidas eso mandaba a revisar el lugar equivocado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FalloDescarga {
    /// El texto de la celda no se puede parsear como URL.
    UrlInvalida,
    /// Esquema distinto de http(s), o el host resuelve a una IP
    /// privada/reservada (filtro anti-SSRF de este módulo).
    HostBloqueado,
    /// No respondió dentro de `DownloadConfig::timeout`, en ningún intento.
    Timeout,
    /// Error de red/DNS/TLS en todos los intentos.
    ErrorDeRed,
    /// Respondió con un status distinto de 200.
    Http(u16),
    /// Respondió 200 pero declarando un `Content-Type` que no es de imagen
    /// (típico: una página de error HTML servida con status 200).
    NoEsImagen,
    /// El cuerpo excede [`MAX_RESPUESTA_BYTES`].
    DemasiadoGrande,
    /// La conexión se cortó a mitad del cuerpo.
    CuerpoIncompleto,
}

impl fmt::Display for FalloDescarga {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UrlInvalida => write!(f, "URL mal formada"),
            Self::HostBloqueado => {
                write!(
                    f,
                    "host bloqueado (no es http/https, o resuelve a una IP privada)"
                )
            }
            Self::Timeout => write!(f, "el servidor no respondió a tiempo"),
            Self::ErrorDeRed => write!(f, "no se pudo conectar con el servidor"),
            Self::Http(codigo) => write!(f, "el servidor respondió HTTP {codigo}"),
            Self::NoEsImagen => write!(f, "la respuesta no es una imagen"),
            Self::DemasiadoGrande => write!(f, "la imagen supera el límite de tamaño"),
            Self::CuerpoIncompleto => write!(f, "la descarga se cortó a mitad"),
        }
    }
}

/// Cota de tamaño de una respuesta HTTP: ninguna imagen de producto real
/// debería acercarse a esto; es un límite duro ante un servidor que mienta
/// el `Content-Length` o lo omita (si no, habría que bufferizar una
/// respuesta arbitrariamente grande en memoria antes de poder rechazarla).
const MAX_RESPUESTA_BYTES: usize = 25 * 1024 * 1024;

/// Máximo de saltos de redirect que se siguen; cada uno se revalida contra
/// el mismo criterio anti-SSRF que la conexión inicial.
const MAX_REDIRECTS: usize = 5;

/// Límites de decodificación anti bomba-de-descompresión: un archivo chico
/// puede declarar dimensiones enormes y forzar una asignación
/// desproporcionada al decodificar, ANTES de que `resize_max_dim` entre en
/// juego (el resize solo actúa post-decode).
const MAX_IMAGE_DIM: u32 = 20_000;
const MAX_IMAGE_ALLOC: u64 = 128 * 1024 * 1024;

/// `true` si `ip` es privada/loopback/link-local/multicast/reservada — no
/// debería ser alcanzable desde una URL provista por un tercero.
fn ip_es_privada(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_documentation()
                || {
                    // RFC 6598: 100.64.0.0/10 (CGNAT / espacio de direcciones compartido).
                    let o = v4.octets();
                    o[0] == 100 && (64..=127).contains(&o[1])
                }
        }
        IpAddr::V6(v6) => {
            if let Some(mapeada) = v6.to_ipv4_mapped() {
                return ip_es_privada(IpAddr::V4(mapeada));
            }
            v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || {
                let s = v6.segments()[0];
                (s & 0xfe00) == 0xfc00 // fc00::/7 (unique local)
                        || (s & 0xffc0) == 0xfe80 // fe80::/10 (link-local)
            }
        }
    }
}

fn direcciones_seguras(direcciones: impl Iterator<Item = std::net::SocketAddr>) -> bool {
    let direcciones: Vec<_> = direcciones.collect();
    !direcciones.is_empty() && direcciones.iter().all(|s| !ip_es_privada(s.ip()))
}

/// Cota de tiempo para la resolución DNS síncrona de [`destino_es_seguro`]:
/// sin esto, un host con DNS lento o que nunca responda bloqueaba
/// indefinidamente el hilo que atiende ese salto de redirect (potencialmente
/// un worker de tokio — con varios redirects concurrentes a hosts lentos,
/// riesgo de agotar el pool). Se resuelve en un hilo aparte con este límite;
/// si no responde a tiempo, se trata igual que cualquier otro fallo de DNS
/// (destino inseguro, falla cerrado) — el hilo de resolución queda corriendo
/// en segundo plano hasta que el propio DNS del SO lo resuelva o expire.
const TIMEOUT_DNS_REDIRECT: Duration = Duration::from_secs(5);

fn resolver_con_timeout(host: &str, puerto: u16) -> Option<Vec<std::net::SocketAddr>> {
    let host = host.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let resultado = (host.as_str(), puerto)
            .to_socket_addrs()
            .map(|it| it.collect::<Vec<_>>());
        let _ = tx.send(resultado);
    });
    rx.recv_timeout(TIMEOUT_DNS_REDIRECT).ok()?.ok()
}

/// Rechaza esquemas != http(s) y hosts sin resolución. Bloqueante (con cota
/// de tiempo, ver [`resolver_con_timeout`]): pensada para la política de
/// redirects de `reqwest`, que no admite una revalidación async — un salto
/// de redirect es una operación rara y acotada (`reqwest` ya cortó todos los
/// que no sean 3xx), así que el costo es aceptable acá.
fn destino_es_seguro(url: &reqwest::Url) -> bool {
    if url.scheme() != "http" && url.scheme() != "https" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    if let Ok(ip) = host.parse::<IpAddr>() {
        return !ip_es_privada(ip);
    }
    let puerto = url.port_or_known_default().unwrap_or(443);
    resolver_con_timeout(host, puerto)
        .map(|direcciones| direcciones_seguras(direcciones.into_iter()))
        .unwrap_or(false)
}

/// Igual criterio que [`destino_es_seguro`], pero con resolución de DNS no
/// bloqueante — para la conexión inicial, donde sí hay runtime async a mano.
async fn host_es_publico(url: &reqwest::Url) -> bool {
    if url.scheme() != "http" && url.scheme() != "https" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    if let Ok(ip) = host.parse::<IpAddr>() {
        return !ip_es_privada(ip);
    }
    let puerto = url.port_or_known_default().unwrap_or(443);
    match tokio::net::lookup_host((host, puerto)).await {
        Ok(direcciones) => direcciones_seguras(direcciones),
        Err(_) => false,
    }
}

#[derive(Clone)]
pub struct DownloadConfig {
    pub timeout: Duration,
    pub retries: u32,
    pub pool_maxsize: usize,
    pub user_agent: String,
    pub backoff: Duration,
    /// Si la imagen decodificada excede esta cota (lado mayor) se reduce.
    /// `None` desactiva el resize.
    pub resize_max_dim: Option<u32>,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs_f64(15.0),
            retries: 2,
            pool_maxsize: 64,
            user_agent: "Mozilla/5.0 AutoparteQC/2.0".to_string(),
            backoff: Duration::from_secs_f64(0.5),
            resize_max_dim: Some(1280),
        }
    }
}

pub struct AsyncImageDownloader {
    client: reqwest::Client,
    cfg: DownloadConfig,
    /// Únicamente para tests: el servidor mock local corre en 127.0.0.1, que
    /// el filtro anti-SSRF rechazaría igual que cualquier otro loopback. En
    /// producción esto siempre es `false` y no hay forma de cambiarlo desde
    /// `DownloadConfig` (`pub`, ver dudas de un caller real desactivándolo
    /// sin querer) — el campo ni siquiera existe fuera de `cfg(test)`/la
    /// feature `test-support` (nunca activa en un build normal).
    #[cfg(any(test, feature = "test-support"))]
    permitir_hosts_privados: bool,
}

impl AsyncImageDownloader {
    pub fn new(cfg: DownloadConfig) -> reqwest::Result<Self> {
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(64) // limit_per_host fijo del original
            .pool_idle_timeout(Duration::from_secs(300)) // analogo a ttl_dns_cache=300
            .timeout(cfg.timeout)
            .user_agent(cfg.user_agent.clone())
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS || !destino_es_seguro(attempt.url()) {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()?;
        Ok(Self {
            client,
            cfg,
            #[cfg(any(test, feature = "test-support"))]
            permitir_hosts_privados: false,
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    fn con_hosts_privados_permitidos(mut self) -> Self {
        self.permitir_hosts_privados = true;
        self
    }

    /// Únicamente para tests (de este crate o de `tests/`, vía la feature
    /// `test-support` — ver `Cargo.toml`) que necesitan un downloader
    /// apuntando al servidor mock local. Nunca existe en un build normal.
    #[cfg(any(test, feature = "test-support"))]
    pub fn nuevo_para_test_con_hosts_privados_permitidos(cfg: DownloadConfig) -> reqwest::Result<Self> {
        Self::new(cfg).map(Self::con_hosts_privados_permitidos)
    }

    /// Los bytes crudos, o [`FalloDescarga`] con la causa REAL del fallo
    /// (ver ahí por qué importa distinguirlas). 5xx transitorio y errores de
    /// red se reintentan con backoff `cfg.backoff * (intento + 1)`; el resto
    /// corta de inmediato. Cuando se agotan los reintentos se reporta el
    /// último fallo visto, no uno genérico.
    pub async fn fetch(&self, url: &str) -> Result<Vec<u8>, FalloDescarga> {
        let url = url.trim();
        let parsed = reqwest::Url::parse(url).map_err(|_| FalloDescarga::UrlInvalida)?;
        #[cfg(any(test, feature = "test-support"))]
        let host_ok = self.permitir_hosts_privados || host_es_publico(&parsed).await;
        #[cfg(not(any(test, feature = "test-support")))]
        let host_ok = host_es_publico(&parsed).await;
        if !host_ok {
            return Err(FalloDescarga::HostBloqueado);
        }
        // Se arrastra el último fallo concreto entre reintentos: si se agotan
        // todos, el llamador recibe QUÉ pasó en el último intento (timeout,
        // 503, red caída) en vez de una causa inventada por defecto.
        let mut ultimo_fallo = FalloDescarga::ErrorDeRed;
        for intento in 0..=self.cfg.retries {
            match self.client.get(parsed.clone()).send().await {
                Ok(resp) if resp.status() == reqwest::StatusCode::OK => {
                    if !content_type_es_imagen_o_desconocido(&resp) {
                        // No es reintentable (no es un problema transitorio):
                        // el servidor respondió bien, solo que con algo que
                        // no es una imagen (típico: una página de error HTML
                        // con status 200). Cortar acá evita bufferizar hasta
                        // `MAX_RESPUESTA_BYTES` de algo que `decode_and_resize`
                        // iba a rechazar de todas formas más adelante.
                        return Err(FalloDescarga::NoEsImagen);
                    }
                    return leer_bytes_acotado(resp).await;
                }
                Ok(resp) => {
                    let codigo = resp.status().as_u16();
                    ultimo_fallo = FalloDescarga::Http(codigo);
                    let reintentable = matches!(codigo, 500 | 502 | 503 | 504);
                    if !reintentable || intento >= self.cfg.retries {
                        return Err(ultimo_fallo);
                    }
                }
                Err(error) => {
                    ultimo_fallo = if error.is_timeout() {
                        FalloDescarga::Timeout
                    } else {
                        FalloDescarga::ErrorDeRed
                    };
                    if intento >= self.cfg.retries {
                        return Err(ultimo_fallo);
                    }
                }
            }
            tokio::time::sleep(self.cfg.backoff * (intento + 1)).await;
        }
        Err(ultimo_fallo)
    }
}

/// `false` solo si el `Content-Type` declarado NO es una imagen (p. ej.
/// `text/html` de una página de error, `application/json`, etc.). Sin
/// header, o con un valor no-ASCII/ilegible, se deja pasar (`true`): no hay
/// con qué descartar temprano con certeza, y algunos servidores de imágenes
/// reales tampoco lo declaran bien — el chequeo real de fondo sigue siendo
/// `decode_and_resize`, esto es solo para no bufferizar de más un cuerpo que
/// ya se sabe que no es una imagen.
fn content_type_es_imagen_o_desconocido(resp: &reqwest::Response) -> bool {
    match resp.headers().get(reqwest::header::CONTENT_TYPE) {
        Some(valor) => valor
            .to_str()
            .map(|s| s.trim_start().to_lowercase().starts_with("image/"))
            .unwrap_or(true),
        None => true,
    }
}

/// Lee el cuerpo en streaming, cortando ante `Content-Length` declarado o
/// bytes reales que excedan `MAX_RESPUESTA_BYTES`.
async fn leer_bytes_acotado(resp: reqwest::Response) -> Result<Vec<u8>, FalloDescarga> {
    leer_bytes_acotado_con_cota(resp, MAX_RESPUESTA_BYTES).await
}

/// Núcleo de `leer_bytes_acotado`, parametrizado por la cota (en producción
/// siempre es `MAX_RESPUESTA_BYTES`; separado así para poder ejercitar el
/// corte con cuerpos de test chicos y rápidos).
async fn leer_bytes_acotado_con_cota(resp: reqwest::Response, cota: usize) -> Result<Vec<u8>, FalloDescarga> {
    if resp.content_length().is_some_and(|n| n as usize > cota) {
        return Err(FalloDescarga::DemasiadoGrande);
    }
    let mut buf = Vec::new();
    let mut stream = std::pin::pin!(resp.bytes_stream());
    while let Some(trozo) = stream.next().await {
        let trozo = trozo.map_err(|_| FalloDescarga::CuerpoIncompleto)?;
        if buf.len() + trozo.len() > cota {
            return Err(FalloDescarga::DemasiadoGrande);
        }
        buf.extend_from_slice(&trozo);
    }
    Ok(buf)
}

/// Decodifica bytes crudos a RGB y opcionalmente reduce a `resize_max_dim`
/// (lado mayor). `None` si los bytes no son una imagen válida o si exceden
/// los límites de `MAX_IMAGE_DIM`/`MAX_IMAGE_ALLOC` (bomba de descompresión:
/// un archivo chico que declara dimensiones desproporcionadas).
///
/// Pensado para correr fuera del runtime async (CPU-bound). Nota de
/// fidelidad: `cv2.resize(..., INTER_AREA)` no tiene equivalente directo en
/// `image`; se usa `FilterType::Triangle` (mismo criterio ya aplicado en
/// `text_detector::prepare_for_ocr`, sin fixture de referencia para exigir
/// paridad bit a bit aquí tampoco).
pub fn decode_and_resize(content: &[u8], resize_max_dim: Option<u32>) -> Option<RgbImage> {
    let mut lector = ImageReader::new(std::io::Cursor::new(content));
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIM);
    limits.max_image_height = Some(MAX_IMAGE_DIM);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC);
    lector.limits(limits);
    let img = lector.with_guessed_format().ok()?.decode().ok()?.to_rgb8();
    let Some(cap) = resize_max_dim else {
        return Some(img);
    };
    Some(crate::imgproc::resize_max_dim(&img, cap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Levanta un servidor HTTP mínimo en un thread aparte: `respuestas[i]`
    /// es la respuesta cruda (línea de estado + headers + body) devuelta en
    /// la petición i-ésima; la última respuesta se repite si hay más
    /// peticiones que respuestas configuradas.
    fn servidor_mock(respuestas: Vec<&'static str>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let puerto = listener.local_addr().unwrap().port();
        let contador = Arc::new(AtomicUsize::new(0));
        let contador_hilo = contador.clone();

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let i = contador_hilo.fetch_add(1, Ordering::SeqCst);
                let respuesta = respuestas.get(i).or_else(|| respuestas.last()).unwrap();
                let _ = stream.write_all(respuesta.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{puerto}/img.jpg"), contador)
    }

    fn cfg_rapida() -> DownloadConfig {
        DownloadConfig {
            timeout: Duration::from_secs(2),
            retries: 2,
            backoff: Duration::from_millis(5),
            ..DownloadConfig::default()
        }
    }

    #[tokio::test]
    async fn fetch_devuelve_los_bytes_en_200() {
        let (url, _) = servidor_mock(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhola!",
        ]);
        let dl = AsyncImageDownloader::new(cfg_rapida())
            .unwrap()
            .con_hosts_privados_permitidos();
        let bytes = dl.fetch(&url).await.expect("debe traer bytes");
        assert_eq!(bytes, b"hola!");
    }

    #[tokio::test]
    async fn fetch_rechaza_content_type_que_no_es_imagen_sin_reintentar() {
        // Antes: se bufferizaba el cuerpo entero (hasta MAX_RESPUESTA_BYTES)
        // de un 200 con Content-Type no-imagen (p. ej. una página de error
        // HTML) antes de que `decode_and_resize` lo rechazara más adelante.
        let (url, contador) = servidor_mock(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 13\r\nConnection: close\r\n\r\n<html></html>",
        ]);
        let dl = AsyncImageDownloader::new(cfg_rapida())
            .unwrap()
            .con_hosts_privados_permitidos();
        assert_eq!(dl.fetch(&url).await, Err(FalloDescarga::NoEsImagen));
        assert_eq!(
            contador.load(Ordering::SeqCst),
            1,
            "un Content-Type no-imagen no es un fallo transitorio: no debe reintentar"
        );
    }

    #[tokio::test]
    async fn fetch_acepta_content_type_de_imagen_explicito() {
        let (url, _) = servidor_mock(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhola!",
        ]);
        let dl = AsyncImageDownloader::new(cfg_rapida())
            .unwrap()
            .con_hosts_privados_permitidos();
        let bytes = dl.fetch(&url).await.expect("Content-Type de imagen debe pasar");
        assert_eq!(bytes, b"hola!");
    }

    #[tokio::test]
    async fn fetch_no_reintenta_en_404() {
        let (url, contador) = servidor_mock(vec![
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ]);
        let dl = AsyncImageDownloader::new(cfg_rapida())
            .unwrap()
            .con_hosts_privados_permitidos();
        assert_eq!(
            dl.fetch(&url).await,
            Err(FalloDescarga::Http(404)),
            "el motivo debe conservar el status real, no una causa genérica"
        );
        assert_eq!(contador.load(Ordering::SeqCst), 1, "404 no debe reintentar");
    }

    #[tokio::test]
    async fn fetch_reintenta_en_500_transitorio_y_se_recupera() {
        let (url, contador) = servidor_mock(vec![
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
        ]);
        let dl = AsyncImageDownloader::new(cfg_rapida())
            .unwrap()
            .con_hosts_privados_permitidos();
        let bytes = dl.fetch(&url).await.expect("debe recuperarse en el reintento");
        assert_eq!(bytes, b"ok");
        assert_eq!(contador.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn fetch_agota_reintentos_y_reporta_el_ultimo_fallo_visto() {
        let (url, contador) = servidor_mock(vec![
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ]);
        let cfg = DownloadConfig {
            retries: 1,
            ..cfg_rapida()
        };
        let dl = AsyncImageDownloader::new(cfg)
            .unwrap()
            .con_hosts_privados_permitidos();
        assert_eq!(
            dl.fetch(&url).await,
            Err(FalloDescarga::Http(503)),
            "agotar reintentos debe reportar el ÚLTIMO fallo real (503), no una causa por defecto"
        );
        assert_eq!(
            contador.load(Ordering::SeqCst),
            2,
            "1 intento inicial + 1 reintento"
        );
    }

    fn png_de_prueba(w: u32, h: u32) -> Vec<u8> {
        let img = RgbImage::from_pixel(w, h, image::Rgb([10, 20, 30]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn decode_and_resize_bytes_invalidos_devuelve_none() {
        assert!(decode_and_resize(b"esto no es una imagen", None).is_none());
    }

    #[test]
    fn decode_and_resize_sin_cota_preserva_el_tamano() {
        let bytes = png_de_prueba(50, 30);
        let img = decode_and_resize(&bytes, None).unwrap();
        assert_eq!(img.dimensions(), (50, 30));
    }

    #[test]
    fn decode_and_resize_reduce_solo_si_excede_la_cota() {
        let bytes = png_de_prueba(2000, 1000);
        let img = decode_and_resize(&bytes, Some(1000)).unwrap();
        assert_eq!(img.dimensions().0, 1000);
        assert_eq!(img.dimensions().1, 500);

        let bytes_chica = png_de_prueba(100, 50);
        let img_chica = decode_and_resize(&bytes_chica, Some(1000)).unwrap();
        assert_eq!(img_chica.dimensions(), (100, 50));
    }

    #[test]
    fn decode_and_resize_rechaza_dimensiones_que_exceden_max_image_dim() {
        // Imagen real y válida (comprime casi nada por ser de color sólido),
        // pero con un ancho que excede MAX_IMAGE_DIM: mismo camino que
        // protege contra una bomba de descompresión (archivo chico,
        // dimensiones declaradas desproporcionadas).
        let bytes = png_de_prueba(MAX_IMAGE_DIM + 1, 2);
        assert!(decode_and_resize(&bytes, None).is_none());
    }

    #[test]
    fn ip_privada_reconoce_loopback_link_local_y_metadata_de_nube() {
        assert!(ip_es_privada("127.0.0.1".parse().unwrap()));
        assert!(
            ip_es_privada("169.254.169.254".parse().unwrap()),
            "metadata de nube"
        );
        assert!(ip_es_privada("10.0.0.5".parse().unwrap()));
        assert!(ip_es_privada("192.168.1.1".parse().unwrap()));
        assert!(ip_es_privada("172.16.0.1".parse().unwrap()));
        assert!(ip_es_privada("100.64.0.1".parse().unwrap()), "CGNAT RFC 6598");
        assert!(ip_es_privada("0.0.0.0".parse().unwrap()));
        assert!(ip_es_privada("::1".parse().unwrap()), "loopback IPv6");
        assert!(ip_es_privada("fe80::1".parse().unwrap()), "link-local IPv6");
        assert!(ip_es_privada("fc00::1".parse().unwrap()), "unique local IPv6");
        assert!(!ip_es_privada("8.8.8.8".parse().unwrap()));
        assert!(!ip_es_privada("1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn resolver_con_timeout_resuelve_localhost() {
        // Ninguno de los tests de `destino_es_seguro` de más abajo pasa por
        // acá: todos usan literales de IP (rama temprana que ni llega a
        // resolver DNS). "localhost" sí ejercita `resolver_con_timeout` de
        // punta a punta (hilo aparte + `recv_timeout`) sin depender de red
        // real: resuelve por loopback, no por un servidor DNS externo.
        let direcciones = resolver_con_timeout("localhost", 80);
        assert!(
            direcciones.is_some_and(|d| !d.is_empty()),
            "localhost debe resolver a al menos una dirección"
        );
    }

    #[test]
    fn destino_es_seguro_rechaza_esquemas_no_http() {
        assert!(!destino_es_seguro(
            &reqwest::Url::parse("ftp://8.8.8.8/x").unwrap()
        ));
        assert!(!destino_es_seguro(
            &reqwest::Url::parse("file:///etc/passwd").unwrap()
        ));
    }

    #[test]
    fn destino_es_seguro_rechaza_ip_literal_privada_y_acepta_publica() {
        assert!(!destino_es_seguro(
            &reqwest::Url::parse("http://127.0.0.1/x").unwrap()
        ));
        assert!(!destino_es_seguro(
            &reqwest::Url::parse("http://169.254.169.254/latest/meta-data").unwrap()
        ));
        assert!(destino_es_seguro(
            &reqwest::Url::parse("http://8.8.8.8/x").unwrap()
        ));
    }

    #[tokio::test]
    async fn fetch_sin_bypass_de_test_rechaza_el_servidor_mock_por_ser_loopback() {
        // Prueba la ruta de PRODUCCIÓN (sin con_hosts_privados_permitidos):
        // aunque el servidor responda 200 con normalidad, el filtro anti-SSRF
        // debe cortar antes de conectar porque 127.0.0.1 es loopback.
        let (url, contador) = servidor_mock(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhola!",
        ]);
        let dl = AsyncImageDownloader::new(cfg_rapida()).unwrap();
        assert_eq!(
            dl.fetch(&url).await,
            Err(FalloDescarga::HostBloqueado),
            "el motivo debe decir que lo bloqueó el filtro, no que la URL sea inválida"
        );
        assert_eq!(
            contador.load(Ordering::SeqCst),
            0,
            "no debe siquiera conectar a un host privado"
        );
    }

    #[tokio::test]
    async fn leer_bytes_acotado_corta_si_el_cuerpo_excede_la_cota() {
        let cuerpo = "x".repeat(100);
        let respuesta: &'static str = Box::leak(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{cuerpo}",
                cuerpo.len()
            )
            .into_boxed_str(),
        );
        let (url, _) = servidor_mock(vec![respuesta]);
        let cliente = reqwest::Client::new();

        let resp = cliente.get(&url).send().await.unwrap();
        assert_eq!(
            leer_bytes_acotado_con_cota(resp, 10).await,
            Err(FalloDescarga::DemasiadoGrande),
            "un cuerpo de 100 bytes debe rechazarse con una cota de 10"
        );

        let resp = cliente.get(&url).send().await.unwrap();
        assert!(
            leer_bytes_acotado_con_cota(resp, 1000).await.is_ok(),
            "el mismo cuerpo debe aceptarse con una cota generosa"
        );
    }

    #[tokio::test]
    async fn fetch_distingue_una_url_mal_formada_de_un_fallo_de_red() {
        // El caso que motivó todo esto: con un archivo de URLs VÁLIDAS, el
        // texto fijo "timeout o URL inválida" mandaba a revisar los datos
        // cuando el problema estaba del lado del servidor. Cada causa debe
        // reportarse por separado.
        let dl = AsyncImageDownloader::new(cfg_rapida())
            .unwrap()
            .con_hosts_privados_permitidos();
        assert_eq!(dl.fetch("no soy una url").await, Err(FalloDescarga::UrlInvalida));
    }

    #[test]
    fn cada_fallo_tiene_un_mensaje_propio_y_legible() {
        // Ningún motivo debe quedar vacío ni repetido: el operador tiene que
        // poder distinguir a simple vista qué le pasó a cada imagen.
        let todos = [
            FalloDescarga::UrlInvalida,
            FalloDescarga::HostBloqueado,
            FalloDescarga::Timeout,
            FalloDescarga::ErrorDeRed,
            FalloDescarga::Http(404),
            FalloDescarga::NoEsImagen,
            FalloDescarga::DemasiadoGrande,
            FalloDescarga::CuerpoIncompleto,
        ];
        let mensajes: Vec<String> = todos.iter().map(|f| f.to_string()).collect();
        assert!(mensajes.iter().all(|m| !m.trim().is_empty()));
        let unicos: std::collections::HashSet<&String> = mensajes.iter().collect();
        assert_eq!(unicos.len(), mensajes.len(), "mensajes duplicados: {mensajes:?}");
        assert!(
            FalloDescarga::Http(503).to_string().contains("503"),
            "el status real debe aparecer en el mensaje"
        );
    }
}
