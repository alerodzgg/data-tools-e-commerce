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

**Both folders are copied there automatically** by `ocr_tools`'s build script,
which skips files already present at the same size — so a rebuild costs
nothing. Everything needed to run lives in `target/<profile>/`: copy that
directory anywhere and the tools work, with no Rust toolchain installed.

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

### Massive-scale image analysis on AWS (millions of images)

The OCR analysis mode is bound by ONNX inference. On a 12-core laptop it
measures **3.59 s per image** (720 px, 8 engines). At that rate 8 million
images would take over a year, so a large batch needs GPU instances.

#### Measured baseline

Every figure below comes from 58 real eBay images on a 12-core laptop:

| Change | Gain | Notes |
|---|---|---|
| `ocr_max_dim` 900 -> 720 | 1.60x | 58/58 verdicts unchanged |
| OCR engine pool (1 -> 8) | 1.82x | saturates: ONNX already uses every core |

Do **not** lower `ocr_max_dim` further without measuring. At 640 three of 58
verdicts change, and at 512 the run gets *slower* (19.9 s/image): the detector
finds more text boxes and the recogniser -- the second network -- ends up
doing more work than the detector saved.

#### Instance choice

| Instances | Type | Purchase | Days for 8M | Cost |
|---|---|---|---|---|
| 4 | `g4dn.2xlarge` (T4, 8 vCPU) | **spot** | ~8.3 | ~$210 |

Use **spot instances**. They cost roughly 65% less, and an interruption is
recoverable: the run resumes from its checkpoint (see below). Without that
checkpoint, spot would be a bad trade for a multi-day job.

Four medium instances beat one large one for two reasons: throughput scales
linearly instead of hitting the saturation curve above, and each instance
downloads from its own IP -- which matters, because image CDNs throttle
sustained bursts from a single address.

The GPU is not optional. Without it the same work takes about 42 days.

#### Preparing an instance

1. Launch `g4dn.2xlarge` as **spot**, with a **Deep Learning Base AMI** (it
   ships the NVIDIA drivers). A plain Linux server, **no desktop**: these
   tools are terminal-only and a GUI just wastes RAM.

2. Replace `runtime/` with the **GPU build** of ONNX Runtime. The bundled
   library is the CPU build; without `libonnxruntime_providers_cuda.so` the
   CUDA provider never registers and the run silently falls back to CPU.

   `ort 2.0.0-rc.10` requires ONNX Runtime **1.22.x** -- no other version
   loads.

   ```bash
   wget https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-linux-x64-gpu-1.22.0.tgz
   tar xzf onnxruntime-linux-x64-gpu-1.22.0.tgz
   cp onnxruntime-linux-x64-gpu-1.22.0/lib/*.so* target/release/runtime/
   ```

3. Build with `cargo build --release --workspace`.

4. Split the input across instances -- one shard of about 2M rows each.

#### Running over SSH

These are terminal programs; no display server is involved.

```bash
ssh -i key.pem ubuntu@<instance-ip>
tmux new -s ocr                    # survives a dropped connection
cd ~/data-tools-e-commerce/target/release
OCR_TOOLS_MOTORES=4 ./hub
```

Pick **OCR tools -> analyse**, choose the shard, then detach with `Ctrl+B`
followed by `D`. Reattach any time with `tmux attach -t ocr`.

Windows needs no extra software: PowerShell ships with `ssh`. If it rejects
the key as too permissive, run
`icacls .\key.pem /inheritance:r /grant:r "$($env:USERNAME):(R)"`.

#### Unattended mode (no prompts)

With any argument, `ocr_tools` skips the menu and asks nothing — required on a
server, where the interactive prompts would block forever.

```
ocr_tools --archivo <path>      file to analyse (repeatable)
          --columnas <a,b>      URL columns; omit to auto-detect
          --salida <dir>        output and checkpoint directory
          --rechazadas-solo     write only the rejected file
          --ayuda               help
```

Exit codes: `0` success, `1` run failed, `2` bad arguments or missing file.

#### Auto-restart after a spot interruption

```ini
[Service]
ExecStart=/home/ubuntu/ocr_tools --archivo /datos/bloque1.xlsx --salida /datos/salida
Restart=always
RestartSec=60
```

`systemd` relaunches on any non-zero exit. Progress is appended per batch to
`_checkpoint_<file>.jsonl` in the output directory, and re-running the same
file against the same directory skips what is already recorded — so an
interruption costs only the batch in flight, not the run.

