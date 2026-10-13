#!/usr/bin/env bash
# Interleaved A/B of durable-commit workloads (serial commits, concurrent writers, writers
# with readers) of two or more Sparkles variants on an ephemeral Namespace instance
# (namespace.so), driven through the `nsc` CLI. It is the write-side sibling of
# scripts/nsc-bench.sh, which measures queries. See docs/DEVELOPMENT.md.
#
#   scripts/nsc-writes.sh [options] NAME=REF[:FLAGS] NAME=REF[:FLAGS] [...]
#
# A variant is a name, a git revision of this repository or a path to a built `sparkles`
# binary, and optionally the global flags (before `serve`) to run it with, so one binary
# can be compared with itself under different flags. The token delay-us=N among the flags
# adds N microseconds to every synchronization and write-through write of the store's files
# (through tests/support/iofault.c), to stand in for a device slower than the instance's:
#
#   scripts/nsc-writes.sh off=HEAD on=HEAD:--experimental-group-commit
#   scripts/nsc-writes.sh base=main head=HEAD
#
# Revisions are built here with `cargo build --release -p sparkles-server` from a
# `git archive` and cached under $SPARKLES_NSC_CACHE/bin/<commit> like nsc-bench.sh does.
# The client is scripts/write-bench.rs, built with rustc. The remote script loads one store
# per binary, then runs ROUNDS rounds in which every variant runs once, the order
# reversed on every other round, each run in a fresh server process on a fresh copy of the
# store. OUT/summary.md gets the per-scenario medians over the runs and the ratio of each
# variant to the first, and OUT/manifest.txt the binaries, the instance and the times.
#
# Options:
#   --people N          dataset size in people, about 10.5 triples each (default 100000,
#                       1.05M triples; 1000000 gives 10.5M)
#   --scenarios "a b"   from serial-literal serial-iri serial-delete writers mixed writers16
#                       (default: serial-literal writers mixed)
#   --rounds N          runs of every variant and scenario (default 3)
#   --changes N         requests of the serial scenarios (default 3000)
#   --writer-changes N  requests of the writers scenarios (default 20000)
#   --mixed-changes N   requests of the mixed scenario (default 5000)
#   --server-cpus L     the servers' CPU list (default 0-5)
#   --client-cpus L     the client's CPU list (default 6-7)
#   --cargo-args "..."  extra `cargo build` flags
#   --machine-type T    Namespace machine type (default linux/amd64:8x16)
#   --duration D        instance lifetime cap, as nsc takes it (default 2h)
#   --out DIR           results directory (default target/nsc-writes/<UTC time>)
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
CACHE=${SPARKLES_NSC_CACHE:-$HOME/.cache/sparkles-nsc}
PEOPLE=100000
SCENARIOS="serial-literal writers mixed"
ROUNDS=3
CHANGES=3000
WRITER_CHANGES=20000
MIXED_CHANGES=5000
SERVER_CPUS=0-5
CLIENT_CPUS=6-7
CARGO_ARGS=
MACHINE=linux/amd64:8x16
DURATION=2h
OUT=
POLL=${POLL:-20}
REMOTE=/root/nsc-writes

