#!/usr/bin/env bash
# Cross-build and package Capishot for Windows x64 on Linux (no Windows machine needed):
#
#   $DIST/capishot-<version>-windows-x64-portable.zip  capishot.exe, capishot-cli.exe, portable.txt
#                                                      and licenses/
#   $DIST/capishot-<version>-windows-x64.msi           per-machine installer (wixl)
#   $DIST/capishot-<version>-windows-x64.sha256        sha256sum of both
#   $DIST/latest.json                                  the update manifest for this release
#
# Usage: packaging/windows/package-linux.sh [--skip-build] [--no-msi]
#
# Needs: the x86_64-pc-windows-msvc Rust target, cargo-xwin, clang, lld and llvm (llvm-rc),
# msitools (wixl), zip and python3 with Pillow (the installer art). cargo-xwin downloads the
# MSVC CRT and the Windows SDK on first use; set XWIN_ACCEPT_LICENSE=1 once you have read and
# accepted Microsoft's licence for them. Optional: wine, to smoke-test capishot-cli.exe.
#
# The binaries link the C runtime statically (+crt-static), so neither package needs the Visual
# C++ redistributable. They are not signed (Windows SmartScreen warns on first launch).
# package.ps1 is the same build on Windows with WiX v5 (a wizard with an install-folder page).
#
# DOWNLOAD_BASE (default https://capichan.com/capishot/desktop/<version>/) is where latest.json
# says the files will be published; nothing is uploaded here.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/windows"
TARGET=x86_64-pc-windows-msvc

SKIP_BUILD=0
MSI=1
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-build) SKIP_BUILD=1; shift ;;
    --no-msi) MSI=0; shift ;;
    -h | --help) sed -n '2,22p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# MSI ProductVersion is numeric (major.minor.build); pre-release tags are dropped there.
MSI_VERSION="${VERSION%%-*}"
NAME="capishot-$VERSION-windows-x64"
WORK="$CARGO_TARGET_DIR/windows-package"
STAGE="$WORK/x64"
BIN="$CARGO_TARGET_DIR/$TARGET/release"
echo "Capishot $VERSION for Windows x64 ($TARGET)"

if [ "$SKIP_BUILD" = 0 ]; then
  : "${XWIN_ACCEPT_LICENSE:?set XWIN_ACCEPT_LICENSE=1 to accept the licence of the MSVC CRT and Windows SDK that cargo-xwin downloads}"
  export XWIN_ACCEPT_LICENSE
  # Some build scripts run `rc.exe` themselves; give them LLVM's compatible resource compiler
  # with the headers of the SDK cargo-xwin downloaded.
  LLVM_RC="$(command -v llvm-rc || ls /usr/lib/llvm-*/bin/llvm-rc 2>/dev/null | sort -V | tail -1 || true)"
  [ -n "$LLVM_RC" ] || { echo "error: llvm-rc not found (install llvm)" >&2; exit 1; }
  XWIN_DIR="${XWIN_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/cargo-xwin}/xwin"
  mkdir -p "$WORK/tools"
  cat > "$WORK/tools/rc.exe" <<EOF
#!/bin/sh
exec "$LLVM_RC" -I "$XWIN_DIR/sdk/include/um" -I "$XWIN_DIR/sdk/include/shared" -I "$XWIN_DIR/sdk/include/ucrt" -I "$XWIN_DIR/crt/include" "\$@"
EOF
  chmod +x "$WORK/tools/rc.exe"
  export PATH="$WORK/tools:$PATH"
  # Static CRT, scoped to the target so host build scripts are unaffected.
  export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="-C target-feature=+crt-static"
  export PHOTOCRAFT_REQUIRE_WINRES=1
  (cd "$ROOT" && cargo xwin build --release --locked -p photocraft -p photocraft-cli --features heif,ai --target "$TARGET")
fi

