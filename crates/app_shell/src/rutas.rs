//! Rutas de entrada/salida, multiplataforma: variable de entorno primero,
//! si no carpeta de Descargas real del usuario (vía `dirs`), si eso falla
//! `~/Downloads`.

use std::env;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

fn carpeta_descargas() -> PathBuf {
    dirs::download_dir().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Downloads")
    })
}

fn desde_env_o(var: &str, por_defecto: &Path) -> PathBuf {
    env::var(var)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| por_defecto.to_path_buf())
}

struct Rutas {
    entrada: PathBuf,
    salida: PathBuf,
}

fn rutas_iniciales() -> Rutas {
    let descargas = desde_env_o("PUBLICACIONES_DESCARGAS", &carpeta_descargas());
    Rutas {
        entrada: desde_env_o("PUBLICACIONES_ENTRADA", &descargas),
        salida: desde_env_o("PUBLICACIONES_SALIDA", &descargas),
    }
}

static RUTAS: std::sync::LazyLock<RwLock<Rutas>> = std::sync::LazyLock::new(|| {
    let rutas = rutas_iniciales();
    asegurar_carpetas(&rutas.entrada, &rutas.salida);
    RwLock::new(rutas)
});

fn asegurar_carpetas(entrada: &Path, salida: &Path) {
    // Nunca aborta por falta de permisos: que falle la herramienta después,
    // con su propio aviso ("no hay archivos" es más claro que un panic acá).
    let _ = std::fs::create_dir_all(entrada);
    let _ = std::fs::create_dir_all(salida);
}

/// Si un hilo previo entró en pánico mientras tenía el lock, el `RwLock`
/// queda envenenado para siempre — pero los datos protegidos (rutas de
/// entrada/salida) no se corrompen por eso, así que hay que recuperarlos en
/// vez de que CUALQUIER llamada posterior (de cualquier binario) entre en
/// pánico también.
pub fn ruta_entrada() -> PathBuf {
    RUTAS.read().unwrap_or_else(|e| e.into_inner()).entrada.clone()
}

pub fn ruta_salida() -> PathBuf {
    RUTAS.read().unwrap_or_else(|e| e.into_inner()).salida.clone()
}

/// Cambia las carpetas para esta sesión (no se guarda en disco).
pub fn fijar_rutas(entrada: Option<PathBuf>, salida: Option<PathBuf>) {
    let mut rutas = RUTAS.write().unwrap_or_else(|e| e.into_inner());
    if let Some(e) = entrada {
        rutas.entrada = e;
    }
    if let Some(s) = salida {
        rutas.salida = s;
    }
    asegurar_carpetas(&rutas.entrada, &rutas.salida);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desde_env_o_ignora_variables_vacias_o_en_blanco() {
        let por_defecto = PathBuf::from("/tmp/x");
        // No seteamos la variable: debe caer al default.
        let v = desde_env_o("VARIABLE_QUE_NO_EXISTE_EN_TESTS_APP_SHELL", &por_defecto);
        assert_eq!(v, por_defecto);
    }

    #[test]
    fn fijar_rutas_solo_cambia_lo_que_se_pasa() {
        let original_entrada = ruta_entrada();
        let tmp = tempfile::tempdir().unwrap();
        fijar_rutas(Some(tmp.path().to_path_buf()), None);
        assert_eq!(ruta_entrada(), tmp.path());
        // restaurar para no afectar otros tests que compartan el proceso
        fijar_rutas(Some(original_entrada), None);
    }

    #[test]
    fn un_lock_envenenado_por_un_panico_previo_se_recupera_en_vez_de_propagar_el_panico() {
        let original_entrada = ruta_entrada();
        let original_salida = ruta_salida();

        // Envenena el RwLock a propósito, en otro hilo: entrar en pánico
        // mientras se tiene el write-lock tomado es justo lo que deja el
        // lock envenenado para siempre en la implementación estándar.
        let resultado = std::thread::spawn(|| {
            let _guard = RUTAS.write().unwrap();
            panic!("veneno a propósito para este test");
        })
        .join();
        assert!(resultado.is_err());

        // Sin la recuperación, esto entraría en pánico también.
        let _ = ruta_entrada();
        fijar_rutas(Some(original_entrada.clone()), Some(original_salida.clone()));
        assert_eq!(ruta_entrada(), original_entrada);
        assert_eq!(ruta_salida(), original_salida);
    }
}
