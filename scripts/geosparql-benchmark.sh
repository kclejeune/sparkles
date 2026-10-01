#!/usr/bin/env bash
# The GeoSPARQL Compliance Benchmark (Jovanovik, Homburg, Spasić 2021; 206 queries over
# the 30 requirements of GeoSPARQL 1.0) against a scratch Sparkles server.
#
#   SPARKLES_ALLOW_GPL_BENCHMARK=1 scripts/geosparql-benchmark.sh
#
# The benchmark is GPL-2.0-only. It is never part of this repository, its releases or its
# CI: this developer tool clones it at a pinned commit into
# target/geosparql-benchmark/ (git-ignored) when asked to, and only then. It loads the
# benchmark's RDF/XML dataset into a fresh database with the spatial index enabled, runs
# every query over HTTP, and compares each answer with the expected result files (any of a
# query's alternatives): solutions as multisets, numbers within a relative 1e-6, geometry
# literals by their coordinates (scripts/bench-answers.py's normalization). It prints the
# correct answers and the score per requirement, and writes every answer and comparison
# under target/geosparql-benchmark/results/.
#
# Env: SPARKLES (default target/release/sparkles), PORT (default 3942),
# GSB_COMMIT (default the pinned commit below), MAX_TIME (seconds per query, default 60).
set -euo pipefail

if [ "${SPARKLES_ALLOW_GPL_BENCHMARK:-}" != 1 ]; then
  cat >&2 << 'EOF'
The GeoSPARQL Compliance Benchmark is licensed under GPL-2.0-only. This script downloads
it into target/geosparql-benchmark/ for a local run and never adds it to the repository.
Set SPARKLES_ALLOW_GPL_BENCHMARK=1 to proceed.
EOF
  exit 2
fi

ROOT=$(cd "$(dirname "$0")/.." && pwd)
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
PORT=${PORT:-3942}
GSB_URL=https://github.com/OpenLinkSoftware/GeoSPARQLBenchmark
GSB_COMMIT=${GSB_COMMIT:-879e0746e74c1327f74e57366104bcf1a6d63fb1}
WORK=$ROOT/target/geosparql-benchmark
SRC=$WORK/src
RES=$SRC/src/main/resources
mkdir -p "$WORK"

if [ ! -d "$SRC/.git" ]; then
  git clone --quiet --filter=blob:none --no-checkout "$GSB_URL" "$SRC"
fi
git -C "$SRC" fetch --quiet origin "$GSB_COMMIT" 2> /dev/null || true
git -C "$SRC" -c advice.detachedHead=false checkout --quiet "$GSB_COMMIT"

DB=$WORK/db
rm -rf "$DB" "$WORK/server" "$WORK/results"
mkdir -p "$WORK/results"
"$SPARKLES" load --loc "$DB" "$RES/gsb_dataset/dataset.rdf"
"$SPARKLES" geo-index --loc "$DB"

SPID=
stop() {
  [ -n "$SPID" ] && { kill "$SPID" 2> /dev/null || true; }
  true
}
trap stop EXIT
"$SPARKLES" serve --data "$WORK/server" --loc gsb="$DB" --port "$PORT" > "$WORK/server.log" 2>&1 &
SPID=$!
for _ in $(seq 1 600); do
  curl -sf "localhost:$PORT/\$/geo/gsb" > /dev/null 2>&1 && break
  sleep 0.5
done

python3 - "$RES" "localhost:$PORT/gsb/sparql" "$WORK/results" "$ROOT/scripts/bench-answers.py" << 'EOF'
import collections, importlib.util, json, math, os, re, sys, urllib.parse, urllib.request
import xml.etree.ElementTree as ET

res, endpoint, out, answers_py = sys.argv[1:5]
sys.dont_write_bytecode = True  # no __pycache__ next to bench-answers.py
spec = importlib.util.spec_from_file_location("bench_answers", answers_py)
ba = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ba)
SRX = "{http://www.w3.org/2005/sparql-results#}"
NUMERIC = ba.NUMERIC


