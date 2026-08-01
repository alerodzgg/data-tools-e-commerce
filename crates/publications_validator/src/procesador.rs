use std::collections::{HashMap, HashSet};
use std::ops::Not;
use std::path::PathBuf;

use commerce_core::{
    concat_diagonal, tomar_filas, AcumuladorParticionado, CoreError, CoreResult, EscritorXlsx,
};
use polars::prelude::*;

use crate::clasificacion::clasificar_uno;
use crate::constantes::{
    columnas_reservadas_en, CANTIDAD_CARACTERES_OEM3, COL_HOJA_ORIGEN, COL_MOTIVO_ELIMINACION, COL_OEM,
    COL_PRECIO2, COL_TRADUCIDO, PARTICIONES_DEDUPLICACION, UMBRAL_RAM_DEDUP,
};
use crate::patrones::{palabras_borradas, palabras_borradas_lower, Patrones};

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub chunks: usize,
    pub procesadas: usize,
    pub eliminadas: usize,
}

/// Envoltorio fino sobre `commerce_core::columna_texto`: solo adapta el tipo
/// de error a `CoreResult` para que los call sites existentes no necesiten
/// tocarse.
fn columna_texto(df: &DataFrame, nombre: &str) -> CoreResult<Vec<Option<String>>> {
    Ok(commerce_core::columna_texto(df, nombre)?)
}

/// Rellena `OEM` vacío con `US123400` y genera `OEM3`. No-op si no se pidió
/// `modificar_oem` o la columna `OEM` no existe. Misma lógica que en
/// `publications_builder` — duplicada a propósito: crates independientes,
/// no vale la pena una dependencia cruzada por una función tan pequeña.
fn aplicar_oem3(df: &DataFrame) -> CoreResult<DataFrame> {
    if df.column(COL_OEM).is_err() {
        return Ok(df.clone());
    }
    let oem: Vec<String> = columna_texto(df, COL_OEM)?
        .into_iter()
        .map(|v| v.unwrap_or_else(|| "US123400".to_string()))
        .collect();
    let mut df = df.clone();
    df.with_column(Column::new(COL_OEM.into(), oem.clone()))?;
    let oem3: Vec<String> = oem
        .iter()
        .map(|v| {
            let base = if v.trim().is_empty() {
                "US123400"
            } else {
                v.as_str()
            };
            base.chars().take(CANTIDAD_CARACTERES_OEM3).collect()
        })
        .collect();
    df.with_column(Column::new("OEM3".into(), oem3))?;
    Ok(df)
}

/// Dedup de una partición: conserva, por 'Traducido', la fila de menor
/// 'Precio2' (empate → la de menor índice de fila, orden estable). Un
/// `Precio2` vacío/no numérico se trata como "el más caro" (`+inf`): sin
/// esto, esa fila desaparecería en vez de sobrevivir cuando es única.
fn dedup_bucket(df_bucket: &DataFrame, avisar: &mut dyn FnMut(&str)) -> CoreResult<(DataFrame, DataFrame)> {
    if df_bucket.column(COL_PRECIO2).is_err() {
        avisar("No se encontró la columna 'Precio2': no se deduplicó por precio en este bloque.");
        return Ok((df_bucket.clone(), DataFrame::empty()));
    }
    let traducido = columna_texto(df_bucket, COL_TRADUCIDO)?;
    let precio2_num: Vec<f64> = columna_texto(df_bucket, COL_PRECIO2)?
        .iter()
        .map(|v| {
            v.as_deref()
                .and_then(|s| s.trim().parse::<f64>().ok())
                .filter(|n| n.is_finite())
                .unwrap_or(f64::INFINITY)
        })
        .collect();

    let n = df_bucket.height();
    let mut grupos: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, t) in traducido.iter().enumerate() {
        grupos.entry(t.clone().unwrap_or_default()).or_default().push(i);
    }
    let mut rank = vec![0u32; n];
    for indices in grupos.into_values() {
        let mut ordenado = indices;
        // `total_cmp` es un orden total sobre floats: no puede fallar ni
        // ante NaN, a diferencia de `partial_cmp`, que obligaría a un
        // `unwrap` dependiente de que el filtro de arriba nunca cambie.
        ordenado.sort_by(|&a, &b| precio2_num[a].total_cmp(&precio2_num[b]).then(a.cmp(&b)));
        for (r, idx) in ordenado.into_iter().enumerate() {
            rank[idx] = (r + 1) as u32;
        }
    }

    let mantener_idx: Vec<usize> = (0..n).filter(|&i| rank[i] == 1).collect();
    let dup_idx: Vec<usize> = (0..n).filter(|&i| rank[i] > 1).collect();
    let mantener = tomar_filas(df_bucket, &mantener_idx)?;
    let mut duplicados = tomar_filas(df_bucket, &dup_idx)?;
    if duplicados.height() > 0 {
        let alto = duplicados.height();
        duplicados.with_column(Column::new(
            COL_MOTIVO_ELIMINACION.into(),
            vec!["Duplicado - precio más alto"; alto],
        ))?;
    }
    Ok((mantener, duplicados))
}

