#!/usr/bin/env bash
# GeoSPARQL benchmark: Sparkles with its spatial index against Sparkles without it, on
# the synthetic dataset of scripts/gen-geo.py, via hyperfine.
#
#   scripts/bench-geo.sh [N] [WORKDIR]
#
# Steps: generate N point features (plus lines, polygons and the administrative
# hierarchy) and the queries over them; load; time the index build (`sparkles geo-index`);
# serve the database with the index and time Q1-Q4, Q8 and Q9; serve it again without the
# index (disabled) and time the same queries (Q10 is Q1 there). Before timing, both
# servers' answers to every query are fingerprinted with scripts/bench-answers.py
# (geometries compared by coordinates): the index must not change any answer. Results
# go to WORKDIR/results/geo-*.json and WORKDIR/results/geo-summary.md.
#
# Env: WARMUP (default 2), RUNS (default 10), ADMIN (administrative levels, default 3),
# SKIP_LOAD=1 to reuse the database, QUERIES="geo-q1-within …" to run a subset,
# PORT (default 3941), SPARKLES (default target/release/sparkles), SPARKLES_ARGS
# (extra `serve` flags), MAX_TIME (seconds per request, default 300).
set -euo pipefail

N=${1:-100000}
WORK=${2:-/tmp/sparkles-geo-bench}
WARMUP=${WARMUP:-2}
RUNS=${RUNS:-10}
ADMIN=${ADMIN:-3}
PORT=${PORT:-3941}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
SPARKLES_ARGS=${SPARKLES_ARGS:-}
mkdir -p "$WORK/results" "$WORK/queries"
WORK=$(cd "$WORK" && pwd)
cd "$WORK"
DB=$WORK/geo.db

if [ ! -f geo.nt ]; then
  echo "generating dataset ($N points, $ADMIN administrative levels)…"
  python3 "$ROOT/scripts/gen-geo.py" "$N" --admin "$ADMIN" --queries queries > geo.nt
fi
echo "dataset: $(wc -l < geo.nt) triples"

# merge <new.json> <dest.json>: replace/add hyperfine results by command name
merge() {
  python3 - "$1" "$2" << 'EOF'
import json, os, sys
new, dest = sys.argv[1], sys.argv[2]
n = json.load(open(new))
old = json.load(open(dest)) if os.path.exists(dest) else {"results": []}
cmds = {r["command"] for r in n["results"]}
old["results"] = [r for r in old["results"] if r["command"] not in cmds] + n["results"]
json.dump(old, open(dest, "w"), indent=1)
EOF
  rm -f "$1"
}

if [ -z "${SKIP_LOAD:-}" ]; then
  rm -rf "$DB"
  hyperfine --runs 1 --style basic --command-name load "$SPARKLES load --loc $DB geo.nt" \
    --export-json results/geo-load.new.json
  merge results/geo-load.new.json results/geo-load.json
  # the index build, from a disabled index each time
  hyperfine --runs 1 --style basic --prepare "$SPARKLES geo-index --loc $DB --disable" \
    --command-name build "$SPARKLES geo-index --loc $DB" --export-json results/geo-build.new.json
  merge results/geo-build.new.json results/geo-build.json
fi
"$SPARKLES" geo-index --loc "$DB" > /dev/null 2>&1
"$SPARKLES" geo-index --loc "$DB" --status > results/geo-status.json

SPID=
stop() {
  [ -n "$SPID" ] && { kill "$SPID" 2> /dev/null || true; }
  pid=$(ss -ltnp 2> /dev/null | grep ":$PORT " | sed -n 's/.*pid=\([0-9]*\).*/\1/p' | head -1 || true)
  [ -n "$pid" ] && { kill "$pid" 2> /dev/null || true; }
  for _ in $(seq 1 100); do
    ss -ltn 2> /dev/null | grep -q ":$PORT " || break
    sleep 0.1
  done
  SPID=
}
trap stop EXIT
serve() { # serve <log name> <flags…>: the database on $PORT
  local log=$1
  shift
  # shellcheck disable=SC2086 # SPARKLES_ARGS is a list of flags
  "$SPARKLES" --result-cache-mb 0 serve --data server --loc bench="$DB" --port "$PORT" --timeout 600 "$@" $SPARKLES_ARGS > "$log.log" 2>&1 &
  SPID=$!
  for _ in $(seq 1 600); do
    if curl -sf "localhost:$PORT/\$/geo/bench" > /dev/null 2>&1; then return 0; fi
    sleep 0.5
  done
  echo "timeout waiting for the server" >&2
  exit 1
}

