# 0005 — Limpieza de escritores ante error

**Estado:** Aceptada parcialmente; el guard RAII completo queda pendiente.

## Contexto

Antes de la Ronda 8/9, el patrón dominante ante un error a mitad de
escritura era `std::fs::remove_file(&ruta)` repetido en cada función que
abría un `EscritorXlsx`/`EscritorCsv` — cada call site "adivinando" qué
archivo limpiar. Cuando `ruta_unica()` había redirigido la escritura (ver
[0001](0001-ruta-real-vs-ruta-pedida.md)), ese `remove_file` apuntaba al
archivo equivocado.

## Decisión

Se reemplazó el patrón por `escritor.abortar()` (que ya conoce su propia
ruta real) en los call sites tocados en esta ronda (`buscarv.rs`,
`duplicados.rs`, `cruzar_y_escribir` en `bin/etl_tools.rs`,
`ejecutar_con_escritor` en `publications_builder`/`publications_validator`).

No se implementó el guard RAII completo (un tipo que aborte por `Drop` a
menos que se llame a un `cerrar()` explícito) porque, al auditar los call
sites existentes, se encontró que varias funciones de producción
(`procesar_por_palabra`, `ordenar_columna`, `dividir_por_caracteres` en
`bin/etl_tools.rs`) dependen HOY de que el `Drop` actual de `EscritorXlsx`
FINALICE el archivo (llame a `cerrar()`) en el camino feliz, sin una
llamada explícita. Invertir ese default habría roto esos call sites en
silencio: dejarían de producir su archivo de salida.

## Consecuencias

- El riesgo específico que motivó este hallazgo (renombrar basura vieja
  sobre datos del usuario) está cerrado por [0001](0001-ruta-real-vs-ruta-pedida.md),
  que es una protección más precisa que un guard RAII genérico.
- Sigue pendiente, como trabajo futuro real (no aceptado como deuda
  permanente): auditar los call sites que dependen del `Drop`-finaliza
  implícito, hacerlos explícitos (`escritor.cerrar()?` al final de su
  camino feliz), y RECIÉN ENTONCES invertir el default de `Drop` a abortar.
  Ese orden importa — invertirlo antes de la auditoría es exactamente el
  tipo de cambio de comportamiento silencioso que este documento existe
  para prevenir.
