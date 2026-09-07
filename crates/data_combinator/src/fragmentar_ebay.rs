//! Fragmenta enlaces de tienda de eBay en tramos de precio, para que Web
//! Scraper Cloud pueda recorrerlos.
//!
//! EL PROBLEMA. eBay corta los resultados de una búsqueda mucho antes de
//! mostrar el catálogo completo de una tienda grande. Una tienda de 300.000
//! publicaciones no se puede raspar con un solo enlace: se raspan las
//! primeras páginas y el resto no existe para el scraper. La salida es partir
//! la tienda en muchas búsquedas ESTRECHAS —una por tramo de precio— que
//! individualmente sí caben.
//!
//! QUÉ HACE. Por cada fila cuya columna `publicaciones` llegue al umbral, se
//! generan todos los tramos de `$10` a `$500` con el paso que corresponda al
//! tamaño de la tienda, y la fila original SE ELIMINA: ya está representada
//! por sus tramos, y dejarla haría que el scraper repitiera la búsqueda ancha
//! que justamente no funciona. Las demás columnas de la fila (nombre de la
//! tienda, etc.) se copian idénticas a cada tramo generado.
//!
//! QUÉ NO HACE. No toca el archivo de entrada: escribe uno nuevo en la
//! carpeta de salida. Reescribir el original en el sitio no aporta nada aquí
//! —el archivo se sube a otra herramienta, no se edita a mano— y sí quita la
//! posibilidad de comparar antes/después cuando algo sale raro.
//!
//! El parseo de las URLs vive en [`enlace`]; acá está la tabla de pasos, la
//! generación de tramos y el recorrido del libro.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use commerce_core::{
    abrir_libro, columna_texto, leer_hoja_por_nombre, nombres_hojas_libro, tomar_filas, CoreError,
    EscritorXlsx, OpcionesEscritorXlsx,
};
use polars::prelude::*;

pub mod enlace;

pub use enlace::{EnlaceTienda, ErrorEnlace, FormaSufijo};

/// Umbral de publicaciones que se ofrece por defecto en la terminal.
pub const UMBRAL_POR_DEFECTO: u64 = 9_000;

/// Extremos del rango de precio que se fragmenta. Son los que ya traen los
/// enlaces de entrada (`_udlo=10&_udhi=500`): fragmentar fuera de ese rango
/// devolvería tramos que la búsqueda original tampoco cubría.
pub const PRECIO_MIN: u32 = 10;
pub const PRECIO_MAX: u32 = 500;

/// Nombres de las dos columnas obligatorias, tal como aparecen en el archivo.
/// La búsqueda es tolerante a mayúsculas y a espacios de más (ver
/// [`buscar_columna`]), pero no inventa sinónimos.
pub const COLUMNA_PUBLICACIONES: &str = "publicaciones";
pub const COLUMNA_ENLACE: &str = "resultado link";

/// El salto entre `_udlo` y `_udhi`, elegido por el tamaño de la tienda.
///
/// Un enum y no un `u32`: los cuatro pasos de la tabla son los únicos
/// legítimos, y un tipo cerrado hace imposible que un cálculo intermedio
/// produzca un paso de 0 (bucle infinito) o de 7 (tramos que no cierran
/// en 500). [`Paso::para`] es total: no hay valor de `publicaciones` sin
/// paso asignado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paso {
    /// Tiendas de hasta 100.000 publicaciones.
    Diez,
    /// 100.001 – 500.000.
    Cinco,
    /// 500.001 – 800.000.
    Tres,
    /// 800.001 en adelante.
    Dos,
}

impl Paso {
    /// La tabla de la especificación, cerrada por abajo a propósito.
    ///
    /// La tabla escrita empieza en 9.001, pero el filtro de ejecución admite
    /// `>= umbral` y el umbral por defecto es 9.000: con la tabla literal,
    /// una tienda de exactamente 9.000 pasaba el filtro y no tenía paso. Y si
    /// el usuario baja el umbral a 500, el agujero es todavía más grande.
    /// Extender el primer tramo hacia abajo cierra el hueco sin cambiar
    /// ninguna de las fronteras que sí están especificadas.
    pub fn para(publicaciones: u64) -> Self {
        match publicaciones {
            0..=100_000 => Paso::Diez,
            100_001..=500_000 => Paso::Cinco,
            500_001..=800_000 => Paso::Tres,
            _ => Paso::Dos,
        }
    }

