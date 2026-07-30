use std::path::{Path, PathBuf};

use commerce_core::{CoreResult, EscritorSalida};
use polars::prelude::DataFrame;

/// Construye el escritor real para una ruta concreta.
type Fabrica = dyn FnMut(&Path) -> CoreResult<Box<dyn EscritorSalida>>;

/// Reparte la salida en VARIOS archivos de `filas_por_archivo` filas
/// (`nombre_1.ext`, `nombre_2.ext`, …), rotando de archivo al llenarse.
///
/// Envuelve al escritor real (XLSX o CSV) que `fabrica` construye para cada
/// ruta, vía el trait [`EscritorSalida`]: mantiene la memoria plana igual que
/// ellos (solo un escritor abierto a la vez).
pub struct EscritorParticionado {
    fabrica: Box<Fabrica>,
    ruta_base: PathBuf,
    filas_por_archivo: usize,
    actual: Option<Box<dyn EscritorSalida>>,
    filas_en_actual: usize,
    cerrado: bool,
    pub rutas: Vec<PathBuf>,
    pub total: usize,
}

impl EscritorParticionado {
    pub fn nuevo(
        fabrica: impl FnMut(&Path) -> CoreResult<Box<dyn EscritorSalida>> + 'static,
        ruta_base: PathBuf,
        filas_por_archivo: usize,
    ) -> Self {
        Self {
            fabrica: Box::new(fabrica),
            ruta_base,
            filas_por_archivo: filas_por_archivo.max(1),
            actual: None,
            filas_en_actual: 0,
            cerrado: false,
            rutas: Vec::new(),
            total: 0,
        }
    }

    fn cerrar_actual(&mut self) -> CoreResult<()> {
        if let Some(mut actual) = self.actual.take() {
            actual.cerrar()?;
            // El total lo aporta el escritor real: así se respeta su recorte
            // de filas vacías finales en vez de contar las que le pasamos.
            self.total += actual.total();
        }
        Ok(())
    }

    fn nuevo_archivo(&mut self) -> CoreResult<()> {
        self.cerrar_actual()?;
        let n = self.rutas.len() + 1;
        let stem = self
            .ruta_base
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let ext = self
            .ruta_base
            .extension()
            .map(|e| e.to_string_lossy().into_owned());
        let nombre = match ext {
            Some(e) if !e.is_empty() => format!("{stem}_{n}.{e}"),
            _ => format!("{stem}_{n}"),
        };
        let ruta = self.ruta_base.with_file_name(nombre);
        let escritor = (self.fabrica)(&ruta)?;
        // La ruta REAL la fija el escritor (se desambigua sola si ya existía).
        self.rutas.push(escritor.ruta().to_path_buf());
        self.actual = Some(escritor);
        self.filas_en_actual = 0;
        Ok(())
    }

    pub fn escribir(&mut self, chunk: &DataFrame) -> CoreResult<()> {
        let n = chunk.height();
        let mut pos = 0usize;
        while pos < n {
            if self.actual.is_none() || self.filas_en_actual >= self.filas_por_archivo {
                self.nuevo_archivo()?;
            }
            let tomar = (self.filas_por_archivo - self.filas_en_actual).min(n - pos);
            let bloque = chunk.slice(pos as i64, tomar);
            self.actual
                .as_mut()
                .expect("acabamos de crearlo")
                .escribir(&bloque, None)?;
            self.filas_en_actual += tomar;
            pos += tomar;
        }
        Ok(())
    }

    /// Idempotente. Sin datos: deja un archivo válido con solo cabeceras.
    pub fn cerrar(&mut self) -> CoreResult<()> {
        if self.cerrado {
            return Ok(());
        }
        self.cerrado = true;
        if self.rutas.is_empty() {
            self.nuevo_archivo()?;
        }
        self.cerrar_actual()
    }

    /// Tras un error: aborta el archivo en curso (cierra su handle antes de
    /// borrarlo, no al revés) y borra los archivos anteriores ya cerrados.
    /// Idempotente.
    pub fn abortar(&mut self) -> CoreResult<()> {
        if self.cerrado {
            return Ok(());
        }
        self.cerrado = true;
        if let Some(mut actual) = self.actual.take() {
            actual.abortar()?;
        }
        for ruta in &self.rutas {
            let _ = std::fs::remove_file(ruta);
        }
        Ok(())
    }
}

impl Drop for EscritorParticionado {
    /// Red de seguridad: si el usuario olvida llamar a `cerrar()`, se
    /// finaliza igualmente en vez de dejar un archivo a medio escribir. Es
    /// idempotente con una llamada explícita previa. Mismo criterio,
    /// deliberado y verificado consistente, que `EscritorXlsx`/`EscritorCsv`
    /// (no un descuido): `cerrar()` ya es transaccional (limpia el archivo a
    /// medias si falla), así que llamarlo acá es seguro incluso en el camino
    /// de error.
    fn drop(&mut self) {
        let _ = self.cerrar();
    }
}
