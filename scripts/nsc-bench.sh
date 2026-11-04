#!/usr/bin/env bash
# Interleaved A/B query benchmark of two Sparkles builds on an ephemeral Namespace instance
# (namespace.so), driven through the `nsc` CLI. See docs/DEVELOPMENT.md.
#
#   scripts/nsc-bench.sh [options] A B
#
# A and B are git revisions of this repository or paths to built `sparkles` binaries. A
# revision is built before any instance exists, from a `git archive` of it with
# `mbx build --release -p sparkles-server`, on this machine or on a build host over ssh
# (--build-host, see scripts/nsc-build-host.sh), and the binary is cached under
# $SPARKLES_NSC_CACHE/bin/<commit> (default ~/.cache/sparkles-nsc), or under
# bin/<commit>-<SHA-256 of the --cargo-args text> when --cargo-args is given. An entry is
# reused only when its build.txt records the same cargo arguments, wherever it was built,
# and build.txt and the manifest name the host that built it. The dataset comes from
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
# OUT/manifest.txt the binaries' commits and SHA-256, the compilers each was built with,
# the instance, the phase times and the billed vCPU-minutes.
#
# The compilers come from each binary's .comment section, which names rustc, the linker
# and the C and C++ compilers that built its native dependencies, such as mimalloc. Two
# binaries whose compilers differ are refused, since that difference alone moves every
# query, unless --allow-mixed-compilers is given. A cached revision keeps the compilers of
# the shell that built it, so build in `nix develop` and remove a stale cache entry.
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
#   --execution MODE    send execution=MODE (eager, streaming or auto) with every query, or
#                       A_MODE,B_MODE for a different mode per variant
#   --prepare "..."     a `sparkles` command run once on each store after its load, timed,
#                       with `--loc STORE` appended, e.g. "text-index --language en"
#   --prepare-a "..."   the same for variant A only (overrides --prepare), and
#   --prepare-b "..."   for variant B only, to compare settings of one binary
#   --prepare-repeat "..."  a `sparkles` command run --prepare-reps times on each store
#                       after the prepare command, each run timed, e.g. "text-index --rebuild"
#   --prepare-reps N    runs of --prepare-repeat (default 5); results/prepare.tsv gets
#                       every timing, and the summary its medians
#   --cargo-args "..."  extra `cargo build` flags, e.g. "--features text"
#   --build-host H      where revisions are built: local, auto (the first free host of
#                       SPARKLES_NSC_BUILD_HOSTS) or a host name (default auto when
#                       SPARKLES_NSC_BUILD_HOSTS is set, else local)
#   --machine-type T    Namespace machine type (default linux/amd64:8x16)
#   --duration D        instance lifetime cap, as nsc takes it (default 2h)
#   --out DIR           results directory (default .artifacts/nsc-bench/<UTC time>)
#   --allow-mixed-compilers
#                       compare binaries built with different compilers anyway
#   --flag-ratio R      a query whose execution-time ratio B/A is R or more is flagged
#                       (default 1.10)
#   --rerun-rounds N    rounds of the rerun of the flagged queries (default 8)
#   --rerun-reps N      timed requests per query and server process in the rerun (default 40)
#   --no-rerun          report the first pass alone
#
# When the first pass flags queries, the same instance runs them again with more rounds and
# requests before it is destroyed, and summary.md reports both passes. The rerun's results
# are in OUT/rerun, with its own summary.md and summary.json.
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
EXECUTION=
CARGO_ARGS=
PREPARE=
PREPARE_A=
PREPARE_B=
PREPARE_REPEAT=
PREPARE_REPS=5
MACHINE=linux/amd64:8x16
DURATION=2h
OUT=
MIXED=0
FLAG_RATIO=1.10
RERUN_ROUNDS=8
RERUN_REPS=40
RERUN=1
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
# shellcheck source=scripts/nsc-build-host.sh
. "$ROOT/scripts/nsc-build-host.sh"
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
    --allow-mixed-compilers)
      MIXED=1
      shift
      continue
      ;;
    --no-rerun)
      RERUN=0
      shift
      continue
      ;;
    --flag-ratio) FLAG_RATIO=$2 ;;
    --rerun-rounds) RERUN_ROUNDS=$2 ;;
    --rerun-reps) RERUN_REPS=$2 ;;
    --serve-args) SERVE_ARGS=$2 ;;
    --execution) EXECUTION=$2 ;;
    --cargo-args) CARGO_ARGS=$2 ;;
    --build-host) BH_ARG=$2 ;;
    --prepare) PREPARE=$2 ;;
    --prepare-a) PREPARE_A=$2 ;;
    --prepare-b) PREPARE_B=$2 ;;
    --prepare-repeat) PREPARE_REPEAT=$2 ;;
    --prepare-reps) PREPARE_REPS=$2 ;;
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
bh_check_arg
for t in nsc zstd python3 mbx cargo readelf flock; do command -v "$t" > /dev/null || die "$t is not on PATH"; done
OUT=$(realpath -m "${OUT:-$ROOT/.artifacts/nsc-bench/$(date -u +%Y%m%dT%H%M%SZ)}")
mkdir -p "$OUT/stage/queries" "$OUT/logs" "$CACHE"/{bin,data,src,locks}
STAGE=$OUT/stage
MANIFEST=$OUT/manifest.txt
: > "$MANIFEST"
note() { echo "$*" | tee -a "$MANIFEST"; }
now() { date +%s.%N; }
secs() { awk "BEGIN {printf \"%.1f\", $2 - $1}"; }

