#!/usr/bin/env bash
# Interleaved A/B query benchmark of two Sparkles builds on an ephemeral Namespace instance
# (namespace.so), driven through the `nsc` CLI. See docs/DEVELOPMENT.md.
#
#   scripts/nsc-bench.sh [options] A B
#
# A and B are git revisions of this repository or paths to built `sparkles` binaries. A
# revision is built here, before any instance exists, from a `git archive` of it with
# `cargo build --release -p sparkles-server`, and the binary is cached under
# $SPARKLES_NSC_CACHE/bin/<commit> (default ~/.cache/sparkles-nsc). The dataset comes from
# scripts/gen-data.py and is cached there as zstd. The queries are those of scripts/bench.sh
# unless --query-dir names a directory of *.rq files.
#
# One instance is created with a hard --duration cap and destroyed on every exit, also on
# errors and Ctrl-C. The binaries, the data, the queries and scripts/nsc-bench-remote.sh are
# uploaded, the remote script runs detached (so a dropped SSH session loses nothing) and is
# polled, and its results are downloaded to OUT. The remote script loads one store per
# binary, then runs ROUNDS rounds of A, B, B, A, each in a fresh server process pinned to
# one CPU with the client pinned to another. OUT/summary.md gets the per-query medians, the
# per-round ratios, the spread between server processes and the geometric mean, and
# OUT/manifest.txt the binaries' commits and SHA-256, the instance, the phase times and the
# billed vCPU-minutes.
#
# Options:
#   --people N          dataset size in people, about 10.5 triples each (default 100000,
#                       1.05M triples; 1000000 gives 10.5M)
#   --queries "a b"     only these queries (default: all of bench.sh except export-500k)
#   --query-dir DIR     use DIR/*.rq instead of the bench.sh queries
#   --rounds N          A,B,B,A rounds (default 4, so 16 server processes)
#   --reps N            timed requests per query and server process (default 20)
#   --warmup N          untimed requests per query and server process (default 3)
#   --rayon-threads N   RAYON_NUM_THREADS of the servers (default 1; "default" leaves it unset)
#   --server-cpu N      the servers' CPU (default 2)
#   --client-cpu N      the client's CPU (default 4, on another core than CPU 2's sibling)
#   --no-spin           no idle-priority busy loops on the server and client CPUs
#   --serve-args "..."  extra `sparkles serve` flags for both servers
#   --cargo-args "..."  extra `cargo build` flags, e.g. "--features text"
#   --machine-type T    Namespace machine type (default linux/amd64:8x16)
#   --duration D        instance lifetime cap, as nsc takes it (default 2h)
#   --out DIR           results directory (default target/nsc-bench/<UTC time>)
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
CACHE=${SPARKLES_NSC_CACHE:-$HOME/.cache/sparkles-nsc}
PEOPLE=100000
QUERIES=
QUERY_DIR=
ROUNDS=4
REPS=20
SPIN=1
WARMUP=3
RAYON_THREADS=1
SERVER_CPU=2
CLIENT_CPU=4
SERVE_ARGS=
CARGO_ARGS=
MACHINE=linux/amd64:8x16
DURATION=2h
OUT=
POLL=${POLL:-20}
REMOTE=/root/nsc-bench

