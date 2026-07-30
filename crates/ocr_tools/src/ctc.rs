//! Decodificación CTC greedy (`CTCLabelConverter`), más la lógica de máscara
//! de idioma y confianza (`recognizer_predict`, `custom_mean`).

use std::collections::HashSet;

use ndarray::{Array2, Axis};

/// Vocabulario completo del reconocedor exportado (351 caracteres), en el
/// mismo orden usado al entrenar/exportar el modelo latin_g2.
pub const CHARACTERS: &str = include_str!("../models/characters.txt");
/// Subconjunto de idioma (es+en) usado para la máscara de idioma.
pub const LANG_CHARS: &str = include_str!("../models/lang_char.txt");

/// `CTCLabelConverter::ignore_idx`: en teoría es `[0] + [i+1 for i, _ in
/// enumerate(separator_char)]`, pero `separator_char` (derivado de la lista
/// de separadores de idioma) siempre queda vacío en este reader, así que en
/// la práctica `ignore_idx = [0]` (solo el blank). Confirmado contra
/// resultados reales: una versión anterior de esta tabla asumía
/// `[0,1,2,3,4]`, y el test de regresión lo detectó al instante (se comía
/// los espacios de "Coming Soon").
const CTC_IGNORE_IDX: [usize; 1] = [0];

pub struct Converter {
    /// `character[0]` es un relleno para el índice de blank (nunca se usa su
    /// valor); `character[i]` para `i>=1` es el i-ésimo caracter del vocabulario.
    character: Vec<char>,
    /// Índices (offset +1, ver `character`) de los caracteres que NO
    /// pertenecen al idioma objetivo — se ponen a 0 antes de argmax.
    ignore_lang_idx: Vec<usize>,
}

impl Default for Converter {
    fn default() -> Self {
        Self::new(CHARACTERS, LANG_CHARS)
    }
}

impl Converter {
    pub fn new(characters: &str, lang_chars: &str) -> Self {
        let dict_character: Vec<char> = characters.chars().collect();
        let lang_set: HashSet<char> = lang_chars.chars().collect();

        let mut character = Vec::with_capacity(dict_character.len() + 1);
        character.push('\u{0}'); // [blank]
        character.extend(dict_character.iter().copied());

        let ignore_lang_idx = dict_character
            .iter()
            .enumerate()
            .filter(|(_, c)| !lang_set.contains(c))
            .map(|(i, _)| i + 1)
            .collect();

        Self {
            character,
            ignore_lang_idx,
        }
    }

    pub fn num_class(&self) -> usize {
        self.character.len()
    }

    /// Pone a 0 las probabilidades de los caracteres fuera del idioma objetivo
    /// y renormaliza cada fila (timestep) para que vuelva a sumar 1.
    pub fn apply_language_mask(&self, probs: &mut Array2<f32>) {
        for mut row in probs.axis_iter_mut(Axis(0)) {
            for &idx in &self.ignore_lang_idx {
                row[idx] = 0.0;
            }
            let sum: f32 = row.iter().sum();
            if sum > 0.0 {
                row.iter_mut().for_each(|v| *v /= sum);
            }
        }
    }

    /// Decodifica greedy (argmax por timestep) ya con la máscara de idioma
    /// aplicada. Devuelve `(texto, confianza)`.
    ///
    /// La confianza usa TODOS los timesteps no-blank (índice != 0), sin
    /// colapsar repetidos — a diferencia del texto, que sí colapsa repetidos
    /// y descarta `CTC_IGNORE_IDX` (ver `recognizer_predict` en el original:
    /// el filtro de confianza es literalmente `v[i!=0]`, independiente del
    /// `ignore_idx` de `CTCLabelConverter`).
    pub fn decode_greedy(&self, probs: &Array2<f32>) -> (String, f32) {
        let seq_len = probs.shape()[0];
        let mut indices = Vec::with_capacity(seq_len);
        let mut max_vals = Vec::with_capacity(seq_len);
        for row in probs.axis_iter(Axis(0)) {
            let mut best_i = 0usize;
            let mut best_v = row[0];
            for (i, &v) in row.iter().enumerate() {
                if v > best_v {
                    best_v = v;
                    best_i = i;
                }
            }
            indices.push(best_i);
            max_vals.push(best_v);
        }

        let mut text = String::new();
        for i in 0..seq_len {
            let no_repetido = i == 0 || indices[i] != indices[i - 1];
            let no_ignorado = !CTC_IGNORE_IDX.contains(&indices[i]);
            if no_repetido && no_ignorado {
                text.push(self.character[indices[i]]);
            }
        }

        let confianza_vals: Vec<f32> = (0..seq_len)
            .filter(|&i| indices[i] != 0)
            .map(|i| max_vals[i])
            .collect();
        let confianza = custom_mean(&confianza_vals);

        (text, confianza)
    }
}