Transient download failures are retried on a schedule (60 s, 5 min, 15 min,
30 min) before being recorded as rejected — a throttling window would otherwise
mark thousands of images as rejected without ever analysing them. Permanent
failures (404, bad URL) are not retried.

#### Verify the GPU is actually being used

On startup the analysis prints one line:

```
OCR engine: GPU (CUDA) (4 in parallel). Set OCR_TOOLS_MOTORES to change.
```

**If it says `CPU`, stop the run.** The CUDA provider is missing, and the job
would take about five times longer on hardware billed for its GPU.

### Architecture

Workspace of 8 crates under `crates/`:

```text
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

Engine modules are split along the same axis — by what has to be reasoned about
separately, not by file size:

| Module | Split into | Because each part is |
| --- | --- | --- |
| `ocr_tools::async_processor` | `batcher`, `veredicto` | a concurrency mechanism (testable with a stub closure, no ONNX), a decision policy (no I/O), and the orchestration that uses both. |
| `commerce_core::escritor_xlsx` | `spool`, `hoja`, `paquete` | a memory policy (RAM→disk), Excel's naming rules, the OOXML format, and the write state machine. |
| `bin/ocr_tools` | `dialogos`, `analizar`, `insertar` | what is asked of the user, and one module per mode; `main.rs` is menu loop and dispatch only. |

**Menu choice and resolved action are separate types.** What the user picks
carries no payload; the action that runs carries everything its branch needs,
inside the variant. A mode that filters by a threshold cannot be constructed
without that threshold, so no code downstream has to defend against its
absence.

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
- **`unwrap`/`expect` policy.** All thirteen crate roots — the seven libraries
  *and the six binaries* — carry
  `#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]`.
  In Rust every `src/bin/*` is its own crate root and does **not** inherit
  lints from `lib.rs`, so declaring it only in the libraries would leave it off
  in precisely the code the user runs, where a panic is a visible crash. The
  lint is active in production code and off in tests, where failing fast is
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
- **Existing files are never overwritten, and that lives in one place.** Both
  writers apply `ruta_unica` when constructed, and the real path is read back
  from the writer (`escritor.ruta()`) rather than assumed by the caller — so no
  caller has to remember to disambiguate, and none can do it inconsistently.
- **A combination that means nothing is rejected, not ignored.** Asking
  `data_combinator` to split into *sheets* while writing CSV used to succeed
  and quietly produce a single undivided file, because a CSV has no sheets. It
  is now an error from the library itself, not just an option the menu happens
  to hide.
- **The tools can be chained, and a test says so.** Each crate used to verify
  its writer against its own reader, so the contract *between* crates belonged
  to nobody — and three defects hid there at once: a missing `xl/styles.xml`
  (which `umya-spreadsheet` requires), missing `r` cell references (optional in
  ECMA-376, but any reader that indexes by reference gets a scrambled sheet),
  and `umya` retyping `inlineStr` cells as numbers, turning `007` into `7`.
  `ocr_tools/tests/cadena_entre_herramientas.rs` now covers that boundary.
- **Untrusted input is bounded — on the way out too.** Image downloads run
  behind an SSRF filter (private/reserved IPs rejected, redirects re-validated),
  decoding is capped against decompression bombs, and `.xlsx` inputs are
  size-checked before being materialised in memory. The same applies to what is
  *written*: a single cell can hold an arbitrarily long list of image URLs, so
  the number of columns inserted is capped at Excel's real limit (16 384) and
  the surplus is reported instead of producing a file Excel silently refuses to
  open.
- **Image downloads are deliberately slow.** Insertion runs 8 concurrent
  downloads, not 32. This is not a conservative guess: measured against
  `i.ebayimg.com`, 32 made all 29 images of a real file fail with timeouts, and
  the CDN kept punishing the following runs (15 ok, then 0, then 0). The server
  tolerates paced requests and tarpits bursts — it accepts the connection and
  never answers — so the limit here is its patience, not our bandwidth. Raising
  the number back makes the tool *appear* faster and deliver nothing. Retries
  back off exponentially for the same reason.

### Tests

```bash
cargo test --workspace                    # everything (381 tests)
cargo clippy --workspace --all-targets    # lints, no warnings
cargo fmt --all --check                   # formatting
```

The suite covers each tool's engine end to end: writers, readers, external
sorting, validation rules, the async OCR pipeline and E2E flows. Interactive
menus have a scripting harness (`app_shell::testing::con_guion`) to exercise
CLI flows without a real terminal. Property-based tests (`proptest`) cover the
invariants the type system cannot close on its own.

