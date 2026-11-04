#!/usr/bin/env bash
# Start the packaged GUI (the tar.gz's capishot) in an Ubuntu 24.04 container on Xvfb with Mesa's
# software Vulkan (lavapipe) for SECONDS (default 30) and fail on a crash, an early exit or an
# ERROR/panic line in its log. Usage: packaging/linux/gui-test.sh [<dist folder>]
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
DIR="$(cd "${1:-$DIST}" && pwd)"
BASENAME="capishot-$VERSION-linux-x86_64"
[[ -f "$DIR/$BASENAME.tar.gz" ]] || { echo "error: $DIR/$BASENAME.tar.gz missing" >&2; exit 1; }
docker run --rm -v "$DIR:/dist:ro" -e DEBIAN_FRONTEND=noninteractive -e B="$BASENAME" -e SECS="${SECONDS_TO_RUN:-30}" ubuntu:24.04 bash -euc '
  apt-get update -qq >/dev/null
  apt-get install -y -qq --no-install-recommends xvfb xauth mesa-vulkan-drivers libvulkan1 libxkbcommon-x11-0 libx11-xcb1 libxcursor1 libxi6 libxrandr2 libegl1 libgl1 >/dev/null
  tar -C /tmp -xzf "/dist/$B.tar.gz"
  export HOME=/tmp/home RUST_LOG=info VK_ICD_FILENAMES=$(ls /usr/share/vulkan/icd.d/lvp_icd*.json | head -1)
  mkdir -p "$HOME"
  xvfb-run -a -s "-screen 0 1600x1000x24" "/tmp/$B/capishot" >/tmp/gui.log 2>&1 &
  pid=$!
  sleep "$SECS"
  if ! kill -0 "$pid" 2>/dev/null; then echo "the app exited early"; tail -40 /tmp/gui.log; exit 1; fi
  pkill -TERM -f "/tmp/$B/capishot" || true
  wait "$pid" || true
  grep -E "adapter|wgpu|Vulkan|llvmpipe" /tmp/gui.log | head -5 || true
  if grep -E "panicked|\] ERROR|ERROR " /tmp/gui.log; then echo "errors in the log"; exit 1; fi
  echo "ok gui: ran ${SECS}s on Xvfb + lavapipe, no errors ($(wc -l </tmp/gui.log) log lines)"
'
