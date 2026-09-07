//! Fragmentación de enlaces de tienda de eBay, de archivo a archivo.
//!
//! Los tests de unidad del módulo cubren la tabla de pasos y el parseo de
//! URLs. Acá se prueba lo que ninguno de ellos ve: que el recorrido del LIBRO
//! toque todas las hojas, que la fila original desaparezca de verdad, y que
//! el resto de las columnas viaje pegado a cada tramo generado.

use std::path::{Path, PathBuf};

use commerce_core::{abrir_libro, columna_texto, leer_hoja_por_nombre, nombres_hojas_libro};
use commerce_core::{EscritorXlsx, OpcionesEscritorXlsx};
use data_combinator::fragmentar_ebay::{
    fragmentar_hoja, rangos, Paso, COLUMNA_ENLACE, COLUMNA_PUBLICACIONES,
};
use data_combinator::{fragmentar_archivo, OpcionesFragmentar, UMBRAL_POR_DEFECTO};
use polars::prelude::*;

const TIENDA_A: &str =
    "https://www.ebay.com/sch/i.html?sid=subarupartsdirect&isRefine=true&_sop=15&_udlo=10&_udhi=500&_ipg=240";
const TIENDA_B: &str =
    "https://www.ebay.com/sch/i.html?sid=otratienda&isRefine=true&_udlo=10&_udhi=500&_ipg=240&_sop=16";

fn sin_avisos(_: &str) {}

/// Una hoja con las dos columnas obligatorias más una tercera de contexto,
/// para comprobar que las columnas ajenas sobreviven a la fragmentación.
fn hoja(nombres: &[&str], publicaciones: &[&str], enlaces: &[&str]) -> DataFrame {
    df!(
        "tienda" => nombres,
        COLUMNA_PUBLICACIONES => publicaciones,
        COLUMNA_ENLACE => enlaces,
    )
    .expect("dataframe de prueba")
}

fn textos(df: &DataFrame, columna: &str) -> Vec<String> {
    columna_texto(df, columna)
        .expect("columna de texto")
        .into_iter()
        .map(|v| v.unwrap_or_default())
        .collect()
}

/// Escribe un libro con las hojas dadas y devuelve su ruta.
fn libro(dir: &Path, hojas: &[(&str, DataFrame)]) -> PathBuf {
    let mut escritor = EscritorXlsx::nuevo(dir.join("entrada.xlsx"), OpcionesEscritorXlsx::default())
        .expect("escritor de entrada");
    for (nombre, df) in hojas {
        escritor.escribir(df, Some(nombre)).expect("escribir hoja");
    }
    escritor.cerrar().expect("cerrar entrada");
    escritor.ruta.clone()
}

#[test]
fn la_fila_original_desaparece_y_la_reemplazan_sus_tramos() {
    // El corazón del pedido: la fila ancha SE BORRA. Si sobreviviera, el
    // scraper volvería a lanzar la búsqueda que no funciona, y encima
    // duplicaría los resultados que sí trajeron los tramos.
    let entrada = hoja(&["A"], &["9500"], &[TIENDA_A]);
    let (salida, resumen) = fragmentar_hoja(&entrada, UMBRAL_POR_DEFECTO, "Hoja1", &mut sin_avisos)
        .expect("fragmenta");

    let esperados = rangos(Paso::Diez).len();
    assert_eq!(salida.height(), esperados);
    assert_eq!(resumen.tiendas_fragmentadas, 1);
    assert_eq!(resumen.filas_generadas, esperados);

    let enlaces = textos(&salida, COLUMNA_ENLACE);
    assert!(
        !enlaces.iter().any(|e| e == TIENDA_A),
        "el enlace original sigue en la salida"
    );
    assert!(enlaces[0].ends_with("&_sop=15&_udlo=10&_udhi=20&_ipg=240"));
    assert!(enlaces[esperados - 1].ends_with("&_sop=15&_udlo=491&_udhi=500&_ipg=240"));
}

