# shellcheck shell=bash
# binaryen_tool <wasm-opt|wasm-split>: the tool's path, from PATH or downloaded once (checksummed)
# into target/tools. Source it after setting ROOT (the workspace root).
BINARYEN_VERSION=133
BINARYEN_SHA256=2dc9c7813f5375db93d96ead4b78222fcc3e2677bbb832297af4797782a37489 # x86_64-linux tarball

binaryen_tool() {
  if command -v "$1" >/dev/null; then
    command -v "$1"
    return
  fi
  local dir="$ROOT/target/tools/binaryen-version_$BINARYEN_VERSION"
  if [ ! -x "$dir/bin/$1" ]; then
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
  echo "$dir/bin/$1"
}
