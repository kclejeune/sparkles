#!/usr/bin/env bash
# Times OWL 2 RL materialization on Sparkles and on the reference, Nemo, over LUBM or
# OWL2Bench, after checking that the two closures are equal.
#
#   scripts/rl-reference/bench.sh lubm|owl2bench SCALE WORKDIR
#
# 1. Data. scripts/bench-lubm.sh or scripts/bench-owl2bench.sh generates the data of SCALE
#    universities under WORKDIR/data, with no engine and no materialization. Sparkles loads
#    it into base.db, and the default graph of that database, base.nt, is the input of
#    every run, so both engines read the same terms.
# 2. Correctness. compare.sh compares the closures triple for triple (the aligned and the
#    core comparison of compare.sh). A difference stops the run.
# 3. Full materialization. Each of RUNS rounds times `sparkles infer --full` on a fresh
#    copy of base.db with 1 thread and with THREADS threads, and Nemo's run of the aligned
#    program on base.nt and on base.nt without the change of step 4. Nemo evaluates its
#    rules on one thread and has no thread setting.
# 4. Incremental maintenance. The change is every DELTA_EVERY-th data triple outside the
#    RDF, RDFS and OWL vocabularies (rl_terms.py delta). Each round inserts it into the
#    materialized database without it and deletes it from the materialized database with
#    it, and times `sparkles infer` after each update with THREADS threads. Nemo has no
#    incremental maintenance, so its baseline is its full run on the data after the
#    update. The first round compares each incrementally maintained closure with Nemo's
#    closure of the same data, triple for triple.
#
# Every engine run is timed by GNU time for its wall time and peak RSS, in its own systemd
# scope with MemoryMax=MEM_MAX and no swap where systemd-run exists (SCOPE=0 runs it
# directly), and within RUN_TIMEOUT seconds. A run is recorded as ok, timeout, oom when
# the kernel's out-of-memory killer ended a process of its scope, killed for another
# SIGKILL, or error. The correctness check and the materializations that prepare the
# incremental runs share the cap and the deadline. Nemo's own report adds its import,
# reasoning and export times. WORKDIR/results/rl-reference.json holds every run and
# summary.md the medians.
#
# Env: RUNS (default 3), THREADS (default all cores), DELTA_EVERY (default 100), MEM_MAX
# (default 24G), RUN_TIMEOUT (default 3600), KILL_AFTER (seconds from TERM to KILL at the
# deadline, default 30), SCOPE, SPARKLES (default target/release/sparkles), NMO (default
# the flake's nemo package), PYTHON.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
PYTHON=${PYTHON:-python3}
RUNS=${RUNS:-3}
THREADS=${THREADS:-$(nproc)}
DELTA_EVERY=${DELTA_EVERY:-100}
MEM_MAX=${MEM_MAX:-24G}
RUN_TIMEOUT=${RUN_TIMEOUT:-3600}
INFERRED=urn:x-sparkles:inferred

[ $# -eq 3 ] || {
  sed -n '2,37p' "$0" >&2
  exit 2
}
suite=$1 scale=$2
case $suite in lubm | owl2bench) ;; *)
  echo "the suite must be lubm or owl2bench" >&2
  exit 2
  ;;
esac
WORK=$(realpath -m "$3")
mkdir -p "$WORK/results" "$WORK/logs"
die() {
  echo "bench.sh: $*" >&2
  exit 1
}
log() { echo "[$(date +%T)] $*" >&2; }

