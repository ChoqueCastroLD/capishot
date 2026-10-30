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
want=$(awk '/^name = "wasm-bindgen"$/{getline; gsub(/"/,"",$3); print $3; exit}' Cargo.lock)
have=$(wasm-bindgen --version 2>/dev/null | awk '{print $2}')
if [ "$want" != "$have" ]; then
  echo "error: wasm-bindgen-cli $want needed (found: ${have:-none}):" >&2
  echo "  cargo install --locked wasm-bindgen-cli --version $want" >&2
  exit 1
fi

# shellcheck source=../web-split/binaryen.sh
. "$ROOT/packaging/web-split/binaryen.sh"
WASM_OPT=$(binaryen_tool wasm-opt)

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
  --debuginfo "$STAGE/photocraft_bg.wasm" -o "$STAGE/photocraft_bg.opt.wasm"
mv "$STAGE/photocraft_bg.opt.wasm" "$STAGE/photocraft_bg.wasm"
# What the first frame needs stays in photocraft_bg.wasm; the rest goes to photocraft_bg.cN.wasm,
# fetched in the background once the app runs (packaging/web-split; it drops the names kept above
# for it). PHOTOCRAFT_WEB_SPLIT=0 skips the split (=names also keeps the names, for profile.sh).
if [ "${PHOTOCRAFT_WEB_SPLIT:-1}" = "names" ]; then
  :
elif [ "${PHOTOCRAFT_WEB_SPLIT:-1}" = "0" ]; then
  python3 "$ROOT/packaging/web-split/split.py" --wasm "$STAGE/photocraft_bg.wasm" --glue "$STAGE/photocraft.js" --cold - --strip-only
else
  python3 "$ROOT/packaging/web-split/split.py" --threads --wasm "$STAGE/photocraft_bg.wasm" --glue "$STAGE/photocraft.js" \
    --cold "$ROOT/packaging/web-split/cold-functions-threads.txt.gz" --wasm-split "$(binaryen_tool wasm-split)"
fi

mkdir -p "$OUT"
cp -R "$STAGE/." "$OUT/"
# The wasm ships gzipped and the page inflates it with DecompressionStream on its way into
# streaming compilation: under half the bytes to upload and download for a host without
# compression. The chunks stay plain wasm (the loader compiles them as they are).
raw=$(wc -c <"$OUT/photocraft_bg.wasm" | tr -d ' ')
# The glue's snippet imports (the worker helper, recovery storage, watchdog) get a content version
# too, cached forever like the glue (_headers): every worker of the pool imports them, and each
# had to revalidate them over the network before the app could start (several round trips on a
# phone). The import object's keys (the wasm's import module names) stay as they are.
snip=$( (find "$OUT/snippets" -name '*.js' -type f | LC_ALL=C sort | xargs cat; cat "$OUT"/photocraft_bg*.wasm) | sha256sum | cut -c1-16)
sed -i -E "/^import /s#(['\"]\./snippets/[^'\"]+\.js)(['\"])#\1?v=$snip\2#" "$OUT/photocraft.js"
# pigz -11 is zopfli on every core: still plain gzip for every browser, about 4% (0.4 MB) less to
# download than gzip -9, in a few seconds.
if command -v pigz >/dev/null; then
  pigz -11 -n "$OUT/photocraft_bg.wasm"
else
  gzip -9 -n "$OUT/photocraft_bg.wasm"
fi
# The page loads the glue and the wasm with ?v=<content hash>, so both can be cached forever.
version=$(cat "$OUT/photocraft.js" "$OUT"/photocraft_bg*.wasm* | sha256sum | cut -c1-16)
sed -e "s/__PHOTOCRAFT_VERSION__/$version/g" -e "s|__PHOTOCRAFT_FALLBACK__|$FALLBACK|g" "$HERE/index.html" >"$OUT/index.html"
# The glue, the module and the glue's snippets go to v/<version>/: an index.html from an earlier
# deploy (a cached page, a tab opened before it) keeps loading its own glue and module, never a
# mix of two builds (deploy.sh keeps the previous builds' v/ directories).
mkdir -p "$OUT/v/$version"
mv "$OUT/photocraft.js" "$OUT"/photocraft_bg*.wasm* "$OUT/v/$version/"
[ -d "$OUT/snippets" ] && mv "$OUT/snippets" "$OUT/v/$version/"
# The page's loader (progress, a broken load's reason, the fall back to the single-threaded build).
cp "$ROOT/apps/photocraft-web/boot.js" "$OUT/"
cp "$HERE/_headers" "$OUT/"
# Translation catalogs the build doesn't compile in (photocraft-ui-egui `all-catalogs`): the app
# fetches i18n/<code>.tsv when that language is chosen.
mkdir -p "$OUT/i18n"
cp "$ROOT"/crates/ui-egui/src/i18n/*.tsv "$ROOT/crates/ui-egui/src/i18n/LICENSE-translations.txt" "$OUT/i18n/"
# No served fonts unless packaging adds them: an empty manifest (a missing one is a 404 in the console).
[ -f "$OUT/fonts/manifest.txt" ] || { mkdir -p "$OUT/fonts"; cp "$ROOT/apps/photocraft-web/fonts/manifest.txt" "$OUT/fonts/"; }

# Image › Upscale with AI: the AI Web Worker module, loaded on demand (PHOTOCRAFT_WEB_AI=0 skips).
if [ "${PHOTOCRAFT_WEB_AI:-1}" != "0" ]; then
  "$ROOT/packaging/web-ai/build.sh" "$OUT" --threads
fi

# Cloudflare Workers/Pages reject any single file over 25 MiB; fail well before that.
max="${PHOTOCRAFT_WASM_MAX_BYTES:-33554432}" # 32 MiB (our nginx has no 25 MiB limit)
size=$(wc -c <"$OUT/v/$version/photocraft_bg.wasm.gz" | tr -d ' ')
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
