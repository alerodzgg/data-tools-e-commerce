//! Carga de los modelos ONNX (detector CRAFT + reconocedor VGG-BiLSTM-CTC) y
//! wiring de inferencia sobre `ort`.

use std::path::Path;
use std::sync::OnceLock;

use ndarray::{Array2, Array3, Array4};
use ort::session::{Session, SessionOutputs};
use ort::value::{DynValue, Tensor};

static RUNTIME_INIT: OnceLock<Result<(), String>> = OnceLock::new();

/// Nombre del binario dinámico de onnxruntime, según el SO del binario final
/// (Windows carga `.dll`, Linux `.so` — cada uno se empaqueta en `runtime/`
/// junto al ejecutable).
#[cfg(target_os = "windows")]
const ONNXRUNTIME_LIB: &str = "onnxruntime.dll";
#[cfg(not(target_os = "windows"))]
const ONNXRUNTIME_LIB: &str = "libonnxruntime.so";

/// Nombre de la (única) salida esperada de ambos modelos, fijado por
/// convención en cómo se exportaron a ONNX. Se busca en vez de indexar: un
/// modelo con otro nombre de salida (otra versión, o `assets_dir()` mal
/// resuelto) debe dar error, no panic.
fn salida_output<'a>(outputs: &'a SessionOutputs<'_>) -> ort::Result<&'a DynValue> {
    outputs.get("output").ok_or_else(|| {
        ort::Error::new(
            "el modelo ONNX no tiene una salida llamada 'output' (¿modelo incorrecto o de otra versión?)",
        )
    })
}

/// Carga el motor de inferencia empaquetado junto al binario (evita el
/// enlazado estático, que en Windows falla por símbolos `__std_*` no
/// resueltos con este MSVC). Idempotente: solo el primer llamador hace el
/// trabajo (según exige `ort`, debe ocurrir antes de crear cualquier
/// `Session`); todos los llamadores, incluido el primero, reciben el mismo
/// resultado — si `assets_dir()` apuntó mal (binario movido sin sus assets,
/// `OCR_TOOLS_ASSETS_DIR` inválida), el error se propaga como `ort::Error`
/// en vez de un panic crudo que tumbe todo el proceso.
fn asegurar_runtime_inicializado() -> ort::Result<()> {
    RUNTIME_INIT
        .get_or_init(|| {
            let dylib = crate::assets_dir().join("runtime").join(ONNXRUNTIME_LIB);
            ort::init_from(dylib.to_string_lossy().into_owned())
                .commit()
                .map(|_| ())
                .map_err(|error| {
                    format!(
                        "no se pudo inicializar onnxruntime desde '{}': {error}",
                        dylib.display()
                    )
                })
        })
        .clone()
        .map_err(ort::Error::new)
}

/// Detector CRAFT: entrada `(1,3,H,W)` normalizada, salida `(text_map, link_map)`
/// cada uno `(H/2, W/2)` (se descarta el batch, siempre 1 en este pipeline).
pub struct Detector {
    session: Session,
}

/// Agrega la dimensión de batch=1: `(c,h,w)` → `(1,c,h,w)`.
///
/// El total de elementos no cambia, así que en la práctica no falla. Se
/// propaga igual en vez de afirmarlo con `expect`: los dos llamadores ya
/// devuelven `ort::Result`, así que propagar no cuesta nada y no hay que
/// sostener a mano una invariante sobre la forma de un tensor.
fn con_batch_1(input: Array3<f32>) -> ort::Result<Array4<f32>> {
    let (c, h, w) = input.dim();
    input
        .into_shape_with_order((1, c, h, w))
        .map_err(|e| ort::Error::new(format!("forma inesperada del tensor de entrada: {e}")))
}

