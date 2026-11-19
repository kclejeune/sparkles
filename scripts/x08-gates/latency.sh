#!/usr/bin/env bash
# The latency gate for queries under 0.3 ms, through scripts/nsc-bench-remote.sh on this
# host, in two forms.
#
#   scripts/x08-gates/latency.sh WORK BIN [ROUNDS] [REPS]   (after prepare.sh WORK)
#   scripts/x08-gates/latency.sh --base BASE --tip TIP [options] WORK
#
# The first form is the X08 Phase 1 gate. One build runs with the query memory pool on and
# with `--total-query-memory-mb 0`, both from one shared store, in both orders and once
# against itself. Then combine.py estimates the pool's effect with the position bias
# cancelled and applies the gate.
#
# The second form is the Intel rule of an executor change. The base and the tip binary
# each load their own store, since their formats may differ. One run has the base as A and
# the tip as B, another the tip as A and the base as B, and a third the tip against itself
# from one store. combine.py --labels base,tip then estimates the tip's time over the
# base's with the position bias cancelled, and a query passes when the upper end of the
# ratio's 95% interval is at most 1.02 or its median added time is at most 0.5 µs. Every
# run checks each binary's untimed TSV answer and the digest of every timed native answer,
# and answers.py fails the gate when the binaries or the timed answers disagree. The two
# binaries must come from one compiler, and from one build configuration when both are
# given, unless --force is given. Each run gets a clean environment, so that no inherited
# setting changes which binary, store or mode a variant uses.
#
#   --base-execution M, --tip-execution M   the execution field each binary's requests
#                       send, such as eager or streaming (default none, the server's default)
#   --queries "q ..."   the queries, each a name in WORK/queries or a path to a .rq file
#                       (default the ten queries of the X08 gate)
#   --rounds N          A,B,B,A rounds per run (default 20, at least 10)
#   --reps N            timed requests per query and server process (default 300)
#   --port P            the server port (default 4171)
#   --base-build-config J, --tip-build-config J   the output of `scripts/bench-gate.py
#                       build-config` in each binary's build environment
#   --force             run two binaries of different compilers or build configurations
#
# Run it on a quiet host while holding the host's benchmark lock. It pins the servers to
# CPU 2 and the client to CPU 4, with one Rayon thread.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
USAGE="usage: latency.sh WORK BIN [ROUNDS] [REPS], or latency.sh --base BASE --tip TIP [options] WORK"
QS="distinct-obj count-all predicate-counts types-grouped path-plus values-star employee-docs star-lookup subquery-agg minus"
BASE="" TIP="" BASE_EXEC="" TIP_EXEC="" PORT=4171 ROUNDS=20 REPS=300 BASE_CONFIG="" TIP_CONFIG="" FORCE=""
die() {
  echo "latency: $*" >&2
  exit 2
}
if [[ ${1:-} == --* ]]; then
  while [ $# -gt 1 ]; do
    if [ "$1" = --force ]; then
      FORCE=1
      shift
      continue
    fi
    case $1 in
      --base-build-config) BASE_CONFIG=$2 ;;
      --tip-build-config) TIP_CONFIG=$2 ;;
      --base) BASE=$2 ;;
      --tip) TIP=$2 ;;
      --base-execution) BASE_EXEC=$2 ;;
      --tip-execution) TIP_EXEC=$2 ;;
      --queries) QS=$2 ;;
      --rounds) ROUNDS=$2 ;;
      --reps) REPS=$2 ;;
      --port) PORT=$2 ;;
      *) die "unknown option $1; $USAGE" ;;
    esac
    shift 2
  done
  [ $# = 1 ] || die "$USAGE"
  [ -n "$BASE" ] && [ -n "$TIP" ] || die "the base-and-tip form needs --base and --tip"
  W=$(realpath "$1")
  BASE=$(realpath "$BASE")
  TIP=$(realpath "$TIP")
  [ -x "$BASE" ] && [ -x "$TIP" ] || die "the base and the tip must be executable"
  for m in "$BASE_EXEC" "$TIP_EXEC"; do
    [[ $m =~ ^[a-z-]*$ ]] || die "an execution mode is a lowercase word, not $m"
  done
  BASE_COMPILERS=$(python3 "$ROOT/scripts/bench-gate.py" compilers "$BASE" 2> /dev/null || true)
  TIP_COMPILERS=$(python3 "$ROOT/scripts/bench-gate.py" compilers "$TIP" 2> /dev/null || true)
  if [ -z "$FORCE" ]; then
    [ -n "$BASE_COMPILERS" ] && [ -n "$TIP_COMPILERS" ] ||
      die "a binary names no compilers in its .comment section; pass --force to run anyway"
    [ "$BASE_COMPILERS" = "$TIP_COMPILERS" ] ||
      die "the base was built by $BASE_COMPILERS and the tip by $TIP_COMPILERS; pass --force to run anyway"
    [ -z "$BASE_CONFIG" ] || [ -z "$TIP_CONFIG" ] || [ "$BASE_CONFIG" = "$TIP_CONFIG" ] ||
      die "the base and the tip have different build configurations; pass --force to run anyway"
  fi
else
  W=$(realpath "${1:?$USAGE}")
  BIN=$(realpath "${2:?$USAGE}")
  ROUNDS=${3:-20}
  REPS=${4:-300}
fi
[[ $ROUNDS =~ ^[0-9]+$ ]] && [ "$ROUNDS" -ge 10 ] || die "at least 10 rounds, as combine.py requires"
[ -f "$W/nsc-bench-remote.sh" ] && [ -f "$W/data.nt.zst" ] || die "run prepare.sh $W first"
read -r -a QUERIES <<< "$QS"
[ ${#QUERIES[@]} -gt 0 ] || die "no queries"
# each query's file, staged under its own name, which must be unique
SOURCES=()
names=" "
for q in "${QUERIES[@]}"; do
  if [[ $q == */* || $q == *.rq ]]; then
    [[ $q == *.rq ]] || die "a query file must end in .rq, not $q"
    [ -f "$q" ] || die "no query file $q"
    src=$(realpath "$q")
  else
    src=$W/queries/$q.rq
    [ -f "$src" ] || die "no query $q in $W/queries"
  fi
  n=$(basename "$src")
  [[ $names != *" $n "* ]] || die "two queries are named ${n%.rq}"
  names+="$n "
  SOURCES+=("$src")
done

run() { # <name> <binary A> <binary B> <env lines>
  local d=$W/runs/$1
  rm -rf "$d"
  mkdir -p "$d/queries"
  for src in "${SOURCES[@]}"; do cp "$src" "$d/queries/"; done
  [ "$(find "$d/queries" -name '*.rq' | wc -l)" = ${#SOURCES[@]} ] || die "$1: the staged queries do not match"
  zstd -q -1 -f "$2" -o "$d/bin-A.zst"
  zstd -q -1 -f "$3" -o "$d/bin-B.zst"
  ln -s "$W/data.nt.zst" "$d/data.nt.zst"
  cp "$W/nsc-bench-remote.sh" "$d/"
  # the build runs on the host it was built for, so it needs no other loader
  cat > "$d/run.env" << ENV
DYNAMIC_A=0
DYNAMIC_B=0
VARIANTS="A B"
ORDERS="A B B A"
ROUNDS=$ROUNDS
REPS=$REPS
WARMUP=3
RAYON_THREADS=1
SERVER_CPU=2
CLIENT_CPU=4
SPIN=1
PORT=$PORT
$4
ENV
  echo "$1: $(uptime)"
  # a clean environment, so that nothing inherited, such as BINARY_OF_B, changes a variant
  env -i PATH="$PATH" HOME="$HOME" TMPDIR="${TMPDIR:-/tmp}" LANG="${LANG:-C.UTF-8}" \
    bash "$d/nsc-bench-remote.sh" > /dev/null || die "$1 failed, see $d/results/progress.log"
  rm -rf "$d/stores" "$d/srv" "$d/data.nt" "$d"/bin-*.zst
}
if [ -z "$BASE" ]; then
  run pool-on-second "$BIN" "$BIN" "SERVE_ARGS_A=\"--total-query-memory-mb 0\"
SERVE_ARGS_B=\"\"
SHARE_STORE=1"
  run pool-on-first "$BIN" "$BIN" "SERVE_ARGS_A=\"\"
SERVE_ARGS_B=\"--total-query-memory-mb 0\"
SHARE_STORE=1"
  run same "$BIN" "$BIN" "SERVE_ARGS_A=\"\"
SERVE_ARGS_B=\"\"
SHARE_STORE=1"
  python3 "$HERE/combine.py" "$W/runs/pool-on-second/results/samples.tsv" \
    "$W/runs/pool-on-first/results/samples.tsv" "$W/runs/same/results/samples.tsv"
else
  {
    echo "base: $BASE $(sha256sum "$BASE" | cut -d' ' -f1) execution ${BASE_EXEC:-default}"
    echo "tip: $TIP $(sha256sum "$TIP" | cut -d' ' -f1) execution ${TIP_EXEC:-default}"
    echo "base compilers: $BASE_COMPILERS"
    echo "tip compilers: $TIP_COMPILERS"
    [ -z "$BASE_CONFIG" ] || echo "base build config: $BASE_CONFIG"
    [ -z "$TIP_CONFIG" ] || echo "tip build config: $TIP_CONFIG"
    echo "host: $(hostname), $ROUNDS rounds, $REPS timed requests, queries ${SOURCES[*]}"
  } | tee "$W/runs-manifest.txt"
  digests="ORDER_DIGEST=\"python3 $ROOT/scripts/bench-gate.py order-digest --data data.nt\"
WARM_DIGEST=\"python3 $ROOT/scripts/bench-gate.py native-digest --data data.nt\""
  run tip-second "$BASE" "$TIP" "EXECUTION=$BASE_EXEC,$TIP_EXEC
$digests"
  run tip-first "$TIP" "$BASE" "EXECUTION=$TIP_EXEC,$BASE_EXEC
$digests"
  run same "$TIP" "$TIP" "EXECUTION=$TIP_EXEC
SHARE_STORE=1
$digests"
  status=0
  python3 "$HERE/answers.py" "$W/runs/tip-second" "$W/runs/tip-first" "$W/runs/same" || status=1
  python3 "$HERE/combine.py" --labels base,tip "$W/runs/tip-second/results/samples.tsv" \
    "$W/runs/tip-first/results/samples.tsv" "$W/runs/same/results/samples.tsv" || status=1
  exit $status
fi