# Both binaries' PE headers: x64 (0x8664); the app is a GUI program (subsystem 2, no console
# window opens with it), the CLI a console one (3) so its output reaches the terminal.
python3 -I - "$BIN/capishot.exe" 2 "$BIN/capishot-cli.exe" 3 <<'EOF'
import struct, sys
args = sys.argv[1:]
for path, want in zip(args[::2], args[1::2]):
    with open(path, "rb") as f:
        head = f.read(4096)
    pe = struct.unpack_from("<I", head, 0x3C)[0]
    machine = struct.unpack_from("<H", head, pe + 4)[0]
    subsystem = struct.unpack_from("<H", head, pe + 0x5C)[0]
    if head[pe:pe + 4] != b"PE\0\0" or machine != 0x8664:
        sys.exit(f"{path}: not an x64 PE file (machine 0x{machine:x})")
    if subsystem != int(want):
        sys.exit(f"{path}: PE subsystem {subsystem}, expected {want}")
    print(f"ok {path.rsplit('/', 1)[-1]}: x64, PE subsystem {subsystem}")
EOF

rm -rf "$STAGE"
mkdir -p "$STAGE"
cp "$BIN/capishot.exe" "$BIN/capishot-cli.exe" "$STAGE/"

# ---- portable zip ------------------------------------------------------------------------------
PORTABLE="$WORK/$NAME-portable"
rm -rf "$PORTABLE"
mkdir -p "$PORTABLE/licenses"
cp "$STAGE"/*.exe "$PORTABLE/"
# portable.txt beside capishot.exe switches on portable mode: settings, presets, recovery files
# and AI models go to CapishotData\ next to the exe (see apps/photocraft/src/app_dirs.rs).
cp "$HERE/portable.txt" "$PORTABLE/"
cp "$ROOT/README.md" "$PORTABLE/"
for f in LICENSE-MIT LICENSE-APACHE NOTICE ATTRIBUTION.md; do cp "$ROOT/$f" "$PORTABLE/licenses/"; done
cp "$ROOT/crates/typer/LICENSE-TypeR.txt" "$PORTABLE/licenses/"
cp "$ROOT/assets/app-icon/LICENSE.txt" "$PORTABLE/licenses/LICENSE-app-icon.txt"
copy_font_licences "$PORTABLE/licenses"
# The Rust crates linked into the binaries, with their licences.
(cd "$ROOT" && cargo metadata --format-version 1 --locked --filter-platform "$TARGET" \
  --features photocraft/heif,photocraft/ai,photocraft-cli/heif,photocraft-cli/ai) > "$WORK/metadata.json"
python3 -I - "$WORK/metadata.json" > "$PORTABLE/licenses/THIRD-PARTY-CRATES.txt" <<'EOF'
import json, sys
with open(sys.argv[1]) as fh:
    m = json.load(fh)
pkgs = {p["id"]: p for p in m["packages"]}
nodes = {n["id"]: n for n in m["resolve"]["nodes"]}
roots = [p["id"] for p in m["packages"] if p["name"] in ("photocraft", "photocraft-cli") and p["source"] is None]
seen, todo = set(), list(roots)
while todo:
    i = todo.pop()
    if i in seen:
        continue
    seen.add(i)
    # Normal dependencies only: build scripts and dev-dependencies aren't in the binaries.
    todo.extend(d["pkg"] for d in nodes[i]["deps"] if any(k["kind"] is None for k in d["dep_kinds"]))
print("Rust crates linked into capishot.exe and capishot-cli.exe: name, version, licence, source.")
print("Each crate's licence text is in its source package.\n")
for p in sorted((pkgs[i] for i in seen if pkgs[i]["source"] is not None), key=lambda p: (p["name"], p["version"])):
    lic = p.get("license") or "see " + str(p.get("license_file"))
    print(f"{p['name']} {p['version']}  {lic}  {p.get('repository') or ''}")
EOF
ZIP="$DIST/$NAME-portable.zip"
rm -f "$ZIP"
(cd "$WORK" && TZ=UTC zip -q -r -X -9 "$ZIP" "$NAME-portable")

# ---- MSI ---------------------------------------------------------------------------------------
if [ "$MSI" = 1 ]; then
  WIXL_SHARE="$(ls -d /usr/share/wixl-* 2>/dev/null | sort -V | tail -1 || true)"
  [ -n "$WIXL_SHARE" ] || { echo "error: wixl's data folder (/usr/share/wixl-*) not found: install msitools" >&2; exit 1; }
  # wixl's setup dialogs, with Capishot's art instead of the stock bitmaps: a 493x58 banner (the
  # page title is drawn over its left side, so that stays white) and a 493x312 backdrop for the
  # Welcome and Finish pages (their text sits on the right; the left 164 px are ours).
  rm -rf "$WORK/wixl-ext"
  cp -r "$WIXL_SHARE/ext" "$WORK/wixl-ext"
  python3 -I - "$ROOT/assets/app-icon/hicolor/256x256/apps/ai.storyteller.photocraft.png" "$WORK/wixl-ext/ui/bitmaps" <<'EOF'
import sys
from PIL import Image
icon = Image.open(sys.argv[1]).convert("RGBA")
out = sys.argv[2]
banner = Image.new("RGB", (493, 58), "white")
banner.paste(icon.resize((46, 46), Image.LANCZOS), (493 - 46 - 8, 6), icon.resize((46, 46), Image.LANCZOS))
banner.save(f"{out}/bannrbmp.bmp")
dialog = Image.new("RGB", (493, 312), "white")
dialog.paste((0xEF, 0xE9, 0xDC), (0, 0, 164, 312))
dialog.paste((0x2F, 0x7B, 0xF5), (160, 0, 164, 312))
big = icon.resize((124, 124), Image.LANCZOS)
dialog.paste(big, (18, 64), big)
dialog.save(f"{out}/dlgbmp.bmp")
EOF
  MSI_OUT="$DIST/$NAME.msi"
  rm -f "$MSI_OUT"
  wixl -a x64 --ext ui --extdir "$WORK/wixl-ext" \
    -D "Version=$MSI_VERSION" -D "DisplayVersion=$VERSION" -D "BinDir=$STAGE" \
    -D "IconPath=$ROOT/assets/app-icon/photocraft.ico" \
    -o "$MSI_OUT" "$HERE/capishot-wixl.wxs"
fi

# ---- checksums, update manifest, smoke test ----------------------------------------------------
FILES=("$NAME-portable.zip")
[ "$MSI" = 1 ] && FILES+=("$NAME.msi")
(cd "$DIST" && sha256sum "${FILES[@]}" > "$NAME.sha256")
BASE="${DOWNLOAD_BASE:-https://capichan.com/capishot/desktop/$VERSION/}"
python3 -I - "$DIST" "$VERSION" "$BASE" "$NAME" "$MSI" <<'EOF'
import hashlib, json, sys
dist, version, base, name, msi = sys.argv[1:]
base = base if base.endswith("/") else base + "/"
def sha(f):
    with open(f"{dist}/{f}", "rb") as fh:
        return hashlib.sha256(fh.read()).hexdigest()
zip_name, msi_name = f"{name}-portable.zip", f"{name}.msi"
manifest = {
    "version": version,
    "url_zip": base + zip_name,
    "url_msi": base + msi_name if msi == "1" else None,
    "sha256": {"zip": sha(zip_name), **({"msi": sha(msi_name)} if msi == "1" else {})},
    "notes": f"Capishot {version} for Windows.",
}
with open(f"{dist}/latest.json", "w") as fh:
    json.dump(manifest, fh, indent=2)
    fh.write("\n")
EOF
if [ "${SMOKE_WINE:-0}" = 1 ] && command -v wine >/dev/null; then
  WINEDEBUG=-all wine "$STAGE/capishot-cli.exe" --version
fi
(cd "$DIST" && ls -l "${FILES[@]}" latest.json && cat "$NAME.sha256")
