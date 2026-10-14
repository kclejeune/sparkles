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
# `git archive` and cached under $SPARKLES_NSC_CACHE/writes-v1, separately from query
# builds. Each commit and exact --cargo-args string has its own locked source, target
# and binary entry. A verified build.json records the artifact's original build, also
# on cache hits. This identifies source and flags, not a hermetic toolchain or environment.
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
#                       and serial-message writers-message writers16-message, which give
#                       every update a commit message
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
#   --out DIR           new results directory; any existing path is refused
#                       (default target/nsc-writes/<UTC time>.<unique suffix>)
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
for t in nsc zstd python3 cargo rustc cc flock; do command -v "$t" > /dev/null || die "$t is not on PATH"; done
if [ -n "$OUT" ]; then
  # Do not resolve the final symlink: even a dangling destination must be refused.
  OUT=$(realpath -ms "$OUT")
  mkdir -p "$(dirname "$OUT")"
  mkdir -- "$OUT" || die "results directory already exists or cannot be created: $OUT"
else
  mkdir -p "$ROOT/target/nsc-writes"
  OUT=$(mktemp -d "$ROOT/target/nsc-writes/$(date -u +%Y%m%dT%H%M%SZ).XXXXXXXX")
fi
CACHE=$(realpath -m "$CACHE")
WRITE_CACHE=$CACHE/writes-v1
mkdir -p "$OUT/stage" "$OUT/logs" "$CACHE/data" "$WRITE_CACHE"/{bin,src,target,locks}
STAGE=$OUT/stage
MANIFEST=$OUT/manifest.txt
: > "$MANIFEST"
note() { echo "$*" | tee -a "$MANIFEST"; }
now() { date +%s.%N; }
secs() { awk "BEGIN {printf \"%.1f\", $2 - $1}"; }

