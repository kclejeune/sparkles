# shellcheck shell=bash disable=SC2034 # the variables set here are used by the sourcing script
# Shared code of scripts/bench.sh: tool lookup, the engines' servers (start, stop, ports),
# memory readings from /proc and the JSON result files. Sourced, never run.
#
# The caller sets ROOT, WORK (absolute), ENGINES and SPARKLES before sourcing it, and STORE
# (the directory that holds the engines' stores, default WORK) before starting servers.

log() { echo "[$(date +%H:%M:%S)] $*" >&2; }
die() {
  log "error: $*"
  exit 1
}
# nixbin <pkg> <bin>: the binary on PATH, or from nixpkgs. The build's out-link in
# WORK/gcroots is a garbage-collector root, so a `nix store gc` during a long run cannot
# remove the binary.
nixbin() {
  if command -v "$2" > /dev/null; then
    command -v "$2"
  else
    mkdir -p "$WORK/gcroots"
    nix build "nixpkgs#$1" --out-link "$WORK/gcroots/$1" > /dev/null
    echo "$WORK/gcroots/$1/bin/$2"
  fi
}
has() { [[ " $ENGINES " == *" $1 "* ]]; }

# ------------------------------------------------------------------------------ tools
resolve_tools() {
  if has jena; then
    TDBLOADER=$(nixbin apache-jena tdb2.tdbloader)
    FUSEKI=$(nixbin apache-jena-fuseki fuseki-server)
  fi
  if has qlever; then
    QINDEX=$(nixbin qlever qlever-index)
    QSERVER=$(nixbin qlever qlever-server)
  fi
  if has oxigraph; then OXIGRAPH=$(nixbin oxigraph oxigraph); fi
  FLUREE_VERSION=${FLUREE_VERSION:-4.2.2}
  if has fluree && [ -z "${FLUREE:-}" ]; then
    local ft fdir furl want got
    case "$(uname -sm)" in
      "Linux x86_64") ft=x86_64-unknown-linux-gnu ;;
      "Linux aarch64") ft=aarch64-unknown-linux-gnu ;;
      "Darwin arm64") ft=aarch64-apple-darwin ;;
      "Darwin x86_64") ft=x86_64-apple-darwin ;;
      *) die "no Fluree release binary for $(uname -sm); set FLUREE" ;;
    esac
    fdir="$WORK/fluree-$FLUREE_VERSION"
    FLUREE="$fdir/fluree-db-cli-$ft/fluree"
    if [ ! -x "$FLUREE" ]; then
      mkdir -p "$fdir"
      furl="https://github.com/fluree/db/releases/download/v$FLUREE_VERSION/fluree-db-cli-$ft.tar.xz"
      curl -sfL "$furl" -o "$fdir/fluree.tar.xz"
      want=$(curl -sfL "$furl.sha256" | cut -d' ' -f1)
      got=$(sha256sum "$fdir/fluree.tar.xz" 2> /dev/null || shasum -a 256 "$fdir/fluree.tar.xz")
      got=${got%% *}
      [ "$want" = "$got" ] || die "Fluree checksum mismatch ($got != $want)"
      tar xJf "$fdir/fluree.tar.xz" -C "$fdir"
    fi
  fi
}
# GNU time (not the shell keyword), for the peak RSS of loads
gnu_time() {
  type -P gtime && return
  [ -x /usr/bin/time ] && echo /usr/bin/time && return
  # not nixbin: `command -v time` finds the shell keyword
  mkdir -p "$WORK/gcroots"
  nix build nixpkgs#time --out-link "$WORK/gcroots/time" > /dev/null
  echo "$WORK/gcroots/time/bin/time"
}

# ------------------------------------------------------------------------- engines
# The ports are PORT_BASE (default 3931) and the four after it, so concurrent runs with
# different PORT_BASE values do not collide.
PORT_BASE=${PORT_BASE:-3931}
declare -A NAME LOADNAME PORT FILEOF URL UPDURL UPDFIELD PID READY_S
NAME=([sparkles]=sparkles [jena]=jena-fuseki [qlever]=qlever [fluree]=fluree [oxigraph]=oxigraph)
LOADNAME=([sparkles]=sparkles [jena]=jena-tdb2 [qlever]=qlever [fluree]=fluree [oxigraph]=oxigraph)
PORT=([sparkles]=$PORT_BASE [qlever]=$((PORT_BASE + 1)) [jena]=$((PORT_BASE + 2)) [fluree]=$((PORT_BASE + 3)) [oxigraph]=$((PORT_BASE + 4)))
FILEOF=([sparkles]=sparkles.db [jena]=jena.db [qlever]=qlever-index [fluree]=fluree [oxigraph]=oxigraph.db)
STORE=${STORE:-$WORK}

