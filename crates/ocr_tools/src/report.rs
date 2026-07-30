//! Reporte final a nivel de IMAGEN (celda-URL): `build_report`.

/// `imagenes` = imágenes analizadas, `rechazadas` = imágenes que algún
/// detector marcó, `filas` = publicaciones procesadas. Aprobadas =
/// analizadas − rechazadas, así siempre cuadran.
pub fn build_report(imagenes: u64, rechazadas: u64, filas: u64, name: &str) -> String {
    let aprobadas = imagenes.saturating_sub(rechazadas);
    let pct = if imagenes > 0 {
        rechazadas as f64 / imagenes as f64 * 100.0
    } else {
        0.0
    };
    format!(
        "\n{sep}\nREPORTE DE CAMBIOS — {name}\n{sep}\nPublicaciones (filas):  {filas}\nImágenes analizadas:    {imagenes}\nImágenes rechazadas:    {rechazadas}  ({pct:.1}%)\nImágenes aprobadas:     {aprobadas}\n{sep}\n",
        sep = "=".repeat(60),
        filas = con_separador_miles(filas),
        imagenes = con_separador_miles(imagenes),
        rechazadas = con_separador_miles(rechazadas),
        aprobadas = con_separador_miles(aprobadas),
    )
}

/// Separador de miles con coma (equivalente a un formato `{:,}`).
fn con_separador_miles(valor: u64) -> String {
    let digitos = valor.to_string();
    let bytes = digitos.as_bytes();
    let mut salida = String::with_capacity(digitos.len() + digitos.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            salida.push(',');
        }
        salida.push(*b as char);
    }
    salida
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn con_separador_miles_agrupa_de_a_tres() {
        assert_eq!(con_separador_miles(6), "6");
        assert_eq!(con_separador_miles(1000), "1,000");
        assert_eq!(con_separador_miles(1_234_567), "1,234,567");
    }

    #[test]
    fn build_report_cuenta_imagenes_no_filas() {
        let reporte = build_report(6, 3, 3, "x.xlsx");
        assert!(reporte.contains("Imágenes analizadas:    6"));
        assert!(reporte.contains("Imágenes rechazadas:    3  (50.0%)"));
        assert!(reporte.contains("Imágenes aprobadas:     3"));
        assert!(reporte.contains("Publicaciones (filas):  3"));
    }

    #[test]
    fn build_report_sin_imagenes_no_divide_por_cero() {
        let reporte = build_report(0, 0, 0, "vacio.xlsx");
        assert!(reporte.contains("Imágenes rechazadas:    0  (0.0%)"));
    }

    #[test]
    fn build_report_rechazadas_mayor_a_imagenes_no_entra_en_panico() {
        // Defensa ante una futura violación de la invariante
        // imagenes >= rechazadas: satura a 0 en vez de un underflow.
        let reporte = build_report(1, 5, 1, "x.xlsx");
        assert!(reporte.contains("Imágenes aprobadas:     0"));
    }
}
