#!/bin/bash
# Corrida completa y desatendida de UN bloque: baja lo que hace falta,
# analiza, y sube los resultados.
#
# Lo ejecuta `ocr-tools.service` al arrancar la instancia. Si AWS reclama la
# instancia spot, systemd lo relanza y el checkpoint retoma donde iba: por eso
# los resultados se suben AL FINAL y el checkpoint se sincroniza durante la
# corrida.
#
# Qué bloque le toca a esta instancia sale de /etc/ocr-tools.env, que se
# escribe por User Data al lanzarla. Sin ese archivo el script no adivina:
# procesar el bloque equivocado sería peor que no arrancar.

set -euo pipefail

CONFIG=/etc/ocr-tools.env
if [ ! -f "$CONFIG" ]; then
    echo "ERROR: falta $CONFIG con la línea BLOQUE=<nombre>." >&2
    exit 2
fi
# shellcheck disable=SC1090
. "$CONFIG"

: "${BLOQUE:?falta BLOQUE en $CONFIG}"
BUCKET="${OCR_BUCKET_RAIZ:-s3://ocr-tools-alerodzgg}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATOS="$HOME/datos"
# Una carpeta POR BLOQUE, no una compartida: `aws s3 cp --recursive` sube todo
# lo que encuentra, asi que una salida de otra corrida se colaria en los
# resultados de esta sin que nadie lo note. Aislar por bloque lo hace
# imposible en vez de depender de acordarse de limpiar.
SALIDA="$HOME/salida/$BLOQUE"

echo "=== Bloque: $BLOQUE ==="

# 1. Modelos. Es idempotente y barato: si ya están, vuelve a copiarlos en
#    segundos. Correrlo siempre evita que una recompilación los deje como
#    punteros de LFS sin que nadie lo note.
"$REPO/scripts/preparar-modelos.sh"

# 2. El bloque de entrada. Solo se baja si falta: en un reinicio tras una
#    interrupción de spot ya está en disco y volver a traerlo es tiempo tirado.
mkdir -p "$DATOS" "$SALIDA"
if [ ! -f "$DATOS/$BLOQUE.xlsx" ]; then
    aws s3 cp "$BUCKET/bloques/$BLOQUE.xlsx" "$DATOS/$BLOQUE.xlsx"
fi

# 3. El checkpoint de una corrida anterior, si lo hay. Permite que una
#    instancia REEMPLAZADA —no solo reiniciada— retome el trabajo en vez de
#    empezar de cero.
aws s3 cp "$BUCKET/resultados/$BLOQUE/" "$SALIDA/" --recursive \
    --exclude "*" --include "_checkpoint_*.jsonl" 2>/dev/null || true

# 4. El análisis. Si lo interrumpen, sale sin escribir salidas parciales y con
#    el checkpoint intacto; systemd lo relanza y esto retoma.
"$REPO/target/release/ocr_tools" \
    --archivo "$DATOS/$BLOQUE.xlsx" \
    --salida "$SALIDA"

# 5. Resultados a S3. Solo se llega acá si el análisis terminó de verdad: un
#    apagado ordenado sale por el paso anterior sin haber escrito salidas.
aws s3 cp "$SALIDA/" "$BUCKET/resultados/$BLOQUE/" --recursive

echo "=== $BLOQUE terminado y subido a $BUCKET/resultados/$BLOQUE/ ==="
