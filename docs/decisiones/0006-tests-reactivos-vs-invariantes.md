# 0006 — Tests reactivos vs. tests de invariante

**Estado:** Aceptada e implementada (Ronda 9).

## Contexto

Cada bug corregido en el historial reciente (lock envenenado, nombre de
carpeta padre ambiguo, esquema de URL en mayúsculas, colisión de temporal
rancio, panic de inferencia OCR) recibió exactamente un test dirigido a ese
escenario puntual. Es buena higiene de regresión — el escenario exacto que
ya rompió no puede volver a romperse en silencio — pero no generaliza a la
siguiente instancia ISOMORFA de la misma clase de bug en una función
hermana.

## Decisión

Para las clases de invariante que ya se repitieron más de una vez (la
colisión de ruta en escritores, el particionado por hash), se prioriza:

1. Cerrar la clase por TIPOS cuando es posible (ver
   [0001](0001-ruta-real-vs-ruta-pedida.md)) — más fuerte que cualquier test,
   porque el compilador lo verifica en cada build.
2. Property-based tests (`proptest`) para las invariantes que no se pueden
   cerrar por tipos sin un refactor mayor, en vez de solo el caso puntual ya
   encontrado.

## Consecuencias

- `proptest` se agregó como dependencia de desarrollo de `commerce_core`.
- `ruta_unica_nunca_devuelve_una_ruta_que_ya_existe` (en `rutas.rs`) cubre,
  para 0 a 11 archivos `"(N)"` previos generados al azar, que `ruta_unica`
  nunca devuelve una ruta ya ocupada.
- `ninguna_fila_se_pierde_sin_importar_particiones_ni_umbral_de_buffer` (en
  `particionado.rs`) cubre, para claves aleatorias repartidas en varias
  llamadas a `agregar` con `n_part` (1–7) y `umbral` (1–49) al azar, que el
  multiset de claves que `finalizar` entrega es exactamente el que entró.
- Próxima clase de invariante candidata a este mismo tratamiento, si vuelve
  a repetirse: el guard de tamaño/descompresión de `image_embedder.rs`
  (Fase 0 de esta ronda) — hoy solo tiene tests puntuales por límite
  (bytes/entradas), no una propiedad general.