    pub fn valor(self) -> u32 {
        match self {
            Paso::Diez => 10,
            Paso::Cinco => 5,
            Paso::Tres => 3,
            Paso::Dos => 2,
        }
    }
}

/// Un tramo de precio cerrado, en dólares enteros.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rango {
    pub lo: u32,
    pub hi: u32,
}

/// Todos los tramos de [`PRECIO_MIN`] a [`PRECIO_MAX`] con el paso dado.
///
/// El PRIMER tramo es más ancho que el resto a propósito: va de 10 a
/// `10 + paso` (10–20 con paso 10, 10–15 con paso 5), que es lo que redondea
/// el arranque a la decena. A partir de ahí cada tramo empieza donde terminó
/// el anterior más uno, así que la cobertura de 10 a 500 no tiene huecos ni
/// solapamientos.
///
/// El último tramo se RECORTA a 500. Con paso 3 la progresión no cae justo en
/// 500 y sin el recorte el último enlace pediría precios hasta 502: no es un
/// error visible —eBay lo aceptaría— pero rasparía artículos fuera del rango
/// que la búsqueda original cubría.
/// Solo hay CUATRO tablas posibles y no dependen de la tienda, así que se
/// calculan una vez por proceso y se prestan. Devolver un `Vec` nuevo hacía
/// una asignación POR TIENDA FRAGMENTADA —10.748 en la corrida real, de hasta
/// 245 elementos cada una— para entregar siempre el mismo contenido.
pub fn rangos(paso: Paso) -> &'static [Rango] {
    static DIEZ: LazyLock<Vec<Rango>> = LazyLock::new(|| calcular_tramos(Paso::Diez));
    static CINCO: LazyLock<Vec<Rango>> = LazyLock::new(|| calcular_tramos(Paso::Cinco));
    static TRES: LazyLock<Vec<Rango>> = LazyLock::new(|| calcular_tramos(Paso::Tres));
    static DOS: LazyLock<Vec<Rango>> = LazyLock::new(|| calcular_tramos(Paso::Dos));
    // `match` exhaustivo y sin rama `_`: un paso nuevo no compila hasta tener
    // su tabla, en vez de caer en la de otro paso en silencio.
    match paso {
        Paso::Diez => &DIEZ,
        Paso::Cinco => &CINCO,
        Paso::Tres => &TRES,
        Paso::Dos => &DOS,
    }
}

fn calcular_tramos(paso: Paso) -> Vec<Rango> {
    let paso = paso.valor();
    let mut tramos = Vec::new();
    let mut lo = PRECIO_MIN;
    // El primero es el tramo ancho que redondea el arranque a la decena.
    let mut hi = PRECIO_MIN + paso;
    while lo <= PRECIO_MAX {
        let hi_recortado = hi.min(PRECIO_MAX);
        tramos.push(Rango {
            lo,
            hi: hi_recortado,
        });
        if hi_recortado >= PRECIO_MAX {
            break;
        }
        lo = hi_recortado + 1;
        // `paso` vale siempre 2 o más, así que `lo + paso - 1 >= lo`: ni
        // resta que se pase de cero, ni tramo invertido.
        hi = lo + paso - 1;
    }
    tramos
}

/// Qué salió mal al fragmentar un archivo.
#[derive(Debug, thiserror::Error)]
pub enum ErrorFragmentar {
    #[error("no se encontró el archivo '{0}'")]
    ArchivoInexistente(PathBuf),
    #[error(
        "ninguna hoja de '{archivo}' tiene las columnas obligatorias '{}' y '{}'",
        COLUMNA_PUBLICACIONES,
        COLUMNA_ENLACE
    )]
    SinColumnasObligatorias { archivo: PathBuf },
    // Un solo camino para los errores de polars: `CoreError` ya los envuelve.
    // Con una variante `Polars` propia, el MISMO error llegaba como
    // `Polars(..)` desde `columna_texto` y como `Core(Polars(..))` desde
    // `tomar_filas`, asi que un `match` del llamador tenia que acordarse de
    // cubrir las dos.
    #[error(transparent)]
    Core(#[from] CoreError),
}

