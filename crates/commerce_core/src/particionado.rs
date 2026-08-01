//! Acumulador de filas particionado a disco por hash de una columna clave,
//! para deduplicar/agrupar de forma GLOBAL (entre TODOS los bloques/hojas de
//! un streaming largo, no solo el sub-bloque en curso) sin cargar el archivo
//! completo en RAM.
//!
//! Solo el MECANISMO de particionado vive acá, compartido por
//! `publications_builder` y `publications_validator`. La POLÍTICA de qué
//! hacer con cada partición ya reunida (qué dedup aplicar, en qué hoja
//! escribir) es una diferencia de negocio real entre ambos y queda al
//! llamador, vía el closure de [`AcumuladorParticionado::finalizar`].

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::path::PathBuf;

use polars::prelude::*;

use crate::columnas::{columna_texto, concat_diagonal, tomar_filas};
use crate::error::CoreResult;

pub struct AcumuladorParticionado {
    tmpdir: tempfile::TempDir,
    n_part: usize,
    buffer_part: Vec<Vec<DataFrame>>,
    archivos_part: Vec<Vec<PathBuf>>,
    contador_archivos: Vec<usize>,
    buffer_filas: usize,
    filas_buffer_umbral: usize,
    // Semilla aleatoria por CORRIDA (no por valor: todos los valores de esta
    // misma corrida deben hashear de forma CONSISTENTE entre sí para caer
    // siempre en la misma partición). `DefaultHasher::new()` a secas usa
    // claves fijas (SipHash con clave (0,0)) — un archivo de entrada
    // diseñado a propósito podría forzar que todo caiga en una sola
    // partición, anulando el acotado de memoria que este particionado
    // existe para garantizar. `RandomState::new()` sortea una semilla nueva
    // una sola vez acá; `build_hasher()`/`hash_one()` la reusan para cada
    // valor.
    hasher_state: RandomState,
}

impl AcumuladorParticionado {
    /// `n_part` particiones (mínimo 1); cada una acumula en RAM hasta que el
    /// total de filas en buffer llega a `filas_buffer_umbral`, momento en el
    /// que se vuelca a un archivo `.ipc` temporal por partición.
    pub fn nuevo(n_part: usize, filas_buffer_umbral: usize) -> CoreResult<Self> {
        let n_part = n_part.max(1);
        Ok(Self {
            tmpdir: tempfile::tempdir()?,
            n_part,
            buffer_part: vec![Vec::new(); n_part],
            archivos_part: vec![Vec::new(); n_part],
            contador_archivos: vec![0; n_part],
            buffer_filas: 0,
            filas_buffer_umbral,
            hasher_state: RandomState::new(),
        })
    }

