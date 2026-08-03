use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};

use polars::prelude::*;

use crate::error::CoreResult;
use crate::rutas::ruta_unica;

/// `true` si `valor` dispara "CSV/Formula Injection": Excel/Sheets pueden
/// interpretar una celda que empieza con `=`, `+`, `-` o `@` como fórmula al
/// reabrir el CSV y ejecutar lo que traiga (p. ej. `=cmd|'/C calc'!A1`).
fn dispara_inyeccion_formula(valor: &str) -> bool {
    matches!(valor.chars().next(), Some('=' | '+' | '-' | '@'))
}

/// Antepone una comilla simple a las celdas de texto que disparan inyección
/// de fórmulas (ver [`dispara_inyeccion_formula`]): Excel/Sheets tratan esa
/// comilla como marca de "forzar texto" y no la muestran, igual que si el
/// usuario la hubiera tipeado a mano. `EscritorXlsx` no necesita esto porque
/// fuerza `inlineStr` (nunca fórmula); acá era la asimetría real, pese a que
/// ambos escritores se documentan como intercambiables vía `EscritorSalida`.
///
/// Solo toca columnas de texto: un número negativo (`-5`) en una columna
/// numérica no es texto con un prefijo peligroso, es un valor numérico
/// válido, y `CsvWriter` ya lo escribe sin comillas.
///
/// Nota: esta comilla es una convención de Excel/Sheets al ABRIR el CSV, no
/// un carácter especial del formato CSV en sí — si este mismo archivo se
/// vuelve a leer con un lector que no la reconozca (p. ej. Polars), la
/// comilla aparece como parte literal del valor. Es el trade-off estándar de
/// esta mitigación (documentado así por OWASP), aceptable porque estos CSV
/// son salidas pensadas para abrirse en una hoja de cálculo, no para
/// reingresar como entrada de otra herramienta de este mismo workspace.
/// Antepone una comilla simple a un NOMBRE de columna que dispara inyección
/// de fórmulas (ver [`dispara_inyeccion_formula`]). Sin esto, una cabecera de
/// ORIGEN maliciosa/corrupta (p. ej. una celda de cabecera de un xlsx externo
/// con el texto `=cmd|'/C calc'!A1`, que termina como nombre de columna acá)
/// se escribía tal cual en la fila de cabecera del CSV de salida, sin la
/// misma protección que ya tienen los VALORES vía `neutralizar_formulas` —
/// era la mitad de la asimetría con `EscritorXlsx` que quedó sin cerrar.
fn nombre_columna_saneado(nombre: &str) -> String {
    if dispara_inyeccion_formula(nombre) {
        format!("'{nombre}")
    } else {
        nombre.to_string()
    }
}

fn neutralizar_formulas(df: &mut DataFrame) -> CoreResult<()> {
    let nombres: Vec<String> = df.get_column_names().iter().map(|s| s.to_string()).collect();
    for nombre in nombres {
        let columna = df.column(&nombre)?;
        if columna.dtype() != &DataType::String {
            continue;
        }
        let serie = columna.str()?;
        if !serie.iter().flatten().any(dispara_inyeccion_formula) {
            continue; // camino rápido: nada peligroso en esta columna
        }
        let saneada: Vec<Option<String>> = serie
            .iter()
            .map(|v| {
                v.map(|s| {
                    if dispara_inyeccion_formula(s) {
                        format!("'{s}")
                    } else {
                        s.to_string()
                    }
                })
            })
            .collect();
        df.with_column(Column::new(nombre.as_str().into(), saneada))?;
    }
    Ok(())
}

/// Escribe filas a un CSV en streaming (la cabecera, una sola vez).
///
/// Alinea cada bloque a la cabecera igual que `EscritorXlsx::alinear`
/// (faltantes → vacías; sobrantes → se descartan CON AVISO, una vez por
/// columna). Una columna faltante se alinea, NO hace fallar `escribir()`: esa
/// simetría con `EscritorXlsx` es lo que sostiene la promesa de que ambos son
/// intercambiables vía `EscritorSalida`.
pub struct EscritorCsv {
    pub ruta: PathBuf,
    columnas: Vec<String>,
    fh: File,
    primero: bool,
    cerrado: bool,
    avisar: Box<dyn FnMut(&str)>,
    extras_avisadas: HashSet<String>,
    pub total: usize,
}

impl EscritorCsv {
    pub fn nuevo(ruta: impl AsRef<Path>, columnas: Vec<String>) -> CoreResult<Self> {
        Self::con_avisador(ruta, columnas, |_| {})
    }

    pub fn con_avisador(
        ruta: impl AsRef<Path>,
        columnas: Vec<String>,
        avisar: impl FnMut(&str) + 'static,
    ) -> CoreResult<Self> {
        let ruta = ruta_unica(ruta);
        let fh = File::create(&ruta)?;
        Ok(Self {
            ruta,
            columnas,
            fh,
            primero: true,
            cerrado: false,
            avisar: Box::new(avisar),
            extras_avisadas: HashSet::new(),
            total: 0,
        })
    }

