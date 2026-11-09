#!/usr/bin/env bash
# Interleaved A/B of durable-commit workloads (serial commits, concurrent writers, writers
# with readers) of two or more Sparkles variants on an ephemeral Namespace instance
# (namespace.so), driven through the `nsc` CLI. It is the write-side sibling of
# scripts/nsc-bench.sh, which measures queries. See docs/DEVELOPMENT.md.
#
#   scripts/writes-remote.sh --executor nsc|boat:bx_ID [options]
#   scripts/nsc-writes.sh [options] NAME=REF[:FLAGS] NAME=REF[:FLAGS] [...]
#
# A variant is a name (a shell identifier: letters, digits and underscores, not starting
# with a digit), a git revision of this repository or a path to a built `sparkles`
# binary, and optionally the global flags (before `serve`) to run it with, so one binary
# can be compared with itself under different flags. The token delay-us=N among the flags
# adds N microseconds to every synchronization and write-through write of the store's files
# (through tests/support/iofault.c), to stand in for a device slower than the instance's:
#
#   scripts/nsc-writes.sh off=HEAD:--no-group-commit on=HEAD
#   scripts/nsc-writes.sh base=main head=HEAD
#
# Revisions are built through scripts/nsc-build.sh with MBX from a committed
# `git archive` and cached under $SPARKLES_NSC_CACHE/writes-v2, separately from query
# builds. Each commit and exact --cargo-args string has a locked binary entry and a
# unique source with a managed target. Build and artifact copy share one target lease. A verified build.json records the artifact's original build, also
# on cache hits. This identifies source and flags, not a hermetic toolchain or environment.
# With --build-host naming a build host (see scripts/nsc-build-host.sh), a revision is
# built there instead, with scripts/nsc-build.sh under the host's Boxington cache, which
# takes feature and dependency-resolution flags only. Its build.json names the host and
# that command, and the entry is reused like any other.
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
#   --write-rate N      pace the writers of the writers and mixed scenarios to N requests
#                       per second in total, so variants compare at the same commit rate
#                       (default 0, unpaced)
#   --index I           configure an index on every store after loading it: none (the
#                       default), text (full-text search), geo (the spatial index) or embed
#                       (an embedding index of the writers' literals, served with
#                       --no-embedding so that no text leaves the instance)
#   --views V           define views on every store after loading it: none (the default),
#                       async:N (N table views kept current after each commit, each
#                       over the writers' literal predicate), commit:Q or graph:N, which
#                       need maintenance at commit (ROADMAP item 421) and graph views
#                       (item 420) and are refused until the server has them
#   --server-cpus L     the servers' CPU list (default 0-5)
#   --client-cpus L     the client's CPU list (default 6-7)
#   --cargo-args "..."  extra `cargo build` flags
#   --build-host H      where revisions are built: local, auto (the first free host of
#                       SPARKLES_NSC_BUILD_HOSTS) or a host name (default auto when
#                       SPARKLES_NSC_BUILD_HOSTS is set, else local)
#   --machine-type T    Namespace machine type (default linux/amd64:8x16)
#   --duration D        instance lifetime cap, as nsc takes it (default 2h)
#   --executor E        nsc (default) or admitted boat:bx_ID
#   --out DIR           new results directory; any existing path is refused
#                       (default .artifacts/nsc-writes/<UTC time>.<unique suffix>)
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
CACHE=${SPARKLES_NSC_CACHE:-$HOME/.cache/sparkles-nsc}
# shellcheck source=scripts/remote-screen.sh
. "$ROOT/scripts/remote-screen.sh"
PEOPLE=100000
SCENARIOS="serial-literal writers mixed"
ROUNDS=3
CHANGES=3000
WRITER_CHANGES=20000
MIXED_CHANGES=5000
WRITE_RATE=0
INDEX=none
VIEWS=none
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
# shellcheck source=scripts/nsc-build-host.sh
. "$ROOT/scripts/nsc-build-host.sh"
ARGS=()
while [ $# -gt 0 ]; do
  case $1 in
    --people) PEOPLE=$2 ;;
    --scenarios) SCENARIOS=$2 ;;
    --rounds) ROUNDS=$2 ;;
    --changes) CHANGES=$2 ;;
    --writer-changes) WRITER_CHANGES=$2 ;;
    --mixed-changes) MIXED_CHANGES=$2 ;;
    --write-rate) WRITE_RATE=$2 ;;
    --index) INDEX=$2 ;;
    --views) VIEWS=$2 ;;
    --server-cpus) SERVER_CPUS=$2 ;;
    --client-cpus) CLIENT_CPUS=$2 ;;
    --cargo-args) CARGO_ARGS=$2 ;;
    --build-host) BH_ARG=$2 ;;
    --machine-type) MACHINE=$2 ;;
    --duration) DURATION=$2 ;;
    --out) OUT=$2 ;;
    --executor) EXECUTOR=$2 ;;
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
bh_check_arg
[[ $WRITE_RATE =~ ^[0-9]+([.][0-9]+)?$ ]] || die "--write-rate needs a number of requests per second, not $WRITE_RATE"
[[ $INDEX =~ ^(none|text|geo|embed)$ ]] || die "--index is none, text, geo or embed, not $INDEX"
case $VIEWS in
  none) ;;
  async:[1-9] | async:[1-9][0-9] | async:[1-9][0-9][0-9]) ;;
  commit:* | graph:*) die "--views $VIEWS needs maintenance at commit (ROADMAP item 421) or graph views (item 420), which the server does not have" ;;
  *) die "--views is none, async:N (N from 1 to 999), commit:Q or graph:N, not $VIEWS" ;;
