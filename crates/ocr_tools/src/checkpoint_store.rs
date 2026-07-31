//! Persistencia incremental por celda en JSONL. `CheckpointStore` evita
//! reescribir todo el XLSX cada N ítems (O(n²)) usando un append-only O(1)
//! por lote, thread-safe.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointEntry {
    pub idx: i64,
    pub col: String,
    pub approved: bool,
    pub reason: String,
}

pub struct CheckpointStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl CheckpointStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Carga las entradas ya persistidas, indexadas por `(idx, col)`. Un
    /// archivo inexistente devuelve vacío. Una línea puntual inválida (p.
    /// ej. un corte a mitad de escritura tras un crash) se IGNORA, no
    /// descarta el historial ya acumulado en las demás líneas: horas de OCR
    /// ya persistidas no deberían perderse por un solo registro truncado.
    pub fn load(&self) -> HashMap<(i64, String), CheckpointEntry> {
        let Ok(file) = std::fs::File::open(&self.path) else {
            return HashMap::new();
        };
        let mut entries = HashMap::new();
        for linea in BufReader::new(file).lines() {
            let Ok(linea) = linea else {
                continue;
            };
            let linea = linea.trim();
            if linea.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<CheckpointEntry>(linea) {
                entries.insert((entry.idx, entry.col.clone()), entry);
            }
        }
        entries
    }

    pub fn append_many(&self, entries: &[CheckpointEntry]) -> std::io::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        for entry in entries {
            // `CheckpointEntry` es campos primitivos (String/bool/i64) sin
            // claves de mapa no-String ni ciclos: `serde_json` no puede
            // fallar en serializar esta forma.
            #[allow(clippy::expect_used)]
            let linea = serde_json::to_string(entry).expect("CheckpointEntry siempre serializa");
            writeln!(file, "{linea}")?;
        }
        Ok(())
    }

    pub fn delete(&self) -> std::io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entrada(idx: i64, col: &str, approved: bool, reason: &str) -> CheckpointEntry {
        CheckpointEntry {
            idx,
            col: col.to_string(),
            approved,
            reason: reason.to_string(),
        }
    }

    #[test]
    fn load_sobre_archivo_inexistente_devuelve_vacio() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path().join("no_existe.jsonl"));
        assert!(store.load().is_empty());
    }

    #[test]
    fn append_many_y_load_hacen_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path().join("cp.jsonl"));
        store
            .append_many(&[
                entrada(0, "Imagen 1", false, "D1·Banner"),
                entrada(1, "Imagen 2", true, ""),
            ])
            .unwrap();

        let cargado = store.load();
        assert_eq!(cargado.len(), 2);
        assert_eq!(cargado[&(0, "Imagen 1".to_string())].reason, "D1·Banner");
        assert!(cargado[&(1, "Imagen 2".to_string())].approved);
    }

    #[test]
    fn append_many_en_dos_lotes_acumula_sin_pisar() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path().join("cp.jsonl"));
        store.append_many(&[entrada(0, "A", true, "")]).unwrap();
        store.append_many(&[entrada(1, "B", false, "x")]).unwrap();
        assert_eq!(store.load().len(), 2);
    }

    #[test]
    fn checkpoint_corrupto_arranca_desde_cero() {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("cp.jsonl");
        std::fs::write(&ruta, "{\"idx\": 0, \"col\": \"A\"\nesto no es json").unwrap();
        let store = CheckpointStore::new(ruta);
        assert!(store.load().is_empty());
    }

    #[test]
    fn linea_corrupta_no_descarta_las_entradas_validas_de_otras_lineas() {
        // Antes: UNA línea inválida (p. ej. un corte a mitad de escritura
        // tras un crash) descartaba TODO el historial ya leído, no solo esa
        // línea — horas de OCR ya persistidas se perdían por un registro
        // truncado.
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("cp.jsonl");
        let mut contenido = serde_json::to_string(&entrada(0, "Imagen 1", true, "")).unwrap();
        contenido.push('\n');
        contenido.push_str("esto no es json\n");
        contenido.push_str(&serde_json::to_string(&entrada(1, "Imagen 2", false, "x")).unwrap());
        contenido.push('\n');
        std::fs::write(&ruta, contenido).unwrap();

        let store = CheckpointStore::new(ruta);
        let cargado = store.load();
        assert_eq!(
            cargado.len(),
            2,
            "las 2 líneas válidas deben sobrevivir a la línea corrupta del medio"
        );
        assert!(cargado[&(0, "Imagen 1".to_string())].approved);
        assert!(!cargado[&(1, "Imagen 2".to_string())].approved);
    }

    #[test]
    fn delete_es_inofensivo_si_no_existe() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path().join("no_existe.jsonl"));
        assert!(store.delete().is_ok());
    }

    #[test]
    fn append_many_vacio_no_crea_el_archivo() {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("cp.jsonl");
        let store = CheckpointStore::new(ruta.clone());
        store.append_many(&[]).unwrap();
        assert!(!ruta.exists());
    }
}
