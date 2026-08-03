//! Buffer que empieza en RAM y pasa a disco si crece demasiado.
//!
//! No sabe nada de XLSX: es solo la política de memoria. Existe porque el
//! elemento `<dimension>` de una hoja va ANTES de los datos pero solo se
//! conoce al terminarla (necesita el nº de filas), así que el cuerpo de la
//! hoja hay que retenerlo entero hasta el cierre — y con 100M de filas eso no
//! entra en RAM.

use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};

/// Cuánto se acumula en RAM por hoja antes de volcar a un temporal en disco.
const RAM_POR_HOJA: usize = 64 * 1024 * 1024;

pub(super) enum SpoolTemp {
    Mem(Vec<u8>),
    Disco(File),
}

impl SpoolTemp {
    pub(super) fn nuevo() -> Self {
        SpoolTemp::Mem(Vec::new())
    }

    pub(super) fn escribir(&mut self, datos: &[u8]) -> io::Result<()> {
        match self {
            SpoolTemp::Mem(buf) => {
                if buf.len() + datos.len() > RAM_POR_HOJA {
                    let mut archivo = tempfile::tempfile()?;
                    archivo.write_all(buf)?;
                    archivo.write_all(datos)?;
                    *self = SpoolTemp::Disco(archivo);
                } else {
                    buf.extend_from_slice(datos);
                }
                Ok(())
            }
            SpoolTemp::Disco(archivo) => archivo.write_all(datos),
        }
    }

    pub(super) fn volcar_en<W: Write>(&mut self, salida: &mut W) -> io::Result<()> {
        match self {
            SpoolTemp::Mem(buf) => salida.write_all(buf),
            SpoolTemp::Disco(archivo) => {
                archivo.seek(SeekFrom::Start(0))?;
                io::copy(archivo, salida)?;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_spool_chico_se_queda_en_ram_y_devuelve_lo_escrito() {
        let mut spool = SpoolTemp::nuevo();
        spool.escribir(b"hola ").unwrap();
        spool.escribir(b"mundo").unwrap();
        assert!(matches!(spool, SpoolTemp::Mem(_)));

        let mut salida = Vec::new();
        spool.volcar_en(&mut salida).unwrap();
        assert_eq!(salida, b"hola mundo");
    }

    #[test]
    fn al_pasar_el_umbral_migra_a_disco_sin_perder_lo_ya_escrito() {
        // El contenido debe sobrevivir a la migración RAM→disco: es la única
        // parte del escritor donde los bytes cambian de soporte a mitad de
        // camino, y perderlos ahí daría un xlsx truncado sin ningún error.
        let mut spool = SpoolTemp::nuevo();
        spool.escribir(b"principio-").unwrap();
        spool.escribir(&vec![b'x'; RAM_POR_HOJA]).unwrap();
        assert!(matches!(spool, SpoolTemp::Disco(_)));

        let mut salida = Vec::new();
        spool.volcar_en(&mut salida).unwrap();
        assert_eq!(salida.len(), RAM_POR_HOJA + b"principio-".len());
        assert!(salida.starts_with(b"principio-xxx"));
    }

    #[test]
    fn volcar_dos_veces_da_lo_mismo_tambien_desde_disco() {
        // `volcar_en` hace `seek(0)` antes de copiar; sin eso, un segundo
        // volcado saldría vacío.
        let mut spool = SpoolTemp::nuevo();
        spool.escribir(&vec![b'z'; RAM_POR_HOJA + 1]).unwrap();

        let mut primera = Vec::new();
        spool.volcar_en(&mut primera).unwrap();
        let mut segunda = Vec::new();
        spool.volcar_en(&mut segunda).unwrap();
        assert_eq!(primera, segunda);
    }
}
