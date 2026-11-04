#!/bin/sh
# Runs the scale measurement of one or more UI builds, interleaved (ROADMAP item 725): in
# each repetition every build runs every group of cases in turn against the same binary and
# data, with a fresh server for each group so that a case that exhausts the server's memory
# does not spoil the next. Results go to PERF_OUT/LABEL, one directory a build.
#
#   SPARKLES_BIN=... PERF_OUT=DIR PERF_UIS="baseline=ui/build after=/path/ui/build:chebi,scroll" \
#     PERF_REPEAT=3 sh scripts/perf-run.sh
#
# PERF_UIS lists LABEL=DIR[:GROUPS] entries, where GROUPS limits a build to some of
# chebi, explore, taxon-open, taxon-reveal, scroll (the 1 million class taxon) and
# scroll-taxon (the 2.6 million class one). The default is all but scroll-taxon. PERF_COMMIT and
# PERF_NOTE are recorded. PERF_SERVER_MEM caps the server (default 14G) and PERF_BROWSER_MEM
# the browser (default 6G), each in a systemd-run scope when systemd-run exists.
# PERF_CHROMIUM names a Chromium. PERF_TAXON_TIMEOUT_MS bounds the open of the taxon.
set -eu
cd "$(dirname "$0")/.."
: "${SPARKLES_BIN:?SPARKLES_BIN names the server binary}"
: "${PERF_OUT:?PERF_OUT names the directory for the results}"
: "${PERF_UIS:?PERF_UIS lists LABEL=DIR[:GROUPS] entries}"
REPEAT="${PERF_REPEAT:-3}"
PORT="${SPARKLES_SEED_PORT:-18725}"
export SPARKLES_BIN PERF_OUT SPARKLES_SEED_PORT="$PORT"
export SPARKLES_PERF_MEM="${PERF_SERVER_MEM:-14G}"
export PERF_URL="http://127.0.0.1:$PORT" PERF_REPEAT=1
ALL=chebi,explore,taxon-open,taxon-reveal,scroll

scoped() {
  if command -v systemd-run > /dev/null 2>&1; then
    systemd-run --user --scope -q -p "MemoryMax=${PERF_BROWSER_MEM:-6G}" "$@"
  else
    "$@"
  fi
}

# one group: its Playwright arguments, with the server started before and stopped after
group() {
  echo "== $PERF_LABEL rep $PERF_REP_BASE: $* $(date +%T), load $(cut -d' ' -f1-3 /proc/loadavg)" >> "$LOG"
  node scripts/perf-seed.mjs > "$PERF_OUT/$PERF_LABEL/server.log" 2>&1 &
  server=$!
  for _ in $(seq 1 600); do
    grep -q '^ready' "$PERF_OUT/$PERF_LABEL/server.log" && break
    sleep 1
  done
  if grep -q '^ready' "$PERF_OUT/$PERF_LABEL/server.log"; then
    scoped node node_modules/@playwright/test/cli.js test --config playwright.perf.config.ts "$@" >> "$LOG" 2>&1 || true
  else
    echo "no server" >> "$LOG"
  fi
  kill "$server" 2> /dev/null || true
  wait "$server" 2> /dev/null || true
}

wants() { case ",$GROUPS_WANTED," in *",$1,"*) return 0 ;; *) return 1 ;; esac }

rep=0
while [ "$rep" -lt "$REPEAT" ]; do
  for entry in $PERF_UIS; do
    PERF_LABEL="${entry%%=*}"
    rest="${entry#*=}"
    export SPARKLES_UI_DIR="${rest%%:*}"
    GROUPS_WANTED="$ALL"
    case "$rest" in *:*) GROUPS_WANTED="${rest#*:}" ;; esac
    export PERF_LABEL PERF_REP_BASE="$rep"
    mkdir -p "$PERF_OUT/$PERF_LABEL"
    LOG="$PERF_OUT/$PERF_LABEL/run.log"
    wants chebi && PERF_ONLY=open-chebi,reveal-chebi-water,open-unesco-thesaurus group workbench.perf -g open
    wants explore && group explore.perf
    wants taxon-open && PERF_ONLY=open-taxon PERF_LOAD_TIMEOUT_MS="${PERF_TAXON_TIMEOUT_MS:-240000}" group workbench.perf -g open
    wants taxon-reveal && PERF_ONLY=reveal-taxon-deepest,reveal-taxon-past-first-page group workbench.perf -g open
    wants scroll && PERF_SCROLL_DS=taxon1m group workbench.perf -g scroll
    wants scroll-taxon && PERF_SCROLL_DS=taxon group workbench.perf -g scroll
  done
  rep=$((rep + 1))
done
echo ALL_DONE
