//! Verifica que los `.xlsx` que produce este workspace respeten el esquema
//! de ECMA-376, no solo que se puedan releer.
//!
//! El oráculo que faltaba. Un libro puede ser XML bien formado, abrirse sin
//! una queja con `calamine` y con `umya-spreadsheet`, y aun así hacer que
//! Excel lo "repare" al abrirlo — que para el usuario es un archivo roto.
//! Los dos defectos que llegaron a producción por esta puerta fueron:
//!
//! - `xl/styles.xml` ausente, que algunos lectores exigen sin condición.
//! - `<sheetFormatPr/>` sin `defaultRowHeight`, atributo OBLIGATORIO en
//!   `CT_SheetFormatPr`.
//!
//! Los dos los descubrió el usuario abriendo un archivo, no el CI.
//!
//! ALCANCE, dicho sin adornos: esto NO es un validador XSD. Codifica las
//! reglas de ECMA-376 que Excel hace cumplir y que este escritor puede
//! violar — orden de elementos, atributos obligatorios e integridad
//! referencial. Una regla que no esté acá no se detecta. Un validador de
//! verdad necesitaría `libxml2`, una dependencia nativa que en Windows y en
//! el build de AWS cuesta más de lo que aporta hoy.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use commerce_core::escritor_xlsx::OpcionesEscritorXlsx;
use commerce_core::EscritorXlsx;
use polars::prelude::*;

/// Orden que `CT_Worksheet` fija para sus hijos (ECMA-376 §18.3.1.99).
///
/// Es una SECUENCIA, no un conjunto: un elemento válido en el lugar
/// equivocado invalida la hoja igual que uno ausente.
const ORDEN_WORKSHEET: &[&str] = &[
    "sheetPr",
    "dimension",
    "sheetViews",
    "sheetFormatPr",
    "cols",
    "sheetData",
    "sheetCalcPr",
    "sheetProtection",
    "protectedRanges",
    "scenarios",
    "autoFilter",
    "sortState",
    "dataConsolidate",
    "customSheetViews",
    "mergeCells",
    "phoneticPr",
    "conditionalFormatting",
    "dataValidations",
    "hyperlinks",
    "printOptions",
    "pageMargins",
    "pageSetup",
    "headerFooter",
    "rowBreaks",
    "colBreaks",
    "customProperties",
    "cellWatches",
    "ignoredErrors",
    "smartTags",
    "drawing",
    "drawingHF",
    "picture",
    "oleObjects",
    "controls",
    "webPublishItems",
    "tableParts",
    "extLst",
];

/// Elementos con atributos que el esquema marca obligatorios y que este
/// escritor podría omitir.
const ATRIBUTOS_OBLIGATORIOS: &[(&str, &str)] =
    &[("sheetFormatPr", "defaultRowHeight"), ("dimension", "ref")];

struct Libro {
    partes: HashMap<String, String>,
}

impl Libro {
    fn abrir(ruta: &Path) -> Self {
        let archivo = std::fs::File::open(ruta).expect("abrir el .xlsx");
        let mut zip = ::zip::ZipArchive::new(archivo).expect("es un zip");
        let mut partes = HashMap::new();
        for i in 0..zip.len() {
            let mut entrada = zip.by_index(i).expect("entrada del zip");
            let nombre = entrada.name().to_string();
            let mut texto = String::new();
            if entrada.read_to_string(&mut texto).is_ok() {
                partes.insert(nombre, texto);
            }
        }
        Self { partes }
    }

    fn parte(&self, nombre: &str) -> &str {
        self.partes
            .get(nombre)
            .unwrap_or_else(|| panic!("falta la parte obligatoria {nombre}"))
    }

    fn hojas(&self) -> Vec<(&String, &String)> {
        let mut v: Vec<_> = self
            .partes
            .iter()
            .filter(|(n, _)| n.starts_with("xl/worksheets/") && n.ends_with(".xml"))
            .collect();
        v.sort();
        v
    }
}

/// Nombres de los hijos directos de `<worksheet>`, en el orden emitido.
fn hijos_de_worksheet(xml: &str) -> Vec<String> {
    use quick_xml::events::Event;
    let mut lector = quick_xml::Reader::from_str(xml);
    let mut hijos = Vec::new();
    let mut profundidad = 0usize;
    let mut buf = Vec::new();
    loop {
        match lector.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                profundidad += 1;
                if profundidad == 2 {
                    hijos.push(String::from_utf8_lossy(e.local_name().as_ref()).to_string());
                }
            }
            Ok(Event::Empty(e)) => {
                // Un elemento vacío no abre nivel: es hijo si estamos dentro
                // de <worksheet> y de nada más.
                if profundidad == 1 {
                    hijos.push(String::from_utf8_lossy(e.local_name().as_ref()).to_string());
                }
            }
            Ok(Event::End(_)) => profundidad = profundidad.saturating_sub(1),
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    hijos
}

fn libro_de_prueba(dir: &Path) -> PathBuf {
    let ruta = dir.join("salida.xlsx");
    let df = df!(
        "Sku" => ["A1", "007", "B2"],
        "Nombre" => ["uno & dos", "<tres>", "cuatro"],
    )
    .expect("dataframe");
    let mut escritor = EscritorXlsx::nuevo(&ruta, OpcionesEscritorXlsx::default()).expect("escritor");
    escritor.escribir(&df, Some("Datos")).expect("escribir");
    escritor.cerrar().expect("cerrar");
    escritor.ruta.clone()
}

