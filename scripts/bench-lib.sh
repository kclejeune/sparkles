# shellcheck shell=bash disable=SC2034,SC2153 # the variables set here are used by the sourcing script
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
# nixbin <pkg> <bin>: the binary on PATH, or from the nixpkgs that flake.lock pins (so the
# engines' versions follow the repository, not the user's registry). The build's out-link in
# WORK/gcroots is a garbage-collector root, so a `nix store gc` during a long run cannot
# remove the binary.
nixbin() {
  if command -v "$2" > /dev/null; then
    command -v "$2"
  else
    mkdir -p "$WORK/gcroots"
    nix build --inputs-from "$ROOT" "nixpkgs#$1" --out-link "$WORK/gcroots/$1" > /dev/null
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
  nix build --inputs-from "$ROOT" nixpkgs#time --out-link "$WORK/gcroots/time" > /dev/null
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
# The longest a request may take, in seconds. The clients (curl, the answer check, the BSBM
# driver) give up after MAX_TIME and record a timeout. The servers' own query timeouts are
# MAX_TIME plus a margin, so that a server stops a query the client has already given up on
# and the next measurement does not start behind it.
MAX_TIME=${MAX_TIME:-300}
export MAX_TIME
SERVER_TIMEOUT=$((MAX_TIME + ${TIMEOUT_MARGIN:-15}))

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
  # Without reflinks the copies are gigabytes of dirty pages at 10.5M triples. Their
  # writeback would run during the first engine's commits, and every commit that syncs
  # would wait behind it: Sparkles' median commit took 4 ms instead of 1 ms on forge.
  sync
}

# Servers run inside a transient systemd scope limited to SERVER_MEM_MAX (for example 12G)
# when it is set, so an engine that runs out of memory is killed alone and its queries
# are reported as errors, instead of the machine swapping until it stops responding.
CAP=()
if [ -n "${SERVER_MEM_MAX:-}" ]; then
  command -v systemd-run > /dev/null || die "SERVER_MEM_MAX needs systemd-run"
  CAP=(systemd-run --user --scope --quiet -p "MemoryMax=$SERVER_MEM_MAX" -p MemorySwapMax=0 --)
fi

# The latency runs start one curl process per request, and starting curl takes 3 to 10 ms
# of CPU, more than most of the queries. Left to the scheduler, each process lands on
# whichever core is idle, often one clocked down or an efficiency core, and the start-up
# time then changes from run to run and in streaks: on an idle hybrid machine the same
# request against the same server took 3.3 ms or 10 ms. So hyperfine and its curl
# processes run on one CPU, a performance core when the machine has two kinds, which
# stays warm because it runs them back to back. The servers are not pinned.
# CLIENT_CPU picks another CPU, and CLIENT_CPU=none turns pinning off.
client_cpu() {
  if [ -n "${CLIENT_CPU:-}" ]; then
    [ "$CLIENT_CPU" != none ] && echo "$CLIENT_CPU"
    return 0
  fi
  command -v taskset > /dev/null || return 0
  local list
  if [ -r /sys/devices/cpu_core/cpus ]; then
    list=$(cat /sys/devices/cpu_core/cpus)
  elif [ -r /sys/devices/system/cpu/online ]; then
    list=$(cat /sys/devices/system/cpu/online)
  else
    return 0
  fi
  # the last CPU of a list like 0-11 or 0,2-5
  echo "${list##*[,-]}"
}
CLIENT=()
if cpu=$(client_cpu) && [ -n "$cpu" ]; then CLIENT=(taskset -c "$cpu"); fi

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
      "${CAP[@]}" "$SPARKLES" --result-cache-mb 0 serve --data "$STORE/sparkles-server" --loc bench="$(store_of sparkles)" \
        --port "$p" --timeout "$SERVER_TIMEOUT" ${SPARKLES_ARGS:-} > "$STORE/sparkles.log" 2>&1 &
      URL[$e]=localhost:$p/bench/sparql
      UPDURL[$e]=localhost:$p/bench/update
      ready="localhost:$p/\$/ping"
      ;;
    jena)
      JVM_ARGS="-Xmx${JENA_HEAP:-8G}" "${CAP[@]}" "$FUSEKI" --update --timeout="$((SERVER_TIMEOUT * 1000))" --port "$p" --loc "$(store_of jena)" /bench > "$STORE/fuseki.log" 2>&1 &
      URL[$e]=localhost:$p/bench/sparql
      UPDURL[$e]=localhost:$p/bench/update
      ready="localhost:$p/\$/ping"
      ;;
    qlever)
      (cd "$(store_of qlever)" && exec "${CAP[@]}" "$QSERVER" -i bench -p "$p" -m 8G -c 2G -e 0B -s "${SERVER_TIMEOUT}s" -a bench -j 16 > server.log 2>&1) &
      URL[$e]=localhost:$p/
      UPDURL[$e]=localhost:$p/
      UPDFIELD[$e]=access-token=bench
      ready="localhost:$p/?cmd=stats"
      ;;
    fluree)
      # property-path traversal is capped at 1M visited nodes by default (knows-reach at 10M)
      (cd "$(store_of fluree)" && FLUREE_CACHE_MAX_MB=4096 FLUREE_PATH_MAX_VISITED=20000000 FLUREE_QUERY_TIMEOUT_MS=$((SERVER_TIMEOUT * 1000)) \
        exec "${CAP[@]}" "$FLUREE" server run --listen-addr "127.0.0.1:$p" --storage-path "$(store_of fluree)/.fluree/storage" \
        --log-level warn > "$STORE/fluree.log" 2>&1) &
      URL[$e]=localhost:$p/v1/fluree/query/bench:main
      UPDURL[$e]=localhost:$p/v1/fluree/update/bench:main
      ready="localhost:$p/health"
      ;;
    oxigraph)
      "${CAP[@]}" "$OXIGRAPH" serve --location "$(store_of oxigraph)" --bind "127.0.0.1:$p" --timeout-s "$SERVER_TIMEOUT" > "$STORE/oxigraph.log" 2>&1 &
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