# capped NAME [-t TIMEFILE] CMD...: CMD in a scope of its own with MemoryMax=MEM_MAX and
# no swap, within RUN_TIMEOUT seconds, with its output in logs/NAME.log. With -t, GNU time
# reports on the timeout supervisor and CMD to TIMEFILE, so that timeout signals CMD
# itself and waits for it. It sets CAPPED_STATUS to ok, timeout, oom (the kernel's
# out-of-memory killer ended a process of the scope, which the scope's memory.events,
# copied to logs/NAME.events, records), killed (another SIGKILL) or error (any other
# failure, including a scope that did not start), and returns 0 only for ok. A small shell in the
# scope runs CMD, copies memory.events when it ends and kills whatever is left in the
# scope, so no process outlives the run.
capped() {
  local name=$1 rc=0 t0 t1 ooms=0
  shift
  local timer=()
  if [ "${1:-}" = -t ]; then
    timer=("$GTIME" -v -o "$2")
    shift 2
  fi
  local scope=()
  rm -f "logs/$name.events"
  if [ "${SCOPE:-1}" != 0 ] && command -v systemd-run > /dev/null; then
    # shellcheck disable=SC2016 # the inner shell expands these
    scope=(systemd-run --user --scope -q --expand-environment=no -p "MemoryMax=$MEM_MAX" -p MemorySwapMax=0
      -p OOMPolicy=continue sh -c 'cg=/sys/fs/cgroup$(sed -n "s/^0:://p" /proc/self/cgroup); ev=$1; shift
"$@"; rc=$?; cat "$cg/memory.events" > "$ev" 2> /dev/null
while read -r p; do [ "$p" = $$ ] || kill -9 "$p" 2> /dev/null; done < "$cg/cgroup.procs"
exit $rc' capped "logs/$name.events")
  fi
  t0=$(date +%s.%N)
  "${scope[@]}" "${timer[@]}" timeout -k "${KILL_AFTER:-30}" "$RUN_TIMEOUT" "$@" > "logs/$name.log" 2>&1 || rc=$?
  t1=$(date +%s.%N)
  ooms=$(sed -n 's/^oom_kill \([0-9]*\)$/\1/p' "logs/$name.events" 2> /dev/null || true)
  if [ "$rc" = 0 ]; then
    CAPPED_STATUS=ok
  elif [ "${ooms:-0}" -gt 0 ]; then
    CAPPED_STATUS=oom
  elif [ "$rc" = 124 ] || awk "BEGIN {exit !($t1 - $t0 >= $RUN_TIMEOUT)}"; then
    CAPPED_STATUS=timeout
  elif [ "$rc" = 137 ]; then
    CAPPED_STATUS=killed
  else
    CAPPED_STATUS=error
  fi
  [ "$CAPPED_STATUS" = ok ]
}

# run NAME ROUND THREADS CMD...: one timed engine run, appended to the record. GNU time
# inside the scope gives the wall time and the peak RSS of the run, whose largest process
# is the engine.
run() {
  local name=$1 round=$2 threads=$3 rss wall
  shift 3
  rm -f "logs/$name.time"
  capped "$name" -t "logs/$name.time" "$@" || true
  LAST_STATUS=$CAPPED_STATUS
  rss=$(sed -n 's/.*Maximum resident set size (kbytes): \([0-9]*\)/\1/p' "logs/$name.time" 2> /dev/null || true)
  wall=$(sed -n 's/.*Elapsed (wall clock) time (h:mm:ss or m:ss): \(.*\)/\1/p' "logs/$name.time" 2> /dev/null || true)
  "$PYTHON" -I - "$RECORD" "$name" "$round" "$threads" "$LAST_STATUS" "${wall:-}" "${rss:-}" "logs/$name.log" << 'EOF'
import json, re, sys
rec, name, rnd, threads, st, wall, rss, logf = sys.argv[1:]
text = open(logf, errors="replace").read()
secs = None
if wall:
    parts = [float(x) for x in wall.split(":")]
    secs = sum(v * 60 ** i for i, v in enumerate(reversed(parts)))
entry = {"run": re.sub(r"-t[0-9]+$", "", name.rsplit("-r", 1)[0]), "round": int(rnd), "threads": int(threads), "status": st,
         "seconds": secs, "max_rss_kib": int(rss) if rss else None}
# Nemo's short report: Data import, Reasoning and Data export, in milliseconds
for key, label in (("import", "Data import"), ("reasoning", "Reasoning"), ("export", "Data export")):
    m = re.search(rf"{label}:\s+([0-9]+)ms", text)
    if m:
        entry[f"nemo_{key}_seconds"] = int(m.group(1)) / 1000
m = re.search(r"^([0-9]+) inferred triples", text, re.M)
if m:
    entry["derived_triples"] = int(m.group(1))
with open(rec, "a") as f:
    f.write(json.dumps(entry) + "\n")
print(f"{entry['run']} round {rnd}, {threads} threads: {st}, {secs if secs is None else round(secs, 2)} s, peak RSS {int(rss) >> 10 if rss else '?'} MiB")
EOF
  return 0
}

# shellcheck source=scripts/bench-tools.sh
. "$ROOT/scripts/bench-tools.sh"
SPARKLES=$(stage_binary "${SPARKLES:-$ROOT/target/release/sparkles}")
[ -x "$SPARKLES" ] || die "no Sparkles binary at $SPARKLES; build it or set SPARKLES"
NMO=${NMO:-$(nixexe nemo nmo)} || die "cannot build the nemo package"
GTIME=$(gnu_time)
export SPARKLES NMO

cd "$WORK"
RECORD=results/runs.jsonl
LAST_STATUS=ok

# ------------------------------------------------------------------------------- data
if [ ! -f data/base.nt ]; then
  log "generating $suite($scale)"
  SPARKLES="$SPARKLES" ENGINES="" INFER_PROFILES=" " "$ROOT/scripts/bench-$suite.sh" "$scale" "$WORK/data" > logs/generate.log 2>&1 ||
    die "generating the data failed, see logs/generate.log"
fi
if [ ! -f base.nt ]; then
  rm -rf base.db
  "$SPARKLES" load --loc base.db data/base.nt > logs/load.log 2>&1 || die "loading failed, see logs/load.log"
  "$SPARKLES" dump --loc base.db --graph default --format nt --out base.nt > logs/dump-base.log 2>&1 ||
    die "dumping the base failed, see logs/dump-base.log"
fi
"$PYTHON" -I "$HERE/rl_terms.py" delta base.nt "$DELTA_EVERY" delta.nt minus.nt insert.ru delete.ru 2> logs/delta.log
mkdir -p enc-full enc-minus
"$PYTHON" -I "$HERE/rl_terms.py" encode base.nt enc-full 2> logs/encode-full.log
"$PYTHON" -I "$HERE/rl_terms.py" encode minus.nt enc-minus 2> logs/encode-minus.log
grep -v '% reflexive-scm-eq$' "$HERE/owl2rl.rls" | cat - "$HERE/sparkles-extensions.rls" > aligned.rls

# ------------------------------------------------------------------------ correctness
log "comparing the closures"
capped compare "$HERE/compare.sh" base.nt compare || die "the closures differ or the comparison failed, see logs/compare.log"

nemo_run() {
  local name=$1 round=$2 enc=$3
  rm -rf "nemo-$name"
  run "nemo-$name-r$round" "$round" 1 "$NMO" --overwrite-results --import-dir "$enc" --export-dir "nemo-$name" --report short aligned.rls
}

# check_closure DB NEMO_DIR ENC NAME: the derived graph of DB equals Nemo's of ENC. A
# maintenance run that did not finish leaves nothing to compare, which the record shows.
check_closure() {
  local db=$1 out=$2 enc=$3 name=$4
  if [ "$LAST_STATUS" != ok ]; then
    log "the $name run ended with $LAST_STATUS, so its closure is not compared"
    return 0
  fi
  if [ ! -f "$out/derived.nt" ]; then
    log "Nemo's run for the $name check did not finish, so the closure is not compared"
    return 0
  fi
  "$SPARKLES" dump --loc "$db" --format nq --out "check-$name.nq" > "logs/check-$name.log" 2>&1 || die "dumping $db failed"
  "$PYTHON" -I "$HERE/rl_terms.py" split "check-$name.nq" "check-$name-base.nt" "check-$name-sparkles.nt" "$INFERRED" 2>> "logs/check-$name.log"
  "$PYTHON" -I "$HERE/rl_terms.py" decode "$enc" "$out/derived.nt" "check-$name-reference.nt" 2>> "logs/check-$name.log"
  "$PYTHON" -I "$HERE/rl_terms.py" diff "check-$name-sparkles.nt" "check-$name-reference.nt" "results/check-$name.json" \
    --known "$HERE/known.tsv" 2>> "logs/check-$name.log" || die "the closure after $name differs from the reference's, see results/check-$name.json"
  rm -f "check-$name.nq" "check-$name-base.nt"
  log "the closure after $name equals the reference's"
}

# ------------------------------------------------------------------------- preparation
rm -f "$RECORD"
log "materializing the databases for the incremental runs"
rm -rf full.db minus.db
cp -a --reflink=auto base.db full.db
capped prepare-full "$SPARKLES" infer --loc full.db --profile owl-rl --full || die "materializing full.db ended with $CAPPED_STATUS, see logs/prepare-full.log"
cp -a --reflink=auto base.db minus.db
"$SPARKLES" update --loc minus.db --update delete.ru > logs/prepare-minus-update.log 2>&1 || die "deleting the change failed"
capped prepare-minus "$SPARKLES" infer --loc minus.db --profile owl-rl --full || die "materializing minus.db ended with $CAPPED_STATUS, see logs/prepare-minus.log"

# ------------------------------------------------------------------------------ rounds
for round in $(seq 1 "$RUNS"); do
  for t in 1 "$THREADS"; do
    rm -rf run.db
    cp -a --reflink=auto base.db run.db
    sync
    run "sparkles-full-t$t-r$round" "$round" "$t" "$SPARKLES" infer --loc run.db --profile owl-rl --full --threads "$t"
  done
  nemo_run full "$round" enc-full
  nemo_run minus "$round" enc-minus

  rm -rf run.db
  cp -a --reflink=auto minus.db run.db
  "$SPARKLES" update --loc run.db --update insert.ru > "logs/insert-update-r$round.log" 2>&1 || die "inserting the change failed"
  sync
  run "sparkles-insert-r$round" "$round" "$THREADS" "$SPARKLES" infer --loc run.db --profile owl-rl --threads "$THREADS"
  [ "$round" != 1 ] || check_closure run.db nemo-full enc-full insert

  rm -rf run.db
  cp -a --reflink=auto full.db run.db
  "$SPARKLES" update --loc run.db --update delete.ru > "logs/delete-update-r$round.log" 2>&1 || die "deleting the change failed"
  sync
  run "sparkles-delete-r$round" "$round" "$THREADS" "$SPARKLES" infer --loc run.db --profile owl-rl --threads "$THREADS"
  [ "$round" != 1 ] || check_closure run.db nemo-minus enc-minus delete
done
rm -rf run.db nemo-full nemo-minus

# ----------------------------------------------------------------------------- results
"$PYTHON" -I - "$RECORD" results "$suite" "$scale" "$THREADS" "$SPARKLES" "$NMO" << 'EOF'
import json, os, platform, statistics, subprocess, sys
rec, out, suite, scale, threads, sparkles, nmo = sys.argv[1:]
runs = [json.loads(line) for line in open(rec)]
count = lambda f: sum(1 for _ in open(f))
compare = json.load(open("compare/diff.json"))
version = subprocess.run([nmo, "--version"], capture_output=True, text=True).stdout.strip()
doc = {
    "suite": suite, "scale": int(scale), "host": platform.node(), "cpus": os.cpu_count(),
    "threads": int(threads), "sparkles": sparkles, "nemo": version,
    "base_triples": count("base.nt"), "change_triples": count("delta.nt"),
    "derived_triples": compare["sparkles"],
    "correctness": {"aligned": compare, "core": json.load(open("compare/core.json"))["differences"]["reference"]["total"]},
    "runs": runs,
}
json.dump(doc, open(f"{out}/rl-reference.json", "w"), indent=1)
groups = {}
for r in runs:
    groups.setdefault((r["run"], r["threads"]), []).append(r)
rows = ["| Run | Threads | Status | Median seconds | Nemo reasoning seconds | Peak RSS (MiB) |", "|---|---:|---|---:|---:|---:|"]
for (name, t), rs in groups.items():
    ok = [r for r in rs if r["status"] == "ok"]
    st = "ok" if len(ok) == len(rs) else ", ".join(sorted({r["status"] for r in rs}))
    med = f"{statistics.median(r['seconds'] for r in ok):.2f}" if ok else ""
    reas = [r["nemo_reasoning_seconds"] for r in ok if "nemo_reasoning_seconds" in r]
    rss = [r["max_rss_kib"] for r in ok if r.get("max_rss_kib")]
    rows.append(f"| {name} | {t} | {st} | {med} | {statistics.median(reas):.2f} | {max(rss) >> 10} |" if reas else
                f"| {name} | {t} | {st} | {med} | | {max(rss) >> 10 if rss else ''} |")
with open(f"{out}/summary.md", "w") as f:
    f.write(f"# OWL 2 RL materialization, {suite}({scale})\n\n")
    f.write(f"{doc['base_triples']} base triples, {doc['derived_triples']} derived triples, equal on both engines. "
            f"The change of the incremental runs has {doc['change_triples']} triples. {version} on {doc['host']}.\n\n")
    f.write("\n".join(rows) + "\n")
print(open(f"{out}/summary.md").read())
EOF
