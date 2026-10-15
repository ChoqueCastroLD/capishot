#!/usr/bin/env bash
# Build the multithreaded browser version into dist/web-threads: shared WebAssembly memory and a
# rayon pool of Web Workers (wasm-bindgen-rayon). Hosting and how it works: README.md here.
#
#   packaging/web-threads/build.sh            release build (wasm-release profile + wasm-opt)
#   packaging/web-threads/build.sh --serve    then serve it on http://127.0.0.1:8777 with the
#                                             cross-origin isolation headers threads need
#
# Needs: a nightly toolchain with rust-src and the wasm32-unknown-unknown target (std is rebuilt
# with atomics):
#   rustup toolchain install nightly --component rust-src --target wasm32-unknown-unknown
# and wasm-bindgen-cli at the version of the wasm-bindgen crate in Cargo.lock:
#   cargo install --locked wasm-bindgen-cli --version <version>
# wasm-opt (binaryen) comes from PATH, or is downloaded once into target/tools.
# Override the toolchain with PHOTOCRAFT_WEB_TOOLCHAIN (default: nightly); PHOTOCRAFT_WEB_FALLBACK
# sets where a browser without cross-origin isolation goes (the single-threaded build).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HERE="$ROOT/packaging/web-threads"
cd "$ROOT"

TC="${PHOTOCRAFT_WEB_TOOLCHAIN:-nightly}"
# Where the page sends a browser that isn't cross-origin isolated: the single-threaded build's URL
# relative to the page (for example `st/` with Trunk's dist/web copied there), or empty to show an
# error instead.
FALLBACK="${PHOTOCRAFT_WEB_FALLBACK:-}"
case "$FALLBACK" in
  *[\'\"\|\\\<\>]*) echo "error: PHOTOCRAFT_WEB_FALLBACK must be a plain relative URL" >&2; exit 1 ;;
esac
PROFILE=wasm-release
OUT="$ROOT/dist/web-threads"
# Its own target dir: the std rebuild and RUSTFLAGS differ from every other build.
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}/web-threads"
BINARYEN_VERSION=133
BINARYEN_SHA256=2dc9c7813f5375db93d96ead4b78222fcc3e2677bbb832297af4797782a37489 # x86_64-linux tarball

want=$(awk '/^name = "wasm-bindgen"$/{getline; gsub(/"/,"",$3); print $3; exit}' Cargo.lock)
have=$(wasm-bindgen --version 2>/dev/null | awk '{print $2}')
if [ "$want" != "$have" ]; then
  echo "error: wasm-bindgen-cli $want needed (found: ${have:-none}):" >&2
  echo "  cargo install --locked wasm-bindgen-cli --version $want" >&2
  exit 1
fi

wasm_opt() {
  if command -v wasm-opt >/dev/null; then
    command -v wasm-opt
    return
  fi
  local dir="$ROOT/target/tools/binaryen-version_$BINARYEN_VERSION"
  if [ ! -x "$dir/bin/wasm-opt" ]; then
    local name="binaryen-version_$BINARYEN_VERSION-x86_64-linux.tar.gz"
    local url="https://github.com/WebAssembly/binaryen/releases/download/version_$BINARYEN_VERSION/$name"
    local tmp
    tmp=$(mktemp -d)
    echo "downloading $name" >&2
    curl -fsSL "$url" -o "$tmp/$name"
    echo "$BINARYEN_SHA256  $tmp/$name" | sha256sum -c - >&2
    mkdir -p "$ROOT/target/tools"
    tar -xzf "$tmp/$name" -C "$ROOT/target/tools"
    rm -rf "$tmp"
  fi
  echo "$dir/bin/wasm-opt"
}
WASM_OPT=$(wasm_opt)

# atomics + bulk-memory: threads (std is rebuilt with them). The linker flags make the memory
# shared and imported (so every worker instantiates the module on the same memory, up to 4 GiB)
# and export the TLS symbols wasm-bindgen needs to start each thread. simd128: auto-vectorized
# pixel loops (every browser with Wasm threads also has Wasm SIMD).
link=""
for a in --shared-memory --import-memory --max-memory=4294967296 --export=__heap_base \
         --export=__wasm_init_tls --export=__tls_size --export=__tls_align --export=__tls_base; do
  link="$link -C link-arg=$a"
