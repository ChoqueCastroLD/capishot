#!/usr/bin/env bash
# Build everything the web deploy serves, as fast as the machine allows:
#   dist/web          single-threaded build (Trunk, apps/photocraft-web), with the AI worker
#   dist/web-threads  multithreaded build (packaging/web-threads), with both AI workers
# The same output as running, one after the other,
#   (cd apps/photocraft-web && trunk build --release)
#   PHOTOCRAFT_WEB_FALLBACK=st/ packaging/web-threads/build.sh
#   packaging/web-ai/build.sh dist/web
#   packaging/web-ai/build.sh dist/web-threads --threads
# but:
# * the three builds (st, threads, AI workers) run at the same time: each ends in a fat-LTO link
#   and wasm-opt, which run on one core, so in sequence most of the machine idles;
# * the AI workers are built once (not once per site) and cached by the content of everything
#   they are built from (their path crates, Cargo.lock, Cargo.toml, toolchains, tools): they
#   rarely change, and a cache hit skips their two fat-LTO links and wasm-opt -O3 runs.
#
#   packaging/web-release.sh            -> dist/web, dist/web-threads; per-stage logs and times
#   packaging/web-release.sh --preview  the same with thin LTO and 16 codegen units for both
#                                       sites: each module's link takes ~1.5 min instead of
#                                       ~6.5, but the wasm is ~10% bigger (measured: 32.9 -> 36.2
#                                       MB optimized, gzip +9%; the first-load part after the
#                                       split 14.6 -> 18.2 MB) and wasm-opt takes twice as long.
#                                       For quick test deploys only, never a release: it is also
#                                       5-19% slower at run time (packaging/web-threads/README.md).
#                                       Switching between the two recompiles every crate once.
#
# Fails (non-zero, the other builds stopped) as soon as any stage fails: never deploy after a
# failure. Environment:
#   PHOTOCRAFT_WEB_JOBS      cores to use in total (default 10), split between the builds
#   PHOTOCRAFT_WEB_MEM_GB    available memory needed to build both sites at once (default 24,
#                            8 with --preview; a fat-LTO link peaks near 10 GB, a thin one near
#                            3.5 GB); below it they build in turn
#   CARGO_TARGET_DIR         as for cargo (default target/); logs go to <it>/web-release/logs
#   PHOTOCRAFT_WEB_FALLBACK  as for packaging/web-threads/build.sh (default here: st/)
#   PHOTOCRAFT_WEB_AI=0      no AI workers;  PHOTOCRAFT_AI_CACHE=0  rebuild them regardless
#   PHOTOCRAFT_AI_MODELS_URL, PHOTOCRAFT_WEB_FEATURES, PHOTOCRAFT_WEB_SPLIT: passed through.
# Needs what the three scripts need (packaging/web-threads/README.md).
set -euo pipefail
case "${1:-}" in
  "") ;;
  --preview)
    export CARGO_PROFILE_WASM_RELEASE_LTO=thin CARGO_PROFILE_WASM_RELEASE_CODEGEN_UNITS=16
    echo "[web-release] PREVIEW build (thin LTO): bigger wasm, not for a release" ;;
  *) echo "usage: packaging/web-release.sh [--preview]" >&2; exit 1 ;;
