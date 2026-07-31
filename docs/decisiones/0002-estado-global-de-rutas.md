# 0002 — Estado global de `app_shell::rutas`

**Estado:** Aceptada como deuda intencional (no se migra por ahora).

## Contexto

`app_shell::rutas` mantiene las carpetas de entrada/salida en un
`static RUTAS: LazyLock<RwLock<Rutas>>` de proceso, mutado por
`fijar_rutas()` y leído por cualquier función vía `ruta_entrada()`/
`ruta_salida()`, sin pasarlo como parámetro explícito.

Esto es exactamente el tipo de estado compartido implícito que hace frágil
a un sistema: dos tests que mutan este global (vía `fijar_rutas`) pueden
pisarse si corren en paralelo — de hecho, `bin/etl_tools.rs` tuvo que
agregar un `Mutex` propio (`rutas_mutex()`) solo para serializar sus propios
tests que tocan este estado. Para un flujo agéntico (reintentos
automáticos, corridas concurrentes, orquestación externa), este tipo de
dependencia oculta es fuente de comportamiento no determinista entre
corridas que deberían ser idénticas.

## Decisión

Se mantiene el diseño actual. Es una excepción razonable, no un descuido:

- Las cinco herramientas son binarios de un solo proceso, lanzados por un
  humano desde un menú interactivo — no hay dos "sesiones" concurrentes
  dentro del mismo proceso en el uso real.
- El `RwLock` ya recupera correctamente un lock envenenado (ver el test
  `un_lock_envenenado_por_un_panico_previo_se_recupera_en_vez_de_propagar_el_panico`
  en `rutas.rs`), así que un panic previo no deja el proceso inutilizable.
- Migrar a un contexto explícito (pasar `entrada`/`salida` como parámetro
  por todos los binarios) es un refactor de alcance mayor — toca los 5
  binarios y probablemente decenas de firmas de función — sin un beneficio
  claro mientras el modelo de ejecución siga siendo "un proceso, un
  operador humano, un menú a la vez".

## Consecuencias

- Este archivo documenta la excepción explícitamente: si en el futuro
  `data-tools-e-commerce` pasa a correr de forma no interactiva (orquestado
  por un agente, con corridas paralelas dentro del mismo proceso, o como
  librería embebida en otro binario), esta decisión debe revisarse primero
  — es la pieza que rompería ese escenario.
- El arnés de aislamiento de tests (el `Mutex` en `bin/etl_tools.rs`) debe
  mantenerse y replicarse en cualquier nuevo test que mute `fijar_rutas`
  directamente, hasta que (si alguna vez ocurre) se migre a contexto
  explícito.
