use std::path::Path;

use polars::prelude::DataFrame;

use crate::error::CoreResult;
use crate::escritor_columnar::EscritorIpc;
use crate::escritor_csv::EscritorCsv;
use crate::escritor_xlsx::EscritorXlsx;

/// Interfaz común de los escritores de salida (`EscritorXlsx` / `EscritorCsv`),
/// para poder tratarlos de forma intercambiable (p. ej. en un particionado por
/// archivos que no necesita saber si el destino final es XLSX o CSV).
pub trait EscritorSalida {
    fn escribir(&mut self, df: &DataFrame, hoja: Option<&str>) -> CoreResult<()>;
    fn cerrar(&mut self) -> CoreResult<()>;
    fn abortar(&mut self) -> CoreResult<()>;
    fn ruta(&self) -> &Path;
    fn total(&self) -> usize;
}

impl EscritorSalida for EscritorXlsx {
    fn escribir(&mut self, df: &DataFrame, hoja: Option<&str>) -> CoreResult<()> {
        EscritorXlsx::escribir(self, df, hoja)
    }
    fn cerrar(&mut self) -> CoreResult<()> {
        EscritorXlsx::cerrar(self)
    }
    fn abortar(&mut self) -> CoreResult<()> {
        EscritorXlsx::abortar(self)
    }
    fn ruta(&self) -> &Path {
        &self.ruta
    }
    fn total(&self) -> usize {
        self.total
    }
}

impl EscritorSalida for EscritorCsv {
    fn escribir(&mut self, df: &DataFrame, hoja: Option<&str>) -> CoreResult<()> {
        EscritorCsv::escribir(self, df, hoja)
    }
    fn cerrar(&mut self) -> CoreResult<()> {
        EscritorCsv::cerrar(self)
    }
    fn abortar(&mut self) -> CoreResult<()> {
        EscritorCsv::abortar(self)
    }
    fn ruta(&self) -> &Path {
        &self.ruta
    }
    fn total(&self) -> usize {
        self.total
    }
}

impl EscritorSalida for EscritorIpc {
    fn escribir(&mut self, df: &DataFrame, hoja: Option<&str>) -> CoreResult<()> {
        EscritorIpc::escribir(self, df, hoja)
    }
    fn cerrar(&mut self) -> CoreResult<()> {
        EscritorIpc::cerrar(self)
    }
    fn abortar(&mut self) -> CoreResult<()> {
        EscritorIpc::abortar(self)
    }
    fn ruta(&self) -> &Path {
        &self.ruta
    }
    fn total(&self) -> usize {
        self.total
    }
}
