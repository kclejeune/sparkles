#!/usr/bin/env bash
# Benchmark on real data at a chosen scale, up to the full 1.24 billion triples of English
# DBpedia (release 2022.12.01, scripts/bench-billion/dbpedia-2022.12.tsv): Sparkles vs
# QLever, optionally Jena (TDB2 xloader + Fuseki), Oxigraph and Fluree.
#
#   scripts/bench-billion.sh [STEP...]      steps: fetch prepare slice load queries report
#                                           (default: all of them, in this order)
#
# * fetch: downloads the manifest's files (resumable, checksum-verified) to
#   WORKDIR/download. Nothing is downloaded twice.
# * prepare: normalizes every file to N-Triples compressed with zstd (WORKDIR/nt/*.nt.zst,
#   no uncompressed copies) and counts the triples (WORKDIR/nt/counts.tsv). The release's
#   .ttl files are N-Triples, except for lines with characters that N-Triples does not
#   allow in an IRI (spaces, `\n` escapes: some image file names). Parsers differ on those
#   (Jena rejects them, QLever takes them), so they are dropped (and counted) and all
#   engines load the same triples. IRIs that are not valid RFC 3987 IRIs (U+FFFD
#   in some) are kept, as QLever, Jena and lenient Oxigraph keep them; Sparkles loads them
#   with --lenient.
# * slice: the data of the scale. SCALE=full is every file; SCALE=50m (10m, 250m, 1b…)
#   keeps the triples of a fixed sample of the subjects, about that many triples
#   (scripts/bench-billion/sample.py): every triple of a sampled entity, in every file,
#   and every smaller slice is contained in the larger ones, so the queries' parameters
#   (scripts/bench-billion/queries, chosen from the 10m slice) occur at every scale.
# * load: builds each selected engine's index in WORKDIR/SCALE (reused by later runs;
#   RELOAD=1 rebuilds it), recording the load time, peak RSS (GNU time) and index size.
#   A load that runs longer than LOAD_TIMEOUT (for example 4h) is stopped and fails.
# * queries: starts the engines, checks that all of them give the same answers
#   (scripts/bench-answers.py), then times every query warm (hyperfine: WARMUP runs, then
#   RUNS), cold (COLD=1: one run after a restart with the engine's files evicted from the
#   page cache), the throughput of NREQ requests from CONC parallel clients for the
#   THROUGHPUT queries, and the servers' RSS.
# * report: WORKDIR/SCALE/results/summary.md (scripts/bench-summary.py), in the format of
#   scripts/bench.sh.
#
# Env: WORKDIR (default target/bench-billion), SCALE (default 50m), ENGINES (default
# "sparkles qlever"; also jena, oxigraph, fluree), RELOAD=1, RUNS (5), WARMUP (1), COLD (1),
# CONC (16), NREQ (64), THROUGHPUT ("entity-facts-1 place-births-1"), QUERIES (a subset of
# query names), ANSWERS_ONLY=1 (check answers and rebuild the summary, time nothing),
# TIMEOUT (seconds per query, default 600), QUERY_MEM_GB (the query memory budget of
# Sparkles and QLever, default 12), QLEVER_INDEX_MEM (QLever's sort memory while indexing,
# default 10G), JENA_HEAP (Fuseki -Xmx, default 8G), FLUREE_IMPORT_MB (the memory budget
# of Fluree's bulk import, default 8192), FLUREE_PIECE (the largest piece of Fluree's
# input, uncompressed, as `split -C` takes it, default 512M), FLUREE_CACHE_MB (the cache of Fluree's server,
# default 4096), LOAD_TIMEOUT (a time limit for each load, in the form `timeout` takes),
# SPARKLES (the binary, default target/release/sparkles).
#
# Tools come from nixpkgs when not on PATH (qlever, apache-jena, apache-jena-fuseki,
# oxigraph, lbzip2, zstd, GNU time). Fluree (BUSL-1.1, not in nixpkgs) is the
# checksum-verified release binary that scripts/bench.sh uses, downloaded to WORKDIR
# unless FLUREE points at one (FLUREE_VERSION, default 4.2.2). The data is DBpedia's, under CC BY-SA 3.0 and GFDL
# (https://www.dbpedia.org/about/); see docs/BENCHMARKS.md for the attribution.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
HERE=$ROOT/scripts/bench-billion
MANIFEST=$HERE/dbpedia-2022.12.tsv
WORK=${WORKDIR:-$ROOT/target/bench-billion}
SCALE=${SCALE:-50m}
ENGINES=${ENGINES:-sparkles qlever}
RUNS=${RUNS:-5}
WARMUP=${WARMUP:-1}
COLD=${COLD:-1}
CONC=${CONC:-16}
NREQ=${NREQ:-64}
THROUGHPUT=${THROUGHPUT:-entity-facts-1 place-births-1}
TIMEOUT=${TIMEOUT:-600}
QUERY_MEM_GB=${QUERY_MEM_GB:-12}
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
export NREQ CONC

