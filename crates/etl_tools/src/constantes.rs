/// Extensiones de archivo soportadas como entrada.
pub const SOPORTADOS_ARCHIVOS: &[&str] = &["csv", "xlsx"];

/// Corte de hoja por defecto (nunca por encima del límite real de Excel).
pub const FILAS_POR_HOJA: usize = 1_000_000;

/// Nombres de columna que los modos usan internamente (auxiliares o de
/// salida). Si el archivo del usuario trajera una columna así, el modo la
/// sobrescribiría EN SILENCIO: se comprueba al entrar a cada modo.
pub const COLUMNAS_RESERVADAS: &[&str] = &[
    "clave_limpia",
    "clave_norm",
    "Motivo_Borrado",
    "_hit",
    "_hits",
    "_match",
    "_rank",
    "_ri",
    "_kraw",
    "_len",
    // Estas 4 no las crea este crate directamente: son temporales de
    // `commerce_core::ordenar_excel_df` (modo "Ordenar columnas"), que
    // agrega/ordena/descarta internamente — reservadas igual, porque una
    // columna real con ese nombre en el archivo del usuario colisionaría a
    // mitad del pipeline.
    "_o_vac",
    "_o_grp",
    "_o_num",
    "_o_txt",
];

/// Prefijo reservado de las columnas auxiliares de color de fuente
/// (`_font_color_<col>`), comprobado aparte porque no es un nombre exacto.
const PREFIJO_RESERVADO: &str = "_font_color_";

/// Columna que añade el modo "Encontrar", siempre al INICIO de la salida.
pub const COL_ENCONTRADA: &str = "Palabra_encontrada";

/// Columna que añade el modo "Dividir por caracteres".
pub const COL_CARACTERES: &str = "Caracteres";

/// `True` (y devuelve las columnas responsables) si `columnas` trae nombres
/// internos de la herramienta: el modo debe cancelarse en vez de
/// corromperlas en silencio.
pub fn columnas_reservadas_presentes(columnas: &[String]) -> Vec<String> {
    let exactas = commerce_core::columnas_reservadas_en(
        columnas.iter().map(|s| s.as_str()),
        COLUMNAS_RESERVADAS.iter().copied(),
    );
    let con_prefijo = columnas
        .iter()
        .filter(|c| c.starts_with(PREFIJO_RESERVADO))
        .cloned();
    let mut todas = exactas;
    todas.extend(con_prefijo);
    todas
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detecta_columnas_reservadas() {
        assert_eq!(
            columnas_reservadas_presentes(&["Sku".into(), "clave_limpia".into()]),
            vec!["clave_limpia".to_string()]
        );
        assert_eq!(
            columnas_reservadas_presentes(&["_font_color_X".into()]),
            vec!["_font_color_X".to_string()]
        );
        assert!(columnas_reservadas_presentes(&["Sku".into(), "Precio".into()]).is_empty());
    }
}
