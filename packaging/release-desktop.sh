#!/usr/bin/env bash
# Build every desktop release artifact (Windows x64 and Linux x86_64) into one folder, with
# checksums and the combined update manifest. Nothing is uploaded or published.
#
#   $DIST/capishot-<version>-windows-x64.msi, -windows-x64-portable.zip, -windows-x64.sha256
#   $DIST/capishot-<version>-linux-x86_64.AppImage, .deb, .tar.gz, -linux-x86_64.sha256
#   $DIST/SHA256SUMS      every file above
#   $DIST/latest.json     the manifest both apps read (Windows keys as before, plus the Linux ones;
#                         latest-linux.json holds the Linux part)
#
# Usage: packaging/release-desktop.sh [--skip-build] [--only windows|linux]
#
# The version comes from Cargo.toml (packaging/env.sh). Windows: packaging/windows/package-linux.sh
# (cargo-xwin, wixl; XWIN_ACCEPT_LICENSE=1). Linux: packaging/linux/package.sh (docker).
# DOWNLOAD_BASE, RELEASE_NOTES, NOTES_URL and MIN_VERSION are passed through; DOWNLOAD_PAGE
# (default https://capichan.com/capishot/) is the page .deb installs open to update.
# --only merges the other OS's earlier output in $DIST into latest.json when it is there.
set -euo pipefail
# shellcheck source=env.sh
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"

SKIP=()
ONLY=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build) SKIP=(--skip-build); shift ;;
    --only) ONLY="$2"; shift 2 ;;
    -h | --help) sed -n '2,18p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
echo "==> Capishot $VERSION desktop release into $DIST"

if [[ "$ONLY" != linux ]]; then
  "$ROOT/packaging/windows/package-linux.sh" "${SKIP[@]}"
fi
if [[ "$ONLY" != windows ]]; then
  "$ROOT/packaging/linux/package.sh" "${SKIP[@]}"
fi

# latest.json: the Windows manifest (package-linux.sh writes it) plus latest-linux.json's keys.
# Windows copies ignore the Linux keys (crates/ui-egui/src/update_notice.rs).
python3 -I - "$DIST" "$VERSION" <<'EOF'
import json, os, sys
dist, version = sys.argv[1:]
def load(name):
    path = f"{dist}/{name}"
    if not os.path.exists(path):
        return None
    with open(path) as fh:
        return json.load(fh)
win, linux = load("latest.json"), load("latest-linux.json")
if win is not None and win.get("version") != version:
    sys.exit(f"latest.json is for {win.get('version')}, not {version}: build Windows again")
m = dict(win or {"version": version, "notes": os.environ.get("RELEASE_NOTES") or f"Capishot {version}."})
if win is not None and not os.environ.get("RELEASE_NOTES"):
    m["notes"] = f"Capishot {version} for Windows and Linux."
if linux is not None:
    for key in ("appimage", "deb", "tar"):
        if f"url_{key}" in linux:
            if not linux[f"url_{key}"].rsplit("/", 1)[-1].startswith(f"capishot-{version}-"):
                sys.exit(f"latest-linux.json is not for {version}: build Linux again")
            m[f"url_{key}"] = linux[f"url_{key}"]
            m.setdefault("sha256", {})[key] = linux["sha256"][key]
            m.setdefault("size", {})[key] = linux["size"][key]
    m["download_page"] = os.environ.get("DOWNLOAD_PAGE") or "https://capichan.com/capishot/"
with open(f"{dist}/latest.json", "w") as fh:
    json.dump(m, fh, indent=2)
    fh.write("\n")
EOF
(cd "$DIST" && ls capishot-"$VERSION"-* | grep -v '\.sha256$' | xargs sha256sum >SHA256SUMS)
echo "==> $DIST"
(cd "$DIST" && ls -l && cat latest.json)