/// Constructor de sesiones ONNX con GPU si la hay, CPU si no.
///
/// La detección es EN TIEMPO DE EJECUCIÓN, no de compilación: el mismo
/// binario tiene que servir en una instancia con CUDA y en una portátil sin
/// GPU. `ort` registra los proveedores en orden y saltea en silencio el que
/// no puede inicializar, así que CUDA primero y CPU como piso.
///
/// Con `load-dynamic`, que CUDA funcione depende de qué biblioteca de
/// ONNX Runtime haya en `runtime/`: la build de CPU no trae el proveedor y
/// el registro falla sin romper nada. Por eso se avisa qué quedó activo —
/// una corrida de horas en CPU cuando se pagó por una GPU tiene que ser
/// visible, no una sorpresa al final.
fn sesion_con_aceleracion() -> ort::Result<ort::session::builder::SessionBuilder> {
    use ort::execution_providers::{CUDAExecutionProvider, ExecutionProvider};

    let builder = Session::builder()?;
    let cuda = CUDAExecutionProvider::default();
    // `is_available()` puede entrar en pánico si la biblioteca de ONNX
    // Runtime no coincide en versión; acá eso significa "sin GPU", no
    // "abortar". Ver `hay_aceleracion_gpu`.
    let disponible = std::panic::catch_unwind(|| cuda.is_available().unwrap_or(false)).unwrap_or(false);
    if disponible {
        // `.build()` no falla acá aunque el proveedor no arranque: `ort` lo
        // reporta y sigue con el siguiente. El piso siempre es CPU.
        return builder.with_execution_providers([cuda.build()]);
    }
    Ok(builder)
}

/// Si hay aceleración por GPU disponible en esta máquina.
///
/// Para que la CLI lo informe una vez al arrancar, en vez de que el usuario
/// lo deduzca del tiempo que tarda.
///
/// Nunca propaga un fallo: no poder AVERIGUAR si hay GPU no es motivo para
/// abortar una corrida que en CPU funciona igual. Dos resguardos, porque
/// `ort` falla de dos formas distintas acá:
///
/// 1. Inicializa el runtime primero. Sin eso, `ort` carga la primera
///    `onnxruntime` que encuentre en el sistema en vez de la de `runtime/`.
/// 2. Atrapa el pánico: ante una versión incompatible de la biblioteca
///    (p.ej. 1.17 donde se espera 1.22), `ort` entra en PÁNICO en vez de
///    devolver `Err`, y esto se llama al arrancar una corrida de horas.
pub fn hay_aceleracion_gpu() -> bool {
    use ort::execution_providers::{CUDAExecutionProvider, ExecutionProvider};

    if asegurar_runtime_inicializado().is_err() {
        return false;
    }
    std::panic::catch_unwind(|| CUDAExecutionProvider::default().is_available().unwrap_or(false))
        .unwrap_or(false)
}

impl Detector {
    pub fn new(model_path: impl AsRef<Path>) -> ort::Result<Self> {
        asegurar_runtime_inicializado()?;
        Ok(Self {
            session: sesion_con_aceleracion()?.commit_from_file(model_path)?,
        })
    }

    /// `input`: CHW normalizado (3,H,W). Devuelve `(text_score, link_score)`,
    /// cada uno de forma `(H/2, W/2)`.
    pub fn forward(&mut self, input: Array3<f32>) -> ort::Result<(Array2<f32>, Array2<f32>)> {
        let batched = con_batch_1(input)?;
        let tensor = Tensor::from_array(batched)?;
        let outputs = self.session.run(ort::inputs!["input" => tensor])?;
        let y = salida_output(&outputs)?.try_extract_array::<f32>()?;
        // y: (1, dim1, dim2, 2) — validar antes de indexar: un modelo ONNX
        // exportado con otra forma de salida (p. ej. otra versión, o
        // `assets_dir()` apuntando a un modelo incorrecto) no debe tumbar el
        // proceso completo con un panic de índice fuera de rango, sino
        // devolver un error de dominio como ya hace `salida_output`.
        let shape = y.shape();
        if shape.len() != 4 || shape[0] != 1 || shape[3] < 2 {
            return Err(ort::Error::new(format!(
                "forma de salida del detector inesperada: se esperaba (1, alto, ancho, >=2), se obtuvo {shape:?}"
            )));
        }
        let (dim1, dim2) = (shape[1], shape[2]);
        let mut text_score = Array2::<f32>::zeros((dim1, dim2));
        let mut link_score = Array2::<f32>::zeros((dim1, dim2));
        for i in 0..dim1 {
            for j in 0..dim2 {
                text_score[[i, j]] = y[[0, i, j, 0]];
                link_score[[i, j]] = y[[0, i, j, 1]];
            }
        }
        Ok((text_score, link_score))
    }
}

