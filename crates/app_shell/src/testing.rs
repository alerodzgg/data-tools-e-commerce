//! Mecanismo de "guion" (script) para testear flujos de CLI interactivos sin
//! terminal real. `inquire` necesita un TTY real para leer teclas; en un test
//! automatizado no hay ninguno (fallaría o colgaría). Con un guion activo,
//! los prompts de [`crate::menus`] NUNCA llegan a invocar a `inquire`: leen
//! la próxima [`Respuesta`] programada en su lugar.
//!
//! Pensado sobre todo para los tests de los binarios (`bin/*.rs`) de cada
//! herramienta, que encadenan varios menús para completar un flujo completo.

use std::cell::RefCell;
use std::collections::VecDeque;

thread_local! {
    static GUION: RefCell<Option<VecDeque<Respuesta>>> = const { RefCell::new(None) };
}

/// Una respuesta programada, en el mismo orden en que el flujo bajo test la
/// va a pedir. Cada variante corresponde a una familia de prompts de
/// [`crate::menus`].
#[derive(Debug, Clone)]
pub enum Respuesta {
    /// Para `menu_seleccionar`/`menu_seleccionar_nav`: índice (0-based) de la
    /// opción elegida, sobre el vector TAL COMO SE PASÓ (sin contar el
    /// « Volver » que agrega `menu_seleccionar_nav`).
    Elegir(usize),
    /// ESC / cancelar. Sirve para `menu_seleccionar`/`menu_seleccionar_nav`
    /// (cancelar un menú de selección única), `menu_confirmar` y
    /// `pedir_texto` (mismo significado: el usuario canceló en vez de
    /// confirmar/escribir algo).
    Cancelar,
    /// Para `menu_multiple`/`menu_multiple_preseleccionado`: índices marcados.
    Marcar(Vec<usize>),
    /// Para `menu_confirmar`.
    Confirmar(bool),
    /// Para `pedir_texto`.
    Texto(String),
}

/// Corre `cuerpo` con un guion activo **para este hilo**: los prompts de
/// [`crate::menus`] consumen `respuestas` en orden en vez de leer la
/// terminal. Restaura el guion anterior (ninguno u otro, para guiones
/// anidados) al salir, incluso si `cuerpo` entra en pánico — un guion
/// agotado a mitad de un test no debe dejar el hilo en un estado que rompa
/// los tests siguientes.
pub fn con_guion<T>(respuestas: Vec<Respuesta>, cuerpo: impl FnOnce() -> T) -> T {
    let anterior = GUION.with(|g| g.borrow_mut().replace(respuestas.into()));
    let resultado = std::panic::catch_unwind(std::panic::AssertUnwindSafe(cuerpo));
    GUION.with(|g| *g.borrow_mut() = anterior);
    match resultado {
        Ok(valor) => valor,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

pub(crate) fn activo() -> bool {
    GUION.with(|g| g.borrow().is_some())
}

fn siguiente(nombre_prompt: &str) -> Respuesta {
    // Solo se llama con un guion activo (lo garantiza `activo()` en cada
    // punto de entrada): sin él es un error de programación del propio test.
    #[allow(clippy::expect_used)]
    let tomada = GUION.with(|g| {
        g.borrow_mut()
            .as_mut()
            .expect("app_shell::testing: se llamó siguiente() sin un guion activo")
            .pop_front()
    });
    tomada.unwrap_or_else(|| {
        panic!(
            "guion de test agotado: se pidió una respuesta más para '{nombre_prompt}' de las \
             que se programaron con con_guion(...)"
        )
    })
}

pub(crate) fn tomar_eleccion(mensaje: &str) -> Option<usize> {
    match siguiente(mensaje) {
        Respuesta::Elegir(indice) => Some(indice),
        Respuesta::Cancelar => None,
        otra => panic!(
            "guion de test: se esperaba Respuesta::Elegir/Cancelar para '{mensaje}', se \
             programó {otra:?}"
        ),
    }
}

pub(crate) fn tomar_marcadas(mensaje: &str) -> Vec<usize> {
    match siguiente(mensaje) {
        Respuesta::Marcar(indices) => indices,
        otra => panic!("guion de test: se esperaba Respuesta::Marcar para '{mensaje}', se programó {otra:?}"),
    }
}

pub(crate) fn tomar_confirmacion(mensaje: &str) -> Option<bool> {
    match siguiente(mensaje) {
        Respuesta::Confirmar(valor) => Some(valor),
        Respuesta::Cancelar => None,
        otra => panic!(
            "guion de test: se esperaba Respuesta::Confirmar/Cancelar para '{mensaje}', se \
             programó {otra:?}"
        ),
    }
}

pub(crate) fn tomar_texto(mensaje: &str) -> Option<String> {
    match siguiente(mensaje) {
        Respuesta::Texto(texto) => Some(texto),
        Respuesta::Cancelar => None,
        otra => panic!(
            "guion de test: se esperaba Respuesta::Texto/Cancelar para '{mensaje}', se programó {otra:?}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuera_de_con_guion_no_hay_guion_activo() {
        assert!(!activo());
    }

    #[test]
    fn con_guion_restaura_el_estado_anterior_al_salir() {
        con_guion(vec![Respuesta::Confirmar(true)], || {
            assert!(activo());
            assert_eq!(tomar_confirmacion("¿ok?"), Some(true));
        });
        assert!(!activo());
    }

    #[test]
    #[should_panic(expected = "guion de test agotado")]
    fn pedir_mas_respuestas_que_las_programadas_entra_en_panico() {
        con_guion(vec![Respuesta::Confirmar(true)], || {
            tomar_confirmacion("uno");
            tomar_confirmacion("dos");
        });
    }

    #[test]
    fn con_guion_restaura_incluso_si_el_cuerpo_entra_en_panico() {
        let resultado = std::panic::catch_unwind(|| {
            con_guion(vec![Respuesta::Confirmar(true)], || {
                panic!("boom");
            });
        });
        assert!(resultado.is_err());
        assert!(!activo());
    }
}
