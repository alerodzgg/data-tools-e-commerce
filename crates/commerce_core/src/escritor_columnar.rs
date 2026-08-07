//! Formato de INTERCAMBIO entre herramientas: Arrow IPC columnar.
//!
//! El XLSX es un formato de PRESENTACIÓN — existe para que una persona lo
//! abra en Excel. Usarlo también como formato intermedio entre herramientas
//! obliga a un viaje completo por texto en cada paso: serializar a XML,
//! comprimir, descomprimir, y volver a parsear.
//!
//! Medido sobre 6M de celdas, ese viaje cuesta ~4 s de escritura y ~4,5 s de
//! lectura, con un piso irreducible de 4,2 s impuesto por los 341 MB de XML
//! que hay que producir y recorrer. Ninguna optimización del parser baja de
//! ahí: el costo es el formato, no el código.
//!
//! Arrow IPC guarda las columnas tal como Polars las tiene en memoria. Leerlo
//! es esencialmente copiar buffers, sin parseo de texto ni construcción de
//! cadenas. El XLSX queda para el archivo final que ve el usuario.
//!
//! ## Qué NO hace
//!
//! No tiene hojas. `escribir(df, Some("Eliminadas"))` agrega una columna
//! `_hoja` con ese nombre en vez de fallar, para que los llamadores que
//! escriben en varias hojas sigan funcionando y la información no se pierda.

use std::fs::File;
use std::path::{Path, PathBuf};

use polars::prelude::*;

use crate::error::CoreResult;
use crate::rutas::ruta_unica;

/// Columna que preserva el nombre de hoja al que iba cada bloque.
///
/// Arrow IPC no tiene hojas; sin esta columna, escribir a dos hojas
/// distintas mezclaría los datos sin ninguna señal.
pub const COL_HOJA: &str = "_hoja";

/// Escritor de intercambio columnar. Acumula los bloques y los vuelca al
/// cerrar: Arrow IPC necesita un esquema único para todo el archivo, así que
/// no se puede ir escribiendo bloque a bloque con esquemas distintos.
///
/// Eso lo hace inadecuado para volúmenes que no entren en memoria — para eso
/// está [`crate::EscritorXlsx`], que escribe en streaming. Acá el uso previsto
/// es el intercambio entre pasos de un pipeline, donde el `DataFrame` ya está
/// en memoria de todos modos.
pub struct EscritorIpc {
    pub ruta: PathBuf,
    bloques: Vec<DataFrame>,
    cerrado: bool,
    pub total: usize,
}

impl EscritorIpc {
    pub fn nuevo(ruta: impl AsRef<Path>) -> CoreResult<Self> {
        Ok(Self {
            ruta: ruta_unica(ruta),
            bloques: Vec::new(),
            cerrado: false,
            total: 0,
        })
    }

    /// Añade un bloque. `hoja` se conserva en la columna [`COL_HOJA`].
    pub fn escribir(&mut self, df: &DataFrame, hoja: Option<&str>) -> CoreResult<()> {
        if self.cerrado {
            return Err(std::io::Error::other(
                "EscritorIpc: no se puede escribir después de cerrar()/abortar()",
            )
            .into());
        }
        if df.height() == 0 {
            return Ok(());
        }
        let mut bloque = df.clone();
        if let Some(nombre) = hoja {
            let alto = bloque.height();
            bloque.with_column(Column::new(COL_HOJA.into(), vec![nombre; alto]))?;
        }
        self.total += bloque.height();
        self.bloques.push(bloque);
        Ok(())
    }

    /// Vuelca todo y cierra. Idempotente.
    ///
    /// Ante un error deja el archivo a medias BORRADO, igual que el resto de
    /// los escritores del workspace (ADR 0005).
    pub fn cerrar(&mut self) -> CoreResult<()> {
        if self.cerrado {
            return Ok(());
        }
        let resultado = self.volcar();
        self.cerrado = true;
        if resultado.is_err() {
            let _ = std::fs::remove_file(&self.ruta);
        }
        resultado
    }

    fn volcar(&mut self) -> CoreResult<()> {
        let mut df = match self.bloques.len() {
            0 => DataFrame::empty(),
            1 => std::mem::take(&mut self.bloques).remove(0),
            _ => {
                let bloques = std::mem::take(&mut self.bloques);
                let mut acumulado = bloques[0].clone();
                for siguiente in &bloques[1..] {
                    acumulado.vstack_mut(siguiente)?;
                }
                acumulado
            }
        };
        let archivo = File::create(&self.ruta)?;
        IpcWriter::new(archivo).finish(&mut df)?;
        Ok(())
    }

