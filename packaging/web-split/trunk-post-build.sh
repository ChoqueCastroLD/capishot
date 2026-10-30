#!/usr/bin/env bash
# Trunk post_build hook (apps/photocraft-web/Trunk.toml): split a release build's module into its
# first-load part and lazily loaded chunks (split.py), rename the files by their new content
# (rehash.py) and refresh the page's integrity hashes. Debug builds (trunk serve) are left as is.
set -euo pipefail
[ "${TRUNK_PROFILE:-}" = "release" ] || exit 0
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
# shellcheck source=binaryen.sh
. "$HERE/binaryen.sh"
D="${TRUNK_STAGING_DIR:?}"
wasm=$(ls "$D"/*_bg.wasm | head -1)
glue="${wasm%_bg.wasm}.js"
[ -f "$glue" ] || { echo "split: no glue next to $wasm" >&2; exit 1; }
if [ "${PHOTOCRAFT_WEB_SPLIT:-1}" = "names" ]; then
  exit 0 # profile.sh: unsplit, names kept
elif [ "${PHOTOCRAFT_WEB_SPLIT:-1}" = "0" ]; then
  echo "split: skipped (PHOTOCRAFT_WEB_SPLIT=0)"
  python3 "$HERE/split.py" --wasm "$wasm" --glue "$glue" --cold - --strip-only
else
  python3 "$HERE/split.py" --wasm "$wasm" --glue "$glue" --cold "$HERE/cold-functions.txt.gz" \
    --wasm-split "$(binaryen_tool wasm-split)"
fi
python3 "$HERE/rehash.py" "$D"
python3 "$HERE/sri.py" "$D/index.html"