    /// Alinea el bloque a `self.columnas`, igual que `EscritorXlsx::alinear`.
    /// Lógica compartida vía `crate::alineacion`.
    fn alinear(&mut self, df: &DataFrame) -> CoreResult<DataFrame> {
        let Self {
            columnas,
            extras_avisadas,
            avisar,
            ..
        } = self;
        crate::alineacion::alinear_columnas(df, columnas, extras_avisadas, &mut |nuevas: &[String]| {
            avisar(&format!(
                "columnas no vistas en la cabecera ({}) — se descartan.",
                nuevas.join(", ")
            ));
        })
    }

    /// `_hoja` se ignora (un CSV no tiene hojas); está para que el escritor
    /// sea intercambiable con `EscritorXlsx`.
    pub fn escribir(&mut self, df: &DataFrame, _hoja: Option<&str>) -> CoreResult<()> {
        if self.cerrado {
            return Err(std::io::Error::other(
                "EscritorCsv: no se puede escribir después de cerrar()/abortar()",
            )
            .into());
        }
        if df.height() == 0 {
            return Ok(());
        }
        let mut df = self.alinear(df)?;
        neutralizar_formulas(&mut df)?;
        if self.primero {
            // Renombrar acá (recién ahora que `alinear()` ya hizo su
            // matching por nombre exacto contra `self.columnas`) para no
            // romper la alineación de bloques futuros: solo afecta la fila
            // de cabecera que se escribe a continuación.
            let nombres: Vec<String> = df.get_column_names().iter().map(|s| s.to_string()).collect();
            for nombre in nombres {
                let saneado = nombre_columna_saneado(&nombre);
                if saneado != nombre {
                    df.rename(&nombre, saneado.into())?;
                }
            }
        }
        CsvWriter::new(&mut self.fh)
            .include_header(self.primero)
            .finish(&mut df)?;
        self.primero = false;
        self.total += df.height();
        Ok(())
    }

    /// Finaliza el CSV. Idempotente (llamarlo dos veces no hace nada).
    ///
    /// Si la escritura de la cabecera final falla, el archivo a medias se
    /// BORRA acá mismo (igual que `abortar()`) antes de propagar el error, y
    /// `cerrado` se marca `true` solo AL FINAL: marcarlo como primer paso
    /// neutralizaría tanto a `abortar()` como al `Drop` automático (ambos son
    /// no-op si `cerrado` ya es `true`) y el CSV a medias quedaría en disco
    /// sin ninguna forma de limpiarlo. Mismo criterio que
    /// `EscritorXlsx::cerrar()`.
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
        if self.primero {
            // nunca se escribió nada: al menos la cabecera.
            let columnas: Vec<Column> = self
                .columnas
                .iter()
                .map(|c| {
                    Column::new(
                        PlSmallStr::from(nombre_columna_saneado(c).as_str()),
                        Vec::<Option<&str>>::new(),
                    )
                })
                .collect();
            let mut vacio = DataFrame::new_infer_height(columnas)?;
            CsvWriter::new(&mut self.fh)
                .include_header(true)
                .finish(&mut vacio)?;
        }
        Ok(())
    }

    pub fn abortar(&mut self) -> CoreResult<()> {
        if self.cerrado {
            return Ok(());
        }
        self.cerrado = true;
        let _ = std::fs::remove_file(&self.ruta);
        Ok(())
    }
}

impl Drop for EscritorCsv {
    fn drop(&mut self) {
        let _ = self.cerrar();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escritor_csv_basico() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("s.csv");
        {
            let mut escritor = EscritorCsv::nuevo(&ruta, vec!["A".into(), "B".into()])?;
            escritor.escribir(&df!("A" => ["1"], "B" => ["x"])?, None)?;
            escritor.escribir(&df!("A" => ["2"], "B" => ["y"])?, None)?;
            assert_eq!(escritor.total, 2);
            escritor.cerrar()?;
        }
        let leido = std::fs::read_to_string(&ruta)?;
        assert_eq!(leido.trim(), "A,B\n1,x\n2,y");
        Ok(())
    }

