use std::collections::HashSet;

use polars::prelude::*;

use crate::error::CoreResult;

/// Alinea `df` a `columnas`: si ya coincide exactamente (mismo orden, mismos
/// nombres), lo devuelve tal cual sin tocar nada. Si no: columnas SOBRANTES
/// (en `df` pero no en `columnas`) se descartan, avisando la primera vez que
/// se ve cada una (rastreado en `extras_avisadas`, para no repetir el aviso
/// en cada bloque); columnas FALTANTES se rellenan con `None`. Devuelve `df`
/// recortado/reordenado a exactamente `columnas`.
///
/// Compartida por `EscritorXlsx::alinear` y `EscritorCsv::alinear`. El
/// mensaje de aviso queda a cargo de `avisar` (recibe los nombres nuevos, no
/// un texto ya armado) porque difiere entre ambos: uno menciona la hoja.
pub(crate) fn alinear_columnas(
    df: &DataFrame,
    columnas: &[String],
    extras_avisadas: &mut HashSet<String>,
    avisar: &mut dyn FnMut(&[String]),
) -> CoreResult<DataFrame> {
    let actuales: Vec<String> = df.get_column_names().iter().map(|s| s.to_string()).collect();
    if actuales.as_slice() == columnas {
        return Ok(df.clone());
    }

    let set_columnas: HashSet<&str> = columnas.iter().map(|s| s.as_str()).collect();
    let nuevas: Vec<String> = actuales
        .iter()
        .filter(|c| !set_columnas.contains(c.as_str()) && !extras_avisadas.contains(*c))
        .cloned()
        .collect();
    if !nuevas.is_empty() {
        for c in &nuevas {
            extras_avisadas.insert(c.clone());
        }
        // Solo se avisa por las columnas DEL USUARIO. Las internas (prefijo
        // `_`) son contabilidad de la herramienta —`_imagen_motivo` ya se
        // renombró a `Motivo_Rechazo` antes de llegar acá— y descartarlas es
        // lo correcto, no una pérdida.
        //
        // La distinción importa más de lo que parece: un aviso que grita
        // cuando no pasa nada entrena a ignorar los avisos, y el día que se
        // descarte una columna real del usuario el mensaje va a pasar
        // desapercibido. Se callan las internas para que el aviso conserve
        // su significado.
        let del_usuario: Vec<String> = nuevas.iter().filter(|c| !c.starts_with('_')).cloned().collect();
        if !del_usuario.is_empty() {
            avisar(&del_usuario);
        }
    }

    let set_actuales: HashSet<&str> = actuales.iter().map(|s| s.as_str()).collect();
    let mut df = df.clone();
    for faltante in columnas.iter().filter(|c| !set_actuales.contains(c.as_str())) {
        let nula: Vec<Option<&str>> = vec![None; df.height()];
        df.with_column(Column::new(faltante.as_str().into(), nula))?;
    }
    Ok(df.select(columnas.iter().map(|s| s.as_str()))?)
}

#[cfg(test)]
mod tests_aviso_selectivo {
    use super::*;

    fn alinear_capturando(df: &DataFrame, columnas: &[&str]) -> Vec<String> {
        let cols: Vec<String> = columnas.iter().map(|s| s.to_string()).collect();
        let mut vistas = HashSet::new();
        let mut avisos: Vec<String> = Vec::new();
        alinear_columnas(df, &cols, &mut vistas, &mut |nuevas| {
            avisos.extend(nuevas.iter().cloned());
        })
        .expect("alinear");
        avisos
    }

    #[test]
    fn descartar_una_columna_del_usuario_si_avisa() {
        // Esto SÍ es pérdida de datos: tiene que verse.
        let df = df! { "Sku" => ["A1"], "Precio" => ["9.99"] }.expect("df");
        assert_eq!(alinear_capturando(&df, &["Sku"]), vec!["Precio".to_string()]);
    }

    #[test]
    fn descartar_columnas_internas_no_avisa() {
        // `_imagen_motivo` y compañía ya cumplieron su función antes de
        // escribir; avisar por ellas es una falsa alarma que desgasta la
        // atención sobre el caso de arriba, que sí importa.
        let df = df! {
            "Sku" => ["A1"],
            "_imagen_motivo" => ["D1"],
            "_source_sheet" => ["Hoja1"],
        }
        .expect("df");
        assert!(alinear_capturando(&df, &["Sku"]).is_empty());
    }

    #[test]
    fn mezcla_avisa_solo_por_las_del_usuario() {
        let df = df! {
            "Sku" => ["A1"],
            "_interna" => ["x"],
            "Color" => ["rojo"],
        }
        .expect("df");
        assert_eq!(alinear_capturando(&df, &["Sku"]), vec!["Color".to_string()]);
    }
}