**The generated XLSX is checked against two independent parsers.** Unit tests
compare our output to our own helpers, which cannot catch XML that is
self-consistent but invalid to whoever reads it. `tests/xlsx_propiedades.rs`
feeds randomised hostile input (XML metacharacters, quotes, control bytes,
astral-plane unicode) through the writer and reads the result back with
`calamine` *and* `umya-spreadsheet` — two readers written by different people.
The second one matters beyond validation: it is what `ocr_tools` reads with,
so it also proves the five tools can be chained (the output of one is valid
input for the next). A single unescaped `&` does not dirty one cell — it makes Excel
refuse the whole file, so that is the property being held.

**Coverage is a threshold, not a number to look at.** CI runs `cargo llvm-cov`
over the libraries (binaries excluded — they are the interactive shell) and
fails below **88 % of lines**; the current figure is **92.91 %**. The threshold
goes up when ground is gained, never down to make a PR pass. The HTML report is
uploaded as an artifact on every run.

CI runs the commands above on every push (`.github/workflows/ci.yml`).

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

**Las dos carpetas se copian ahí solas**, vía el script de compilación de
`ocr_tools`, que saltea los archivos que ya están con el mismo tamaño — así
recompilar no cuesta nada. Todo lo necesario para ejecutar vive en
`target/<perfil>/`: copiás esa carpeta a donde quieras y las herramientas
funcionan, sin tener Rust instalado.

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

### Análisis de imágenes a escala masiva en AWS (millones de imágenes)

El modo de análisis está limitado por la inferencia ONNX. En una portátil de
12 núcleos mide **3,59 s por imagen** (720 px, 8 motores). A ese ritmo 8
millones de imágenes llevarían más de un año, así que una tanda grande
necesita instancias con GPU.

#### Punto de partida medido

Todas las cifras salen de 58 imágenes reales de eBay en una portátil de 12
núcleos:

| Cambio | Ganancia | Notas |
|---|---|---|
| `ocr_max_dim` 900 -> 720 | 1,60x | los 58 veredictos quedan idénticos |
| Pool de motores OCR (1 -> 8) | 1,82x | satura: ONNX ya usa todos los núcleos |

**No bajar más `ocr_max_dim` sin medir.** A 640 cambian 3 veredictos de 58, y
a 512 la corrida se vuelve *más lenta* (19,9 s por imagen): el detector
encuentra más cajas de texto y el reconocedor -- la segunda red -- termina con
más trabajo del que el detector ahorró.

#### Elección de instancia

| Instancias | Tipo | Compra | Días para 8M | Costo |
|---|---|---|---|---|
| 4 | `g4dn.2xlarge` (T4, 8 vCPU) | **spot** | ~8,3 | ~$210 |

Usar **instancias spot**. Cuestan alrededor de un 65% menos, y que AWS te
quite una no es un problema: la corrida se reanuda desde su checkpoint (ver
abajo). Sin ese checkpoint, spot sería un mal negocio para un trabajo de
varios días.

Cuatro instancias medianas le ganan a una grande por dos motivos: el
rendimiento escala de forma líneal en vez de chocar con la curva de saturación
de arriba, y cada instancia descarga desde su propia IP -- lo que importa,
porque los CDN de imágenes limitan las ráfagas sostenidas desde una sola
dirección.

La GPU no es opcional: sin ella el mismo trabajo tarda unos 42 días.

#### Preparar una instancia

1. Lanzar `g4dn.2xlarge` como **spot**, con una **Deep Learning Base AMI**
   (trae los drivers de NVIDIA). Linux de servidor pelado, **sin escritorio**:
   estas herramientas son de terminal y una GUI solo gasta RAM.

2. Reemplazar `runtime/` por la build **GPU** de ONNX Runtime. La biblioteca
   incluida es la de CPU; sin `libonnxruntime_providers_cuda.so` el proveedor
   CUDA no se registra y la corrida cae a CPU en silencio.

   `ort 2.0.0-rc.10` exige ONNX Runtime **1.22.x** -- ningúna otra versión
   carga.

   ```bash
   wget https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-linux-x64-gpu-1.22.0.tgz
   tar xzf onnxruntime-linux-x64-gpu-1.22.0.tgz
   cp onnxruntime-linux-x64-gpu-1.22.0/lib/*.so* target/release/runtime/
   ```

3. Compilar con `cargo build --release --workspace`.

4. Repartir la entrada entre las instancias -- un bloque de unas 2M filas cada
   una.

#### Ejecutar por SSH

