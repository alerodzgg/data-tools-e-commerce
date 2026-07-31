//! Capa de REGLAS DE NEGOCIO de 'Compatibilidades'/'Comprimidas': qué filas
//! se descartan de 'Combinada' (exceso de caracteres, 'ERROR', 2+ 'NA'), el
//! explode de 'Coincidencia' y el dedup final por menor precio. Separado del
//! resto de `compatibilidad` (Ronda 9 de auditoría) porque antes vivía en un
//! único archivo de 1552 líneas que mezclaba esta capa con parsing puro y
//! con el motor de I/O particionado.

use std::ops::Not;
use std::sync::LazyLock;

use commerce_core::{CoreResult, EscritorXlsx};
use polars::prelude::*;
use regex::Regex;

use super::ModoCompatibilidad;
use crate::comunes::columna_texto;
use crate::constantes::COL_PRECIO;

static RE_NA_FINAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-NA$").unwrap());
static RE_GUION_DIGITOS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d)-(\d)").unwrap());

fn dedup_por_combinada_menor_precio2(df: &DataFrame) -> CoreResult<DataFrame> {
    let opciones = SortMultipleOptions::default().with_nulls_last(true);
    let ordenado = df.sort(["Precio2"], opciones)?;
    Ok(ordenado.unique::<(), ()>(Some(&["Combinada".to_string()]), UniqueKeepStrategy::First, None)?)
}

/// Deduplica un bucket ya reunido por [`commerce_core::AcumuladorParticionado`]
/// (partición COMPLETA de 'Combinada', entre TODOS los bloques/hojas) y lo
/// escribe en la hoja "Procesadas". Si falta 'Precio2', se avisa en vez de
/// saltear el dedup en silencio (misma asimetría que `publications_validator`'s
/// `dedup_bucket` ya cubre para su propio caso).
pub(super) fn escribir_bucket_procesadas(
    bucket: DataFrame,
    escritor: &mut EscritorXlsx,
    avisar: &mut dyn FnMut(&str),
) -> CoreResult<()> {
    let deduped = if bucket.column("Precio2").is_ok() {
        dedup_por_combinada_menor_precio2(&bucket)?
    } else {
        avisar("No se encontró la columna 'Precio2': no se deduplicó por precio en 'Procesadas'.");
        bucket
    };
    escritor.escribir(&deduped, Some("Procesadas"))?;
    Ok(())
}

/// Filtra/limpia sobre 'Combinada'; devuelve las válidas (SIN deduplicar
/// todavía: eso es responsabilidad de [`escribir_bucket_procesadas`], que lo
/// hace de forma GLOBAL). Escribe a hojas aparte: 'Exceso_caracteres' (>60)
/// y, en 'comprimidas', 'Eliminadas' (con 'ERROR' o con >=2 'NA'). Luego
/// normaliza guiones.
pub fn aplicar_filtros_combinada(
    df_proc: &DataFrame,
    modo: ModoCompatibilidad,
    escritor: &mut EscritorXlsx,
) -> CoreResult<DataFrame> {
    if df_proc.height() == 0 || df_proc.column("Combinada").is_err() {
        return Ok(df_proc.clone());
    }
    let mut df_proc = df_proc.clone();

    let combinada = columna_texto(&df_proc, "Combinada")?;
    let mask_exceso: Vec<bool> = combinada
        .iter()
        .map(|v| v.as_deref().unwrap_or("").chars().count() > 60)
        .collect();
    let mask_exceso_ca = BooleanChunked::from_iter_values("m".into(), mask_exceso.iter().copied());
    let mut df_exceso = df_proc.filter(&mask_exceso_ca)?;
    let mask_no_exceso: BooleanChunked = mask_exceso_ca.not();
    df_proc = df_proc.filter(&mask_no_exceso)?;
    if df_exceso.column(COL_PRECIO).is_ok() {
        df_exceso = df_exceso.drop(COL_PRECIO)?;
    }
    escritor.escribir(&df_exceso, Some("Exceso_caracteres"))?;

    if modo == ModoCompatibilidad::Comprimidas {
        let combinada = columna_texto(&df_proc, "Combinada")?;
        let mask_error: Vec<bool> = combinada
            .iter()
            .map(|v| v.as_deref().unwrap_or("").contains("ERROR"))
            .collect();
        let mask_error_ca = BooleanChunked::from_iter_values("m".into(), mask_error.iter().copied());
        escritor.escribir(&df_proc.filter(&mask_error_ca)?, Some("Eliminadas"))?;
        let mask_no_error: BooleanChunked = mask_error_ca.not();
        df_proc = df_proc.filter(&mask_no_error)?;

        let combinada = columna_texto(&df_proc, "Combinada")?;
        let mask_na: Vec<bool> = combinada
            .iter()
            .map(|v| v.as_deref().unwrap_or("").matches("NA").count() >= 2)
            .collect();
        let mask_na_ca = BooleanChunked::from_iter_values("m".into(), mask_na.iter().copied());
        escritor.escribir(&df_proc.filter(&mask_na_ca)?, Some("Eliminadas"))?;
        let mask_no_na: BooleanChunked = mask_na_ca.not();
        df_proc = df_proc.filter(&mask_no_na)?;

        let combinada: Vec<String> = columna_texto(&df_proc, "Combinada")?
            .into_iter()
            .map(|v| {
                let s = v.unwrap_or_default();
                let s = RE_NA_FINAL.replace(&s, "");
                let s = RE_GUION_DIGITOS.replace_all(&s, "${1}@@HYPHEN@@${2}");
                let s = s.replace('-', " ");
                s.replace("@@HYPHEN@@", "-")
            })
            .collect();
        df_proc.with_column(Column::new("Combinada".into(), combinada))?;
    }

    Ok(df_proc)
}

