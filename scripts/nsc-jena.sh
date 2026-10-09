#!/usr/bin/env bash
# The realistic Jena comparison of scripts/bench-bindings/jena.py on an ephemeral Namespace
# instance (namespace.so), driven through the `nsc` CLI. See docs/DEVELOPMENT.md.
#
#   scripts/nsc-jena.sh [options] [-- jena.py options...]
#
# The JVM bindings and their native library are built here, before any instance exists,
# as `mise run bench:jena` builds them. The instance's image has no JDK and no Python, so
# the script ships the bench classpath, the native library with its libgcc_s, a Temurin
# JDK 21 and a standalone CPython from uv, with the jena.py scripts. The JDK is downloaded
# once into $SPARKLES_NSC_CACHE/tools (default ~/.cache/sparkles-nsc).
#
# One instance is created with a hard --duration cap and destroyed on every exit, also on
# errors and Ctrl-C. jena.py runs detached on it with --no-build and the options after
# `--`, so a dropped SSH session loses nothing, and its progress log is polled. Its
# results directory, logs and env.txt (kernel, CPU, memory, Java) are downloaded to OUT.
#
# Options:
#   --out DIR           results directory (default target/nsc-jena/<UTC time>)
#   --extra-lib PATH    another native library to ship, repeatable. An arm uses it with
#                       -Dsparkles.native.path=/root/nsc-jena/repo/native/NAME
#   --no-build          ship the bindings as they are built
#   --machine-type T    Namespace machine type (default linux/amd64:8x16)
#   --duration D        instance lifetime cap, as nsc takes it (default 4h)
#
# Example, a feature switch against TDB2 and TIM:
#   scripts/nsc-jena.sh -- --arm sparkles=sparkles-mem \
#     --arm "before=sparkles-mem;-Dsparkles.smallQueries=0" --arm tdb2=tdb2 --arm tim=tim
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
CACHE=${SPARKLES_NSC_CACHE:-$HOME/.cache/sparkles-nsc}
MACHINE=linux/amd64:8x16
DURATION=4h
OUT=
BUILD=1
EXTRA_LIBS=()
POLL=${POLL:-30}
REMOTE=/root/nsc-jena
JDK_URL=https://api.adoptium.net/v3/binary/latest/21/ga/linux/x64/jdk/hotspot/normal/eclipse

die() {
  echo "nsc-jena: $*" >&2
  exit 1
}
usage() {
  sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
  exit "${1:-0}"
}
while [ $# -gt 0 ]; do
  case $1 in
    --out) OUT=$2 ;;
    --extra-lib) EXTRA_LIBS+=("$(realpath "$2")") ;;
    --no-build)
      BUILD=0
      shift
      continue
      ;;
    --machine-type) MACHINE=$2 ;;
    --duration) DURATION=$2 ;;
    -h | --help) usage ;;
    --)
      shift
      break
      ;;
    *) usage 1 ;;
  esac
  shift 2 || usage 1
done
for t in nsc zstd uv curl; do command -v "$t" > /dev/null || die "$t is not on PATH"; done
OUT=$(realpath -m "${OUT:-$ROOT/target/nsc-jena/$(date -u +%Y%m%dT%H%M%SZ)}")
mkdir -p "$OUT/logs"

if [ "$BUILD" = 1 ]; then
  echo "building the bindings"
  "$ROOT/scripts/jvm-native.sh" > "$OUT/logs/build.log" 2>&1 || die "the native build failed; see $OUT/logs/build.log"
  (cd "$ROOT/jvm" && ./gradlew --console=plain -q :sparkles-jena:bindingsBenchClasspath) >> "$OUT/logs/build.log" 2>&1 ||
    die "the Gradle build failed; see $OUT/logs/build.log"
fi
LIB=$ROOT/target/release/libsparkles_ffi.so
CLASSPATH_FILE=$ROOT/jvm/sparkles-jena/build/bindings-bench/classpath.txt
[ -e "$LIB" ] && [ -e "$CLASSPATH_FILE" ] || die "no built bindings; run without --no-build"

mkdir -p "$CACHE/tools"
JDK=$CACHE/tools/temurin21.tar.gz
if [ ! -e "$JDK" ]; then
  echo "downloading Temurin 21"
  curl -fsSL -o "$JDK.tmp" "$JDK_URL" && mv "$JDK.tmp" "$JDK"
fi
uv python install --quiet 3.12 > /dev/null 2>&1 || true
PYBIN=$(uv python find --managed-python 3.12) || die "no uv-managed CPython 3.12"
PY=$(dirname "$(dirname "$(readlink -f "$PYBIN")")")

STAGE=$OUT/stage
rm -rf "$STAGE"
mkdir -p "$STAGE/repo/scripts/bench-bindings" "$STAGE/repo/target/release" "$STAGE/repo/cp" \
  "$STAGE/repo/jvm/sparkles-jena/build/bindings-bench" "$STAGE/repo/native"
# the classpath, each entry copied to repo/cp/<n>-<name>
cp_remote=()
i=0
IFS=: read -r -a entries < "$CLASSPATH_FILE"
for e in "${entries[@]}"; do
  [ -e "$e" ] || continue
  n="$i-$(basename "$e")"
  cp -a "$e" "$STAGE/repo/cp/$n"
  cp_remote+=("$REMOTE/repo/cp/$n")
  i=$((i + 1))
