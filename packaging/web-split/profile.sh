#!/usr/bin/env bash
# Regenerate a cold-function list: which functions the split moves out of the first-load module.
#
#   CHROME=<chromium> CAPISHOT_PROFILE_PAGE=<a page image> packaging/web-split/profile.sh st
#   CHROME=<chromium> CAPISHOT_PROFILE_PAGE=<a page image> packaging/web-split/profile.sh threads
#
# Builds the single-threaded (Trunk) or the threads (packaging/web-threads) release with its names
# kept and unsplit, instruments the module (wasm-split --instrument; the threads build's records
# go to memory, shared by its workers), serves it, runs the basic page flows (profile-flow.mjs:
# desktop and phone, in Spanish, English and Portuguese) and writes every function they never ran
# to cold-functions.txt.gz (st) or cold-functions-threads.txt.gz, and the commands they ran to
# hot-commands.txt (or -threads): those run at once, the rest wait for the chunks. The list is by name, so it keeps working across builds: a
# function renamed or added since stays in the first-load module until the next profile. Run it
# again when the first-load module has grown, or after changes to what the first frames or the
# basic tools run (a function the page needs before its chunk arrived is fetched synchronously,
# which stalls the page). Needs node with playwright-core, and what each build needs.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
# shellcheck source=binaryen.sh
. "$HERE/binaryen.sh"
: "${CHROME:?set CHROME to a Chromium binary}" "${CAPISHOT_PROFILE_PAGE:?set CAPISHOT_PROFILE_PAGE to a page image}"
BUILD="${1:?st or threads}"
SPLIT=$(binaryen_tool wasm-split)
F="--enable-mutable-globals --enable-nontrapping-float-to-int --enable-bulk-memory --enable-sign-ext
   --enable-reference-types --enable-multivalue --enable-bulk-memory-opt"
WORK=$(mktemp -d)
trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$WORK"' EXIT
case "$BUILD" in
  st)
    (cd "$ROOT/apps/photocraft-web" && PHOTOCRAFT_WEB_SPLIT=names trunk build --release --dist "$WORK/site")
    wasm=$(ls "$WORK"/site/*_bg.wasm | head -1)
    glue="${wasm%_bg.wasm}.js"
    cp "$wasm" "$WORK/named.wasm"
    # shellcheck disable=SC2086
    "$SPLIT" --instrument $F -g "$WORK/named.wasm" -o "$wasm"
    out="$HERE/cold-functions.txt.gz" hot="$HERE/hot-commands.txt"
    ;;
  threads)
    # With the AI worker, as deployed: the page sets it up at startup.
    PHOTOCRAFT_WEB_SPLIT=names "$ROOT/packaging/web-threads/build.sh"
    cp -R "$ROOT/dist/web-threads" "$WORK/site"
    wasm=$(ls "$WORK"/site/v/*/photocraft_bg.wasm.gz)
    glue="$(dirname "$wasm")/photocraft.js"
    gunzip -c "$wasm" >"$WORK/named.wasm"
    F="$F --enable-threads --enable-simd"
    # In memory (its first bytes, below the main thread's stack): every worker's runs count. The
    # page compiles the file as is when it isn't gzip.
    # shellcheck disable=SC2086
    "$SPLIT" --instrument --in-memory $F -g "$WORK/named.wasm" -o "$wasm"
    out="$HERE/cold-functions-threads.txt.gz" hot="$HERE/hot-commands-threads.txt"
    ;;
  *) echo "usage: profile.sh st|threads" >&2; exit 1 ;;
esac
# As deployed: an AI models URL (the app lists the models at startup).
grep -q 'name="capishot-ai-models"' "$WORK/site/index.html" \
  || sed -i 's#</head>#<meta name="capishot-ai-models" content="models/"></head>#' "$WORK/site/index.html"
# The flows read the profile out of the page's instance.
sed -i "0,/wasm = instance.exports;/s//wasm = instance.exports; if (typeof document !== 'undefined') self.__wasm = wasm;/" "$glue"
python3 "$HERE/sri.py" "$WORK/site/index.html"
port=${CAPISHOT_PROFILE_PORT:-8791}
python3 "$ROOT/packaging/web-threads/serve.py" "$WORK/site" "$port" >/dev/null 2>&1 &
sleep 1
# Each interface language takes its own paths at startup (catalogs, fonts, layout), and so does a
# URL with a query (?r=: what a retry or the fall back to the single-threaded build opens).
for run in desktop:es-ES: phone:en-US:?r=1 desktop:pt-BR:?r=1 phone:es-ES: desktop:en-US: phone:pt-BR:?r=1 phone360:es-ES:; do
  IFS=: read -r mode lang query <<<"$run"
  node "$HERE/profile-flow.mjs" "http://127.0.0.1:$port/$query" "$WORK/$mode-$lang${query:+-q}.prof" "$mode" "$lang"
done
# shellcheck disable=SC2086
"$SPLIT" --merge-profiles $F "$WORK"/*.prof -o "$WORK/all.prof"
# shellcheck disable=SC2086
"$SPLIT" --print-profile="$WORK/all.prof" $F "$WORK/named.wasm" >"$WORK/printed.txt" 2>/dev/null
python3 "$HERE/cold_list.py" "$WORK/printed.txt" "$out"
# The commands the flows ran: they don't wait for the chunks.
{ echo "# Commands the basic page flows ran (profile.sh): they run before the chunks are in."; cat "$WORK"/*.prof.commands | grep -v '^$' | LC_ALL=C sort -u; } >"$hot"
echo "$(grep -vc '^#' "$hot") hot commands"
