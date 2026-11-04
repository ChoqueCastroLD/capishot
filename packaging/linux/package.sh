#!/usr/bin/env bash
# Build and package Capishot for Linux x86_64:
#
#   $DIST/capishot-<version>-linux-x86_64.AppImage  any distro with glibc >= 2.35 (Ubuntu 22.04+)
#   $DIST/capishot-<version>-linux-x86_64.deb       Ubuntu, Debian, Mint, Pop!_OS, ...
#   $DIST/capishot-<version>-linux-x86_64.tar.gz    a folder to unpack anywhere (portable)
#   $DIST/capishot-<version>-linux-x86_64.sha256    sha256sum of the three
#   $DIST/latest-linux.json                         latest.json's Linux keys for this release
#
# Usage: packaging/linux/package.sh [--skip-build] [--formats "appimage deb tar"]
#
# The binaries (features heif,ai) are built in an Ubuntu 22.04 container (packaging/linux/Dockerfile,
# glibc 2.35), so they run on Ubuntu 22.04, Debian 12, Mint 21 and newer. Needs docker; the Rust
# toolchain is installed into $CARGO_TARGET_DIR/docker on first use. --skip-build packages the
# binaries already in $CARGO_TARGET_DIR/docker/target/release.
#
# AI: the GPU runs burn's wgpu path (Vulkan); the CPU runs ONNX Runtime's CPU provider from
# libonnxruntime.so beside the binaries (the official 1.24.4 build, SHA-256 checked below; loaded at
# run time, nothing links it), else burn's CPU backend.
#
# Each package puts a capishot.channel file beside the binaries (appimage, tar or deb): the app reads
# it to know how to update itself (crates/ui-egui/src/win_update.rs). Needs: docker, dpkg-deb,
# python3; optional desktop-file-validate and appstreamcli. DOWNLOAD_BASE (default
# https://capichan.com/capishot/desktop/<version>/) is where latest-linux.json says the files will be.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/linux"
# shellcheck source=verified-download.sh
. "$HERE/verified-download.sh"
APP_ID=com.capichan.capishot
ARCH=x86_64

SKIP_BUILD=0
FORMATS="appimage deb tar"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build) SKIP_BUILD=1; shift ;;
    --formats) FORMATS="$2"; shift 2 ;;
    -h | --help) sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[[ "$(uname -m)" = x86_64 ]] || { echo "error: x86_64 hosts only" >&2; exit 2; }
has() { case " $FORMATS " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }

BASENAME="capishot-$VERSION-linux-$ARCH"
DOCKER="$CARGO_TARGET_DIR/docker"
BIN="$DOCKER/target/release"
WORK="$CARGO_TARGET_DIR/linux-package"
echo "==> Capishot $VERSION for Linux $ARCH ($FORMATS)"