mkdir -p "$WORK"
WORK=$(cd "$WORK" && pwd)
DL=$WORK/download
NT=$WORK/nt
S=$WORK/$SCALE
mkdir -p "$S/results" "$S/logs"

log() { echo "[$(date +%H:%M:%S)] $*" >&2; }
die() {
  log "error: $*"
  exit 1
}
nixbin() { # nixbin <pkg> <bin>
  if command -v "$2" > /dev/null; then command -v "$2"; else echo "$(nix build "nixpkgs#$1" --no-link --print-out-paths | tail -1)/bin/$2"; fi
}
has() { [[ " $ENGINES " == *" $1 "* ]]; }
selected() { [ -z "${QUERIES:-}" ] || [[ " $QUERIES " == *" $1 "* ]]; }
ZSTD=$(nixbin zstd zstd)
# Fluree's release binary, downloaded and checked as scripts/bench-lib.sh does
fluree_bin() {
  local ft fdir furl want got f
  case "$(uname -sm)" in
    "Linux x86_64") ft=x86_64-unknown-linux-gnu ;;
    "Linux aarch64") ft=aarch64-unknown-linux-gnu ;;
    "Darwin arm64") ft=aarch64-apple-darwin ;;
    "Darwin x86_64") ft=x86_64-apple-darwin ;;
    *) die "no Fluree release binary for $(uname -sm); set FLUREE" ;;
  esac
  fdir=$WORK/fluree-${FLUREE_VERSION:-4.2.2}
  f=$fdir/fluree-db-cli-$ft/fluree
  if [ ! -x "$f" ]; then
    mkdir -p "$fdir"
    furl="https://github.com/fluree/db/releases/download/v${FLUREE_VERSION:-4.2.2}/fluree-db-cli-$ft.tar.xz"
    curl -sfL "$furl" -o "$fdir/fluree.tar.xz"
    want=$(curl -sfL "$furl.sha256" | cut -d' ' -f1)
    got=$(sha256sum "$fdir/fluree.tar.xz" 2> /dev/null || shasum -a 256 "$fdir/fluree.tar.xz")
    got=${got%% *}
    [ "$want" = "$got" ] || die "Fluree checksum mismatch ($got != $want)"
    tar xJf "$fdir/fluree.tar.xz" -C "$fdir"
  fi
  echo "$f"
}
if has fluree && [ -z "${FLUREE:-}" ]; then FLUREE=$(fluree_bin); fi
# GNU time (not the shell keyword) for the peak RSS of loads
GTIME=/usr/bin/time
[ -x "$GTIME" ] || GTIME=$(nix build nixpkgs#time --no-link --print-out-paths | tail -1)/bin/time

# the manifest: local path, URL, size in bytes, sha256 (largest first)
mapfile -t ENTRIES < <(grep -v '^#' "$MANIFEST" | sort -t$'\t' -k3,3nr)
# the normalized file of a manifest path: its base name, as .nt.zst
ntname() {
  local b=${1##*/}
  b=${b%.bz2}
  b=${b%.ttl}
  b=${b%.nt}
  echo "$b.nt.zst"
}

# ----------------------------------------------------------------------------- fetch
fetch_one() { # fetch_one <path> <url> <sha256>
  local f=$DL/$1
  mkdir -p "$(dirname "$f")"
  curl -sSfL --retry 5 --retry-all-errors -C - -o "$f.part" "$2"
  if ! echo "$3  $f.part" | sha256sum -c --quiet; then
    rm -f "$f.part"
    die "checksum mismatch: $2"
  fi
  mv "$f.part" "$f"
  log "downloaded $1"
}
step_fetch() {
  local path url size sha need=0
  for e in "${ENTRIES[@]}"; do
    IFS=$'\t' read -r path url size sha <<< "$e"
    [ -f "$DL/$path" ] || need=$((need + size))
  done
  [ "$need" -eq 0 ] && return
  log "downloading $((need >> 20)) MiB"
  # a file is only moved into place after its checksum matched: a present file is complete
  for e in "${ENTRIES[@]}"; do
    IFS=$'\t' read -r path url size sha <<< "$e"
    [ -f "$DL/$path" ] && continue
    while [ "$(jobs -rp | wc -l)" -ge 4 ]; do wait -n; done
    fetch_one "$path" "$url" "$sha" &
  done
  while [ "$(jobs -rp | wc -l)" -gt 0 ]; do wait -n; done
}

# --------------------------------------------------------------------------- prepare
# lines whose subject, predicate or object IRI has a character N-Triples does not allow
# in one (space, control, "{}|^` or an escape other than \u / \U)
# shellcheck disable=SC2016 # a regular expression: the backquote is a literal character
BAD_IRI='^(<[^>]*>|_:[^ ]*) (<[^>]*> )?<[^>]*(["{}|^`[:cntrl:] ]|\\[^uU])|^<[^>]*(["{}|^`[:cntrl:] ]|\\[^uU])'
# prepare_one <path> <name>: decompress, drop the lines with invalid IRIs, compress
# with zstd; <name>.count gets the triples and the lines dropped
prepare_one() {
  local src=$DL/$1 out=$NT/$2 lines dropped
  rm -f "$out.dropped"
  # grep -v exits with 1 when it keeps no line (an empty file), grep -c when it counts none
  case $1 in
    *.bz2) "$LBZIP2" -n 4 -dc "$src" ;;
    *) cat "$src" ;;
  esac | tee >(
    LC_ALL=C grep -cE "$BAD_IRI" > "$out.dropped.part" || true
    mv "$out.dropped.part" "$out.dropped"
  ) |
    { LC_ALL=C grep -vE "$BAD_IRI" || [ $? -eq 1 ]; } | "$ZSTD" -q -T2 -f -o "$out.part"
  mv "$out.part" "$out"
  # the count comes from a process substitution, which this shell cannot wait for
  until [ -f "$out.dropped" ]; do sleep 0.2; done
  lines=$("$ZSTD" -dc "$out" | LC_ALL=C grep -vc '^[[:space:]]*\(#\|$\)' || true)
  dropped=$(cat "$out.dropped")
  rm -f "$out.dropped"
  printf '%s\t%s\n' "$lines" "$dropped" > "$out.count"
  log "normalized ${1##*/}: $lines triples, $dropped lines dropped"
}
step_prepare() {
  mkdir -p "$NT"
  local path url size sha name
  LBZIP2=$(nixbin lbzip2 lbzip2)
  # largest first, a few at a time
  for e in "${ENTRIES[@]}"; do
    IFS=$'\t' read -r path url size sha <<< "$e"
    name=$(ntname "$path")
    [ -f "$NT/$name.count" ] && continue
    [ -f "$DL/$path" ] || die "missing $DL/$path (run the fetch step)"
    while [ "$(jobs -rp | wc -l)" -ge "${PREPARE_JOBS:-6}" ]; do wait -n; done
    prepare_one "$path" "$name" &
  done
  while [ "$(jobs -rp | wc -l)" -gt 0 ]; do wait -n; done
  # name, triples and dropped lines of every file, for the slices
  for e in "${ENTRIES[@]}"; do
    IFS=$'\t' read -r path url size sha <<< "$e"
    name=$(ntname "$path")
    printf '%s\t%s\n' "$name" "$(cat "$NT/$name.count")"
  done > "$NT/counts.tsv"
  log "normalized: $(awk '{n += $2; d += $3} END {print n " triples, " d " invalid lines dropped"}' "$NT/counts.tsv") in ${#ENTRIES[@]} files"
}

