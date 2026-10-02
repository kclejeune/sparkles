#!/usr/bin/env bash
# Fuzz the formatter: each target in turn for a time budget, at low priority, with
# libFuzzer's fork mode so one crash does not end the run (every crash, timeout and
# out-of-memory input lands in artifacts/<target>/). Seeds the corpora first when they
# are missing (seed.sh). Needs a nightly toolchain and cargo-fuzz:
#   rustup toolchain install nightly --profile minimal
#   cargo install cargo-fuzz --locked
#
# Usage: run.sh [--time SECONDS] [--jobs N] [target...]
#   --time  seconds per target (default 1800)
#   --jobs  fuzzing processes per target (default 2; the machine is often shared)
#   target  fmt_sparql, fmt_turtle, fmt_lines, fmt_jsonld (default: all four)
# Opt-in (`mise run fmt:fuzz`), not part of `mise run ci`.
set -euo pipefail

fuzz="$(cd "$(dirname "$0")" && pwd)"
time=1800
jobs=2
targets=()
while [ $# -gt 0 ]; do
  case "$1" in
    --time)
      time="$2"
      shift 2
      ;;
    --jobs)
      jobs="$2"
      shift 2
      ;;
    -h | --help)
      sed -n '2,13p' "$0"
      exit 0
      ;;
    *)
      targets+=("$1")
      shift
      ;;
  esac
done
[ ${#targets[@]} -gt 0 ] || targets=(fmt_sparql fmt_turtle fmt_lines fmt_jsonld)

if ! cargo +nightly fuzz --version > /dev/null 2>&1; then
  echo "run.sh: needs a nightly toolchain and cargo-fuzz:" >&2
  echo "  rustup toolchain install nightly --profile minimal" >&2
  echo "  cargo install cargo-fuzz --locked" >&2
  exit 2
fi

cd "$fuzz"
# the line formats' chunks go to rayon's pool: keep it to two threads per process
export RAYON_NUM_THREADS="${RAYON_NUM_THREADS:-2}"
found=0
for target in "${targets[@]}"; do
  if [ ! -d "corpus/$target" ] || [ -z "$(ls -A "corpus/$target")" ]; then
    ./seed.sh "$target"
  fi
  echo "== $target: ${time}s, $jobs jobs"
  # -a: debug assertions on, so the library's own consistency checks run too; a run
  # that found something fails, and the next target still runs
  nice -n 19 cargo +nightly fuzz run -a "$target" -- \
    -fork="$jobs" -ignore_crashes=1 -ignore_timeouts=1 -ignore_ooms=1 \
    -max_total_time="$time" -max_len=8192 -timeout=10 -rss_limit_mb=2048 ||
    found=1
done

echo "artifacts (crashes, timeouts, out of memory):"
find artifacts -type f 2> /dev/null || true
exit "$found"