Son programás de terminal; no interviene ningún servidor gráfico.

```bash
ssh -i clave.pem ubuntu@<ip-de-la-instancia>
tmux new -s ocr                    # sobrevive a que se corte la conexión
cd ~/data-tools-e-commerce/target/release
OCR_TOOLS_MOTORES=4 ./hub
```

Elegir **OCR tools -> analizar**, indicar el bloque, y desconectarse con
`Ctrl+B` y después `D`. Para volver a ver el progreso, `tmux attach -t ocr`.

En Windows no hace falta instalar nada: PowerShell ya trae `ssh`. Si rechaza
la clave por permisos demasiado abiertos, correr
`icacls .\clave.pem /inheritance:r /grant:r "$($env:USERNAME):(R)"`.

#### Modo desatendido (sin preguntas)

Con cualquier argumento, `ocr_tools` saltea el menú y no pregunta nada —
necesario en un servidor, donde los diálogos interactivos bloquearían para
siempre.

```
ocr_tools --archivo <ruta>      archivo a analizar (repetible)
          --columnas <a,b>      columnas de URL; sin esto se detectan solas
          --salida <carpeta>    carpeta de salida y checkpoint
          --rechazadas-solo     escribir solo el archivo de rechazadas
          --ayuda               ayuda
```

Códigos de salida: `0` éxito, `1` la corrida falló, `2` argumentos inválidos o
archivo inexistente.

#### Reinicio automático tras una interrupción de spot

```ini
[Service]
ExecStart=/home/ubuntu/ocr_tools --archivo /datos/bloque1.xlsx --salida /datos/salida
Restart=always
RestartSec=60
```

`systemd` relanza ante cualquier salida distinta de cero. El progreso se
agrega por lote a `_checkpoint_<archivo>.jsonl` en la carpeta de salida, y
volver a correr el mismo archivo contra la misma carpeta saltea lo ya
registrado — así que una interrupción cuesta solo el lote en vuelo, no la
corrida.

Los fallos transitorios de descarga se reintentan con un calendario (60 s,
5 min, 15 min, 30 min) antes de darse por rechazados — sin eso, una ventana de
throttling marcaría miles de imágenes como rechazadas sin haberlas analizado.
Los fallos permanentes (404, URL inválida) no se reintentan.

#### Verificar que la GPU se está usando de verdad

Al arrancar, el análisis imprime una línea:

```
Motor OCR: GPU (CUDA) (4 en paralelo). Ajustable con OCR_TOOLS_MOTORES.
```

**Si dice `CPU`, cortar la corrida.** Falta el proveedor CUDA, y el trabajo
tardaría unas cinco veces más en un hardware que se paga por su GPU.

### Arquitectura

Workspace de 8 crates bajo `crates/`:

```text
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

Los módulos de motor se cortan por el mismo eje: por lo que hay que razonar
aparte, no por tamaño de archivo.

| Módulo | Dividido en | Porque cada parte es |
| --- | --- | --- |
| `ocr_tools::async_processor` | `batcher`, `veredicto` | un mecanismo de concurrencia (se prueba con un cierre falso, sin ONNX), una política de decisión (sin I/O), y la orquestación que usa ambos. |
| `commerce_core::escritor_xlsx` | `spool`, `hoja`, `paquete` | una política de memoria (RAM→disco), las reglas de nombre de Excel, el formato OOXML, y la máquina de estados de escritura. |
| `bin/ocr_tools` | `dialogos`, `analizar`, `insertar` | lo que se le pregunta al usuario, y un módulo por modo; `main.rs` es solo bucle de menú y despacho. |

**La opción del menú y la acción resuelta son tipos distintos.** Lo que el
usuario elige no lleva datos; la acción que se ejecuta lleva adentro de la
variante todo lo que su rama necesita. Un modo que filtra por un umbral no se
puede construir sin ese umbral, así que ningún código posterior tiene que
defenderse de su ausencia.

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
- **Política de `unwrap`/`expect`.** Los trece crate roots —los siete de
  librería *y los seis binarios*— llevan
  `#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]`.
  En Rust cada `src/bin/*` es un crate root propio y **no** hereda los lints de
  `lib.rs`, así que declararlo solo en las librerías lo dejaría apagado justo
  en el código que corre el usuario, donde un panic es un crash visible. El
  lint está activo en producción y apagado en tests, donde fallar rápido es
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
- **Nunca se pisa un archivo existente, y eso vive en un solo lugar.** Los dos
  escritores aplican `ruta_unica` al construirse, y la ruta real se lee de
  vuelta del escritor (`escritor.ruta()`) en vez de asumirla el llamador: así
  ninguno tiene que acordarse de desambiguar, y ninguno puede hacerlo de forma
  distinta al resto.
