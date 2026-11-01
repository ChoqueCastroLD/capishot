#!/usr/bin/env bash
# Cross-build and package Capishot for Windows x64 on Linux (no Windows machine needed):
#
#   $DIST/capishot-<version>-windows-x64-portable.zip  capishot.exe, capishot-cli.exe, portable.txt
#                                                      and licenses/
#   $DIST/capishot-<version>-windows-x64.msi           per-machine installer (wixl)
#   $DIST/capishot-<version>-windows-x64.sha256        sha256sum of both
#   $DIST/latest.json                                  the update manifest for this release
#
# Usage: packaging/windows/package-linux.sh [--skip-build] [--no-msi] [--no-directml]
#
# Both packages carry onnxruntime.dll and DirectML.dll beside capishot.exe (the GPU AI models
# on Windows, DirectML), from the pinned NuGet packages below, checked against their SHA-256,
# with their licences in licenses/. --no-directml leaves them out.
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
# says the files will be published; nothing is uploaded here. RELEASE_NOTES, NOTES_URL and
# MIN_VERSION fill latest.json's notes, notes_url and min_version.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/windows"
TARGET=x86_64-pc-windows-msvc

SKIP_BUILD=0
MSI=1
DIRECTML=1
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-build) SKIP_BUILD=1; shift ;;
    --no-msi) MSI=0; shift ;;
    --no-directml) DIRECTML=0; shift ;;
    -h | --help) sed -n '2,27p' "$0"; exit 0 ;;
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

# ---- DirectML runtime (onnxruntime.dll + DirectML.dll) ------------------------------------------
# Pinned NuGet packages; a package whose SHA-256 differs stops the build. Bump the version and
# the checksum together (sha256sum of the .nupkg).
ORT_VERSION=1.24.4
ORT_SHA256=57e9f11b73437bef7a309496135d4c1f96b1a8e9ddba60013fa27bfc1d788681
DML_VERSION=1.15.4
DML_SHA256=4e7cb7ddce8cf837a7a75dc029209b520ca0101470fcdf275c1f49736a3615b9
DML_LICENSES="$WORK/directml-licenses"
rm -rf "$DML_LICENSES"
if [ "$DIRECTML" = 1 ]; then
  NUGET="$WORK/nuget"
  mkdir -p "$NUGET" "$DML_LICENSES"
  fetch_nupkg() { # id version sha256 -> path
    local id="$1" ver="$2" sum="$3" lower
    lower="$(echo "$id" | tr '[:upper:]' '[:lower:]')"
    local out="$NUGET/$lower.$ver.nupkg"
    if [ ! -f "$out" ] || ! echo "$sum  $out" | sha256sum -c --quiet - 2>/dev/null; then
      curl -fsSL --retry 3 -o "$out.part" "https://api.nuget.org/v3-flatcontainer/$lower/$ver/$lower.$ver.nupkg"
      mv "$out.part" "$out"
    fi
    echo "$sum  $out" | sha256sum -c --quiet - || { echo "error: $id $ver: SHA-256 mismatch" >&2; exit 1; }
    echo "$out"
  }
  ORT_PKG="$(fetch_nupkg Microsoft.ML.OnnxRuntime.DirectML "$ORT_VERSION" "$ORT_SHA256")"
  DML_PKG="$(fetch_nupkg Microsoft.AI.DirectML "$DML_VERSION" "$DML_SHA256")"
  unzip -p "$ORT_PKG" runtimes/win-x64/native/onnxruntime.dll > "$STAGE/onnxruntime.dll"
  unzip -p "$DML_PKG" bin/x64-win/DirectML.dll > "$STAGE/DirectML.dll"
  unzip -p "$ORT_PKG" LICENSE > "$DML_LICENSES/LICENSE-onnxruntime.txt"
  unzip -p "$ORT_PKG" ThirdPartyNotices.txt > "$DML_LICENSES/ThirdPartyNotices-onnxruntime.txt"
  unzip -p "$DML_PKG" LICENSE.txt > "$DML_LICENSES/LICENSE-DirectML.txt"
  unzip -p "$DML_PKG" ThirdPartyNotices.txt > "$DML_LICENSES/ThirdPartyNotices-DirectML.txt"
  python3 -I - "$STAGE/onnxruntime.dll" "$STAGE/DirectML.dll" <<'EOF2'
