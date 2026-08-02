# DATA TOOLS E-COMMERCE

**[English](#english) · [Español](#español)**

Five terminal tools to prepare, clean and validate auto-parts listings
(eBay / Amazon) at a scale of **millions of rows**, written in **Rust** on top
of [Polars](https://pola.rs): native binaries, no runtime dependencies.

---

## English

### Overview

| Tool | Binary | What it does |
| --- | --- | --- |
| **Publications builder** | `publications_builder` | Builds listings from raw scraped data: prices, SKUs, features, compatibilities (eBay / Amazon). |
| **Publications validator** | `publications_validator` | Validates and cleans titles (7 rules + brand list), deduplicates by title keeping the best price. |
| **OCR tools** | `ocr_tools` | Filters product images by their content: banners, non-neutral backgrounds, diagrams, commercial text, logos and placeholders (CPU detectors + OCR via `ort`/ONNX Runtime). Also embeds images from a URL column into the spreadsheet. |
| **ETL tools** | `etl_tools` | VLOOKUP (exact and partial by whole word), duplicates, deletion by keyword, Excel-style sorting, character counting, flagging rows that contain given words. |
| **DATA combinator** | `data_combinator` | Merges several XLSX/CSV files into a single output, with global ordering (external on-disk merge for 10M+ rows) and splitting by sheets or files. |

All five launch from a single menu, `hub`. Everything is driven by **arrow-key
menus** — no arguments to memorise. XLSX files are written by emitting OOXML
XML directly (no third-party spreadsheet library) and **everything is treated
as text**: SKUs and codes like `007` are never turned into numbers.

### Requirements

Rust ≥ 1.80 (edition 2021). No other runtime dependency.

### Installation and use

```bash
cargo run --release -p hub          # menu with the five tools
cargo run --release -p etl_tools    # or a single tool directly
```

After building (see [Building](#building)), the compiled binary can be run
directly:

```bash
./target/release/hub
```

Tools **read from and write to your Downloads folder** (detected automatically
on Windows, macOS and Linux, including the localised name "Descargas"). This
can be changed from the menu itself (⚙ Cambiar rutas) or with environment
variables:

```bash
PUBLICACIONES_ENTRADA=/data/in PUBLICACIONES_SALIDA=/data/out cargo run --release -p hub
```

| Variable | Meaning |
| --- | --- |
| `PUBLICACIONES_ENTRADA` | Input folder (where files are read from). |
| `PUBLICACIONES_SALIDA` | Output folder (where results are written). |
| `PUBLICACIONES_DESCARGAS` | Base folder used as the default for both of the above. |

### Building

```bash
cargo build --release --workspace
```

Binaries land in `target/release/` (one per tool, plus `hub`). `ocr_tools`
additionally needs `models/` (detector/recogniser weights) and `runtime/`
(inference engine: `onnxruntime.dll` on Windows, `libonnxruntime.so` on
Linux) — it looks for both folders next to its own executable at **runtime**
(`ocr_tools::assets_dir()`), so the compiled binary can be moved or packaged
without being tied to the checkout.

`--release` is not optional in practice: without it, Polars and the OCR
pipeline run substantially slower.

### Installing on Ubuntu (`.deb` package)

```bash
cargo install cargo-deb
cargo build --release --workspace   # build the six binaries first
cargo deb -p hub --no-build         # package everything into target/debian/*.deb
sudo dpkg -i target/debian/data-tools-e-commerce_*.deb
```

Installs the six binaries into `/usr/lib/data-tools-e-commerce/` (along with
`models/`/`runtime/` for `ocr_tools`) and leaves a `data-tools-e-commerce`
command on the PATH (a symlink to `hub`, created by the package's `postinst`).
To uninstall: `sudo dpkg -r data-tools-e-commerce`.

### Architecture

Workspace of 8 crates under `crates/`:

```
commerce_core           ← shared engine (XLSX/CSV writers, readers, XML,
                          mojibake repair, on-disk partitioning). No UI.
app_shell               ← cross-platform paths + menus/styling + shared dialogs.
hub                     ← launcher for the five tools (single menu).
publications_builder    ← build listings (eBay / Amazon).
publications_validator  ← validate and clean titles.
ocr_tools               ← filter images by their content.
etl_tools               ← VLOOKUP, duplicates, character counts, Encontrar.
data_combinator         ← merge several files into one.
```

**Golden rule:** `commerce_core` knows nothing about the interface (it does not
print, it does not ask). `app_shell` is the only place where `println!`, menus
and progress bars are acceptable; engines report through `avisar`/`progreso`
callbacks and `Result`. Each tool is its own binary — an independent process
that `hub` spawns as a child, not a module linked into the same process — so
each can have its own runtime (e.g. `ocr_tools` uses `tokio`) without forcing a
shared one on the other four.

Interactive binaries are decomposed by mode. `etl_tools`, the largest, is split
into `comunes` (shared dialogs), `cruce` (plumbing shared by the three
cross-referencing modes) and one module per mode (`palabra`, `duplicados`,
`ordenar`, `caracteres`, `buscarv`, `encontrar`); `main.rs` holds only the menu
loop and the dispatch table.

**Business data lives outside the source code**, loaded with `include_str!`:

| File | Contents |
| --- | --- |
| `publications_validator/src/datos/palabras_base.txt` | Brand list (one per line). |
| `publications_validator/src/datos/palabras_prohibidas.txt` | Forbidden words. |
| `publications_builder/src/datos/rangos_precios.csv` | Price ranges → list price. |

Editing a brand or a price produces a diff of that data file, not a source
change. Lines starting with `#` are comments.

Architecture decisions are recorded as short ADRs in
[`docs/decisiones/`](docs/decisiones/).

### Error handling

The workspace never uses a panic as a control-flow mechanism for bad input.

- **Propagation.** `commerce_core::CoreResult` is the engine-wide result type;
  each binary defines its own `AppError` wrapping the layers it can fail in
  (flow control, engine, Polars, I/O) via `thiserror`, and propagates with `?`.
- **`unwrap`/`expect` policy.** All seven library crates carry
  `#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]`:
  the lint is active in production code and off in tests, where failing fast is
  the point. The few legitimate exceptions are annotated one by one with their
  justification. Literal regex patterns go through
  `commerce_core::regex_literal`, which centralises that single exception
  instead of repeating it at every call site.
- **Types that make mistakes unrepresentable.** `RutaEscritaReal` distinguishes
  *the path a writer was asked for* from *the path it actually wrote to* — a
  function that overwrites the original file demands the latter, so the
  compiler rejects passing the wrong one. Menu dispatch uses algebraic enums
  resolved by exhaustive `match`: adding a mode does not compile until every
  branch accounts for it.
- **Specific causes, not generic messages.** A failed image download reports
  which cause actually occurred (`FalloDescarga`: malformed URL, blocked host,
  timeout, network error, HTTP status, not an image, too large, truncated
  body). Likewise `MotivoSinProcesar` distinguishes a corrupt file from one
  with no URL columns.
- **Partial output is never left behind.** Writers expose `abortar()`, and a
  mid-run error removes the half-written file instead of leaving a corrupt
  XLSX in the output folder.
- **Untrusted input is bounded.** Image downloads run behind an SSRF filter
  (private/reserved IPs rejected, redirects re-validated), decoding is capped
  against decompression bombs, and `.xlsx` inputs are size-checked before being
  materialised in memory.

### Tests

```bash
cargo test --workspace                    # everything (350 tests)
cargo clippy --workspace --all-targets    # lints, no warnings
cargo fmt --all --check                   # formatting
```

The suite covers each tool's engine end to end: writers, readers, external
sorting, validation rules, the async OCR pipeline and E2E flows. Interactive
menus have a scripting harness (`app_shell::testing::con_guion`) to exercise
CLI flows without a real terminal. Property-based tests (`proptest`) cover the
invariants the type system cannot close on its own.

CI runs the three commands above on every push (`.github/workflows/ci.yml`).

---

## Español

### Resumen

Cinco herramientas de terminal para preparar, limpiar y validar publicaciones
de autopartes (eBay / Amazon) a escala de **millones de filas**, escritas en
**Rust** sobre [Polars](https://pola.rs): binarios nativos, sin dependencias
de runtime.

| Herramienta | Binario | Qué hace |
| --- | --- | --- |
| **Publications builder** | `publications_builder` | Construye las publicaciones desde el scrap crudo: precios, SKUs, características, compatibilidades (eBay / Amazon). |
| **Publications validator** | `publications_validator` | Valida y limpia los títulos (7 reglas + lista de marcas), deduplica por título conservando el mejor precio. |
| **OCR tools** | `ocr_tools` | Filtra imágenes de producto por su contenido: banners, fondos no neutros, diagramas, texto comercial, logos y placeholders (detectores CPU + OCR con `ort`/ONNX Runtime). También incrusta en el Excel las imágenes de una columna de URLs. |
| **ETL tools** | `etl_tools` | BUSCARV (exacto y parcial por palabra completa), duplicados, borrado por palabra, ordenar estilo Excel, contar caracteres, marcar filas que contienen palabras. |
| **DATA combinator** | `data_combinator` | Combina varios XLSX/CSV en una salida única, con orden global (mezcla externa en disco para 10M+ filas) y división por hojas o archivos. |

Las cinco se lanzan desde un menú único, `hub`. Todo se maneja con **menús de
flechas** en la terminal — sin argumentos que memorizar. Los XLSX se escriben
generando el XML OOXML directo (sin librería de terceros) y **todo se trata
como texto**: los SKUs y códigos como `007` nunca se convierten en números.

### Requisitos

Rust ≥ 1.80 (edition 2021). Ninguna otra dependencia de runtime.

### Instalación y uso

```bash
cargo run --release -p hub          # menú con las 5 herramientas
cargo run --release -p etl_tools    # o una herramienta directa
```

Tras compilar (ver [Compilar](#compilar)), también se puede correr el binario
ya construido directamente:

```bash
./target/release/hub
```

Las herramientas **leen y escriben en tu carpeta de Descargas** (detectada
automáticamente en Windows, macOS y Linux, incluido el nombre localizado
"Descargas"). Se puede cambiar desde el propio menú (⚙ Cambiar rutas) o con
variables de entorno:

```bash
PUBLICACIONES_ENTRADA=/ruta/datos PUBLICACIONES_SALIDA=/ruta/salidas cargo run --release -p hub
```

| Variable | Significado |
| --- | --- |
| `PUBLICACIONES_ENTRADA` | Carpeta de entrada (de dónde se leen los archivos). |
| `PUBLICACIONES_SALIDA` | Carpeta de salida (dónde se guardan los resultados). |
| `PUBLICACIONES_DESCARGAS` | Carpeta base que sirve de valor por defecto para las dos anteriores. |

### Compilar

```bash
cargo build --release --workspace
```

Los binarios quedan en `target/release/` (uno por herramienta, más `hub`).
`ocr_tools` necesita además `models/` (pesos del detector/reconocedor) y
`runtime/` (motor de inferencia: `onnxruntime.dll` en Windows,
`libonnxruntime.so` en Linux) — busca ambas carpetas junto a su propio
ejecutable en tiempo de **EJECUCIÓN** (`ocr_tools::assets_dir()`), así que el
binario compilado se puede mover o empaquetar sin quedar atado al checkout.

`--release` no es opcional en la práctica: sin él, Polars y el pipeline de OCR
corren bastante más lento.

### Instalar en Ubuntu (paquete `.deb`)

```bash
cargo install cargo-deb
cargo build --release --workspace   # compila los 6 binarios primero
cargo deb -p hub --no-build         # empaqueta todo junto en target/debian/*.deb
sudo dpkg -i target/debian/data-tools-e-commerce_*.deb
```

Instala los 6 binarios en `/usr/lib/data-tools-e-commerce/` (junto con
`models/`/`runtime/` de `ocr_tools`) y deja un comando `data-tools-e-commerce`
en el PATH (símlink a `hub`, creado por el `postinst` del paquete). Para
desinstalar: `sudo dpkg -r data-tools-e-commerce`.

### Arquitectura

Workspace de 8 crates bajo `crates/`:

```
commerce_core           ← motor compartido (escritores XLSX/CSV, lectores, XML,
                          mojibake, particionado a disco). Sin UI.
app_shell               ← rutas multiplataforma + menús/estilo + diálogos comunes.
hub                     ← lanzador de las 5 herramientas (menú único).
publications_builder    ← construir publicaciones (eBay / Amazon).
publications_validator  ← validar y limpiar títulos.
ocr_tools               ← filtrar imágenes por su contenido.
etl_tools               ← BUSCARV, duplicados, caracteres, Encontrar.
data_combinator         ← combinar varios archivos en uno.
```

**Regla de oro:** `commerce_core` no sabe nada de la interfaz (no imprime, no
pregunta). `app_shell` es el único lugar donde `println!`, menús y barras de
progreso son aceptables; los motores avisan por callbacks `avisar`/`progreso`
y por `Result`. Cada herramienta es su propio binario — un proceso
independiente que `hub` lanza como hijo, no un módulo importado en el mismo
proceso — así cada una puede tener su propio runtime (p. ej. `ocr_tools` usa
`tokio`) sin forzar uno compartido entre las otras cuatro.

Los binarios interactivos están descompuestos por modo. `etl_tools`, el mayor,
se divide en `comunes` (diálogos compartidos), `cruce` (plomería común a los
tres modos de cruce) y un módulo por modo (`palabra`, `duplicados`, `ordenar`,
`caracteres`, `buscarv`, `encontrar`); `main.rs` conserva solo el bucle de
menú y la tabla de despacho.

**Los datos de negocio viven fuera del código fuente**, cargados con
`include_str!`:

| Archivo | Contenido |
| --- | --- |
| `publications_validator/src/datos/palabras_base.txt` | Lista de marcas (una por línea). |
| `publications_validator/src/datos/palabras_prohibidas.txt` | Palabras prohibidas. |
| `publications_builder/src/datos/rangos_precios.csv` | Rangos de precio → precio de lista. |

Editar una marca o un precio produce un diff de ese archivo de datos, no un
cambio de código. Las líneas que empiezan con `#` son comentarios.

Las decisiones de arquitectura quedan registradas como ADRs breves en
[`docs/decisiones/`](docs/decisiones/).

### Manejo de errores

El workspace nunca usa un panic como mecanismo de control ante entrada
inválida.

- **Propagación.** `commerce_core::CoreResult` es el tipo de resultado del
  motor; cada binario define su propio `AppError` que envuelve las capas en
  las que puede fallar (flujo, motor, Polars, E/S) vía `thiserror`, y propaga
  con `?`.
- **Política de `unwrap`/`expect`.** Los siete crates de librería llevan
  `#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]`:
  el lint está activo en producción y apagado en tests, donde fallar rápido es
  justamente lo que se busca. Las pocas excepciones legítimas están anotadas
  una por una con su justificación. Los patrones regex literales pasan por
  `commerce_core::regex_literal`, que centraliza esa única excepción en vez de
  repetirla en cada sitio.
- **Tipos que hacen inexpresable el error.** `RutaEscritaReal` distingue *la
  ruta que se le pidió a un escritor* de *la ruta donde realmente escribió* —
  una función que sobrescribe el archivo original exige la segunda, así que el
  compilador rechaza pasarle la equivocada. El despacho de menús usa enums
  algebraicos resueltos con `match` exhaustivo: agregar un modo no compila
  hasta contemplarlo en cada rama.
- **Causas concretas, no mensajes genéricos.** Una descarga de imagen fallida
  reporta la causa real (`FalloDescarga`: URL mal formada, host bloqueado,
  timeout, error de red, status HTTP, no es una imagen, demasiado grande,
  descarga cortada). Igual `MotivoSinProcesar` distingue un archivo corrupto
  de uno sin columnas de URL.
- **Nunca queda una salida a medias.** Los escritores exponen `abortar()`, y un
  error a mitad de corrida borra el archivo incompleto en vez de dejar un XLSX
  corrupto en la carpeta de salida.
- **La entrada no confiable está acotada.** Las descargas de imágenes pasan por
  un filtro anti-SSRF (IPs privadas/reservadas rechazadas, redirects
  revalidados), la decodificación tiene tope contra bombas de descompresión, y
  los `.xlsx` de entrada se verifican por tamaño antes de materializarse en
  memoria.

### Pruebas

```bash
cargo test --workspace                    # todo (350 tests)
cargo clippy --workspace --all-targets    # lints, sin warnings
cargo fmt --all --check                   # formato
```

La suite cubre el motor completo de cada herramienta: escritores, lectores,
orden externo, reglas de validación, pipeline async de OCR y flujos E2E. Los
menús interactivos cuentan con un arnés de scripting
(`app_shell::testing::con_guion`) para testear flujos de CLI sin terminal
real. Los tests basados en propiedades (`proptest`) cubren las invariantes que
el sistema de tipos no puede cerrar por sí solo.

CI corre los tres comandos de arriba en cada push
(`.github/workflows/ci.yml`).
