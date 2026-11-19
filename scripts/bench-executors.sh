#!/usr/bin/env bash
# The eager-against-cursor matrix of ROADMAP item 469: the 31 queries of scripts/bench.sh
# with execution=eager and execution=streaming, at 1.05M and 10.5M triples, with 12 Rayon
# threads and with one, on this host. See docs/benchmarks/synthetic/README.md.
#
#   scripts/bench-executors.sh [options] SPARKLES_BINARY
#   scripts/bench-executors.sh [options] --base BASE_BINARY TIP_BINARY
#
# Each cell (scale and thread count) runs scripts/nsc-bench-remote.sh in OUT/<cell> with
# one arm per binary and execution mode. With one binary the arms are its --modes, by
# default eager and streaming, served from one shared store: A sends execution=eager and B
# execution=streaming with every query, and ROUNDS rounds of A, B, B, A each start a fresh
# server process. Another list of modes gives a round of every arm in order and then in
# reverse.
#
# With --base, the arms are the base binary in each of --base-modes (default eager and
# streaming) and the tip binary in each of --modes (default "default streaming", where
# "default" sends no execution field), named base-MODE and tip-MODE. Each binary loads its
# own store, which its arms share, since the two may differ in format. A round alternates
# the binaries, base-eager, tip-default, base-streaming and tip-streaming, and then runs
# them in reverse. The next round starts with the tip, so that each binary takes each
# position equally often, and ROUNDS must be even. `scripts/bench-gate.py compare
# OUT/record.json --best-of base-eager,base-streaming` scores each tip arm against the
# faster base arm of each query and cell. The two binaries must come from one compiler
# and, when both are given, one build configuration.
#
# Before the first timed request each arm's sorted TSV answer to every query is
# checksummed, so the cell also checks that every arm agrees. A one-thread cell
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
#                     `scripts/bench-gate.py build-config --cargo ARGS` in its build environment;
#                     compare refuses a record without one
#   --modes "M ..."   the execution modes of the binary, or of the tip with --base
#                     (default "eager streaming", or "default streaming" with --base)
#   --base BIN        the base binary, which makes the run a four-arm A/B of two binaries
#   --base-modes "M ..." the base binary's modes (default "eager streaming")
#   --base-commit REV and --base-build-config J  the same as --commit and
#                     --build-config, for the base binary
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
MODES=
BASE=
BASE_MODES="eager streaming"
BASE_COMMIT=
BASE_BUILD_CONFIG=
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
    --modes) MODES=$2 ;;
    --base) BASE=$2 ;;
    --base-modes) BASE_MODES=$2 ;;
    --base-commit) BASE_COMMIT=$2 ;;
    --base-build-config) BASE_BUILD_CONFIG=$2 ;;
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
[ $# -eq 1 ] || die "usage: bench-executors.sh [options] [--base BASE_BINARY] SPARKLES_BINARY"
BIN=$(realpath "$1")
[ -x "$BIN" ] || die "$BIN is not executable"
[[ $ROUNDS =~ ^[1-9][0-9]*$ ]] || die "--rounds must be a positive number"
if [ -n "$BASE" ]; then
  BASE=$(realpath "$BASE")
  [ -x "$BASE" ] || die "$BASE is not executable"
  MODES=${MODES:-default streaming}
  [ $((ROUNDS % 2)) = 0 ] || die "--rounds must be even with --base, so each binary leads equally often"
else
  MODES=${MODES:-eager streaming}
  [ -z "$BASE_COMMIT$BASE_BUILD_CONFIG" ] || die "--base-commit and --base-build-config need --base"
fi
# The arms in the order of a round's first half: the variant letter, its name, the binary
# variant it runs (A for the base, or the only binary, and B for the tip) and its mode.
ARM_LETTERS=(A B C D E F G H)
ARMS=()
add_arm() { # <name> <binary variant> <mode>
  [ ${#ARMS[@]} -lt ${#ARM_LETTERS[@]} ] || die "at most ${#ARM_LETTERS[@]} arms"
  [[ $3 =~ ^[a-z][a-z-]*$ ]] || die "a mode is a lowercase word, not $3"
  ARMS+=("${ARM_LETTERS[${#ARMS[@]}]} $1 $2 $3")
}
read -r -a tip_modes <<< "$MODES"
[ ${#tip_modes[@]} -gt 0 ] || die "--modes is empty"
if [ -n "$BASE" ]; then
  read -r -a base_modes <<< "$BASE_MODES"
  [ ${#base_modes[@]} -gt 0 ] || die "--base-modes is empty"
  n=$((${#base_modes[@]} > ${#tip_modes[@]} ? ${#base_modes[@]} : ${#tip_modes[@]}))
  for i in $(seq 0 $((n - 1))); do
    [ -z "${base_modes[$i]:-}" ] || add_arm "base-${base_modes[$i]}" A "${base_modes[$i]}"
    [ -z "${tip_modes[$i]:-}" ] || add_arm "tip-${tip_modes[$i]}" B "${tip_modes[$i]}"
  done
else
  for m in "${tip_modes[@]}"; do add_arm "$m" A "$m"; done
fi
names=$(printf '%s\n' "${ARMS[@]}" | cut -d' ' -f2 | sort | uniq -d)
[ -z "$names" ] || die "an arm is named twice: $names"
LEGACY=
[ -z "$BASE" ] && [ "$MODES" = "eager streaming" ] && LEGACY=1
VARIANTS="" EXEC="" ARM_MAP="" FIRST="" REVERSE=""
for a in "${ARMS[@]}"; do
  read -r v name b m <<< "$a"
  VARIANTS+="${VARIANTS:+ }$v"
  EXEC+="${EXEC:+,}$([ "$m" = default ] || echo "$m")"
  ARM_MAP+="${ARM_MAP:+ }$v=$name"
  FIRST+="${FIRST:+ }$v"
  REVERSE="$v${REVERSE:+ }$REVERSE"
done
# the tip-first order: each base arm swapped with the tip arm beside it
SWAPPED=
if [ -n "$BASE" ]; then
  read -r -a first <<< "$FIRST"
  i=0
  while [ $i -lt ${#first[@]} ]; do
    a=${first[$i]} b=${first[$((i + 1))]:-}
    if [ -n "$b" ] && [[ " ${ARMS[*]} " == *" $a base-"* ]] && [[ " ${ARMS[*]} " == *" $b tip-"* ]]; then
      SWAPPED+="${SWAPPED:+ }$b $a"
      i=$((i + 2))
    else
      SWAPPED+="${SWAPPED:+ }$a"
      i=$((i + 1))
    fi
  done
  rev_swapped=$(echo "$SWAPPED" | tr ' ' '\n' | tac | tr '\n' ' ' | sed 's/ $//')
  ORDERS="$FIRST $REVERSE;$SWAPPED $rev_swapped"
else
  ORDERS="$FIRST $REVERSE"
fi
ROUND_TEXT=${ORDERS//;/ then }
ROUND_TEXT=${ROUND_TEXT// /,}
ROUND_TEXT=${ROUND_TEXT//,then,/ then }
[ -z "$LEGACY" ] || ROUND_TEXT=A,B,B,A
if [ -n "$BASE" ] && [ -z "$FORCE" ]; then
  bc=$(python3 "$ROOT/scripts/bench-gate.py" compilers "$BASE")
  tc=$(python3 "$ROOT/scripts/bench-gate.py" compilers "$BIN")
  [ "$bc" = "$tc" ] || die "the base was built by $bc and the tip by $tc; pass --force to run anyway"
  [ -z "$BASE_BUILD_CONFIG" ] || [ -z "$BUILD_CONFIG" ] || [ "$BASE_BUILD_CONFIG" = "$BUILD_CONFIG" ] ||
    die "the base and the tip have different build configurations; pass --force to run anyway"
fi
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
  if [ -n "$BASE" ]; then
    echo "base binary: $BASE"
    echo "base binary sha256: $(sha256sum "$BASE" | cut -d' ' -f1)"
    echo "base binary commit: ${BASE_COMMIT:-unknown}"
    echo "base version: $("$BASE" --version 2> /dev/null || echo unknown)"
    echo "base compilers: $(python3 "$ROOT/scripts/bench-gate.py" compilers "$BASE")"
    [ -z "$BASE_BUILD_CONFIG" ] || echo "base build config: $BASE_BUILD_CONFIG"
  fi
  # the cells run with a clean environment, so SPARKLES_ switches and the remote
  # runner's own options, such as SERVE_ARGS, come only from run.env
  echo "runtime env: clean"
  echo "scripts commit: $(git -C "$ROOT" rev-parse HEAD 2> /dev/null || echo unknown)"
  echo "scripts dirty: $(git -C "$ROOT" status --porcelain 2> /dev/null | grep -c . || true)"
  echo "host: $(hostname)"
  echo "started: $(date -u +%FT%TZ)"
  if [ -n "$LEGACY" ]; then
    echo "settings: $NQ queries, scales $SCALES people, threads $THREADS, $ROUNDS rounds of A,B,B,A, $WARMUP warm-up and $REPS timed requests per query and server"
    echo "variants: A execution=eager, B execution=streaming, one binary and one shared store"
  else
    echo "settings: $NQ queries, scales $SCALES people, threads $THREADS, $ROUNDS rounds of $ROUND_TEXT, $WARMUP warm-up and $REPS timed requests per query and server"
    if [ -n "$BASE" ]; then
      echo "variants: $ARM_MAP, a base and a tip binary with one store each"
    else
      echo "variants: $ARM_MAP, one binary and one shared store"
    fi
  fi
  echo "arms: $ARM_MAP"
  echo "generator sha256: $(sha256sum "$ROOT/scripts/gen-data.py" | cut -d' ' -f1)"
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
  # the digest of the uncompressed data, which compare requires to match
  data_sha=$(zstd -dc "$data" | sha256sum | cut -d' ' -f1)
  for t in $THREADS; do
    cell=people-$people-threads-$t
    d=$OUT/$cell
    mkdir -p "$d"
    cp -r "$OUT/queries" "$d/queries"
    cp "$ROOT/scripts/nsc-bench-remote.sh" "$d/"
    ln -s "$data" "$d/data.nt.zst"
    if [ -n "$BASE" ]; then
      zstd -q -1 -f "$BASE" -o "$d/bin-A.zst"
      zstd -q -1 -f "$BIN" -o "$d/bin-B.zst"
    else
      zstd -q -1 -f "$BIN" -o "$d/bin-A.zst"
    fi
    owners=
    for a in "${ARMS[@]}"; do
      read -r v _ b _ <<< "$a"
      [ "$v" = "$b" ] || owners+="BINARY_OF_$v=$b"$'\n'
    done
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
VARIANTS="$VARIANTS"
ORDERS="$ORDERS"
EXECUTION=$EXEC
${owners}ORDER_DIGEST="python3 $ROOT/scripts/bench-gate.py order-digest --data data.nt"
WARM_DIGEST="python3 $ROOT/scripts/bench-gate.py native-digest --data data.nt"
ENV
    {
      sed -n '1,/^settings:/p' "$OUT/manifest.txt" | grep -v '^settings:'
      echo "settings: $NQ queries, $ROUNDS rounds of $ROUND_TEXT, $WARMUP warm-up and $REPS timed requests per query and server process, RAYON_NUM_THREADS=$t, server CPU $server_cpu, client CPU $client_cpu, spinners $([ "$spin" = 1 ] && echo on || echo off), execution $EXEC"
      for a in "${ARMS[@]}"; do
        read -r v name b _ <<< "$a"
        bin=$BIN
        [ "$b" = A ] && [ -n "$BASE" ] && bin=$BASE
        if [ -n "$LEGACY" ]; then echo "$v: $bin"; else echo "$v: $bin ($name)"; fi
      done
      echo "data sha256: $data_sha"
    } > "$d/manifest.txt"
    log "$cell: start, load $(cut -d' ' -f1-3 /proc/loadavg)"
    env -i PATH="$PATH" HOME="$HOME" TMPDIR="${TMPDIR:-/tmp}" LANG="${LANG:-C.UTF-8}" \
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