# ------------------------------------------------------------------------------- load
# load_all: DATAFILE (default data.nt, relative to WORK) into every selected engine
# (LOADRES, default results, gets the numbers). Each load runs under GNU time for its
# peak RSS (the largest of the processes it waits
# for); load.json and load-details.json get the time, peak RSS and index size.
load_all() {
  local gtime e cmd t0 t1 df=${DATAFILE:-data.nt}
  gtime=$(gnu_time)
  for e in $ENGINES; do
    case $e in
      sparkles) cmd="'$SPARKLES' load --loc sparkles.db $df" ;;
      jena) cmd="'$TDBLOADER' --loc jena.db $df" ;;
      qlever)
        cmd="mkdir -p qlever-index && cd qlever-index && echo '{\"num-triples-per-batch\": 1000000}' > settings.json &&
          '$QINDEX' -i bench -F nt -f ../$df -p true -s settings.json"
        ;;
      # Fluree's bulk import (`create --from`) builds the index directly; without
      # --chunk-size-mb it parses the file as a single chunk (about 3x slower)
      fluree) cmd="mkdir -p fluree && cd fluree && '$FLUREE' init -q && '$FLUREE' --memory-budget-mb 8192 create bench --from ../$df --chunk-size-mb 16" ;;
      # Oxigraph's bulk loader (writes RocksDB files directly), then the compaction it
      # recommends before read-heavy workloads (`optimize`); both are timed as the load. The
      # data goes in on standard input: with `--file`, Oxigraph 0.5.11 parses N-Triples in
      # parallel chunks and gives a blank node label that occurs in two chunks two different
      # nodes (SP2Bench at 1M triples: 109,384 blank nodes instead of 86,870), which changes
      # query answers. On standard input the labels stay whole, at about 3x the load time.
      oxigraph) cmd="'$OXIGRAPH' load --location oxigraph.db --format nt < $df && '$OXIGRAPH' optimize --location oxigraph.db" ;;
    esac
    rm -rf "${WORK:?}/${FILEOF[$e]}"
    log "$e: loading (log: logs/load-$e.log)"
    t0=$(date +%s.%N)
    "$gtime" -v -o "logs/load-$e.time" bash -c "$cmd" > "logs/load-$e.log" 2>&1 || die "$e: load failed, see logs/load-$e.log"
    t1=$(date +%s.%N)
    record_load "${LOADRES:-results}" "${LOADNAME[$e]}" "$(awk "BEGIN {print $t1 - $t0}")" "logs/load-$e.time" "$WORK/${FILEOF[$e]}"
  done
}

