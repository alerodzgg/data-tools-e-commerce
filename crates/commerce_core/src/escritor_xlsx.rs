use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use ::zip::ZipWriter;
use polars::prelude::*;

use crate::error::CoreResult;
use crate::rutas::ruta_unica;
use crate::xml::{col_letra, fila_xml, serializar_bloque_xml, MAX_FILAS_EXCEL};

mod hoja;
mod paquete;
mod spool;

use hoja::Hoja;

struct EstadoBase {
    columnas: Vec<String>,
    cabecera: String,
    col_final: String,
    hoja_idx: Option<usize>,
    parte: usize,
    indice: usize, // 1-based: posición de la hoja abierta en `hojas` (sheetN.xml)
    vacias_pendientes: usize,
    extras_avisadas: HashSet<String>,
}

/// Opciones de construcción de [`EscritorXlsx`].
pub struct OpcionesEscritorXlsx {
    pub columnas: Option<Vec<String>>,
    pub filas_por_hoja: usize,
    pub hoja_por_defecto: String,
    pub numerar_siempre: bool,
    pub recortar_vacias: bool,
}

impl Default for OpcionesEscritorXlsx {
    fn default() -> Self {
        Self {
            columnas: None,
            filas_por_hoja: MAX_FILAS_EXCEL,
            hoja_por_defecto: "Hoja1".to_string(),
            numerar_siempre: false,
            recortar_vacias: true,
        }
    }
}

/// Escribe XLSX generando el XML OOXML directo dentro del zip, sin pasar por
/// una librería de hojas de cálculo. Memoria plana: funciona igual con 10
/// filas o con 100M.
///
/// · *Inline strings*: TODO se guarda como texto, así que los SKUs y códigos
///   como '007' nunca se reinterpretan como números.
/// · Hojas NOMBRADAS escritas de forma intercalada (`escribir(df, "Eliminadas")`).
/// · División automática al llegar a `filas_por_hoja` (nunca por encima del
///   límite real de Excel).
/// · Alineación de cada bloque a las columnas de la hoja; las que sobran se
///   descartan CON AVISO.
/// · Recorte de filas totalmente vacías al final (las interiores se conservan).
/// · Saneado del nombre de hoja (Excel: 31 caracteres, sin `[]:*?/\`).
/// · `abortar()` y cierre idempotente: un error nunca deja un xlsx corrupto.
pub struct EscritorXlsx {
    pub ruta: PathBuf,
    zip: Option<ZipWriter<File>>,
    filas_por_hoja: usize,
    hoja_defecto: String,
    numerar_siempre: bool,
    recortar_vacias: bool,
    avisar: Box<dyn FnMut(&str)>,

    bases: Vec<EstadoBase>,
    indice_base: HashMap<String, usize>,
    hojas: Vec<Hoja>,
    nombres_hojas: HashSet<String>,
    cerrado: bool,
    pub total: usize,
}

impl EscritorXlsx {
    pub fn nuevo(ruta: impl AsRef<Path>, opciones: OpcionesEscritorXlsx) -> CoreResult<Self> {
        Self::con_avisador(ruta, opciones, |_| {})
    }

    pub fn con_avisador(
        ruta: impl AsRef<Path>,
        opciones: OpcionesEscritorXlsx,
        avisar: impl FnMut(&str) + 'static,
    ) -> CoreResult<Self> {
        let ruta = ruta_unica(ruta);
        let archivo = File::create(&ruta)?;
        let zip = ZipWriter::new(archivo);
        let filas_por_hoja = opciones.filas_por_hoja.clamp(1, MAX_FILAS_EXCEL);

        let mut escritor = Self {
            ruta,
            zip: Some(zip),
            filas_por_hoja,
            hoja_defecto: opciones.hoja_por_defecto.clone(),
            numerar_siempre: opciones.numerar_siempre,
            recortar_vacias: opciones.recortar_vacias,
            avisar: Box::new(avisar),
            bases: Vec::new(),
            indice_base: HashMap::new(),
            hojas: Vec::new(),
            nombres_hojas: HashSet::new(),
            cerrado: false,
            total: 0,
        };

        if let Some(columnas) = opciones.columnas {
            let base = opciones.hoja_por_defecto;
            escritor.crear_base(base, columnas)?;
        }
        Ok(escritor)
    }