    #[test]
    fn escribir_rellena_columna_faltante_con_vacio_en_vez_de_fallar() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("faltante.csv");
        {
            let mut escritor = EscritorCsv::nuevo(&ruta, vec!["A".into(), "B".into()])?;
            escritor.escribir(&df!("A" => ["1"])?, None)?;
            escritor.cerrar()?;
        }
        let leido = std::fs::read_to_string(&ruta)?;
        assert_eq!(leido.trim(), "A,B\n1,");
        Ok(())
    }

    #[test]
    fn escribir_avisa_y_descarta_columnas_sobrantes_una_sola_vez() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("sobrante.csv");
        let avisos = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let avisos_clon = avisos.clone();
        {
            let mut escritor = EscritorCsv::con_avisador(&ruta, vec!["A".into()], move |m: &str| {
                avisos_clon.borrow_mut().push(m.to_string())
            })?;
            escritor.escribir(&df!("A" => ["1"], "Extra" => ["x"])?, None)?;
            escritor.escribir(&df!("A" => ["2"], "Extra" => ["y"])?, None)?;
            escritor.cerrar()?;
        }
        let leido = std::fs::read_to_string(&ruta)?;
        assert_eq!(leido.trim(), "A\n1\n2");
        assert_eq!(
            avisos.borrow().len(),
            1,
            "debe avisar una sola vez por columna sobrante"
        );
        Ok(())
    }

    #[test]
    fn escribir_antepone_comilla_a_celdas_que_empiezan_con_disparador_de_formula() -> CoreResult<()> {
        // Mitigación de "CSV/Formula Injection": una celda que empiece con
        // =, +, - o @ puede ejecutarse como fórmula al reabrir el archivo en
        // Excel/Sheets. `EscritorXlsx` no lo necesita porque fuerza
        // `inlineStr`, pero ambos se ofrecen como intercambiables.
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("formulas.csv");
        {
            let mut escritor = EscritorCsv::nuevo(&ruta, vec!["A".into()])?;
            escritor.escribir(
                &df!("A" => ["=cmd|'/C calc'!A1", "+1+1", "-5-PACK", "@Home", "Producto normal"])?,
                None,
            )?;
            escritor.cerrar()?;
        }
        let leido = std::fs::read_to_string(&ruta)?;
        assert_eq!(
            leido.trim(),
            "A\n'=cmd|'/C calc'!A1\n'+1+1\n'-5-PACK\n'@Home\nProducto normal"
        );
        Ok(())
    }

    #[test]
    fn escribir_antepone_comilla_a_un_nombre_de_columna_que_dispara_formula() -> CoreResult<()> {
        // Los NOMBRES de columna también se neutralizan: una cabecera venida
        // de un xlsx externo llega igual de sin confiar que los valores.
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("cabecera_peligrosa.csv");
        {
            let mut escritor = EscritorCsv::nuevo(&ruta, vec!["=cmd|'/C calc'!A1".into(), "B".into()])?;
            escritor.escribir(&df!("=cmd|'/C calc'!A1" => ["1"], "B" => ["x"])?, None)?;
            escritor.cerrar()?;
        }
        let leido = std::fs::read_to_string(&ruta)?;
        assert_eq!(leido.trim(), "'=cmd|'/C calc'!A1,B\n1,x");
        Ok(())
    }

    #[test]
    fn cabecera_peligrosa_tambien_se_sanea_cuando_no_hay_datos() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("cabecera_peligrosa_vacio.csv");
        {
            let mut escritor = EscritorCsv::nuevo(&ruta, vec!["=peligroso".into(), "B".into()])?;
            escritor.cerrar()?;
        }
        let leido = std::fs::read_to_string(&ruta)?;
        assert_eq!(leido.trim(), "'=peligroso,B");
        Ok(())
    }

    #[test]
    fn escribir_no_toca_numeros_negativos_de_una_columna_numerica() -> CoreResult<()> {
        // Un -5 en una columna numérica no es texto con un prefijo
        // peligroso: es un valor numérico válido, y no debe salir con
        // comilla (`CsvWriter` ya lo escribe sin comillas de por sí).
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("numeros.csv");
        {
            let mut escritor = EscritorCsv::nuevo(&ruta, vec!["N".into()])?;
            escritor.escribir(&df!("N" => [-5i64, 3])?, None)?;
            escritor.cerrar()?;
        }
        let leido = std::fs::read_to_string(&ruta)?;
        assert_eq!(leido.trim(), "N\n-5\n3");
        Ok(())
    }

    #[test]
    fn escritor_csv_sin_datos_deja_cabecera() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("v.csv");
        {
            let mut escritor = EscritorCsv::nuevo(&ruta, vec!["A".into(), "B".into()])?;
            escritor.cerrar()?;
        }
        let leido = std::fs::read_to_string(&ruta)?;
        assert_eq!(leido.trim(), "A,B");
        Ok(())
    }

    #[test]
    fn escribir_tras_cerrar_da_error_en_vez_de_duplicar_la_cabecera() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("tras_cerrar.csv");
        let mut escritor = EscritorCsv::nuevo(&ruta, vec!["A".into()])?;
        escritor.escribir(&df!("A" => ["1"])?, None)?;
        escritor.cerrar()?;

        assert!(
            escritor.escribir(&df!("A" => ["2"])?, None).is_err(),
            "escribir() tras cerrar() debe devolver Err, no duplicar la cabecera"
        );
        Ok(())
    }

    #[test]
    fn escribir_tras_abortar_da_error() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("tras_abortar.csv");
        let mut escritor = EscritorCsv::nuevo(&ruta, vec!["A".into()])?;
        escritor.escribir(&df!("A" => ["1"])?, None)?;
        escritor.abortar()?;

        assert!(
            escritor.escribir(&df!("A" => ["2"])?, None).is_err(),
            "escribir() tras abortar() debe devolver Err"
        );
        Ok(())
    }
}