- **Una combinación que no significa nada se rechaza, no se ignora.** Pedirle a
  `data_combinator` dividir en *hojas* escribiendo CSV antes funcionaba y
  producía en silencio un único archivo sin dividir, porque un CSV no tiene
  hojas. Ahora es un error de la biblioteca misma, no una opción que el menú
  simplemente oculta.
- **Las herramientas se pueden encadenar, y hay un test que lo dice.** Cada
  crate verificaba su escritor contra su propio lector, así que el contrato
  *entre* crates no era de nadie — y ahí se escondían tres defectos a la vez:
  faltaba `xl/styles.xml` (que `umya-spreadsheet` exige), faltaban las
  referencias `r` de las celdas (opcionales en ECMA-376, pero cualquier lector
  que indexe por referencia recibe una hoja desarmada), y `umya` retipaba las
  celdas `inlineStr` como números, convirtiendo `007` en `7`.
  `ocr_tools/tests/cadena_entre_herramientas.rs` cubre ahora esa frontera.
- **La entrada no confiable está acotada — también a la salida.** Las descargas
  de imágenes pasan por un filtro anti-SSRF (IPs privadas/reservadas
  rechazadas, redirects revalidados), la decodificación tiene tope contra
  bombas de descompresión, y los `.xlsx` de entrada se verifican por tamaño
  antes de materializarse en memoria. Lo mismo aplica a lo que se *escribe*:
  una sola celda puede traer una lista arbitrariamente larga de URLs, así que
  la cantidad de columnas insertadas tiene como tope el límite real de Excel
  (16 384) y el sobrante se reporta, en vez de producir un archivo que Excel se
  niega a abrir sin decir por qué.
- **Las descargas de imágenes son lentas a propósito.** La inserción usa 8
  descargas simultáneas, no 32. No es una estimación prudente: medido contra
  `i.ebayimg.com`, con 32 fallaban por timeout las 29 imágenes de un archivo
  real y el CDN seguía castigando las corridas siguientes (15 ok, después 0,
  después 0). El servidor tolera pedidos espaciados y hace *tarpit* con las
  ráfagas —acepta la conexión y nunca responde—, así que acá el límite es su
  paciencia, no nuestro ancho de banda. Subir el número otra vez hace que la
  herramienta *parezca* más rápida y no entregue nada. Los reintentos usan
  backoff exponencial por la misma razón.

### Pruebas

```bash
cargo test --workspace                    # todo (381 tests)
cargo clippy --workspace --all-targets    # lints, sin warnings
cargo fmt --all --check                   # formato
```

La suite cubre el motor completo de cada herramienta: escritores, lectores,
orden externo, reglas de validación, pipeline async de OCR y flujos E2E. Los
menús interactivos cuentan con un arnés de scripting
(`app_shell::testing::con_guion`) para testear flujos de CLI sin terminal
real. Los tests basados en propiedades (`proptest`) cubren las invariantes que
el sistema de tipos no puede cerrar por sí solo.

**El XLSX generado se verifica contra dos parsers independientes.** Los tests
unitarios comparan nuestra salida contra nuestras propias funciones, lo que no
detecta un XML coherente con lo que creemos escribir pero inválido para quien
lo lee. `tests/xlsx_propiedades.rs` mete entrada hostil generada al azar
(metacaracteres XML, comillas, bytes de control, unicode fuera del plano
básico) por el escritor y relee el resultado con `calamine` *y*
`umya-spreadsheet` — dos lectores escritos por gente distinta. El segundo
importa más allá de validar: es con el que lee `ocr_tools`, así que además
demuestra que las cinco herramientas se pueden encadenar (la salida de una es
entrada válida de la siguiente). Un solo `&` sin
escapar no ensucia una celda: hace que Excel rechace el archivo entero, y esa
es la propiedad que se sostiene.

**La cobertura es un umbral, no un número para mirar.** CI corre
`cargo llvm-cov` sobre las librerías (los binarios quedan fuera: son la capa
interactiva) y falla por debajo del **88 % de líneas**; la cifra actual es
**92.91 %**. El umbral sube cuando se gana terreno, nunca baja para que pase un
PR. El reporte HTML se sube como artefacto en cada corrida.

CI corre los comandos de arriba en cada push (`.github/workflows/ci.yml`).