    /// Añade las filas de `df` a la hoja indicada (o a la única que haya).
    pub fn escribir(&mut self, df: &DataFrame, hoja: Option<&str>) -> CoreResult<()> {
        if self.cerrado {
            return Err(
                io::Error::other("EscritorXlsx: no se puede escribir después de cerrar()/abortar()").into(),
            );
        }
        if df.height() == 0 {
            return Ok(());
        }
        let base = hoja::sanear(hoja.unwrap_or(&self.hoja_defecto));
        if !self.indice_base.contains_key(&base) {
            let columnas: Vec<String> = df.get_column_names().iter().map(|s| s.to_string()).collect();
            self.crear_base(base.clone(), columnas)?;
        }

        let df = self.alinear(df, &base)?;

        if !self.recortar_vacias {
            self.emitir_datos(&base, &df)?;
            return Ok(());
        }

        // Las filas 100% vacías del final se DIFIEREN: solo se escriben si más
        // adelante aparece otra con datos (entonces eran interiores). Las que
        // quedan al final del archivo se descartan.
        let idx = self.indice_base[&base];
        let columnas = self.bases[idx].columnas.clone();

        let con_datos: Vec<bool> = {
            let mut acumulado = vec![false; df.height()];
            for c in &columnas {
                let serie = df.column(c)?.as_materialized_series().clone();
                let serie = if serie.dtype() == &DataType::String {
                    serie
                } else {
                    serie.cast(&DataType::String)?
                };
                let ca = serie.str()?;
                for (i, valor) in ca.iter().enumerate() {
                    if matches!(valor, Some(v) if !v.is_empty()) {
                        acumulado[i] = true;
                    }
                }
            }
            acumulado
        };

        // Un bloque 100% vacío se difiere entero. Si hay datos, `ultima` es la
        // última fila con contenido: lo que venga después queda diferido igual.
        let Some(ultima) = con_datos.iter().rposition(|&b| b) else {
            self.bases[idx].vacias_pendientes += df.height();
            return Ok(());
        };

        let pendientes = self.bases[idx].vacias_pendientes;
        if pendientes > 0 {
            self.emitir_vacias(&base, pendientes)?;
            self.bases[idx].vacias_pendientes = 0;
        }

        let bloque = df.slice(0, ultima + 1);
        self.emitir_datos(&base, &bloque)?;
        self.bases[idx].vacias_pendientes += df.height() - (ultima + 1);
        Ok(())
    }

    /// Finaliza el xlsx. Idempotente (llamarlo dos veces no hace nada).
    ///
    /// Si alguna operación falible falla a mitad de camino, el archivo a
    /// medias se BORRA acá mismo (igual que `abortar()`) antes de propagar
    /// el error, y `cerrado` se marca `true` solo AL FINAL: marcarlo como
    /// primer paso neutralizaría tanto a `abortar()` como al `Drop`
    /// automático (ambos son no-op si `cerrado` ya es `true`) y el xlsx
    /// corrupto quedaría en disco sin ninguna forma de limpiarlo.
    pub fn cerrar(&mut self) -> CoreResult<()> {
        if self.cerrado {
            return Ok(());
        }
        let resultado = self.cerrar_interno();
        self.cerrado = true;
        if resultado.is_err() {
            let _ = std::fs::remove_file(&self.ruta);
        }
        resultado
    }

    fn cerrar_interno(&mut self) -> CoreResult<()> {
        if self.hojas.is_empty() {
            // sin datos: deja un libro válido con una hoja vacía.
            let base = self.hoja_defecto.clone();
            self.crear_base(base, Vec::new())?;
        }
        let indices: Vec<usize> = self.bases.iter().filter_map(|b| b.hoja_idx).collect();
        for idx_hoja in indices {
            self.cerrar_hoja(idx_hoja)?;
        }
        self.escribir_estructura()?;
        // `self.zip` solo pasa a `None` acá mismo, y `cerrar_interno` no
        // puede correr dos veces (`cerrar()` es idempotente vía `cerrado`).
        if let Some(zip) = self.zip.take() {
            zip.finish()?;
        }
        Ok(())
    }

