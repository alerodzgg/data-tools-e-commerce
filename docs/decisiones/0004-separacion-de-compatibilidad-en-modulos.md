# 0004 — Separación de `compatibilidad` en módulos

**Estado:** Aceptada e implementada (Ronda 9).

## Contexto

`publications_builder::compatibilidad` (antes un único archivo de ~1550
líneas, ~750 de código de producción) mezclaba tres capas distintas:
parsing/normalización de `DataFrame` (precio, hoja completa, SKU
secuencial), reglas de negocio sobre qué filas se descartan (exceso de
caracteres, 'ERROR', 2+ 'NA', explode de 'Coincidencia'), y la orquestación
de E/S (join invertido en streaming, motor de particionado, escritor).
Tocar el archivo para un cambio de regla de negocio dejaba a un typo de
distancia el motor de particionado, y viceversa.

## Decisión

Se dividió en tres módulos, cada uno con su propia suite de tests:

- `compatibilidad::parsing` — funciones puras de transformación de
  `DataFrame` (`limpiar_precio_hoja1`, `preprocesar_hoja1`,
  `limpiar_hoja_compat`, `columnas_a_combinar`, `aplicar_sku_secuencial`,
  `procesar_dataframe_compatibilidad`). Sin E/S, sin `EscritorXlsx`.
- `compatibilidad::filtros` — reglas de negocio sobre qué sobrevive
  (`aplicar_filtros_combinada`, `explotar_coincidencia`,
  `escribir_bucket_procesadas`, el dedup por menor 'Precio2').
- `compatibilidad` (el módulo raíz) — orquestación: el join invertido en
  streaming, `ContextoUnido`/`ContextoIterCompat`, y
  `ejecutar_procesamiento_compatibilidad`, que compone `parsing` + `filtros`
  + `commerce_core::AcumuladorParticionado` + `EscritorXlsx`.

## Consecuencias

- El radio de impacto de un cambio ahora es visible por la ruta del
  archivo: una regla de negocio nueva se toca en `filtros.rs` sin rozar el
  motor de particionado; una columna nueva a normalizar se toca en
  `parsing.rs` sin rozar las reglas de descarte.
- Los tests migraron junto con su función: los de parsing puro (5) a
  `parsing.rs`, los de reglas de filtrado (2) a `filtros.rs`, y los e2e/
  orquestación (10) se quedaron en el módulo raíz, donde tiene sentido que
  vivan.
- `ModoCompatibilidad` sigue siendo público desde el módulo raíz
  (re-exportado), porque tanto `parsing` como `filtros` lo necesitan en sus
  firmas y es, correctamente, un concepto transversal a las tres capas.
