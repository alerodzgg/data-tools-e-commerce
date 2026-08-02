//! Modo «Contar caracteres de una columna»: añadir columna y ordenar,
//! dividir en dos hojas por un límite, o reporte de las que lo superan.

use std::path::Path;

use etl_tools::constantes::{COL_CARACTERES, FILAS_POR_HOJA};

use crate::comunes::{
    abortar_si_reservadas, armar_desde_bloques, escribir_por_bloques, excluir_refs,
    preguntar_hojas_excluir_de, seleccionar_archivo,
};
use crate::AppResult;

// ════════════════════════════════════════════════════════════════════════
// Modo — Dividir por caracteres
// ════════════════════════════════════════════════════════════════════════

fn preguntar_limite_caracteres() -> AppResult<u32> {
    loop {
        let texto =
            app_shell::pedir_texto("Número máximo de caracteres (cuenta los espacios):")?.unwrap_or_default();
        let limpio: String = texto.chars().filter(|c| !matches!(c, '.' | ',' | ' ')).collect();
        match limpio.parse::<u32>() {
            Ok(n) if n >= 1 => return Ok(n),
            _ => app_shell::warn("Escribe un número entero mayor que 0."),
        }
    }
}

pub(crate) fn dividir_por_caracteres(ruta_salida: &Path) -> AppResult<()> {
    let Some(archivo) = seleccionar_archivo("Selecciona el archivo a analizar")? else {
        app_shell::warn("No se seleccionó archivo. Volviendo al menú.");
        return Ok(());
    };
    let Some(hojas_excluir) = preguntar_hojas_excluir_de(&archivo, "")? else {
        return Ok(());
    };
    // Una sola apertura del archivo para columnas + datos (ver
    // `armar_desde_bloques` y el comentario de `ordenar_columna`).
    let bloques = etl_tools::iter_hojas_valores(
        std::slice::from_ref(&archivo),
        Some(&excluir_refs(&hojas_excluir)),
        app_shell::warn,
    )?;
    let columnas = commerce_core::columnas_union_de_bloques(&bloques);
    if columnas.is_empty() {
        app_shell::error(&format!(
            "No se pudieron leer columnas de '{}'.",
            archivo.file_name().unwrap_or_default().to_string_lossy()
        ));
        return Ok(());
    }
    if abortar_si_reservadas(&columnas) {
        return Ok(());
    }

    let Some(columna) = app_shell::menu_seleccionar_nav(
        "¿Qué columna quieres contar (caracteres, espacios incluidos)?",
        columnas.clone(),
    )?
    else {
        app_shell::warn("Operación cancelada.");
        return Ok(());
    };

    /// Qué se hace con el conteo de caracteres de la columna elegida.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum AccionConteo {
        ColumnaYOrdenar,
        DosHojas,
        ReporteAparte,
    }
    impl std::fmt::Display for AccionConteo {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                AccionConteo::ColumnaYOrdenar => {
                    write!(f, "Añadir columna con el conteo y ordenar todo el archivo")
                }
                AccionConteo::DosHojas => {
                    write!(f, "Dividir en 2 hojas por un límite: hasta N / más de N")
                }
                AccionConteo::ReporteAparte => {
                    write!(f, "Reporte aparte: solo las filas que superan un límite")
                }
            }
        }
    }

    let Some(accion) = app_shell::menu_seleccionar_nav(
        "¿Qué quieres hacer con el conteo?",
        vec![
            AccionConteo::ColumnaYOrdenar,
            AccionConteo::DosHojas,
            AccionConteo::ReporteAparte,
        ],
    )?
    else {
        app_shell::warn("Operación cancelada.");
        return Ok(());
    };

    // Solo el modo que ordena por el conteo no necesita un límite.
    let limite = if accion == AccionConteo::ColumnaYOrdenar {
        None
    } else {
        Some(preguntar_limite_caracteres()?)
    };

    if accion == AccionConteo::ColumnaYOrdenar && columnas.contains(&COL_CARACTERES.to_string()) {
        app_shell::error(&format!(
            "El archivo ya tiene una columna '{COL_CARACTERES}'; renómbrala y reintenta."
        ));
        return Ok(());
    }

    let stem = archivo.file_stem().unwrap_or_default().to_string_lossy();

    if accion == AccionConteo::ColumnaYOrdenar {
        // Este modo SÍ necesita el archivo completo en memoria: es un orden
        // global por la nueva columna de conteo, no un filtro por fila.
        app_shell::info(&format!(
            "Leyendo '{}'...",
            archivo.file_name().unwrap_or_default().to_string_lossy()
        ));
        let Some(df) = armar_desde_bloques(bloques, &columnas)? else {
            app_shell::warn("El archivo está vacío.");
            return Ok(());
        };
        let opciones_orden = vec!["Menor a mayor".to_string(), "Mayor a menor".to_string()];
        let ascendente = app_shell::menu_seleccionar("Orden por número de caracteres:", opciones_orden)?
            .as_deref()
            != Some("Mayor a menor");

        let df = etl_tools::anadir_columna_y_ordenar(&df, &columna, ascendente)?;
        let columnas_salida: Vec<String> = std::iter::once(COL_CARACTERES.to_string())
            .chain(columnas.iter().cloned())
            .collect();
        let df = df.select(&columnas_salida)?;
        let ruta = commerce_core::ruta_unica(ruta_salida.join(format!("conteo_{stem}.xlsx")));
        let mut escritor = etl_tools::nuevo_escritor(&ruta, columnas_salida)?;
        let barra = app_shell::barra_progreso(
            &format!(
                "Escribiendo {}",
                ruta.file_name().unwrap_or_default().to_string_lossy()
            ),
            df.height() as u64,
        );
        escribir_por_bloques(&mut escritor, &df, None, &barra)?;
        barra.finish_and_clear();
        let sentido = if ascendente {
            "menor a mayor"
        } else {
            "mayor a menor"
        };
        app_shell::success(&format!(
            "Guardado: '{}' ({} filas, '{COL_CARACTERES}' añadida y ordenada {sentido}).",
            ruta.file_name().unwrap_or_default().to_string_lossy(),
            escritor.total
        ));
        return Ok(());
    }

    // Los otros dos modos son un FILTRO: cada fila se clasifica solo por su
    // propia longitud (`dividir_dentro_fuera`), así que no hace falta juntar
    // el archivo entero en un único `DataFrame` (a diferencia del modo de
    // arriba) — se procesa bloque por bloque, sin la copia extra de
    // `armar_desde_bloques`.
    if bloques.is_empty() {
        app_shell::warn("El archivo está vacío.");
        return Ok(());
    }
    let limite = limite.expect("hojas/reporte siempre piden limite");
    let total_filas: u64 = bloques.iter().map(|df| df.height() as u64).sum();

    if accion == AccionConteo::DosHojas {
        let ruta = commerce_core::ruta_unica(ruta_salida.join(format!("dividido_{stem}.xlsx")));
        let mut escritor = commerce_core::EscritorXlsx::con_avisador(
            &ruta,
            commerce_core::escritor_xlsx::OpcionesEscritorXlsx {
                columnas: None,
                filas_por_hoja: FILAS_POR_HOJA,
                hoja_por_defecto: "Hoja1".to_string(),
                numerar_siempre: false,
                recortar_vacias: true,
            },
            app_shell::warn,
        )?;
        let barra = app_shell::barra_progreso(
            &format!(
                "Escribiendo {}",
                ruta.file_name().unwrap_or_default().to_string_lossy()
            ),
            total_filas,
        );
        let hoja_dentro = format!("Hasta_{limite}");
        let hoja_fuera = format!("Mas_de_{limite}");
        let mut n_dentro = 0u64;
        let mut n_fuera = 0u64;
        for chunk in &bloques {
            let preparado = etl_tools::preparar_chunk(chunk, &columnas)?.select(&columnas)?;
            let (dentro, fuera) = etl_tools::dividir_dentro_fuera(&preparado, &columna, limite)?;
            n_dentro += dentro.height() as u64;
            n_fuera += fuera.height() as u64;
            escribir_por_bloques(&mut escritor, &dentro, Some(&hoja_dentro), &barra)?;
            escribir_por_bloques(&mut escritor, &fuera, Some(&hoja_fuera), &barra)?;
        }
        barra.finish_and_clear();
        app_shell::success(&format!(
            "Guardado: '{}' — {n_dentro} fila(s) en 'Hasta_{limite}', {n_fuera} fila(s) en 'Mas_de_{limite}'.",
            ruta.file_name().unwrap_or_default().to_string_lossy(),
        ));
    } else {
        // Primero se cuenta SIN escribir nada, para no crear un archivo de
        // reporte vacío si ninguna fila supera el límite (mismo
        // comportamiento que antes, que decidía esto después de cargar todo).
        let mut n_fuera_total = 0u64;
        for chunk in &bloques {
            let preparado = etl_tools::preparar_chunk(chunk, &columnas)?.select(&columnas)?;
            let conteos = etl_tools::contar_caracteres(&preparado, &columna)?;
            n_fuera_total += conteos.iter().filter(|c| **c > limite).count() as u64;
        }
        if n_fuera_total == 0 {
            app_shell::info(&format!(
                "Ninguna fila supera los {limite} caracteres en '{columna}'. Sin reporte que generar."
            ));
            return Ok(());
        }
        let ruta =
            commerce_core::ruta_unica(ruta_salida.join(format!("reporte_mas_de_{limite}_{stem}.xlsx")));
        let mut escritor = etl_tools::nuevo_escritor(&ruta, columnas.clone())?;
        let barra = app_shell::barra_progreso(
            &format!(
                "Escribiendo {}",
                ruta.file_name().unwrap_or_default().to_string_lossy()
            ),
            n_fuera_total,
        );
        for chunk in &bloques {
            let preparado = etl_tools::preparar_chunk(chunk, &columnas)?.select(&columnas)?;
            let (_dentro, fuera) = etl_tools::dividir_dentro_fuera(&preparado, &columna, limite)?;
            if fuera.height() > 0 {
                escribir_por_bloques(&mut escritor, &fuera, None, &barra)?;
            }
        }
        barra.finish_and_clear();
        app_shell::success(&format!(
            "Reporte guardado: '{}' ({n_fuera_total} de {total_filas} filas superan {limite} caracteres en '{columna}').",
            ruta.file_name().unwrap_or_default().to_string_lossy(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializa los tests que mutan el `static` global de
    /// `app_shell::rutas` (vía `fijar_rutas`), para que cargo pueda seguir
    /// corriendo tests en paralelo sin que dos de ellos pisen la carpeta de
    /// entrada del otro a mitad de camino.
    fn rutas_mutex() -> &'static std::sync::Mutex<()> {
        static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        M.get_or_init(|| std::sync::Mutex::new(()))
    }

    #[test]
    fn dividir_por_caracteres_modo_hojas_particiona_por_bloques_sin_perder_filas() {
        // Cubre el refactor a streaming (sin `cargar_completo`/vstack) de las
        // 2 variantes de partición: cada fila se clasifica por su propia
        // longitud, acumulando dentro/fuera bloque por bloque en vez de
        // materializar el archivo entero de una vez.
        use app_shell::testing::{con_guion, Respuesta};
        use rust_xlsxwriter::Workbook;

        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("entrada");
        let salida = tmp.path().join("salida");
        std::fs::create_dir_all(&entrada).unwrap();
        std::fs::create_dir_all(&salida).unwrap();

        let archivo = entrada.join("datos.xlsx");
        let descripciones = [
            "corto".to_string(),
            "x".repeat(20),
            "y".repeat(15),
            "z".to_string(),
        ];
        {
            let mut wb = Workbook::new();
            let hoja = wb.add_worksheet();
            hoja.write(0, 0, "Sku").unwrap();
            hoja.write(0, 1, "Descripcion").unwrap();
            for (i, desc) in descripciones.iter().enumerate() {
                let sku = format!("A{}", i + 1);
                hoja.write(i as u32 + 1, 0, sku.as_str()).unwrap();
                hoja.write(i as u32 + 1, 1, desc.as_str()).unwrap();
            }
            wb.save(&archivo).unwrap();
        }

        // `app_shell::ruta_entrada()`/`fijar_rutas` son un `static` global del
        // proceso: sin este mutex, dos tests de este archivo que lo mutaran a
        // la vez (cargo corre los tests en paralelo por defecto) podrían leer
        // la carpeta de entrada del OTRO test a mitad de camino.
        let _guard = rutas_mutex().lock().unwrap_or_else(|e| e.into_inner());
        let original_entrada = app_shell::ruta_entrada();
        app_shell::fijar_rutas(Some(entrada), None);

        con_guion(
            vec![
                Respuesta::Elegir(0),               // archivo: datos.xlsx (el único)
                Respuesta::Elegir(1),               // columna: Descripcion
                Respuesta::Elegir(1),               // acción: Dividir en 2 hojas
                Respuesta::Texto("10".to_string()), // límite
            ],
            || dividir_por_caracteres(&salida).unwrap(),
        );

        app_shell::fijar_rutas(Some(original_entrada), None);

        let ruta = salida.join("dividido_datos.xlsx");
        let mut libro = commerce_core::abrir_libro(&ruta).unwrap();
        let dentro = commerce_core::leer_hoja_por_nombre(&mut libro, &ruta, "Hasta_10").unwrap();
        let fuera = commerce_core::leer_hoja_por_nombre(&mut libro, &ruta, "Mas_de_10").unwrap();
        // "corto" (5) y "z" (1) caen dentro del límite; los dos repeats (20 y
        // 15) quedan fuera — ninguna fila debe perderse en la partición.
        assert_eq!(dentro.height(), 2);
        assert_eq!(fuera.height(), 2);
    }

    #[test]
    fn dividir_por_caracteres_modo_reporte_no_crea_archivo_si_nadie_supera_el_limite() {
        use app_shell::testing::{con_guion, Respuesta};
        use rust_xlsxwriter::Workbook;

        let tmp = tempfile::tempdir().unwrap();
        let entrada = tmp.path().join("entrada");
        let salida = tmp.path().join("salida");
        std::fs::create_dir_all(&entrada).unwrap();
        std::fs::create_dir_all(&salida).unwrap();

        let archivo = entrada.join("datos.xlsx");
        {
            let mut wb = Workbook::new();
            let hoja = wb.add_worksheet();
            hoja.write(0, 0, "Sku").unwrap();
            hoja.write(0, 1, "Descripcion").unwrap();
            hoja.write(1, 0, "A1").unwrap();
            hoja.write(1, 1, "corto").unwrap();
            wb.save(&archivo).unwrap();
        }

        let _guard = rutas_mutex().lock().unwrap_or_else(|e| e.into_inner());
        let original_entrada = app_shell::ruta_entrada();
        app_shell::fijar_rutas(Some(entrada), None);

        con_guion(
            vec![
                Respuesta::Elegir(0),                 // archivo: datos.xlsx
                Respuesta::Elegir(1),                 // columna: Descripcion
                Respuesta::Elegir(2),                 // acción: Reporte aparte
                Respuesta::Texto("1000".to_string()), // límite altísimo: nadie lo supera
            ],
            || dividir_por_caracteres(&salida).unwrap(),
        );

        app_shell::fijar_rutas(Some(original_entrada), None);

        let ruta = salida.join("reporte_mas_de_1000_datos.xlsx");
        assert!(
            !ruta.exists(),
            "no debe crearse ningún archivo de reporte si ninguna fila supera el límite"
        );
    }
}