#[test]
fn las_columnas_ajenas_viajan_pegadas_a_cada_tramo() {
    // Sin esto, los enlaces generados quedarían huérfanos: 49 filas sin saber
    // de qué tienda salieron.
    let entrada = hoja(&["subaru"], &["9500"], &[TIENDA_A]);
    let (salida, _) =
        fragmentar_hoja(&entrada, UMBRAL_POR_DEFECTO, "Hoja1", &mut sin_avisos).expect("fragmenta");

    assert!(textos(&salida, "tienda").iter().all(|t| t == "subaru"));
    assert!(textos(&salida, COLUMNA_PUBLICACIONES).iter().all(|p| p == "9500"));
}

#[test]
fn por_debajo_del_umbral_la_fila_se_deja_exactamente_como_estaba() {
    let entrada = hoja(&["A", "B"], &["8999", "9000"], &[TIENDA_A, TIENDA_B]);
    let (salida, resumen) =
        fragmentar_hoja(&entrada, UMBRAL_POR_DEFECTO, "Hoja1", &mut sin_avisos).expect("fragmenta");

    // 9000 SÍ llega al umbral por defecto (el filtro es `>=`); 8999 no.
    assert_eq!(resumen.tiendas_fragmentadas, 1);
    let enlaces = textos(&salida, COLUMNA_ENLACE);
    assert_eq!(enlaces.iter().filter(|e| *e == TIENDA_A).count(), 1);
    assert!(!enlaces.iter().any(|e| e == TIENDA_B));
}

#[test]
fn el_umbral_lo_decide_quien_llama_y_no_una_constante_escondida() {
    // El usuario lo escribe en la terminal: si el motor ignorara el valor y
    // usara siempre 9000, el error sería invisible en la salida.
    let entrada = hoja(&["A"], &["500"], &[TIENDA_A]);
    let (salida, resumen) = fragmentar_hoja(&entrada, 100, "Hoja1", &mut sin_avisos).expect("fragmenta");
    assert_eq!(resumen.tiendas_fragmentadas, 1);
    assert_eq!(salida.height(), rangos(Paso::Diez).len());
}

#[test]
fn el_tamano_de_la_tienda_cambia_el_paso() {
    for (publicaciones, paso) in [
        ("50000", Paso::Diez),
        ("250000", Paso::Cinco),
        ("600000", Paso::Tres),
        ("900000", Paso::Dos),
    ] {
        let entrada = hoja(&["A"], &[publicaciones], &[TIENDA_A]);
        let (salida, _) = fragmentar_hoja(&entrada, UMBRAL_POR_DEFECTO, "Hoja1", &mut sin_avisos)
            .expect("fragmenta");
        assert_eq!(
            salida.height(),
            rangos(paso).len(),
            "{publicaciones} publicaciones deberían usar el paso {paso:?}"
        );
    }
}

#[test]
fn un_enlace_que_no_se_puede_fragmentar_deja_la_fila_intacta_y_avisa() {
    // La alternativa —descartar la fila— perdería la tienda en silencio, que
    // es justo lo que no puede pasar con un archivo que después se sube a
    // otra herramienta.
    let entrada = hoja(&["A"], &["9500"], &["https://www.ebay.com/itm/123"]);
    let mut avisos = Vec::new();
    let (salida, resumen) = fragmentar_hoja(&entrada, UMBRAL_POR_DEFECTO, "Hoja1", &mut |m| {
        avisos.push(m.to_string())
    })
    .expect("fragmenta");

    assert_eq!(resumen.enlaces_invalidos, 1);
    assert_eq!(resumen.tiendas_fragmentadas, 0);
    assert_eq!(textos(&salida, COLUMNA_ENLACE), ["https://www.ebay.com/itm/123"]);
    assert_eq!(avisos.len(), 1, "el problema tiene que llegar al usuario");
}

#[test]
fn una_columna_publicaciones_ilegible_se_reporta_en_vez_de_pasar_de_largo() {
    let entrada = hoja(&["A"], &["muchas"], &[TIENDA_A]);
    let mut avisos = Vec::new();
    let (salida, resumen) = fragmentar_hoja(&entrada, UMBRAL_POR_DEFECTO, "Hoja1", &mut |m| {
        avisos.push(m.to_string())
    })
    .expect("fragmenta");

    assert_eq!(resumen.publicaciones_ilegibles, 1);
    assert_eq!(salida.height(), 1);
    assert_eq!(avisos.len(), 1);
}

