use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};

use commerce_core::{concatenar_vertical, CoreError, CoreResult, EscritorCsv, EscritorSalida, EscritorXlsx};
use polars::prelude::*;

use crate::constantes::{UmbralesLoteCsv, UmbralesOrden, FILAS_POR_HOJA};
use crate::escritor_particionado::EscritorParticionado;
use crate::lectura::{csv_a_io, iter_chunks};
use crate::orden::{ordenar_excel_df, ClaveExcel};

const FILAS_POR_BLOQUE_FUSION: usize = 200_000;
const FILAS_POR_SUBBLOQUE_STREAMING: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Formato {
    Csv,
    Xlsx,
}

/// Texto de menú del binario. Vive acá (no en `bin/`) para que elegir el
/// `Formato` sea por VALOR (vía `menu_seleccionar_nav<Formato>`) en vez de
/// por re-derivar el enum comparando el texto mostrado contra un prefijo
/// hardcodeado (`v.starts_with("Excel")`) — eso último se desincroniza en
/// silencio si esta etiqueta cambia y nadie actualiza el `starts_with` a la
/// vez: elegir "Excel" pasaba a producir CSV sin ningún error.
impl fmt::Display for Formato {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Formato::Xlsx => write!(f, "Excel (.xlsx) — multi-hoja automático si excede 1M filas"),
            Formato::Csv => write!(f, "CSV (.csv) — un solo archivo, sin límite de filas"),
        }
    }
}

/// `Hojas(n)`: un XLSX con hojas de `n` filas. `Archivos(n)`: varios archivos
/// de `n` filas. Dividir en hojas solo tiene sentido para XLSX.
#[derive(Debug, Clone, Copy)]
pub enum Division {
    Ninguna,
    Hojas(usize),
    Archivos(usize),
}

pub struct OpcionesCombinar<'a> {
    pub archivos: &'a [PathBuf],
    pub columnas: &'a [String],
    pub hojas_excluir: &'a [String],
    pub formato: Formato,
    pub columna_orden: Option<&'a str>,
    pub ascendente: bool,
    pub nombre_salida: &'a str,
    pub ruta_salida: &'a Path,
    pub division: Division,
    pub umbrales_orden: UmbralesOrden,
    pub umbrales_lote_csv: UmbralesLoteCsv,
}

fn error_generico(mensaje: String) -> CoreError {
    CoreError::Io(std::io::Error::other(mensaje))
}

/// Nombres de dispositivo DOS que Windows reserva a nivel de sistema de
/// archivos (con o sin extensión: `nul.csv` sigue apuntando al dispositivo
/// nulo, no a un archivo real). Comparación case-insensitive, sobre el
/// nombre SIN extensión.
const NOMBRES_RESERVADOS_WINDOWS: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
    "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// `nombre_salida` es texto libre tipeado por el usuario: si trajera un
/// separador de ruta (`..\otro\archivo`, `/etc/passwd`) o fuera una ruta
/// absoluta (`C:\ruta\ajena\x`), `Path::join` lo tomaría tal cual y se
/// escribiría FUERA de `ruta_salida` — acá se lo rechaza en vez de escribir
/// en cualquier ubicación con los permisos del usuario. También rechaza los
/// nombres de [`NOMBRES_RESERVADOS_WINDOWS`].
fn validar_nombre_salida(nombre: &str) -> CoreResult<()> {
    // Ambos separadores ('/' y '\\') se chequean explícitamente, no vía
    // `Path::file_name()`: la noción de separador de `Path` es la del SO
    // donde corre, y en Linux '\\' no lo es — `..\\otro\\archivo` pasaría
    // como "un solo nombre de archivo" y la validación quedaría rota ahí.
    let tiene_separador = nombre.contains('/') || nombre.contains('\\');
    let es_relativo_especial = nombre == "." || nombre == "..";
    if nombre.is_empty() || tiene_separador || es_relativo_especial {
        return Err(error_generico(format!(
            "El nombre de salida '{nombre}' no es válido: no puede contener separadores de ruta \
             ni ser '.'/'..'/una ruta absoluta. Usa solo un nombre de archivo."
        )));
    }
    let base = Path::new(nombre)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(nombre);
    if NOMBRES_RESERVADOS_WINDOWS
        .iter()
        .any(|r| base.eq_ignore_ascii_case(r))
    {
        return Err(error_generico(format!(
            "El nombre de salida '{nombre}' no es válido: es un nombre de dispositivo reservado \
             de Windows (CON, PRN, AUX, NUL, COM1-9, LPT1-9), con o sin extensión. Elegí otro nombre."
        )));
    }
    Ok(())
}