# ------------------------------------------------------------------ query suites
# Shared by scripts/bench.sh and the suite scripts: they set NAMES (query names, texts in
# queries/<name>.rq), RES (the results directory) and selected <name> before calling these.
# qform <file>: select, ask, construct or describe (the first of these words outside IRIs and comments)
qform() { sed -e 's/<[^<>[:space:]]*>//g' -e 's/#.*//' "$1" | grep -ioE '\b(select|construct|describe|ask)\b' | head -1 | tr '[:upper:]' '[:lower:]'; }
# accept_of <query name>: the result format a timed request asks for, by the query form
accept_of() {
  case "$(qform "queries/$1.rq")" in
    ask) echo application/sparql-results+json ;;
    construct | describe) echo application/n-triples ;;
    *) echo text/tab-separated-values ;;
  esac
}
q() { # curl command for endpoint + query name (fails on HTTP errors)
  echo "curl -sf --max-time $MAX_TIME -o /dev/null -H 'Accept: $(accept_of "$2")' --data-urlencode query@queries/$2.rq $1"
}
# hyperfine arguments for every selected engine: engine_args <command-fn>
engine_args() { for e in $ENGINES; do printf '%s\0' --command-name "${NAME[$e]}" "$($1 "$e")"; done; }
# the rise a - b as JSON (0 if negative: the kernel's RSS counters are approximate), or "?"
minus() { if [[ $1 =~ ^[0-9]+$ && $2 =~ ^[0-9]+$ ]]; then echo $(($1 > $2 ? $1 - $2 : 0)); else echo '"?"'; fi; }
# a reading as JSON: the number, or "?"
num() { if [[ $1 =~ ^[0-9]+$ ]]; then echo "$1"; else echo '"?"'; fi; }

# start_all: every selected engine's server on STORE; mem.json gets each one's RSS
# 2 s after all are ready, before any query
start_all() {
  local e kv=()
  for e in $ENGINES; do start "$e"; done
  sleep 2
  for e in $ENGINES; do kv+=("${NAME[$e]}=$(rss_of "$e")"); done
  setj "$RES/mem.json" idle "${kv[@]}"
}

# answer check: every engine's answer to every query is fingerprinted (row count, exact
# RDF terms, numeric values) into answers.json; the summary compares engines. An engine
# that errors (HTTP failure) is reported as "error" instead of a time. LIMIT without
# ORDER BY may legitimately return different solutions: row counts only.
# Each request is also the query's memory reading: the server's peak RSS is reset
# (clear_refs) before it, and mem.json gets the peak (VmHWM) after it and its rise over
# the RSS at the reset.
COUNT_ONLY=${COUNT_ONLY-export-500k}
answer_check() {
  local n e r before peak kv
  echo
  printf '%-16s' rows
  for e in $ENGINES; do printf ' %12s' "${NAME[$e]}"; done
  echo
  for n in "${NAMES[@]}"; do
    selected "$n" || continue
    local flag=()
    [[ " $COUNT_ONLY " == *" $n "* ]] && flag=(--count-only)
    [[ " ${NO_CHECK:-} " == *" $n "* ]] && flag+=(--no-check)
    printf '%-16s' "$n"
    kv=()
    for e in $ENGINES; do
      # right after the reset, the peak is the RSS: the baseline (a JVM may shrink between
      # a separate RSS reading and the reset)
      reset_peak "$e"
      before=$(hwm_of "$e")
      r=$(python3 "$ROOT/scripts/bench-answers.py" "${URL[$e]}" "queries/$n.rq" "$RES/answers.json" "$n" "${NAME[$e]}" "${flag[@]}")
      peak=$(hwm_of "$e")
      kv+=("${NAME[$e]}={\"before\": $(num "$before"), \"peak\": $(num "$peak"), \"delta\": $(minus "$peak" "$before")}")
      printf ' %12s' "$r"
    done
    setj "$RES/mem.json" "queries.$n" "${kv[@]}"
    echo
  done
  # the RSS after every query ran once (a fixed warm-up), read once the servers are idle
  sleep 2
  kv=()
  for e in $ENGINES; do kv+=("${NAME[$e]}=$(rss_of "$e")"); done
  setj "$RES/mem.json" warm "${kv[@]}"
}