    /// Tras un error: cierra el zip SIN finalizar y BORRA el archivo a
    /// medias. Un xlsx incompleto en la carpeta de salida solo estorba.
    pub fn abortar(&mut self) -> CoreResult<()> {
        if self.cerrado {
            return Ok(());
        }
        self.cerrado = true;
        let _ = std::fs::remove_file(&self.ruta);
        Ok(())
    }

    // ── internos: hojas y bases ──────────────────────────────────────────

    fn crear_base(&mut self, base: String, columnas: Vec<String>) -> CoreResult<()> {
        let cabecera = fila_xml(columnas.iter().map(|c| Some(c.as_str())), 1);
        let col_final = col_letra(columnas.len());
        let estado = EstadoBase {
            columnas,
            cabecera,
            col_final,
            hoja_idx: None,
            parte: 0,
            indice: 0,
            vacias_pendientes: 0,
            extras_avisadas: HashSet::new(),
        };
        let idx = self.bases.len();
        self.bases.push(estado);
        self.indice_base.insert(base.clone(), idx);
        self.nueva_hoja(&base)?;
        Ok(())
    }

    fn nombre_de_hoja(&self, base: &str, parte: usize) -> String {
        if self.numerar_siempre {
            hoja::sanear(&format!("{base}_{parte}"))
        } else {
            let nombre = if parte == 1 {
                base.to_string()
            } else {
                format!("{base}_{parte}")
            };
            hoja::sanear(&nombre)
        }
    }

    fn nueva_hoja(&mut self, base: &str) -> CoreResult<()> {
        let idx = self.indice_base[base];
        if let Some(hoja_idx) = self.bases[idx].hoja_idx {
            self.cerrar_hoja(hoja_idx)?;
        }

        self.bases[idx].parte += 1;
        // Unicidad entre TODAS las hojas del libro (Excel no admite repetidos).
        let candidato = self.nombre_de_hoja(base, self.bases[idx].parte);
        let nombre = hoja::nombre_unico(candidato, &self.nombres_hojas);
        self.nombres_hojas.insert(nombre.clone());

        let col_final = self.bases[idx].col_final.clone();
        let cabecera = self.bases[idx].cabecera.clone();
        let hoja = Hoja::nueva(nombre, col_final, &cabecera)?;
        self.hojas.push(hoja);
        self.bases[idx].hoja_idx = Some(self.hojas.len() - 1);
        self.bases[idx].indice = self.hojas.len(); // 1-based: sheet{indice}.xml
        Ok(())
    }

    /// Asegura una hoja con sitio y devuelve EN CUÁL quedó y cuántas filas
    /// caben. Devolver el índice —en vez de que cada llamador lo vuelva a
    /// sacar de `hoja_idx` sabiendo que ya es `Some`— es lo que hace que ahí
    /// no haga falta ningún `unwrap`.
    fn espacio(&mut self, base: &str) -> CoreResult<(usize, usize)> {
        let idx = self.indice_base[base];
        let necesita_nueva = match self.bases[idx].hoja_idx {
            None => true,
            Some(hoja_idx) => self.hojas[hoja_idx].filas >= self.filas_por_hoja,
        };
        if necesita_nueva {
            self.nueva_hoja(base)?;
        }
        match self.bases[idx].hoja_idx {
            Some(hoja_idx) => Ok((hoja_idx, self.filas_por_hoja - self.hojas[hoja_idx].filas)),
            // `nueva_hoja` siempre deja `hoja_idx` en `Some`. Que no lo hiciera
            // sería un bug de este módulo, no un dato malo del usuario, pero se
            // propaga como error igual: nada acá justifica abortar el proceso.
            None => Err(io::Error::other("EscritorXlsx: hoja sin abrir tras nueva_hoja()").into()),
        }
    }

    // ── internos: emisión ────────────────────────────────────────────────

