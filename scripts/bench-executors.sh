#!/usr/bin/env bash
# The eager-against-cursor matrix of ROADMAP item 469: the 31 queries of scripts/bench.sh
# with execution=eager and execution=streaming, at 1.05M and 10.5M triples, with 12 Rayon
# threads and with one, on this host. See docs/benchmarks/synthetic/README.md.
#
#   scripts/bench-executors.sh [options] SPARKLES_BINARY
#
# Each cell (scale and thread count) runs scripts/nsc-bench-remote.sh in OUT/<cell> with
# both variants on the one binary and one shared store: A sends execution=eager and B
# execution=streaming with every query. ROUNDS rounds of A, B, B, A each start a fresh
# server process. Before the first timed request each variant's sorted TSV answer to every
# query is checksummed, so the cell also checks that both executors agree. A one-thread cell
# pins the server to CPU 2 and the client to CPU 4 with idle-priority spinners, as
# scripts/nsc-bench.sh does. A cell of T threads pins the server to CPUs 0 to T-1 and the
# client to CPU T, without spinners. On forge, CPUs 0 to 11 are the six performance cores
# and their hyperthreads, and CPU 12 is an efficiency core.
#
# OUT/<cell>/summary.md is the per-cell A/B summary of scripts/nsc-bench-summary.py, where
# B/A is streaming over eager. OUT/record.json is the gate record of
# scripts/bench-gate.py, and `scripts/bench-gate.py compare BASELINE OUT/record.json`
# checks a later build against a recorded baseline.
#
# Run it alone on the benchmark host, holding its locks, for example on forge:
#   flock ~/lanes/.bench.lock flock ~/lanes/.heavy.lock scripts/bench-executors.sh BIN
# The script refuses to start while a compiler or another engine's server runs.
#
# Options:
#   --out DIR         results directory (default .artifacts/bench-executors/<UTC time>)
#   --scales "N ..."  dataset sizes in people (default "100000 1000000", 1.05M and 10.5M)
#   --threads "T ..." Rayon thread counts (default "12 1")
#   --rounds N        A,B,B,A rounds per cell (default 2)
#   --reps N          timed requests per query and server process (default 10)
#   --warmup N        untimed requests per query and server process (default 3)
#   --queries "a b"   only these queries (default: all 31 of scripts/bench.sh)
#   --data-dir DIR    cache of generated datasets (default ~/.cache/sparkles-nsc/data,
#                     shared with scripts/nsc-bench.sh)
#   --port P          server port (default 4181, below the ephemeral port range)
#   --commit REV      the commit the binary was built from, for the record
#   --build-config J  the binary's build configuration, the output of
#                     `scripts/bench-gate.py build-config` in its build environment;
#                     compare refuses a record without one
#   --force           start even when a compiler or another server is running
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=
SCALES="100000 1000000"
THREADS="12 1"
ROUNDS=2
REPS=10
WARMUP=3
QUERIES=
DATA_DIR=${SPARKLES_NSC_CACHE:-$HOME/.cache/sparkles-nsc}/data
PORT=4181
COMMIT=
BUILD_CONFIG=
FORCE=
die() {
  echo "bench-executors: $*" >&2
  exit 1
}
while [ $# -gt 0 ]; do
  case $1 in
    --out) OUT=$2 ;;
    --scales) SCALES=$2 ;;
    --threads) THREADS=$2 ;;
    --rounds) ROUNDS=$2 ;;
    --reps) REPS=$2 ;;
    --warmup) WARMUP=$2 ;;
    --queries) QUERIES=$2 ;;
    --data-dir) DATA_DIR=$2 ;;
    --port) PORT=$2 ;;
    --commit) COMMIT=$2 ;;
    --build-config) BUILD_CONFIG=$2 ;;
    --force)
      FORCE=1
      shift
      continue
      ;;
    -h | --help)
      sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
      exit 0
      ;;
    -*) die "unknown option $1" ;;
    *) break ;;
  esac
  shift 2