# ----------------------------------------------------------------------------- slice
# the scale in triples (empty for full)
target() {
  case $SCALE in
    full) echo "" ;;
    *[0-9]k) echo $((${SCALE%k} * 1000)) ;;
    *[0-9]m) echo $((${SCALE%m} * 1000000)) ;;
    *[0-9]b) echo $((${SCALE%b} * 1000000000)) ;;
    *[0-9]) echo "$SCALE" ;;
    *) die "SCALE must be full or a triple count like 50m, 1b" ;;
  esac
}
FILES=()
step_slice() {
  [ -f "$NT/counts.tsv" ] || die "no normalized data (run the prepare step)"
  local total t name
  total=$(awk '{n += $2} END {print n}' "$NT/counts.tsv")
  t=$(target)
  if [ -z "$t" ] || [ "$t" -ge "$total" ]; then
    mapfile -t FILES < <(cut -f1 "$NT/counts.tsv" | sort | sed "s|^|$NT/|")
    log "scale $SCALE: all $total triples"
    return
  fi
  mkdir -p "$S/data"
  if [ ! -f "$S/data/.complete" ]; then
    log "sampling about $t of $total triples"
    while IFS=$'\t' read -r name _; do
      while [ "$(jobs -rp | wc -l)" -ge "${PREPARE_JOBS:-6}" ]; do wait -n; done
      {
        "$ZSTD" -dc "$NT/$name" | python3 "$HERE/sample.py" "$(awk "BEGIN {print $t / $total}")" |
          "$ZSTD" -q -T2 -f -o "$S/data/$name.part"
        mv "$S/data/$name.part" "$S/data/$name"
      } &
    done < "$NT/counts.tsv"
    while [ "$(jobs -rp | wc -l)" -gt 0 ]; do wait -n; done
    touch "$S/data/.complete"
  fi
  mapfile -t FILES < <(find "$S/data" -name '*.nt.zst' | sort)
  log "scale $SCALE: ${#FILES[@]} files"
}

