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

# ------------------------------------------------------------------------ inference
# The materialization of the reasoning suites (LUBM, OWL2Bench; docs/specs/X06 sections 3.3
# and 3.4, F16 section 12). Both read WORK/base.nt and write WORK/results/inference.json.

# inferred <key>: whether inference.json already has that materialization
inferred() { python3 -I -c 'import json, sys; sys.exit(sys.argv[2] not in json.load(open(sys.argv[1])))' "$RES_DIR/inference.json" "$1" 2> /dev/null; }

# suite_infer <profile>: load base.nt into base.db once, materialize a copy with
# `sparkles infer --profile <profile>`, and write <profile>.nt (the data with its inferred
# triples). inference.json gets the wall time, the peak RSS (GNU time), the derived triples,
# the reasoner's own peak heap estimate and thread count (`infer --status`, null for a binary
# that does not report them), and, unless DERIVED_DIGEST=0, the SHA-256 of the sorted derived
# triples, so two binaries' closures can be compared. INFER_THREADS sets `--threads`.
# CLOSURE_DUMP=0 leaves out <profile>.nt, which only the query engines read, for an
# acceptance run with no engines whose closure would not fit on the disk (LUBM(1000) is
# about 30 GB of N-Triples). The total triples are then the store's distinct base triples
# plus the derived ones.
suite_infer() {
  local p=$1 gtime derived t0 t1 status digest=null threads=()
  gtime=$(gnu_time)
  [ -n "${INFER_THREADS:-}" ] && threads=(--threads "$INFER_THREADS")
  [ -d base.db ] || "$SPARKLES" load --loc base.db base.nt > logs/load-base.log 2>&1 || die "loading base.nt failed, see logs/load-base.log"
  rm -rf "infer-$p.db"
  cp -a --reflink=auto base.db "infer-$p.db"
  sync
  t0=$(date +%s.%N)
  "$gtime" -v -o "logs/infer-$p.time" "$SPARKLES" infer --loc "infer-$p.db" --profile "$p" "${threads[@]}" > "logs/infer-$p.log" 2>&1 ||
    die "infer --profile $p failed, see logs/infer-$p.log"
  t1=$(date +%s.%N)
  derived=$(sed -n 's/^\([0-9]*\) inferred triples.*/\1/p' "logs/infer-$p.log" | tail -1)
  [[ $derived =~ ^[0-9]+$ ]] || die "infer --profile $p did not report its inferred triples, see logs/infer-$p.log"
  status=$("$SPARKLES" infer --loc "infer-$p.db" --status --format json 2> "logs/infer-$p-status.log") || status='{}'
  if [ "${DERIVED_DIGEST:-1}" != 0 ]; then
    # The derived graph streams from the dump through sort into one pass that counts and
    # hashes it, so no copy of it is written. sort still spills to temporary files in the
    # workdir, which take about the size of the derived graph in N-Triples.
    local hashed
    hashed=$("$SPARKLES" dump --loc "infer-$p.db" --graph urn:x-sparkles:inferred --merge --format nt 2> "logs/dump-derived-$p.log" |
      LC_ALL=C sort -S 25% -T . | python3 -I -c '
import hashlib, sys
h, n = hashlib.sha256(), 0
for b in iter(lambda: sys.stdin.buffer.read(1 << 20), b""):
    h.update(b)
    n += b.count(b"\n")
print(n, h.hexdigest())') || die "dumping the derived graph of $p failed, see logs/dump-derived-$p.log"
    [ "${hashed% *}" = "$derived" ] ||
      die "the derived graph of $p has ${hashed% *} triples, but infer reported $derived (logs/dump-derived-$p.log)"
    digest=\"${hashed#* }\"
  fi
  local base total
  base=$(wc -l < base.nt)
  if [ "${CLOSURE_DUMP:-1}" = 0 ]; then
    [ -z "${ENGINES// /}" ] || die "CLOSURE_DUMP=0 needs ENGINES=\"\", since the engines load $p.nt"
    # the input can repeat a statement, so the closure's size is the store's distinct base
    # triples plus the derived ones, which are new by definition
    local stored
    stored=$("$SPARKLES" query --loc base.db --results tsv 'SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }' 2> "logs/count-base.log" |
      sed -n '2s/^"\{0,1\}\([0-9][0-9]*\).*/\1/p')
    [[ $stored =~ ^[0-9]+$ ]] || die "counting the stored base triples failed, see logs/count-base.log"
    total=$((stored + derived))
  else
    "$SPARKLES" dump --loc "infer-$p.db" --merge --format nt --out "$p.tmp.nt" > /dev/null 2> "logs/dump-$p.log" ||
      die "dumping the closure of $p failed, see logs/dump-$p.log"
    mv "$p.tmp.nt" "$p.nt"
    total=$(wc -l < "$p.nt")
  fi
  python3 -I - "$RES_DIR/inference.json" "$p" "$(awk "BEGIN {print $t1 - $t0}")" "logs/infer-$p.time" "${derived:-0}" \
    "$base" "$total" "$status" "$digest" "${INFER_NOTES:-}" << 'EOF'
import json, os, re, sys
f, p, secs, timef, derived, base, total, status, digest, notes = sys.argv[1:]
rss = int(re.search(r"Maximum resident set size \(kbytes\): (\d+)", open(timef).read()).group(1))
try:
    run = json.loads(status).get("run") or {}
except ValueError:
    run = {}
d = json.load(open(f)) if os.path.exists(f) else {}
d[p] = {"seconds": float(secs), "max_rss_kib": rss, "derived_triples": int(derived),
        "base_triples": int(base), "total_triples": int(total),
        "peak_bytes": run.get("peakBytes"), "threads": run.get("threads"), "method": run.get("method"),
        "derived_sha256": json.loads(digest)}
if notes:
    d["notes"] = notes.split("\n")
json.dump(d, open(f, "w"), indent=1)
peak = run.get("peakBytes")
print(f"{p}: {float(secs):.1f} s, peak RSS {rss >> 10} MiB, peak heap {peak >> 20 if peak else '?'} MiB, {derived} derived triples")
EOF
  rm -rf "infer-$p.db"
}

# inference_table <inference.json>: the materializations as a Markdown table
inference_table() {
  python3 -I - "$1" << 'EOF'
import json, sys
d = json.load(open(sys.argv[1]))
print("\n## Materialization\n")
print("| Engine | Profile | Status | Seconds | Peak RSS (MiB) | Peak heap estimate (MiB) | Derived triples | Closure triples |")
print("|---|---|---|---:|---:|---:|---:|---:|")
mib = lambda v, s=0: f"{v / (1 << s):.0f}" if isinstance(v, (int, float)) else ""
for k, v in d.items():
    if not isinstance(v, dict):
        continue
    if v.get("engine") == "jena":
        print(f"| Jena {v['reasoner']} | {v['profile']} | {v['status']} | {v['seconds']:.1f} (start and parse {v['load_seconds']:.1f}) "
              f"| {mib(v.get('max_rss_kib'), 10)} | | | {v.get('closure_triples') or ''} |")
    else:
        print(f"| Sparkles | {k} | ok | {v['seconds']:.1f} | {mib(v['max_rss_kib'], 10)} | {mib(v.get('peak_bytes'), 20)} "
              f"| {v['derived_triples']} | {v['total_triples']} |")
for n in d.get("notes", []):
    print(f"\n{n}.")
EOF
}

# jena_reason <profile> <reasoner>: the materialization of base.nt by one of Jena's rule
# reasoners, for comparison with suite_infer. Jena has no command that writes a closure, so
# `sparql` counts every triple of an inference model (ja:InfModel over an in-memory copy of
# base.nt), which makes the forward and the backward rules derive everything. The same count
# without the reasoner gives the time to start the JVM and parse the file, which inference.json
# records apart as `load_seconds`. The reasoner is rdfs, owl-micro, owl-mini or owl (Jena's
# full OWL rule set). The run is limited to JENA_REASON_TIMEOUT seconds (default 3600) and
# JENA_HEAP (default 8G); a run that hits either is recorded as a timeout or an error, not
# retried. Jena's closure counts are not comparable with Sparkles' derived triples: its rule
# sets derive other triples (rdfs:Resource types, owl:sameAs of every individual with itself).
jena_reason() {
  local p=$1 r=$2 url gtime sparql t0 t1 tl rc n base_n st=ok
  case $r in
    rdfs) url=http://jena.hpl.hp.com/2003/RDFSExptRuleReasoner ;;
    owl-micro) url=http://jena.hpl.hp.com/2003/OWLMicroFBRuleReasoner ;;
    owl-mini) url=http://jena.hpl.hp.com/2003/OWLMiniFBRuleReasoner ;;
    owl) url=http://jena.hpl.hp.com/2003/OWLFBRuleReasoner ;;
    *) die "unknown Jena reasoner $r (rdfs, owl-micro, owl-mini or owl)" ;;
  esac
  gtime=$(gnu_time)
  sparql=$(nixbin apache-jena sparql)
  mkdir -p jena-reason
  cat > jena-reason/plain.ttl << EOF