esac
for spec in "${ARGS[@]}"; do
  # the remote script keeps each variant's settings in variables named after it
  [[ ${spec%%=*} =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || die "variant name ${spec%%=*} is not a shell identifier (letters, digits and underscores)"
done
screen_check
for t in zstd python3 cargo rustc cc flock; do command -v "$t" > /dev/null || die "$t is not on PATH"; done
if [ -n "$OUT" ]; then
  # Resolve symlinks and .. in the parent, while preserving the final name so
  # even a dangling destination symlink must be refused by the exclusive mkdir.
  OUT_PARENT=$(realpath -m -- "$(dirname -- "$OUT")")
  OUT_NAME=$(basename -- "$OUT")
  mkdir -p -- "$OUT_PARENT"
  OUT=$OUT_PARENT/$OUT_NAME
  mkdir -- "$OUT" || die "results directory already exists or cannot be created: $OUT"
else
  mkdir -p "$ROOT/.artifacts/nsc-writes"
  OUT=$(mktemp -d "$ROOT/.artifacts/nsc-writes/$(date -u +%Y%m%dT%H%M%SZ).XXXXXXXX")
fi
CACHE=$(realpath -m "$CACHE")
WRITE_CACHE=$CACHE/writes-v2
mkdir -p "$OUT/stage" "$OUT/logs" "$CACHE/data" "$WRITE_CACHE"/{bin,src,locks}
STAGE=$OUT/stage
MANIFEST=$OUT/manifest.txt
: > "$MANIFEST"
note() { echo "$*" | tee -a "$MANIFEST"; }
now() { date +%s.%N; }
secs() { awk "BEGIN {printf \"%.1f\", $2 - $1}"; }

# ------------------------------------------------------------------------- binaries
# binary <rev|path> <id>: stages bin-<id>.zst and notes its source and SHA-256
binary() (
  local ref=$1 id=$2 bin sha src='' publish='' snapshot='' key entry args_hash
  # All temporary paths are unique and owned by this invocation. The subshell keeps
  # its lock and cleanup independent of the eventual Namespace instance EXIT trap.
  trap '[ -z "$src" ] || rm -rf -- "$src"; [ -z "$publish" ] || rm -rf -- "$publish"; [ -z "$snapshot" ] || rm -f -- "$snapshot"; bh_cleanup' EXIT
  trap 'exit 130' INT TERM
  if [ -f "$ref" ]; then
    bin=$(realpath "$ref")
    snapshot=$(mktemp "$STAGE/.binary-$id.XXXXXXXX")
    cp -- "$bin" "$snapshot"
    note "binary $id: externally built file $bin (staged snapshot)"
    bin=$snapshot
  else
    sha=$(git -C "$ROOT" rev-parse --verify --quiet "$ref^{commit}") || die "$ref is neither a file nor a commit"
    args_hash=$(printf '%s' "$CARGO_ARGS" | sha256sum | cut -d' ' -f1)
    key=$sha-$args_hash$(bh_entry_key_suffix)
    entry=$WRITE_CACHE/bin/$key
    exec {build_lock}> "$WRITE_CACHE/locks/$key.lock"
    flock "$build_lock"
    # Recheck only after acquiring the lock. Old query builds use neither this
    # namespace and cannot replace a build's output.
    if [ ! -e "$entry" ] && [ ! -L "$entry" ] && { bh_resolve && [ "$BH_HOST" != local ]; }; then
      echo "building $ref ($sha) on $BH_HOST; log: $OUT/logs/build-$id.log"
      src=$(mktemp -d "$WRITE_CACHE/src/$key.XXXXXXXX")
      publish=$(mktemp -d "$WRITE_CACHE/bin/.$key.XXXXXXXX")
      # shellcheck disable=SC2086 # CARGO_ARGS is a list of flags
      bh_build "$BH_HOST" "$sha" "$src" $CARGO_ARGS > "$OUT/logs/build-$id.log" 2>&1 ||
        die "build of $ref on $BH_HOST failed, see $OUT/logs/build-$id.log"
      cp -- "$src/sparkles" "$publish/sparkles"
      chmod 755 "$publish/sparkles"
      python3 - "$publish" "$sha" "$CARGO_ARGS" "$args_hash" "$BH_HOST" "$src" << 'PY'
import datetime, hashlib, json, pathlib, sys
directory, revision, arguments, args_hash, host, received = sys.argv[1:]
path, received = pathlib.Path(directory), pathlib.Path(received)
environment = {}
for line in (received / 'env.txt').read_text().splitlines():
    name, _, value = line.partition('=')
    environment[name] = value
record = {
    'schema': 1, 'build_policy': 'mbx-managed-v1', 'revision': revision, 'cargo_args': arguments,
    'cargo_args_sha256': args_hash,
    'binary_sha256': hashlib.sha256((path / 'sparkles').read_bytes()).hexdigest(),
    'built_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'build_host': host,
    'rustc': (received / 'rustc.txt').read_text(), 'cargo': (received / 'cargo.txt').read_text(),
    'version_commands': 'rustc -vV and cargo -V in the extracted source directory on the build host',
    'command': 'scripts/nsc-build.sh (mbx build --release -p sparkles-server $CARGO_ARGS) in a git archive on build host ' + host,
    'artifact': {'manifest': 'crates/sparkles-server/Cargo.toml', 'name': 'sparkles', 'kind': ['bin']},
    'environment': environment,
}
(path / 'build.json').write_text(json.dumps(record, indent=2) + '\n')
PY
      mv -T -- "$publish" "$entry"
      publish=
      rm -rf -- "$src"
      src=
    fi
    if [ ! -e "$entry" ] && [ ! -L "$entry" ]; then
      echo "building $ref ($sha); log: $OUT/logs/build-$id.log"
      src=$(mktemp -d "$WRITE_CACHE/src/$key.XXXXXXXX")
      publish=$(mktemp -d "$WRITE_CACHE/bin/.$key.XXXXXXXX")
      git -C "$ROOT" archive "$sha" | tar -x -m -C "$src"
      bh_ui_placeholder "$src"
      (cd "$src" && rustc -vV) > "$publish/rustc.txt"
      (cd "$src" && cargo -V) > "$publish/cargo.txt"
      # The fixed native-release helper validates feature/dependency flags, uses
      # this source's managed target and copies the binary before its lease ends.
      # shellcheck disable=SC2086 # CARGO_ARGS is a list of flags
      (cd "$src" && bash "$ROOT/scripts/nsc-build.sh" "$publish/sparkles" $CARGO_ARGS) \
        > "$OUT/logs/build-$id.log" 2>&1 || die "build of $ref failed, see $OUT/logs/build-$id.log"
      python3 - "$publish" "$sha" "$CARGO_ARGS" "$args_hash" << 'PY'
import datetime, hashlib, json, os, pathlib, sys
directory, revision, arguments, args_hash = sys.argv[1:]
path = pathlib.Path(directory)
record = {
    'schema': 1, 'build_policy': 'mbx-managed-v1', 'revision': revision, 'cargo_args': arguments,
    'cargo_args_sha256': args_hash,
    'binary_sha256': hashlib.sha256((path / 'sparkles').read_bytes()).hexdigest(),
    'built_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'build_host': 'local (' + os.uname().nodename + ')',
    'rustc': (path / 'rustc.txt').read_text(), 'cargo': (path / 'cargo.txt').read_text(),
    'version_commands': 'rustc -vV and cargo -V in the extracted source directory',
    'command': 'scripts/nsc-build.sh (mbx build --release -p sparkles-server $CARGO_ARGS); native artifact copied under the same managed target lease',
    'artifact': {'manifest': 'crates/sparkles-server/Cargo.toml', 'name': 'sparkles', 'kind': ['bin']},
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
    if (record['schema'] != 1 or record.get('build_policy') != 'mbx-managed-v1' or record['revision'] != revision
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
    note "binary $id: commit $sha ($ref), build key $key, built on $(python3 -c 'import json, sys; print(json.load(open(sys.argv[1])).get("build_host") or "an unrecorded host")' "$entry/build.json")"
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
WRITE_RATE=$WRITE_RATE
INDEX=$INDEX
VIEWS=$VIEWS
SERVER_CPUS=$SERVER_CPUS
CLIENT_CPUS=$CLIENT_CPUS
EOF
note "data: $PEOPLE people (scripts/gen-data.py), $(du -h "$DATA" | cut -f1) compressed"
note "settings: variants $VARIANTS, scenarios $SCENARIOS, $ROUNDS rounds, serial $CHANGES, writers $WRITER_CHANGES, mixed $MIXED_CHANGES requests, write rate $WRITE_RATE/s (0: unpaced), index $INDEX, views $VIEWS, server CPUs $SERVER_CPUS, client CPUs $CLIENT_CPUS"

# ---------------------------------------------------------------------------- instance
ID=
T_CREATE=
VCPUS=${MACHINE##*:}
VCPUS=${VCPUS%%x*}
destroy() {
  local t
  [ -n "$ID" ] || return 0
  echo "destroying $ID"
  screen_destroy > "$OUT/logs/destroy.log" 2>&1 || {
    echo "cleanup uncertain; reconcile $OUT/logs/destroy.log and the exact recorded units" >&2
    return 97
  }
  if [ "$EXECUTOR" != nsc ]; then
    ID=
    return
  fi
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
screen_create "sparkles writes $VARIANTS" || die "executor acquire failed; reconcile $OUT before retrying"
T_UP=$(now)
note "instance: $ID ($MACHINE, capped at $DURATION)"
note "phase create: $(secs "$T_CREATE" "$T_UP") s"

rsh() { screen_exec "$@"; }
for f in "$STAGE"/bin-*.zst "$STAGE/data.nt.zst" "$STAGE/write-bench" "$STAGE/libiofault.so" "$STAGE/nsc-writes-remote.sh" "$STAGE/run.env"; do
  screen_upload "$f" "$REMOTE/$(basename "$f")" > "$OUT/logs/upload.log" 2>&1 || die "upload of $f failed"
done
T_SENT=$(now)
note "phase upload: $(secs "$T_UP" "$T_SENT") s"

screen_launch nsc-writes-remote.sh nohup.log ||
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
screen_download "$REMOTE/results.tgz" "$OUT/results.tgz" > "$OUT/logs/download.log" 2>&1 || die "download failed"
screen_unpack "$OUT/results.tgz" "$OUT"
note "phase download: $(secs "$T_RAN" "$(now)") s"
destroy
rm -f "$STAGE"/bin-*.zst "$STAGE/data.nt.zst"
[ "$status" = ok ] || die "the remote run failed ($status), see $OUT/results/progress.log"

# shellcheck disable=SC2086 # VARIANTS is a list of names
python3 "$ROOT/scripts/nsc-writes-summary.py" "$OUT" $VARIANTS > "$OUT/summary.md"
cat "$OUT/summary.md"
echo
echo "results: $OUT"
