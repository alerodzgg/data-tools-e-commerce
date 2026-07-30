//! Binario interactivo de `etl_tools`: borrar por palabra, gestionar
//! duplicados, ordenar columnas, dividir por caracteres, BUSCARV
//! (exacto/parcial), Encontrar.
//!
//! "Borrar por color de fuente" queda fuera (mismo motivo que en el motor:
//! ningún lector XLSX en Rust expone hoy el color de fuente de una celda) —
//! la opción aparece en el menú pero avisa que no está disponible, en vez de
//! desaparecer en silencio.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use aho_corasick::AhoCorasick;
use app_shell::FlujoError;
use commerce_core::{columnas_union, total_filas, CoreError, EscritorXlsx};
use etl_tools::constantes::{COL_CARACTERES, COL_ENCONTRADA, FILAS_POR_HOJA, SOPORTADOS_ARCHIVOS};
use polars::prelude::*;

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Flujo(#[from] FlujoError),
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error(transparent)]
    Polars(#[from] polars::error::PolarsError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

type AppResult<T> = Result<T, AppError>;

/// Tamaño de bloque para las escrituras en streaming de este binario (evita
/// materializar el DataFrame completo de una sola vez). Antes repetido como
/// literal `200_000` en 4 sitios sin nombrar.
const FILAS_POR_BLOQUE_ESCRITURA: usize = 200_000;

// ════════════════════════════════════════════════════════════════════════
// Helpers compartidos
// ════════════════════════════════════════════════════════════════════════

fn listar_archivos_soportados(entrada: &Path) -> Vec<PathBuf> {
    let mut archivos: Vec<PathBuf> = std::fs::read_dir(entrada)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| SOPORTADOS_ARCHIVOS.contains(&e.to_lowercase().as_str()))
                    .unwrap_or(false)
        })
        .collect();
    archivos.sort();
    archivos
}

fn seleccionar_archivo(mensaje: &str) -> AppResult<Option<PathBuf>> {
    let entrada = app_shell::ruta_entrada();
    let archivos = listar_archivos_soportados(&entrada);
    if archivos.is_empty() {
        app_shell::error(&format!("No se encontraron archivos en '{}'", entrada.display()));
        return Ok(None);
    }
    Ok(app_shell::elegir_archivo(mensaje, archivos)?)
}

fn excluir_refs(excluir: &HashSet<String>) -> Vec<&str> {
    excluir.iter().map(String::as_str).collect()
}

/// Renombra `origen` a `destino` con un mensaje de error claro si falla, en
/// vez de propagar el `io::Error` genérico del SO. En Windows, sobrescribir
/// un `.xlsx` que el usuario tiene abierto en Excel es el caso típico de
/// fallo acá: sin este mensaje, el usuario ve un error críptico y no sabe
/// que sus datos ya procesados quedaron a salvo en `origen`, sin renombrar.
fn renombrar_o_avisar(origen: &Path, destino: &Path) -> AppResult<()> {
    std::fs::rename(origen, destino).map_err(|e| {
        std::io::Error::other(format!(
            "No se pudo sobrescribir '{}' (¿está abierto en Excel u otro programa? \
             ciérralo e intenta de nuevo): {e}. Los datos procesados quedaron en '{}'.",
            destino.display(),
            origen.display()
        ))
        .into()
    })
}

/// Gate Sí/No + checkbox sobre las hojas reales. `None` si se canceló o se
/// excluyó todo. Envuelve `app_shell::preguntar_hojas_excluir` para un solo
/// archivo (uso más común en este binario).
fn preguntar_hojas_excluir_de(archivo: &Path, etiqueta: &str) -> AppResult<Option<HashSet<String>>> {
    Ok(app_shell::preguntar_hojas_excluir(
        std::slice::from_ref(&archivo.to_path_buf()),
        etiqueta,
    )?)
}

/// A diferencia de `app_shell::abortar_si_reservadas` (chequeo exacto), acá
/// hace falta además el matching por prefijo de `_font_color_*` — por eso
/// esta versión propia, pero reusando el mismo mensaje vía
/// `avisar_si_hay_reservadas` en vez de duplicarlo a mano.
fn abortar_si_reservadas(columnas: &[String]) -> bool {
    let chocan = etl_tools::columnas_reservadas_presentes(columnas);
    app_shell::avisar_si_hay_reservadas(&chocan)
}

fn seleccionar_columnas(columnas: &[String], para: &str) -> AppResult<Vec<String>> {
    if columnas.is_empty() {
        app_shell::warn("No hay columnas disponibles.");
        return Ok(Vec::new());
    }
    Ok(app_shell::menu_multiple(
        &format!("Columnas para {para}:"),
        columnas.to_vec(),
    )?)
}

// ════════════════════════════════════════════════════════════════════════
// Modo — Borrar por palabra
// ════════════════════════════════════════════════════════════════════════

fn procesar_por_palabra(archivo: &Path, ruta_salida: &Path) -> AppResult<()> {
    let Some(excluir) = preguntar_hojas_excluir_de(archivo, "")? else {
        return Ok(());
    };
    let refs = excluir_refs(&excluir);

    let columnas = columnas_union(
        std::slice::from_ref(&archivo.to_path_buf()),
        Some(&refs),
        app_shell::warn,
    );
    if columnas.is_empty() {
        app_shell::error("No se pudieron leer columnas de los archivos.");
        return Ok(());
    }
    if abortar_si_reservadas(&columnas) {
        return Ok(());
    }

    let columnas_obj = seleccionar_columnas(&columnas, "borrado por palabra")?;
    if columnas_obj.is_empty() {
        app_shell::warn("Sin columnas. Operación cancelada.");
        return Ok(());
    }

    let entrada = app_shell::pedir_texto("Palabras a borrar (separadas por coma, ej: 'rojo,azul'):")?
        .unwrap_or_default();
    let palabras: Vec<String> = entrada
        .split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(regex::escape)
        .collect();
    if palabras.is_empty() {
        app_shell::warn("Sin palabras válidas. Operación cancelada.");
        return Ok(());
    }
    let patron = regex::RegexBuilder::new(&format!("(?:{})", palabras.join("|")))
        .case_insensitive(true)
        .build()
        .expect("patron valido: palabras escapadas");

    let stem = archivo.file_stem().unwrap_or_default().to_string_lossy();
    let total = total_filas(
        std::slice::from_ref(&archivo.to_path_buf()),
        Some(&refs),
        app_shell::warn,
    );
    let bloques = etl_tools::iter_hojas_valores(
        std::slice::from_ref(&archivo.to_path_buf()),
        Some(&refs),
        app_shell::warn,
    )?;

    let ruta_proc = commerce_core::ruta_unica(ruta_salida.join(format!("procesado_{stem}.xlsx")));
    let ruta_borr = commerce_core::ruta_unica(ruta_salida.join(format!("borradas_{stem}.xlsx")));
    let cols_borradas: Vec<String> = columnas
        .iter()
        .cloned()
        .chain(["Motivo_Borrado".to_string()])
        .collect();

    let mut escritor_validas = etl_tools::nuevo_escritor(&ruta_proc, columnas.clone())?;
    let mut escritor_borradas = etl_tools::nuevo_escritor(&ruta_borr, cols_borradas.clone())?;
    let barra = app_shell::barra_progreso(&format!("Procesando {stem}"), total.unwrap_or(0));

    for chunk in bloques {
        let filas_entrada = chunk.height() as u64;
        let chunk = etl_tools::preparar_chunk(&chunk, &columnas)?;
        let (validas, borradas) = etl_tools::procesar_columnas_con_desplazamiento(
            &chunk,
            &columnas_obj,
            |_col, valor| valor.map(|v| patron.is_match(v)).unwrap_or(false),
            "Todas las celdas seleccionadas contienen la palabra a eliminar",
            "Todas las celdas seleccionadas quedaron vacías tras el borrado",
        )?;
        if validas.height() > 0 {
            escritor_validas.escribir(&validas.select(&columnas)?, None)?;
        }
        if borradas.height() > 0 {
            escritor_borradas.escribir(&borradas.select(&cols_borradas)?, None)?;
        }
        barra.inc(filas_entrada);
    }
    barra.finish_and_clear();

    if escritor_borradas.total == 0 {
        let _ = std::fs::remove_file(&ruta_borr);
    }
    let (n_validas, n_borradas) = (escritor_validas.total as u64, escritor_borradas.total as u64);

    print!(
        "{}",
        etl_tools::generar_reporte_cambios(n_validas + n_borradas, n_borradas, n_validas)
    );
    app_shell::success(&format!(
        "Guardado: '{}' ({n_validas} filas).",
        ruta_proc.file_name().unwrap_or_default().to_string_lossy()
    ));
    if n_borradas > 0 {
        app_shell::success(&format!(
            "Guardado: '{}' ({n_borradas} filas).",
            ruta_borr.file_name().unwrap_or_default().to_string_lossy()
        ));
    } else {
        app_shell::info("No se borró ninguna fila en esta ejecución.");
    }
    Ok(())
}