fn escribir_dedup(
    df_bucket: DataFrame,
    escritor: &mut EscritorXlsx,
    stats: &mut Stats,
    avisar: &mut dyn FnMut(&str),
) -> CoreResult<()> {
    let (mantener, duplicados) = dedup_bucket(&df_bucket, avisar)?;
    if mantener.height() > 0 {
        let hojas = columna_texto(&mantener, COL_HOJA_ORIGEN)?;
        let mut grupos: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, h) in hojas.iter().enumerate() {
            grupos.entry(h.clone().unwrap_or_default()).or_default().push(i);
        }
        for (hoja, indices) in grupos {
            let mut df_h = tomar_filas(&mantener, &indices)?;
            df_h = df_h.drop(COL_HOJA_ORIGEN)?;
            let alto = df_h.height();
            escritor.escribir(&df_h, Some(&hoja))?;
            stats.procesadas += alto;
        }
    }
    if duplicados.height() > 0 {
        stats.eliminadas += duplicados.height();
        escritor.escribir(&duplicados, Some("Eliminadas"))?;
    }
    Ok(())
}

/// Tope de entradas del caché de `Clasificador`: sin esto, un archivo
/// patológico con una cantidad enorme de títulos DISTINTOS (no el caso
/// normal, donde se repiten mucho entre variantes) hacía crecer el caché sin
/// límite, encima del resto del pipeline que sí procesa en streaming/
/// particionado acotado. Pasado el tope, se sigue clasificando correctamente
/// (`clasificar_uno` es pura y determinista) — solo se deja de cachear, así
/// que un título repetido después del tope se recalcula en vez de reusarse.
const MAX_CACHE_CLASIFICACION: usize = 200_000;

/// Cachea la clasificación de cada título DISTINTO (factorización: los
/// títulos se repiten muchísimo entre variantes de un mismo producto),
/// separa un chunk en (válidas, eliminadas) y aplica OEM por fila.
struct Clasificador {
    patrones: Patrones,
    marcas_lower: HashSet<String>,
    cache: HashMap<String, (String, Option<String>)>,
    limite_cache: usize,
}

impl Clasificador {
    fn nuevo() -> Self {
        let palabras = palabras_borradas();
        Self {
            patrones: Patrones::compilar(),
            marcas_lower: palabras_borradas_lower(&palabras),
            cache: HashMap::new(),
            limite_cache: MAX_CACHE_CLASIFICACION,
        }
    }

