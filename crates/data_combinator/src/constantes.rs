/// Extensiones de archivo soportadas como entrada.
pub const SOPORTADOS_ARCHIVOS: &[&str] = &["csv", "xlsx"];

/// Corte de hoja por defecto (el usuario puede pedir uno menor).
pub const FILAS_POR_HOJA: usize = 1_000_000;

/// Tokens que representan "vacío" en los datos crudos.
pub const TOKENS_NULOS: &[&str] = &["none", "nan", "nat", "null", "n/a"];

/// Nombres de columna que el ordenamiento usa internamente
/// (`ordenar_excel_df`). Si el archivo del usuario trajera una columna así,
/// el camino CON orden la sobrescribiría y la eliminaría de la salida EN
/// SILENCIO: se comprueba al inicio con `columnas_reservadas_en`.
pub const COLUMNAS_RESERVADAS: &[&str] = &["_o_vac", "_o_grp", "_o_num", "_o_txt"];

/// Umbrales de la mezcla externa de ordenamiento. Si el total cabe en un run,
/// se ordena TODO en memoria (ruta rápida). Por encima, se generan runs, se
/// vuelcan a CSV temporal y se fusionan en streaming (memoria plana a 10M+,
/// a cambio de más lento por el I/O). El presupuesto es en CELDAS (filas ×
/// columnas), así que el nº de filas por run depende del ANCHO del archivo;
/// `filas_min_run` evita runs diminutos (y una fusión de mil vías) con
/// archivos muy anchos.
///
/// Se pasan como parámetros explícitos (no como constantes globales): evita
/// estado global mutable y dos tests no pueden pisarse los umbrales si
/// llegaran a correr en paralelo.
#[derive(Debug, Clone, Copy)]
pub struct UmbralesOrden {
    pub celdas_por_run: usize,
    pub filas_min_run: usize,
}

impl Default for UmbralesOrden {
    fn default() -> Self {
        Self {
            celdas_por_run: 30_000_000,
            filas_min_run: 500_000,
        }
    }
}

impl UmbralesOrden {
    /// Fuerza el camino "todo en memoria" (un único run).
    pub fn forzar_memoria() -> Self {
        Self {
            celdas_por_run: 10_usize.pow(12),
            filas_min_run: 10_usize.pow(9),
        }
    }

    /// Fuerza la mezcla externa en disco (runs mínimos).
    pub fn forzar_disco() -> Self {
        Self {
            celdas_por_run: 1,
            filas_min_run: 1,
        }
    }

    pub(crate) fn filas_por_run(&self, ancho: usize) -> usize {
        self.filas_min_run.max(self.celdas_por_run / ancho.max(1))
    }
}

/// Presupuesto de LECTURA de un CSV, en celdas (filas × columnas), para que
/// el tamaño de lote se adapte al ancho del archivo igual que los runs.
#[derive(Debug, Clone, Copy)]
pub struct UmbralesLoteCsv {
    pub celdas_por_lote: usize,
    pub filas_min_lote: usize,
}

impl Default for UmbralesLoteCsv {
    fn default() -> Self {
        Self {
            celdas_por_lote: 5_000_000,
            filas_min_lote: 50_000,
        }
    }
}

impl UmbralesLoteCsv {
    pub(crate) fn filas_por_lote(&self, ancho: usize) -> usize {
        self.filas_min_lote.max(self.celdas_por_lote / ancho.max(1))
    }
}