    /// Reparte las filas de `df` entre particiones según el hash de
    /// `columna_clave`, acumulando en RAM hasta `filas_buffer_umbral` antes
    /// de volcar a disco. Se llama una vez por sub-bloque durante streaming.
    pub fn agregar(&mut self, df: &DataFrame, columna_clave: &str) -> CoreResult<()> {
        if df.height() == 0 {
            return Ok(());
        }
        let alto = df.height();
        let claves = columna_texto(df, columna_clave)?;
        let mut por_particion: Vec<Vec<usize>> = vec![Vec::new(); self.n_part];
        for (i, v) in claves.iter().enumerate() {
            let pid = (self.hasher_state.hash_one(v) % self.n_part as u64) as usize;
            por_particion[pid].push(i);
        }
        for (pid, indices) in por_particion.into_iter().enumerate() {
            if !indices.is_empty() {
                self.buffer_part[pid].push(tomar_filas(df, &indices)?);
            }
        }
        self.buffer_filas += alto;
        if self.buffer_filas >= self.filas_buffer_umbral {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> CoreResult<()> {
        for p in 0..self.n_part {
            if self.buffer_part[p].is_empty() {
                continue;
            }
            let dfs = std::mem::take(&mut self.buffer_part[p]);
            let mut df = concat_diagonal(dfs)?;
            let ruta = self
                .tmpdir
                .path()
                .join(format!("p{p}_{}.ipc", self.contador_archivos[p]));
            self.contador_archivos[p] += 1;
            let mut archivo = std::fs::File::create(&ruta)?;
            IpcWriter::new(&mut archivo).finish(&mut df)?;
            self.archivos_part[p].push(ruta);
        }
        self.buffer_filas = 0;
        Ok(())
    }

    /// Vuelca lo que quede en buffer y, para cada partición con datos,
    /// relee sus archivos `.ipc`, los concatena en un único `DataFrame` (la
    /// partición COMPLETA, de todos los bloques/hojas que cayeron ahí) y se
    /// lo pasa a `por_bucket` — quien decide la política real de
    /// dedup/agrupamiento y dónde escribir el resultado. Memoria acotada:
    /// cada partición vive en RAM una a la vez, nunca el archivo completo.
    pub fn finalizar(mut self, mut por_bucket: impl FnMut(DataFrame) -> CoreResult<()>) -> CoreResult<()> {
        self.flush()?;
        for rutas in &self.archivos_part {
            if rutas.is_empty() {
                continue;
            }
            let dfs: Vec<DataFrame> = rutas
                .iter()
                .map(|r| -> CoreResult<DataFrame> {
                    let archivo = std::fs::File::open(r)?;
                    Ok(IpcReader::new(archivo).finish()?)
                })
                .collect::<CoreResult<_>>()?;
            let bucket = concat_diagonal(dfs)?;
            por_bucket(bucket)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn agrupa_por_clave_entre_llamadas_a_agregar_separadas() -> CoreResult<()> {
        // Dos `agregar()` distintos (simulando dos bloques/hojas separados)
        // con la MISMA clave repartida entre ambos: `finalizar` debe verlas
        // juntas en el mismo bucket, no una por separado — eso es justo lo
        // que el particionado global (vs. el dedup solo-local que tenían
        // ambos crates antes de esta clase) existe para garantizar.
        let mut acc = AcumuladorParticionado::nuevo(4, 1_000_000)?;
        acc.agregar(&df!("Clave" => ["A", "B"], "V" => [1, 2])?, "Clave")?;
        acc.agregar(&df!("Clave" => ["A", "C"], "V" => [3, 4])?, "Clave")?;

        let mut vistos: Vec<(String, i32)> = Vec::new();
        acc.finalizar(|bucket| {
            let claves = columna_texto(&bucket, "Clave")?;
            let valores = bucket.column("V")?.i32()?.clone();
            for (k, v) in claves.iter().zip(valores.iter()) {
                vistos.push((k.clone().unwrap(), v.unwrap()));
            }
            Ok(())
        })?;
        vistos.sort();
        assert_eq!(
            vistos,
            vec![
                ("A".to_string(), 1),
                ("A".to_string(), 3),
                ("B".to_string(), 2),
                ("C".to_string(), 4),
            ]
        );
        Ok(())
    }

    #[test]
    fn flush_automatico_por_umbral_no_pierde_filas() -> CoreResult<()> {
        // Umbral bajo a propósito para forzar varios `flush()` a disco
        // dentro de un único `agregar()` de muchas filas.
        let mut acc = AcumuladorParticionado::nuevo(3, 2)?;
        let claves: Vec<String> = (0..50).map(|i| format!("k{i}")).collect();
        acc.agregar(&df!("Clave" => claves.clone())?, "Clave")?;

        let mut total = 0usize;
        acc.finalizar(|bucket| {
            total += bucket.height();
            Ok(())
        })?;
        assert_eq!(
            total, 50,
            "ninguna fila debe perderse entre los flush intermedios"
        );
        Ok(())
    }

    #[test]
    fn filas_vacias_no_generan_particiones_ni_llamadas_a_por_bucket() -> CoreResult<()> {
        let mut acc = AcumuladorParticionado::nuevo(4, 1_000_000)?;
        acc.agregar(&df!("Clave" => Vec::<String>::new())?, "Clave")?;

        let mut llamadas = 0usize;
        acc.finalizar(|_bucket| {
            llamadas += 1;
            Ok(())
        })?;
        assert_eq!(llamadas, 0);
        Ok(())
    }

    proptest::proptest! {
        // Invariante (docs/decisiones/0006-tests-reactivos-vs-invariantes.md):
        // sin importar cuántas particiones, qué umbral de buffer, o en
        // cuántas llamadas a `agregar` se repartan las filas (simulando
        // varios bloques/hojas), el multiset de claves que `finalizar`
        // entrega a `por_bucket` debe coincidir EXACTO con lo que entró —
        // ni una fila de más, ni una de menos.
        #[test]
        fn ninguna_fila_se_pierde_sin_importar_particiones_ni_umbral_de_buffer(
            claves in proptest::collection::vec("[a-z]{1,3}", 0..200),
            n_part in 1usize..8,
            umbral in 1usize..50,
        ) {
            let mut acc = AcumuladorParticionado::nuevo(n_part, umbral).unwrap();
            for lote in claves.chunks(7) {
                let df = df!("Clave" => lote.to_vec()).unwrap();
                acc.agregar(&df, "Clave").unwrap();
            }
            let mut vistas: Vec<String> = Vec::new();
            acc.finalizar(|bucket| {
                vistas.extend(columna_texto(&bucket, "Clave")?.into_iter().map(|v| v.unwrap_or_default()));
                Ok(())
            }).unwrap();
            let mut esperadas = claves.clone();
            esperadas.sort();
            vistas.sort();
            prop_assert_eq!(vistas, esperadas, "el multiset de claves debe preservarse exactamente");
        }
    }
}
