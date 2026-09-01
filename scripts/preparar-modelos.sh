#!/bin/bash
# Baja los modelos ONNX desde S3 y los deja donde el binario los busca.
#
# Los modelos viven en Git LFS, pero la cuota de la cuenta está agotada: un
# `git clone` trae punteros de 133 bytes en lugar de los archivos, y el
# síntoma al correr es "Protobuf parsing failed". Este script los trae de S3,
# que además es rápido entre instancias de la misma región.
#
# Copia a DOS destinos a propósito:
#   - `crates/ocr_tools/models/`  para que `build.rs` no pise los buenos con
#     los punteros de LFS en la próxima compilación.
#   - `target/release/models/`    donde el binario los busca en ejecución.
#
# Requiere que la instancia tenga un rol de IAM con lectura sobre el bucket.

set -euo pipefail

BUCKET="${OCR_BUCKET:-s3://ocr-tools-alerodzgg/modelos}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

for destino in "$REPO/crates/ocr_tools/models" "$REPO/target/release/models"; do
    mkdir -p "$destino"
    aws s3 cp "$BUCKET/craft_detector.onnx" "$destino/"
    aws s3 cp "$BUCKET/recognizer_latin_g2.onnx" "$destino/"
done

# Verificar que llegaron los modelos y no los punteros: un puntero de LFS pesa
# ~133 bytes y el detector real ronda los 79 MB. Fallar acá es mucho más
# barato que descubrirlo tras 20 minutos de compilación.
detector="$REPO/target/release/models/craft_detector.onnx"
tamano=$(stat -c%s "$detector")
if [ "$tamano" -lt 1000000 ]; then
    echo "ERROR: craft_detector.onnx pesa $tamano bytes; parece un puntero de LFS, no el modelo." >&2
    exit 1
fi

echo "Modelos listos en ambos directorios ($tamano bytes el detector)."