done
# -Zlocation-detail=none (nightly, like the std rebuild): panic messages keep their text but not
# the source file and line, which drops every panic location's path string from the module.
export RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+simd128$link -Zlocation-detail=none"
# The About dialog's "dev build" stamp, as the release workflow sets it.
export PHOTOCRAFT_BUILD_SHA="${PHOTOCRAFT_BUILD_SHA:-$(git rev-parse HEAD 2>/dev/null || true)}"
export PHOTOCRAFT_BUILD_DATE="${PHOTOCRAFT_BUILD_DATE:-$(date -u +%F)}"

# PHOTOCRAFT_WEB_FEATURES: extra cargo features of photocraft-web (`full` for everything the
# desktop app has: HEIF, OpenEXR, camera raw, plug-ins, all catalogs compiled in).
cargo "+$TC" build --profile "$PROFILE" -p photocraft-web --bin photocraft-web ${PHOTOCRAFT_WEB_FEATURES:+--features "$PHOTOCRAFT_WEB_FEATURES"} \
  --target wasm32-unknown-unknown -Z build-std=panic_abort,std --target-dir "$TARGET_DIR"

STAGE="$TARGET_DIR/bindgen"
rm -rf "$STAGE" "$OUT"
wasm-bindgen --target web --no-typescript --weak-refs --out-dir "$STAGE" --out-name photocraft \
  "$TARGET_DIR/wasm32-unknown-unknown/$PROFILE/photocraft-web.wasm"
"$WASM_OPT" -Oz --enable-threads --enable-bulk-memory --enable-mutable-globals --enable-sign-ext \
  --enable-nontrapping-float-to-int --enable-simd --enable-reference-types --enable-multivalue \
  "$STAGE/photocraft_bg.wasm" -o "$STAGE/photocraft_bg.opt.wasm"
mv "$STAGE/photocraft_bg.opt.wasm" "$STAGE/photocraft_bg.wasm"

mkdir -p "$OUT"
cp -R "$STAGE/." "$OUT/"
# The wasm ships gzipped and the page inflates it with DecompressionStream on its way into
# streaming compilation: a third of the bytes to upload and download, and far below the 25 MiB
# per-file cap of Cloudflare Workers/Pages that the raw module (about 25 MB) is close to.
raw=$(wc -c <"$OUT/photocraft_bg.wasm" | tr -d ' ')
gzip -9 -n "$OUT/photocraft_bg.wasm"
# The page loads the glue and the wasm with ?v=<content hash>, so both can be cached forever.
version=$(cat "$OUT/photocraft.js" "$OUT/photocraft_bg.wasm.gz" | sha256sum | cut -c1-16)
sed -e "s/__PHOTOCRAFT_VERSION__/$version/g" -e "s|__PHOTOCRAFT_FALLBACK__|$FALLBACK|g" "$HERE/index.html" >"$OUT/index.html"
cp "$HERE/_headers" "$OUT/"
# Translation catalogs the build doesn't compile in (photocraft-ui-egui `all-catalogs`): the app
# fetches i18n/<code>.tsv when that language is chosen.
mkdir -p "$OUT/i18n"
cp "$ROOT"/crates/ui-egui/src/i18n/*.tsv "$ROOT/crates/ui-egui/src/i18n/LICENSE-translations.txt" "$OUT/i18n/"

# Image › Upscale with AI: the AI Web Worker module, loaded on demand (PHOTOCRAFT_WEB_AI=0 skips).
if [ "${PHOTOCRAFT_WEB_AI:-1}" != "0" ]; then
  "$ROOT/packaging/web-ai/build.sh" "$OUT"
fi

# Cloudflare Workers/Pages reject any single file over 25 MiB; fail well before that.
max="${PHOTOCRAFT_WASM_MAX_BYTES:-25165824}" # 24 MiB
size=$(wc -c <"$OUT/photocraft_bg.wasm.gz" | tr -d ' ')
echo "photocraft_bg.wasm: $raw bytes ($((raw / 1048576)) MiB), gzipped $size bytes ($((size / 1048576)) MiB; limit $max)"
big=$(find "$OUT" -type f -size +"$max"c)
if [ -n "$big" ]; then
  echo "error: over the $max-byte limit (Cloudflare's per-file cap is 25 MiB): $big" >&2
  exit 1
fi
echo "wrote $OUT (version $version)"

if [ "${1:-}" = "--serve" ]; then
  exec python3 "$HERE/serve.py" "$OUT" 8777
fi