/// `custom_mean`: media geométrica ponderada, `prod(x) ** (2/sqrt(len(x)))`.
/// Con lista vacía, se usa `[0]` como relleno → confianza 0.
fn custom_mean(xs: &[f32]) -> f32 {
    if xs.is_empty() {
        return 0.0;
    }
    let prod: f32 = xs.iter().product();
    prod.powf(2.0 / (xs.len() as f32).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabulario_tiene_351_caracteres_mas_blank() {
        let conv = Converter::default();
        assert_eq!(conv.num_class(), 352);
    }

    #[test]
    fn mascara_de_idioma_no_deja_pasar_probabilidad_fuera_del_idioma() {
        // vocab de 3 chars ('a','b','c'); idioma solo 'a'. class 0=blank,1='a',2='b',3='c'.
        let conv = Converter::new("abc", "a");
        let mut probs = Array2::<f32>::from_shape_vec((1, 4), vec![0.1, 0.2, 0.3, 0.4]).unwrap();
        conv.apply_language_mask(&mut probs);
        assert_eq!(probs[[0, 2]], 0.0);
        assert_eq!(probs[[0, 3]], 0.0);
        let suma: f32 = probs.row(0).sum();
        assert!((suma - 1.0).abs() < 1e-6);
    }

    #[test]
    fn decode_greedy_colapsa_repetidos_y_quita_blank() {
        let conv = Converter::new("ab", "ab"); // character: [blank(0), 'a'(1), 'b'(2)]
                                               // secuencia: a a blank b b b -> "ab"
        let filas = vec![
            vec![0.0, 0.9, 0.1],
            vec![0.0, 0.9, 0.1],
            vec![0.9, 0.05, 0.05],
            vec![0.1, 0.1, 0.8],
            vec![0.1, 0.1, 0.8],
            vec![0.1, 0.1, 0.8],
        ];
        let probs = Array2::from_shape_vec((6, 3), filas.into_iter().flatten().collect()).unwrap();
        let (texto, _conf) = conv.decode_greedy(&probs);
        assert_eq!(texto, "ab");
    }

    #[test]
    fn decode_greedy_no_descarta_espacios_ni_los_primeros_caracteres_del_vocabulario() {
        // `separator_list` queda vacío en la práctica (ver comentario de
        // CTC_IGNORE_IDX): el espacio (primer caracter de characters.txt) debe
        // decodificarse con normalidad, no tratarse como ignorado.
        let conv = Converter::default();
        // character[0]=blank, character[1]=' ' (primer char de CHARACTERS)
        let mut fila = vec![0.0; conv.num_class()];
        fila[1] = 1.0;
        let probs = Array2::from_shape_vec((1, conv.num_class()), fila).unwrap();
        let (texto, _conf) = conv.decode_greedy(&probs);
        assert_eq!(texto, " ");
    }

    #[test]
    fn confianza_usa_solo_no_blank_sin_colapsar_repetidos() {
        let conv = Converter::new("ab", "ab");
        // dos timesteps iguales no-blank -> ambos cuentan para la confianza aunque el texto colapse a 1 char
        let probs = Array2::from_shape_vec((2, 3), vec![0.0, 0.8, 0.2, 0.0, 0.8, 0.2]).unwrap();
        let (texto, conf) = conv.decode_greedy(&probs);
        assert_eq!(texto, "a");
        // custom_mean([0.8,0.8]) = (0.8*0.8)**(2/sqrt(2)), no un promedio simple.
        let esperado = (0.8_f32 * 0.8).powf(2.0 / (2.0_f32).sqrt());
        assert!((conf - esperado).abs() < 1e-5, "conf={conf} esperado={esperado}");
    }
}
