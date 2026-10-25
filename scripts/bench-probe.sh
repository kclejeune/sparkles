# shellcheck shell=bash
# Timing of one query on every engine of ENGINES, shared by scripts/bench.sh, the suites of
# scripts/bench-suite-lib.sh, scripts/bench-watdiv.sh and scripts/bench-billion.sh. The
# caller defines q, merge, log, ENGINES, URL, NAME, WARMUP, RUNS and CLIENT, and start and
# stop when its engines can be restarted one at a time.
#
# time_query <query> <results dir> [prepare command] sends one untimed request to each
# engine first. An engine whose first request fails or reaches MAX_TIME is recorded with
# that one sample, as an error or a timeout (curl exit 28), and is not timed, so a query that
# cannot finish costs MAX_TIME once instead of WARMUP + RUNS times. hyperfine then times the
# other engines. After a failure, and after the timed runs, every engine must answer a
# one-row query within 30 s. A server can answer its ping while every query waits on a lock:
# Fuseki did after a StackOverflowError in its property path engine left TDB2's node cache
# locked. An engine that does not answer is restarted when the caller can restart it, and
# the restart is noted in <results dir>/restarts.json. Otherwise it is left out of the
# remaining queries, each of which records it as an error.

DEAD_ENGINES=${DEAD_ENGINES:-}

engine_started() {
  local engine=$1 other kept=""
  for other in $DEAD_ENGINES; do
    [ "$other" = "$engine" ] || kept="$kept $other"
  done
  DEAD_ENGINES=${kept# }
}

# Resolve preparation after the probes, since a restart can change QLever's URL.
# A server left out of the remaining queries has no cache to clear.
cache_prepare() {
  if [[ " $ENGINES " == *" qlever "* && " $DEAD_ENGINES " != *" qlever "* ]]; then
    printf 'curl -sf --max-time 30 -o /dev/null %q\n' "${URL[qlever]}?cmd=clear-cache&access-token=bench"
  else
    echo true
  fi
}

# answering <engine>: a one-row query answers within 30 s
answering() {
  curl -sf -o /dev/null --max-time 30 -H 'Accept: application/sparql-results+json' \
    --data-urlencode 'query=SELECT * WHERE { ?s ?p ?o } LIMIT 1' "${URL[$1]}" 2> /dev/null
}

# revive <engine> <after> <results dir>: restart an engine that no longer answers
revive() {
  if answering "$1"; then
    engine_started "$1"
    return 0
  fi
  if declare -F start > /dev/null && declare -F stop > /dev/null; then
    log "$1: no answer after $2, restarting it"
    stop "$1"
    if ! start "$1"; then
      # only with START_SOFT; without it a failed start ends the run
      log "$1: did not start again; it is left out of the remaining queries"
      DEAD_ENGINES="$DEAD_ENGINES $1"
      FAILED_ENGINES="${FAILED_ENGINES:-} $1"
      return 0
    fi
    engine_started "$1"
    python3 - "$3/restarts.json" "${NAME[$1]}" "$2" << 'EOF'
import json, os, sys, time
f, engine, after = sys.argv[1:]
d = json.load(open(f)) if os.path.exists(f) else []
d.append({"engine": engine, "after": after, "at": time.strftime("%Y-%m-%dT%H:%M:%S")})
json.dump(d, open(f, "w"), indent=1)
EOF
  else
    log "$1: no answer after $2, and it cannot be restarted alone; it is left out of the remaining queries"
    DEAD_ENGINES="$DEAD_ENGINES $1"
  fi
}

# failed_result <json> <command> <seconds> <exit> [stage]: a failed sample, read as a
# timeout or error. The default stage is the first request, before timing.
failed_result() {
  python3 - "$@" << 'EOF'
import json, os, sys
f, cmd, secs, code = sys.argv[1], sys.argv[2], float(sys.argv[3]), int(sys.argv[4])
timed = len(sys.argv) > 5 and sys.argv[5] == "timed"
d = json.load(open(f)) if os.path.exists(f) else {"results": []}
d["results"] = [r for r in d["results"] if r.get("command") != cmd]
d["results"].append({"command": cmd, "mean": secs, "stddev": None, "median": secs, "user": 0.0, "system": 0.0,
                     "min": secs, "max": secs, "times": [secs], "exit_codes": [code],
                     "first_request_failed": not timed, "timed_run_failed": timed})
json.dump(d, open(f, "w"), indent=1)
EOF
}

time_query() {
  local n=$1 res=$2 prepare=${3:-} all=$ENGINES ok="" e rc t0 t1 clear hf
  local ENGINES=$ENGINES
  local new=$res/$n.new.json
  rm -f "$new"
  for e in $all; do
    if [[ " $DEAD_ENGINES " == *" $e "* ]]; then
      failed_result "$new" "${NAME[$e]}" 0 1
      continue
    fi
    t0=$(date +%s.%N)
    rc=0
    eval "$(q "${URL[$e]}" "$n")" > /dev/null || rc=$?
    t1=$(date +%s.%N)
    # curl exit 7: nothing listens. The server died before this query, for example when an
    # earlier query of the answer check or the warm-up pass ran it out of memory, so it is
    # restarted and the query gets its first request on the new server.
    if [ "$rc" = 7 ] && ! answering "$e"; then
      log "$e: not running before $n, restarting it"
      revive "$e" "before $n" "$res"
      t0=$(date +%s.%N)
      rc=0
      eval "$(q "${URL[$e]}" "$n")" > /dev/null || rc=$?
      t1=$(date +%s.%N)
    fi
    if [ "$rc" = 0 ]; then
      ok="$ok $e"
    else
      log "$e: $n failed on its first request (curl exit $rc), recorded without timing"
      failed_result "$new" "${NAME[$e]}" "$(awk "BEGIN {print $t1 - $t0}")" "$rc"
      revive "$e" "$n" "$res"
    fi
  done
  # Prepare and time each engine separately. Clearing QLever's cache must not
  # prevent another engine's timing after QLever crashes.
  for e in $ok; do
    ENGINES=$e
    clear=$prepare
    [ -n "$clear" ] || clear=$(cache_prepare)
    hf=$res/$n.$e.hf.json
    rm -f "$hf"
    rc=0
    ${CLIENT[@]+"${CLIENT[@]}"} hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic --ignore-failure --prepare "$clear" \
      --command-name "${NAME[$e]}" "$(q "${URL[$e]}" "$n")" --export-json "$hf" || rc=$?
    if answering "$e"; then
      # A live server with a broken preparation command is a harness failure.
      [ "$rc" = 0 ] || return "$rc"
      python3 - "$hf" "$new" << 'EOF'
import json, os, sys
hf, new = sys.argv[1:]
d = json.load(open(new)) if os.path.exists(new) else {"results": []}
d["results"] += json.load(open(hf))["results"]
json.dump(d, open(new, "w"), indent=1)
EOF
      rm -f "$hf"
    else
      # --ignore-failure can exit zero after a server dies. Discard all of that
      # engine's timed samples before merging, even when hyperfine succeeded.
      [ "$rc" != 0 ] || rc=1
      log "$e: stopped answering while timing $n, recorded as an engine failure"
      failed_result "$new" "${NAME[$e]}" 0 "$rc" timed
    fi
    revive "$e" "$n" "$res"
  done
  ENGINES=$all
  merge "$new" "$res/$n.json"
}