/// Reconocedor VGG-BiLSTM-CTC (generation2/latin_g2): entrada `(1,1,64,W)`
/// (escala de grises normalizada a [-1,1]), salida softmax `(seq_len, 352)`.
pub struct Recognizer {
    session: Session,
}

impl Recognizer {
    pub fn new(model_path: impl AsRef<Path>) -> ort::Result<Self> {
        asegurar_runtime_inicializado()?;
        Ok(Self {
            session: sesion_con_aceleracion()?.commit_from_file(model_path)?,
        })
    }

    /// `input`: (1, 64, W) en escala de grises normalizada a [-1,1].
    /// Devuelve las probabilidades tras softmax, forma `(seq_len, num_class)`.
    pub fn forward(&mut self, input: Array3<f32>) -> ort::Result<Array2<f32>> {
        let batched = con_batch_1(input)?;
        let tensor = Tensor::from_array(batched)?;
        let outputs = self.session.run(ort::inputs!["input" => tensor])?;
        let logits = salida_output(&outputs)?.try_extract_array::<f32>()?;
        // logits: (1, seq_len, num_class) → softmax sobre el ultimo eje,
        // quitando el batch. Igual que en `Detector::forward`: un modelo con
        // otra forma de salida debe devolver un error, no panickear.
        let shape = logits.shape();
        if shape.len() != 3 || shape[0] != 1 {
            return Err(ort::Error::new(format!(
                "forma de salida del reconocedor inesperada: se esperaba (1, seq_len, num_class), se obtuvo {shape:?}"
            )));
        }
        let logits = logits.index_axis(ndarray::Axis(0), 0).to_owned();
        let logits = logits.into_dimensionality::<ndarray::Ix2>().map_err(|e| {
            ort::Error::new(format!(
                "no se pudo interpretar la salida del reconocedor como (seq_len, num_class): {e}"
            ))
        })?;
        Ok(softmax_last_axis(logits))
    }
}

fn softmax_last_axis(mut logits: Array2<f32>) -> Array2<f32> {
    for mut row in logits.axis_iter_mut(ndarray::Axis(0)) {
        let max = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let mut sum = 0.0f32;
        for v in row.iter_mut() {
            *v = (*v - max).exp();
            sum += *v;
        }
        for v in row.iter_mut() {
            *v /= sum;
        }
    }
    logits
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn modelo(nombre: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("models")
            .join(nombre)
    }

    #[test]
    fn detector_corre_sobre_una_imagen_negra() {
        let mut det = Detector::new(modelo("craft_detector.onnx")).expect("carga detector");
        let input = Array3::<f32>::zeros((3, 64, 64));
        let (text, link) = det.forward(input).expect("forward detector");
        assert_eq!(text.dim(), (32, 32));
        assert_eq!(link.dim(), (32, 32));
    }

    #[test]
    fn recognizer_corre_y_produce_probabilidades_normalizadas() {
        let mut rec = Recognizer::new(modelo("recognizer_latin_g2.onnx")).expect("carga recognizer");
        let input = Array3::<f32>::zeros((1, 64, 64));
        let probs = rec.forward(input).expect("forward recognizer");
        assert_eq!(probs.shape()[1], 352);
        let fila0: f32 = probs.index_axis(ndarray::Axis(0), 0).iter().sum();
        assert!((fila0 - 1.0).abs() < 1e-3, "softmax debe sumar 1, dio {fila0}");
    }
}

#[cfg(test)]
mod tests_aceleracion {
    /// No afirma SI hay GPU —depende de la máquina— sino que preguntarlo no
    /// entra en pánico ni cuelga. La detección corre en el arranque de una
    /// corrida de horas: fallar ahí sería peor que no tener GPU.
    #[test]
    fn preguntar_por_la_gpu_no_rompe_en_ninguna_maquina() {
        let hay = super::hay_aceleracion_gpu();
        println!("aceleracion GPU detectada en esta maquina: {hay}");
    }
}