# ------------------------------------------------------------------------- binaries
# compilers <binary>: the distinct entries of its .comment section, joined by "; "
compilers() {
  readelf -p .comment "$1" 2> /dev/null | sed -n 's/^ *\[ *[0-9a-f]*\] *//p' | sort -u | paste -sd ';' - | sed 's/;/; /g'
}
declare -A COMPILERS
# The private source tree of this run's build, removed when the run ends in any way.
SRC_TMP=
cleanup_src() {
  [ -z "$SRC_TMP" ] || rm -rf -- "$SRC_TMP"
  SRC_TMP=
  bh_cleanup
}
trap cleanup_src EXIT
trap 'exit 130' INT TERM
# binary <rev|path> <A|B>: stages bin-<A|B>.zst and notes its source, SHA-256 and compilers
binary() {
  local ref=$1 v=$2 bin sha src key entry lock host
  entry_current() { [ -x "$bin" ] && grep -qxF "cargo args: $CARGO_ARGS" "$entry/build.txt" 2> /dev/null; }
  if [ -f "$ref" ]; then
    bin=$(realpath "$ref")
    src="file $bin"
  else
    sha=$(git -C "$ROOT" rev-parse --verify --quiet "$ref^{commit}") || die "$ref is neither a file nor a commit"
    # Default builds keep the plain commit key, so existing entries stay usable. Other
    # cargo arguments get their own entry, as in scripts/nsc-writes.sh.
    key=$sha
    [ -z "$CARGO_ARGS" ] || key=$sha-$(printf '%s' "$CARGO_ARGS" | sha256sum | cut -d' ' -f1)
    entry=$CACHE/bin/$key
    bin=$entry/sparkles
    # An entry without a build record, or one recording other arguments, is rebuilt.
    # A lock per cache key makes a concurrent run of the same commit wait here and then
    # reuse the finished build. The source tree is private to this run in any case.
    if ! entry_current; then
      exec {lock}> "$CACHE/locks/$key.lock"
      flock "$lock"
      if ! entry_current; then
        bh_resolve
        rm -f "$entry/build.txt"
        mkdir -p "$entry"
        # Holding the lock, no other run is building this entry, so earlier private trees are
        # abandoned by runs that were killed.
        rm -rf -- "${CACHE:?}/src/$key".*
        SRC_TMP=$(mktemp -d "$CACHE/src/$key.XXXXXXXX")
        if [ "$BH_HOST" = local ]; then
          echo "building $ref ($sha); log: $OUT/logs/build-$v.log"
          # Keep source mtimes current. Each archive has its own managed mutable target;
          # Boxington shares immutable build results rather than one Cargo target.
          git -C "$ROOT" archive "$sha" | tar -x -m -C "$SRC_TMP"
          bh_ui_placeholder "$SRC_TMP"
          # The build inherits the lock descriptor, so a build that outlives a killed run keeps
          # the lock and the next run waits for it.
          # shellcheck disable=SC2086 # CARGO_ARGS is a list of flags
          (cd "$SRC_TMP" && bash "$ROOT/scripts/nsc-build.sh" "$bin" $CARGO_ARGS) \
            > "$OUT/logs/build-$v.log" 2>&1 || die "build of $ref failed, see $OUT/logs/build-$v.log"
          # the shell's compilers, beside the binary's own record of them in .comment
          {
            echo "build host: local ($(uname -n))"
            echo "cc: $(cc --version 2> /dev/null | head -1)"
            echo "c++: $(c++ --version 2> /dev/null | head -1)"
            (cd "$SRC_TMP" && rustc -V)
          } > "$SRC_TMP/.nsc-build-info.txt"
        else
          echo "building $ref ($sha) on $BH_HOST; log: $OUT/logs/build-$v.log"
          # The private tree receives the binary and the build host's record of its compilers.
          # shellcheck disable=SC2086 # CARGO_ARGS is a list of flags
          bh_build "$BH_HOST" "$sha" "$SRC_TMP" $CARGO_ARGS > "$OUT/logs/build-$v.log" 2>&1 ||
            die "build of $ref on $BH_HOST failed, see $OUT/logs/build-$v.log"
          cp -- "$SRC_TMP/sparkles" "$bin.tmp.$$"
          chmod 755 "$bin.tmp.$$"
          mv -- "$bin.tmp.$$" "$bin"
          mv -- "$SRC_TMP/info.txt" "$SRC_TMP/.nsc-build-info.txt"
        fi
        {
          echo "built: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
          echo "cargo args: $CARGO_ARGS"
          cat "$SRC_TMP/.nsc-build-info.txt"
        } > "$entry/build.txt.tmp.$$"
        mv "$entry/build.txt.tmp.$$" "$entry/build.txt"
        cleanup_src
      fi
      exec {lock}>&-
    fi
    src="commit $sha ($ref)"
    [ -z "$CARGO_ARGS" ] || src+=", cargo $CARGO_ARGS"
    host=$(sed -n 's/^build host: //p' "$entry/build.txt")
    src+=", built on ${host:-an unrecorded host}"
  fi
  note "$v: $src"
  note "$v sha256: $(sha256sum "$bin" | cut -d' ' -f1)"
  COMPILERS[$v]=$(compilers "$bin")
  note "$v compilers: ${COMPILERS[$v]:-unknown}"
  # a dynamically linked binary runs under the instance's own glibc loader
  if ldd "$bin" > /dev/null 2>&1; then echo "DYNAMIC_$v=1" >> "$STAGE/run.env"; else echo "DYNAMIC_$v=0" >> "$STAGE/run.env"; fi
  zstd -q -T0 -3 -f "$bin" -o "$STAGE/bin-$v.zst"
}
: > "$STAGE/run.env"
binary "${ARGS[0]}" A
binary "${ARGS[1]}" B
if [ -z "${COMPILERS[A]}" ] || [ -z "${COMPILERS[B]}" ] || [ "${COMPILERS[A]}" != "${COMPILERS[B]}" ]; then
  if [ "$MIXED" = 1 ]; then
    note "warning: the compilers differ or are unknown, compared anyway (--allow-mixed-compilers)"
  else
    die "A and B were not built with the same known compilers (see $MANIFEST). Rebuild both in \`nix develop\`, after removing a stale $CACHE/bin/<commit> entry, or pass --allow-mixed-compilers"
  fi
