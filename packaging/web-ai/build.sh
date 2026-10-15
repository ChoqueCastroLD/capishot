#!/usr/bin/env bash
# Build Capishot's AI Web Worker (apps/photocraft-ai-worker) into a built web site and point the
# page at it. The worker is a second wasm module that the page loads only when Image › Upscale
# with AI… runs, so the main module stays the same size.
#
#   packaging/web-ai/build.sh <site-dir>        e.g. dist/web or dist/web-threads
#
# Writes <site-dir>/ai/<hash>/photocraft_ai_worker.js and photocraft_ai_worker_bg.wasm (+ .gz for
# servers with precompressed files), content-addressed so they can be cached forever, and adds
# <meta name="capishot-ai-worker" content="ai/<hash>/photocraft_ai_worker.js"> to index.html.
# PHOTOCRAFT_AI_MODELS_URL, when set, adds <meta name="capishot-ai-models" content="..."> (where
# the browser downloads model weights from; same origin, or CORS + Cross-Origin-Resource-Policy:
# cross-origin under the threads build's COEP). Weights are never part of the build.
#
# The worker is the same for the single-threaded and the threads build (no shared memory).
# Needs: wasm32-unknown-unknown, wasm-bindgen-cli at Cargo.lock's version (or trunk's cached copy),
# wasm-opt (binaryen) on PATH or trunk's cached copy.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SITE="${1:?usage: packaging/web-ai/build.sh <site-dir>}"
[ -f "$SITE/index.html" ] || { echo "error: $SITE/index.html missing (build the site first)" >&2; exit 1; }
cd "$ROOT"
# Not the threads build's flags (shared memory): the worker is a plain module.
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
PROFILE=wasm-release

want=$(awk '/^name = "wasm-bindgen"$/{getline; gsub(/"/,"",$3); print $3; exit}' Cargo.lock)
BINDGEN=$(command -v wasm-bindgen || true)
if [ -z "$BINDGEN" ] || [ "$("$BINDGEN" --version | awk '{print $2}')" != "$want" ]; then
  BINDGEN="$HOME/.cache/trunk/wasm-bindgen-$want/wasm-bindgen"
fi
[ -x "$BINDGEN" ] || { echo "error: wasm-bindgen-cli $want needed: cargo install --locked wasm-bindgen-cli --version $want" >&2; exit 1; }
WASM_OPT=$(command -v wasm-opt || find "$HOME/.cache/trunk" -name wasm-opt -type f 2>/dev/null | head -1 || true)

cargo build --profile "$PROFILE" -p photocraft-ai-worker --target wasm32-unknown-unknown --target-dir "$TARGET_DIR"
STAGE="$TARGET_DIR/ai-worker-bindgen"
rm -rf "$STAGE"
"$BINDGEN" --target web --no-typescript --out-dir "$STAGE" --out-name photocraft_ai_worker \
  "$TARGET_DIR/wasm32-unknown-unknown/$PROFILE/photocraft-ai-worker.wasm"
if [ -n "$WASM_OPT" ]; then
  "$WASM_OPT" -Oz --enable-bulk-memory --enable-mutable-globals --enable-sign-ext --enable-nontrapping-float-to-int \
    --enable-reference-types --enable-multivalue "$STAGE/photocraft_ai_worker_bg.wasm" -o "$STAGE/opt.wasm"
  mv "$STAGE/opt.wasm" "$STAGE/photocraft_ai_worker_bg.wasm"
fi
hash=$(cat "$STAGE/photocraft_ai_worker.js" "$STAGE/photocraft_ai_worker_bg.wasm" | sha256sum | cut -c1-16)
rm -rf "$SITE/ai"
mkdir -p "$SITE/ai/$hash"
cp "$STAGE/photocraft_ai_worker.js" "$STAGE/photocraft_ai_worker_bg.wasm" "$SITE/ai/$hash/"
gzip -9 -n -k "$SITE/ai/$hash/photocraft_ai_worker_bg.wasm"

# Point the page at it (replacing an earlier build's tags).
tags="<meta name=\"capishot-ai-worker\" content=\"ai/$hash/photocraft_ai_worker.js\">"
if [ -n "${PHOTOCRAFT_AI_MODELS_URL:-}" ]; then
  case "$PHOTOCRAFT_AI_MODELS_URL" in
    *[\"\<\>\|\\]*) echo "error: PHOTOCRAFT_AI_MODELS_URL must be a plain URL" >&2; exit 1 ;;
  esac
  tags="$tags<meta name=\"capishot-ai-models\" content=\"$PHOTOCRAFT_AI_MODELS_URL\">"
fi
sed -i -e 's|<meta name="capishot-ai-[a-z]*" content="[^"]*">||g' -e "s|</head>|$tags</head>|" "$SITE/index.html"
raw=$(wc -c <"$SITE/ai/$hash/photocraft_ai_worker_bg.wasm" | tr -d ' ')
gz=$(wc -c <"$SITE/ai/$hash/photocraft_ai_worker_bg.wasm.gz" | tr -d ' ')
echo "AI worker: ai/$hash/photocraft_ai_worker_bg.wasm $raw bytes ($((raw / 1048576)) MiB), gzipped $gz bytes"
max="${PHOTOCRAFT_WASM_MAX_BYTES:-25165824}" # 24 MiB, as the main module
if [ "$raw" -gt "$max" ]; then
  echo "error: the AI worker is over the $max-byte per-file limit" >&2
  exit 1
fi