fn nuevo_escritor(
    formato: Formato,
    ruta: &Path,
    columnas: Vec<String>,
    filas_por_hoja: usize,
) -> CoreResult<Box<dyn EscritorSalida>> {
    Ok(match formato {
        Formato::Csv => Box::new(EscritorCsv::nuevo(ruta, columnas)?),
        Formato::Xlsx => Box::new(EscritorXlsx::nuevo(
            ruta,
            commerce_core::escritor_xlsx::OpcionesEscritorXlsx {
                columnas: Some(columnas),
                filas_por_hoja,
                hoja_por_defecto: "Parte".to_string(),
                numerar_siempre: true,
                recortar_vacias: true,
            },
        )?),
    })
}

/// Escribe `df` troceado en bloques de `tamano_bloque` filas (para que la
/// barra de progreso avance suave en vez de saltar de golpe).
fn escribir_df_en_bloques(
    df: &DataFrame,
    tamano_bloque: usize,
    escribir: &mut dyn FnMut(&DataFrame) -> CoreResult<()>,
    progreso: &mut dyn FnMut(u64),
) -> CoreResult<()> {
    let n = df.height();
    let mut pos = 0usize;
    while pos < n {
        let tomar = tamano_bloque.min(n - pos);
        let bloque = df.slice(pos as i64, tomar);
        escribir(&bloque)?;
        progreso(tomar as u64);
        pos += tomar;
    }
    Ok(())
}

/// Convierte filas en memoria (`Vec<Option<String>>`, una por columna) a un
/// DataFrame de texto y las escribe.
fn escribir_filas(
    filas: &mut Vec<Vec<Option<String>>>,
    columnas: &[String],
    escribir: &mut dyn FnMut(&DataFrame) -> CoreResult<()>,
    progreso: &mut dyn FnMut(u64),
) -> CoreResult<()> {
    if filas.is_empty() {
        return Ok(());
    }
    let ancho = columnas.len();
    let mut por_columna: Vec<Vec<Option<String>>> = vec![Vec::with_capacity(filas.len()); ancho];
    for fila in filas.drain(..) {
        for (i, valor) in fila.into_iter().enumerate() {
            if i < ancho {
                por_columna[i].push(valor);
            }
        }
    }
    let n = por_columna.first().map(|v| v.len()).unwrap_or(0);
    let series: Vec<Column> = columnas
        .iter()
        .zip(por_columna)
        .map(|(nombre, valores)| Column::new(nombre.as_str().into(), valores))
        .collect();
    let df = DataFrame::new_infer_height(series)?;
    escribir(&df)?;
    progreso(n as u64);
    Ok(())
}

/// Una entrada activa de la fusión k-vías: la fila ya leída de un run y de
/// qué run vino (para saber a cuál pedirle la siguiente al desempilarla).
struct EntradaFusion {
    clave: ClaveExcel,
    fila: Vec<Option<String>>,
    origen: usize,
}

// `eq` se define en términos de `cmp` para no romper el contrato Eq/Ord:
// `cmp` desempata por `origen`, así que compararlo solo por `clave` daría
// `eq() == true` y `cmp() != Equal` a la vez. `BinaryHeap` solo usa `Ord`,
// pero `==`/`dedup()`/`HashSet` sobre `EntradaFusion` dependerían de esto.
impl PartialEq for EntradaFusion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}
impl Eq for EntradaFusion {}
impl PartialOrd for EntradaFusion {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for EntradaFusion {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Desempate por `origen` (el run/chunk de procedencia), SIEMPRE
        // ascendente sin importar el sentido del orden: la fusión k-vías debe
        // ser estable respecto al orden de los runs de entrada en ambos
        // sentidos, igual que el sort estable del camino en memoria. Sin
        // esto, dos claves iguales llegadas de runs distintos podían salir en
        // cualquier orden relativo según el montículo, divergiendo del camino
        // en memoria (sort estable).
        self.clave
            .cmp(&other.clave)
            .then_with(|| self.origen.cmp(&other.origen))
    }
}