    fn procesar_chunk(
        &mut self,
        chunk: &DataFrame,
        nombre_hoja: &str,
        modificar_oem: bool,
    ) -> CoreResult<(DataFrame, DataFrame)> {
        if chunk.column(COL_TRADUCIDO).is_err() {
            // Sin esta columna no hay nada que clasificar ni deduplicar: dejar
            // pasar el chunk "tal cual" (como se hacía antes) sería un bypass
            // silencioso de las 7 reglas y las palabras prohibidas, y además
            // rompía el dedup particionado más abajo (todas las filas sin
            // título colapsaban bajo la misma clave vacía y se marcaban
            // "Duplicado" entre sí, aunque fueran productos distintos). Se
            // aborta con el mismo criterio que el chequeo de columnas
            // reservadas dos líneas más abajo: un error claro en vez de un
            // resultado incorrecto en silencio.
            return Err(CoreError::Io(std::io::Error::other(format!(
                "La hoja '{nombre_hoja}' no tiene la columna '{COL_TRADUCIDO}': no hay nada que clasificar."
            ))));
        }
        let reservadas = columnas_reservadas_en(chunk.get_column_names().iter().map(|s| s.as_str()));
        if !reservadas.is_empty() {
            return Err(CoreError::Io(std::io::Error::other(format!(
                "El archivo trae columnas con nombres reservados del validator: {}. Renómbralas en el archivo y reintenta.",
                reservadas.join(", ")
            ))));
        }

        let titulos: Vec<String> = columna_texto(chunk, COL_TRADUCIDO)?
            .into_iter()
            .map(|v| v.unwrap_or_default())
            .collect();
        let mut chunk = chunk.clone();
        chunk.with_column(Column::new(COL_TRADUCIDO.into(), titulos.clone()))?;

        if modificar_oem && chunk.column(COL_OEM).is_ok() {
            chunk = aplicar_oem3(&chunk)?;
        }

        let mut salidas: Vec<String> = Vec::with_capacity(titulos.len());
        let mut motivos: Vec<Option<String>> = Vec::with_capacity(titulos.len());
        for t in &titulos {
            let (salida, motivo) = match self.cache.get(t) {
                Some(par) => par.clone(),
                None => {
                    let resultado = clasificar_uno(t, &self.patrones, &self.marcas_lower);
                    if self.cache.len() < self.limite_cache {
                        self.cache.insert(t.clone(), resultado.clone());
                    }
                    resultado
                }
            };
            salidas.push(salida);
            motivos.push(motivo);
        }

        chunk.with_column(Column::new(COL_TRADUCIDO.into(), salidas))?;
        chunk.with_column(Column::new(COL_MOTIVO_ELIMINACION.into(), motivos))?;

        let mask_eliminada = BooleanChunked::from_iter_values(
            "m".into(),
            chunk
                .column(COL_MOTIVO_ELIMINACION)?
                .str()?
                .iter()
                .map(|v| v.is_some()),
        );
        let mask_valida: BooleanChunked = mask_eliminada.clone().not();

        let validas = chunk.filter(&mask_valida)?.drop(COL_MOTIVO_ELIMINACION)?;
        let mut eliminadas = chunk.filter(&mask_eliminada)?;
        if eliminadas.height() > 0 {
            let alto = eliminadas.height();
            eliminadas.with_column(Column::new(
                COL_HOJA_ORIGEN.into(),
                vec![nombre_hoja.to_string(); alto],
            ))?;
        }
        Ok((validas, eliminadas))
    }
}

/// Procesador de publicaciones a gran escala: limpia/valida títulos (7
/// reglas + palabras prohibidas) y deduplica por 'Traducido' conservando la
/// fila de menor 'Precio2'. Ninguna fila se pierde: las que no sobreviven
/// se mueven a la hoja `Eliminadas` con su motivo.
pub struct Procesador {
    pub archivo_entrada: PathBuf,
    pub archivo_salida: PathBuf,
    pub chunk_size: usize,
    pub modificar_oem: bool,
    umbral_ram_dedup: u64,
    particiones_dedup: usize,
}

impl Procesador {
    pub fn nuevo(
        archivo_entrada: impl Into<PathBuf>,
        archivo_salida: impl Into<PathBuf>,
        chunk_size: usize,
        modificar_oem: bool,
    ) -> Self {
        Self {
            archivo_entrada: archivo_entrada.into(),
            archivo_salida: archivo_salida.into(),
            chunk_size,
            modificar_oem,
            umbral_ram_dedup: UMBRAL_RAM_DEDUP,
            particiones_dedup: PARTICIONES_DEDUPLICACION,
        }
    }