fi

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
EXECUTION=$EXECUTION
PREPARE_A="${PREPARE_A:-$PREPARE}"
PREPARE_B="${PREPARE_B:-$PREPARE}"
PREPARE_REPEAT="$PREPARE_REPEAT"
PREPARE_REPS=$PREPARE_REPS
EOF
[ -z "$PREPARE$PREPARE_A$PREPARE_B" ] || note "prepare: A ${PREPARE_A:-$PREPARE}; B ${PREPARE_B:-$PREPARE}${PREPARE_REPEAT:+; then $PREPARE_REPS runs of $PREPARE_REPEAT}"
note "settings: $NQ queries, $ROUNDS rounds of A,B,B,A, $WARMUP warm-up and $REPS timed requests per query and server, RAYON_NUM_THREADS=$RAYON_THREADS, server CPU $SERVER_CPU, client CPU $CLIENT_CPU, spinners $([ "$SPIN" = 1 ] && echo on || echo off)${SERVE_ARGS:+, serve $SERVE_ARGS}${EXECUTION:+, execution $EXECUTION}"

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
trap 'destroy; cleanup_src' EXIT
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
  ok=0
  for _try in 1 2 3 4 5 6 7 8 9 10 11 12; do
    if nsc instance upload "$ID" "$STAGE/$f" "$REMOTE/$f" --mkdir > "$OUT/logs/upload.log" 2>&1; then
      ok=1
      break
    fi
    echo "upload of $f failed (try $_try), retrying" >&2
    sleep 2
  done
  [ "$ok" = 1 ] || die "upload of $f failed"
