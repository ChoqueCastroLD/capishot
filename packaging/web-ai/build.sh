#!/usr/bin/env bash
# Build Capishot's AI Web Worker (apps/photocraft-ai-worker) into a built web site and point the
# page at it. The worker is a second wasm module that the page loads only when an AI command
# runs, so the main module stays the same size.
#
#   packaging/web-ai/build.sh <site-dir> [--threads]     e.g. dist/web, or dist/web-threads --threads
#
# Writes <site-dir>/ai/<hash>/ (content-addressed, so it can be cached forever; .gz copies for
# servers with precompressed files) and adds <meta> tags to index.html:
#
# * photocraft_ai_worker.js + _bg.wasm, <meta name="capishot-ai-worker">: the plain worker.
#   WebGPU, else the CPU on one thread. SIMD128 kernels.
# * with --threads, also photocraft_ai_worker_mt.js + _bg.wasm + snippets/,
#   <meta name="capishot-ai-worker-threads">: the threads variant for cross-origin isolated
#   pages. CPU only (wgpu doesn't build with shared memory), with a rayon pool of nested Web
#   Workers; the page uses it for CPU runs. Skipped with PHOTOCRAFT_WEB_AI_THREADS=0.
#
# PHOTOCRAFT_AI_MODELS_URL, when set, adds <meta name="capishot-ai-models" content="..."> (where
# the browser downloads model weights from; same origin, or CORS + Cross-Origin-Resource-Policy:
# cross-origin under the threads build's COEP). Weights are never part of the build.
#
# Needs: wasm32-unknown-unknown, wasm-bindgen-cli at Cargo.lock's version (or trunk's cached copy),
# wasm-opt (binaryen) on PATH or trunk's cached copy. The threads variant rebuilds std with
# atomics: rust-src for PHOTOCRAFT_AI_THREADS_TOOLCHAIN (default `stable`, with RUSTC_BOOTSTRAP=1
# for -Zbuild-std: current nightlies don't build burn's `branches` dependency).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SITE="${1:?usage: packaging/web-ai/build.sh <site-dir> [--threads]}"
THREADS=0
if [ "${2:-}" = "--threads" ] && [ "${PHOTOCRAFT_WEB_AI_THREADS:-1}" != "0" ]; then
  THREADS=1
fi
[ -f "$SITE/index.html" ] || { echo "error: $SITE/index.html missing (build the site first)" >&2; exit 1; }
cd "$ROOT"
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
# Speed over size (Cargo.toml: the worker is cached and loaded only for AI runs).
PROFILE="${PHOTOCRAFT_AI_PROFILE:-wasm-ai}"
TC="${PHOTOCRAFT_AI_THREADS_TOOLCHAIN:-stable}"

want=$(awk '/^name = "wasm-bindgen"$/{getline; gsub(/"/,"",$3); print $3; exit}' Cargo.lock)
BINDGEN=$(command -v wasm-bindgen || true)
if [ -z "$BINDGEN" ] || [ "$("$BINDGEN" --version | awk '{print $2}')" != "$want" ]; then
  BINDGEN="$HOME/.cache/trunk/wasm-bindgen-$want/wasm-bindgen"
fi
[ -x "$BINDGEN" ] || { echo "error: wasm-bindgen-cli $want needed: cargo install --locked wasm-bindgen-cli --version $want" >&2; exit 1; }
WASM_OPT=$(command -v wasm-opt || find "$HOME/.cache/trunk" -name wasm-opt -type f 2>/dev/null | head -1 || true)
OPT_FEATURES="--enable-bulk-memory --enable-mutable-globals --enable-sign-ext --enable-nontrapping-float-to-int --enable-simd --enable-reference-types --enable-multivalue"

STAGE="$TARGET_DIR/ai-worker-bindgen"
rm -rf "$STAGE"
mkdir -p "$STAGE"

# The plain worker. SIMD128: every browser with WebGPU has it (Chrome 91, Firefox 89, Safari 16.4).
RUSTFLAGS="-C target-feature=+simd128" \
  cargo build --profile "$PROFILE" -p photocraft-ai-worker --target wasm32-unknown-unknown --target-dir "$TARGET_DIR/web-ai"