@prefix ja: <http://jena.hpl.hp.com/2005/11/Assembler#> .
<#dataset> a ja:RDFDataset ; ja:defaultGraph <#base> .
<#base> a ja:MemoryModel ; ja:content [ ja:externalContent <file://$WORK/base.nt> ] .
EOF
  cat > "jena-reason/$r.ttl" << EOF
@prefix ja: <http://jena.hpl.hp.com/2005/11/Assembler#> .
<#dataset> a ja:RDFDataset ; ja:defaultGraph <#inf> .
<#inf> a ja:InfModel ; ja:baseModel <#base> ; ja:reasoner [ ja:reasonerURL <$url> ] .
<#base> a ja:MemoryModel ; ja:content [ ja:externalContent <file://$WORK/base.nt> ] .
EOF
  echo 'SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }' > jena-reason/count.rq
  log "Jena $r reasoner over base.nt (limit ${JENA_REASON_TIMEOUT:-3600} s, heap ${JENA_HEAP:-8G})"
  t0=$(date +%s.%N)
  # The nixpkgs wrapper puts its JDK first on PATH, but Jena's scripts prefer JAVA_HOME,
  # which a developer's environment may point at an older JDK than the jars need.
  env -u JAVA_HOME JVM_ARGS="-Xmx${JENA_HEAP:-8G}" timeout -k 30 "${JENA_REASON_TIMEOUT:-3600}" "$sparql" --desc=jena-reason/plain.ttl \
    --query=jena-reason/count.rq --results=csv > "logs/jena-plain.out" 2> "logs/jena-plain.log" || die "Jena could not count base.nt, see logs/jena-plain.log"
  t1=$(date +%s.%N)
  tl=$(awk "BEGIN {print $t1 - $t0}")
  base_n=$(tail -1 logs/jena-plain.out | tr -d '\r')
  rc=0
  t0=$(date +%s.%N)
  env -u JAVA_HOME JVM_ARGS="-Xmx${JENA_HEAP:-8G}" "$gtime" -v -o "logs/jena-$r.time" "${CAP[@]}" timeout -k 30 "${JENA_REASON_TIMEOUT:-3600}" "$sparql" \
    --desc="jena-reason/$r.ttl" --query=jena-reason/count.rq --results=csv > "logs/jena-$r.out" 2> "logs/jena-$r.log" || rc=$?
  t1=$(date +%s.%N)
  n=$(tail -1 "logs/jena-$r.out" 2> /dev/null | tr -d '\r')
  # timeout exits with 124, or with 137 when the JVM ignored SIGTERM and needed SIGKILL
  if [ "$rc" = 124 ] || { [ "$rc" = 137 ] && awk "BEGIN {exit !($t1 - $t0 >= ${JENA_REASON_TIMEOUT:-3600})}"; }; then
    st=timeout
  elif grep -q OutOfMemoryError "logs/jena-$r.log" || [ "$rc" = 137 ]; then
    st=oom
  elif [ "$rc" != 0 ] || ! [[ $n =~ ^[0-9]+$ ]]; then
    st=error
  fi
  python3 -I - "$RES_DIR/inference.json" "jena-$r" "$p" "$(awk "BEGIN {print $t1 - $t0}")" "$tl" "logs/jena-$r.time" "$st" "${n:-}" "${base_n:-}" "$r" << 'EOF'