/// Qué se hizo con UNA fila.
///
/// Las cuatro variantes PARTICIONAN la entrada: cada fila cae en exactamente
/// una, así que los contadores del informe suman siempre `filas_entrada`. Con
/// un `if` suelto por caso —como estaba antes— una fila con `publicaciones`
/// ilegible se contaba a la vez como ilegible y como "no llegó al umbral", y
/// el informe cuadraba mal sin que nada fallara.
enum Destino {
    Fragmentada(EnlaceTienda, Paso),
    /// No llega al umbral (o no trae dato de tamaño): se copia sin tocar.
    BajoUmbral,
    /// Llega al umbral pero su enlace no se pudo parsear: queda INTACTA.
    EnlaceInvalido(ErrorEnlace),
    /// `publicaciones` traía algo que no es un número: queda INTACTA.
    PublicacionesIlegibles,
}

/// Cuentas de una corrida, para el informe final.
///
/// En una hoja PROCESADA, `tiendas_fragmentadas + filas_bajo_umbral +
/// enlaces_invalidos + publicaciones_ilegibles == filas_entrada`. Es lo que
/// permite imprimir el informe completo —incluidos los ceros— y que el usuario
/// pueda creerle: un total que no cuadra delata que una fila se perdió por el
/// camino. Las hojas copiadas sin columnas suman a `filas_entrada` y a
/// ninguno de los cuatro, y por eso se reportan aparte.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Resumen {
    pub hojas_procesadas: usize,
    pub hojas_sin_columnas: usize,
    pub filas_entrada: usize,
    pub filas_salida: usize,
    /// Filas que llegaron al umbral y se reemplazaron por sus tramos.
    pub tiendas_fragmentadas: usize,
    pub filas_generadas: usize,
    /// Filas que no llegaron al umbral: se copiaron sin tocar. NO son un
    /// error, y el informe tiene que decirlo con esas palabras.
    pub filas_bajo_umbral: usize,
    /// Llegaron al umbral pero su enlace no se pudo parsear: quedan INTACTAS.
    pub enlaces_invalidos: usize,
    /// `publicaciones` tenía contenido pero no un número: quedan INTACTAS.
    pub publicaciones_ilegibles: usize,
}

impl Resumen {
    /// Filas que llegaron al umbral y aun así no se pudieron fragmentar. Es
    /// el único número del informe que significa "algo salió mal".
    pub fn errores(&self) -> usize {
        self.enlaces_invalidos + self.publicaciones_ilegibles
    }

    /// Tiendas que quedaron con su enlace original: las que no llegan al
    /// umbral MÁS las que fallaron. Se calcula desde la partición y no como
    /// `filas_entrada - fragmentadas` para que siga cuadrando aunque el libro
    /// traiga hojas copiadas sin procesar.
    pub fn sin_fragmentar(&self) -> usize {
        self.filas_bajo_umbral + self.errores()
    }

    /// Tiendas consideradas: una por fila de las hojas procesadas.
    pub fn tiendas_totales(&self) -> usize {
        self.tiendas_fragmentadas + self.sin_fragmentar()
    }

    /// Acumula las cuentas de una hoja en el total del libro.
    ///
    /// Se desestructura SIN `..` a proposito: agregar un campo a `Resumen` y
    /// olvidarlo aca deja de compilar. Con la suma campo a campo anterior
    /// compilaba igual y el total de un libro multi-hoja salia mal en
    /// silencio — el defecto exacto que se busca hacer imposible, y que ya
    /// aparecio dos veces mientras se escribia este modulo.
    fn sumar(&mut self, otro: Resumen) {
        let Resumen {
            hojas_procesadas,
            hojas_sin_columnas,
            filas_entrada,
            filas_salida,
            tiendas_fragmentadas,
            filas_generadas,
            filas_bajo_umbral,
            enlaces_invalidos,
            publicaciones_ilegibles,
        } = otro;
        self.hojas_procesadas += hojas_procesadas;
        self.hojas_sin_columnas += hojas_sin_columnas;
        self.filas_entrada += filas_entrada;
        self.filas_salida += filas_salida;
        self.tiendas_fragmentadas += tiendas_fragmentadas;
        self.filas_generadas += filas_generadas;
        self.filas_bajo_umbral += filas_bajo_umbral;
        self.enlaces_invalidos += enlaces_invalidos;
        self.publicaciones_ilegibles += publicaciones_ilegibles;
    }
}

pub struct OpcionesFragmentar<'a> {
    pub archivo: &'a Path,
    /// Se fragmenta la fila si `publicaciones >= umbral`.
    pub umbral: u64,
    pub nombre_salida: &'a str,
    pub ruta_salida: &'a Path,
}

