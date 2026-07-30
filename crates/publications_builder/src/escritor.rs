use std::path::Path;

use commerce_core::{CoreResult, EscritorXlsx, OpcionesEscritorXlsx};

use crate::constantes::MAX_ROWS_PER_SHEET;

/// El escritor compartido por los 3 modos, con hojas NOMBRADAS (`Hoja1`,
/// `Eliminadas`, `Procesadas`… — la 2ª parte de una hoja llena es
/// `Hoja1_2`). Las columnas de cada hoja se fijan con su primer bloque.
pub fn nuevo_escritor(archivo_salida: &Path, avisar: impl FnMut(&str) + 'static) -> CoreResult<EscritorXlsx> {
    EscritorXlsx::con_avisador(
        archivo_salida,
        OpcionesEscritorXlsx {
            filas_por_hoja: MAX_ROWS_PER_SHEET,
            ..Default::default()
        },
        avisar,
    )
}

/// Orquestación compartida por los 3 modos (Únicas, Amazon, Compatibilidades):
/// crea el escritor de salida (capturando sus propios avisos, p. ej. el
/// recorte de una hoja al límite de Excel), corre `trabajo` con acceso a él
/// y a `avisar` (para avisos inmediatos, p. ej. una hoja no legible), drena
/// los avisos del escritor hacia `avisar` al terminar, y cierra o aborta
/// según el resultado. Un error dentro de `trabajo` nunca deja un `.xlsx` de
/// salida a medias.
///
/// `avisar` se pasa como PARÁMETRO de `trabajo` (no capturado del entorno)
/// para que el llamador no tenga que lidiar con el préstamo mutable
/// compartido entre este helper y su propio closure.
pub fn ejecutar_con_escritor(
    archivo_salida: &Path,
    avisar: &mut dyn FnMut(&str),
    trabajo: impl FnOnce(&mut EscritorXlsx, &mut dyn FnMut(&str)) -> CoreResult<()>,
) -> CoreResult<()> {
    let avisos_escritor = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let avisos_clon = avisos_escritor.clone();
    let mut escritor = nuevo_escritor(archivo_salida, move |m: &str| {
        avisos_clon.borrow_mut().push(m.to_string())
    })?;

    let resultado = trabajo(&mut escritor, &mut *avisar);

    for aviso in avisos_escritor.borrow().iter() {
        avisar(aviso);
    }

    match resultado {
        Ok(()) => {
            escritor.cerrar()?;
            Ok(())
        }
        Err(error) => {
            escritor.abortar()?;
            Err(error)
        }
    }
}
