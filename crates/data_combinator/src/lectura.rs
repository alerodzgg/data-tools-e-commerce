use std::fs::File;
use std::path::{Path, PathBuf};

use commerce_core::CoreResult;
use polars::prelude::*;

use crate::constantes::{UmbralesLoteCsv, SOPORTADOS_ARCHIVOS};
use crate::normalizar::normalizar;

/// Archivos soportados de la carpeta de entrada, ordenados por nombre.
pub fn listar_archivos(ruta_entrada: &Path) -> Vec<PathBuf> {
    let mut archivos: Vec<PathBuf> = std::fs::read_dir(ruta_entrada)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entrada| entrada.path())
        .filter(|ruta| ruta.is_file())
        .filter(|ruta| {
            ruta.extension()
                .map(|e| SOPORTADOS_ARCHIVOS.contains(&e.to_string_lossy().to_lowercase().as_str()))
                .unwrap_or(false)
        })
        .collect();
    archivos.sort();
    archivos
}

/// Lee un CSV por LOTES (streaming, nunca el archivo entero en RAM). Usa el
/// crate `csv` en vez del motor de Polars: la versión de Polars usada aquí ya
/// no expone una API pública de lectura por lotes, y volver a parsear el
/// archivo desde el principio en cada lote (con `skip_rows`/`n_rows`) sería
/// O(n²) — justo lo que este diseño evita.
///
/// Nota de fidelidad: las cabeceras vacías/duplicadas de un CSV se dejan tal
/// cual, sin la convención canónica `Columna_N` que sí aplica a XLSX.
pub struct LotesCsv {
    lector: csv::Reader<File>,
    columnas: Vec<String>,
    filas_por_lote: usize,
    terminado: bool,
    filas_descartadas: usize,
}

/// Duplicada antes de forma idéntica en `combinar.rs`.
pub(crate) fn csv_a_io(error: csv::Error) -> std::io::Error {
    std::io::Error::other(error)
}

impl LotesCsv {
    pub fn abrir(archivo: &Path, umbrales: UmbralesLoteCsv) -> CoreResult<Self> {
        let mut lector = csv::ReaderBuilder::new()
            .flexible(true)
            .from_path(archivo)
            .map_err(csv_a_io)?;
        let columnas: Vec<String> = lector
            .headers()
            .map_err(csv_a_io)?
            .iter()
            .map(|s| s.to_string())
            .collect();
        let filas_por_lote = umbrales.filas_por_lote(columnas.len());
        Ok(Self {
            lector,
            columnas,
            filas_por_lote,
            terminado: false,
            filas_descartadas: 0,
        })
    }

    /// Filas descartadas por un error de lectura puntual (p. ej. bytes no
    /// UTF-8) desde que se abrió el lector. Para que el llamador pueda
    /// avisar al usuario sin abortar el procesamiento del resto del archivo.
    pub fn filas_descartadas(&self) -> usize {
        self.filas_descartadas
    }
}

impl Iterator for LotesCsv {
    type Item = CoreResult<DataFrame>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.terminado {
            return None;
        }
        let ancho = self.columnas.len();
        let mut columnas_datos: Vec<Vec<Option<String>>> = vec![Vec::new(); ancho];
        let mut leidas = 0usize;
        let mut registro = csv::StringRecord::new();
        let mut errores_consecutivos = 0u32;
        const MAX_ERRORES_CONSECUTIVOS: u32 = 100;

        loop {
            match self.lector.read_record(&mut registro) {
                Ok(true) => {
                    errores_consecutivos = 0;
                    for (i, columna) in columnas_datos.iter_mut().enumerate() {
                        columna.push(registro.get(i).map(|s| s.to_string()));
                    }
                    leidas += 1;
                    if leidas >= self.filas_por_lote {
                        break;
                    }
                }
                Ok(false) => {
                    self.terminado = true;
                    break;
                }
                // Fila con error de parseo (p. ej. bytes no-UTF8): se salta,
                // igual que `ignore_errors=True` en el original — pero si el
                // error se repite sin parar (una falla de E/S real, no un
                // simple corte de codificación puntual), no vale la pena
                // reintentar para siempre: se corta el lote y se termina.
                Err(_) => {
                    self.filas_descartadas += 1;
                    errores_consecutivos += 1;
                    if errores_consecutivos >= MAX_ERRORES_CONSECUTIVOS {
                        self.terminado = true;
                        break;
                    }
                    continue;
                }
            }
        }

        if leidas == 0 {
            return None;
        }
        let series: Vec<Column> = self
            .columnas
            .iter()
            .zip(columnas_datos)
            .map(|(nombre, valores)| Column::new(nombre.as_str().into(), valores))
            .collect();
        Some(DataFrame::new_infer_height(series).map_err(Into::into))
    }
}