    fn emitir_vacias(&mut self, base: &str, n: usize) -> CoreResult<()> {
        let mut restante = n;
        while restante > 0 {
            let (hoja_idx, disponible) = self.espacio(base)?;
            let tomar = disponible.min(restante).min(Self::FILAS_POR_BLOQUE);
            let hoja = &mut self.hojas[hoja_idx];
            // Cada fila lleva su propio número, así que ya no se puede
            // repetir un string precalculado. Una fila sin datos se emite
            // sin celdas: no hay nada que posicionar dentro de ella.
            let primera = hoja.filas + 2; // +1 por la cabecera, +1 para 1-based
            let mut vacias = String::with_capacity(tomar * 16);
            for fila in primera..primera + tomar {
                vacias.push_str(&format!(r#"<row r="{fila}"/>"#));
            }
            hoja.tmp.escribir(vacias.as_bytes())?;
            hoja.filas += tomar;
            self.total += tomar;
            restante -= tomar;
        }
        Ok(())
    }

    const FILAS_POR_BLOQUE: usize = 200_000;

    fn emitir_datos(&mut self, base: &str, df: &DataFrame) -> CoreResult<()> {
        let n = df.height();
        let mut pos = 0usize;
        let idx = self.indice_base[base];
        let columnas = self.bases[idx].columnas.clone();
        while pos < n {
            let (hoja_idx, disponible) = self.espacio(base)?;
            let tomar = disponible.min(n - pos).min(Self::FILAS_POR_BLOQUE);
            let bloque = df.slice(pos as i64, tomar);
            // +1 por la cabecera, +1 porque las filas de Excel son 1-based.
            let fila_inicial = self.hojas[hoja_idx].filas + 2;
            let xml = serializar_bloque_xml(&bloque, &columnas, fila_inicial)?;

            let hoja = &mut self.hojas[hoja_idx];
            hoja.tmp.escribir(xml.as_bytes())?;
            hoja.filas += tomar;
            self.total += tomar;
            pos += tomar;
        }
        Ok(())
    }

    /// Alinea el bloque a las columnas de la hoja. Faltantes → vacías;
    /// sobrantes → se descartan CON AVISO (una vez por columna). Lógica
    /// compartida con `EscritorCsv::alinear` vía `crate::alineacion`.
    fn alinear(&mut self, df: &DataFrame, base: &str) -> CoreResult<DataFrame> {
        let idx = self.indice_base[base];
        let Self { bases, avisar, .. } = self;
        let columnas = bases[idx].columnas.clone();
        let extras_avisadas = &mut bases[idx].extras_avisadas;
        crate::alineacion::alinear_columnas(df, &columnas, extras_avisadas, &mut |nuevas: &[String]| {
            avisar(&format!(
                "Hoja '{base}': columnas no vistas en el primer bloque ({}) — se descartan.",
                nuevas.join(", ")
            ));
        })
    }

    // ── internos: empaquetado OOXML ──────────────────────────────────────

    fn cerrar_hoja(&mut self, hoja_idx: usize) -> CoreResult<()> {
        // El zip y la hoja se prestan por separado porque el volcado escribe
        // una DENTRO del otro.
        let Self { zip, hojas, .. } = self;
        paquete::volcar_hoja(zip, &mut hojas[hoja_idx], hoja_idx + 1)
    }

    fn escribir_estructura(&mut self) -> CoreResult<()> {
        let nombres: Vec<String> = self.hojas.iter().map(|h| h.nombre.clone()).collect();
        paquete::escribir_estructura(&mut self.zip, &nombres)
    }
}

impl Drop for EscritorXlsx {
    /// Red de seguridad: si el usuario olvida llamar a `cerrar()`, se
    /// finaliza igualmente en vez de dejar un zip a medio escribir. Es
    /// idempotente con una llamada explícita previa (`cerrado` ya en `true`).
    fn drop(&mut self) {
        let _ = self.cerrar();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;
    use std::io::Read;

    fn leer_hojas(ruta: &Path) -> Map<String, DataFrame> {
        use calamine::{open_workbook_auto, Reader};
        let mut libro = open_workbook_auto(ruta).unwrap();
        let nombres = libro.sheet_names().to_vec();
        let mut mapa = Map::new();
        for nombre in nombres {
            let rango = libro.worksheet_range(&nombre).unwrap();
            let df = crate::lector::hoja_a_dataframe(&rango).unwrap();
            mapa.insert(nombre, df);
        }
        mapa
    }

    fn leer_filas_texto(ruta: &Path) -> Vec<Vec<Option<String>>> {
        let mapa = leer_hojas(ruta);
        let df = mapa.values().next().unwrap();
        let alto = df.height();
        let cols: Vec<_> = df.columns().to_vec();
        (0..alto)
            .map(|i| {
                cols.iter()
                    .map(|c| c.str().ok().and_then(|ca| ca.get(i)).map(|s| s.to_string()))
                    .collect()
            })
            .collect()
    }

    fn xml_hoja(ruta: &Path, indice: usize) -> String {
        let archivo = File::open(ruta).unwrap();
        let mut zip = ::zip::ZipArchive::new(archivo).unwrap();
        let mut entrada = zip.by_name(&format!("xl/worksheets/sheet{indice}.xml")).unwrap();
        let mut contenido = String::new();
        entrada.read_to_string(&mut contenido).unwrap();
        contenido
    }

    #[test]
    fn escritor_una_hoja() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("w.xlsx");
        let opciones = OpcionesEscritorXlsx {
            columnas: Some(vec!["A".into(), "B".into()]),
            ..Default::default()
        };
        let mut escritor = EscritorXlsx::nuevo(&ruta, opciones)?;
        escritor.escribir(&df!("A" => ["1", "2"], "B" => ["x", "y"])?, None)?;
        let total = escritor.total;
        escritor.cerrar()?;

        assert_eq!(total, 2);
        let filas = leer_filas_texto(&ruta);
        assert_eq!(filas[0], vec![Some("1".to_string()), Some("x".to_string())]);
        assert_eq!(filas[1], vec![Some("2".to_string()), Some("y".to_string())]);
        Ok(())
    }

    #[test]
    fn escritor_escapa_xml_y_borra_control() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("esc.xlsx");
        let opciones = OpcionesEscritorXlsx {
            columnas: Some(vec!["A".into(), "B".into()]),
            ..Default::default()
        };
        let mut escritor = EscritorXlsx::nuevo(&ruta, opciones)?;
        escritor.escribir(
            &df!(
                "A" => ["a&b", "<tag>", " espacios ", "normal"],
                "B" => ["1", "x\u{1}y", "z", "w"],
            )?,
            None,
        )?;
        escritor.cerrar()?;

        let filas = leer_filas_texto(&ruta);
        let col_a: Vec<_> = filas.iter().map(|f| f[0].clone()).collect();
        assert_eq!(
            col_a,
            vec![
                Some("a&b".to_string()),
                Some("<tag>".to_string()),
                Some(" espacios ".to_string()),
                Some("normal".to_string())
            ]
        );
        assert_eq!(
            filas[1][1],
            Some("xy".to_string()),
            "el carácter de control se borra"
        );
        Ok(())
    }

    #[test]
    fn escritor_sin_datos_deja_libro_valido() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("vacio.xlsx");
        let opciones = OpcionesEscritorXlsx {
            columnas: Some(vec!["A".into()]),
            ..Default::default()
        };
        let mut escritor = EscritorXlsx::nuevo(&ruta, opciones)?;
        escritor.cerrar()?;

        use calamine::{open_workbook_auto, Reader};
        let libro = open_workbook_auto(&ruta).unwrap();
        assert_eq!(libro.sheet_names(), vec!["Hoja1".to_string()]);
        Ok(())
    }