# ---- build in Ubuntu 22.04 ------------------------------------------------------------------------
if [[ "$SKIP_BUILD" = 0 ]]; then
  IMAGE=capishot-linux-build:22.04
  docker build -q -t "$IMAGE" "$HERE" >/dev/null
  TOOLCHAIN="$(awk -F'"' '/^rust-version/ { print $2; exit }' "$ROOT/Cargo.toml")"
  mkdir -p "$DOCKER/rustup" "$DOCKER/cargo" "$DOCKER/target"
  # The source is mounted at /src, so no host path ends up in the binaries. Fonts (CRAFT_FONTS_DIR)
  # are mounted read-only when set. The container runs as the calling user (files stay theirs).
  FONTS_ARGS=()
  if [[ -n "${CRAFT_FONTS_DIR:-}" ]]; then
    FONTS_ARGS=(-v "$CRAFT_FONTS_DIR:/fonts:ro" -e CRAFT_FONTS_DIR=/fonts)
  fi
  docker run --rm -u "$(id -u):$(id -g)" \
    -v "$ROOT:/src:ro" -v "$DOCKER/target:/target" -v "$DOCKER/rustup:/opt/rustup" -v "$DOCKER/cargo:/opt/cargo" \
    "${FONTS_ARGS[@]}" \
    -e CARGO_TARGET_DIR=/target -e HOME=/tmp -e CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
    -e PHOTOCRAFT_BUILD_SHA -e PHOTOCRAFT_BUILD_DATE \
    -w /src "$IMAGE" sh -euc "
      command -v cargo >/dev/null || curl -fsSL https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain stable --no-modify-path
      # At least the workspace's rust-version.
      have=\$(rustc --version | cut -d' ' -f2)
      [ \"\$(printf '%s\\n%s\\n' '$TOOLCHAIN' \"\$have\" | sort -V | head -1)\" = '$TOOLCHAIN' ] || rustup update stable
      rustc --version
      cargo build --release --locked -p photocraft -p photocraft-cli --features heif,ai --bins
      ldd --version | head -1"
fi
for b in capishot capishot-cli; do
  [[ -x "$BIN/$b" ]] || { echo "error: $BIN/$b not built" >&2; exit 1; }
done

# The newest glibc symbol the binaries need: 2.35 at most (Ubuntu 22.04).
python3 -I - "$BIN/capishot" "$BIN/capishot-cli" <<'EOF'
import re, subprocess, sys
for path in sys.argv[1:]:
    out = subprocess.run(["objdump", "-T", path], capture_output=True, text=True, check=True).stdout
    vers = {tuple(int(x) for x in v.split(".")) for v in re.findall(r"GLIBC_([0-9.]+)", out)}
    top = max(vers)
    if top > (2, 35):
        sys.exit(f"{path}: needs glibc {'.'.join(map(str, top))}, newer than Ubuntu 22.04's 2.35")
    print(f"ok {path.rsplit('/', 1)[-1]}: glibc >= {'.'.join(map(str, top))}")
EOF

rm -rf "$WORK"
mkdir -p "$WORK"

# ---- ONNX Runtime (CPU) ---------------------------------------------------------------------------
# The official release; a file whose SHA-256 differs stops the build. Bump both together (the digest
# GitHub publishes for the asset).
ORT_VERSION=1.24.4
ORT_SHA256=3a211fbea252c1e66290658f1b735b772056149f28321e71c308942cdb54b747
ORT_TGZ="$CARGO_TARGET_DIR/onnxruntime-linux-x64-$ORT_VERSION.tgz"
download_verified "https://github.com/microsoft/onnxruntime/releases/download/v$ORT_VERSION/onnxruntime-linux-x64-$ORT_VERSION.tgz" \
  "$ORT_TGZ" "$ORT_SHA256"
ORT_DIR="onnxruntime-linux-x64-$ORT_VERSION"
tar -C "$WORK" -xzf "$ORT_TGZ" "$ORT_DIR/lib/libonnxruntime.so.$ORT_VERSION" "$ORT_DIR/LICENSE" "$ORT_DIR/ThirdPartyNotices.txt"

# ---- the app folder (shared by every format) -------------------------------------------------------
# capishot, capishot-cli and libonnxruntime.so side by side (the app loads it from its own folder),
# with the licences.
APP="$WORK/app"
mkdir -p "$APP/licenses"
install -m755 "$BIN/capishot" "$BIN/capishot-cli" "$APP/"
strip "$APP/capishot" "$APP/capishot-cli"
install -m644 "$WORK/$ORT_DIR/lib/libonnxruntime.so.$ORT_VERSION" "$APP/libonnxruntime.so"
cp "$WORK/$ORT_DIR/LICENSE" "$APP/licenses/LICENSE-onnxruntime.txt"
cp "$WORK/$ORT_DIR/ThirdPartyNotices.txt" "$APP/licenses/ThirdPartyNotices-onnxruntime.txt"
for f in LICENSE-MIT LICENSE-APACHE NOTICE ATTRIBUTION.md; do cp "$ROOT/$f" "$APP/licenses/"; done
cp "$ROOT/crates/typer/LICENSE-TypeR.txt" "$APP/licenses/"
cp "$ROOT/assets/app-icon/LICENSE.txt" "$APP/licenses/LICENSE-app-icon.txt"
copy_font_licences "$APP/licenses"
# The Rust crates linked into the binaries, with their licences.
(cd "$ROOT" && cargo metadata --format-version 1 --locked --filter-platform x86_64-unknown-linux-gnu \
  --features photocraft/heif,photocraft/ai,photocraft-cli/heif,photocraft-cli/ai) >"$WORK/metadata.json"
python3 -I - "$WORK/metadata.json" >"$APP/licenses/THIRD-PARTY-CRATES.txt" <<'EOF'
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
    todo.extend(d["pkg"] for d in nodes[i]["deps"] if any(k["kind"] is None for k in d["dep_kinds"]))
print("Rust crates linked into capishot and capishot-cli: name, version, licence, source.")
print("Each crate's licence text is in its source package.\n")
for p in sorted((pkgs[i] for i in seen if pkgs[i]["source"] is not None), key=lambda p: (p["name"], p["version"])):
    lic = p.get("license") or "see " + str(p.get("license_file"))
    print(f"{p['name']} {p['version']}  {lic}  {p.get('repository') or ''}")
EOF

# The desktop files and icons (the icons in assets/ carry the upstream name; they go in under ours).
SHARE="$WORK/share"
install -Dm644 "$HERE/$APP_ID.desktop" "$SHARE/applications/$APP_ID.desktop"
install -Dm644 "$HERE/$APP_ID.mime.xml" "$SHARE/mime/packages/$APP_ID.xml"
mkdir -p "$SHARE/metainfo"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@DATE@/$PHOTOCRAFT_BUILD_DATE/g" "$HERE/$APP_ID.metainfo.xml.in" >"$SHARE/metainfo/$APP_ID.metainfo.xml"
(cd "$ROOT/assets/app-icon/hicolor" && find . -name 'ai.storyteller.photocraft.*') | while read -r rel; do
  install -Dm644 "$ROOT/assets/app-icon/hicolor/$rel" "$SHARE/icons/hicolor/${rel/ai.storyteller.photocraft/$APP_ID}"
done
if command -v desktop-file-validate >/dev/null; then desktop-file-validate "$SHARE/applications/$APP_ID.desktop"; fi
if command -v appstreamcli >/dev/null; then appstreamcli validate --no-net "$SHARE/metainfo/$APP_ID.metainfo.xml" || warn "appstreamcli found issues"; fi

# The app as the deb and the AppImage install it: /usr/lib/capishot, linked from /usr/bin (the app
# finds libonnxruntime.so through its own resolved path).
fhs() { # dest channel
  local dest="$1"
  mkdir -p "$dest/usr/bin" "$dest/usr/lib"
  cp -R "$APP" "$dest/usr/lib/capishot"
  echo "$2" >"$dest/usr/lib/capishot/capishot.channel"
  ln -s ../lib/capishot/capishot "$dest/usr/bin/capishot"
  ln -s ../lib/capishot/capishot-cli "$dest/usr/bin/capishot-cli"
  cp -R "$SHARE" "$dest/usr/share"
  mkdir -p "$dest/usr/share/doc/capishot"
  cp "$ROOT/README.md" "$dest/usr/share/doc/capishot/"
}

FILES=()

# ---- .tar.gz: one folder, unpacked anywhere -----------------------------------------------------------
if has tar; then
  T="$WORK/tar/$BASENAME"
  mkdir -p "$WORK/tar"
  cp -R "$APP" "$T"
  echo tar >"$T/capishot.channel"
  cp "$ROOT/README.md" "$T/"
  cp "$SHARE/applications/$APP_ID.desktop" "$T/"
  cp "$SHARE/icons/hicolor/256x256/apps/$APP_ID.png" "$T/"
  cat >"$T/README-linux.txt" <<'EOF'
Capishot for Linux (portable folder)

Run ./capishot (the editor) or ./capishot-cli (the command line). Settings go to
~/.config/capishot; put an empty portable.txt beside capishot to keep them in a CapishotData
folder here instead. Help > Check for Updates replaces this folder's files with the new version
(CapishotData is kept).

To add Capishot to the applications menu, copy com.capichan.capishot.desktop to
~/.local/share/applications, set its Exec= line to the full path of capishot here, and copy
com.capichan.capishot.png to ~/.local/share/icons/hicolor/256x256/apps.
EOF
  (cd "$WORK/tar" && tar --owner=0 --group=0 --numeric-owner -czf "$DIST/$BASENAME.tar.gz" "$BASENAME")
  FILES+=("$BASENAME.tar.gz")
  echo "wrote $DIST/$BASENAME.tar.gz"
fi

# ---- .deb -----------------------------------------------------------------------------------------
if has deb; then
  D="$WORK/deb"
  fhs "$D" deb
  mkdir -p "$D/DEBIAN"
  INSTALLED_KB="$(du -sk "$D/usr" | cut -f1)"
  cat >"$D/DEBIAN/control" <<EOF
Package: capishot
Version: $VERSION
Architecture: amd64
Maintainer: Capishot maintainers <noreply@capichan.com>
Installed-Size: $INSTALLED_KB
Depends: libc6 (>= 2.35), libgcc-s1, libstdc++6, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libx11-6, libx11-xcb1, libxcb1, libxcursor1, libxi6, libvulkan1 | libegl1
Recommends: mesa-vulkan-drivers, libegl-mesa0, xdg-desktop-portal
Section: graphics
Priority: optional
Homepage: https://capichan.com/capishot
Description: Image editor for scanlation (manga and comic pages)
 Open-source, native image editor with familiar, professional tools: layers,
 masks, type and brushes; opens and saves layered PSD/PSB files; AI upscaling
 and cleaning on the GPU (Vulkan) or the CPU.
 Includes capishot-cli, a headless converter and command runner.
EOF
  install -m755 "$HERE/postinst.sh" "$D/DEBIAN/postinst"
  install -m755 "$HERE/postinst.sh" "$D/DEBIAN/postrm"
  dpkg-deb --root-owner-group -Zxz --build "$D" "$DIST/$BASENAME.deb" >/dev/null
  FILES+=("$BASENAME.deb")
  echo "wrote $DIST/$BASENAME.deb"
fi

# ---- AppImage -------------------------------------------------------------------------------------
if has appimage; then
  APPDIR="$WORK/Capishot.AppDir"
  fhs "$APPDIR" appimage
  # AppRun installs the .desktop entry and icons into $XDG_DATA_HOME before launching, so Wayland
  # compositors resolve the window's app_id to our icon (packaging/linux/apprun-test.sh).
  install -Dm755 "$HERE/AppRun" "$APPDIR/AppRun"
  cp "$HERE/$APP_ID.desktop" "$APPDIR/$APP_ID.desktop"
  cp "$SHARE/icons/hicolor/256x256/apps/$APP_ID.png" "$APPDIR/$APP_ID.png"
  ln -s "$APP_ID.png" "$APPDIR/.DirIcon"
  # Digests from the official GitHub release asset metadata:
  # https://github.com/AppImage/appimagetool/releases/tag/1.9.1
  # https://github.com/AppImage/type2-runtime/releases/tag/20251108
  TOOL_VERSION=1.9.1
  RUNTIME_VERSION=20251108
  TOOL_SHA256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
  RUNTIME_SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
  TOOL="$CARGO_TARGET_DIR/appimagetool-$TOOL_VERSION-$ARCH.AppImage"
  download_verified "https://github.com/AppImage/appimagetool/releases/download/$TOOL_VERSION/appimagetool-$ARCH.AppImage" "$TOOL" "$TOOL_SHA256" true
  RUNTIME="$CARGO_TARGET_DIR/type2-runtime-$RUNTIME_VERSION-$ARCH"
  download_verified "https://github.com/AppImage/type2-runtime/releases/download/$RUNTIME_VERSION/runtime-$ARCH" "$RUNTIME" "$RUNTIME_SHA256"
  OUT="$(cd "$DIST" && pwd)/$BASENAME.AppImage"
  # Extract-and-run: works without FUSE (containers). The output embeds the static runtime, so users
  # don't need libfuse2 either. No zsync update information: the app updates itself (latest.json).
  (cd "$WORK" && ARCH="$ARCH" APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream --runtime-file "$RUNTIME" "$APPDIR" "$OUT" >/dev/null)
  FILES+=("$BASENAME.AppImage")
  echo "wrote $OUT"
fi

# ---- checksums, latest.json's Linux keys, smoke test -------------------------------------------------
(cd "$DIST" && sha256sum "${FILES[@]}" >"$BASENAME.sha256")
BASE="${DOWNLOAD_BASE:-https://capichan.com/capishot/desktop/$VERSION/}"
python3 -I - "$DIST" "$BASE" "${FILES[@]}" <<'EOF'
import hashlib, json, os, sys
dist, base, *files = sys.argv[1:]
base = base if base.endswith("/") else base + "/"
keys = {".AppImage": "appimage", ".deb": "deb", ".tar.gz": "tar"}
out = {"sha256": {}, "size": {}}
for f in files:
    key = next(k for ext, k in keys.items() if f.endswith(ext))
    with open(f"{dist}/{f}", "rb") as fh:
        out["sha256"][key] = hashlib.sha256(fh.read()).hexdigest()
    out["size"][key] = os.path.getsize(f"{dist}/{f}")
    out[f"url_{key}"] = base + f
with open(f"{dist}/latest-linux.json", "w") as fh:
    json.dump(out, fh, indent=2)
    fh.write("\n")
EOF
"$APP/capishot-cli" --version
echo "==> done"
(cd "$DIST" && ls -l "${FILES[@]}" latest-linux.json && cat "$BASENAME.sha256")
