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
# --thresholds is not given. Without one, only failures count, which is how the
# qualification runs are made (scripts/timing-cohort.py qualify).
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
rc=0
rm -f target/nextest/default/junit.xml
mbx nextest run --workspace --no-fail-fast --test-threads 1 --retries 0 -E 'group(host-timing)' > "$out/nextest.log" 2>&1 || rc=$?
cp target/nextest/default/junit.xml "$out/junit.xml" || {
  echo "no JUnit report; see $out/nextest.log" >&2
  exit 1
}
args=()
[ -z "$thresholds" ] || args=(--thresholds "$thresholds")
python3 scripts/timing-cohort.py check "$out" --commit "$(git rev-parse --short HEAD)" "${args[@]}" || rc=1
echo "results in $out"
exit $rc