import struct, sys
for path in sys.argv[1:]:
    with open(path, "rb") as f:
        head = f.read(4096)
    pe = struct.unpack_from("<I", head, 0x3C)[0]
    if head[pe:pe + 4] != b"PE\0\0" or struct.unpack_from("<H", head, pe + 4)[0] != 0x8664:
        sys.exit(f"{path}: not an x64 DLL")
    print(f"ok {path.rsplit('/', 1)[-1]}: x64")
EOF2
  echo "DirectML runtime: onnxruntime $ORT_VERSION, DirectML $DML_VERSION"
fi

# ---- portable zip ------------------------------------------------------------------------------
PORTABLE="$WORK/$NAME-portable"
rm -rf "$PORTABLE"
mkdir -p "$PORTABLE/licenses"
cp "$STAGE"/*.exe "$PORTABLE/"
# portable.txt beside capishot.exe switches on portable mode: settings, presets, recovery files
# and AI models go to CapishotData\ next to the exe (see apps/photocraft/src/app_dirs.rs).
cp "$HERE/portable.txt" "$PORTABLE/"
[ "$DIRECTML" = 1 ] && cp "$DML_LICENSES"/*.txt "$PORTABLE/licenses/"
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
  # The DirectML runtime's component, when the packages carry it (wixl has no FeatureRef or
  # conditional sections: the markers in the source are filled in).
  WXS="$WORK/capishot.wxs"
  python3 -I - "$HERE/capishot-wixl.wxs" "$WXS" "$DIRECTML" "$DML_LICENSES" <<'EOF'
import sys
src, out, on, lic = sys.argv[1:]
s = open(src, encoding="utf-8").read()
if on == "1":
    names = ["onnxruntime.dll", "DirectML.dll", "LICENSE-onnxruntime.txt", "ThirdPartyNotices-onnxruntime.txt", "LICENSE-DirectML.txt", "ThirdPartyNotices-DirectML.txt"]
    rows = ""
    for k, n in enumerate(names):
        d = "$(var.BinDir)" if n.endswith(".dll") else lic
        key = ' KeyPath="yes"' if k == 0 else ""
        rows += f'<File Id="DirectML{k}" Name="{n}" Source="{d}/{n}"{key} />'
    comp = f'<Component Id="CapishotDirectML" Guid="3F0C2B7E-5D4A-4C1E-9B8F-6A2D7E1C4B90" Win64="yes">{rows}</Component>'
    s = s.replace("<!-- DIRECTML_REF:", '<ComponentRef Id="CapishotDirectML" /><!-- DIRECTML_REF:', 1)
    s = s.replace("<!-- DIRECTML_COMPONENT:", comp + "<!-- DIRECTML_COMPONENT:", 1)
open(out, "w", encoding="utf-8").write(s)
EOF
  MSI_OUT="$DIST/$NAME.msi"
  rm -f "$MSI_OUT"
  wixl -a x64 --ext ui --extdir "$WORK/wixl-ext" \
    -D "Version=$MSI_VERSION" -D "DisplayVersion=$VERSION" -D "BinDir=$STAGE" \
    -D "IconPath=$ROOT/assets/app-icon/photocraft.ico" \
    -o "$MSI_OUT" "$WXS"
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
import os
zip_name, msi_name = f"{name}-portable.zip", f"{name}.msi"
def size(f):
    return os.path.getsize(f"{dist}/{f}")
manifest = {
    "version": version,
    "url_zip": base + zip_name,
    "url_msi": base + msi_name if msi == "1" else None,
    # The app downloads these itself and refuses a file whose checksum differs.
    "sha256": {"zip": sha(zip_name), **({"msi": sha(msi_name)} if msi == "1" else {})},
    "size": {"zip": size(zip_name), **({"msi": size(msi_name)} if msi == "1" else {})},
    "notes": os.environ.get("RELEASE_NOTES") or f"Capishot {version} for Windows.",
}
# Optional: older versions must update (no "Skip This Version"), and a page with the notes.
if os.environ.get("MIN_VERSION"):
    manifest["min_version"] = os.environ["MIN_VERSION"]
if os.environ.get("NOTES_URL"):
    manifest["notes_url"] = os.environ["NOTES_URL"]
with open(f"{dist}/latest.json", "w") as fh:
    json.dump(manifest, fh, indent=2)
    fh.write("\n")
EOF
if [ "${SMOKE_WINE:-0}" = 1 ] && command -v wine >/dev/null; then
  WINEDEBUG=-all wine "$STAGE/capishot-cli.exe" --version
fi
(cd "$DIST" && ls -l "${FILES[@]}" latest.json && cat "$NAME.sha256")