#[test]
fn se_fragmentan_todas_las_hojas_del_libro_y_las_cuentas_suman_el_total() {
    // El defecto que este test existe para impedir: procesar solo la primera
    // hoja. No da ningún error —el archivo de salida se abre perfecto— y las
    // tiendas de las hojas siguientes simplemente no se raspan nunca.
    let tmp = tempfile::tempdir().expect("tempdir");
    let ruta = libro(
        tmp.path(),
        &[
            ("Primera", hoja(&["A"], &["9500"], &[TIENDA_A])),
            ("Segunda", hoja(&["B"], &["250000"], &[TIENDA_B])),
            ("Tercera", hoja(&["C"], &["900000"], &[TIENDA_A])),
        ],
    );

    let (destino, resumen) = fragmentar_archivo(
        &OpcionesFragmentar {
            archivo: &ruta,
            umbral: UMBRAL_POR_DEFECTO,
            nombre_salida: "fragmentado",
            ruta_salida: tmp.path(),
        },
        sin_avisos,
    )
    .expect("fragmenta el archivo");

    assert_eq!(resumen.hojas_procesadas, 3);
    assert_eq!(resumen.tiendas_fragmentadas, 3);
    let esperadas = rangos(Paso::Diez).len() + rangos(Paso::Cinco).len() + rangos(Paso::Dos).len();
    assert_eq!(resumen.filas_generadas, esperadas);
    assert_eq!(resumen.filas_salida, esperadas);

    // Y el archivo escrito tiene que coincidir con el resumen: un resumen
    // correcto sobre un archivo con una sola hoja no serviría de nada.
    let mut libro_salida = abrir_libro(&destino).expect("abrir salida");
    let hojas = nombres_hojas_libro(&libro_salida);
    assert_eq!(hojas, ["Primera", "Segunda", "Tercera"]);

    let mut filas = 0;
    for nombre in &hojas {
        let df = leer_hoja_por_nombre(&mut libro_salida, &destino, nombre).expect("leer hoja");
        filas += df.height();
        assert!(
            !textos(&df, COLUMNA_ENLACE).iter().any(|e| e == TIENDA_A || e == TIENDA_B),
            "hoja '{nombre}': quedó un enlace original sin fragmentar"
        );
    }
    assert_eq!(filas, esperadas);
}

#[test]
fn una_hoja_sin_las_columnas_obligatorias_se_copia_y_no_tumba_el_resto() {
    // Un libro real trae hojas de notas al lado de la de datos. Abortar por
    // eso tiraría el trabajo de las hojas que sí sirven.
    let tmp = tempfile::tempdir().expect("tempdir");
    let notas = df!("comentario" => ["revisar el lunes"]).expect("hoja de notas");
    let ruta = libro(
        tmp.path(),
        &[
            ("Datos", hoja(&["A"], &["9500"], &[TIENDA_A])),
            ("Notas", notas),
        ],
    );

    let mut avisos = Vec::new();
    let (destino, resumen) = fragmentar_archivo(
        &OpcionesFragmentar {
            archivo: &ruta,
            umbral: UMBRAL_POR_DEFECTO,
            nombre_salida: "fragmentado",
            ruta_salida: tmp.path(),
        },
        |m| avisos.push(m.to_string()),
    )
    .expect("fragmenta el archivo");

    assert_eq!(resumen.hojas_procesadas, 1);
    assert_eq!(resumen.hojas_sin_columnas, 1);
    assert_eq!(avisos.len(), 1, "la hoja saltada tiene que avisarse");

    let libro_salida = abrir_libro(&destino).expect("abrir salida");
    assert_eq!(nombres_hojas_libro(&libro_salida), ["Datos", "Notas"]);
}