/// Fragmenta TODAS las hojas del archivo y escribe un `.xlsx` nuevo.
///
/// Las hojas que no tengan las dos columnas obligatorias se copian tal cual
/// —con aviso— en vez de abortar: un libro real suele traer una hoja de notas
/// al lado de la de datos, y perder el trabajo por eso sería peor que
/// copiarla. Solo se aborta si NINGUNA hoja las tiene, porque entonces el
/// archivo elegido no es el que la herramienta espera.
pub fn fragmentar_archivo(
    opciones: &OpcionesFragmentar,
    mut avisar: impl FnMut(&str),
) -> Result<(PathBuf, Resumen), ErrorFragmentar> {
    if !opciones.archivo.is_file() {
        return Err(ErrorFragmentar::ArchivoInexistente(
            opciones.archivo.to_path_buf(),
        ));
    }
    let mut libro = abrir_libro(opciones.archivo)?;
    let hojas = nombres_hojas_libro(&libro);

    let destino = opciones
        .ruta_salida
        .join(format!("{}.xlsx", opciones.nombre_salida));
    let mut escritor = EscritorXlsx::nuevo(&destino, OpcionesEscritorXlsx::default())?;
    let ruta_real = escritor.ruta.clone();

    let mut total = Resumen::default();
    for hoja in &hojas {
        let df = leer_hoja_por_nombre(&mut libro, opciones.archivo, hoja)?;
        // Una hoja sin filas SÍ pasa por `fragmentar_hoja`: si tiene las
        // columnas, cuenta como procesada. Saltearla antes hacía que un
        // archivo vacío pero bien formado fallara con "ninguna hoja tiene las
        // columnas obligatorias", que manda a buscar un problema que no está.
        let (salida, resumen) = fragmentar_hoja(&df, opciones.umbral, hoja, &mut avisar)?;
        total.sumar(resumen);
        if let Err(error) = escritor.escribir(&salida, Some(hoja)) {
            // Sin esto, un fallo a mitad dejaría un .xlsx truncado en la
            // carpeta de salida con pinta de resultado bueno.
            let _ = escritor.abortar();
            return Err(error.into());
        }
    }

    if total.hojas_procesadas == 0 {
        let _ = escritor.abortar();
        return Err(ErrorFragmentar::SinColumnasObligatorias {
            archivo: opciones.archivo.to_path_buf(),
        });
    }
    escritor.cerrar()?;
    Ok((ruta_real, total))
}