#[test]
fn los_hijos_de_worksheet_van_en_el_orden_que_fija_el_esquema() {
    let tmp = tempfile::tempdir().unwrap();
    let libro = Libro::abrir(&libro_de_prueba(tmp.path()));

    for (nombre, xml) in libro.hojas() {
        let hijos = hijos_de_worksheet(xml);
        assert!(
            !hijos.is_empty(),
            "{nombre}: no se leyó ningún hijo de <worksheet>"
        );
        let mut ultima = 0usize;
        for hijo in &hijos {
            let pos = ORDEN_WORKSHEET
                .iter()
                .position(|e| e == hijo)
                .unwrap_or_else(|| panic!("{nombre}: <{hijo}> no es un hijo válido de <worksheet>"));
            assert!(
                pos >= ultima,
                "{nombre}: <{hijo}> aparece fuera de secuencia. El orden de \
                 CT_Worksheet no es opcional: Excel repara la hoja. Emitido: {hijos:?}"
            );
            ultima = pos;
        }
    }
}

#[test]
fn los_atributos_obligatorios_del_esquema_estan_presentes() {
    let tmp = tempfile::tempdir().unwrap();
    let libro = Libro::abrir(&libro_de_prueba(tmp.path()));

    for (nombre, xml) in libro.hojas() {
        for (elemento, atributo) in ATRIBUTOS_OBLIGATORIOS {
            // El elemento puede ser opcional; su atributo, si aparece, no.
            let Some(inicio) = xml.find(&format!("<{elemento}")) else {
                continue;
            };
            let fin = xml[inicio..].find('>').map(|f| inicio + f).unwrap_or(xml.len());
            let etiqueta = &xml[inicio..fin];
            assert!(
                etiqueta.contains(&format!("{atributo}=")),
                "{nombre}: <{elemento}> sin {atributo}, que el esquema marca \
                 obligatorio. Emitido: {etiqueta}>"
            );
        }
    }
}

#[test]
fn toda_referencia_apunta_a_algo_que_existe() {
    let tmp = tempfile::tempdir().unwrap();
    let libro = Libro::abrir(&libro_de_prueba(tmp.path()));

    // 1) Cada r:id de workbook.xml tiene su Relationship.
    let workbook = libro.parte("xl/workbook.xml");
    let rels = libro.parte("xl/_rels/workbook.xml.rels");
    for trozo in workbook.split("r:id=\"").skip(1) {
        let id = trozo.split('"').next().expect("r:id");
        assert!(
            rels.contains(&format!("Id=\"{id}\"")),
            "workbook.xml cita {id} pero no está en workbook.xml.rels"
        );
    }

    // 2) Cada Target de las relaciones existe como parte del paquete.
    for trozo in rels.split("Target=\"").skip(1) {
        let destino = trozo.split('"').next().expect("Target");
        let ruta = format!("xl/{destino}");
        assert!(
            libro.partes.contains_key(&ruta),
            "workbook.xml.rels apunta a {ruta}, que no está en el paquete"
        );
    }

    // 3) Cada fontId citado existe en styles.xml. Este es el que se rompió:
    //    `umya` escribe <phoneticPr fontId="1"/> al reescribir la hoja, sin
    //    verificar que ese índice exista.
    let estilos = libro.parte("xl/styles.xml");
    let declaradas: usize = estilos
        .split("<fonts count=\"")
        .nth(1)
        .and_then(|r| r.split('"').next())
        .and_then(|n| n.parse().ok())
        .expect("styles.xml declara cuántas fuentes tiene");
    //
    //    Se revisan las hojas Y `styles.xml`. Nuestro escritor no emite
    //    `fontId` en las hojas —solo lo hace `umya` al reescribirlas— pero sí
    //    en `cellXfs`/`cellStyleXfs`. Mirando solo las hojas, esta regla no
    //    cubría ni una línea de lo que producimos.
    let mut a_revisar: Vec<(&str, &str)> = libro
        .hojas()
        .into_iter()
        .map(|(n, x)| (n.as_str(), x.as_str()))
        .collect();
    a_revisar.push(("xl/styles.xml", estilos));
    for (nombre, xml) in a_revisar {
        for trozo in xml.split("fontId=\"").skip(1) {
            let indice: usize = trozo
                .split('"')
                .next()
                .unwrap_or_default()
                .parse()
                .expect("fontId numérico");
            assert!(
                indice < declaradas,
                "{nombre}: cita fontId={indice} pero styles.xml declara {declaradas}"
            );
        }
    }

    // 4) Cada Override de [Content_Types].xml apunta a una parte real.
    let tipos = libro.parte("[Content_Types].xml");
    for trozo in tipos.split("PartName=\"/").skip(1) {
        let parte = trozo.split('"').next().expect("PartName");
        assert!(
            libro.partes.contains_key(parte),
            "[Content_Types].xml declara {parte}, que no está en el paquete"
        );
    }
}