#[test]
fn un_libro_donde_ninguna_hoja_tiene_las_columnas_falla_en_vez_de_escribir_una_copia() {
    // Elegir el archivo equivocado en el menú tiene que decirlo, no dejar en
    // la carpeta de salida una copia idéntica con nombre de resultado.
    let tmp = tempfile::tempdir().expect("tempdir");
    let ruta = libro(
        tmp.path(),
        &[("Notas", df!("comentario" => ["nada"]).expect("df"))],
    );

    let resultado = fragmentar_archivo(
        &OpcionesFragmentar {
            archivo: &ruta,
            umbral: UMBRAL_POR_DEFECTO,
            nombre_salida: "fragmentado",
            ruta_salida: tmp.path(),
        },
        sin_avisos,
    );
    assert!(resultado.is_err());
    assert!(
        !tmp.path().join("fragmentado.xlsx").exists(),
        "quedó un archivo de salida a medias"
    );
}

#[test]
fn un_archivo_que_no_existe_da_un_error_claro_y_no_un_panic() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let resultado = fragmentar_archivo(
        &OpcionesFragmentar {
            archivo: &tmp.path().join("no_existe.xlsx"),
            umbral: UMBRAL_POR_DEFECTO,
            nombre_salida: "fragmentado",
            ruta_salida: tmp.path(),
        },
        sin_avisos,
    );
    assert!(matches!(
        resultado,
        Err(data_combinator::ErrorFragmentar::ArchivoInexistente(_))
    ));
}

#[test]
fn una_hoja_con_cabecera_pero_sin_filas_cuenta_como_procesada() {
    // Un archivo vacío pero bien formado no es un archivo equivocado: decir
    // "ninguna hoja tiene las columnas obligatorias" mandaría al usuario a
    // buscar un problema que no existe.
    let vacia = hoja(&[], &[], &[]);
    let (_, resumen) =
        fragmentar_hoja(&vacia, UMBRAL_POR_DEFECTO, "Hoja1", &mut sin_avisos).expect("fragmenta");
    assert_eq!(resumen.hojas_procesadas, 1);
    assert_eq!(resumen.hojas_sin_columnas, 0);
    assert_eq!(resumen.filas_salida, 0);
}

#[test]
fn los_cuatro_contadores_particionan_la_entrada() {
    // La propiedad que hace creíble al informe: cada fila cae en exactamente
    // una categoría. Si una se contara dos veces —o ninguna— el resumen
    // cuadraría mal sin que nada fallara, y "0 errores" dejaría de ser una
    // afirmación en la que se pueda confiar.
    let entrada = hoja(
        &["chica", "grande", "rota", "ilegible"],
        &["300", "9500", "9500", "muchas"],
        &[TIENDA_A, TIENDA_A, "https://www.ebay.com/itm/1", TIENDA_B],
    );
    let (_, r) =
        fragmentar_hoja(&entrada, UMBRAL_POR_DEFECTO, "Hoja1", &mut sin_avisos).expect("fragmenta");

    assert_eq!(r.tiendas_fragmentadas, 1);
    assert_eq!(r.filas_bajo_umbral, 1);
    assert_eq!(r.enlaces_invalidos, 1);
    assert_eq!(r.publicaciones_ilegibles, 1);
    assert_eq!(
        r.tiendas_fragmentadas + r.filas_bajo_umbral + r.enlaces_invalidos + r.publicaciones_ilegibles,
        r.filas_entrada,
        "los contadores no suman la entrada: alguna fila se contó dos veces o ninguna"
    );
    assert_eq!(r.errores(), 2);
}

#[test]
fn una_celda_de_publicaciones_vacia_no_cuenta_como_error() {
    // Una tienda sin dato de tamaño es un dato que falta, no uno equivocado:
    // inflar el contador de errores con esas filas haría que "0 errores"
    // nunca se pudiera alcanzar en un archivo real.
    let entrada = hoja(&["sin dato"], &[""], &[TIENDA_A]);
    let (_, r) =
        fragmentar_hoja(&entrada, UMBRAL_POR_DEFECTO, "Hoja1", &mut sin_avisos).expect("fragmenta");
    assert_eq!(r.errores(), 0);
    assert_eq!(r.filas_bajo_umbral, 1);
}