    /// Fuerza el umbral de dedup en memoria (para tests: verificar que el
    /// camino particionado a disco da el mismo resultado que el de RAM).
    pub fn con_umbral_ram_dedup(mut self, umbral: u64) -> Self {
        self.umbral_ram_dedup = umbral;
        self
    }

    pub fn con_particiones_dedup(mut self, n: usize) -> Self {
        self.particiones_dedup = n.max(1);
        self
    }

    fn pasada1(
        &self,
        escritor: &mut EscritorXlsx,
        mut avisar: impl FnMut(&str),
        mut progreso: impl FnMut(u64),
        mut on_validas: impl FnMut(&str, DataFrame) -> CoreResult<()>,
    ) -> CoreResult<Stats> {
        let mut stats = Stats::default();
        let mut clasificador = Clasificador::nuevo();
        crate::lectura::iter_hojas_streaming(
            &self.archivo_entrada,
            self.chunk_size,
            &mut avisar,
            |nombre_hoja, chunk| {
                let filas_in = chunk.height() as u64;
                let (validas, eliminadas) =
                    clasificador.procesar_chunk(&chunk, nombre_hoja, self.modificar_oem)?;
                stats.chunks += 1;
                stats.eliminadas += eliminadas.height();
                if eliminadas.height() > 0 {
                    escritor.escribir(&eliminadas, Some("Eliminadas"))?;
                }
                if validas.height() > 0 {
                    on_validas(nombre_hoja, validas)?;
                }
                progreso(filas_in);
                Ok(())
            },
        )?;
        Ok(stats)
    }

    fn procesar_en_memoria(
        &self,
        escritor: &mut EscritorXlsx,
        mut avisar: impl FnMut(&str),
        progreso: impl FnMut(u64),
    ) -> CoreResult<Stats> {
        let mut validas_acc: Vec<DataFrame> = Vec::new();
        let mut stats = self.pasada1(escritor, &mut avisar, progreso, |nombre_hoja, validas| {
            let alto = validas.height();
            let mut validas = validas;
            validas.with_column(Column::new(
                COL_HOJA_ORIGEN.into(),
                vec![nombre_hoja.to_string(); alto],
            ))?;
            validas_acc.push(validas);
            Ok(())
        })?;
        if !validas_acc.is_empty() {
            let df_todo = concat_diagonal(validas_acc)?;
            escribir_dedup(df_todo, escritor, &mut stats, &mut avisar)?;
        }
        Ok(stats)
    }

    fn procesar_particionado(
        &self,
        escritor: &mut EscritorXlsx,
        mut avisar: impl FnMut(&str),
        progreso: impl FnMut(u64),
        total_est: u64,
    ) -> CoreResult<Stats> {
        let n_part = if total_est > 0 {
            self.particiones_dedup.max(total_est.div_ceil(500_000) as usize)
        } else {
            self.particiones_dedup
        };
        // Mecanismo de particionado (buffer + spill a `.ipc` + semilla de
        // hash aleatoria por corrida) compartido con
        // `publications_builder::compatibilidad` vía
        // `commerce_core::AcumuladorParticionado`.
        let mut acumulador = AcumuladorParticionado::nuevo(n_part, 1_000_000)?;

        let mut stats = self.pasada1(escritor, &mut avisar, progreso, |nombre_hoja, validas| {
            let alto = validas.height();
            let mut validas = validas;
            validas.with_column(Column::new(
                COL_HOJA_ORIGEN.into(),
                vec![nombre_hoja.to_string(); alto],
            ))?;
            acumulador.agregar(&validas, COL_TRADUCIDO)
        })?;

        acumulador.finalizar(|bucket| escribir_dedup(bucket, escritor, &mut stats, &mut avisar))?;
        Ok(stats)
    }