# ------------------------------------------------------------------------------ load
# record <engine> <seconds> <time -v output> <index path>: load.json (hyperfine's format,
# for the summary) and load-details.json
record_load() {
  python3 - "$S/results" "$@" << 'EOF'
import json, os, re, subprocess, sys
d, engine, secs, timef, index = sys.argv[1:]
t = open(timef).read()
rss = int(re.search(r"Maximum resident set size \(kbytes\): (\d+)", t).group(1))
size = int(subprocess.run(["du", "-sb", index], capture_output=True, text=True).stdout.split()[0])
f = f"{d}/load.json"
j = json.load(open(f)) if os.path.exists(f) else {"results": []}
j["results"] = [r for r in j["results"] if r["command"] != engine] + [
    {"command": engine, "mean": float(secs), "stddev": 0.0, "times": [float(secs)], "exit_codes": [0]}]
json.dump(j, open(f, "w"), indent=1)
f = f"{d}/load-details.json"
j = json.load(open(f)) if os.path.exists(f) else {}
j[engine] = {"seconds": float(secs), "max_rss_kib": rss, "index_bytes": size}
json.dump(j, open(f, "w"), indent=1)
print(f"{engine}: {float(secs):.0f} s, peak RSS {rss >> 10} MiB, index {size / 2**30:.1f} GiB")
EOF
}
# timed <engine> <index path> <command…>: run a load under GNU time unless it is done
timed() {
  local engine=$1 index=$2 t0 t1 limit=() rc=0
  shift 2
  [ -n "${LOAD_TIMEOUT:-}" ] && limit=(timeout --kill-after=60 "$LOAD_TIMEOUT")
  if [ -f "$S/$engine.loaded" ] && [ -z "${RELOAD:-}" ]; then
    log "$engine: reusing $index (RELOAD=1 rebuilds it)"
    return
  fi
  rm -rf "$index" "$S/$engine.loaded"
  log "$engine: loading (log: $S/logs/load-$engine.log)"
  t0=$(date +%s.%N)
  "$GTIME" -v -o "$S/logs/load-$engine.time" "${limit[@]}" "$@" > "$S/logs/load-$engine.log" 2>&1 || rc=$?
  [ "$rc" -eq 124 ] && die "$engine: load stopped after LOAD_TIMEOUT ($LOAD_TIMEOUT), see $S/logs/load-$engine.log"
  [ "$rc" -eq 0 ] || die "$engine: load failed (exit $rc), see $S/logs/load-$engine.log"
  t1=$(date +%s.%N)
  record_load "$engine" "$(awk "BEGIN {print $t1 - $t0}")" "$S/logs/load-$engine.time" "$index"
  touch "$S/$engine.loaded"
}
step_load() {
  [ ${#FILES[@]} -gt 0 ] || step_slice
  if has sparkles; then
    [ -x "$SPARKLES" ] || die "no $SPARKLES (mise run build)"
    # some DBpedia IRIs are not valid RFC 3987 IRIs (U+FFFD in them): QLever, Jena and
    # Oxigraph (--lenient below) keep them, and so does Sparkles with --lenient
    timed sparkles "$S/sparkles.db" "$SPARKLES" load --lenient --loc "$S/sparkles.db" "${FILES[@]}"
  fi
  if has qlever; then
    local qindex
    qindex=$(nixbin qlever qlever-index)
    # QLever reads the concatenated files from stdin, parsed in parallel
    # shellcheck disable=SC2016 # expanded by the inner shell
    timed qlever "$S/qlever-index" bash -c '
      mkdir -p "$1" && cd "$1" && shift &&
      echo "{\"num-triples-per-batch\": 5000000}" > settings.json &&
      "$1" -dc "${@:4}" | "$2" -i dbpedia -f - -F nt -p true -s settings.json -m "$3"' _ \
      "$S/qlever-index" "$ZSTD" "$qindex" "${QLEVER_INDEX_MEM:-10G}" "${FILES[@]}"
  fi
  if has jena; then
    # xloader reads its input twice and takes gzip, not zstd: a gzip copy of the data
    local xloader g gz=()
    xloader=$(nixbin apache-jena tdb2.xloader)
    mkdir -p "$S/jena-input"
    for f in "${FILES[@]}"; do
      g=$S/jena-input/$(basename "${f%.zst}").gz
      [ -f "$g" ] || { "$ZSTD" -dc "$f" | gzip -1 > "$g.part" && mv "$g.part" "$g"; }
      gz+=("$g")
    done
    mkdir -p "$S/jena-tmp"
    timed jena-tdb2 "$S/jena.db" "$xloader" --loc "$S/jena.db" --tmpdir "$S/jena-tmp" "${gz[@]}"
  fi
  if has oxigraph; then
    local oxigraph
    oxigraph=$(nixbin oxigraph oxigraph)
    # the bulk loader from stdin, then the compaction Oxigraph recommends before reads
    # shellcheck disable=SC2016 # expanded by the inner shell
    timed oxigraph "$S/oxigraph.db" bash -c '
      "$1" -dc "${@:4}" | "$2" load --lenient --location "$3" --format nt && "$2" optimize --location "$3"' _ \
      "$ZSTD" "$oxigraph" "$S/oxigraph.db" "${FILES[@]}"
  fi
  if has fluree; then
    # Fluree's parallel bulk import reads a directory of .nt.zst files, as bench.sh's
    # import reads data.nt. Fluree 4.2.2 fails on a file larger than its chunk size (768 MB
    # with the default budget) because it finds no statement boundary where it cuts the
    # file, so the files are split into pieces of whole lines of at most FLUREE_PIECE
    # bytes uncompressed (512 MB by default), which it reads whole.
    if [ ! -f "$S/fluree-input.complete" ]; then
      rm -rf "$S/fluree-input"
      mkdir -p "$S/fluree-input"
      log "fluree: splitting the input into pieces of ${FLUREE_PIECE:-512M}"
      # shellcheck disable=SC2016 # expanded by the inner shell
      printf '%s\0' "${FILES[@]}" | xargs -0 -P "${PREPARE_JOBS:-6}" -I{} bash -c '
        set -o pipefail
        "$1" -dc "$2" | split -C "$4" -d -a 4 --additional-suffix=.nt.zst \
          --filter="\"$1\" -q -o \"\$FILE\"" - "$3/$(basename "$2" .nt.zst)-"' _ \
        "$ZSTD" {} "$S/fluree-input" "${FLUREE_PIECE:-512M}"
      touch "$S/fluree-input.complete"
    fi
    # shellcheck disable=SC2016 # expanded by the inner shell
    timed fluree "$S/fluree" bash -c '
      mkdir -p "$1" && cd "$1" && "$2" init -q &&
      "$2" create dbpedia --from "$3" --memory-budget-mb "$4"' _ \
      "$S/fluree" "$FLUREE" "$S/fluree-input" "${FLUREE_IMPORT_MB:-8192}"
  fi
}

# --------------------------------------------------------------------------- servers
freeport() { python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'; }
declare -A NAME URL READY PORT PID FILESOF
NAME=([sparkles]=sparkles [qlever]=qlever [jena]=jena-fuseki [oxigraph]=oxigraph [fluree]=fluree)
FILESOF=([sparkles]=$S/sparkles.db [qlever]=$S/qlever-index [jena]=$S/jena.db [oxigraph]=$S/oxigraph.db [fluree]=$S/fluree)
pid_on() { ss -ltnp 2> /dev/null | grep ":$1 " | sed -n 's/.*pid=\([0-9]*\).*/\1/p' | head -1 || true; }
start() { # start <engine>
  local e=$1 p
  p=$(freeport)
  PORT[$e]=$p
  case $e in
    sparkles)
      "$SPARKLES" --result-cache-mb 0 serve --data "$S/sparkles-server" --loc dbpedia="$S/sparkles.db" \
        --port "$p" --timeout "$TIMEOUT" --query-memory-mb $((QUERY_MEM_GB * 1024)) --no-access-log > "$S/logs/server-sparkles.log" 2>&1 &
      URL[$e]=localhost:$p/dbpedia/sparql
      READY[$e]="localhost:$p/\$/ping"
      ;;
    qlever)
      (cd "$S/qlever-index" && exec "$(nixbin qlever qlever-server)" -i dbpedia -p "$p" -m "${QUERY_MEM_GB}G" \
        -c 2G -e 0B -s "${TIMEOUT}s" -a bench -j 16 > "$S/logs/server-qlever.log" 2>&1) &
      URL[$e]=localhost:$p/
      READY[$e]="localhost:$p/?cmd=stats"
      ;;
    jena)
      JVM_ARGS="-Xmx${JENA_HEAP:-8G}" "$(nixbin apache-jena-fuseki fuseki-server)" --port "$p" \
        --loc "$S/jena.db" /dbpedia > "$S/logs/server-jena.log" 2>&1 &
      URL[$e]=localhost:$p/dbpedia/sparql
      READY[$e]="localhost:$p/\$/ping"
      ;;
    oxigraph)
      "$(nixbin oxigraph oxigraph)" serve-read-only --location "$S/oxigraph.db" --bind "127.0.0.1:$p" \
        --timeout-s "$TIMEOUT" > "$S/logs/server-oxigraph.log" 2>&1 &
      URL[$e]=localhost:$p/query
      READY[$e]="localhost:$p/query?query=ASK%7B%7D"
      ;;
    fluree)
      # the settings of scripts/bench-lib.sh, with this script's TIMEOUT
      (cd "$S/fluree" && FLUREE_CACHE_MAX_MB=${FLUREE_CACHE_MB:-4096} FLUREE_PATH_MAX_VISITED=20000000 \
        FLUREE_QUERY_TIMEOUT_MS=$((TIMEOUT * 1000)) exec "$FLUREE" server run --listen-addr "127.0.0.1:$p" \
        --storage-path "$S/fluree/.fluree/storage" --log-level warn > "$S/logs/server-fluree.log" 2>&1) &
      URL[$e]=localhost:$p/v1/fluree/query/dbpedia:main
      READY[$e]="localhost:$p/health"
      ;;
  esac
  PID[$e]=$!
  for _ in $(seq 1 1200); do
    curl -sf -o /dev/null --max-time 5 "${READY[$e]}" && return 0
    kill -0 "${PID[$e]}" 2> /dev/null || die "$e: server exited, see $S/logs/server-$e.log"
    sleep 0.5
  done
  die "$e: server did not start"
}
stop() { # stop <engine>: by port, since wrapper scripts (fuseki-server) outlive their JVM
  local e=$1 pid
  [ -n "${PORT[$e]:-}" ] || return 0
  pid=$(pid_on "${PORT[$e]}")
  [ -n "$pid" ] && kill "$pid" 2> /dev/null
  kill "${PID[$e]}" 2> /dev/null || true
  for _ in $(seq 1 120); do
    [ -z "$(pid_on "${PORT[$e]}")" ] && break
    sleep 0.5
  done
  wait "${PID[$e]}" 2> /dev/null || true
  unset "PORT[$e]"
}
stop_all() { for e in $ENGINES; do stop "$e"; done; }
PSI_PID= # the pressure sampler of the preflight, stopped on exit
trap 'stop_all; [ -z "$PSI_PID" ] || kill "$PSI_PID" 2> /dev/null || true' EXIT
# drop an engine's files from the page cache (no root needed: posix_fadvise DONTNEED);
# a running server's mapped pages stay, so this runs between a stop and a start. One
# process for all files: Fluree's index has about 90,000 files at full scale, and a dd
# per file took six minutes.
evict() {
  find "${FILESOF[$1]}" -type f -print0 | python3 -c '
import os, sys
for p in sys.stdin.buffer.read().split(b"\0"):
    if p:
        fd = os.open(p, os.O_RDONLY)
        os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
        os.close(fd)'
}

