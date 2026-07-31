# 0001 — Ruta real vs. ruta pedida en escritores

**Estado:** Aceptada e implementada (Ronda 9).

## Contexto

`EscritorXlsx`/`EscritorCsv` llaman a `ruta_unica()` al construirse: si la
ruta pedida ya existe (p. ej. un temporal `__tmp_buscarv.xlsx` de una
corrida anterior interrumpida), redirigen la escritura a `"(1).xlsx"` en
silencio. El único rastro de esa decisión es el campo `escritor.ruta`.

Nada en el sistema de tipos impedía que un caller usara la ruta que
*pidió* en vez de la ruta *real* del escritor al renombrar el resultado
sobre el archivo original — compilaba igual en ambos casos, y en el peor
caso renombraba basura vieja sobre los datos del usuario.

Ese bug se corrigió, por separado, en `buscarv.rs`, `duplicados.rs` y
`cruzar_y_escribir` (`bin/etl_tools.rs`) — la misma clase de bug, en tres
lugares, porque el compilador no podía distinguir "ruta pedida" de "ruta
real": ambas eran `PathBuf`.

## Decisión

Se introdujo `commerce_core::RutaEscritaReal`, un newtype sobre `PathBuf`
que envuelve específicamente la ruta REAL de un escritor ya cerrado. Las
funciones que antes devolvían `PathBuf`/`Option<PathBuf>` para este
propósito (`buscarv`, `escribir_reporte_y_limpio`, `cruzar_y_escribir`)
ahora devuelven `RutaEscritaReal`/`Option<RutaEscritaReal>`, y las
funciones que renombran sobre el archivo original (`renombrar_o_avisar`,
`cerrar_cruce`) piden `&RutaEscritaReal` en vez de `&Path`.

Un caller que intente pasar la ruta que pidió (un `PathBuf`/`&Path`
cualquiera) a estas funciones no compila — tiene que pasar por
`RutaEscritaReal::nueva(escritor.ruta.clone())` explícitamente, lo que hace
la confusión mucho más difícil de cometer sin darse cuenta.

## Consecuencias

- La clase de bug (no la instancia puntual) queda cerrada para los tres
  call sites migrados. Un caller nuevo que necesite este mismo patrón
  (escribir → renombrar sobre el original) hereda la protección por tipos
  si adopta `RutaEscritaReal` en su firma.
- No se migró el resto de callers de `EscritorXlsx`/`EscritorCsv` que leen
  `.ruta` solo para reportar (logs, mensajes de éxito) — ahí no hay riesgo
  de sobrescribir el archivo original, así que el costo de migrarlos no se
  justificaba.
- Se evaluó (y se descartó) cambiar el `Drop` de `EscritorXlsx`/`EscritorCsv`
  para que aborte por defecto en vez de finalizar: varios call sites de
  producción (p. ej. `procesar_por_palabra`, `ordenar_columna` en
  `bin/etl_tools.rs`) dependen HOY de que `Drop` finalice el archivo sin
  llamar a `.cerrar()` explícito. Cambiar ese default habría sido un cambio
  de comportamiento silencioso y de alto riesgo sin auditar antes cada uno
  de esos call sites — se dejó fuera de esta ronda.
