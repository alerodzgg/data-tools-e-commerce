use std::path::Path;

use commerce_core::{CoreResult, EscritorXlsx, OpcionesEscritorXlsx};

use crate::constantes::MAX_ROWS_PER_SHEET;

/// El escritor del validator: una hoja por cada hoja del archivo de
/// entrada, más `Eliminadas`. `recortar_vacias=false`: este escritor nunca
/// recortó las filas vacías finales y no hay motivo para cambiarle la
/// salida al traducir.
pub fn nuevo_escritor(archivo_salida: &Path, avisar: impl FnMut(&str) + 'static) -> CoreResult<EscritorXlsx> {
    EscritorXlsx::con_avisador(
        archivo_salida,
        OpcionesEscritorXlsx {
            filas_por_hoja: MAX_ROWS_PER_SHEET,
            recortar_vacias: false,
            ..Default::default()
        },
        avisar,
    )
}
