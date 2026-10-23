#!/usr/bin/env bash
# Run the Graphalytics BFS, WCC and PageRank examples natively against a Sparkles binary
# and compare every vertex with the pinned reference outputs. The binary defaults to
# target/debug/sparkles. Results go to a new directory given as the second argument
# (default: a fresh directory under target/graphalytics-native). Exit status is nonzero
# when any engine run or comparison fails. See testsuite/graphalytics/README.md.
# Usage: scripts/graphalytics-native.sh [binary] [output-directory]
set -euo pipefail
cd "$(dirname "$0")/.."

bin=${1:-target/debug/sparkles}
out=${2:-target/graphalytics-native/$(date +%Y%m%dT%H%M%S)}
[ -x "$bin" ] || {
  echo "no executable at $bin" >&2
  exit 2
}
mkdir -p "$out"
echo "binary $bin sha256 $(sha256sum "$bin" | cut -d' ' -f1)" | tee "$out/summary.txt"
echo "revision $(git rev-parse HEAD)" | tee -a "$out/summary.txt"

failed=0
for case in example-directed example-undirected; do
  python3 scripts/graphalytics-reference.py prepare "$case" "$out/$case" > /dev/null
  for alg in bfs wcc pr; do
    dir="$out/$case"
    if timeout 120 "$bin" query --data "$dir/graph.nt" --query "$dir/$alg.rq" --results json \
      > "$dir/$alg.json" 2> "$dir/$alg.err" &&
      python3 scripts/graphalytics-reference.py validate "$case" "$alg" "$dir/$alg.json" \
        > "$dir/$alg.receipt.json" 2> "$dir/$alg.cmp.err"; then
      echo "PASS $case $alg" | tee -a "$out/summary.txt"
    else
      echo "FAIL $case $alg (see $dir/$alg.err and $dir/$alg.cmp.err)" | tee -a "$out/summary.txt"
      failed=1
    fi
  done
done
exit $failed