esac
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
JOBS="${PHOTOCRAFT_WEB_JOBS:-10}"
case "$JOBS" in '' | *[!0-9]* | 0) echo "error: PHOTOCRAFT_WEB_JOBS must be a positive number" >&2; exit 1 ;; esac
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
export PHOTOCRAFT_WEB_FALLBACK="${PHOTOCRAFT_WEB_FALLBACK-st/}"
WR="$CARGO_TARGET_DIR/web-release"
LOGS="$WR/logs"
mkdir -p "$LOGS"
rm -f "$LOGS"/*.log
T0=$(date +%s)
STAMP="$WR/started"
touch "$STAMP"
el() { echo $(( $(date +%s) - $1 )); }
log() { echo "[web-release $(date +%T)] $*"; }

# Both site builds download the pinned binaryen (wasm-split) on first use: once, before they race.
# shellcheck source=web-split/binaryen.sh
. "$ROOT/packaging/web-split/binaryen.sh"
binaryen_tool wasm-split >/dev/null
binaryen_tool wasm-opt >/dev/null

# wasm-opt, cached: the same input bytes with the same arguments give the output of an earlier
# run (wasm-opt is deterministic), so a rebuild whose module didn't change (a change outside the
# Rust code, or none) skips its minute of -Oz. Only with wasm-opt on PATH (then Trunk and the
# threads build both already use it); a cache in <target>/web-release/wasm-opt, 3 days.
if REAL_WASM_OPT=$(command -v wasm-opt); then
  mkdir -p "$WR/bin" "$WR/wasm-opt"
  find "$WR/wasm-opt" -type f -mtime +3 -delete
  cat >"$WR/bin/wasm-opt.tmp" <<EOF
#!/usr/bin/env bash
# Written by packaging/web-release.sh: $REAL_WASM_OPT with an output cache.
set -euo pipefail
REAL='$REAL_WASM_OPT' CACHE='$WR/wasm-opt'
EOF
  cat >>"$WR/bin/wasm-opt.tmp" <<'EOF'
out="" prev="" key=()
for a in "$@"; do
  if [ -n "$prev" ]; then out=$a prev=""; key+=(OUT); continue; fi
  case "$a" in
    -o | --output) prev=$a; key+=("$a"); continue ;;
    --output=*) out=${a#--output=}; key+=(--output=OUT); continue ;;
    *source-map* | --version | --help) exec "$REAL" "$@" ;;
  esac
  if [ -f "$a" ]; then key+=("$(sha256sum <"$a" | cut -c1-64)"); else key+=("$a"); fi
done
[ -n "$out" ] || exec "$REAL" "$@"
k=$( { "$REAL" --version; printf '%s
' "${key[@]}"; } | sha256sum | cut -c1-64)
if [ -s "$CACHE/$k" ]; then
  cp "$CACHE/$k" "$out"
  touch "$CACHE/$k"
  echo "wasm-opt: cached output reused" >&2
  exit 0
fi
"$REAL" "$@"
tmp=$(mktemp "$CACHE/.tmp.XXXXXX")
cp "$out" "$tmp" && mv -f "$tmp" "$CACHE/$k"
EOF
  chmod +x "$WR/bin/wasm-opt.tmp"
  mv -f "$WR/bin/wasm-opt.tmp" "$WR/bin/wasm-opt"
  export PATH="$WR/bin:$PATH"
fi

# --- AI workers: cache key -----------------------------------------------------------------------
AI=1
[ "${PHOTOCRAFT_WEB_AI:-1}" = "0" ] && AI=0
AI_KEY="" AI_DIR=""
if [ "$AI" = 1 ]; then
  # Everything the worker build reads: its workspace path crates (from cargo's resolve, so a new
  # dependency counts), the lock file and manifests (profiles), cargo config, the build script,
  # the toolchains and tools, and the environment it reads.
  tools="$(rustc -vV)
$(cargo "+${PHOTOCRAFT_AI_THREADS_TOOLCHAIN:-stable}" -V 2>&1)
$(rustc "+${PHOTOCRAFT_AI_THREADS_TOOLCHAIN:-stable}" -vV 2>&1)
$(wasm-bindgen --version 2>&1 || true)
$(wasm-opt --version 2>&1 || true)
${PHOTOCRAFT_AI_PROFILE:-}|${PHOTOCRAFT_AI_THREADS_TOOLCHAIN:-}|${PHOTOCRAFT_WEB_AI_THREADS:-}"
  AI_KEY=$(cargo metadata --format-version 1 --filter-platform wasm32-unknown-unknown --locked 2>/dev/null \
    | AI_TOOLS="$tools" python3 -c '
import hashlib, json, os, subprocess, sys
m = json.load(sys.stdin)
root = m["workspace_root"]
pkgs = {p["id"]: p for p in m["packages"]}
nodes = {n["id"]: n for n in m["resolve"]["nodes"]}
start = next(p["id"] for p in m["packages"] if p["name"] == "photocraft-ai-worker" and p["source"] is None)
seen, todo = set(), [start]
while todo:
    i = todo.pop()
    if i in seen:
        continue
    seen.add(i)
    todo += [d["pkg"] for d in nodes[i]["deps"]
             if any(k["kind"] in (None, "build") for k in d["dep_kinds"])]
dirs = sorted({os.path.relpath(os.path.dirname(pkgs[i]["manifest_path"]), root)
               for i in seen if pkgs[i]["source"] is None})
files = subprocess.run(["git", "ls-files", "-co", "--exclude-standard", "-z", "--", *dirs,
                        "Cargo.lock", "Cargo.toml", ".cargo", "packaging/web-ai/build.sh"],
                       cwd=root, capture_output=True, check=True).stdout.split(b"\0")
h = hashlib.sha256(os.environ["AI_TOOLS"].encode())
for f in sorted(x for x in files if x):
    p = os.path.join(root.encode(), f)
    if os.path.isfile(p):
        h.update(f + b"\0" + hashlib.sha256(open(p, "rb").read()).digest())
print(h.hexdigest()[:24])
') || { echo "error: could not hash the AI worker's inputs (cargo metadata)" >&2; exit 1; }
  AI_DIR="$WR/ai/$AI_KEY"
fi

# --- the builds, concurrently --------------------------------------------------------------------
# One core each for the fat-LTO links; the rest of the share for compiling crates.
if [ "$AI" = 1 ] && { [ "${PHOTOCRAFT_AI_CACHE:-1}" = "0" ] || [ ! -f "$AI_DIR/done" ]; }; then
  BUILD_AI=1
  J_SITE=$(( JOBS * 2 / 5 )) J_AI=$(( JOBS / 10 ))
else
  BUILD_AI=0
  J_SITE=$(( JOBS / 2 )) J_AI=1
fi
[ "$J_SITE" -ge 1 ] || J_SITE=1
[ "$J_AI" -ge 1 ] || J_AI=1

declare -A PIDS=()
# run <name> <jobs> <command...>: in the background, its own process group (to stop it whole),
# output in $LOGS/<name>.log.
run() {
  local name=$1 jobs=$2
  shift 2
  log "$name: started (-j $jobs, log $LOGS/$name.log)"
  CARGO_BUILD_JOBS=$jobs setsid bash -c 's=$(date +%s); "$@"; r=$?; echo "== exit $r after $(( $(date +%s) - s ))s"; exit $r' \
    "$name" "$@" >"$LOGS/$name.log" 2>&1 &
  PIDS[$!]=$name
}
stop_all() {
  local p
  for p in "${!PIDS[@]}"; do kill -TERM -- "-$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
}
trap 'stop_all; exit 130' INT TERM
# wait_all: until every job ends; on the first failure, stop the rest and exit.
wait_all() {
  local pid r
  while [ ${#PIDS[@]} -gt 0 ]; do
    r=0
    wait -n -p pid "${!PIDS[@]}" || r=$?
    local name=${PIDS[$pid]}
    unset "PIDS[$pid]"
    if [ "$r" != 0 ]; then
      log "$name: FAILED (exit $r). Last lines of $LOGS/$name.log:"
      tail -n 40 "$LOGS/$name.log" >&2
      stop_all
      log "FAILED after $(el "$T0")s: dist/ is incomplete, do not deploy"
      exit 1
    fi
    log "$name: done ($(tail -n 1 "$LOGS/$name.log" | sed -n 's/^== exit 0 after //p'))"
  done
}

AI_TMP="$WR/ai/tmp-$$"
if [ "$BUILD_AI" = 1 ]; then
  rm -rf "$AI_TMP"
  run ai-plain "$J_AI" packaging/web-ai/build.sh --stage "$AI_TMP/plain" plain
  if [ "${PHOTOCRAFT_WEB_AI_THREADS:-1}" != "0" ]; then
    run ai-threads "$J_AI" packaging/web-ai/build.sh --stage "$AI_TMP/mt" threads
  fi
elif [ "$AI" = 1 ]; then
  log "AI workers: unchanged inputs, reusing $AI_DIR"
fi
# Each site's fat-LTO link peaks near 10 GB: link both at once only with the memory for it (on a
# loaded machine, swapping costs far more than the overlap gains); otherwise one after the other,
# each with the whole share of jobs.
avail_gb=$(awk '/^MemAvailable:/ {print int($2 / 1048576)}' /proc/meminfo 2>/dev/null || echo 0)
need_gb="${PHOTOCRAFT_WEB_MEM_GB:-$([ -n "${CARGO_PROFILE_WASM_RELEASE_LTO:-}" ] && echo 8 || echo 24)}"
if [ "${avail_gb:-0}" -ge "$need_gb" ]; then
  run st "$J_SITE" bash -c 'cd apps/photocraft-web && exec trunk build --release'
  run threads "$J_SITE" env PHOTOCRAFT_WEB_AI=0 packaging/web-threads/build.sh
else
  log "${avail_gb} GB available (< $need_gb GB, PHOTOCRAFT_WEB_MEM_GB): the two sites build one after the other"
  run st+threads "$(( J_SITE * 2 ))" bash -c '(cd apps/photocraft-web && trunk build --release) &&
    echo "== st done, threads next" && PHOTOCRAFT_WEB_AI=0 exec packaging/web-threads/build.sh'
fi
wait_all
trap - INT TERM
# Both sites must come from this run (a stage that silently did nothing would deploy a stale one).
for f in dist/web/index.html dist/web-threads/index.html; do
  [ "$f" -nt "$STAMP" ] || { log "error: $f was not rebuilt by this run: do not deploy"; exit 1; }
done

# --- AI workers into the sites -------------------------------------------------------------------
if [ "$BUILD_AI" = 1 ]; then
  # plain/ for the st site; threads/ = plain + threads variants for the threads site (the site's
  # ai/<hash>/ is the hash of exactly those files, as packaging/web-ai/build.sh makes it).
  mkdir -p "$AI_TMP/threads"
  cp -R "$AI_TMP/plain/." "$AI_TMP/threads/"
  [ -d "$AI_TMP/mt" ] && cp -R "$AI_TMP/mt/." "$AI_TMP/threads/"
  rm -rf "$AI_TMP/mt" "$AI_DIR"
  touch "$AI_TMP/done"
  mv "$AI_TMP" "$AI_DIR"
  # Keep the last 3 cached builds.
  find "$WR/ai" -mindepth 1 -maxdepth 1 -type d -name 'tmp-*' -mmin +1440 -exec rm -rf {} +
  ls -1dt "$WR/ai"/*/ | grep -v '/tmp-' | tail -n +4 | xargs -r rm -rf
fi
if [ "$AI" = 1 ]; then
  touch "$AI_DIR"
  run ai-install-st 1 packaging/web-ai/build.sh dist/web --prebuilt "$AI_DIR/plain"
  run ai-install-threads 1 packaging/web-ai/build.sh dist/web-threads --threads --prebuilt "$AI_DIR/threads"
  wait_all
fi

# The threads build's per-file cap, now with the AI workers in (as packaging/web-threads/build.sh).
max="${PHOTOCRAFT_WASM_MAX_BYTES:-33554432}"
big=$(find dist/web dist/web-threads -type f -size +"$max"c)
[ -z "$big" ] || { log "error: over the $max-byte limit: $big"; exit 1; }
for s in st threads st+threads ai-install-st ai-install-threads; do
  if [ -f "$LOGS/$s.log" ]; then
    grep -hE '^(split: |photocraft_bg.wasm: |wrote |AI worker: )' "$LOGS/$s.log" | sed "s/^/  $s: /" || true
  fi
done
log "done in $(el "$T0")s: dist/web, dist/web-threads"
