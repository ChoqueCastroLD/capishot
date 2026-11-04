#!/usr/bin/env bash
# Smoke-test the Linux packages package.sh wrote, in clean Ubuntu 22.04 and 24.04 containers:
# the .deb installs with apt; capishot-cli from the .deb, the AppImage and the tar.gz prints its
# version and converts PNG -> PSD -> JPG; the AppImage and the tar.gz update themselves from a
# "newer" copy (the helper the app runs after Restart and Update) and keep the user's settings.
# The GUI under Xvfb is gui-test.sh.
#
# Usage: packaging/linux/smoke-test.sh [<dist folder>]   (default $DIST)
# Needs docker and python3 (a test PNG is generated).
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
DIR="$(cd "${1:-$DIST}" && pwd)"
BASENAME="capishot-$VERSION-linux-x86_64"
for f in AppImage deb tar.gz; do
  [[ -f "$DIR/$BASENAME.$f" ]] || { echo "error: $DIR/$BASENAME.$f missing" >&2; exit 1; }
done
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
# A 64x48 RGB gradient PNG.
python3 -I - "$WORK/in.png" <<'EOF'
import struct, sys, zlib
w, h = 64, 48
raw = b"".join(b"\0" + bytes(v for x in range(w) for v in (x * 4, y * 5, 128)) for y in range(h))
def chunk(t, d):
    return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
EOF
cat >"$WORK/test.sh" <<'EOF'
#!/bin/sh
set -eu
B="$1"
cd /tmp
convert() { # cli label
  "$1" --version
  "$1" convert /work/in.png "/tmp/$2.psd"
  "$1" convert "/tmp/$2.psd" "/tmp/$2.jpg"
  head -c 4 "/tmp/$2.psd" | grep -q 8BPS
  [ "$(head -c 3 "/tmp/$2.jpg" | od -An -tx1 | tr -d ' ')" = ffd8ff ]
  echo "ok $2: PNG -> PSD -> JPG"
}
apt-get update -qq >/dev/null
apt-get install -y -qq "/dist/$B.deb" >/dev/null
[ "$(readlink -f /usr/bin/capishot)" = /usr/lib/capishot/capishot ]
[ "$(cat /usr/lib/capishot/capishot.channel)" = deb ]
[ -f /usr/share/applications/com.capichan.capishot.desktop ]
[ -f /usr/share/icons/hicolor/256x256/apps/com.capichan.capishot.png ]
convert capishot-cli deb
# The GUI binary starts and answers --version without a display.
capishot --version
cp "/dist/$B.AppImage" /tmp/Capishot.AppImage
APPIMAGE_EXTRACT_AND_RUN=1 /tmp/Capishot.AppImage --version
# The CLI inside the AppImage (no FUSE in a container: extract it).
(cd /tmp && /tmp/Capishot.AppImage --appimage-extract >/dev/null)
convert /tmp/squashfs-root/usr/bin/capishot-cli appimage
tar -C /tmp -xzf "/dist/$B.tar.gz"
convert "/tmp/$B/capishot-cli" tar
# ONNX Runtime loads (the CPU AI path) and links nothing missing.
if ldd "/tmp/$B/libonnxruntime.so" | grep -q 'not found'; then ldd "/tmp/$B/libonnxruntime.so"; exit 1; fi
# Restart and Update, as the app runs it after exiting: a copy of the binary in the updates folder
# runs `--apply-update <kind> <new> --pid <the app's, gone> --exe <the copy to replace> ...`, then
# starts the app again (it can't open a window here; only the files are checked).
U=/tmp/home/.cache/capishot/updates
mkdir -p "$U" /tmp/home/.config/capishot
echo '{"mine": true}' >/tmp/home/.config/capishot/prefs.json
cp /tmp/squashfs-root/usr/lib/capishot/capishot "$U/capishot-update-helper"
# AppImage: the new file replaces $APPIMAGE.
mkdir -p /tmp/apps && cp /tmp/Capishot.AppImage /tmp/apps/Capishot.AppImage
cp /tmp/Capishot.AppImage "$U/new.AppImage" && printf 'new' >>"$U/new.AppImage"
HOME=/tmp/home "$U/capishot-update-helper" --apply-update appimage "$U/new.AppImage" --pid 999999 --exe /tmp/apps/Capishot.AppImage --version 9.9.9 --dir "$U" || true
cmp "$U/new.AppImage" /tmp/apps/Capishot.AppImage
[ -x /tmp/apps/Capishot.AppImage ]
grep -q '"ok": true' "$U/last-update.json" && rm "$U/last-update.json"
APPIMAGE_EXTRACT_AND_RUN=1 /tmp/apps/Capishot.AppImage --version
echo "ok appimage update: replaced in place, still runs"
# tar.gz: the new folder's files over the old one's, CapishotData kept.
mkdir -p /tmp/old && tar -C /tmp/old -xzf "/dist/$B.tar.gz"
OLD="/tmp/old/$B"
touch "$OLD/portable.txt" && mkdir -p "$OLD/CapishotData" && echo '{"portable": true}' >"$OLD/CapishotData/prefs.json"
mkdir -p "$U/new" && tar -C "$U/new" -xzf "/dist/$B.tar.gz"
echo new >"$U/new/$B/README-linux.txt"
HOME=/tmp/home "$U/capishot-update-helper" --apply-update tar "$U/new/$B" --pid 999999 --exe "$OLD/capishot" --version 9.9.9 --dir "$U" || true
grep -q '"ok": true' "$U/last-update.json"
[ "$(cat "$OLD/README-linux.txt")" = new ]
[ "$(cat "$OLD/CapishotData/prefs.json")" = '{"portable": true}' ] && [ -f "$OLD/portable.txt" ]
"$OLD/capishot-cli" --version
[ "$(cat /tmp/home/.config/capishot/prefs.json)" = '{"mine": true}' ]
echo "ok tar update: files swapped, CapishotData and ~/.config/capishot kept"
apt-get remove -y -qq capishot >/dev/null
[ ! -e /usr/bin/capishot ]
echo "ok: $(. /etc/os-release && echo "$PRETTY_NAME")"
EOF
chmod +x "$WORK/test.sh"
for img in ubuntu:22.04 ubuntu:24.04; do
  echo "==> $img"
  docker run --rm -v "$DIR:/dist:ro" -v "$WORK:/work:ro" -e DEBIAN_FRONTEND=noninteractive "$img" /work/test.sh "$BASENAME"
done
