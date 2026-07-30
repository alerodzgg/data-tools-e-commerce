//! Diagnóstico del sistema (CPU/RAM). No incluye rama GPU/CUDA a propósito —
//! el motor ONNX de este workspace (`ort`) corre siempre en CPU, nunca se
//! conectó un execution provider CUDA, así que un preset "GPU" no
//! correspondería a ningún camino de código real.
//!
//! Solo `ocr_tools` lo invoca al arrancar (no `etl_tools`/`data_combinator`,
//! que también procesan volúmenes grandes): decisión deliberada, no un
//! descuido — importa más para una carga CPU-bound de inferencia ONNX
//! (núcleos/RAM disponibles afectan directamente cuánta concurrencia tiene
//! sentido) que para el I/O por lotes de esas otras herramientas.

use sysinfo::System;

use crate::mensajes::{info, mostrar_subcabecera};

/// Imprime CPU y RAM de la máquina. Puramente informativo.
pub fn mostrar_diagnostico_sistema() {
    mostrar_subcabecera("DIAGNÓSTICO DEL SISTEMA");

    let mut sys = System::new_all();
    sys.refresh_all();

    let nucleos = sys.cpus().len();
    info(&format!("CPU:  {nucleos} núcleos lógicos"));

    let total_gb = sys.total_memory() as f64 / 1e9;
    let disponible_gb = sys.available_memory() as f64 / 1e9;
    info(&format!(
        "RAM:  {total_gb:.1} GB total · {disponible_gb:.1} GB disponible"
    ));
}