done
[ $# -eq 1 ] || die "usage: bench-executors.sh [options] SPARKLES_BINARY"
BIN=$(realpath "$1")
[ -x "$BIN" ] || die "$BIN is not executable"
OUT=$(realpath -m "${OUT:-$ROOT/.artifacts/bench-executors/$(date -u +%Y%m%dT%H%M%SZ)}")
[ ! -e "$OUT" ] || die "$OUT exists; give a new --out"
NCPU=$(nproc --all)
for t in $THREADS; do
  [ "$t" -ge 1 ] && [ "$t" -lt "$NCPU" ] || die "--threads $t needs at least $((t + 1)) CPUs"
done

# The processes that launched this script, such as `mise run` and `mbx sparkles-task`,
# which hold the managed target's lease, are not competing work.
ancestors=" "
pid=$$
while [ "$pid" -gt 1 ] 2> /dev/null; do
  ancestors+="$pid "
  pid=$(ps -o ppid= -p "$pid" 2> /dev/null | tr -d ' ' || echo 1)
done
if [ -z "$FORCE" ]; then
  for p in rustc cargo mbx java qlever-server oxigraph fluree sparkles; do
    for other in $(pgrep -x "$p" || true); do
      [[ $ancestors == *" $other "* ]] && continue
      ps -eo pid,args --sort=-pcpu | head -8 >&2
      die "$p (PID $other) is running; stop it or pass --force"
    done
  done
fi

mkdir -p "$OUT" "$DATA_DIR"
DATA_DIR=$(realpath "$DATA_DIR")
log() { echo "$(date -u +%H:%M:%S) $*" | tee -a "$OUT/progress.log"; }

# the queries of scripts/bench.sh: its `P='…'` line and `add name '…'` lines
mkdir -p "$OUT/queries"
P=$(sed -n "s/^P='\(.*\)'$/\1/p" "$ROOT/scripts/bench.sh")
while IFS=$'\t' read -r n q; do
  printf '%s%s' "$P" "$q" > "$OUT/queries/$n.rq"
done < <(sed -n "s/^add \([a-z0-9-]*\) '\(.*\)'$/\1\t\2/p" "$ROOT/scripts/bench.sh")
if [ -n "$QUERIES" ]; then
  for f in "$OUT"/queries/*.rq; do
    n=$(basename "$f" .rq)
    [[ " $QUERIES " == *" $n "* ]] || rm "$f"
  done
  for n in $QUERIES; do [ -f "$OUT/queries/$n.rq" ] || die "no query named $n"; done
fi
NQ=$(find "$OUT/queries" -name '*.rq' | wc -l)
[ "$NQ" -gt 0 ] || die "no queries"

{
  echo "binary: $BIN"
  echo "binary sha256: $(sha256sum "$BIN" | cut -d' ' -f1)"
  echo "binary commit: ${COMMIT:-unknown}"
  echo "version: $("$BIN" --version 2> /dev/null || echo unknown)"
  # the compilers, from the binary's .comment section, for compare's toolchain check
  echo "compilers: $(python3 "$ROOT/scripts/bench-gate.py" compilers "$BIN")"
  [ -z "$BUILD_CONFIG" ] || echo "build config: $BUILD_CONFIG"
  echo "scripts commit: $(git -C "$ROOT" rev-parse HEAD 2> /dev/null || echo unknown)"
  echo "scripts dirty: $(git -C "$ROOT" status --porcelain 2> /dev/null | grep -c . || true)"
  echo "host: $(hostname)"
  echo "started: $(date -u +%FT%TZ)"
  echo "settings: $NQ queries, scales $SCALES people, threads $THREADS, $ROUNDS rounds of A,B,B,A, $WARMUP warm-up and $REPS timed requests per query and server"
  echo "variants: A execution=eager, B execution=streaming, one binary and one shared store"
  echo "load before: $(cat /proc/loadavg)"
} > "$OUT/manifest.txt"
log "matrix of $NQ queries into $OUT"

for people in $SCALES; do
  data=$DATA_DIR/people-$people.nt.zst
  if [ ! -f "$data" ]; then
    log "generating $people people"
    python3 "$ROOT/scripts/gen-data.py" "$people" | zstd -q -T0 -10 -o "$data.tmp"
    mv "$data.tmp" "$data"
  fi
  for t in $THREADS; do
    cell=people-$people-threads-$t
    d=$OUT/$cell
    mkdir -p "$d"
    cp -r "$OUT/queries" "$d/queries"
    cp "$ROOT/scripts/nsc-bench-remote.sh" "$d/"
    ln -s "$data" "$d/data.nt.zst"
    zstd -q -1 -f "$BIN" -o "$d/bin-A.zst"
    cp "$d/bin-A.zst" "$d/bin-B.zst"
    if [ "$t" = 1 ]; then
      server_cpu=2 client_cpu=4 spin=1
    else
      server_cpu=0-$((t - 1)) client_cpu=$t spin=0
    fi
    # the binary runs on the host it was built for, so it needs no other loader
    cat > "$d/run.env" << ENV
DYNAMIC_A=0
DYNAMIC_B=0
ROUNDS=$ROUNDS
REPS=$REPS
WARMUP=$WARMUP
RAYON_THREADS=$t
SERVER_CPU=$server_cpu
CLIENT_CPU=$client_cpu
SPIN=$spin
PORT=$PORT
EXECUTION=eager,streaming
SHARE_STORE=1
ORDER_DIGEST="python3 $ROOT/scripts/bench-gate.py order-digest"
ENV
    {
      sed -n '1,/^settings:/p' "$OUT/manifest.txt" | grep -v '^settings:'
      echo "settings: $NQ queries, $ROUNDS rounds of A,B,B,A, $WARMUP warm-up and $REPS timed requests per query and server process, RAYON_NUM_THREADS=$t, server CPU $server_cpu, client CPU $client_cpu, spinners $([ "$spin" = 1 ] && echo on || echo off), execution eager,streaming"
      echo "A: $BIN"
      echo "B: $BIN"
    } > "$d/manifest.txt"
    log "$cell: start, load $(cut -d' ' -f1-3 /proc/loadavg)"
    bash "$d/nsc-bench-remote.sh" > "$d/remote.log" 2>&1 || die "$cell failed, see $d/results/progress.log"
    grep -q '^ok' "$d/results/done" || die "$cell did not finish: $(cat "$d/results/done")"
    rm -rf "$d/stores" "$d/srv" "$d/data.nt" "$d"/bin-*.zst "$d/A" "$d/B" "$d/resp.json" "$d/ans.tsv"
    python3 "$ROOT/scripts/nsc-bench-summary.py" "$d" > "$d/summary.md"
    log "$cell: done"
  done
done
echo "ended: $(date -u +%FT%TZ)" >> "$OUT/manifest.txt"
echo "load after: $(cat /proc/loadavg)" >> "$OUT/manifest.txt"
python3 "$ROOT/scripts/bench-gate.py" executors "$OUT" --out "$OUT/record.json" > "$OUT/summary.md"
log "finished: $OUT/summary.md and $OUT/record.json"
