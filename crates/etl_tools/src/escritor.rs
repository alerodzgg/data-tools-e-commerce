use std::path::Path;

use commerce_core::escritor_xlsx::OpcionesEscritorXlsx;
use commerce_core::{CoreResult, EscritorXlsx};

use crate::constantes::FILAS_POR_HOJA;

/// El escritor de `commerce_core`, con las hojas `Parte_N` que usa esta
/// herramienta.
pub fn nuevo_escritor(ruta: &Path, columnas: Vec<String>) -> CoreResult<EscritorXlsx> {
    EscritorXlsx::nuevo(
        ruta,
        OpcionesEscritorXlsx {
            columnas: Some(columnas),
            filas_por_hoja: FILAS_POR_HOJA,
            hoja_por_defecto: "Parte".to_string(),
            numerar_siempre: true,
            recortar_vacias: true,
        },
    )
}