mapfile -t NAMES < <(find queries -name '*.rq' -printf '%f\n' | sed 's|\.rq$||' | sort)
selected() { [ -z "${QUERIES:-}" ] || [[ " $QUERIES " == *" $1 "* ]]; }
q() { echo "curl -sf --max-time ${MAX_TIME:-300} -o /dev/null -H 'Accept: text/tab-separated-values' --data-urlencode query@queries/$1.rq localhost:$PORT/bench/sparql"; }

# run <engine>: the answer check and the timings against the running server
run() {
  local engine=$1
  for n in "${NAMES[@]}"; do
    selected "$n" || continue
    r=$(python3 "$ROOT/scripts/bench-answers.py" "localhost:$PORT/bench/sparql" "queries/$n.rq" results/geo-answers.json "$n" "$engine")
    printf '%-20s %-14s %s rows\n' "$n" "$engine" "$r"
  done
  for n in "${NAMES[@]}"; do
    selected "$n" || continue
    hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic --ignore-failure \
      --command-name "$engine" "$(q "$n")" --export-json "results/$n.new.json"
    merge "results/$n.new.json" "results/$n.json"
  done
}

# with the index
serve sparkles --geo bench
run sparkles
stop
# without it (the spatial index disabled, then enabled again for the next run)
"$SPARKLES" geo-index --loc "$DB" --disable > /dev/null 2>&1
serve sparkles-scan
run sparkles-scan
stop
"$SPARKLES" geo-index --loc "$DB" > /dev/null 2>&1

python3 - "$WORK/results" "${NAMES[@]}" << 'EOF'
import json, os, sys
d, names = sys.argv[1], sys.argv[2:]
def load(f):
    return {r["command"]: r for r in json.load(open(f))["results"]} if os.path.exists(f) else {}
answers = json.load(open(f"{d}/geo-answers.json")) if os.path.exists(f"{d}/geo-answers.json") else {}
def ms(r):
    if any(e != 0 for e in r.get("exit_codes", [])):
        return "error"
    return f"{r['mean'] * 1000:.1f} ± {(r['stddev'] or 0) * 1000:.1f}"
out = ["| query | rows | with the index (ms) | without (ms) | speedup | same answer |",
       "|---|---:|---:|---:|---:|:-:|"]
for n in names:
    rs, an = load(f"{d}/{n}.json"), answers.get(n, {})
    a, b = rs.get("sparkles"), rs.get("sparkles-scan")
    x, y = an.get("sparkles", {}), an.get("sparkles-scan", {})
    same = "yes" if x and x.get("value") == y.get("value") and x.get("rows") != "error" else "**no**"
    speed = f"{b['mean'] / a['mean']:.1f}×" if a and b and ms(a) != "error" and ms(b) != "error" else "—"
    out.append(f"| {n} | {x.get('rows', '—')} | {ms(a) if a else '—'} | {ms(b) if b else '—'} | {speed} | {same} |")
for f, label in (("geo-load", "load"), ("geo-build", "index build")):
    r = load(f"{d}/{f}.json")
    for v in r.values():
        out.append(f"\n{label}: {v['mean']:.2f} s")
if os.path.exists(f"{d}/geo-status.json"):
    s = json.load(open(f"{d}/geo-status.json"))
    if s.get("enabled"):
        m = s["memory"]
        out.append(f"\nindex: {s['rows']['base']} rows, {s['literals']} literals, "
                   f"{(m['treeBytes'] + m['geometryBytes']) / 2**20:.1f} MiB (tree {m['treeBytes'] / 2**20:.1f}, geometries {m['geometryBytes'] / 2**20:.1f})")
open(f"{d}/geo-summary.md", "w").write("\n".join(out) + "\n")
print("\n".join(out))
EOF
