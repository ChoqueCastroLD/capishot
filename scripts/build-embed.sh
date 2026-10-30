#!/bin/sh
# Builds Capishot's embeddable web package (the trunk web build: wasm + JS glue, plus the embedding
# API's TypeScript types) and publishes it, versioned by commit, to
#   ${CAPISHOT_EMBED_BUILDS:-dist/embed}/<hash>/
# with a manifest.json (embed API version, commit, sha256 of every file) that hosts pin.
#
#   scripts/build-embed.sh            # build HEAD (refuses uncommitted changes to tracked files)
#   ALLOW_DIRTY=1 scripts/build-embed.sh   # a <hash>-dirty build of the working tree
#   FORCE=1 scripts/build-embed.sh    # rebuild a hash that's already published
#
# Env: CARGO_TARGET_DIR (reuse a warm wasm cache), CARGO_BUILD_JOBS (default 6).
# Contract: apps/photocraft-web/embed/README.md.
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

OUT_ROOT=${CAPISHOT_EMBED_BUILDS:-$PWD/dist/embed}
COMMIT=$(git rev-parse HEAD)
HASH=$(git rev-parse --short=12 HEAD)
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  if [ "${ALLOW_DIRTY:-0}" != 1 ]; then
    echo "build-embed: uncommitted changes; commit first (or ALLOW_DIRTY=1 for a -dirty build)" >&2
    exit 1
  fi
  HASH="$HASH-dirty"
fi
DEST="$OUT_ROOT/$HASH"
if [ -e "$DEST/manifest.json" ] && [ "${FORCE:-0}" != 1 ]; then
  echo "build-embed: $DEST already published (FORCE=1 rebuilds it)"
  exit 0
fi

API=$(sed -n 's/^pub const EMBED_API_VERSION: u32 = \([0-9]*\);/\1/p' apps/photocraft-web/src/embed_api_core.rs)
[ -n "$API" ] || { echo "build-embed: EMBED_API_VERSION not found" >&2; exit 1; }

export CAPISHOT_COMMIT="$COMMIT" CAPISHOT_BUILD_HASH="$HASH"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
(cd apps/photocraft-web && trunk build --release --dist "$STAGE/dist")

JS=$(cd "$STAGE/dist" && ls photocraft-web-*.js | head -n1)
WASM=$(cd "$STAGE/dist" && ls photocraft-web-*_bg.wasm | head -n1)
[ -n "$JS" ] && [ -n "$WASM" ] || { echo "build-embed: trunk produced no module" >&2; exit 1; }

PKG="$STAGE/pkg"
mkdir -p "$PKG"
# Stable names inside the versioned directory; the glue gets the wasm URL from the host.
cp "$STAGE/dist/$JS" "$PKG/capishot-embed.js"
cp "$STAGE/dist/$WASM" "$PKG/capishot-embed_bg.wasm"
# The module's lazily loaded chunks (packaging/web-split), under their own names: the glue finds
# them next to itself.
for c in "$STAGE/dist/${WASM%.wasm}".c*.wasm; do if [ -f "$c" ]; then cp "$c" "$PKG/"; fi; done
[ -d "$STAGE/dist/snippets" ] && cp -r "$STAGE/dist/snippets" "$PKG/"
cp apps/photocraft-web/embed/capishot-embed.d.ts "$PKG/capishot-embed.d.ts"

python3 - "$PKG" "$API" "$COMMIT" "$HASH" <<'PY'
import hashlib, json, os, sys, datetime
pkg, api, commit, build = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
files = {}
for root, _, names in os.walk(pkg):
    for n in sorted(names):
        p = os.path.join(root, n)
        rel = os.path.relpath(p, pkg)
        data = open(p, 'rb').read()
        files[rel] = {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}
manifest = {
    "embedApi": api,
    "capishotCommit": commit,
    "buildHash": build,
    "builtAt": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "entry": {"js": "capishot-embed.js", "wasm": "capishot-embed_bg.wasm", "types": "capishot-embed.d.ts"},
    "files": dict(sorted(files.items())),
}
json.dump(manifest, open(os.path.join(pkg, "manifest.json"), "w"), indent=2)
PY

mkdir -p "$OUT_ROOT"
rm -rf "$DEST.tmp" && cp -r "$PKG" "$DEST.tmp" && rm -rf "$DEST" && mv "$DEST.tmp" "$DEST"
echo "build-embed: published $DEST"
ls -la "$DEST"