done
T_SENT=$(now)
note "phase upload: $(secs "$T_UP" "$T_SENT") s for $(du -ch "$STAGE"/{bin-A.zst,bin-B.zst,data.nt.zst,queries.tar} | tail -1 | cut -f1)"

# wait_remote: poll the detached remote run, print its progress and set status
wait_remote() {
  local seen=0 fails=0 r line
  status=
  while :; do
    sleep "$POLL"
    if ! r=$(rsh "tail -n +$((seen + 1)) $REMOTE/results/progress.log 2> /dev/null; if [ -f $REMOTE/results/done ]; then echo __DONE__ \$(cat $REMOTE/results/done); fi" 2> /dev/null); then
      fails=$((fails + 1))
      [ "$fails" -lt 60 ] || die "lost contact with $ID"
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
}

# fetch_results DIR: pack the remote results directory and unpack it into DIR
fetch_results() {
  local dir=$1 ok=0 _try
  for _try in $(seq 40); do
    if rsh "cd $REMOTE && tar -czf results.tgz results" 2> "$OUT/logs/pack.log"; then
      ok=1
      break
    fi
    echo "pack failed (try $_try), retrying" >&2
    sleep 5
  done
  [ "$ok" = 1 ] || die "could not pack the results"
  ok=0
  for _try in $(seq 40); do
    if nsc instance download "$ID" "$REMOTE/results.tgz" "$dir/results.tgz" > "$OUT/logs/download.log" 2>&1; then
      ok=1
      break
    fi
    if rsh "cat $REMOTE/results.tgz" > "$dir/results.tgz.ssh" 2>> "$OUT/logs/download.log" && [ -s "$dir/results.tgz.ssh" ] && tar -tzf "$dir/results.tgz.ssh" > /dev/null 2>&1; then
      mv "$dir/results.tgz.ssh" "$dir/results.tgz"
      ok=1
      break
    fi
    echo "download failed (try $_try), retrying" >&2
    sleep 5
  done
  [ "$ok" = 1 ] || die "download failed"
  tar -xzf "$dir/results.tgz" -C "$dir"
}

rsh "cd $REMOTE && tar -xf queries.tar && setsid nohup bash nsc-bench-remote.sh > nohup.log 2>&1 < /dev/null &" ||
  die "could not start the remote run"
wait_remote
T_RAN=$(now)
note "phase remote run: $(secs "$T_SENT" "$T_RAN") s"
fetch_results "$OUT"
note "phase download: $(secs "$T_RAN" "$(now)") s"
[ "$status" = ok ] || die "the remote run failed ($status), see $OUT/results/progress.log"
python3 "$ROOT/scripts/nsc-bench-summary.py" "$OUT" > "$OUT/summary.md"

