#!/usr/bin/env bash
# Run the host-timing test group of .config/nextest.toml alone, one test at a time, and
# check it against the worker's thresholds (`mise run test:timing-cohort`).
#
# Usage: scripts/test-timing-cohort.sh [--thresholds FILE] [--out DIR]
#
# The batch gate leaves these tests out of its pass/fail run. This runs them after the
# gate on one dedicated worker with no other job on it, and scripts/timing-cohort.py
# reports a test that failed, timed out or ran past its threshold, with a roadmap row
# for a fix-forward item. SPARKLES_TIMING_THRESHOLDS names the thresholds file when
# --thresholds is not given. Without one, only failures and coverage count, which is how
# the qualification runs are made (scripts/timing-cohort.py qualify).
# SPARKLES_TIMING_WORKER names the worker in the run's identity, such as boat:bx_ID, and
# defaults to the host name. The run's identity also records the commit, the compiler,
# the build configuration, nextest's exit status and the tests the group selects, and a
# check refuses thresholds from another worker or build configuration.
set -euo pipefail
cd "$(dirname "$0")/.."
thresholds=${SPARKLES_TIMING_THRESHOLDS:-}
out=.artifacts/timing-cohort/$(date -u +%Y%m%dT%H%M%SZ)-$(git rev-parse --short HEAD)
while [ $# -gt 0 ]; do
  case $1 in
    --thresholds)
      thresholds=${2:?--thresholds needs a file}
      shift
      ;;
    --out)
      out=${2:?--out needs a directory}
      shift
      ;;
    *)
      echo "usage: $0 [--thresholds FILE] [--out DIR]" >&2
      exit 2
      ;;
  esac
  shift
done
mkdir -p "$out"
git rev-parse HEAD > "$out/commit"
{
  hostname
  nproc
  uptime
} > "$out/host.txt" 2>&1
rustc -vV > "$out/rustc.txt"
{
  mbx --version
  echo "MBX_LINKER=${MBX_LINKER:-}"
  echo "RUSTFLAGS=${RUSTFLAGS:-}"
  python3 -c 'import json, sys, tomllib; print(json.dumps(tomllib.load(open("Cargo.toml", "rb")).get("profile", {}), sort_keys=True))'
} > "$out/build.txt"
filter='group(host-timing)'
rc=0
rm -f target/nextest/default/junit.xml
mbx nextest run --workspace --no-fail-fast --test-threads 1 --retries 0 -E "$filter" > "$out/nextest.log" 2>&1 || rc=$?
cp target/nextest/default/junit.xml "$out/junit.xml" || {
  echo "no JUnit report; see $out/nextest.log" >&2
  exit 1
}
mbx nextest list --workspace -E "$filter" --message-format json > "$out/list.json" 2> "$out/list.log" || {
  echo "the group's tests could not be listed; see $out/list.log" >&2
  exit 1
}
python3 scripts/timing-cohort.py record "$out" --nextest-exit "$rc" --list "$out/list.json" \
  --worker "${SPARKLES_TIMING_WORKER:-$(hostname)}"
args=()
[ -z "$thresholds" ] || args=(--thresholds "$thresholds")
check=0
python3 scripts/timing-cohort.py check "$out" --commit "$(git rev-parse --short HEAD)" "${args[@]}" || check=$?
[ "$check" -eq 0 ] || rc=$check
echo "results in $out"
exit $rc