    /// Lee y clasifica el archivo en streaming, deduplica (en memoria o
    /// particionado a disco, según el tamaño estimado) y escribe la
    /// salida. Un error no deja un `.xlsx` a medias: se aborta el escritor.
    pub fn procesar_masivo(
        &self,
        mut avisar: impl FnMut(&str),
        mut progreso: impl FnMut(u64),
    ) -> CoreResult<Stats> {
        let total_est = crate::lectura::estimar_total_filas(&self.archivo_entrada);
        let en_ram = total_est > 0 && total_est <= self.umbral_ram_dedup;

        let avisos_escritor = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let avisos_clon = avisos_escritor.clone();
        let mut escritor = crate::escritor::nuevo_escritor(&self.archivo_salida, move |m: &str| {
            avisos_clon.borrow_mut().push(m.to_string())
        })?;

        let resultado: CoreResult<Stats> = if en_ram {
            self.procesar_en_memoria(&mut escritor, &mut avisar, &mut progreso)
        } else {
            self.procesar_particionado(&mut escritor, &mut avisar, &mut progreso, total_est)
        };

        for aviso in avisos_escritor.borrow().iter() {
            avisar(aviso);
        }

        match resultado {
            Ok(stats) => {
                escritor.cerrar()?;
                Ok(stats)
            }
            Err(error) => {
                escritor.abortar()?;
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escribir_libro(ruta: &std::path::Path, hojas: &[(&str, Vec<Vec<Option<&str>>>)]) {
        use rust_xlsxwriter::Workbook;
        let mut wb = Workbook::new();
        for (nombre, filas) in hojas {
            let hoja = wb.add_worksheet().set_name(*nombre).unwrap();
            for (f, fila) in filas.iter().enumerate() {
                for (c, valor) in fila.iter().enumerate() {
                    if let Some(v) = valor {
                        hoja.write(f as u32, c as u16, *v).unwrap();
                    }
                }
            }
        }
        wb.save(ruta).unwrap();
    }

    fn leer_hojas(ruta: &std::path::Path) -> HashMap<String, DataFrame> {
        let mut libro = commerce_core::abrir_libro(ruta).unwrap();
        let nombres = commerce_core::nombres_hojas_libro(&libro);
        nombres
            .into_iter()
            .map(|n| {
                let df = commerce_core::leer_hoja_por_nombre(&mut libro, ruta, &n).unwrap();
                (n, df)
            })
            .collect()
    }

    fn libro_de_prueba(ruta: &std::path::Path) {
        escribir_libro(
            ruta,
            &[(
                "Hoja1",
                vec![
                    vec![Some("Traducido"), Some("Precio2"), Some("OEM")],
                    vec![
                        Some("Disco Freno Delantero Honda Civic Touring"),
                        Some("5000"),
                        Some("A1"),
                    ],
                    vec![
                        Some("Disco Freno Delantero Honda Civic Touring"),
                        Some("4000"),
                        Some("A2"),
                    ],
                    vec![
                        Some("Bosch Amortiguador Delantero Toyota Corolla Sport"),
                        Some("3000"),
                        Some("A3"),
                    ],
                    vec![
                        Some("Radio Pantalla Tactil Para Auto Con Bluetooth"),
                        Some("2000"),
                        Some("A4"),
                    ],
                    vec![Some("Corto"), Some("1000"), Some("A5")],
                    vec![
                        Some("Disco Clutch Caterpillar 3116 938g 3126 38f"),
                        Some("1500"),
                        Some("A6"),
                    ],
                    vec![
                        Some("Amortiguador Trasero Nissan Sentra Clasico B13"),
                        None,
                        Some("A7"),
                    ],
                ],
            )],
        );
    }

    fn verificar_resultado(salida: &std::path::Path) {
        let hojas = leer_hojas(salida);
        let validas = &hojas["Hoja1"];
        let eliminadas = &hojas["Eliminadas"];

        assert_eq!(validas.height(), 3);
        let titulos: HashSet<String> = validas
            .column(COL_TRADUCIDO)
            .unwrap()
            .str()
            .unwrap()
            .iter()
            .map(|v| v.unwrap().to_string())
            .collect();

        let dup = validas
            .clone()
            .lazy()
            .filter(col(COL_TRADUCIDO).eq(lit("Disco Freno Delantero Honda Civic Touring")))
            .collect()
            .unwrap();
        assert_eq!(
            dup.column("Precio2").unwrap().str().unwrap().get(0),
            Some("4000"),
            "el duplicado conserva el de MENOR Precio2"
        );

        assert!(
            titulos.contains("Amortiguador Delantero Toyota Corolla Sport"),
            "marca 'Bosch' borrada"
        );
        assert!(
            titulos.contains("Amortiguador Trasero Nissan Sentra Clasico B13"),
            "un Precio2 vacío único debe SOBREVIVIR"
        );

        assert_eq!(eliminadas.height(), 4);
        let motivos: Vec<String> = eliminadas
            .column(COL_MOTIVO_ELIMINACION)
            .unwrap()
            .str()
            .unwrap()
            .iter()
            .map(|v| v.unwrap().to_string())
            .collect();
        let unidos = motivos.join(" | ");
        for esperado in [
            "Palabra prohibida(Radio)",
            "Longitud fuera de rango",
            "(Regla 6)",
            "Duplicado - precio más alto",
        ] {
            assert!(
                unidos.contains(esperado),
                "falta motivo {esperado:?} en {unidos:?}"
            );
        }
    }

    #[test]
    fn e2e_procesar_masivo_en_memoria() {
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("in.xlsx");
        libro_de_prueba(&entrada);

        let salida = tmp.path().join("out.xlsx");
        let procesador = Procesador::nuevo(&entrada, &salida, 3, false);
        procesador.procesar_masivo(|_| {}, |_| {}).unwrap();
        verificar_resultado(&salida);
    }

    #[test]
    fn particionado_da_el_mismo_resultado_que_en_memoria() {
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("in.xlsx");
        libro_de_prueba(&entrada);

        let salida = tmp.path().join("out_part.xlsx");
        let procesador = Procesador::nuevo(&entrada, &salida, 3, false)
            .con_umbral_ram_dedup(0)
            .con_particiones_dedup(3);
        procesador.procesar_masivo(|_| {}, |_| {}).unwrap();
        verificar_resultado(&salida);
    }

    #[test]
    fn columna_reservada_aborta_sin_dejar_salida() {
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("res.xlsx");
        escribir_libro(
            &entrada,
            &[(
                "Hoja1",
                vec![
                    vec![Some("Traducido"), Some("Motivo_Eliminacion")],
                    vec![Some("t"), Some("x")],
                ],
            )],
        );

        let salida = tmp.path().join("res_out.xlsx");
        let procesador = Procesador::nuevo(&entrada, &salida, 10, false);
        assert!(procesador.procesar_masivo(|_| {}, |_| {}).is_err());
        assert!(!salida.exists(), "no debe quedar salida a medias");
    }

    #[test]
    fn hoja_de_entrada_llamada_eliminadas_aborta() {
        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("hoja.xlsx");
        escribir_libro(
            &entrada,
            &[("Eliminadas", vec![vec![Some("Traducido")], vec![Some("t")]])],
        );

        let salida = tmp.path().join("hoja_out.xlsx");
        let procesador = Procesador::nuevo(&entrada, &salida, 10, false);
        assert!(procesador.procesar_masivo(|_| {}, |_| {}).is_err());
        assert!(!salida.exists());
    }

    #[test]
    fn archivo_corrupto_no_deja_salida_a_medias() {
        let tmp = tempfile::tempdir().unwrap();
        let bueno = tmp.path().join("bueno.xlsx");
        escribir_libro(&bueno, &[("Hoja1", vec![vec![Some("Traducido")]])]);
        let bytes = std::fs::read(&bueno).unwrap();
        let truncado = tmp.path().join("corrupto.xlsx");
        std::fs::write(&truncado, &bytes[..bytes.len() / 2]).unwrap();

        let salida = tmp.path().join("salida.xlsx");
        let procesador = Procesador::nuevo(&truncado, &salida, 10, false);
        assert!(procesador.procesar_masivo(|_| {}, |_| {}).is_err());
        assert!(!salida.exists());
    }

    #[test]
    fn dedup_bucket_no_entra_en_panico_con_un_precio2_textual_nan_o_inf() {
        // "nan"/"inf"/"-inf" parsean con ÉXITO como f64 en Rust (a diferencia
        // de, por ejemplo, "" o "abc"), así que antes del fix caían en el
        // camino feliz del `.parse().ok()` en vez de en el default de
        // `+inf` — y luego el `partial_cmp(...).unwrap()` del ordenamiento
        // entraba en pánico al comparar contra `NaN`, tumbando todo
        // `procesar_masivo` por una sola celda mal formada.
        let df = df! {
            COL_TRADUCIDO => [
                "Disco Freno Delantero Honda Civic Touring",
                "Disco Freno Delantero Honda Civic Touring",
                "Disco Freno Delantero Honda Civic Touring",
            ],
            COL_PRECIO2 => ["nan", "5000", "-inf"],
        }
        .unwrap();

        let (mantener, duplicados) = dedup_bucket(&df, &mut |_| {}).expect("no debe entrar en pánico");

        assert_eq!(mantener.height(), 1, "solo sobrevive una fila del grupo");
        assert_eq!(duplicados.height(), 2);
        assert_eq!(
            mantener.column(COL_PRECIO2).unwrap().str().unwrap().get(0),
            Some("5000"),
            "el valor numérico real es siempre 'más barato' que un literal nan/inf"
        );
    }

    #[test]
    fn dedup_bucket_avisa_cuando_falta_precio2_en_vez_de_saltear_en_silencio() {
        let df = df! {
            COL_TRADUCIDO => ["Disco Freno Delantero Honda Civic Touring"],
        }
        .unwrap();

        let mut avisos: Vec<String> = Vec::new();
        let (mantener, duplicados) =
            dedup_bucket(&df, &mut |m| avisos.push(m.to_string())).expect("no debe fallar");

        assert_eq!(
            mantener.height(),
            1,
            "sin Precio2 no se deduplica, pero no se pierde la fila"
        );
        assert_eq!(duplicados.height(), 0);
        assert!(
            avisos.iter().any(|m| m.contains("Precio2")),
            "debe avisar que no se pudo deduplicar por falta de 'Precio2': {avisos:?}"
        );
    }

    #[test]
    fn cache_de_clasificacion_no_crece_mas_alla_del_limite_pero_sigue_clasificando_todo() {
        // El caché de `Clasificador` necesita un tope: sin él crece con el
        // número de títulos DISTINTOS de toda la corrida, y un archivo con
        // muchísimos únicos consume memoria sin límite. `limite_cache` se
        // fija bajo (2) para poder ejercitar el
        // camino "caché lleno" sin procesar cientos de miles de títulos.
        let palabras = palabras_borradas();
        let mut clasificador = Clasificador {
            patrones: Patrones::compilar(),
            marcas_lower: palabras_borradas_lower(&palabras),
            cache: HashMap::new(),
            limite_cache: 2,
        };

        let df = df! {
            COL_TRADUCIDO => [
                "Disco Freno Delantero Honda Civic",
                "Filtro Aire Motor Ford Focus",
                "Bomba Agua Toyota Corolla",
                "Disco Freno Delantero Honda Civic",
            ],
        }
        .unwrap();

        let (validas, eliminadas) = clasificador
            .procesar_chunk(&df, "Hoja1", false)
            .expect("no debe fallar aunque el caché no alcance para todos los títulos");

        assert!(
            clasificador.cache.len() <= 2,
            "el caché nunca debe superar el límite configurado, tiene {}",
            clasificador.cache.len()
        );
        assert_eq!(
            validas.height() + eliminadas.height(),
            4,
            "ninguna fila se pierde ni queda sin clasificar aunque el caché esté lleno"
        );
    }
}