    /// Tras un error: descarta lo acumulado y borra el archivo si existía.
    pub fn abortar(&mut self) -> CoreResult<()> {
        if self.cerrado {
            return Ok(());
        }
        self.cerrado = true;
        self.bloques.clear();
        let _ = std::fs::remove_file(&self.ruta);
        Ok(())
    }
}

impl Drop for EscritorIpc {
    /// Red de seguridad: si nadie llamó a `cerrar()`, se vuelca igual en vez
    /// de perder los datos en silencio.
    fn drop(&mut self) {
        let _ = self.cerrar();
    }
}

/// Lee un archivo Arrow IPC. Si trae la columna [`COL_HOJA`], devuelve un
/// `DataFrame` por hoja (en orden de aparición) para que el resultado sea
/// equivalente al de leer un XLSX de varias hojas.
pub fn leer_ipc(ruta: &Path) -> CoreResult<Vec<DataFrame>> {
    let archivo = File::open(ruta)?;
    let df = IpcReader::new(archivo).finish()?;
    if !df.get_column_names().iter().any(|c| c.as_str() == COL_HOJA) {
        return Ok(vec![df]);
    }

    // Se agrupa conservando el ORDEN de aparición: iterar un `HashMap` haría
    // que dos corridas idénticas devolvieran las hojas barajadas.
    let hojas = df.column(COL_HOJA)?.str()?.clone();
    let mut orden: Vec<String> = Vec::new();
    let mut vistos: std::collections::HashSet<String> = std::collections::HashSet::new();
    for valor in hojas.iter() {
        let nombre = valor.unwrap_or("").to_string();
        if vistos.insert(nombre.clone()) {
            orden.push(nombre);
        }
    }

    let mut salida = Vec::with_capacity(orden.len());
    for nombre in orden {
        let mascara = hojas.equal(nombre.as_str());
        salida.push(df.filter(&mascara)?.drop(COL_HOJA)?);
    }
    Ok(salida)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn df_codigos() -> DataFrame {
        df!(
            "Sku" => ["007", "0012", "1e5"],
            "Nombre" => ["a", "b", "c"],
        )
        .unwrap()
    }

    #[test]
    fn los_codigos_sobreviven_el_viaje_columnar() {
        // El contrato central del producto: un SKU no se reinterpreta nunca.
        // Arrow guarda el tipo junto a los datos, así que acá ni siquiera hay
        // ocasión de adivinar — pero se fija igual, porque es la razón de ser
        // de todo el pipeline.
        let tmp = tempfile::tempdir().unwrap();
        let mut e = EscritorIpc::nuevo(tmp.path().join("c.ipc")).unwrap();
        e.escribir(&df_codigos(), None).unwrap();
        e.cerrar().unwrap();

        let leido = leer_ipc(&e.ruta).unwrap();
        assert_eq!(leido.len(), 1);
        assert_eq!(leido[0], df_codigos());
    }

    #[test]
    fn varias_hojas_vuelven_separadas_y_en_orden() {
        let tmp = tempfile::tempdir().unwrap();
        let mut e = EscritorIpc::nuevo(tmp.path().join("h.ipc")).unwrap();
        e.escribir(&df!("A" => ["1"]).unwrap(), Some("Primera")).unwrap();
        e.escribir(&df!("A" => ["2"]).unwrap(), Some("Segunda")).unwrap();
        e.escribir(&df!("A" => ["3"]).unwrap(), Some("Primera")).unwrap();
        e.cerrar().unwrap();

        let leido = leer_ipc(&e.ruta).unwrap();
        assert_eq!(leido.len(), 2, "una por hoja distinta");
        assert_eq!(leido[0].height(), 2, "las dos filas de 'Primera'");
        assert_eq!(leido[1].height(), 1);
        assert!(
            !leido[0].get_column_names().iter().any(|c| c.as_str() == COL_HOJA),
            "la columna de control no debe filtrarse al llamador"
        );
    }

    #[test]
    fn escribir_despues_de_cerrar_es_un_error_no_un_dato_perdido() {
        let tmp = tempfile::tempdir().unwrap();
        let mut e = EscritorIpc::nuevo(tmp.path().join("x.ipc")).unwrap();
        e.cerrar().unwrap();
        assert!(e.escribir(&df_codigos(), None).is_err());
    }

    #[test]
    fn abortar_no_deja_archivo() {
        let tmp = tempfile::tempdir().unwrap();
        let mut e = EscritorIpc::nuevo(tmp.path().join("a.ipc")).unwrap();
        e.escribir(&df_codigos(), None).unwrap();
        e.abortar().unwrap();
        assert!(!e.ruta.exists());
    }
}