/// Itera, normalizados, los chunks de todos los `archivos`: por lotes (CSV) o
/// hoja a hoja (XLSX). Llama a `on_chunk` una vez por chunk en vez de
/// acumularlos: así el llamador decide qué hacer con cada uno sin que este
/// iterador retenga más de un chunk en memoria a la vez.
///
/// Un archivo que falla al leerse se avisa (`avisar`) y se salta: no aborta
/// el procesamiento del resto.
pub fn iter_chunks(
    archivos: &[PathBuf],
    columnas: &[String],
    hojas_excluir: &[String],
    umbral_lote: UmbralesLoteCsv,
    mut avisar: impl FnMut(&str),
    mut on_chunk: impl FnMut(DataFrame) -> CoreResult<()>,
) -> CoreResult<()> {
    let excluir: Vec<&str> = hojas_excluir.iter().map(|s| s.as_str()).collect();

    for archivo in archivos {
        let extension = archivo
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let resultado: CoreResult<()> = if extension == "ipc" {
            // Formato de INTERCAMBIO: lo escribió otra herramienta del
            // workspace, no un humano. Se carga directo a los buffers de
            // Polars, sin parser de texto de por medio.
            (|| -> CoreResult<()> {
                for bloque in commerce_core::leer_ipc(archivo)? {
                    on_chunk(normalizar(&bloque, columnas)?)?;
                }
                Ok(())
            })()
        } else if extension == "csv" {
            (|| -> CoreResult<()> {
                let mut lotes = LotesCsv::abrir(archivo, umbral_lote)?;
                for lote in &mut lotes {
                    on_chunk(normalizar(&lote?, columnas)?)?;
                }
                if lotes.filas_descartadas() > 0 {
                    avisar(&format!(
                        "'{}': se descartaron {} fila(s) con error de lectura (bytes no UTF-8 u otro problema de formato).",
                        archivo.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                        lotes.filas_descartadas()
                    ));
                }
                Ok(())
            })()
        } else {
            let hojas = commerce_core::iter_hojas_xlsx(archivo, Some(&excluir), &mut avisar);
            (|| -> CoreResult<()> {
                for hoja in hojas {
                    on_chunk(normalizar(&hoja, columnas)?)?;
                }
                Ok(())
            })()
        };
        if let Err(error) = resultado {
            avisar(&format!(
                "Error al leer '{}': {error}",
                archivo
                    .file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lotes_csv_no_pierde_filas_con_saltos_de_linea() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("d.csv");
        let mut df = df!("A" => ["uno", "dos\ncon salto", "tres"], "B" => ["1", "2", "3"])?;
        let mut archivo = File::create(&ruta)?;
        CsvWriter::new(&mut archivo).finish(&mut df)?;

        let lotes: Vec<DataFrame> =
            LotesCsv::abrir(&ruta, UmbralesLoteCsv::default())?.collect::<CoreResult<_>>()?;
        let total: usize = lotes.iter().map(polars::prelude::DataFrame::height).sum();
        assert_eq!(total, 3);
        Ok(())
    }

    #[test]
    fn lotes_csv_descarta_filas_con_bytes_invalidos_y_lo_reporta() -> CoreResult<()> {
        // Una fila ilegible (p. ej. bytes no UTF-8) se descarta, pero se
        // cuenta y se avisa: descartarla en silencio ocultaría pérdida real.
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("invalido.csv");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"A\n");
        bytes.extend_from_slice(b"uno\n");
        bytes.extend_from_slice(&[0xFF, 0xFE, b'\n']); // fila con bytes no UTF-8
        bytes.extend_from_slice(b"dos\n");
        std::fs::write(&ruta, &bytes)?;

        let mut lotes = LotesCsv::abrir(&ruta, UmbralesLoteCsv::default())?;
        let mut valores = Vec::new();
        for lote in &mut lotes {
            let df = lote?;
            valores.extend(df.column("A")?.str()?.iter().map(|v| v.unwrap().to_string()));
        }
        assert_eq!(
            valores,
            vec!["uno".to_string(), "dos".to_string()],
            "las filas válidas deben sobrevivir a la fila con bytes inválidos"
        );
        assert_eq!(
            lotes.filas_descartadas(),
            1,
            "la fila con bytes inválidos debe contarse como descartada"
        );
        Ok(())
    }

    #[test]
    fn lotes_csv_no_convierte_codigos_en_numeros() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("c.csv");
        let mut df = df!("C" => ["007", "1e5"])?;
        let mut archivo = File::create(&ruta)?;
        CsvWriter::new(&mut archivo).finish(&mut df)?;

        let lotes: Vec<DataFrame> =
            LotesCsv::abrir(&ruta, UmbralesLoteCsv::default())?.collect::<CoreResult<_>>()?;
        let valores: Vec<_> = lotes[0]
            .column("C")?
            .str()?
            .iter()
            .map(|v| v.map(str::to_string))
            .collect();
        assert_eq!(valores, vec![Some("007".to_string()), Some("1e5".to_string())]);
        Ok(())
    }
}