die() {
  echo "nsc-bench: $*" >&2
  exit 1
}
usage() {
  sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
  exit "${1:-0}"
}
ARGS=()
while [ $# -gt 0 ]; do
  case $1 in
    --people) PEOPLE=$2 ;;
    --queries) QUERIES=$2 ;;
    --query-dir) QUERY_DIR=$(realpath "$2") ;;
    --rounds) ROUNDS=$2 ;;
    --reps) REPS=$2 ;;
    --warmup) WARMUP=$2 ;;
    --rayon-threads) RAYON_THREADS=$2 ;;
    --server-cpu) SERVER_CPU=$2 ;;
    --client-cpu) CLIENT_CPU=$2 ;;
    --no-spin)
      SPIN=0
      shift
      continue
      ;;
    --serve-args) SERVE_ARGS=$2 ;;
    --cargo-args) CARGO_ARGS=$2 ;;
    --machine-type) MACHINE=$2 ;;
    --duration) DURATION=$2 ;;
    --out) OUT=$2 ;;
    -h | --help) usage ;;
    -*) die "unknown option $1 (see --help)" ;;
    *)
      ARGS+=("$1")
      shift
      continue
      ;;
  esac
  [ $# -ge 2 ] || die "$1 needs a value"
  shift 2
done
[ ${#ARGS[@]} -eq 2 ] || usage 1
for t in nsc zstd python3 cargo; do command -v "$t" > /dev/null || die "$t is not on PATH"; done
OUT=$(realpath -m "${OUT:-$ROOT/target/nsc-bench/$(date -u +%Y%m%dT%H%M%SZ)}")
mkdir -p "$OUT/stage/queries" "$OUT/logs" "$CACHE"/{bin,data,src}
STAGE=$OUT/stage
MANIFEST=$OUT/manifest.txt
: > "$MANIFEST"
note() { echo "$*" | tee -a "$MANIFEST"; }
now() { date +%s.%N; }
secs() { awk "BEGIN {printf \"%.1f\", $2 - $1}"; }

# ------------------------------------------------------------------------- binaries
# binary <rev|path> <A|B>: stages bin-<A|B>.zst and notes its source and SHA-256
binary() {
  local ref=$1 v=$2 bin sha src
  if [ -f "$ref" ]; then
    bin=$(realpath "$ref")
    src="file $bin"
  else
    sha=$(git -C "$ROOT" rev-parse --verify --quiet "$ref^{commit}") || die "$ref is neither a file nor a commit"
    bin=$CACHE/bin/$sha/sparkles
    if [ ! -x "$bin" ]; then
      echo "building $ref ($sha); log: $OUT/logs/build-$v.log"
      rm -rf "${CACHE:?}/src/$sha"
      mkdir -p "$CACHE/src/$sha" "$CACHE/bin/$sha"
      git -C "$ROOT" archive "$sha" | tar -x -C "$CACHE/src/$sha"
      # the server embeds ui/build; its build script writes a placeholder, but does not
      # rerun for a second source tree in the shared target directory
      mkdir -p "$CACHE/src/$sha/ui/build"
      echo '<!doctype html><title>Sparkles</title><p>The UI was not built for this benchmark binary.</p>' \
        > "$CACHE/src/$sha/ui/build/index.html"
      # shellcheck disable=SC2086 # CARGO_ARGS is a list of flags
      (cd "$CACHE/src/$sha" && CARGO_TARGET_DIR=$CACHE/target cargo build --release -p sparkles-server $CARGO_ARGS) \
        > "$OUT/logs/build-$v.log" 2>&1 || die "build of $ref failed, see $OUT/logs/build-$v.log"
      cp "$CACHE/target/release/sparkles" "$bin.tmp"
      mv "$bin.tmp" "$bin"
      rm -rf "${CACHE:?}/src/$sha"
    fi
    src="commit $sha ($ref)"
    [ -z "$CARGO_ARGS" ] || src+=", cargo $CARGO_ARGS"
  fi
  note "$v: $src"
  note "$v sha256: $(sha256sum "$bin" | cut -d' ' -f1)"
  # a dynamically linked binary runs under the instance's own glibc loader
  if ldd "$bin" > /dev/null 2>&1; then echo "DYNAMIC_$v=1" >> "$STAGE/run.env"; else echo "DYNAMIC_$v=0" >> "$STAGE/run.env"; fi
  zstd -q -T0 -3 -f "$bin" -o "$STAGE/bin-$v.zst"
}
: > "$STAGE/run.env"
binary "${ARGS[0]}" A
binary "${ARGS[1]}" B

# ----------------------------------------------------------------------- data, queries
DATA=$CACHE/data/people-$PEOPLE.nt.zst
if [ ! -f "$DATA" ]; then
  echo "generating the dataset ($PEOPLE people)"
  python3 "$ROOT/scripts/gen-data.py" "$PEOPLE" | zstd -q -T0 -10 -o "$DATA.tmp"
  mv "$DATA.tmp" "$DATA"
fi
cp "$DATA" "$STAGE/data.nt.zst"
note "data: $PEOPLE people (scripts/gen-data.py), $(du -h "$DATA" | cut -f1) compressed"

if [ -n "$QUERY_DIR" ]; then
  cp "$QUERY_DIR"/*.rq "$STAGE/queries/"
else
  # the prefixes and queries of scripts/bench.sh: its `P='…'` line and `add name '…'` lines
  P=$(sed -n "s/^P='\(.*\)'$/\1/p" "$ROOT/scripts/bench.sh")
  while IFS=$'\t' read -r n q; do
    [ "$n" = export-500k ] && [ -z "$QUERIES" ] && continue
    printf '%s%s' "$P" "$q" > "$STAGE/queries/$n.rq"
  done < <(sed -n "s/^add \([a-z0-9-]*\) '\(.*\)'$/\1\t\2/p" "$ROOT/scripts/bench.sh")
fi
if [ -n "$QUERIES" ]; then
  for f in "$STAGE"/queries/*.rq; do
    n=$(basename "$f" .rq)
    [[ " $QUERIES " == *" $n "* ]] || rm "$f"
  done
  for n in $QUERIES; do [ -f "$STAGE/queries/$n.rq" ] || die "no query named $n"; done
fi
NQ=$(find "$STAGE/queries" -name '*.rq' | wc -l)
[ "$NQ" -gt 0 ] || die "no queries"
tar -C "$STAGE" -cf "$STAGE/queries.tar" queries
cp "$ROOT/scripts/nsc-bench-remote.sh" "$STAGE/"
cat >> "$STAGE/run.env" << EOF
ROUNDS=$ROUNDS
REPS=$REPS
WARMUP=$WARMUP
RAYON_THREADS=$RAYON_THREADS
SERVER_CPU=$SERVER_CPU
CLIENT_CPU=$CLIENT_CPU
SPIN=$SPIN
SERVE_ARGS="$SERVE_ARGS"
EOF
note "settings: $NQ queries, $ROUNDS rounds of A,B,B,A, $WARMUP warm-up and $REPS timed requests per query and server, RAYON_NUM_THREADS=$RAYON_THREADS, server CPU $SERVER_CPU, client CPU $CLIENT_CPU, spinners $([ "$SPIN" = 1 ] && echo on || echo off)${SERVE_ARGS:+, serve $SERVE_ARGS}"

# ---------------------------------------------------------------------------- instance
ID=
T_CREATE=
VCPUS=${MACHINE##*:}
VCPUS=${VCPUS%%x*}
destroy() {
  local t
  [ -n "$ID" ] || return 0
  echo "destroying $ID"
  nsc destroy --force "$ID" > "$OUT/logs/destroy.log" 2>&1 || echo "nsc-bench: destroying $ID failed; run nsc destroy $ID" >&2
  t=$(now)
  local s min
  s=$(secs "$T_CREATE" "$t")
  min=$(awk "BEGIN {m = int($s / 60); if (m * 60 < $s) m++; if (m < 1) m = 1; print m}")
  note "instance lifetime: $s s, billed as $min minutes x $VCPUS vCPUs = $((min * VCPUS)) vCPU-minutes (\$$(awk "BEGIN {printf \"%.3f\", $min * $VCPUS * 0.0015}") at the overage rate)"
  ID=
}
trap destroy EXIT
trap 'exit 130' INT TERM

T_CREATE=$(now)
nsc create --bare --machine_type "$MACHINE" --duration "$DURATION" \
  --purpose "sparkles nsc-bench ${ARGS[0]} vs ${ARGS[1]}" --cidfile "$OUT/instance.id" \
  --output_json_to "$OUT/instance.json" > "$OUT/logs/create.log" 2>&1 || {
  # the instance may exist even though the command failed
  [ -s "$OUT/instance.id" ] && ID=$(cat "$OUT/instance.id")
  die "nsc create failed, see $OUT/logs/create.log"
}
ID=$(cat "$OUT/instance.id")
T_UP=$(now)
URL=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1])).get("app_url", "?"))' "$OUT/instance.json")
note "instance: $ID ($MACHINE, capped at $DURATION), $URL"
note "phase create: $(secs "$T_CREATE" "$T_UP") s"

rsh() { nsc ssh -T "$ID" -- "$@"; }
for f in bin-A.zst bin-B.zst data.nt.zst queries.tar nsc-bench-remote.sh run.env; do
  nsc instance upload "$ID" "$STAGE/$f" "$REMOTE/$f" --mkdir > "$OUT/logs/upload.log" 2>&1 || die "upload of $f failed"
done
T_SENT=$(now)
note "phase upload: $(secs "$T_UP" "$T_SENT") s for $(du -ch "$STAGE"/{bin-A.zst,bin-B.zst,data.nt.zst,queries.tar} | tail -1 | cut -f1)"

rsh "cd $REMOTE && tar -xf queries.tar && setsid nohup bash nsc-bench-remote.sh > nohup.log 2>&1 < /dev/null &" ||
  die "could not start the remote run"
seen=0
fails=0
status=
while :; do
  sleep "$POLL"
  if ! r=$(rsh "tail -n +$((seen + 1)) $REMOTE/results/progress.log 2> /dev/null; if [ -f $REMOTE/results/done ]; then echo __DONE__ \$(cat $REMOTE/results/done); fi" 2> /dev/null); then
    fails=$((fails + 1))
    [ "$fails" -lt 6 ] || die "lost contact with $ID"
    continue
  fi
  fails=0
  while IFS= read -r line; do
    case $line in
      __DONE__*) status=${line#__DONE__ } ;;
      '') ;;
      *)
        echo "  $line"
        seen=$((seen + 1))
        ;;
    esac
  done <<< "$r"
  [ -z "$status" ] || break
done
T_RAN=$(now)
note "phase remote run: $(secs "$T_SENT" "$T_RAN") s"

rsh "cd $REMOTE && tar -czf results.tgz results" || die "could not pack the results"
nsc instance download "$ID" "$REMOTE/results.tgz" "$OUT/results.tgz" > "$OUT/logs/download.log" 2>&1 || die "download failed"
tar -xzf "$OUT/results.tgz" -C "$OUT"
note "phase download: $(secs "$T_RAN" "$(now)") s"
destroy
rm -f "$STAGE"/{bin-A.zst,bin-B.zst,data.nt.zst}
[ "$status" = ok ] || die "the remote run failed ($status), see $OUT/results/progress.log"

python3 "$ROOT/scripts/nsc-bench-summary.py" "$OUT" > "$OUT/summary.md"
cat "$OUT/summary.md"
echo
echo "results: $OUT"