# ------------------------------------------------------------------------- binaries
# binary <rev|path> <id>: stages bin-<id>.zst and notes its source and SHA-256
binary() (
  local ref=$1 id=$2 bin sha src='' publish='' snapshot='' key entry target args_hash
  # All temporary paths are unique and owned by this invocation. The subshell keeps
  # its lock and cleanup independent of the eventual Namespace instance EXIT trap.
  trap '[ -z "$src" ] || rm -rf -- "$src"; [ -z "$publish" ] || rm -rf -- "$publish"; [ -z "$snapshot" ] || rm -f -- "$snapshot"' EXIT
  if [ -f "$ref" ]; then
    bin=$(realpath "$ref")
    snapshot=$(mktemp "$STAGE/.binary-$id.XXXXXXXX")
    cp -- "$bin" "$snapshot"
    note "binary $id: externally built file $bin (staged snapshot)"
    bin=$snapshot
  else
    sha=$(git -C "$ROOT" rev-parse --verify --quiet "$ref^{commit}") || die "$ref is neither a file nor a commit"
    args_hash=$(printf '%s' "$CARGO_ARGS" | sha256sum | cut -d' ' -f1)
    key=$sha-$args_hash
    entry=$WRITE_CACHE/bin/$key
    target=$WRITE_CACHE/target/$key
    exec {build_lock}> "$WRITE_CACHE/locks/$key.lock"
    flock "$build_lock"
    # Recheck only after acquiring the lock. Old query builds use neither this
    # namespace nor these per-key targets and cannot replace a build's output.
    if [ ! -e "$entry" ] && [ ! -L "$entry" ]; then
      echo "building $ref ($sha); log: $OUT/logs/build-$id.log"
      src=$(mktemp -d "$WRITE_CACHE/src/$key.XXXXXXXX")
      publish=$(mktemp -d "$WRITE_CACHE/bin/.$key.XXXXXXXX")
      git -C "$ROOT" archive "$sha" | tar -x -m -C "$src"
      mkdir -p "$src/ui/build" "$target/release"
      echo '<!doctype html><title>Sparkles</title><p>The UI was not built for this benchmark binary.</p>' \
        > "$src/ui/build/index.html"
      (cd "$src" && rustc -vV) > "$publish/rustc.txt"
      (cd "$src" && cargo -V) > "$publish/cargo.txt"
      # An option selecting another profile or target must not reuse stale output
      # at our expected path. Explicit paths also make conflicting CLI overrides
      # fail in Cargo rather than redirecting into a shared target or other source.
      rm -f -- "$target/release/sparkles"
      # shellcheck disable=SC2086 # CARGO_ARGS is a list of flags
      (cd "$src" && CARGO_TARGET_DIR=$target cargo build --release -p sparkles-server \
        --manifest-path "$src/Cargo.toml" --target-dir "$target" \
        --message-format=json-render-diagnostics $CARGO_ARGS) \
        > "$OUT/logs/build-$id.jsonl" 2> "$OUT/logs/build-$id.log" ||
        die "build of $ref failed, see $OUT/logs/build-$id.log and build-$id.jsonl"
      [ -f "$target/release/sparkles" ] && [ -x "$target/release/sparkles" ] && [ ! -L "$target/release/sparkles" ] ||
        die "build of $ref did not produce the expected release/sparkles"
      cp -- "$target/release/sparkles" "$publish/sparkles"
      python3 - "$publish" "$sha" "$CARGO_ARGS" "$args_hash" "$OUT/logs/build-$id.jsonl" "$src" "$target" << 'PY'
import datetime, hashlib, json, os, pathlib, sys
directory, revision, arguments, args_hash, build_log, source, target = sys.argv[1:]
path = pathlib.Path(directory)
expected = (pathlib.Path(target) / 'release/sparkles').resolve()
try:
    artifacts = []
    for line in pathlib.Path(build_log).read_text().splitlines():
        event = json.loads(line)
        if (event.get('reason') == 'compiler-artifact' and event.get('executable')
                and pathlib.Path(event['executable']).resolve() == expected):
            artifacts.append(event)
    if len(artifacts) != 1:
        raise ValueError('expected exactly one artifact at the owned executable path')
    artifact = artifacts[0]
    if (pathlib.Path(artifact['manifest_path']).resolve()
            != (pathlib.Path(source) / 'crates/sparkles-server/Cargo.toml').resolve()
            or artifact['target']['name'] != 'sparkles'
            or 'bin' not in artifact['target']['kind']):
        raise ValueError('executable did not come from the archived sparkles-server binary')
except (OSError, ValueError, KeyError, TypeError) as error:
    sys.exit(f'nsc-writes: cannot verify Cargo executable provenance: {error}')
record = {
    'schema': 1, 'revision': revision, 'cargo_args': arguments,
    'cargo_args_sha256': args_hash,
    'binary_sha256': hashlib.sha256((path / 'sparkles').read_bytes()).hexdigest(),
    'built_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'rustc': (path / 'rustc.txt').read_text(), 'cargo': (path / 'cargo.txt').read_text(),
    'version_commands': 'rustc -vV and cargo -V in the extracted source directory',
    'command': 'cargo build --release -p sparkles-server --manifest-path <owned source>/Cargo.toml --target-dir <key target> --message-format=json-render-diagnostics $CARGO_ARGS',
    'artifact': {'manifest': 'crates/sparkles-server/Cargo.toml', 'name': 'sparkles', 'kind': artifact['target']['kind']},
    'environment': {name: os.environ.get(name) for name in (
        'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'RUSTUP_TOOLCHAIN', 'CARGO_BUILD_TARGET',
        'RUSTC', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER')},
}
(path / 'build.json').write_text(json.dumps(record, indent=2) + '\n')
(path / 'rustc.txt').unlink()
(path / 'cargo.txt').unlink()
PY
      # Publishing the directory makes the executable and its receipt visible
      # together. No uncooperative query-cache writer uses this location.
      mv -T -- "$publish" "$entry"
      publish=
      rm -rf -- "$src"
      src=
    fi
    bin=$entry/sparkles
    python3 - "$entry" "$sha" "$CARGO_ARGS" "$args_hash" << 'PY'
import hashlib, json, pathlib, sys
directory, revision, arguments, args_hash = sys.argv[1:]
path = pathlib.Path(directory)
try:
    if not path.is_dir() or path.is_symlink():
        raise ValueError('entry is not a regular directory')
    binary = path / 'sparkles'
    receipt = path / 'build.json'
    if not binary.is_file() or binary.is_symlink() or not receipt.is_file() or receipt.is_symlink():
        raise ValueError('binary or receipt is not a regular file')
    record = json.loads(receipt.read_text())
    if (record['schema'] != 1 or record['revision'] != revision
            or record['cargo_args'] != arguments or record['cargo_args_sha256'] != args_hash
            or record['binary_sha256'] != hashlib.sha256(binary.read_bytes()).hexdigest()
            or not all(isinstance(record[key], str) and record[key] for key in ('rustc', 'cargo', 'built_at', 'command'))
            or not isinstance(record['environment'], dict)
            or record['artifact']['manifest'] != 'crates/sparkles-server/Cargo.toml'
            or record['artifact']['name'] != 'sparkles' or 'bin' not in record['artifact']['kind']):
        raise ValueError('build receipt does not match the requested key and binary')
except (AssertionError, OSError, ValueError, KeyError, TypeError) as error:
    sys.exit(f'nsc-writes: untrusted write-cache entry {path}: {type(error).__name__}; preserve it for inspection and use a new cache root')
PY
    [ -x "$bin" ] || die "cached binary is not executable: $bin"
    cp -- "$entry/build.json" "$STAGE/bin-$id.build.json"
    note "binary $id: commit $sha ($ref), build key $key"
    note "binary $id provenance: stage/bin-$id.build.json (original build, not a fresh-build claim)"
  fi
  note "binary $id sha256: $(sha256sum "$bin" | cut -d' ' -f1)"
  if ldd "$bin" > /dev/null 2>&1; then echo "DYNAMIC_$id=1" >> "$STAGE/run.env"; else echo "DYNAMIC_$id=0" >> "$STAGE/run.env"; fi
  zstd -q -T0 -3 -f "$bin" -o "$STAGE/bin-$id.zst"
  # The per-key lock remains held through hashing and compression, not only build.
)
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