/// Explota 'Coincidencia' (separada por '@') en una fila por coincidencia.
/// Implementado como un `take` con índices repetidos (en vez de la
/// maquinaria de listas de Polars): más simple de leer para este caso.
pub(super) fn explotar_coincidencia(df: &DataFrame) -> CoreResult<DataFrame> {
    let coincidencia = columna_texto(df, "Coincidencia")?;
    let n = df.height();
    let partes_por_fila: Vec<Vec<String>> = coincidencia
        .iter()
        .map(|v| match v.as_deref() {
            Some(s) if !s.is_empty() => s.split('@').map(str::to_string).collect(),
            _ => vec![String::new()],
        })
        .collect();

    let indices: Vec<IdxSize> = (0..n)
        .flat_map(|i| std::iter::repeat(i as IdxSize).take(partes_por_fila[i].len().max(1)))
        .collect();
    let coincidencia_explotada: Vec<String> = partes_por_fila.into_iter().flatten().collect();

    let idx_ca = IdxCa::from_vec(PlSmallStr::EMPTY, indices);
    let mut df_explotado = df.take(&idx_ca)?;
    df_explotado.with_column(Column::new("Coincidencia".into(), coincidencia_explotada))?;
    Ok(df_explotado)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aplicar_filtros_combinada_exceso_de_caracteres_va_a_hoja_aparte() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("filtros.xlsx");
        let mut escritor = crate::escritor::nuevo_escritor(&ruta, |_: &str| {})?;

        let combinada_larga = "x".repeat(61);
        let df = df!(
            "Sku" => ["s1", "s2"],
            "Combinada" => [combinada_larga.as_str(), "corta"],
        )?;
        let resultado = aplicar_filtros_combinada(&df, ModoCompatibilidad::Repetidas, &mut escritor)?;
        escritor.cerrar()?;

        assert_eq!(
            resultado.height(),
            1,
            "la fila con más de 60 caracteres en Combinada se saca"
        );
        assert_eq!(resultado.column("Combinada")?.str()?.get(0), Some("corta"));

        let mut libro = commerce_core::abrir_libro(&ruta).unwrap();
        let hojas = commerce_core::nombres_hojas_libro(&libro);
        assert!(hojas.iter().any(|h| h == "Exceso_caracteres"));
        let exceso = commerce_core::leer_hoja_por_nombre(&mut libro, &ruta, "Exceso_caracteres").unwrap();
        assert_eq!(exceso.height(), 1);
        Ok(())
    }

    #[test]
    fn aplicar_filtros_combinada_modo_comprimidas_filtra_error_na_y_normaliza_guiones() -> CoreResult<()> {
        let tmp = tempfile::tempdir().unwrap();
        let ruta = tmp.path().join("filtros_comp.xlsx");
        let mut escritor = crate::escritor::nuevo_escritor(&ruta, |_: &str| {})?;

        let df = df!(
            "Sku" => ["s1", "s2", "s3"],
            "Combinada" => ["Tiene ERROR aqui", "NA parte-NA otra", "Civic 2-3 puerta-lateral"],
        )?;
        let resultado = aplicar_filtros_combinada(&df, ModoCompatibilidad::Comprimidas, &mut escritor)?;
        escritor.cerrar()?;

        assert_eq!(
            resultado.height(),
            1,
            "solo sobrevive la fila sin 'ERROR' y con menos de 2 'NA'"
        );
        assert_eq!(
            resultado.column("Combinada")?.str()?.get(0),
            Some("Civic 2-3 puerta lateral"),
            "el guión ENTRE DÍGITOS se preserva; el resto de guiones se normaliza a espacio"
        );

        let mut libro = commerce_core::abrir_libro(&ruta).unwrap();
        let eliminadas = commerce_core::leer_hoja_por_nombre(&mut libro, &ruta, "Eliminadas").unwrap();
        assert_eq!(
            eliminadas.height(),
            2,
            "la fila con 'ERROR' y la de 2+ 'NA' van a Eliminadas"
        );
        Ok(())
    }
}