# --------------------------------------------------------------------------- queries
NAMES=()
for f in "$HERE"/queries/*.rq; do
  n=$(basename "$f" .rq)
  selected "$n" && NAMES+=("$n")
done
q() { # q <url> <query name>: the timed request (fails on HTTP errors)
  echo "curl -sf --max-time $TIMEOUT -o /dev/null -H 'Accept: text/tab-separated-values' --data-urlencode query@$HERE/queries/$2.rq $1"
}
# LIMIT without ORDER BY may legitimately return different solutions: row counts only
COUNT_ONLY="export-1m"
merge() { # merge <new.json> <dest.json>: replace hyperfine results by command name
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
setcold() { # setcold <engine> <query> <seconds|error>
  python3 - "$S/results/cold.json" "$@" << 'EOF'
import json, os, sys
f, e, n, v = sys.argv[1:]
d = json.load(open(f)) if os.path.exists(f) else {}
d.setdefault(e, {})[n] = float(v) if v != "error" else v
json.dump(d, open(f, "w"), indent=1)
EOF
}
step_queries() {
  local e n r
  for e in $ENGINES; do
    [ -f "$S/${NAME[$e]/jena-fuseki/jena-tdb2}.loaded" ] || die "$e: no index at scale $SCALE (run the load step)"
  done
  cd "$S"
  # cold: per query, restart the engine with its files evicted from the page cache
  if [ "$COLD" = 1 ] && [ -z "${ANSWERS_ONLY:-}" ]; then
    for e in $ENGINES; do
      for n in "${NAMES[@]}"; do
        evict "$e"
        start "$e"
        # curl prints the time even when the request fails, so a failure replaces it
        if ! r=$(curl -sf --max-time "$TIMEOUT" -o /dev/null -w '%{time_total}' -H 'Accept: text/tab-separated-values' \
          --data-urlencode "query@$HERE/queries/$n.rq" "${URL[$e]}"); then
          r=error
        fi
        log "cold $e $n: $r"
        setcold "${NAME[$e]}" "$n" "$r"
        stop "$e"
      done
    done
  fi
  for e in $ENGINES; do start "$e"; done
  echo
  printf '%-24s' rows
  for e in $ENGINES; do printf ' %12s' "${NAME[$e]}"; done
  echo
  for n in "${NAMES[@]}"; do
    local flag=()
    [[ " $COUNT_ONLY " == *" $n "* ]] && flag=(--count-only)
    printf '%-24s' "$n"
    for e in $ENGINES; do
      r=$(python3 "$ROOT/scripts/bench-answers.py" "${URL[$e]}" "$HERE/queries/$n.rq" results/answers.json "$n" "${NAME[$e]}" "${flag[@]}")
      printf ' %12s' "$r"
    done
    echo
  done
  [ -n "${ANSWERS_ONLY:-}" ] && return
  local clear=true args
  has qlever && clear="curl -sf -o /dev/null 'localhost:${PORT[qlever]}/?cmd=clear-cache&access-token=bench'"
  for n in "${NAMES[@]}"; do
    echo
    echo "== $n"
    args=()
    for e in $ENGINES; do args+=(--command-name "${NAME[$e]}" "$(q "${URL[$e]}" "$n")"); done
    hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic --ignore-failure --prepare "$clear" \
      "${args[@]}" --export-json "results/$n.new.json"
    merge "results/$n.new.json" "results/$n.json"
  done
  for n in $THROUGHPUT; do
    selected "$n" || continue
    echo
    echo "== throughput $n ($NREQ requests, $CONC clients)"
    args=()
    for e in $ENGINES; do
      args+=(--command-name "${NAME[$e]}" "seq $NREQ | xargs -P $CONC -I{} $(q "${URL[$e]}" "$n")")
    done
    hyperfine --warmup 1 --runs 3 --style basic --ignore-failure --prepare "$clear" \
      "${args[@]}" --export-json "results/throughput-$n.new.json"
    merge "results/throughput-$n.new.json" "results/throughput-$n.json"
  done
  sleep 2 # idle servers may hand free memory back
  local kv=()
  for e in $ENGINES; do
    kv+=("${NAME[$e]}=$(awk '/VmRSS/ {printf "%.0f", $2/1024}' "/proc/$(pid_on "${PORT[$e]}")/status" 2> /dev/null || echo "?")")
  done
  python3 - "$S/results/rss.json" "${kv[@]}" << 'EOF'
import json, os, sys
f = sys.argv[1]
d = json.load(open(f)) if os.path.exists(f) else {}
d.update(kv.split("=", 1) for kv in sys.argv[2:])
json.dump(d, open(f, "w"), indent=1)
EOF
  log "server RSS (MiB): $(cat "$S/results/rss.json")"
}

step_report() {
  {
    echo "Scale $SCALE: $(wc -l < "$NT/counts.tsv") files of $(basename "$MANIFEST"); engines: $ENGINES"
    echo
    python3 "$ROOT/scripts/bench-summary.py" "$S/results" "${NAMES[@]}"
  } >&2
  log "summary: $S/results/summary.md"
}

# ------------------------------------------------------------------------- preflight
# A full load writes about 145 GB. On Forge (an SSD without online discard) DBpedia
# loads went from 596 s to 1,351 s and back to 547 s after an fstrim. The extra time was
# write stall on a drive that had not been trimmed for days. Before a load or a
# query step, record how long ago the filesystem was trimmed, how full it is and the
# PSI I/O pressure, warn when they look like that state, and sample PSI once per second
# while the steps run (logs/psi-<time>.tsv). This warns and never refuses.
# TRIM_WARN_DAYS (default 2) and FREE_WARN_PCT (default 30) set the thresholds.
# last_trim <mount point>: the epoch of the last trim of that filesystem that the journal
# shows (by fstrim.service, or an fstrim run through sudo), or nothing
last_trim() {
  command -v journalctl > /dev/null || return 0
  {
    journalctl -u fstrim.service -o short-unix --no-pager -q -g ' trimmed on ' 2> /dev/null |
      awk -v m="$1:" '$4 == m {print int($1)}'
    journalctl -t sudo -o short-unix --no-pager -q -g 'COMMAND=[^ ]*/fstrim( |$)' 2> /dev/null | awk '{print int($1)}'
  } | sort -n | tail -1 || true
}
preflight() {
  [ "$(uname -s)" = Linux ] || return 0
  local stamp f out t now days size avail pct
  stamp=$(date +%Y%m%dT%H%M%S)
  f=$S/logs/preflight-$stamp.txt
  now=$(date +%s)
  t=$(last_trim "$(findmnt -no TARGET --target "$S" 2> /dev/null)")
  read -r size avail < <(df -B1 --output=size,avail "$S" | tail -1)
  pct=$((avail * 100 / size))
  {
    echo "date $(date -Is)"
    echo "steps ${STEPS[*]}"
    echo "filesystem $(df --output=source,target "$S" | tail -1)"
    echo "size_bytes $size"
    echo "avail_bytes $avail"
    echo "avail_pct $pct"
    if [ -n "$t" ]; then
      echo "last_trim $(date -d "@$t" -Is)"
      echo "days_since_trim $(awk "BEGIN {printf \"%.1f\", ($now - $t) / 86400}")"
    else
      echo "last_trim unknown"
    fi
    echo "fstrim_timer_last $(systemctl show fstrim.timer -p LastTriggerUSec --value 2> /dev/null)"
    echo "fstrim_timer_next $(systemctl show fstrim.timer -p NextElapseUSecRealtime --value 2> /dev/null)"
    echo "mount_options $(findmnt -no OPTIONS --target "$S" 2> /dev/null)"
    sed 's/^/io_pressure /' /proc/pressure/io 2> /dev/null
    echo "dirty_ratio $(cat /proc/sys/vm/dirty_ratio 2> /dev/null) dirty_background_ratio $(cat /proc/sys/vm/dirty_background_ratio 2> /dev/null)"
  } > "$f"
  log "preflight: $((avail >> 30)) GiB free ($pct% of the filesystem), last trim $([ -n "$t" ] && date -d "@$t" '+%F %H:%M' || echo unknown) ($f)"
  if [ -z "$t" ]; then
    log "warning: no trim of the filesystem found in the journal; on an SSD without online discard, writes may stall (fstrim -v / before a full load)"
  else
    days=$(((now - t) / 86400))
    [ "$days" -ge "${TRIM_WARN_DAYS:-2}" ] &&
      log "warning: the filesystem was last trimmed $days days ago; on an SSD without online discard, writes may stall (fstrim -v / before a full load)"
  fi
  [ "$pct" -lt "${FREE_WARN_PCT:-30}" ] &&
    log "warning: only $pct% of the filesystem is free; a full load needs about 100 GiB of temporary space and a full SSD writes more slowly"
  if [ -r /proc/pressure/io ] && [ -z "$PSI_PID" ]; then
    out=$S/logs/psi-$stamp.tsv
    # cumulative stall in microseconds (the total= fields of /proc/pressure/*), once per second
    (
      echo -e "epoch\tio_some_us\tio_full_us\tcpu_some_us\tmem_some_us\tmem_full_us\tnr_dirty\tnr_writeback"
      while :; do
        echo -e "$(date +%s.%N)\t$(awk '{split($5, a, "="); printf "%s\t", a[2]}' /proc/pressure/io)$(awk 'NR == 1 {split($5, a, "="); printf "%s\t", a[2]}' /proc/pressure/cpu)$(awk '{split($5, a, "="); printf "%s\t", a[2]}' /proc/pressure/memory)$(awk '$1 == "nr_dirty" {d = $2} $1 == "nr_writeback" {w = $2} END {printf "%s\t%s", d, w}' /proc/vmstat)"
        sleep 1
      done
    ) > "$out" 2> /dev/null &
    PSI_PID=$!
    log "PSI pressure sampled once per second to $out"
  fi
  return 0
}
STEPS=("$@")
[ ${#STEPS[@]} -gt 0 ] || STEPS=(fetch prepare slice load queries report)
[[ " ${STEPS[*]} " == *" load "* || " ${STEPS[*]} " == *" queries "* ]] && preflight
for st in "${STEPS[@]}"; do
  case $st in
    fetch | prepare | slice | load | queries | report) "step_$st" ;;
    *) die "unknown step $st (fetch prepare slice load queries report)" ;;
  esac
done