# the loaded store of an engine in WORK (SPARKLES_DB overrides Sparkles')
source_store() {
  if [ "$1" = sparkles ] && [ -n "${SPARKLES_DB:-}" ]; then echo "$SPARKLES_DB"; else echo "$WORK/${FILEOF[$1]}"; fi
}
# the store a server opens: the loaded one, or its copy when STORE is a scratch directory
store_of() {
  if [ "$STORE" = "$WORK" ]; then source_store "$1"; else echo "$STORE/${FILEOF[$1]}"; fi
}
# copy_stores: a fresh copy of every selected engine's store into STORE (reflinks where the
# file system has them), so writes never touch the loaded stores
copy_stores() {
  local e src
  mkdir -p "$STORE"
  for e in $ENGINES; do
    src=$(source_store "$e")
    [ -e "$src" ] || die "$e: no store at $src (run the load first)"
    rm -rf "${STORE:?}/${FILEOF[$e]}"
    cp -a --reflink=auto "$src" "$STORE/${FILEOF[$e]}"
  done
  rm -rf "$STORE/sparkles-server"
}

pid_on() { ss -ltnp 2> /dev/null | grep ":$1 " | sed -n 's/.*pid=\([0-9]*\).*/\1/p' | head -1 || true; }

# start <engine>: start its server on STORE and wait until it answers; READY_S gets the
# seconds from launch to the first answer
start() {
  local e=$1 p=${PORT[$1]} ready t0 t1
  [ -z "$(pid_on "$p")" ] || die "port $p is in use (set PORT_BASE)"
  t0=$(date +%s.%N)
  case $e in
    sparkles)
      # shellcheck disable=SC2086 # SPARKLES_ARGS is a list of flags
      "$SPARKLES" --result-cache-mb 0 serve --data "$STORE/sparkles-server" --loc bench="$(store_of sparkles)" \
        --port "$p" --timeout 600 ${SPARKLES_ARGS:-} > "$STORE/sparkles.log" 2>&1 &
      URL[$e]=localhost:$p/bench/sparql
      UPDURL[$e]=localhost:$p/bench/update
      ready="localhost:$p/\$/ping"
      ;;
    jena)
      JVM_ARGS="-Xmx${JENA_HEAP:-8G}" "$FUSEKI" --update --port "$p" --loc "$(store_of jena)" /bench > "$STORE/fuseki.log" 2>&1 &
      URL[$e]=localhost:$p/bench/sparql
      UPDURL[$e]=localhost:$p/bench/update
      ready="localhost:$p/\$/ping"
      ;;
    qlever)
      (cd "$(store_of qlever)" && exec "$QSERVER" -i bench -p "$p" -m 8G -c 2G -e 0B -s 600s -a bench -j 16 > server.log 2>&1) &
      URL[$e]=localhost:$p/
      UPDURL[$e]=localhost:$p/
      UPDFIELD[$e]=access-token=bench
      ready="localhost:$p/?cmd=stats"
      ;;
    fluree)
      # property-path traversal is capped at 1M visited nodes by default (knows-reach at 10M)
      (cd "$(store_of fluree)" && FLUREE_CACHE_MAX_MB=4096 FLUREE_PATH_MAX_VISITED=20000000 FLUREE_QUERY_TIMEOUT_MS=600000 \
        exec "$FLUREE" server run --listen-addr "127.0.0.1:$p" --storage-path "$(store_of fluree)/.fluree/storage" \
        --log-level warn > "$STORE/fluree.log" 2>&1) &
      URL[$e]=localhost:$p/v1/fluree/query/bench:main
      UPDURL[$e]=localhost:$p/v1/fluree/update/bench:main
      ready="localhost:$p/health"
      ;;
    oxigraph)
      "$OXIGRAPH" serve --location "$(store_of oxigraph)" --bind "127.0.0.1:$p" --timeout-s 600 > "$STORE/oxigraph.log" 2>&1 &
      URL[$e]=localhost:$p/query
      UPDURL[$e]=localhost:$p/update
      ready="localhost:$p/query?query=ASK%7B%7D"
      ;;
    *) die "unknown engine $e" ;;
  esac
  PID[$e]=$!
  for _ in $(seq 1 12000); do
    if curl -sf -o /dev/null --max-time 5 "$ready" 2> /dev/null; then
      t1=$(date +%s.%N)
      READY_S[$e]=$(awk "BEGIN {printf \"%.3f\", $t1 - $t0}")
      return 0
    fi
    kill -0 "${PID[$e]}" 2> /dev/null || die "$e: server exited, see its log in $STORE"
    sleep 0.05
  done
  die "$e: server did not start"
}
# stop <engine>: by port, since wrapper scripts (fuseki-server) may outlive their JVM
stop() {
  local e=$1 pid
  pid=$(pid_on "${PORT[$e]}")
  [ -n "$pid" ] && { kill "$pid" 2> /dev/null || true; }
  [ -n "${PID[$e]:-}" ] && { kill "${PID[$e]}" 2> /dev/null || true; }
  for _ in $(seq 1 240); do
    [ -z "$(pid_on "${PORT[$e]}")" ] && break
    sleep 0.25
  done
  [ -n "${PID[$e]:-}" ] && { wait "${PID[$e]}" 2> /dev/null || true; }
  unset "PID[$e]"
  true
}
stop_all() { for e in $ENGINES; do stop "$e"; done; }
# drop an engine's files from the page cache (no root needed: posix_fadvise DONTNEED); a
# running server's mapped pages stay, so this runs between a stop and a start
evict() { find "$(store_of "$1")" -type f -exec dd if={} iflag=nocache count=0 status=none \;; }

