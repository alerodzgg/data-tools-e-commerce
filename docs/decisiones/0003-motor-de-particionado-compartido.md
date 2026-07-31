# 0003 — Motor de particionado a disco compartido

**Estado:** Aceptada e implementada (Ronda 9).

## Contexto

`publications_builder::compatibilidad::AcumuladorProcesadas` y el método
privado `procesar_particionado` de
`publications_validator::procesador::Procesador` implementaban, cada uno
por su cuenta, el mismo mecanismo: acumular filas en RAM, particionarlas
por hash de una columna clave, volcar a `.ipc` cuando el buffer crece
demasiado, y releer cada partición completa al final para un dedup GLOBAL
(entre todos los bloques/hojas, no solo el sub-bloque en curso).

Ambas copias tenían el mismo bug (semilla de hash fija en vez de aleatoria
por corrida) y se corrigieron por separado, en la misma ronda de auditoría,
en dos archivos distintos. Cualquier cambio futuro al MECANISMO (tamaño de
buffer, formato de spool, comportamiento ante colisión) exigía tocar dos
lugares y confiar en que alguien recordara mantenerlos sincronizados.

## Decisión

Se extrajo el mecanismo (no la política) a
`commerce_core::AcumuladorParticionado`: acumula, particiona, spillea a
`.ipc` y relee cada partición completa, pero delega en un closure
(`finalizar(|bucket| ...)`) qué hacer con cada partición ya reunida — esa
es la parte que SÍ difiere legítimamente entre `publications_builder`
(dedup por menor 'Precio2', escribe a "Procesadas") y
`publications_validator` (dedup con seguimiento de `Stats`, agrupa por hoja
de origen).

## Consecuencias

- Un cambio al mecanismo compartido (p. ej. cambiar el formato de spool, o
  el criterio de cuándo hacer flush) se hace una sola vez, en
  `commerce_core::particionado`, y beneficia a ambos crates.
- La política de negocio (qué dedup, en qué hoja) sigue viviendo en cada
  crate, donde corresponde — no se forzó una abstracción que mezclara
  mecanismo con política.
- Ambos crates conservan su propia suite de tests e2e sobre su propia
  política; `commerce_core::particionado` tiene su propia suite enfocada
  en el mecanismo (agrupamiento entre llamadas separadas, flush automático
  por umbral, filas vacías).