import json, os, re, sys
f, key, p, secs, load, timef, st, n, base, r = sys.argv[1:]
m = re.search(r"Maximum resident set size \(kbytes\): (\d+)", open(timef).read()) if os.path.exists(timef) else None
d = json.load(open(f)) if os.path.exists(f) else {}
ok = st == "ok"
d[key] = {"engine": "jena", "reasoner": r, "profile": p, "status": st, "seconds": float(secs),
          "load_seconds": float(load), "max_rss_kib": int(m.group(1)) if m else None,
          "closure_triples": int(n) if ok else None, "base_triples": int(base) if base.isdigit() else None}
json.dump(d, open(f, "w"), indent=1)
print(f"Jena {r}: {st}, {float(secs):.1f} s (of which about {float(load):.1f} s start and parse), closure {n or '?'}")
EOF
}

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
  for n in "${NAMES[@]}"; do
    selected "$n" || continue
    echo
    echo "== $n"
    time_query "$n" "$RES"
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
    if [ -f "$RES/inference.json" ]; then
      inference_table "$RES/inference.json" > "$RES/inference.md"
      log "materialization: $WORK/$RES/inference.md"
    fi
    return 0
  fi
  setj "$RES/extra.json" settings "maxTimeSeconds=$MAX_TIME" "serverTimeoutSeconds=$SERVER_TIMEOUT" \
    "sparklesArgs=${SPARKLES_ARGS:-}" "answerStreamTSVQueries=${ANSWERS_TSV_QUERIES:-}" "correctnessEngineIsolation=true" \
    "jenaHeap=${JENA_HEAP:-8G}" "jenaStack=${JENA_STACK:-default}" "qleverMemory=${QLEVER_MEMORY:-8G}"
  resolve_tools
  use_hyperfine
  trap stop_all EXIT
  suite_manifest
  [ -n "${SKIP_LOAD:-}" ] || load_all
  # An engine whose server does not start, or no longer starts, is left out of the rest of
  # the run, which goes on with the others and exits with status 1 at the end.
  local START_SOFT=1
  local checking=$ENGINES checked="" e
  for e in $checking; do
    ENGINES=$e
    start_all
    if [ -n "$ENGINES" ]; then
      checked="$checked $e"
      answer_check
    fi
    stop_all
  done
  ENGINES=${checked# }
  if [ -n "${SUITE_VERIFY:-}" ]; then "$SUITE_VERIFY"; fi
  if [ -z "${ANSWERS_ONLY:-}" ]; then
    # Each engine runs alone for correctness and timings (docs/specs/X06 section 5.3).
    # A fresh server answers every
    # selected query once before the timings, so it is as warm as the one that was checked.
    local all=$ENGINES e up=""
    for e in $all; do
      ENGINES=$e
      if ! start "$e"; then
        [ -z "${PID[$e]:-}" ] || stop "$e"
        FAILED_ENGINES="$FAILED_ENGINES $e"
        continue
      fi
      engine_started "$e"
      up="$up $e"
      warm_pass "$e"
      suite_time
      if declare -F "${SUITE_EXTRA:-}" > /dev/null; then "$SUITE_EXTRA"; fi
      stop "$e"
    done
    ENGINES=${up# }
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
  [ -f "$RES/inference.json" ] && inference_table "$RES/inference.json" >> "$RES/summary.md"
  log "record: $WORK/$RES/record.json, table: $WORK/$RES/summary.md"
  if [ -n "${FAILED_ENGINES// /}" ]; then
    log "error: these engines did not start, or stopped and did not start again, so results are missing from the record:$FAILED_ENGINES"
    return 1
  fi
}