/// Fragmenta UNA hoja. Expuesta para poder probar la transformación sin
/// pasar por el disco.
pub fn fragmentar_hoja(
    df: &DataFrame,
    umbral: u64,
    hoja: &str,
    avisar: &mut impl FnMut(&str),
) -> Result<(DataFrame, Resumen), ErrorFragmentar> {
    let mut resumen = Resumen {
        filas_entrada: df.height(),
        ..Resumen::default()
    };

    let (Some(col_pub), Some(col_enlace)) = (
        buscar_columna(df, COLUMNA_PUBLICACIONES),
        buscar_columna(df, COLUMNA_ENLACE),
    ) else {
        avisar(&format!(
            "Hoja '{hoja}': no tiene las columnas '{COLUMNA_PUBLICACIONES}' y \
             '{COLUMNA_ENLACE}'; se copia sin cambios."
        ));
        resumen.hojas_sin_columnas = 1;
        resumen.filas_salida = df.height();
        return Ok((df.clone(), resumen));
    };

    resumen.hojas_procesadas = 1;
    let publicaciones = columna_texto(df, &col_pub).map_err(CoreError::from)?;
    // `mut` porque los enlaces que se copian tal cual se MUEVEN a la salida
    // (`Option::take`) en vez de clonarse: son una String por fila que no
    // vuelve a leerse.
    let mut enlaces = columna_texto(df, &col_enlace).map_err(CoreError::from)?;

    // `indices` dice de qué fila de ENTRADA sale cada fila de salida (una fila
    // fragmentada aparece tantas veces como tramos genere); `nuevos` trae el
    // enlace de cada una. Construir así la salida —en vez de ir concatenando
    // DataFrames fila a fila— mantiene el resto de las columnas alineadas sin
    // tener que copiarlas a mano.
    let mut indices: Vec<usize> = Vec::with_capacity(df.height());
    let mut nuevos: Vec<Option<String>> = Vec::with_capacity(df.height());

    for fila in 0..df.height() {
        let crudo = publicaciones[fila].as_deref().unwrap_or("").trim();
        let destino = match parsear_publicaciones(crudo) {
            // Debajo del umbral no se toca nada: es el caso normal de la
            // mayoría de las filas del archivo.
            Some(cantidad) if cantidad < umbral => Destino::BajoUmbral,
            Some(cantidad) => match EnlaceTienda::parsear(enlaces[fila].as_deref().unwrap_or("")) {
                Ok(enlace) => Destino::Fragmentada(enlace, Paso::para(cantidad)),
                Err(error) => Destino::EnlaceInvalido(error),
            },
            // Una celda vacía es una tienda sin dato de tamaño, no un dato
            // equivocado: no llega al umbral y no se reporta como error.
            None if crudo.is_empty() => Destino::BajoUmbral,
            None => Destino::PublicacionesIlegibles,
        };

        match destino {
            Destino::Fragmentada(enlace, paso) => {
                for rango in rangos(paso) {
                    indices.push(fila);
                    nuevos.push(Some(enlace.con_rango(rango.lo, rango.hi)));
                    resumen.filas_generadas += 1;
                }
                resumen.tiendas_fragmentadas += 1;
                // La fila original NO se agrega: queda representada por sus
                // tramos y se elimina del resultado.
            }
            Destino::BajoUmbral => {
                resumen.filas_bajo_umbral += 1;
                indices.push(fila);
                nuevos.push(enlaces[fila].take());
            }
            Destino::EnlaceInvalido(error) => {
                resumen.enlaces_invalidos += 1;
                avisar(&format!(
                    "Hoja '{hoja}', fila {}: llega al umbral pero su enlace no se pudo \
                     fragmentar ({error}); la fila se deja intacta.",
                    fila + 2
                ));
                indices.push(fila);
                nuevos.push(enlaces[fila].take());
            }
            Destino::PublicacionesIlegibles => {
                resumen.publicaciones_ilegibles += 1;
                avisar(&format!(
                    "Hoja '{hoja}', fila {}: '{COLUMNA_PUBLICACIONES}' = '{crudo}' no es un \
                     número; la fila se deja intacta.",
                    fila + 2
                ));
                indices.push(fila);
                nuevos.push(enlaces[fila].take());
            }
        }
    }

    let mut salida = tomar_filas(df, &indices)?;
    salida
        .with_column(Column::new(col_enlace.as_str().into(), nuevos))
        .map_err(CoreError::from)?;
    resumen.filas_salida = salida.height();
    Ok((salida, resumen))
}

/// Busca una columna ignorando mayúsculas y espacios de más.
///
/// Los archivos vienen de exportaciones distintas y el mismo campo aparece
/// como `Publicaciones`, `publicaciones ` o `Resultado Link`. Exigir el
/// nombre exacto haría fallar la herramienta por un detalle que al usuario le
/// resulta invisible.
fn buscar_columna(df: &DataFrame, buscada: &str) -> Option<String> {
    let objetivo = normalizar_nombre(buscada);
    df.get_column_names()
        .iter()
        .map(|c| c.to_string())
        .find(|c| normalizar_nombre(c) == objetivo)
}

