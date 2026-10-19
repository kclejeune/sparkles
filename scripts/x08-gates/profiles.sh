#!/usr/bin/env bash
# The X08 profile gate of §3.3: the query suite on one store with the `default` profile, the
# `small` profile and the defaults before X08, warm and cold, through
# scripts/bench-regressions.py. The three settings take turns in each round, and the order
# reverses every round (default, small, earlier, then earlier, small, default), so no setting
# always runs first. profiles.py compares each profile with the earlier defaults.
#
#   scripts/x08-gates/profiles.sh OUT BIN STORE QUERIES [ROUNDS]
#
# STORE is a loaded Sparkles store, served read-only and never changed. QUERIES is a
# directory of .rq files, all of which run, or QUERIES=dir:"name name" for a subset. Env:
# RUNS (warm samples per round, default 7), COLD_RUNS (cold samples per round, default 3),
# MODES (default "warm cold"), PORT (default 5671), SERVER_CPUS (as bench-regressions.py
# takes it). Run it on a quiet host while holding the host's benchmark lock.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
USAGE="usage: profiles.sh OUT BIN STORE QUERIES [ROUNDS]"
OUT=$(realpath -m "${1:?$USAGE}")
BIN=$(realpath "${2:?$USAGE}")
STORE=$(realpath "${3:?$USAGE}")
QARG=${4:?$USAGE}
ROUNDS=${5:-3}
QDIR=${QARG%%:*}
if [[ $QARG == *:* ]]; then
  read -r -a NAMES <<< "${QARG#*:}"
else
  mapfile -t NAMES < <(find "$QDIR" -maxdepth 1 -name '[!_]*.rq' -printf '%f\n' | sed 's/\.rq$//' | sort)
fi
[ ${#NAMES[@]} -gt 0 ] || {
  echo "no queries in $QDIR" >&2
  exit 1
}
declare -A ARGS=(
  [default]=""
  [small]="--memory-profile small"
  # the defaults before X08: a 1 GiB block cache and 1 GiB history cache per dataset, no pool
  [earlier]="--cache-mb 1024 --history-cache-mb 1024 --service-cache-mb 64 --query-memory-mb 8192 --total-query-memory-mb 0"
)
ORDER=(default small earlier)
mkdir -p "$OUT"
cpus=()
[ -n "${SERVER_CPUS:-}" ] && cpus=(--server-cpus "$SERVER_CPUS")
for mode in ${MODES:-warm cold}; do
  for r in $(seq 1 "$ROUNDS"); do
    order=("${ORDER[@]}")
    if [ $((r % 2)) = 0 ]; then order=(earlier small default); fi
    for c in "${order[@]}"; do
      sa=()
      for w in ${ARGS[$c]}; do sa+=("--serve-arg=$w"); done
      m=()
      runs=${RUNS:-7}
      if [ "$mode" = cold ]; then
        m=(--cold)
        runs=${COLD_RUNS:-3}
      fi
      echo "[$(date +%T)] $mode round $r: $c"
      python3 "$HERE/../bench-regressions.py" --binary "$BIN" --db "$STORE" --queries "$QDIR" --names "${NAMES[@]}" \
        --out "$OUT/$mode/$c-r$r.json" --runs "$runs" --port "${PORT:-5671}" --keep-going "${m[@]}" "${cpus[@]}" \
        "${sa[@]}" > "$OUT/$mode-$c-r$r.log" 2>&1 || echo "  failed, see $OUT/$mode-$c-r$r.log"
    done
  done
done
python3 "$HERE/profiles.py" "$OUT" | tee "$OUT/summary.md"
