# Decisiones de arquitectura (ADR)

Registro corto de las causas raíz identificadas en la auditoría "Code
Excellence Board" (Ronda 9) y de las decisiones tomadas para cada una. El
objetivo de este directorio es que la PRÓXIMA vez que aparezca un síntoma de
la misma clase de bug, quien lo investigue encuentre acá la causa raíz ya
diagnosticada y la decisión ya tomada — en vez de descubrirla de cero,
parchear el síntoma puntual, y dejar la misma causa lista para producir el
siguiente síntoma en otro archivo.

Cada entrada es corta a propósito: contexto, decisión, consecuencias. No es
un documento de diseño.

| # | Título | Estado |
|---|---|---|
| [0001](0001-ruta-real-vs-ruta-pedida.md) | Ruta real vs. ruta pedida en escritores | Aceptada |
| [0002](0002-estado-global-de-rutas.md) | Estado global de `app_shell::rutas` | Aceptada (deuda intencional) |
| [0003](0003-motor-de-particionado-compartido.md) | Motor de particionado a disco compartido | Aceptada |
| [0004](0004-separacion-de-compatibilidad-en-modulos.md) | Separación de `compatibilidad` en módulos | Aceptada |
| [0005](0005-limpieza-de-escritores-ante-error.md) | Limpieza de escritores ante error | Aceptada (parcial) |
| [0006](0006-tests-reactivos-vs-invariantes.md) | Tests reactivos vs. tests de invariante | En progreso |
