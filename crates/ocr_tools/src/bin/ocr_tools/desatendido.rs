//! Modo no interactivo: analizar sin que nadie conteste preguntas.
//!
//! Existe por una razón operativa concreta. Una corrida de millones de
//! imágenes vive en instancias spot, que AWS puede reclamar en cualquier
//! momento. El proceso muere, la instancia vuelve, y hay que empezar de
//! nuevo — pero el menú interactivo se queda esperando una tecla que en un
//! servidor desatendido nadie va a apretar.
//!
//! Con este modo, un `systemd` con `Restart=always` reanuda solo: el
//! checkpoint saltea lo ya procesado y solo se repite el lote en vuelo.
//!
//! El parseo es a mano y no con `clap` a propósito: son cinco banderas, y una
//! dependencia nueva en el árbol se paga en cada compilación de cada crate.

use std::path::PathBuf;

/// Lo que se puede fijar desde la línea de comandos.
pub(crate) struct Opciones {
    pub archivos: Vec<PathBuf>,
    /// Columnas de URL a usar. `None` = detectarlas solo, sin preguntar.
    pub columnas: Option<Vec<String>>,
    pub salida: Option<PathBuf>,
    pub rechazadas_solo: bool,
}

/// Qué salió mal al leer los argumentos.
///
/// Un enum y no un `String` porque el llamador decide distinto según el
/// caso: pedir ayuda es una salida exitosa, un argumento inválido no.
pub(crate) enum FalloArgumentos {
    /// Se pidió `--ayuda`: hay que imprimirla y salir con éxito.
    PidioAyuda,
    /// Argumento desconocido, o falta el valor de uno que lo exige.
    Invalido(String),
}

pub(crate) const AYUDA: &str = "\
ocr_tools — analizar imágenes por su contenido

MODO INTERACTIVO
  ocr_tools                       menú guiado (lo habitual en escritorio)

MODO DESATENDIDO
  ocr_tools --archivo <ruta> [opciones]

  --archivo <ruta>          archivo .xlsx a analizar (repetible)
  --columnas <a,b>          columnas de URL; sin esto se detectan solas
  --salida <carpeta>        dónde escribir resultados y checkpoint
  --rechazadas-solo         escribir solo el archivo de rechazadas
  --ayuda                   esta ayuda

Pensado para servidores: no hace ninguna pregunta, así que puede correr bajo
systemd o nohup y reanudarse solo tras una interrupción de spot. El progreso
va al checkpoint de la carpeta de salida; relanzar el mismo comando saltea lo
ya procesado.

  ocr_tools --archivo bloque1.xlsx --salida /datos/salida --rechazadas-solo
";

/// Lee los argumentos. `Ok(None)` = sin argumentos, va el menú interactivo.
pub(crate) fn parsear<I>(args: I) -> Result<Option<Opciones>, FalloArgumentos>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter().peekable();
    if args.peek().is_none() {
        return Ok(None);
    }

    let mut opciones = Opciones {
        archivos: Vec::new(),
        columnas: None,
        salida: None,
        rechazadas_solo: false,
    };

    while let Some(arg) = args.next() {
        // El valor se pide acá y no en cada rama para que "falta el valor"
        // se reporte igual en todas.
        let mut valor = |bandera: &str| {
            args.next()
                .ok_or_else(|| FalloArgumentos::Invalido(format!("{bandera} necesita un valor")))
        };
        match arg.as_str() {
            "--ayuda" | "--help" | "-h" => return Err(FalloArgumentos::PidioAyuda),
            "--archivo" => opciones.archivos.push(PathBuf::from(valor("--archivo")?)),
            "--columnas" => opciones.columnas = Some(separar_lista(&valor("--columnas")?)),
            "--salida" => opciones.salida = Some(PathBuf::from(valor("--salida")?)),
            "--rechazadas-solo" => opciones.rechazadas_solo = true,
            otro => {
                return Err(FalloArgumentos::Invalido(format!(
                    "argumento desconocido: {otro}"
                )))
            }
        }
    }

    if opciones.archivos.is_empty() {
        return Err(FalloArgumentos::Invalido(
            "hace falta al menos un --archivo".to_string(),
        ));
    }
    Ok(Some(opciones))
}

/// Separa `"a, b ,c"` en tres elementos, descartando los vacíos.
fn separar_lista(crudo: &str) -> Vec<String> {
    crudo
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsear_str(args: &[&str]) -> Result<Option<Opciones>, FalloArgumentos> {
        parsear(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn sin_argumentos_va_el_menu_interactivo() {
        assert!(parsear_str(&[]).is_ok_and(|o| o.is_none()));
    }

    #[test]
    fn se_pueden_pasar_varios_archivos() {
        let o = parsear_str(&["--archivo", "a.xlsx", "--archivo", "b.xlsx"])
            .ok()
            .flatten()
            .expect("debe parsear");
        assert_eq!(o.archivos.len(), 2);
    }

    #[test]
    fn las_listas_toleran_espacios_y_elementos_vacios() {
        let o = parsear_str(&["--archivo", "a.xlsx", "--columnas", " Fotos , Img ,"])
            .ok()
            .flatten()
            .expect("debe parsear");
        assert_eq!(
            o.columnas.as_deref(),
            Some(["Fotos".to_string(), "Img".to_string()].as_slice())
        );
    }

    #[test]
    fn sin_columnas_se_detectan_solas() {
        // `None` no es "ninguna columna": es "detectalas vos". La distinción
        // importa porque en desatendido no hay a quién preguntarle.
        let o = parsear_str(&["--archivo", "a.xlsx"])
            .ok()
            .flatten()
            .expect("parsea");
        assert!(o.columnas.is_none());
    }

    #[test]
    fn una_bandera_sin_su_valor_es_error_y_no_un_panic() {
        assert!(matches!(
            parsear_str(&["--archivo"]),
            Err(FalloArgumentos::Invalido(_))
        ));
    }

    #[test]
    fn un_argumento_desconocido_no_se_ignora_en_silencio() {
        // Tragarse un argumento mal escrito haría que una corrida de días
        // use valores por defecto sin que nadie lo note.
        assert!(matches!(
            parsear_str(&["--archivos", "a.xlsx"]),
            Err(FalloArgumentos::Invalido(_))
        ));
    }

    #[test]
    fn pedir_archivos_es_obligatorio_en_modo_desatendido() {
        assert!(matches!(
            parsear_str(&["--rechazadas-solo"]),
            Err(FalloArgumentos::Invalido(_))
        ));
    }

    #[test]
    fn la_ayuda_se_distingue_de_un_error() {
        assert!(matches!(
            parsear_str(&["--ayuda"]),
            Err(FalloArgumentos::PidioAyuda)
        ));
    }
}