# the curl command that sends the SPARQL Update in file $2 to engine $1
upd() {
  local f=()
  [ -n "${UPDFIELD[$1]:-}" ] && f=(--data-urlencode "${UPDFIELD[$1]}")
  echo "curl -sf -o /dev/null --data-urlencode update@$2 ${f[*]} ${UPDURL[$1]}"
}

# ---------------------------------------------------------------- memory from /proc
# Readings are in MiB, "?" when the process or the file is not there (macOS has no /proc).
mem_field() { # mem_field <engine> <VmRSS|VmHWM>
  local pid
  pid=$(pid_on "${PORT[$1]}")
  [ -n "$pid" ] && awk -v k="$2:" '$1 == k {printf "%.0f", $2/1024; f=1} END {exit !f}' "/proc/$pid/status" 2> /dev/null || echo "?"
}
rss_of() { mem_field "$1" VmRSS; }
hwm_of() { mem_field "$1" VmHWM; }
# reset_peak <engine>: VmHWM restarts from the current RSS (clear_refs, Linux 4.0 and later)
reset_peak() {
  local pid
  pid=$(pid_on "${PORT[$1]}")
  [ -n "$pid" ] && { echo 5 > "/proc/$pid/clear_refs"; } 2> /dev/null || true
}
# the decoded-block cache of Sparkles, in MiB (`/$/stats/<dataset>`)
block_cache_of() {
  curl -sf --max-time 10 "localhost:${PORT[sparkles]}/\$/stats/bench" |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["cache"]["bytes"] >> 20)' 2> /dev/null || echo "?"
}

# -------------------------------------------------------------------- result files
# merge <new.json> <dest.json>: replace/add hyperfine results by command name
merge() {
  python3 - "$1" "$2" << 'EOF'
import json, os, sys
new, dest = sys.argv[1], sys.argv[2]
n = json.load(open(new))
old = json.load(open(dest)) if os.path.exists(dest) else {"results": []}
cmds = {r["command"] for r in n["results"]}
old["results"] = [r for r in old["results"] if r["command"] not in cmds] + n["results"]
json.dump(old, open(dest, "w"), indent=1)
EOF
  rm -f "$1"
}
# setj <file> <key path> <name>=<value>…: merge values into a JSON object under the
# dot-separated key path (empty: the top level). A value that parses as JSON (a number,
# an object) is stored as that, any other as a string.
setj() {
  python3 - "$@" << 'EOF'
import json, os, sys
f, key, kvs = sys.argv[1], sys.argv[2], sys.argv[3:]
d = json.load(open(f)) if os.path.exists(f) else {}
t = d
for k in filter(None, key.split(".")):
    t = t.setdefault(k, {})
for kv in kvs:
    k, v = kv.split("=", 1)
    try:
        t[k] = json.loads(v)
    except ValueError:
        t[k] = v.strip()
json.dump(d, open(f, "w"), indent=1)
EOF
}
# record_load <results dir> <engine> <seconds> <time -v output> <index path>: load.json (in
# hyperfine's format, for the summary) and load-details.json (peak RSS, index size)
record_load() {
  python3 - "$@" << 'EOF'
import json, os, re, subprocess, sys
d, engine, secs, timef, index = sys.argv[1:]
t = open(timef).read()
rss = int(re.search(r"Maximum resident set size \(kbytes\): (\d+)", t).group(1))
du = lambda *a: int(subprocess.run(["du", "-s", *a, index], capture_output=True, text=True).stdout.split()[0])
# the bytes allocated on disk: TDB2's files are sparse, so their apparent size is much larger
size, apparent = du("--block-size=1"), du("-b")
f = f"{d}/load.json"
j = json.load(open(f)) if os.path.exists(f) else {"results": []}
j["results"] = [r for r in j["results"] if r["command"] != engine] + [
    {"command": engine, "mean": float(secs), "stddev": 0.0, "times": [float(secs)], "exit_codes": [0]}]
json.dump(j, open(f, "w"), indent=1)
f = f"{d}/load-details.json"
j = json.load(open(f)) if os.path.exists(f) else {}
j[engine] = {"seconds": float(secs), "max_rss_kib": rss, "index_bytes": size, "index_apparent_bytes": apparent}
json.dump(j, open(f, "w"), indent=1)
print(f"{engine}: {float(secs):.1f} s, peak RSS {rss >> 10} MiB, index {size / 2**20:.0f} MiB")
EOF
}