def norm(t):
    """A term as (kind, value…) with geometry literals normalized."""
    if t is None:
        return ("undef",)
    kind, value = t["type"], t["value"]
    if kind == "uri":
        return ("uri", value)
    if kind == "bnode":
        return ("bnode",)
    dt = t.get("datatype", ba.XSD + "string")
    if t.get("xml:lang"):
        return ("lang", value, t["xml:lang"].lower())
    if dt in (ba.WKT, ba.GEOJSON):
        return ("geom", ba.geometry(value, dt) or value)
    if dt in NUMERIC:
        try:
            return ("num", float(value))
        except ValueError:
            pass
    return ("lit", value, dt)


def same(a, b):
    if a[0] == b[0] == "num":
        x, y = a[1], b[1]
        return x == y or (math.isfinite(x) and math.isfinite(y) and abs(x - y) <= 1e-6 * max(abs(x), abs(y)))
    return a == b


def rows_equal(xs, ys):
    """Multisets of solutions equal, numbers within tolerance (greedy matching)."""
    if len(xs) != len(ys):
        return False
    left = list(ys)
    for x in xs:
        for i, y in enumerate(left):
            if x.keys() == y.keys() and all(same(x[k], y[k]) for k in x):
                del left[i]
                break
        else:
            return False
    return True


def expected(path):
    root = ET.parse(path).getroot()
    b = root.find(f"{SRX}boolean")
    if b is not None:
        return ("ask", b.text.strip() == "true")
    sols = []
    for r in root.iter(f"{SRX}result"):
        sol = {}
        for binding in r.findall(f"{SRX}binding"):
            node = binding[0]
            tag = node.tag.replace(SRX, "")
            t = {"type": {"uri": "uri", "bnode": "bnode", "literal": "literal"}[tag], "value": node.text or ""}
            if node.get("datatype"):
                t["datatype"] = node.get("datatype")
            lang = node.get("{http://www.w3.org/XML/1998/namespace}lang")
            if lang:
                t["xml:lang"] = lang
            sol[binding.get("name")] = norm(t)
        sols.append(sol)
    return ("select", sols)


def actual(query):
    req = urllib.request.Request(
        "http://" + endpoint,
        data=urllib.parse.urlencode({"query": query}).encode(),
        headers={"Accept": "application/sparql-results+json"},
    )
    with urllib.request.urlopen(req, timeout=float(os.environ.get("MAX_TIME", "60"))) as r:
        j = json.load(r)
    if "boolean" in j:
        return ("ask", j["boolean"])
    return ("select", [{k: norm(v) for k, v in b.items()} for b in j["results"]["bindings"]])


qdir, adir = os.path.join(res, "gsb_queries"), os.path.join(res, "gsb_answers")
alternatives = collections.defaultdict(list)
for f in sorted(os.listdir(adir)):
    m = re.match(r"(query-r[0-9-]+?)(-alternative-\d+)?\.srx$", f)
    if m:
        alternatives[m.group(1)].append(os.path.join(adir, f))
per_req = collections.defaultdict(lambda: [0, 0])
report = []
for f in sorted(os.listdir(qdir)):
    # the benchmark's queries (the directory holds a few others)
    m = re.match(r"(query-r(\d+)[0-9-]*)\.rq$", f)
    if not m:
        continue
    name, req = m.group(1), m.group(2)
    try:
        got = actual(open(os.path.join(qdir, f)).read())
        error = None
    except Exception as e:  # noqa: BLE001 - recorded as a failure
        got, error = None, str(e)[:300]
    ok = False
    for exp in alternatives.get(name, []):
        want = expected(exp)
        if got and got[0] == want[0] and (got[1] == want[1] if got[0] == "ask" else rows_equal(got[1], want[1])):
            ok = True
            break
    per_req[req][0] += ok
    per_req[req][1] += 1
    report.append({"query": name, "correct": ok, "error": error,
                   "rows": None if not got or got[0] == "ask" else len(got[1])})
json.dump(report, open(os.path.join(out, "report.json"), "w"), indent=1)
correct = sum(r["correct"] for r in report)
print(f"\ncorrect answers: {correct}/{len(report)}")
print("| requirement | correct | queries |\n|---|---:|---:|")
score = 0.0
for req in sorted(per_req, key=int):
    c, n = per_req[req]
    score += c / n
    print(f"| R{int(req)} | {c} | {n} |")
print(f"\nmean per-requirement score: {100 * score / max(len(per_req), 1):.1f}% over {len(per_req)} requirements")
EOF
