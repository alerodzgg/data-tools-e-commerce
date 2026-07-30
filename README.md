# DATA TOOLS E-COMMERCE

Cinco herramientas de terminal para preparar, limpiar y validar publicaciones
de autopartes (eBay / Amazon) a escala de **millones de filas**, escritas en
**Rust** sobre [Polars](https://pola.rs): binarios nativos, sin dependencias
de runtime.

| Herramienta | Binario | Qué hace |
|---|---|---|
| **Publications builder** | `publications_builder` | Construye las publicaciones desde el scrap crudo: precios, SKUs, características, compatibilidades (eBay / Amazon). |
| **Publications validator** | `publications_validator` | Valida y limpia los títulos (7 reglas + lista de marcas), deduplica por título conservando el mejor precio. |
| **OCR tools** | `ocr_tools` | Filtra imágenes de producto por su contenido: banners, fondos no neutros, diagramas, texto comercial, logos y placeholders (detectores CPU + OCR con `ort`/ONNX Runtime). |
| **ETL tools** | `etl_tools` | BUSCARV (exacto y parcial por palabra completa), duplicados, borrado por palabra/color, ordenar estilo Excel, marcar filas que contienen palabras. |
| **DATA combinator** | `data_combinator` | Combina varios XLSX/CSV en una salida única, con orden global (mezcla externa en disco para 10M+ filas) y división por hojas o archivos. |

Las cinco se lanzan desde un menú único, `hub`. Todo se maneja con **menús de
flechas** en la terminal — sin argumentos que memorizar. Los XLSX se escriben
generando el XML OOXML directo (sin librería de terceros) y **todo se trata
como texto**: los SKUs y códigos como `007` nunca se convierten en números.

## Uso

```bash
cargo run --release -p hub                      # menú con las 5 herramientas
cargo run --release -p etl_tools                # o una herramienta directa
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

## Compilar

Rust ≥ 1.80 (edition 2021).

```bash
cargo build --release --workspace
```

Los binarios quedan en `target/release/` (uno por herramienta, más `hub`).
`ocr_tools` necesita además `models/` (pesos del detector/reconocedor) y
`runtime/` (motor de inferencia: `onnxruntime.dll` en Windows,
`libonnxruntime.so` en Linux) — busca ambas carpetas junto a su propio
ejecutable en tiempo de EJECUCIÓN (`ocr_tools::assets_dir()`), así que el
binario compilado se puede mover o empaquetar sin quedar atado al checkout.

## Instalar en Ubuntu (paquete `.deb`)

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

## Pruebas

```bash
cargo test --workspace              # todo (307 tests)
cargo clippy --workspace --all-targets   # lints, sin warnings
cargo fmt --all --check                  # formato
```

La suite cubre el motor completo de cada herramienta: escritores, lectores,
orden externo, reglas de validación, pipeline async de OCR y flujos E2E. Los
menús interactivos cuentan además con un arnés de scripting
(`app_shell::testing::con_guion`) para testear flujos de CLI sin terminal
real.

## Arquitectura

Workspace de 8 crates bajo `crates/`:

```
commerce_core          ← motor compartido (escritores XLSX/CSV, lectores, XML, mojibake). Sin UI.
app_shell               ← rutas multiplataforma + menús/estilo + diálogos comunes.
hub                     ← lanzador de las 5 herramientas (menú único).
publications_builder    ← construir publicaciones (eBay / Amazon).
publications_validator  ← validar y limpiar títulos.
ocr_tools               ← filtrar imágenes por su contenido.
etl_tools               ← BUSCARV, duplicados, colores, Encontrar.
data_combinator         ← combinar varios archivos en uno.
```

Regla de oro: `commerce_core` no sabe nada de la interfaz (no imprime, no
pregunta); `app_shell` es el único lugar donde `println!`/menús/barras de
progreso son aceptables. Cada herramienta es su propio binario (proceso
independiente que `hub` lanza como hijo), no un módulo importado en el mismo
proceso — así cada una puede tener su propio runtime (p. ej. `ocr_tools` usa
`tokio`) sin forzar uno compartido entre las cinco.