# --------------------------------------------------------------------------- cold
# cold <engine>: per query, COLD_RUNS times: stop, evict, start, one timed query.
# cold.json gets the median time (the format bench-billion.sh writes), cold-runs.json
# every run's time and time to ready.
cold() {
  local e=$1 n i t rc times ready
  for n in "${NAMES[@]}"; do
    selected "$n" || continue
    times=()
    ready=()
    for i in $(seq 1 "$COLD_RUNS"); do
      stop "$e"
      evict "$e"
      start "$e"
      ready+=("${READY_S[$e]}")
      # curl prints the time even when the request fails. A failure that took as long as the
      # limit (the client gave up, or the server's own timeout fired) is a timeout, any other an error.
      rc=0
      t=$(curl -sf --max-time "$MAX_TIME" -o /dev/null -w '%{time_total}' -H "Accept: $(accept_of "$n")" \
        --data-urlencode "query@queries/$n.rq" "${URL[$e]}") || rc=$?
      if [ "$rc" -ne 0 ]; then
        if [ "$rc" = 28 ] || awk -v t="${t:-0}" -v m="$MAX_TIME" 'BEGIN {exit !(t >= 0.95 * m)}'; then t=timeout; else t=error; fi
      fi
      times+=("$t")
      log "cold $e $n run $i: ready ${READY_S[$e]} s, query $t s"
    done
    stop "$e"
    python3 - "$RES" "${NAME[$e]}" "$n" "${times[*]}" "${ready[*]}" << 'EOF'
import json, os, statistics, sys
d, e, n, times, ready = sys.argv[1:]
times, ready = times.split(), [float(r) for r in ready.split()]
ok = [float(t) for t in times if t not in ("error", "timeout")]
def put(f, v):
    j = json.load(open(f)) if os.path.exists(f) else {}
    j.setdefault(e, {})[n] = v
    json.dump(j, open(f, "w"), indent=1)
put(f"{d}/cold.json", statistics.median(ok) if ok else "error")
put(f"{d}/cold-runs.json", {"times": [t if t in ("error", "timeout") else float(t) for t in times], "ready": ready})
EOF
  done
}

# pinned_build <nix file> <out-link>: build a generator's derivation (the file takes
# { pkgs }) against the nixpkgs revision locked in flake.lock, so the compiler and the
# libraries are pinned too (as scripts/bench-watdiv.sh does).
pinned_build() {
  nix build --impure --out-link "$2" --expr "
    let
      n = (builtins.fromJSON (builtins.readFile $ROOT/flake.lock)).nodes.nixpkgs.locked;
      pkgs = import (builtins.fetchTarball {
        url = \"https://github.com/\${n.owner}/\${n.repo}/archive/\${n.rev}.tar.gz\";
        sha256 = n.narHash;
      }) { };
    in import $1 { inherit pkgs; }"
}

# fetch_pinned <pins.tsv> <name> <dest>: download a file listed in a pins file (name, URL,
# SHA-256 and license, tab separated) unless it is there, and check its checksum
fetch_pinned() {
  local url sum got
  url=$(awk -F'\t' -v n="$2" '$1 == n {print $2}' "$1")
  sum=$(awk -F'\t' -v n="$2" '$1 == n {print $3}' "$1")
  [ -n "$url" ] || die "$2 is not in $1"
  if [ ! -f "$3" ]; then
    log "downloading $2"
    curl -sfL "$url" -o "$3.part" || die "cannot download $url"
    mv "$3.part" "$3"
  fi
  got=$(sha256sum "$3" 2> /dev/null || shasum -a 256 "$3")
  [ "${got%% *}" = "$sum" ] || die "$2: checksum ${got%% *} does not match the pinned $sum"
}