"$BINDGEN" --target web --no-typescript --out-dir "$STAGE" --out-name photocraft_ai_worker \
  "$TARGET_DIR/web-ai/wasm32-unknown-unknown/$PROFILE/photocraft-ai-worker.wasm"
if [ -n "$WASM_OPT" ]; then
  # shellcheck disable=SC2086
  "$WASM_OPT" -O3 $OPT_FEATURES "$STAGE/photocraft_ai_worker_bg.wasm" -o "$STAGE/opt.wasm"
  mv "$STAGE/opt.wasm" "$STAGE/photocraft_ai_worker_bg.wasm"
fi

if [ "$THREADS" = 1 ]; then
  # As packaging/web-threads: shared, imported memory (up to 4 GiB) and the TLS exports
  # wasm-bindgen needs to start each pool worker.
  link=""
  for a in --shared-memory --import-memory --max-memory=4294967296 --export=__heap_base \
           --export=__wasm_init_tls --export=__tls_size --export=__tls_align --export=__tls_base; do
    link="$link -C link-arg=$a"
  done
  RUSTC_BOOTSTRAP=1 RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+simd128$link -Zlocation-detail=none" \
    cargo "+$TC" build --profile "$PROFILE" -p photocraft-ai-worker --target wasm32-unknown-unknown \
    -Z build-std=panic_abort,std --target-dir "$TARGET_DIR/web-ai-threads"
  "$BINDGEN" --target web --no-typescript --out-dir "$STAGE" --out-name photocraft_ai_worker_mt \
    "$TARGET_DIR/web-ai-threads/wasm32-unknown-unknown/$PROFILE/photocraft-ai-worker.wasm"
  if [ -n "$WASM_OPT" ]; then
    # shellcheck disable=SC2086
    "$WASM_OPT" -O3 --enable-threads $OPT_FEATURES "$STAGE/photocraft_ai_worker_mt_bg.wasm" -o "$STAGE/opt.wasm"
    mv "$STAGE/opt.wasm" "$STAGE/photocraft_ai_worker_mt_bg.wasm"
  fi
fi

hash=$(find "$STAGE" -type f | LC_ALL=C sort | xargs cat | sha256sum | cut -c1-16)
rm -rf "$SITE/ai"
mkdir -p "$SITE/ai/$hash"
cp -R "$STAGE/." "$SITE/ai/$hash/"
for w in "$SITE/ai/$hash"/*_bg.wasm; do
  gzip -9 -n -k "$w"
done

# Point the page at it (replacing an earlier build's tags).
tags="<meta name=\"capishot-ai-worker\" content=\"ai/$hash/photocraft_ai_worker.js\">"
if [ "$THREADS" = 1 ]; then
  tags="$tags<meta name=\"capishot-ai-worker-threads\" content=\"ai/$hash/photocraft_ai_worker_mt.js\">"
fi
if [ -n "${PHOTOCRAFT_AI_MODELS_URL:-}" ]; then
  case "$PHOTOCRAFT_AI_MODELS_URL" in
    *[\"\<\>\|\\]*) echo "error: PHOTOCRAFT_AI_MODELS_URL must be a plain URL" >&2; exit 1 ;;
  esac
  tags="$tags<meta name=\"capishot-ai-models\" content=\"$PHOTOCRAFT_AI_MODELS_URL\">"
fi
sed -i -e 's|<meta name="capishot-ai-[a-z-]*" content="[^"]*">||g' -e "s|</head>|$tags</head>|" "$SITE/index.html"
max="${PHOTOCRAFT_WASM_MAX_BYTES:-33554432}" # 32 MiB, as the main module
for w in "$SITE/ai/$hash"/*_bg.wasm; do
  raw=$(wc -c <"$w" | tr -d ' ')
  gz=$(wc -c <"$w.gz" | tr -d ' ')
  echo "AI worker: ai/$hash/$(basename "$w") $raw bytes ($((raw / 1048576)) MiB), gzipped $gz bytes"
  if [ "$raw" -gt "$max" ]; then
    echo "error: $(basename "$w") is over the $max-byte per-file limit" >&2
    exit 1
  fi
done