// ════════════════════════════════════════════════════════════════════════
// Modo — Duplicados
// ════════════════════════════════════════════════════════════════════════

fn seleccionar_columna_clave(columnas: &[String]) -> AppResult<Option<String>> {
    if columnas.is_empty() {
        app_shell::warn("No hay columnas disponibles.");
        return Ok(None);
    }
    Ok(app_shell::menu_seleccionar(
        "Elige la columna clave:",
        columnas.to_vec(),
    )?)
}

fn buscar_duplicados(ruta_salida: &Path) -> AppResult<()> {
    app_shell::mostrar_subcabecera("BÚSQUEDA DE DUPLICADOS EN UN ARCHIVO");
    let Some(archivo) = seleccionar_archivo("Selecciona el archivo para buscar duplicados")? else {
        app_shell::warn("No se seleccionó archivo. Volviendo al menú.");
        return Ok(());
    };
    let Some(excluir) = preguntar_hojas_excluir_de(&archivo, "")? else {
        return Ok(());
    };
    let refs = excluir_refs(&excluir);

    let columnas = columnas_union(std::slice::from_ref(&archivo), Some(&refs), app_shell::warn);
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
    let Some(columna_clave) = seleccionar_columna_clave(&columnas)? else {
        return Ok(());
    };

    app_shell::info("Detectando claves repetidas...");
    let (claves_repetidas, num_duplicados) =
        etl_tools::claves_repetidas(&archivo, &columna_clave, Some(&refs), app_shell::warn)?;
    app_shell::success(&format!(
        "Análisis completado. {num_duplicados} filas con clave repetida."
    ));
    if num_duplicados == 0 {
        app_shell::info("No se encontraron duplicados internos en el archivo.");
        return Ok(());
    }

    let stem = archivo.file_stem().unwrap_or_default().to_string_lossy();
    let opciones = vec![
        format!(
            "Limpiar el archivo actual ('{}') (sobrescribir)",
            archivo.file_name().unwrap_or_default().to_string_lossy()
        ),
        "Crear un nuevo archivo con las filas limpias".to_string(),
        "Solo generar el reporte de duplicados (no limpiar)".to_string(),
    ];
    let eleccion =
        app_shell::menu_seleccionar_nav("¿Qué quieres hacer con la versión limpia?", opciones.clone())?
            .unwrap_or_else(|| opciones[2].clone());

    let ruta_dup = commerce_core::ruta_unica(ruta_salida.join(format!("duplicados_internos_{stem}.xlsx")));
    let ruta_limpia = if eleccion == opciones[0] {
        Some(archivo.with_file_name(format!("{stem}__tmp_limpio.xlsx")))
    } else if eleccion == opciones[1] {
        Some(commerce_core::ruta_unica(
            ruta_salida.join(format!("{stem}_sin_duplicados.xlsx")),
        ))
    } else {
        None
    };

    app_shell::info(if ruta_limpia.is_some() {
        "Escribiendo duplicados y versión limpia en una sola pasada..."
    } else {
        "Escribiendo reporte de duplicados..."
    });
    let barra = app_shell::barra_progreso("Escribiendo", 0);
    let resultado = etl_tools::escribir_reporte_y_limpio(
        &archivo,
        &ruta_dup,
        ruta_limpia.as_deref(),
        &columnas,
        &columna_clave,
        &claves_repetidas,
        Some(&refs),
        app_shell::warn,
        |n| barra.inc(n),
    );
    barra.finish_and_clear();
    let (n_dup, n_limpio) = match resultado {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&ruta_dup);
            if let Some(rl) = &ruta_limpia {
                let _ = std::fs::remove_file(rl);
            }
            return Err(e.into());
        }
    };

    app_shell::success(&format!(
        "Reporte de duplicados: '{}' ({n_dup} filas).",
        ruta_dup.file_name().unwrap_or_default().to_string_lossy()
    ));
    if eleccion == opciones[0] {
        if let Some(rl) = &ruta_limpia {
            renombrar_o_avisar(rl, &archivo)?;
        }
        app_shell::success(&format!(
            "Archivo original sobrescrito: '{}' ({n_limpio} filas).",
            archivo.file_name().unwrap_or_default().to_string_lossy()
        ));
    } else if eleccion == opciones[1] {
        if let Some(rl) = &ruta_limpia {
            app_shell::success(&format!(
                "Nuevo archivo limpio en '{}' ({n_limpio} filas).",
                rl.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    } else {
        app_shell::info("Solo se generó el reporte de duplicados (no se limpió nada).");
    }
    Ok(())
}

fn comparar_duplicados(ruta_salida: &Path) -> AppResult<()> {
    app_shell::mostrar_subcabecera("COMPARACIÓN DE DUPLICADOS ENTRE DOS ARCHIVOS");
    let Some(base) = seleccionar_archivo("Selecciona el archivo 'Base'")? else {
        return Ok(());
    };
    let Some(comp) = seleccionar_archivo("Selecciona el archivo de 'Comparación'")? else {
        return Ok(());
    };
    let Some(excluir_base) = preguntar_hojas_excluir_de(&base, "la Base")? else {
        return Ok(());
    };
    let Some(excluir_comp) = preguntar_hojas_excluir_de(&comp, "la Comparación")? else {
        return Ok(());
    };
    let refs_base = excluir_refs(&excluir_base);
    let refs_comp = excluir_refs(&excluir_comp);

    let columnas_base = columnas_union(std::slice::from_ref(&base), Some(&refs_base), app_shell::warn);
    if columnas_base.is_empty() {
        app_shell::error(&format!(
            "No se pudieron leer columnas de '{}'.",
            base.file_name().unwrap_or_default().to_string_lossy()
        ));
        return Ok(());
    }
    if abortar_si_reservadas(&columnas_base) {
        return Ok(());
    }
    let Some(columna_clave) = seleccionar_columna_clave(&columnas_base)? else {
        return Ok(());
    };

    let columnas_comp = columnas_union(std::slice::from_ref(&comp), Some(&refs_comp), app_shell::warn);
    if !columnas_comp.contains(&columna_clave) {
        app_shell::error("La columna clave no existe en el archivo de Comparación.");
        app_shell::info("Ambos archivos deben tener la columna clave con el mismo nombre.");
        return Ok(());
    }
    if abortar_si_reservadas(&columnas_comp) {
        return Ok(());
    }

    app_shell::info("Leyendo claves del archivo de comparación...");
    let claves_comp = etl_tools::claves_unicas(&comp, &columna_clave, Some(&refs_comp), app_shell::warn)?;

    let base_stem = base.file_stem().unwrap_or_default().to_string_lossy();
    let ruta_dup = commerce_core::ruta_unica(ruta_salida.join(format!("duplicados_{base_stem}.xlsx")));
    app_shell::info(&format!(
        "Detectando y escribiendo duplicados en '{}'...",
        ruta_dup.file_name().unwrap_or_default().to_string_lossy()
    ));
    let barra = app_shell::barra_progreso("Comparando", 0);
    let num_duplicados = etl_tools::escribir_filtrado(
        &base,
        &ruta_dup,
        &columnas_base,
        &columna_clave,
        &claves_comp,
        true,
        Some(&refs_base),
        app_shell::warn,
        |n| barra.inc(n),
    )?;
    barra.finish_and_clear();

    app_shell::success(&format!(
        "Análisis completado. Se encontraron {num_duplicados} duplicados en Base."
    ));
    if num_duplicados == 0 {
        app_shell::info("No se encontraron duplicados. El archivo base ya está limpio.");
        return Ok(());
    }

    let comp_stem = comp.file_stem().unwrap_or_default().to_string_lossy();
    let opciones = vec![
        format!(
            "Limpiar archivo Base ('{}')",
            base.file_name().unwrap_or_default().to_string_lossy()
        ),
        format!(
            "Limpiar archivo de Comparación ('{}')",
            comp.file_name().unwrap_or_default().to_string_lossy()
        ),
        "No limpiar ningún archivo".to_string(),
    ];
    let eleccion = app_shell::menu_seleccionar_nav("¿Qué quieres limpiar?", opciones.clone())?
        .unwrap_or_else(|| opciones[2].clone());

    if eleccion == opciones[0] {
        let ruta_limpia =
            commerce_core::ruta_unica(ruta_salida.join(format!("base_sin_duplicados_{base_stem}.xlsx")));
        let barra = app_shell::barra_progreso("Limpiando", 0);
        let n = etl_tools::escribir_filtrado(
            &base,
            &ruta_limpia,
            &columnas_base,
            &columna_clave,
            &claves_comp,
            false,
            Some(&refs_base),
            app_shell::warn,
            |x| barra.inc(x),
        )?;
        barra.finish_and_clear();
        app_shell::success(&format!(
            "Guardado: '{}' ({n} filas).",
            ruta_limpia.file_name().unwrap_or_default().to_string_lossy()
        ));
    } else if eleccion == opciones[1] {
        app_shell::info("Leyendo claves del archivo Base...");
        let claves_base = etl_tools::claves_unicas(&base, &columna_clave, Some(&refs_base), app_shell::warn)?;
        let ruta_limpia = commerce_core::ruta_unica(
            ruta_salida.join(format!("comparacion_sin_duplicados_{comp_stem}.xlsx")),
        );
        let barra = app_shell::barra_progreso("Limpiando", 0);
        let n = etl_tools::escribir_filtrado(
            &comp,
            &ruta_limpia,
            &columnas_comp,
            &columna_clave,
            &claves_base,
            false,
            Some(&refs_comp),
            app_shell::warn,
            |x| barra.inc(x),
        )?;
        barra.finish_and_clear();
        app_shell::success(&format!(
            "Guardado: '{}' ({n} filas).",
            ruta_limpia.file_name().unwrap_or_default().to_string_lossy()
        ));
    } else {
        app_shell::info("Operación finalizada. No se ha limpiado ningún archivo.");
    }
    Ok(())
}

fn gestionar_duplicados(ruta_salida: &Path) -> AppResult<()> {
    let opciones = vec![
        "Buscar/quitar duplicados dentro de un archivo".to_string(),
        "Comparar duplicados entre dos archivos".to_string(),
    ];
    match app_shell::menu_seleccionar_nav("Gestión de duplicados:", opciones.clone())? {
        Some(v) if v == opciones[0] => buscar_duplicados(ruta_salida),
        Some(v) if v == opciones[1] => comparar_duplicados(ruta_salida),
        _ => {
            app_shell::warn("Operación cancelada.");
            Ok(())
        }
    }
}

// ════════════════════════════════════════════════════════════════════════
// Modo — Ordenar columnas
// ════════════════════════════════════════════════════════════════════════

fn cargar_completo(
    archivo: &Path,
    excluir: &HashSet<String>,
    columnas: &[String],
) -> AppResult<Option<DataFrame>> {
    let refs = excluir_refs(excluir);
    let bloques = etl_tools::iter_hojas_valores(
        std::slice::from_ref(&archivo.to_path_buf()),
        Some(&refs),
        app_shell::warn,
    )?;
    let mut partes = Vec::new();
    for chunk in bloques {
        let chunk = etl_tools::preparar_chunk(&chunk, columnas)?;
        partes.push(chunk.select(columnas)?);
    }
    if partes.is_empty() {
        return Ok(None);
    }
    let mut base = partes.remove(0);
    for parte in partes {
        base.vstack_mut_owned(parte)?;
    }
    Ok(Some(base))
}

fn escribir_por_bloques(
    escritor: &mut EscritorXlsx,
    df: &DataFrame,
    hoja: Option<&str>,
    barra: &indicatif::ProgressBar,
) -> AppResult<()> {
    let mut inicio = 0i64;
    while (inicio as usize) < df.height() {
        let bloque = df.slice(inicio, FILAS_POR_BLOQUE_ESCRITURA);
        escritor.escribir(&bloque, hoja)?;
        barra.inc(bloque.height() as u64);
        inicio += FILAS_POR_BLOQUE_ESCRITURA as i64;
    }
    Ok(())
}

fn ordenar_columna(ruta_salida: &Path) -> AppResult<()> {
    let Some(archivo) = seleccionar_archivo("Selecciona el archivo a ordenar")? else {
        app_shell::warn("No se seleccionó archivo. Volviendo al menú.");
        return Ok(());
    };
    let Some(hojas_excluir) = preguntar_hojas_excluir_de(&archivo, "")? else {
        return Ok(());
    };
    let columnas = columnas_union(
        std::slice::from_ref(&archivo),
        Some(&excluir_refs(&hojas_excluir)),
        app_shell::warn,
    );
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

    let Some(columna) = app_shell::menu_seleccionar_nav("¿Qué columna quieres ordenar?", columnas.clone())?
    else {
        app_shell::warn("Operación cancelada.");
        return Ok(());
    };
    let ascendente =
        app_shell::menu_confirmar("Sentido del orden: ¿ascendente (A→Z, 0→9)?", true)?.unwrap_or(true);

    app_shell::info(&format!("Leyendo y ordenando por '{columna}'..."));
    let Some(df) = cargar_completo(&archivo, &hojas_excluir, &columnas)? else {
        app_shell::warn("El archivo está vacío.");
        return Ok(());
    };
    let df = commerce_core::ordenar_excel_df(&df, &columna, ascendente)?;

    let stem = archivo.file_stem().unwrap_or_default().to_string_lossy();
    let ruta = commerce_core::ruta_unica(ruta_salida.join(format!("ordenado_{stem}.xlsx")));
    let mut escritor = etl_tools::nuevo_escritor(&ruta, columnas.clone())?;
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
        "ascendente (A→Z, 0→9)"
    } else {
        "descendente (Z→A, 9→0)"
    };
    app_shell::success(&format!(
        "Guardado: '{}' ({} filas, ordenado por '{columna}' {sentido}).",
        ruta.file_name().unwrap_or_default().to_string_lossy(),
        escritor.total
    ));
    Ok(())
}

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

fn dividir_por_caracteres(ruta_salida: &Path) -> AppResult<()> {
    let Some(archivo) = seleccionar_archivo("Selecciona el archivo a analizar")? else {
        app_shell::warn("No se seleccionó archivo. Volviendo al menú.");
        return Ok(());
    };
    let Some(hojas_excluir) = preguntar_hojas_excluir_de(&archivo, "")? else {
        return Ok(());
    };
    let columnas = columnas_union(
        std::slice::from_ref(&archivo),
        Some(&excluir_refs(&hojas_excluir)),
        app_shell::warn,
    );
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

    let opciones = vec![
        "Añadir columna con el conteo y ordenar todo el archivo".to_string(),
        "Dividir en 2 hojas por un límite: hasta N / más de N".to_string(),
        "Reporte aparte: solo las filas que superan un límite".to_string(),
    ];
    let Some(accion) =
        app_shell::menu_seleccionar_nav("¿Qué quieres hacer con el conteo?", opciones.clone())?
    else {
        app_shell::warn("Operación cancelada.");
        return Ok(());
    };

    let limite = if accion != opciones[0] {
        Some(preguntar_limite_caracteres()?)
    } else {
        None
    };

    if accion == opciones[0] && columnas.contains(&COL_CARACTERES.to_string()) {
        app_shell::error(&format!(
            "El archivo ya tiene una columna '{COL_CARACTERES}'; renómbrala y reintenta."
        ));
        return Ok(());
    }

    app_shell::info(&format!(
        "Leyendo '{}'...",
        archivo.file_name().unwrap_or_default().to_string_lossy()
    ));
    let Some(df) = cargar_completo(&archivo, &hojas_excluir, &columnas)? else {
        app_shell::warn("El archivo está vacío.");
        return Ok(());
    };
    let stem = archivo.file_stem().unwrap_or_default().to_string_lossy();

    if accion == opciones[0] {
        let opciones_orden = vec!["Menor a mayor".to_string(), "Mayor a menor".to_string()];
        let ascendente =
            app_shell::menu_seleccionar("Orden por número de caracteres:", opciones_orden.clone())?
                .as_deref()
                != Some("Mayor a menor");

        let df = etl_tools::anadir_columna_y_ordenar(&df, &columna, ascendente)?;
        let columnas_salida: Vec<String> = std::iter::once(COL_CARACTERES.to_string())
            .chain(columnas.iter().cloned())
            .collect();
        let df = df.select(&columnas_salida)?;
        let ruta = commerce_core::ruta_unica(ruta_salida.join(format!("conteo_{stem}.xlsx")));
        let mut escritor = etl_tools::nuevo_escritor(&ruta, columnas_salida.clone())?;
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

    let limite = limite.expect("hojas/reporte siempre piden limite");
    let (dentro, fuera) = etl_tools::dividir_dentro_fuera(&df, &columna, limite)?;

    if accion == opciones[1] {
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
            df.height() as u64,
        );
        let hoja_dentro = format!("Hasta_{limite}");
        let hoja_fuera = format!("Mas_de_{limite}");
        let dentro = dentro.select(&columnas)?;
        let fuera_sel = fuera.select(&columnas)?;
        for (bloque, hoja) in [(&dentro, hoja_dentro.as_str()), (&fuera_sel, hoja_fuera.as_str())] {
            escribir_por_bloques(&mut escritor, bloque, Some(hoja), &barra)?;
        }
        barra.finish_and_clear();
        app_shell::success(&format!(
            "Guardado: '{}' — {} fila(s) en 'Hasta_{limite}', {} fila(s) en 'Mas_de_{limite}'.",
            ruta.file_name().unwrap_or_default().to_string_lossy(),
            dentro.height(),
            fuera.height()
        ));
    } else {
        if fuera.height() == 0 {
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
            fuera.height() as u64,
        );
        let fuera_sel = fuera.select(&columnas)?;
        escribir_por_bloques(&mut escritor, &fuera_sel, None, &barra)?;
        barra.finish_and_clear();
        app_shell::success(&format!(
            "Reporte guardado: '{}' ({} de {} filas superan {limite} caracteres en '{columna}').",
            ruta.file_name().unwrap_or_default().to_string_lossy(),
            fuera.height(),
            df.height()
        ));
    }
    Ok(())
}

// ════════════════════════════════════════════════════════════════════════
// Diálogo y plomería compartidos por BUSCARV / BUSCARV parcial / Encontrar
// ════════════════════════════════════════════════════════════════════════

struct ParametrosCruce {
    base: PathBuf,
    tabla: PathBuf,
    excluir_base: HashSet<String>,
    excluir_tabla: HashSet<String>,
    cols_base: Vec<String>,
    clave_base: String,
    clave_tabla: String,
    columnas_traer: Vec<String>,
}

fn preparar_cruce(prompt_base: &str, prompt_tabla: &str) -> AppResult<Option<ParametrosCruce>> {
    let Some(base) = seleccionar_archivo("Selecciona el archivo BASE (el que se va a enriquecer)")? else {
        return Ok(None);
    };
    let Some(tabla) = seleccionar_archivo("Selecciona el archivo TABLA (de donde se traen los datos)")?
    else {
        return Ok(None);
    };
    let Some(excluir_base) = preguntar_hojas_excluir_de(&base, "la Base")? else {
        return Ok(None);
    };
    let Some(excluir_tabla) = preguntar_hojas_excluir_de(&tabla, "la Tabla")? else {
        return Ok(None);
    };

    let cols_base = columnas_union(
        std::slice::from_ref(&base),
        Some(&excluir_refs(&excluir_base)),
        app_shell::warn,
    );
    if cols_base.is_empty() {
        app_shell::error(&format!(
            "No se pudieron leer columnas de '{}'.",
            base.file_name().unwrap_or_default().to_string_lossy()
        ));
        return Ok(None);
    }
    let cols_tabla = columnas_union(
        std::slice::from_ref(&tabla),
        Some(&excluir_refs(&excluir_tabla)),
        app_shell::warn,
    );
    if cols_tabla.is_empty() {
        app_shell::error(&format!(
            "No se pudieron leer columnas de '{}'.",
            tabla.file_name().unwrap_or_default().to_string_lossy()
        ));
        return Ok(None);
    }
    if abortar_si_reservadas(&cols_base) || abortar_si_reservadas(&cols_tabla) {
        return Ok(None);
    }

    let Some(clave_base) = app_shell::menu_seleccionar_nav(prompt_base, cols_base.clone())? else {
        return Ok(None);
    };
    let Some(clave_tabla) = app_shell::menu_seleccionar_nav(prompt_tabla, cols_tabla.clone())? else {
        return Ok(None);
    };

    let candidatas: Vec<String> = cols_tabla
        .iter()
        .filter(|c| **c != clave_tabla)
        .cloned()
        .collect();
    if candidatas.is_empty() {
        app_shell::warn("La Tabla no tiene otras columnas para traer.");
        return Ok(None);
    }
    let columnas_traer = seleccionar_columnas(&candidatas, "TRAER de la Tabla")?;
    if columnas_traer.is_empty() {
        app_shell::warn("Sin columnas a traer. Operación cancelada.");
        return Ok(None);
    }

    Ok(Some(ParametrosCruce {
        base,
        tabla,
        excluir_base,
        excluir_tabla,
        cols_base,
        clave_base,
        clave_tabla,
        columnas_traer,
    }))
}

/// Pregunta qué hacer con las coincidencias y devuelve directamente el
/// ÍNDICE de la opción elegida (0=sobrescribir, 1=nuevo, 2=reporte) dentro de
/// su propio `opciones`, en vez de devolver el texto para que cada llamador
/// lo re-derive comparando contra un literal (`starts_with("Limpiar")` vs
/// `starts_with("Añadir")` según el `sobrescribir_como` de turno): eso
/// desincroniza en silencio si el texto por defecto cambia en un solo lugar.
fn preguntar_accion(base: &Path, sobrescribir_como: Option<&str>) -> AppResult<usize> {
    let texto_sobre = sobrescribir_como.map(str::to_string).unwrap_or_else(|| {
        format!(
            "Limpiar el archivo base ('{}') (sobrescribir)",
            base.file_name().unwrap_or_default().to_string_lossy()
        )
    });
    let opciones = vec![
        texto_sobre,
        "Crear un archivo nuevo con el resultado".to_string(),
        "Solo generar el reporte de coincidencias (solo las que cruzaron)".to_string(),
    ];
    let eleccion =
        app_shell::menu_seleccionar_nav("¿Qué quieres hacer con las coincidencias?", opciones.clone())?
            .unwrap_or_else(|| opciones[2].clone());
    Ok(opciones.iter().position(|o| *o == eleccion).unwrap_or(2))
}

/// Ruta de salida para BUSCARV según `indice_accion` (0=sobrescribir vía
/// temporal, 1=archivo nuevo, 2+=solo reporte). `prefijo` distingue exacto
/// ("") de parcial ("parcial_") en el nombre — antes el caso 0 (temporal) no
/// lo incorporaba como sí hacían los otros dos, así que BUSCARV exacto y
/// parcial corriendo sobre la MISMA base podían pisarse el mismo archivo
/// temporal (`{stem}__tmp_buscarv.xlsx` en ambos casos).
fn ruta_cruce(indice_accion: usize, base: &Path, ruta_salida: &Path, prefijo: &str) -> PathBuf {
    let stem = base.file_stem().unwrap_or_default().to_string_lossy();
    match indice_accion {
        0 => base.with_file_name(format!("{stem}__tmp_{prefijo}buscarv.xlsx")),
        1 => commerce_core::ruta_unica(ruta_salida.join(format!("buscarv_{prefijo}{stem}.xlsx"))),
        _ => commerce_core::ruta_unica(ruta_salida.join(format!("coincidencias_{prefijo}{stem}.xlsx"))),
    }
}

fn cerrar_cruce(
    indice_accion: usize,
    ruta: &Path,
    base: &Path,
    total_escrito: u64,
    filas_match: u64,
    filas_total: u64,
) -> AppResult<()> {
    let sin_match = filas_total.saturating_sub(filas_match);
    let resumen = format!("Con coincidencia: {filas_match} · sin coincidencia: {sin_match}.");
    match indice_accion {
        0 => {
            renombrar_o_avisar(ruta, base)?;
            app_shell::success(&format!(
                "Archivo base sobrescrito: '{}' ({total_escrito} filas). {resumen}",
                base.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        1 => app_shell::success(&format!(
            "Guardado: '{}' ({total_escrito} filas). {resumen}",
            ruta.file_name().unwrap_or_default().to_string_lossy()
        )),
        _ => app_shell::success(&format!(
            "Reporte de coincidencias: '{}' ({total_escrito} filas que cruzaron). {resumen}",
            ruta.file_name().unwrap_or_default().to_string_lossy()
        )),
    }
    Ok(())
}

/// Núcleo compartido de BUSCARV parcial y Encontrar: cruza `archivo` contra
/// el autómata Aho-Corasick por lotes acotados (`FILAS_MIN_LOTE_PARCIAL`/
/// `CELDAS_POR_LOTE_PARCIAL`), escribe el resultado y limpia la salida a
/// medias si algo falla. Antes vivía duplicado (~70 líneas casi idénticas)
/// entre `buscarv_parcial_modo` y `encontrar_modo`; lo único que cambia
/// entre los dos modos son estos parámetros. Devuelve `(filas_escritas,
/// filas_con_match)` para que el llamador arme su propio `cerrar_cruce`.
#[allow(clippy::too_many_arguments)]
fn cruzar_y_escribir(
    archivo: &Path,
    refs: &[&str],
    columnas_archivo: &[String],
    clave: &str,
    lookup: &DataFrame,
    ac: &AhoCorasick,
    claves_originales: &[String],
    salida_cols: &[String],
    opcion: etl_tools::OpcionMultiple,
    columnas_salida: &[String],
    solo_match: bool,
    ruta: &Path,
    total: u64,
    etiqueta_barra: &str,
) -> AppResult<(u64, u64)> {
    let filas_por_lote = FILAS_MIN_LOTE_PARCIAL.max(CELDAS_POR_LOTE_PARCIAL / columnas_archivo.len().max(1));
    let barra = app_shell::barra_progreso(etiqueta_barra, total);

    let procesar =
        |escritor: &mut EscritorXlsx, lote: &mut Vec<DataFrame>, filas_match: &mut u64| -> AppResult<()> {
            if lote.is_empty() {
                return Ok(());
            }
            let mut bloque = lote.remove(0);
            for parte in lote.drain(..) {
                bloque.vstack_mut_owned(parte)?;
            }
            let alto = bloque.height() as u64;
            let (mut out, n_match) = etl_tools::cruzar_chunk_parcial(
                &bloque,
                clave,
                lookup,
                ac,
                claves_originales,
                salida_cols,
                opcion,
            )?;
            *filas_match += n_match as u64;
            if solo_match {
                let mascara = out.column("_match")?.bool()?.clone();
                out = out.filter(&mascara)?;
            }
            if out.height() > 0 {
                escritor.escribir(&out.select(columnas_salida)?, None)?;
            }
            barra.inc(alto);
            Ok(())
        };

    let mut escritor = etl_tools::nuevo_escritor(ruta, columnas_salida.to_vec())?;
    let ejecucion = (|| -> AppResult<u64> {
        let mut lote: Vec<DataFrame> = Vec::new();
        let mut alto_lote = 0usize;
        let mut filas_match = 0u64;
        for chunk in etl_tools::iter_hojas_valores(
            std::slice::from_ref(&archivo.to_path_buf()),
            Some(refs),
            app_shell::warn,
        )? {
            let chunk = etl_tools::preparar_chunk_clave(&chunk, columnas_archivo, clave)?;
            alto_lote += chunk.height();
            lote.push(chunk);
            if alto_lote >= filas_por_lote {
                procesar(&mut escritor, &mut lote, &mut filas_match)?;
                alto_lote = 0;
            }
        }
        procesar(&mut escritor, &mut lote, &mut filas_match)?;
        Ok(filas_match)
    })();
    barra.finish_and_clear();

    let filas_match = match ejecucion {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(ruta);
            return Err(e);
        }
    };
    Ok((escritor.total as u64, filas_match))
}

// ════════════════════════════════════════════════════════════════════════
// Modo — BUSCARV
// ════════════════════════════════════════════════════════════════════════

fn buscarv_modo(ruta_salida: &Path) -> AppResult<()> {
    app_shell::mostrar_subcabecera("BUSCARV — traer columnas de otro archivo (VLOOKUP)");
    let Some(prep) = preparar_cruce(
        "Columna CLAVE en el archivo BASE (por dónde cruzar):",
        "Columna CLAVE en el archivo TABLA (la que se compara contra la Base):",
    )?
    else {
        return Ok(());
    };

    let opciones_multi = vec![
        "Solo la primera (como Excel BUSCARV)".to_string(),
        "Unir todas con salto de línea".to_string(),
    ];
    let unir_multiples = app_shell::menu_seleccionar(
        "Si una clave tiene VARIAS coincidencias en la Tabla:",
        opciones_multi.clone(),
    )?
    .as_deref()
        == Some(opciones_multi[1].as_str());

    let indice_accion = preguntar_accion(&prep.base, None)?;

    let (salida_traer, renombrar) = etl_tools::renombrar_traidas(&prep.cols_base, &prep.columnas_traer);

    app_shell::info("Cargando la tabla de búsqueda...");
    let mut lookup = etl_tools::cargar_tabla_busqueda(
        &prep.tabla,
        &prep.clave_tabla,
        &prep.columnas_traer,
        unir_multiples,
        Some(&excluir_refs(&prep.excluir_tabla)),
        app_shell::warn,
    )?
    .lazy()
    .rename(
        renombrar.keys().cloned().collect::<Vec<_>>(),
        renombrar.values().cloned().collect::<Vec<_>>(),
        true,
    )
    .collect()?;
    if lookup.height() == 0 {
        app_shell::warn("La Tabla no tiene claves usables. Nada que cruzar.");
        return Ok(());
    }
    app_shell::info(&format!("Tabla: {} claves distintas cargadas.", lookup.height()));
    let alto = lookup.height();
    lookup.with_column(Column::new("_hit".into(), vec![true; alto]))?;

    let solo_match = indice_accion == 2;
    let ruta = ruta_cruce(indice_accion, &prep.base, ruta_salida, "");
    let refs_base = excluir_refs(&prep.excluir_base);
    let total = total_filas(
        std::slice::from_ref(&prep.base),
        Some(&refs_base),
        app_shell::warn,
    )
    .unwrap_or(0);
    let barra = app_shell::barra_progreso(
        &format!(
            "Cruzando {}",
            prep.base.file_name().unwrap_or_default().to_string_lossy()
        ),
        total,
    );

    let resultado = etl_tools::buscarv(
        &prep.base,
        &prep.cols_base,
        &prep.clave_base,
        &lookup,
        &salida_traer,
        Some(&refs_base),
        solo_match,
        &ruta,
        app_shell::warn,
        |n| barra.inc(n),
    );
    barra.finish_and_clear();
    let (total_escrito, filas_match, filas_total) = match resultado {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&ruta);
            return Err(e.into());
        }
    };

    cerrar_cruce(
        indice_accion,
        &ruta,
        &prep.base,
        total_escrito as u64,
        filas_match,
        filas_total,
    )
}

// ════════════════════════════════════════════════════════════════════════
// Modo — BUSCARV parcial
// ════════════════════════════════════════════════════════════════════════

const FILAS_MIN_LOTE_PARCIAL: usize = 500_000;
const CELDAS_POR_LOTE_PARCIAL: usize = 20_000_000;

fn buscarv_parcial_modo(ruta_salida: &Path) -> AppResult<()> {
    app_shell::mostrar_subcabecera("BUSCARV PARCIAL — por contención de palabra completa");
    let Some(prep) = preparar_cruce(
        "Columna de la BASE donde buscar (el texto largo):",
        "Columna CLAVE de la TABLA (el término a buscar dentro de la Base):",
    )?
    else {
        return Ok(());
    };

    let opciones_opcion = vec![
        "Traer la más larga (más específica)".to_string(),
        "Traer la primera (orden de la Tabla)".to_string(),
        "Traer todas, unidas con salto de línea".to_string(),
    ];
    let Some(opcion_texto) = app_shell::menu_seleccionar_nav(
        "Si VARIAS claves de la Tabla aparecen en el mismo texto de la Base:",
        opciones_opcion.clone(),
    )?
    else {
        return Ok(());
    };
    let opcion = if opcion_texto == opciones_opcion[0] {
        etl_tools::OpcionMultiple::Larga
    } else if opcion_texto == opciones_opcion[1] {
        etl_tools::OpcionMultiple::Primera
    } else {
        etl_tools::OpcionMultiple::Todas
    };

    let opciones_multi = vec![
        "Solo la primera".to_string(),
        "Unir sus valores con salto de línea".to_string(),
    ];
    let unir_multiples = app_shell::menu_seleccionar(
        "Si una MISMA clave está repetida en la Tabla (varias filas):",
        opciones_multi.clone(),
    )?
    .as_deref()
        == Some(opciones_multi[1].as_str());

    let indice_accion = preguntar_accion(&prep.base, None)?;
    let (salida_traer, renombrar) = etl_tools::renombrar_traidas(&prep.cols_base, &prep.columnas_traer);

    app_shell::info("Cargando la tabla y construyendo el buscador (Aho-Corasick)...");
    let lookup = etl_tools::cargar_tabla_parcial(
        &prep.tabla,
        &prep.clave_tabla,
        &prep.columnas_traer,
        unir_multiples,
        Some(&excluir_refs(&prep.excluir_tabla)),
        app_shell::warn,
    )?
    .lazy()
    .rename(
        renombrar.keys().cloned().collect::<Vec<_>>(),
        renombrar.values().cloned().collect::<Vec<_>>(),
        true,
    )
    .collect()?;
    if lookup.height() == 0 {
        app_shell::warn("La Tabla no tiene claves usables. Nada que cruzar.");
        return Ok(());
    }
    let (ac, claves_originales) = etl_tools::construir_automata(&lookup)?;
    app_shell::info(&format!("Tabla: {} claves distintas.", lookup.height()));

    let solo_match = indice_accion == 2;
    let ruta = ruta_cruce(indice_accion, &prep.base, ruta_salida, "parcial_");
    let refs_base = excluir_refs(&prep.excluir_base);
    let total = total_filas(
        std::slice::from_ref(&prep.base),
        Some(&refs_base),
        app_shell::warn,
    )
    .unwrap_or(0);
    let columnas_salida: Vec<String> = salida_traer
        .iter()
        .cloned()
        .chain(prep.cols_base.iter().cloned())
        .collect();

    let (escritor_total, filas_match) = cruzar_y_escribir(
        &prep.base,
        &refs_base,
        &prep.cols_base,
        &prep.clave_base,
        &lookup,
        &ac,
        &claves_originales,
        &salida_traer,
        opcion,
        &columnas_salida,
        solo_match,
        &ruta,
        total,
        &format!(
            "Cruzando {}",
            prep.base.file_name().unwrap_or_default().to_string_lossy()
        ),
    )?;

    cerrar_cruce(
        indice_accion,
        &ruta,
        &prep.base,
        escritor_total,
        filas_match,
        total,
    )
}

// ════════════════════════════════════════════════════════════════════════
// Modo — Encontrar
// ════════════════════════════════════════════════════════════════════════

fn pedir_palabras_encontrar() -> AppResult<Option<Vec<String>>> {
    let opciones = vec![
        "Escribirlas aquí (separadas por coma)".to_string(),
        "Un XLSX con las palabras en su PRIMERA columna".to_string(),
    ];
    let Some(fuente) =
        app_shell::menu_seleccionar_nav("¿De dónde vienen las palabras a buscar?", opciones.clone())?
    else {
        return Ok(None);
    };

    if fuente == opciones[0] {
        let texto =
            app_shell::pedir_texto("Palabras a buscar (separadas por coma, ej: 'honda,depo,faro led'):")?
                .unwrap_or_default();
        let palabras: Vec<String> = texto
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
        return Ok(Some(palabras));
    }

    let Some(archivo) = seleccionar_archivo("Selecciona el XLSX con las palabras (1ª columna)")? else {
        return Ok(None);
    };
    let Some(excluir) = preguntar_hojas_excluir_de(&archivo, "las palabras")? else {
        return Ok(None);
    };
    let opciones_fila = vec![
        "Es una cabecera/título (ignorarla)".to_string(),
        "Ya es una palabra (incluirla en la búsqueda)".to_string(),
    ];
    let primera_es_palabra = app_shell::menu_seleccionar(
        &format!(
            "En '{}', ¿la PRIMERA fila es una cabecera o ya es una palabra?",
            archivo.file_name().unwrap_or_default().to_string_lossy()
        ),
        opciones_fila.clone(),
    )?
    .as_deref()
        == Some(opciones_fila[1].as_str());

    Ok(Some(etl_tools::palabras_de_xlsx(
        &archivo,
        Some(&excluir_refs(&excluir)),
        primera_es_palabra,
        app_shell::warn,
    )?))
}

fn encontrar_modo(ruta_salida: &Path) -> AppResult<()> {
    app_shell::mostrar_subcabecera("ENCONTRAR — marcar filas que contienen palabras");
    let Some(palabras) = pedir_palabras_encontrar()? else {
        return Ok(());
    };
    if palabras.is_empty() {
        app_shell::warn("No hay palabras usables. Operación cancelada.");
        return Ok(());
    }
    let lookup = etl_tools::lookup_palabras(&palabras)?;
    if lookup.height() == 0 {
        app_shell::warn("Ninguna palabra quedó usable tras normalizar (¿solo símbolos?).");
        return Ok(());
    }

    let Some(base) = seleccionar_archivo("Selecciona el archivo donde BUSCAR")? else {
        return Ok(());
    };
    let Some(excluir_base) = preguntar_hojas_excluir_de(&base, "la Base")? else {
        return Ok(());
    };
    let refs_base = excluir_refs(&excluir_base);
    let cols_base = columnas_union(std::slice::from_ref(&base), Some(&refs_base), app_shell::warn);
    if cols_base.is_empty() {
        app_shell::error(&format!(
            "No se pudieron leer columnas de '{}'.",
            base.file_name().unwrap_or_default().to_string_lossy()
        ));
        return Ok(());
    }
    if abortar_si_reservadas(&cols_base) {
        return Ok(());
    }
    if cols_base.contains(&COL_ENCONTRADA.to_string()) {
        app_shell::error(&format!(
            "El archivo ya tiene una columna '{COL_ENCONTRADA}'; renómbrala y reintenta."
        ));
        return Ok(());
    }

    let Some(clave_base) =
        app_shell::menu_seleccionar_nav("Columna donde buscar las palabras:", cols_base.clone())?
    else {
        return Ok(());
    };
    let opciones_opcion = vec![
        "Solo la primera (el orden de tu lista marca la prioridad)".to_string(),
        "Todas, unidas con salto de línea".to_string(),
    ];
    let Some(opcion_texto) = app_shell::menu_seleccionar_nav(
        "Si VARIAS palabras coinciden en la misma celda:",
        opciones_opcion.clone(),
    )?
    else {
        return Ok(());
    };
    let opcion = if opcion_texto == opciones_opcion[0] {
        etl_tools::OpcionMultiple::Primera
    } else {
        etl_tools::OpcionMultiple::Todas
    };

    let stem = base.file_stem().unwrap_or_default().to_string_lossy();
    let indice_accion = preguntar_accion(
        &base,
        Some(&format!(
            "Añadir la columna al archivo base ('{}') (sobrescribir)",
            base.file_name().unwrap_or_default().to_string_lossy()
        )),
    )?;

    app_shell::info(&format!(
        "Lista: {} palabras distintas. Construyendo el buscador (Aho-Corasick)...",
        lookup.height()
    ));
    let (ac, claves_originales) = etl_tools::construir_automata(&lookup)?;

    let columnas_salida: Vec<String> = std::iter::once(COL_ENCONTRADA.to_string())
        .chain(cols_base.iter().cloned())
        .collect();
    let solo_match = indice_accion == 2;
    let ruta = match indice_accion {
        0 => base.with_file_name(format!("{stem}__tmp_encontrar.xlsx")),
        1 => commerce_core::ruta_unica(ruta_salida.join(format!("encontrar_{stem}.xlsx"))),
        _ => commerce_core::ruta_unica(ruta_salida.join(format!("encontradas_{stem}.xlsx"))),
    };

    let total = total_filas(std::slice::from_ref(&base), Some(&refs_base), app_shell::warn).unwrap_or(0);
    let col_encontrada = vec![COL_ENCONTRADA.to_string()];

    let (escritor_total, filas_match) = cruzar_y_escribir(
        &base,
        &refs_base,
        &cols_base,
        &clave_base,
        &lookup,
        &ac,
        &claves_originales,
        &col_encontrada,
        opcion,
        &columnas_salida,
        solo_match,
        &ruta,
        total,
        &format!(
            "Buscando en {}",
            base.file_name().unwrap_or_default().to_string_lossy()
        ),
    )?;

    cerrar_cruce(indice_accion, &ruta, &base, escritor_total, filas_match, total)
}

// ════════════════════════════════════════════════════════════════════════
// main()
// ════════════════════════════════════════════════════════════════════════

fn ejecutar() -> AppResult<()> {
    app_shell::mostrar_cabecera("ETL TOOLS — BUSCARV, duplicados, colores, Encontrar");
    let ruta_salida = app_shell::ruta_salida();

    loop {
        let opciones = vec![
            "Borrar por palabra clave".to_string(),
            "Borrar por color de fuente (solo XLSX)".to_string(),
            "Gestionar duplicados (por columna clave)".to_string(),
            "Ordenar columnas (A→Z / 0→9, estilo Excel)".to_string(),
            "Contar caracteres de una columna (columna, hojas o reporte)".to_string(),
            "BUSCARV — traer columnas de otro archivo (VLOOKUP)".to_string(),
            "BUSCARV parcial — cruce por coincidencia parcial (palabra completa)".to_string(),
            "Encontrar — marcar filas que contienen palabras (terminal o XLSX)".to_string(),
        ];
        let modo = match app_shell::menu_seleccionar_nav("¿Qué quieres hacer?", opciones.clone()) {
            Ok(Some(m)) => m,
            Ok(None) => {
                app_shell::info("Hasta luego.");
                break;
            }
            Err(FlujoError::VolverAlMenu) => continue,
            Err(e) => return Err(e.into()),
        };

        let resultado = if modo == opciones[0] {
            match seleccionar_archivo("Selecciona el archivo a procesar:") {
                Ok(Some(archivo)) => procesar_por_palabra(&archivo, &ruta_salida),
                Ok(None) => {
                    app_shell::warn("No se seleccionó archivo. Volviendo al menú.");
                    Ok(())
                }
                Err(e) => Err(e),
            }
        } else if modo == opciones[1] {
            app_shell::warn("Borrar por color de fuente todavía no está disponible en este puerto: ningún lector XLSX en Rust expone hoy el color de fuente de una celda.");
            Ok(())
        } else if modo == opciones[2] {
            gestionar_duplicados(&ruta_salida)
        } else if modo == opciones[3] {
            ordenar_columna(&ruta_salida)
        } else if modo == opciones[4] {
            dividir_por_caracteres(&ruta_salida)
        } else if modo == opciones[5] {
            buscarv_modo(&ruta_salida)
        } else if modo == opciones[6] {
            buscarv_parcial_modo(&ruta_salida)
        } else {
            encontrar_modo(&ruta_salida)
        };

        if let Err(e) = resultado {
            match e {
                AppError::Flujo(FlujoError::VolverAlMenu) => continue,
                e => app_shell::error(&format!("Error: {e}")),
            }
        }
    }
    Ok(())
}

fn main() {
    if let Err(e) = ejecutar() {
        match e {
            AppError::Flujo(FlujoError::VolverAlMenu) => {}
            e => app_shell::error(&format!("Error fatal: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruta_cruce_temporal_distingue_exacto_de_parcial_sobre_la_misma_base() {
        // Antes: el caso 0 (sobrescribir vía temporal) ignoraba `prefijo` —
        // BUSCARV exacto ("") y parcial ("parcial_") corriendo sobre la
        // MISMA base producían el mismo nombre de temporal
        // (`{stem}__tmp_buscarv.xlsx`), con riesgo real de pisarse entre sí.
        let base = Path::new("carpeta/datos.xlsx");
        let salida = Path::new("salida");
        let exacto = ruta_cruce(0, base, salida, "");
        let parcial = ruta_cruce(0, base, salida, "parcial_");
        assert_ne!(
            exacto, parcial,
            "exacto y parcial no deben compartir el mismo temporal"
        );
    }

    #[test]
    fn renombrar_o_avisar_mueve_el_archivo_en_el_camino_feliz() {
        let tmp = tempfile::tempdir().unwrap();
        let origen = tmp.path().join("origen.xlsx");
        let destino = tmp.path().join("destino.xlsx");
        std::fs::write(&origen, b"contenido").unwrap();

        renombrar_o_avisar(&origen, &destino).unwrap();

        assert!(!origen.exists());
        assert_eq!(std::fs::read(&destino).unwrap(), b"contenido");
    }

    #[test]
    fn renombrar_o_avisar_da_un_mensaje_claro_si_falla() {
        let tmp = tempfile::tempdir().unwrap();
        let origen = tmp.path().join("no_existe.xlsx");
        let destino = tmp.path().join("destino.xlsx");

        let err = renombrar_o_avisar(&origen, &destino).unwrap_err().to_string();

        assert!(
            err.contains(&destino.display().to_string()) && err.contains(&origen.display().to_string()),
            "el mensaje debe mencionar tanto el destino como dónde quedaron los datos: {err}"
        );
    }
}