# The flagged queries run again on the same instance with more rounds and requests, as a
# screen's flags are rerun before anyone reads them as regressions.
FLAGGED=$(python3 -c '
import json, sys
s = json.load(open(sys.argv[1]))
print(" ".join(q for q, m in s["queries"].items() if "exec_ms" in m and m["exec_ms"]["ratio"] >= float(sys.argv[2])))
' "$OUT/summary.json" "$FLAG_RATIO")
if [ -n "$FLAGGED" ] && [ "$RERUN" = 1 ]; then
  note "flagged at B/A >= $FLAG_RATIO: $FLAGGED"
  note "rerun: $RERUN_ROUNDS rounds of A,B,B,A, $RERUN_REPS timed requests per query and server"
  # shellcheck disable=SC2086 # FLAGGED is a list of query names
  keep=$(printf '%s.rq ' $FLAGGED)
  rsh "cd $REMOTE && mv results results-first && rm -f results.tgz && cd queries && for f in *.rq; do case ' $keep' in *\" \$f \"*) ;; *) rm \"\$f\" ;; esac; done && cd .. && sed -i -e 's/^ROUNDS=.*/ROUNDS=$RERUN_ROUNDS/' -e 's/^REPS=.*/REPS=$RERUN_REPS/' run.env && setsid nohup bash nsc-bench-remote.sh > nohup-rerun.log 2>&1 < /dev/null &" ||
    die "could not start the rerun"
  T_RERUN=$(now)
  wait_remote
  mkdir -p "$OUT/rerun"
  fetch_results "$OUT/rerun"
  note "phase rerun: $(secs "$T_RERUN" "$(now)") s"
  [ "$status" = ok ] || die "the rerun failed ($status), see $OUT/rerun/results/progress.log"
  {
    grep -v '^settings: ' "$MANIFEST"
    echo "settings: the flagged queries, $RERUN_ROUNDS rounds of A,B,B,A, $WARMUP warm-up and $RERUN_REPS timed requests per query and server"
  } > "$OUT/rerun/manifest.txt"
  python3 "$ROOT/scripts/nsc-bench-summary.py" "$OUT/rerun" > "$OUT/rerun/summary.md"
  # A flagged query without execution times in the rerun, such as one whose requests all
  # failed, is inconclusive rather than cleared.
  read -r STILL INCONCLUSIVE < <(python3 -c '
import json, sys
s = json.load(open(sys.argv[1]))["queries"]
still, unknown = [], []
for q in sys.argv[3].split():
    m = s.get(q, {}).get("exec_ms")
    if m is None:
        unknown.append(q)
    elif m["ratio"] >= float(sys.argv[2]):
        still.append(q)
print(",".join(still) or "-", ",".join(unknown) or "-")
' "$OUT/rerun/summary.json" "$FLAG_RATIO" "$FLAGGED")
  [ "$STILL" != - ] || STILL=
  [ "$INCONCLUSIVE" != - ] || INCONCLUSIVE=
  STILL=${STILL//,/ }
  INCONCLUSIVE=${INCONCLUSIVE//,/ }
  # appended to summary.md at the end, after the first pass's prepare timings
  {
    echo
    echo "# Rerun of the flagged queries"
    echo
    if [ -n "$STILL" ]; then verdict="Still flagged: ${STILL// /, }."; else verdict="None of them stays flagged."; fi
    [ -z "$INCONCLUSIVE" ] || verdict+=" Inconclusive, with no execution times in the rerun: ${INCONCLUSIVE// /, }."
    echo "The first pass flagged ${FLAGGED// /, } at an execution-time ratio of $FLAG_RATIO or more. The same instance ran them again in $RERUN_ROUNDS rounds with $RERUN_REPS timed requests per query and server process. $verdict"
    echo
    # the rerun's summary without its title and manifest lines, keeping its answer checks
    # and failed-request warnings
    sed '1,/^## /{/^# Interleaved/d; /^- /d}' "$OUT/rerun/summary.md" | cat -s
  } > "$OUT/rerun-section.md"
elif [ -n "$FLAGGED" ]; then
  note "flagged at B/A >= $FLAG_RATIO: $FLAGGED (no rerun)"
fi
destroy
rm -f "$STAGE"/{bin-A.zst,bin-B.zst,data.nt.zst}
if [ -s "$OUT/results/prepare.tsv" ]; then
  {
    echo
    echo "Prepare commands (seconds, median of the runs; results/prepare.tsv has each run):"
    echo
    awk -F'\t' '{k = $1 " " $2; n[k]++; v[k, n[k]] = $3; c[k] = $4}
      END {for (k in n) {m = n[k]; for (i = 1; i <= m; i++) a[i] = v[k, i]
        for (i = 1; i <= m; i++) for (j = i + 1; j <= m; j++) if (a[j] < a[i]) {t = a[i]; a[i] = a[j]; a[j] = t}
        med = (m % 2) ? a[(m + 1) / 2] : (a[m / 2] + a[m / 2 + 1]) / 2
        printf "- %s: %.3f s over %d runs (%s)\n", k, med, m, c[k]}}' "$OUT/results/prepare.tsv" | sort
  } >> "$OUT/summary.md"
fi
if [ -f "$OUT/rerun-section.md" ]; then
  cat "$OUT/rerun-section.md" >> "$OUT/summary.md"
  rm "$OUT/rerun-section.md"
fi
cat "$OUT/summary.md"
echo
echo "results: $OUT"