die() {
  echo "nsc-writes: $*" >&2
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
    --scenarios) SCENARIOS=$2 ;;
    --rounds) ROUNDS=$2 ;;
    --changes) CHANGES=$2 ;;
    --writer-changes) WRITER_CHANGES=$2 ;;
    --mixed-changes) MIXED_CHANGES=$2 ;;
    --server-cpus) SERVER_CPUS=$2 ;;
    --client-cpus) CLIENT_CPUS=$2 ;;
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
[ ${#ARGS[@]} -ge 2 ] || usage 1
for t in nsc zstd python3 cargo rustc cc; do command -v "$t" > /dev/null || die "$t is not on PATH"; done
OUT=$(realpath -m "${OUT:-$ROOT/target/nsc-writes/$(date -u +%Y%m%dT%H%M%SZ)}")
mkdir -p "$OUT/stage" "$OUT/logs" "$CACHE"/{bin,data,src}
STAGE=$OUT/stage
MANIFEST=$OUT/manifest.txt
: > "$MANIFEST"
note() { echo "$*" | tee -a "$MANIFEST"; }
now() { date +%s.%N; }
secs() { awk "BEGIN {printf \"%.1f\", $2 - $1}"; }

# ------------------------------------------------------------------------- binaries
# binary <rev|path> <id>: stages bin-<id>.zst and notes its source and SHA-256
binary() {
  local ref=$1 id=$2 bin sha src
  if [ -f "$ref" ]; then
    bin=$(realpath "$ref")
    src="file $bin"
  else
    sha=$(git -C "$ROOT" rev-parse --verify --quiet "$ref^{commit}") || die "$ref is neither a file nor a commit"
    bin=$CACHE/bin/$sha/sparkles
    if [ ! -x "$bin" ]; then
      echo "building $ref ($sha); log: $OUT/logs/build-$id.log"
      rm -rf "${CACHE:?}/src/$sha"
      mkdir -p "$CACHE/src/$sha" "$CACHE/bin/$sha"
      git -C "$ROOT" archive "$sha" | tar -x -m -C "$CACHE/src/$sha"
      mkdir -p "$CACHE/src/$sha/ui/build"
      echo '<!doctype html><title>Sparkles</title><p>The UI was not built for this benchmark binary.</p>' \
        > "$CACHE/src/$sha/ui/build/index.html"
      # shellcheck disable=SC2086 # CARGO_ARGS is a list of flags
      (cd "$CACHE/src/$sha" && CARGO_TARGET_DIR=$CACHE/target cargo build --release -p sparkles-server $CARGO_ARGS) \
        > "$OUT/logs/build-$id.log" 2>&1 || die "build of $ref failed, see $OUT/logs/build-$id.log"
      cp "$CACHE/target/release/sparkles" "$bin.tmp"
      mv "$bin.tmp" "$bin"
      rm -rf "${CACHE:?}/src/$sha"
    fi
    src="commit $sha ($ref)"
    [ -z "$CARGO_ARGS" ] || src+=", cargo $CARGO_ARGS"
  fi
  note "binary $id: $src"
  note "binary $id sha256: $(sha256sum "$bin" | cut -d' ' -f1)"
  if ldd "$bin" > /dev/null 2>&1; then echo "DYNAMIC_$id=1" >> "$STAGE/run.env"; else echo "DYNAMIC_$id=0" >> "$STAGE/run.env"; fi
  zstd -q -T0 -3 -f "$bin" -o "$STAGE/bin-$id.zst"
}
: > "$STAGE/run.env"
declare -A BINID
NEXT=0
VARIANTS=
for spec in "${ARGS[@]}"; do
  name=${spec%%=*}
  rest=${spec#*=}
  [ "$name" != "$spec" ] || die "variant $spec is not NAME=REF[:FLAGS]"
  ref=${rest%%:*}
  flags=
  [ "$ref" = "$rest" ] || flags=${rest#*:}
  if [ -z "${BINID[$ref]:-}" ]; then
    BINID[$ref]=b$NEXT
    NEXT=$((NEXT + 1))
    binary "$ref" "${BINID[$ref]}"
  fi
  VARIANTS+="$name "
  printf 'V_%s_BIN=%s\nV_%s_FLAGS=%q\n' "$name" "${BINID[$ref]}" "$name" "$flags" >> "$STAGE/run.env"
done

# ---------------------------------------------------------------------- client, data
rustc --edition 2024 -O -o "$STAGE/write-bench" "$ROOT/scripts/write-bench.rs" 2> "$OUT/logs/rustc.log" || die "write-bench did not build, see $OUT/logs/rustc.log"
cc -shared -fPIC -O1 -o "$STAGE/libiofault.so" "$ROOT/crates/sparkles-core/tests/support/iofault.c" -ldl 2> "$OUT/logs/cc.log" || die "the I/O shim did not build, see $OUT/logs/cc.log"
if ldd "$STAGE/write-bench" > /dev/null 2>&1; then echo "DYNAMIC_CLIENT=1" >> "$STAGE/run.env"; else echo "DYNAMIC_CLIENT=0" >> "$STAGE/run.env"; fi
DATA=$CACHE/data/people-$PEOPLE.nt.zst
if [ ! -f "$DATA" ]; then
  echo "generating the dataset ($PEOPLE people)"
  python3 "$ROOT/scripts/gen-data.py" "$PEOPLE" | zstd -q -T0 -10 -o "$DATA.tmp"
  mv "$DATA.tmp" "$DATA"
fi
cp "$DATA" "$STAGE/data.nt.zst"
cp "$ROOT/scripts/nsc-writes-remote.sh" "$STAGE/"
cat >> "$STAGE/run.env" << EOF
VARIANTS="$VARIANTS"
PEOPLE=$PEOPLE
SCENARIOS="$SCENARIOS"
ROUNDS=$ROUNDS
CHANGES=$CHANGES
WRITER_CHANGES=$WRITER_CHANGES
MIXED_CHANGES=$MIXED_CHANGES
SERVER_CPUS=$SERVER_CPUS
CLIENT_CPUS=$CLIENT_CPUS
EOF
note "data: $PEOPLE people (scripts/gen-data.py), $(du -h "$DATA" | cut -f1) compressed"
note "settings: variants $VARIANTS, scenarios $SCENARIOS, $ROUNDS rounds, serial $CHANGES, writers $WRITER_CHANGES, mixed $MIXED_CHANGES requests, server CPUs $SERVER_CPUS, client CPUs $CLIENT_CPUS"

# ---------------------------------------------------------------------------- instance
ID=
T_CREATE=
VCPUS=${MACHINE##*:}
VCPUS=${VCPUS%%x*}
destroy() {
  local t
  [ -n "$ID" ] || return 0
  echo "destroying $ID"
  nsc destroy --force "$ID" > "$OUT/logs/destroy.log" 2>&1 || echo "nsc-writes: destroying $ID failed; run nsc destroy $ID" >&2
  t=$(now)
  local s min
  s=$(secs "$T_CREATE" "$t")
  min=$(awk "BEGIN {m = int($s / 60); if (m * 60 < $s) m++; if (m < 1) m = 1; print m}")
  note "instance lifetime: $s s, billed as $min minutes x $VCPUS vCPUs = $((min * VCPUS)) vCPU-minutes"
  ID=
}
trap destroy EXIT
trap 'exit 130' INT TERM

T_CREATE=$(now)
nsc create --bare --machine_type "$MACHINE" --duration "$DURATION" \
  --purpose "sparkles nsc-writes $VARIANTS" --cidfile "$OUT/instance.id" \
  --output_json_to "$OUT/instance.json" > "$OUT/logs/create.log" 2>&1 || {
  [ -s "$OUT/instance.id" ] && ID=$(cat "$OUT/instance.id")
  die "nsc create failed, see $OUT/logs/create.log"
}
ID=$(cat "$OUT/instance.id")
T_UP=$(now)
note "instance: $ID ($MACHINE, capped at $DURATION)"
note "phase create: $(secs "$T_CREATE" "$T_UP") s"

rsh() { nsc ssh -T "$ID" -- "$@"; }
for f in "$STAGE"/bin-*.zst "$STAGE/data.nt.zst" "$STAGE/write-bench" "$STAGE/libiofault.so" "$STAGE/nsc-writes-remote.sh" "$STAGE/run.env"; do
  nsc instance upload "$ID" "$f" "$REMOTE/$(basename "$f")" --mkdir > "$OUT/logs/upload.log" 2>&1 || die "upload of $f failed"
done
T_SENT=$(now)
note "phase upload: $(secs "$T_UP" "$T_SENT") s"

rsh "cd $REMOTE && setsid nohup bash nsc-writes-remote.sh > nohup.log 2>&1 < /dev/null &" ||
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
rm -f "$STAGE"/bin-*.zst "$STAGE/data.nt.zst"
[ "$status" = ok ] || die "the remote run failed ($status), see $OUT/results/progress.log"

# shellcheck disable=SC2086 # VARIANTS is a list of names
python3 "$ROOT/scripts/nsc-writes-summary.py" "$OUT" $VARIANTS > "$OUT/summary.md"
cat "$OUT/summary.md"
echo
echo "results: $OUT"
