# shellcheck shell=bash disable=SC2034,SC2153 # the variables set here are used by the sourcing script
# The common run of the standard benchmark suites (scripts/bench-bsbm.sh, bench-sp2b.sh,
# bench-lubm.sh; docs/specs/X06 section 3): load one N-Triples file into the selected engines,
# serve them, check every answer, time the queries warm (and cold on request), and write the
# run manifest and the result record. Sourced after scripts/bench-lib.sh, never run.
#
# The suite script sets, before calling suite_run:
#   SUITE, SCALE   the record's suite and scale labels
#   DATAFILE       the data to load, relative to WORK (one N-Triples file)
#   NAMES          the query names; the texts are WORK/queries/<name>.rq
#   DATA_SPECS     bench-record.py --data arguments (name=path|license|params), as an array
#   LICENSE_NOTES  optional array of notes for the manifest's license check
#   COUNT_ONLY, NO_CHECK   optional query names compared by row count or not compared
#   SUITE_EXTRA    optional function run after the timings of each engine, with its server up
#   SUITE_AFTER    optional function run once every engine has been timed
#   POOL=1         the queries are <template>-<i> instances, recorded per template
# and the environment knobs of scripts/bench.sh: WARMUP, RUNS, ENGINES, QUERIES, SKIP_LOAD,
# ANSWERS_ONLY, PORT_BASE, SERVER_MEM_MAX, CLIENT_CPU, MAX_TIME, plus COLD_RUNS (restarts
# per query, default 0: no cold measurement).

selected() { [ -z "${QUERIES:-}" ] || [[ " $QUERIES " == *" $1 "* ]] || [[ " $QUERIES " == *" ${1%-*} "* ]]; }

suite_manifest() {
  local args=(--out "$RES/manifest.json" --suite "$SUITE" --sparkles "$SPARKLES" --workdir "$WORK"
    --memory-limit "${SERVER_MEM_MAX:-}" --client-cpu "${CLIENT[2]:-}")
  local spec n
  has jena && args+=(--engine "jena=$FUSEKI")
  has qlever && args+=(--engine "qlever=$QSERVER")
  has oxigraph && args+=(--engine "oxigraph=$OXIGRAPH")
  has fluree && args+=(--engine "fluree=$FLUREE")
  for spec in ${DATA_SPECS[@]+"${DATA_SPECS[@]}"}; do args+=(--data "$spec"); done
  for n in ${LICENSE_NOTES[@]+"${LICENSE_NOTES[@]}"}; do args+=(--license-note "$n"); done
  python3 "$ROOT/scripts/bench-record.py" manifest "${args[@]}" > /dev/null
}

suite_time() {
  local n
  # clears QLever's result cache before every run (a no-op without QLever); ENGINES is one engine here
  local clear=true
  has qlever && clear="curl -sf -o /dev/null 'localhost:${PORT[qlever]}/?cmd=clear-cache&access-token=bench'"
  for n in "${NAMES[@]}"; do
    selected "$n" || continue
    echo
    echo "== $n"
    # shellcheck disable=SC2329 # called by engine_args
    qcmd() { q "${URL[$1]}" "$n"; }
    mapfile -d '' ARGS < <(engine_args qcmd)
    "${CLIENT[@]}" hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic --ignore-failure --prepare "$clear" \
      "${ARGS[@]}" --export-json "$RES/$n.new.json"
    merge "$RES/$n.new.json" "$RES/$n.json"
  done
}

# warm_pass <engine>: every selected query once, untimed, in the format the timed runs ask
# for, so the server being timed has answered the whole suite (docs/specs/X06 section 5.2)
warm_pass() {
  local n
  log "$1: warm-up pass over the selected queries"
  for n in "${NAMES[@]}"; do
    selected "$n" || continue
    eval "$(q "${URL[$1]}" "$n")" || true
  done
}

suite_run() {
  RES=results
  WARMUP=${WARMUP:-2}
  RUNS=${RUNS:-10}
  COLD_RUNS=${COLD_RUNS:-0}
  mkdir -p "$RES" logs queries
  if [ -z "${ENGINES// /}" ]; then
    log "no engines selected: the data and the queries are ready in $WORK"
    return 0
  fi
  resolve_tools
  trap stop_all EXIT
  suite_manifest
  [ -n "${SKIP_LOAD:-}" ] || load_all
  start_all
  answer_check
  stop_all
  if [ -z "${ANSWERS_ONLY:-}" ]; then
    # Each engine runs alone for the timings (docs/specs/X06 section 5.3). The answer check
    # above may keep every server up, since it measures no time. A fresh server answers every
    # selected query once before the timings, so it is as warm as the one that was checked.
    local all=$ENGINES e
    for e in $all; do
      ENGINES=$e
      start "$e"
      warm_pass "$e"
      suite_time
      if declare -F "${SUITE_EXTRA:-}" > /dev/null; then "$SUITE_EXTRA"; fi
      stop "$e"
    done
    ENGINES=$all
    if declare -F "${SUITE_AFTER:-}" > /dev/null; then "$SUITE_AFTER"; fi
  fi
  if [ -z "${ANSWERS_ONLY:-}" ] && [ "$COLD_RUNS" -gt 0 ]; then
    local e
    for e in $ENGINES; do cold "$e"; done
  fi
  python3 "$ROOT/scripts/bench-record.py" finish "$RES/manifest.json"
  local names=() n pool=()
  for n in "${NAMES[@]}"; do selected "$n" && names+=("$n"); done
  [ -n "${POOL:-}" ] && pool=(--pool-instances)
  python3 "$ROOT/scripts/bench-record.py" build --suite "$SUITE" --scale "$SCALE" --results "$RES" \
    --manifest "$RES/manifest.json" --out "$RES/record.json" --warmup "$WARMUP" --runs "$RUNS" --max-time "$MAX_TIME" \
    --queries "${names[@]}" "${pool[@]}"
  python3 "$ROOT/scripts/bench-summary.py" "$RES" "${names[@]}" > /dev/null || log "bench-summary.py failed"
  log "record: $WORK/$RES/record.json, table: $WORK/$RES/summary.md"
}