fn leer_fila(lector: &mut csv::Reader<File>, ancho: usize) -> CoreResult<Option<Vec<Option<String>>>> {
    let mut registro = csv::StringRecord::new();
    if lector.read_record(&mut registro).map_err(csv_a_io)? {
        let fila = (0..ancho)
            .map(|i| registro.get(i).map(|s| s.to_string()))
            .collect();
        Ok(Some(fila))
    } else {
        Ok(None)
    }
}

/// Ordena por `columna_orden` con una **mezcla externa** (external merge
/// sort): genera runs ya ordenados —volcados a CSV temporal— y los fusiona
/// en streaming con un montículo k-vías. La memoria queda plana a cualquier
/// escala. Si todo cabe en un único run, ordena en memoria sin tocar el disco.
/// Orden global por `columna_orden` con mezcla externa en disco. Recibe las
/// `opciones` completas en vez de sus campos sueltos: el llamador ya las
/// tiene armadas y explotarlas solo abría la puerta a cruzarlas entre sí.
/// `columnas` y `columna_orden` van aparte porque llegan ya resueltos (la
/// unión efectiva de columnas, y el nombre ya desempaquetado del `Option`).
fn ordenar_y_escribir(
    opciones: &OpcionesCombinar,
    columnas: &[String],
    columna_orden: &str,
    escribir: &mut dyn FnMut(&DataFrame) -> CoreResult<()>,
    mut avisar: impl FnMut(&str),
    mut progreso: impl FnMut(u64),
) -> CoreResult<()> {
    let OpcionesCombinar {
        archivos,
        hojas_excluir,
        ascendente,
        umbrales_orden,
        umbrales_lote_csv,
        ..
    } = *opciones;
    let idx = columnas
        .iter()
        .position(|c| c == columna_orden)
        .ok_or_else(|| error_generico(format!("columna de orden '{columna_orden}' no existe")))?;

    let filas_por_run = umbrales_orden.filas_por_run(columnas.len());
    let tmpdir = tempfile::tempdir()?;

    let mut runs: Vec<PathBuf> = Vec::new();
    let mut buffer: Vec<DataFrame> = Vec::new();
    let mut alto = 0usize;

    let volcar_run =
        |buffer: &mut Vec<DataFrame>, alto: &mut usize, runs: &mut Vec<PathBuf>| -> CoreResult<()> {
            let concatenado = concatenar_vertical(std::mem::take(buffer))?;
            let mut ordenado = ordenar_excel_df(&concatenado, columna_orden, ascendente)?;
            let ruta_run = tmpdir.path().join(format!("run_{}.csv", runs.len()));
            let mut archivo = File::create(&ruta_run)?;
            CsvWriter::new(&mut archivo).finish(&mut ordenado)?;
            runs.push(ruta_run);
            *alto = 0;
            Ok(())
        };

    iter_chunks(
        archivos,
        columnas,
        hojas_excluir,
        umbrales_lote_csv,
        &mut avisar,
        |chunk| {
            let mut pos = 0usize;
            while pos < chunk.height() {
                let tomar = (filas_por_run - alto).min(chunk.height() - pos);
                buffer.push(chunk.slice(pos as i64, tomar));
                alto += tomar;
                pos += tomar;
                if alto >= filas_por_run {
                    volcar_run(&mut buffer, &mut alto, &mut runs)?;
                }
            }
            Ok(())
        },
    )?;

    // Camino rápido: todo cupo en memoria → una sola ordenación, sin disco.
    if runs.is_empty() {
        let df = if buffer.is_empty() {
            let esquema: Vec<Column> = columnas
                .iter()
                .map(|c| Column::new(c.as_str().into(), Vec::<Option<String>>::new()))
                .collect();
            DataFrame::new_infer_height(esquema)?
        } else {
            ordenar_excel_df(&concatenar_vertical(buffer)?, columna_orden, ascendente)?
        };
        return escribir_df_en_bloques(&df, FILAS_POR_BLOQUE_FUSION, escribir, &mut progreso);
    }

    // Vuelca el último run parcial antes de la fusión.
    if !buffer.is_empty() {
        volcar_run(&mut buffer, &mut alto, &mut runs)?;
    }

    // Fusión k-vías perezosa sobre los runs ya ordenados (memoria plana).
    let mut lectores: Vec<csv::Reader<File>> = runs
        .iter()
        .map(|r| {
            csv::ReaderBuilder::new()
                .has_headers(true)
                .from_path(r)
                .map_err(csv_a_io)
        })
        .collect::<Result<_, _>>()?;

    let ancho = columnas.len();
    let mut monticulo: BinaryHeap<Reverse<EntradaFusion>> = BinaryHeap::new();
    for (origen, lector) in lectores.iter_mut().enumerate() {
        if let Some(fila) = leer_fila(lector, ancho)? {
            let clave = ClaveExcel::nueva(fila[idx].as_deref().unwrap_or(""), ascendente);
            monticulo.push(Reverse(EntradaFusion { clave, fila, origen }));
        }
    }

    let mut bloque: Vec<Vec<Option<String>>> = Vec::new();
    while let Some(Reverse(entrada)) = monticulo.pop() {
        bloque.push(entrada.fila);
        if let Some(fila) = leer_fila(&mut lectores[entrada.origen], ancho)? {
            let clave = ClaveExcel::nueva(fila[idx].as_deref().unwrap_or(""), ascendente);
            monticulo.push(Reverse(EntradaFusion {
                clave,
                fila,
                origen: entrada.origen,
            }));
        }
        if bloque.len() >= FILAS_POR_BLOQUE_FUSION {
            escribir_filas(&mut bloque, columnas, escribir, &mut progreso)?;
        }
    }
    escribir_filas(&mut bloque, columnas, escribir, &mut progreso)?;
    Ok(())
}

