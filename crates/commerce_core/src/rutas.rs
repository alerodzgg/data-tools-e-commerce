use std::ops::Deref;
use std::path::{Path, PathBuf};

/// Ruta REAL donde terminó escribiendo un `EscritorXlsx`/`EscritorCsv` tras
/// cerrar con éxito — puede diferir de la ruta que se le pidió si
/// [`ruta_unica`] la redirigió por una colisión (p. ej. un temporal rancio
/// de una corrida anterior interrumpida).
///
/// Se distingue por TIPO, no solo por nombre de variable/comentario, de un
/// `PathBuf` cualquiera: una función que vaya a sobrescribir el archivo
/// ORIGINAL con el resultado de un escritor debe pedir este tipo, no
/// `&Path` — así el compilador rechaza pasarle por error la ruta que se
/// PIDIÓ en vez de la que realmente se usó. Esa confusión exacta causó
/// pérdida de datos y se corrigió tres veces por separado, en tres
/// funciones distintas (`buscarv`, `escribir_reporte_y_limpio`,
/// `cruzar_y_escribir`), antes de que existiera este tipo — la clase entera
/// de bug es irrepresentable para código que adopte `RutaEscritaReal` en su
/// firma en vez de `&Path`/`PathBuf`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RutaEscritaReal(PathBuf);

impl RutaEscritaReal {
    pub fn nueva(ruta: PathBuf) -> Self {
        Self(ruta)
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl Deref for RutaEscritaReal {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for RutaEscritaReal {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

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
    use proptest::prelude::*;

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

    proptest::proptest! {
        // Invariante (docs/decisiones/0006-tests-reactivos-vs-invariantes.md):
        // sin importar CUÁNTOS archivos con el patrón "(N)" ya existan de
        // antes (p. ej. temporales rancios de corridas interrumpidas),
        // `ruta_unica` nunca debe devolver una ruta que YA exista — es
        // justamente la garantía de la que dependen `EscritorXlsx`/
        // `EscritorCsv` para no pisar datos del usuario al abrir su archivo
        // de salida.
        #[test]
        fn ruta_unica_nunca_devuelve_una_ruta_que_ya_existe(previos in 0usize..12) {
            let tmp = tempfile::tempdir().unwrap();
            let base = tmp.path().join("salida.xlsx");
            std::fs::write(&base, "x").unwrap();
            for n in 1..=previos {
                std::fs::write(tmp.path().join(format!("salida({n}).xlsx")), "x").unwrap();
            }
            let candidata = ruta_unica(&base);
            prop_assert!(!candidata.exists(), "ruta_unica devolvió una ruta ya ocupada: {candidata:?}");
        }
    }
}