    #[test]
    fn escritor_hojas_nombradas_intercaladas() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("multi.xlsx");
        let mut escritor = EscritorXlsx::nuevo(&ruta, OpcionesEscritorXlsx::default())?;
        escritor.escribir(&df!("A" => ["1"])?, Some("Validas"))?;
        escritor.escribir(&df!("A" => ["x"], "Motivo" => ["m"])?, Some("Eliminadas"))?;
        escritor.escribir(&df!("A" => ["2"])?, Some("Validas"))?; // vuelve a la 1ª
        escritor.cerrar()?;

        let hojas = leer_hojas(&ruta);
        let mut nombres: Vec<_> = hojas.keys().cloned().collect();
        nombres.sort();
        let mut esperado = vec!["Validas".to_string(), "Eliminadas".to_string()];
        esperado.sort();
        assert_eq!(nombres, esperado);

        let validas = &hojas["Validas"];
        let col_a: Vec<_> = validas
            .column("A")?
            .str()?
            .iter()
            .map(|v| v.map(str::to_string))
            .collect();
        assert_eq!(col_a, vec![Some("1".to_string()), Some("2".to_string())]);
        let nombres: Vec<&str> = hojas["Eliminadas"]
            .get_column_names()
            .iter()
            .map(|s| s.as_str())
            .collect();
        assert_eq!(nombres, vec!["A", "Motivo"]);
        Ok(())
    }

    #[test]
    fn escritor_divide_en_hojas() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("div.xlsx");
        let opciones = OpcionesEscritorXlsx {
            columnas: Some(vec!["A".into()]),
            filas_por_hoja: 3,
            numerar_siempre: true,
            ..Default::default()
        };
        let mut escritor = EscritorXlsx::nuevo(&ruta, opciones)?;
        let valores: Vec<String> = (0..7).map(|i| i.to_string()).collect();
        escritor.escribir(&df!("A" => valores)?, None)?;
        let total = escritor.total;
        escritor.cerrar()?;

        let hojas = leer_hojas(&ruta);
        let mut nombres: Vec<_> = hojas.keys().cloned().collect();
        nombres.sort();
        assert_eq!(nombres, vec!["Hoja1_1", "Hoja1_2", "Hoja1_3"]);
        let mut alturas: Vec<_> = hojas.iter().map(|(n, d)| (n.clone(), d.height())).collect();
        alturas.sort();
        assert_eq!(
            alturas,
            vec![
                ("Hoja1_1".to_string(), 3),
                ("Hoja1_2".to_string(), 3),
                ("Hoja1_3".to_string(), 1)
            ]
        );
        assert_eq!(total, 7);
        Ok(())
    }

    #[test]
    fn escritor_nunca_supera_el_limite_de_excel() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let opciones = OpcionesEscritorXlsx {
            columnas: Some(vec!["A".into()]),
            filas_por_hoja: 99_999_999,
            ..Default::default()
        };
        let mut escritor = EscritorXlsx::nuevo(tmp.path().join("lim.xlsx"), opciones)?;
        assert_eq!(escritor.filas_por_hoja, MAX_FILAS_EXCEL);
        escritor.cerrar()?;
        Ok(())
    }

    #[test]
    fn escritor_recorta_vacias_finales_pero_no_interiores() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("vac.xlsx");
        let opciones = OpcionesEscritorXlsx {
            columnas: Some(vec!["A".into()]),
            ..Default::default()
        };
        let mut escritor = EscritorXlsx::nuevo(&ruta, opciones)?;
        escritor.escribir(&df!("A" => ["x", "", ""])?, None)?; // 2 vacías…
        escritor.escribir(&df!("A" => ["y"])?, None)?; // …eran interiores
        escritor.escribir(&df!("A" => ["", ""])?, None)?; // vacías finales → fuera
        escritor.cerrar()?;

        let xml = xml_hoja(&ruta, 1);
        // Se verifica la NUMERACIÓN, no solo la cantidad: filas correlativas
        // sin huecos son lo que permite a un lector posicionar las celdas.
        let filas: Vec<&str> = xml.matches(r#"<row r=""#).collect();
        assert_eq!(filas.len(), 5, "cabecera + x + 2 vacías + y");
        for n in 1..=5 {
            assert!(
                xml.contains(&format!(r#"<row r="{n}"#)),
                "falta la fila {n}: {xml}"
            );
        }
        assert!(xml.trim_end().ends_with(
            r#"<row r="5"><c r="A5" t="inlineStr"><is><t>y</t></is></c></row></sheetData></worksheet>"#
        ));
        Ok(())
    }

    #[test]
    fn emitir_vacias_trocea_en_bloques_igual_que_emitir_datos() -> CoreResult<()> {
        // `emitir_vacias` debe trocear en `FILAS_POR_BLOQUE` igual que
        // `emitir_datos`: armar un único `String` de hasta `filas_por_hoja`
        // filas de un tirón rompería el diseño de memoria acotada del
        // módulo. Esta racha de vacías interiores cruza ese límite, así que
        // ejerce el camino multi-iteración.
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("vacias_grandes.xlsx");
        let opciones = OpcionesEscritorXlsx {
            columnas: Some(vec!["A".into()]),
            ..Default::default()
        };
        let mut escritor = EscritorXlsx::nuevo(&ruta, opciones)?;
        escritor.escribir(&df!("A" => ["dato"])?, None)?;
        let vacias: Vec<String> = vec![String::new(); 200_001];
        escritor.escribir(&df!("A" => vacias)?, None)?;
        escritor.escribir(&df!("A" => ["final"])?, None)?;
        let total = escritor.total;
        escritor.cerrar()?;

        assert_eq!(total, 200_003, "dato + 200_001 vacías interiores + final");
        let filas = leer_filas_texto(&ruta);
        assert_eq!(filas.len(), 200_003);
        assert_eq!(filas[0], vec![Some("dato".to_string())]);
        assert_eq!(filas[200_002], vec![Some("final".to_string())]);
        Ok(())
    }

    #[test]
    fn escritor_puede_no_recortar() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("sin.xlsx");
        let opciones = OpcionesEscritorXlsx {
            columnas: Some(vec!["A".into()]),
            recortar_vacias: false,
            ..Default::default()
        };
        let mut escritor = EscritorXlsx::nuevo(&ruta, opciones)?;
        escritor.escribir(&df!("A" => ["x", ""])?, None)?;
        assert_eq!(
            escritor.total, 2,
            "sin recorte, la vacía final también se escribe"
        );
        escritor.cerrar()?;
        Ok(())
    }

    #[test]
    fn escritor_sanea_nombre_de_hoja() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("san.xlsx");
        let mut escritor = EscritorXlsx::nuevo(&ruta, OpcionesEscritorXlsx::default())?;
        escritor.escribir(&df!("A" => ["1"])?, Some("nombre/con:caracteres*malos"))?;
        escritor.cerrar()?;

        use calamine::{open_workbook_auto, Reader};
        let libro = open_workbook_auto(&ruta).unwrap();
        let nombre = &libro.sheet_names()[0];
        // Literales y no la constante del código: un test que se compara
        // contra la misma constante que ejercita pasaría igual si alguien la
        // vaciara por error.
        assert!(!nombre.contains(['[', ']', ':', '*', '?', '/', '\\']));
        assert!(nombre.chars().count() <= 31);
        Ok(())
    }

    #[test]
    fn escritor_sanea_caracteres_de_control_del_nombre_de_hoja() -> CoreResult<()> {
        // Un nombre de hoja con un byte de control (0x00-0x1F) es ilegal en
        // XML 1.0 y produciría un `xl/workbook.xml` inválido: el .xlsx debe
        // seguir siendo reabrible y el byte no debe sobrevivir.
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("control.xlsx");
        let mut escritor = EscritorXlsx::nuevo(&ruta, OpcionesEscritorXlsx::default())?;
        escritor.escribir(&df!("A" => ["1"])?, Some("nombre\u{1}con\u{2}control"))?;
        escritor.cerrar()?;

        use calamine::{open_workbook_auto, Reader};
        let libro = open_workbook_auto(&ruta).unwrap();
        let nombre = &libro.sheet_names()[0];
        assert!(!nombre.chars().any(|c| (c as u32) < 0x20));
        Ok(())
    }

    #[test]
    fn escritor_abortar_borra_el_archivo() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("roto.xlsx");
        let opciones = OpcionesEscritorXlsx {
            columnas: Some(vec!["A".into()]),
            ..Default::default()
        };
        let mut escritor = EscritorXlsx::nuevo(&ruta, opciones)?;
        escritor.escribir(&df!("A" => ["1"])?, None)?;
        let ruta_final = escritor.ruta.clone();
        escritor.abortar()?;
        assert!(!ruta_final.exists(), "un xlsx a medias solo estorba");
        Ok(())
    }

    #[test]
    fn escritor_acepta_columnas_numericas() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("num.xlsx");
        let df = df!(
            "Texto" => ["a", "b"],
            "Entero" => [10u64, 20u64],
            "Flotante" => [Some(1.5f64), None],
        )?;
        let columnas: Vec<String> = df.get_column_names().iter().map(|s| s.to_string()).collect();
        let opciones = OpcionesEscritorXlsx {
            columnas: Some(columnas),
            ..Default::default()
        };
        let mut escritor = EscritorXlsx::nuevo(&ruta, opciones)?;
        escritor.escribir(&df, None)?;
        let total = escritor.total;
        escritor.cerrar()?;

        let filas = leer_filas_texto(&ruta);
        assert_eq!(filas[0][1], Some("10".to_string()));
        assert_eq!(filas[1][1], Some("20".to_string()));
        assert_eq!(total, 2);
        Ok(())
    }

    #[test]
    fn escritor_alinea_bloques_con_otro_orden() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("alin.xlsx");
        let avisos = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let avisos_clon = avisos.clone();
        let opciones = OpcionesEscritorXlsx {
            columnas: Some(vec!["A".into(), "B".into()]),
            ..Default::default()
        };
        let mut escritor = EscritorXlsx::con_avisador(&ruta, opciones, move |m: &str| {
            avisos_clon.borrow_mut().push(m.to_string())
        })?;
        escritor.escribir(&df!("A" => ["a1"], "B" => ["b1"])?, None)?;
        escritor.escribir(&df!("B" => ["b2"], "A" => ["a2"], "EXTRA" => ["z"])?, None)?;
        escritor.escribir(&df!("A" => ["a3"])?, None)?; // falta B
        escritor.cerrar()?;

        let filas = leer_filas_texto(&ruta);
        assert_eq!(
            filas[1],
            vec![Some("a2".to_string()), Some("b2".to_string())],
            "el bloque invertido se realinea"
        );
        assert_eq!(
            filas[2],
            vec![Some("a3".to_string()), None],
            "la columna faltante se rellena"
        );
        assert!(
            avisos.borrow().iter().any(|a| a.contains("EXTRA")),
            "debe AVISAR de lo que descarta"
        );
        Ok(())
    }

    #[test]
    fn escribir_tras_cerrar_da_error_en_vez_de_panic() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("tras_cerrar.xlsx");
        let opciones = OpcionesEscritorXlsx {
            columnas: Some(vec!["A".into()]),
            ..Default::default()
        };
        let mut escritor = EscritorXlsx::nuevo(&ruta, opciones)?;
        escritor.escribir(&df!("A" => ["1"])?, None)?;
        escritor.cerrar()?;

        assert!(
            escritor.escribir(&df!("A" => ["2"])?, None).is_err(),
            "escribir() tras cerrar() debe devolver Err, no entrar en pánico"
        );
        Ok(())
    }

    #[test]
    fn escribir_tras_abortar_da_error_en_vez_de_panic() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("tras_abortar.xlsx");
        let opciones = OpcionesEscritorXlsx {
            columnas: Some(vec!["A".into()]),
            ..Default::default()
        };
        let mut escritor = EscritorXlsx::nuevo(&ruta, opciones)?;
        escritor.escribir(&df!("A" => ["1"])?, None)?;
        escritor.abortar()?;

        assert!(
            escritor.escribir(&df!("A" => ["2"])?, None).is_err(),
            "escribir() tras abortar() debe devolver Err, no entrar en pánico"
        );
        Ok(())
    }

    #[test]
    fn sufijo_de_desempate_de_hoja_se_compone_siempre_sobre_el_nombre_original() -> CoreResult<()> {
        // Colisión que necesita DOS intentos de desempate: el sufijo debe
        // recalcularse siempre desde el nombre ORIGINAL ("X_2_2"), no
        // componerse sobre el ya truncado del intento anterior ("X_2_1_2").
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("desempate.xlsx");
        let opciones = OpcionesEscritorXlsx {
            columnas: Some(vec!["A".into()]),
            hoja_por_defecto: "X".to_string(),
            filas_por_hoja: 1,
            ..Default::default()
        };
        let mut escritor = EscritorXlsx::nuevo(&ruta, opciones)?;
        // 2 filas con filas_por_hoja=1 => hojas "X" y "X_2".
        escritor.escribir(&df!("A" => ["1", "2"])?, None)?;
        // Ocupa de antemano el nombre que el desempate probaría primero.
        escritor.escribir(&df!("A" => ["3"])?, Some("X_2_1"))?;
        // Esta colisiona con la "X_2" de arriba y debe caer en "X_2_2".
        escritor.escribir(&df!("A" => ["4"])?, Some("X_2"))?;
        escritor.cerrar()?;

        let hojas = leer_hojas(&ruta);
        let mut nombres: Vec<_> = hojas.keys().cloned().collect();
        nombres.sort();
        assert_eq!(nombres, vec!["X", "X_2", "X_2_1", "X_2_2"]);
        Ok(())
    }
}