enum Salida {
    Unica(Box<dyn EscritorSalida>),
    Particionada(EscritorParticionado),
}

impl Salida {
    fn rutas(&self) -> Vec<PathBuf> {
        match self {
            Salida::Unica(e) => vec![e.ruta().to_path_buf()],
            Salida::Particionada(p) => p.rutas.clone(),
        }
    }

    fn total(&self) -> usize {
        match self {
            Salida::Unica(e) => e.total(),
            Salida::Particionada(p) => p.total,
        }
    }

    fn cerrar(&mut self) -> CoreResult<()> {
        match self {
            Salida::Unica(e) => e.cerrar(),
            Salida::Particionada(p) => p.cerrar(),
        }
    }

    fn abortar(&mut self) -> CoreResult<()> {
        match self {
            Salida::Unica(e) => e.abortar(),
            Salida::Particionada(p) => p.abortar(),
        }
    }
}

/// Lee y combina los archivos en la salida (CSV o XLSX), en streaming.
/// Devuelve (rutas_generadas, filas_escritas).
pub fn combinar(
    opciones: &OpcionesCombinar,
    mut avisar: impl FnMut(&str),
    mut progreso: impl FnMut(u64),
) -> CoreResult<(Vec<PathBuf>, usize)> {
    validar_nombre_salida(opciones.nombre_salida)?;
    // `Division::Hojas` divide en HOJAS de un libro, y un CSV no tiene hojas:
    // `EscritorCsv` ignora `filas_por_hoja`, así que la combinación no falla,
    // simplemente no divide. Se rechaza acá en vez de dejarla pasar en
    // silencio — el binario ya no la ofrece, pero la biblioteca no puede
    // depender de que su llamador se acuerde de la regla.
    if !matches!(opciones.formato, Formato::Xlsx) && matches!(opciones.division, Division::Hojas(_)) {
        return Err(error_generico(
            "dividir en hojas requiere formato Excel: los demás formatos no tienen hojas".to_string(),
        ));
    }
    let extension = match opciones.formato {
        Formato::Csv => "csv",
        Formato::Xlsx => "xlsx",
    };
    let base = opciones
        .ruta_salida
        .join(format!("{}.{extension}", opciones.nombre_salida));
    let columnas = opciones.columnas.to_vec();

    let mut salida = match opciones.division {
        Division::Archivos(filas) => {
            let formato = opciones.formato;
            let cols_fabrica = columnas.clone();
            let fabrica =
                move |ruta: &Path| nuevo_escritor(formato, ruta, cols_fabrica.clone(), FILAS_POR_HOJA);
            Salida::Particionada(EscritorParticionado::nuevo(fabrica, base, filas))
        }
        // `base` viaja sin desambiguar en las tres ramas: `EscritorXlsx` y
        // `EscritorCsv` ya aplican `ruta_unica` al construirse, y la ruta
        // REAL se lee después de ellos (`escritor.ruta()`). Hacerlo también
        // acá duplicaba una responsabilidad que ADR 0001 le asigna al
        // escritor, y solo en dos de las tres ramas.
        Division::Hojas(filas) => {
            Salida::Unica(nuevo_escritor(opciones.formato, &base, columnas.clone(), filas)?)
        }
        Division::Ninguna => Salida::Unica(nuevo_escritor(
            opciones.formato,
            &base,
            columnas.clone(),
            FILAS_POR_HOJA,
        )?),
    };

    let resultado: CoreResult<()> = {
        let escribir_uno = |df: &DataFrame, salida: &mut Salida| -> CoreResult<()> {
            match salida {
                Salida::Unica(escritor) => escritor.escribir(df, None),
                Salida::Particionada(particionado) => particionado.escribir(df),
            }
        };

        if let Some(columna_orden) = opciones.columna_orden {
            let mut escribir = |df: &DataFrame| escribir_uno(df, &mut salida);
            ordenar_y_escribir(
                opciones,
                &columnas,
                columna_orden,
                &mut escribir,
                &mut avisar,
                &mut progreso,
            )
        } else {
            iter_chunks(
                opciones.archivos,
                &columnas,
                opciones.hojas_excluir,
                opciones.umbrales_lote_csv,
                &mut avisar,
                |chunk| {
                    let mut escribir = |df: &DataFrame| escribir_uno(df, &mut salida);
                    escribir_df_en_bloques(
                        &chunk,
                        FILAS_POR_SUBBLOQUE_STREAMING,
                        &mut escribir,
                        &mut progreso,
                    )
                },
            )
        }
    };

    match resultado {
        Ok(()) => {
            salida.cerrar()?;
            let rutas = salida.rutas();
            let total_escrito = salida.total();
            Ok((rutas, total_escrito))
        }
        Err(error) => {
            // Salida a medias (zip/CSV corrupto): abortar() cierra/suelta el
            // archivo (o los varios, si es particionado) ANTES de borrarlos —
            // borrar con el handle todavía abierto puede fallar en silencio
            // en Windows y dejar el archivo corrupto en la carpeta de salida.
            salida.abortar()?;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entrada_fusion_eq_consistente_con_ord_pese_al_desempate_por_origen() {
        // `eq` debe ser consistente con `cmp`: si `cmp` desempata por
        // `origen`, dos entradas de distinto origen no pueden ser `==`.
        let a = EntradaFusion {
            clave: ClaveExcel::nueva("Honda", true),
            fila: vec![],
            origen: 0,
        };
        let b = EntradaFusion {
            clave: ClaveExcel::nueva("Honda", true),
            fila: vec![],
            origen: 1,
        };
        assert_ne!(
            a.cmp(&b),
            std::cmp::Ordering::Equal,
            "el desempate por origen debe distinguirlas"
        );
        assert!(a != b, "eq debe ser consistente con cmp != Equal");
    }

    #[test]
    fn nombre_salida_simple_es_valido() {
        assert!(validar_nombre_salida("combinado").is_ok());
        assert!(validar_nombre_salida("combinado_2024").is_ok());
    }

    #[test]
    fn nombre_salida_rechaza_nombres_de_dispositivo_reservados_de_windows() {
        // Un nombre de dispositivo reservado pasa como "solo un nombre de
        // archivo", pero `File::create` apuntaría al dispositivo real y los
        // datos se descartarían en silencio.
        assert!(validar_nombre_salida("nul").is_err());
        assert!(validar_nombre_salida("NUL").is_err());
        assert!(validar_nombre_salida("con").is_err());
        assert!(validar_nombre_salida("COM1").is_err());
        assert!(validar_nombre_salida("lpt3").is_err());
        assert!(
            validar_nombre_salida("nul.csv").is_err(),
            "reservado con o sin extensión"
        );
        // No debe dar falsos positivos: nombres que solo CONTIENEN uno de
        // estos como substring no son el dispositivo en sí.
        assert!(validar_nombre_salida("nulificado").is_ok());
        assert!(validar_nombre_salida("concatenado").is_ok());
    }

    #[test]
    fn nombre_salida_rechaza_separadores_y_rutas_absolutas() {
        assert!(validar_nombre_salida("..\\otro\\archivo").is_err());
        assert!(validar_nombre_salida("../otro/archivo").is_err());
        assert!(validar_nombre_salida("C:\\ruta\\ajena\\x").is_err());
        assert!(validar_nombre_salida("/etc/passwd").is_err());
        assert!(validar_nombre_salida("..").is_err());
        assert!(validar_nombre_salida(".").is_err());
        assert!(validar_nombre_salida("").is_err());
    }
}