fn normalizar_nombre(nombre: &str) -> String {
    nombre.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Lee la cantidad de publicaciones de una celda de texto.
///
/// Una celda numérica de Excel llega ya como dígitos pelados, así que el caso
/// normal es un `parse` directo. Se toleran además el separador de miles
/// (`9,001`) y la parte decimal (`9001.0`) porque aparecen cuando la columna
/// se pegó como texto. Se descarta el separador `,` y NO el `.`: en los datos
/// de eBay el punto es decimal, y tratarlo como miles convertiría `9.5` en
/// noventa y cinco.
fn parsear_publicaciones(crudo: &str) -> Option<u64> {
    let limpio: String = crudo
        .chars()
        .filter(|c| !matches!(c, ',' | ' ' | '\u{a0}'))
        .collect();
    let valor: f64 = limpio.parse().ok()?;
    if !valor.is_finite() || valor < 0.0 {
        return None;
    }
    Some(valor.trunc() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_tabla_de_pasos_cubre_las_cuatro_franjas_y_sus_fronteras() {
        assert_eq!(Paso::para(9_001).valor(), 10);
        assert_eq!(Paso::para(100_000).valor(), 10);
        assert_eq!(Paso::para(100_001).valor(), 5);
        assert_eq!(Paso::para(500_000).valor(), 5);
        assert_eq!(Paso::para(500_001).valor(), 3);
        assert_eq!(Paso::para(800_000).valor(), 3);
        assert_eq!(Paso::para(800_001).valor(), 2);
        assert_eq!(Paso::para(5_000_000).valor(), 2);
    }

    #[test]
    fn una_tienda_justo_en_el_umbral_por_defecto_tiene_paso() {
        // La tabla escrita empieza en 9.001, pero el filtro admite `>= 9000`.
        // Sin cerrar la franja por abajo, 9.000 pasaba el filtro y se quedaba
        // sin paso asignado.
        assert_eq!(Paso::para(UMBRAL_POR_DEFECTO).valor(), 10);
    }

    #[test]
    fn los_tramos_de_paso_diez_son_los_del_ejemplo() {
        let t = rangos(Paso::Diez);
        assert_eq!(t[0], Rango { lo: 10, hi: 20 });
        assert_eq!(t[1], Rango { lo: 21, hi: 30 });
        assert_eq!(*t.last().expect("hay tramos"), Rango { lo: 491, hi: 500 });
        // 49 y no 50: el primer tramo es ancho (10–20), y los 48 restantes
        // arrancan en 21 de diez en diez hasta 491–500.
        assert_eq!(t.len(), 49);
    }

    #[test]
    fn los_tramos_de_paso_cinco_tres_y_dos_son_los_del_ejemplo() {
        assert_eq!(rangos(Paso::Cinco)[0], Rango { lo: 10, hi: 15 });
        assert_eq!(rangos(Paso::Cinco)[1], Rango { lo: 16, hi: 20 });
        assert_eq!(rangos(Paso::Tres)[0], Rango { lo: 10, hi: 13 });
        assert_eq!(rangos(Paso::Tres)[1], Rango { lo: 14, hi: 16 });
        assert_eq!(rangos(Paso::Dos)[0], Rango { lo: 10, hi: 12 });
        assert_eq!(rangos(Paso::Dos)[1], Rango { lo: 13, hi: 14 });
    }

    #[test]
    fn los_tramos_cubren_de_diez_a_quinientos_sin_huecos_ni_solapes() {
        // La propiedad que de verdad importa: si hay un hueco, los artículos
        // de ese precio no los raspa NADIE, y el faltante es invisible.
        for paso in [Paso::Diez, Paso::Cinco, Paso::Tres, Paso::Dos] {
            let tramos = rangos(paso);
            assert_eq!(tramos[0].lo, PRECIO_MIN, "paso {:?}", paso);
            assert_eq!(
                tramos.last().expect("hay tramos").hi,
                PRECIO_MAX,
                "paso {:?}",
                paso
            );
            for par in tramos.windows(2) {
                assert_eq!(par[1].lo, par[0].hi + 1, "paso {:?}: {par:?}", paso);
            }
            // Contigüidad + último == 500 implica que NINGÚN tramo se pasa de
            // 500, que es lo que verifica el recorte con paso 3 (la
            // progresión llegaría a 502). No hace falta un test aparte.
            for tramo in tramos {
                assert!(tramo.lo <= tramo.hi, "paso {:?}: {tramo:?}", paso);
                assert!(tramo.hi <= PRECIO_MAX, "paso {:?}: {tramo:?}", paso);
            }
        }
    }

    #[test]
    fn se_leen_las_publicaciones_venga_como_venga_la_celda() {
        assert_eq!(parsear_publicaciones("9001"), Some(9_001));
        assert_eq!(parsear_publicaciones("9,001"), Some(9_001));
        assert_eq!(parsear_publicaciones("9001.0"), Some(9_001));
        assert_eq!(parsear_publicaciones("9001.9"), Some(9_001));
        assert_eq!(parsear_publicaciones(""), None);
        assert_eq!(parsear_publicaciones("muchas"), None);
        assert_eq!(parsear_publicaciones("-5"), None);
    }

    #[test]
    fn el_nombre_de_columna_tolera_mayusculas_y_espacios() {
        let df = df!("Resultado  Link" => ["a"], "PUBLICACIONES" => ["1"]).expect("df");
        assert_eq!(
            buscar_columna(&df, COLUMNA_ENLACE).as_deref(),
            Some("Resultado  Link")
        );
        assert_eq!(
            buscar_columna(&df, COLUMNA_PUBLICACIONES).as_deref(),
            Some("PUBLICACIONES")
        );
    }
}
