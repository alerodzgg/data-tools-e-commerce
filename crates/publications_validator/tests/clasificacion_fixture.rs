//! Compara `clasificar_uno` contra un fixture curado (~1500 títulos con su
//! salida y motivo esperados). Es la red que permite tocar la lógica de
//! clasificación sin cambiar resultados en silencio.

use std::collections::HashSet;

use publications_validator::{clasificar_uno, palabras_borradas, palabras_borradas_lower, Patrones};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fila {
    titulo: String,
    salida: String,
    motivo: Option<String>,
}

#[test]
fn clasificacion_coincide_con_el_fixture_de_referencia() {
    let datos = include_str!("fixture_clasificacion.json");
    let filas: Vec<Fila> = serde_json::from_str(datos).expect("fixture_clasificacion.json inválido");
    assert!(
        filas.len() > 1000,
        "la fixture debe traer un volumen representativo de títulos"
    );

    let patrones = Patrones::compilar();
    let marcas = palabras_borradas_lower(&palabras_borradas());

    let mut divergencias: Vec<String> = Vec::new();
    for fila in &filas {
        let (salida, motivo) = clasificar_uno(&fila.titulo, &patrones, &marcas);
        if salida != fila.salida || motivo != fila.motivo {
            divergencias.push(format!(
                "titulo={:?}\n  esperado: salida={:?} motivo={:?}\n  obtenido: salida={:?} motivo={:?}",
                fila.titulo, fila.salida, fila.motivo, salida, motivo
            ));
        }
    }
    assert!(
        divergencias.is_empty(),
        "{} divergencias:\n{}",
        divergencias.len(),
        divergencias.join("\n")
    );
}

#[test]
fn set_de_marcas_coincide_con_el_tamano_esperado() {
    // Guard contra un cambio accidental de la lista curada de marcas: si se
    // rompe la construcción (p. ej. deja de deduplicar), este número salta.
    let palabras = palabras_borradas();
    let unicas: HashSet<&String> = palabras.iter().collect();
    assert_eq!(unicas.len(), palabras.len());
    assert!(
        palabras.len() > 600,
        "la lista curada de marcas no debería encogerse de golpe"
    );
}
