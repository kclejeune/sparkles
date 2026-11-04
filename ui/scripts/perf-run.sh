#!/bin/sh
# Runs the whole scale measurement of one UI build (ROADMAP item 725): a fresh server for
# each group of cases, so that a case that exhausts the server's memory does not spoil the
# next. It needs SPARKLES_BIN, and takes the UI build under test from SPARKLES_UI_DIR
# (default ui/build). Results go to PERF_OUT/PERF_LABEL.
#
#   SPARKLES_BIN=... PERF_OUT=DIR PERF_LABEL=baseline PERF_COMMIT=4ab93ecb sh scripts/perf-run.sh
#
# PERF_SERVER_MEM caps the server (default 18G) and PERF_BROWSER_MEM the browser (default
# 10G), each in a systemd-run scope when systemd-run exists. PERF_CHROMIUM names a Chromium.
set -eu
cd "$(dirname "$0")/.."
: "${SPARKLES_BIN:?SPARKLES_BIN names the server binary}"
: "${PERF_OUT:?PERF_OUT names the directory for the results}"
: "${PERF_LABEL:?PERF_LABEL names the build under test}"
SPARKLES_UI_DIR="${SPARKLES_UI_DIR:-$PWD/build}"
PORT="${SPARKLES_SEED_PORT:-18725}"
export SPARKLES_BIN SPARKLES_UI_DIR PERF_OUT PERF_LABEL
export SPARKLES_SEED_PORT="$PORT" SPARKLES_PERF_MEM="${PERF_SERVER_MEM:-18G}"
export PERF_URL="http://127.0.0.1:$PORT"
mkdir -p "$PERF_OUT/$PERF_LABEL"
LOG="$PERF_OUT/$PERF_LABEL/run.log"

scoped() {
  if command -v systemd-run > /dev/null 2>&1; then
    systemd-run --user --scope -q -p "MemoryMax=${PERF_BROWSER_MEM:-10G}" "$@"
  else
    "$@"
  fi
}

# one group: its Playwright arguments, with the server started before and stopped after
group() {
  echo "== $* $(date +%T)" | tee -a "$LOG"
  node scripts/perf-seed.mjs > "$PERF_OUT/$PERF_LABEL/server.log" 2>&1 &
  server=$!
  for _ in $(seq 1 600); do
    grep -q '^ready' "$PERF_OUT/$PERF_LABEL/server.log" && break
    sleep 1
  done
  grep -q '^ready' "$PERF_OUT/$PERF_LABEL/server.log" || {
    echo "no server" | tee -a "$LOG"
    kill "$server"
    return 1
  }
  scoped node node_modules/@playwright/test/cli.js test --config playwright.perf.config.ts "$@" >> "$LOG" 2>&1 || true
  kill "$server" 2> /dev/null || true
  wait "$server" 2> /dev/null || true
}

PERF_ONLY=open-chebi,reveal-chebi-water,open-unesco-thesaurus group workbench.perf -g open
group explore.perf
PERF_ONLY=open-taxon PERF_LOAD_TIMEOUT_MS="${PERF_TAXON_TIMEOUT_MS:-240000}" group workbench.perf -g open
PERF_ONLY=reveal-taxon-deepest,reveal-taxon-past-first-page group workbench.perf -g open
group workbench.perf -g scroll
echo ALL_DONE | tee -a "$LOG"