done
bb=$STAGE/repo/jvm/sparkles-jena/build/bindings-bench
(
  IFS=:
  echo "${cp_remote[*]}"
) > "$bb/classpath.txt"
cp "$bb/classpath.txt" "$bb/classpath-calls.txt"
cp "$LIB" "$STAGE/repo/target/release/"
cp "$(ldd "$LIB" | awk '/libgcc_s/ {print $3}')" "$STAGE/repo/native/"
for f in "${EXTRA_LIBS[@]}"; do cp "$f" "$STAGE/repo/native/"; done
cp "$ROOT"/scripts/bench-bindings/{jena.py,bench.py} "$STAGE/repo/scripts/bench-bindings/"
cp "$ROOT"/scripts/{gen-data.py,bench.sh,bench-answers.py} "$STAGE/repo/scripts/"
tar -C "$STAGE" -cf - repo | zstd -q -T0 -3 -o "$STAGE/repo.tar.zst"
tar -C "$(dirname "$PY")" -cf - "$(basename "$PY")" | zstd -q -T0 -3 -o "$STAGE/python.tar.zst"
cp "$JDK" "$STAGE/jdk.tar.gz"
COMMIT=$(git -C "$ROOT" rev-parse --short=12 HEAD)
DIRTY=$(git -C "$ROOT" status --porcelain --untracked-files=no | head -c1)
{
  printf 'BENCH_COMMIT=%q\n' "$COMMIT${DIRTY:+-dirty}"
  printf 'ARGS=('
  printf '%q ' "$@"
  printf ')\n'
} > "$STAGE/run.env"
cat > "$STAGE/remote.sh" << 'EOF'
#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "$0")"
source ./run.env
mkdir -p work
exec > >(tee -a work/progress.log) 2>&1
zstd -q -d -c repo.tar.zst | tar -xf -
zstd -q -d -c python.tar.zst | tar -xf -
mkdir -p jdk && tar -xzf jdk.tar.gz -C jdk --strip-components=1
export JAVA=$PWD/jdk/bin/java
export LD_LIBRARY_PATH=$PWD/repo/native
export BENCH_COMMIT
PYBIN=$(ls -d "$PWD"/cpython-*/bin)/python3
{
  echo "kernel: $(uname -r)"
  echo "cpu: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//')"
  echo "cpus: $(grep -c ^processor /proc/cpuinfo)"
  echo "memory: $(free -m | awk '/Mem:/ {print $2}') MiB"
  echo "java: $("$JAVA" -version 2>&1 | head -1)"
} > work/env.txt
"$PYBIN" repo/scripts/bench-bindings/jena.py --no-build --workdir "$PWD/work" "${ARGS[@]}"
echo "exit $?" > work/done
EOF
echo "staged: $(du -ch "$STAGE"/*.zst "$STAGE"/jdk.tar.gz | tail -1 | cut -f1)"

ID=
destroy() {
  [ -n "$ID" ] || return 0
  echo "destroying $ID"
  nsc destroy --force "$ID" > "$OUT/logs/destroy.log" 2>&1 || echo "destroying $ID failed; run nsc destroy $ID" >&2
  ID=
}
trap destroy EXIT
trap 'exit 130' INT TERM
nsc create --bare --machine_type "$MACHINE" --duration "$DURATION" --purpose "sparkles jena use cases" \
  --cidfile "$OUT/instance.id" --output_json_to "$OUT/instance.json" > "$OUT/logs/create.log" 2>&1 || {
  [ -s "$OUT/instance.id" ] && ID=$(cat "$OUT/instance.id")
  die "nsc create failed; see $OUT/logs/create.log"
}
ID=$(cat "$OUT/instance.id")
echo "instance $ID"
for f in repo.tar.zst python.tar.zst jdk.tar.gz run.env remote.sh; do
  nsc instance upload "$ID" "$STAGE/$f" "$REMOTE/$f" --mkdir >> "$OUT/logs/upload.log" 2>&1 || die "the upload of $f failed"
done
nsc ssh -T "$ID" -- "cd $REMOTE && setsid nohup bash remote.sh > nohup.log 2>&1 < /dev/null &"
seen=0
fails=0
status=
while :; do
  sleep "$POLL"
  if ! r=$(nsc ssh -T "$ID" -- "tail -n +$((seen + 1)) $REMOTE/work/progress.log 2> /dev/null; [ -f $REMOTE/work/done ] && echo __DONE__ \$(cat $REMOTE/work/done)" 2> /dev/null); then
    fails=$((fails + 1))
    [ "$fails" -lt 10 ] || die "lost contact with $ID"
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
nsc ssh -T "$ID" -- "cd $REMOTE/work && tar -czf ../results.tgz results logs env.txt progress.log configs" || true
nsc instance download "$ID" "$REMOTE/results.tgz" "$OUT/results.tgz" > "$OUT/logs/download.log" 2>&1 || echo "the download failed" >&2
destroy
tar -xzf "$OUT/results.tgz" -C "$OUT" 2> /dev/null || true
rm -f "$STAGE"/*.zst "$STAGE"/jdk.tar.gz
echo "status: $status; results in $OUT"
