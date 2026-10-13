#!/usr/bin/env bash
# Fuzz the Office converter for a time budget, at low priority, with libFuzzer's fork
# mode so one crash does not end the run (every crash, timeout and out-of-memory input
# lands in artifacts/office_convert/). Seeds the corpus first when it is missing. Needs a
# nightly toolchain and cargo-fuzz:
#   rustup toolchain install nightly --profile minimal
#   cargo install cargo-fuzz --locked
#
# Usage: run.sh [--time SECONDS] [--jobs N]
#   --time  seconds (default 1800)
#   --jobs  fuzzing processes (default 2; the machine is often shared)
# Opt-in (`mise run office:fuzz`), not part of `mise run ci`.
set -euo pipefail

fuzz="$(cd "$(dirname "$0")" && pwd)"
time=1800
jobs=2
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
      sed -n '2,12p' "$0"
      exit 0
      ;;
    *)
      echo "run.sh: unknown argument $1" >&2
      exit 2
      ;;
  esac
done

if ! cargo +nightly fuzz --version > /dev/null 2>&1; then
  echo "run.sh: needs a nightly toolchain and cargo-fuzz:" >&2
  echo "  rustup toolchain install nightly --profile minimal" >&2
  echo "  cargo install cargo-fuzz --locked" >&2
  exit 2
fi

cd "$fuzz"
if [ ! -d corpus/office_convert ] || [ -z "$(ls -A corpus/office_convert)" ]; then
  cargo run --quiet --manifest-path ../Cargo.toml --example seed --features fixtures -- corpus/office_convert
fi
found=0
nice -n 19 cargo +nightly fuzz run -a office_convert -- \
  -fork="$jobs" -ignore_crashes=1 -ignore_timeouts=1 -ignore_ooms=1 \
  -max_total_time="$time" -max_len=65536 -timeout=10 -rss_limit_mb=2048 ||
  found=1

echo "artifacts (crashes, timeouts, out of memory):"
find artifacts -type f 2> /dev/null || true
exit "$found"
