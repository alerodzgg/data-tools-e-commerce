use std::path::{Path, PathBuf};

/// Devuelve `ruta_base`, o `nombre(N).ext` si ya existe (estilo Windows).
pub fn ruta_unica(ruta_base: impl AsRef<Path>) -> PathBuf {
    let ruta = ruta_base.as_ref().to_path_buf();
    if !ruta.exists() {
        return ruta;
    }
    let stem = ruta
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = ruta.extension().map(|s| s.to_string_lossy().into_owned());
    let padre = ruta.parent().map(|p| p.to_path_buf()).unwrap_or_default();

    let mut contador = 1u64;
    loop {
        let nombre = match &ext {
            Some(e) if !e.is_empty() => format!("{stem}({contador}).{e}"),
            _ => format!("{stem}({contador})"),
        };
        let candidata = padre.join(nombre);
        if !candidata.exists() {
            return candidata;
        }
        contador += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruta_unica_estilo_parentesis() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("salida.xlsx");
        assert_eq!(ruta_unica(&base), base, "si no existe, se devuelve tal cual");

        std::fs::write(&base, "x").unwrap();
        assert_eq!(ruta_unica(&base).file_name().unwrap(), "salida(1).xlsx");

        std::fs::write(tmp.path().join("salida(1).xlsx"), "x").unwrap();
        assert_eq!(ruta_unica(&base).file_name().unwrap(), "salida(2).xlsx");
    }
}
