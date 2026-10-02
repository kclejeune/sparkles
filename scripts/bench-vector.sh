#!/usr/bin/env bash
# Vector search benchmark: Sparkles' HNSW index against its exact search, over HTTP.
#
#   scripts/bench-vector.sh [N] [DIM] [WORKDIR]
#
# Generates N clustered vectors of dimension DIM (CLUSTERS centres, Gaussian noise, fixed
# SEED) as spk:vector literals of one subject each, plus QUERIES query vectors from the
# same distribution that are not loaded. It loads them with `sparkles load`, then builds
# the index with `sparkles vector create` (timed, with the process's peak memory and the
# index file's size), and serves the store with the result cache off.
#
# For each EFS value and for the exact search (`exact:true`), every query runs once
# through a keep-alive HTTP client, which gives recall@10 against the exact answers and
# the latency distribution. hyperfine then times one top-10 query per mode with curl
# (RUNS runs after WARMUP), which includes curl's own start-up. Results go to
# WORKDIR/<N>x<DIM>/results/ (build.json, recall.json, hyperfine.json) and summary.md.
#
# Env: QUERIES (default 1000), EFS (default "16 32 64 128 256"), K (default 10), RUNS
# (default 20), WARMUP (default 3), M (default 16), EFC (efConstruction, default 128),
# CLUSTERS (default 1000), SEED (default 42), PORT (default 4800), SKIP_LOAD=1 to reuse
# the store and index of an earlier run, SPARKLES (the binary, default
# target/release/sparkles). Measure on a quiet machine: compiles or other servers running
# at the same time inflate every number.
set -euo pipefail

N=${1:-100000}
DIM=${2:-384}
WORK=${3:-target/bench-vector}
QUERIES=${QUERIES:-1000}
EFS=${EFS:-16 32 64 128 256}
K=${K:-10}
RUNS=${RUNS:-20}
WARMUP=${WARMUP:-3}
M=${M:-16}
EFC=${EFC:-128}
CLUSTERS=${CLUSTERS:-1000}
SEED=${SEED:-42}
PORT=${PORT:-4800}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
PRED=urn:x-bench:emb
mkdir -p "$WORK/${N}x${DIM}/results"
WORK=$(cd "$WORK/${N}x${DIM}" && pwd)
cd "$WORK"
echo "== vector benchmark: $N x $DIM in $WORK"

# ---------------------------------------------------------------------------- data
if [ ! -s data.nt ] || [ ! -s queries.jsonl ]; then
  echo "-- generating data"
  python3 - "$N" "$DIM" "$QUERIES" "$SEED" "$CLUSTERS" "$PRED" << 'EOF'
import sys
import numpy as np
n, dim, nq, seed, clusters, pred = int(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5]), sys.argv[6]
rng = np.random.default_rng(seed)
centres = rng.standard_normal((clusters, dim)).astype(np.float32)
def points(count):
    c = centres[rng.integers(0, clusters, count)]
    return (c + 0.6 * rng.standard_normal((count, dim))).astype(np.float32)
fmt = ",".join(["%.5g"] * dim)
with open("data.nt", "w") as f:
    step = 20000
    for lo in range(0, n, step):
        block = points(min(step, n - lo))
        f.write("".join(
            f'<urn:x-bench:v{lo + i}> <{pred}> "[{fmt % tuple(row)}]"^^<urn:x-sparkles:vector> .\n'
            for i, row in enumerate(block)))
with open("queries.jsonl", "w") as f:
    for row in points(nq):
        f.write("[" + fmt % tuple(row) + "]\n")
EOF
  rm -rf db
fi
du -m data.nt | awk '{print "data.nt: " $1 " MB"}'

# ----------------------------------------------------------------- load and build
# run <json-file> <command…>: wall seconds and the peak RSS of the command
run_measured() {
  python3 - "$@" << 'EOF'
import json, resource, subprocess, sys, time
out, cmd = sys.argv[1], sys.argv[2:]
t = time.time()
subprocess.run(cmd, check=True)
wall = time.time() - t
rss = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss / 1024
json.dump({"seconds": round(wall, 2), "peakRssMb": round(rss, 1)}, open(out, "w"))
print(f"   {wall:.1f} s, peak RSS {rss:.0f} MB")
EOF
}
if [ -z "${SKIP_LOAD:-}" ] || [ ! -d db ]; then
  rm -rf db
  echo "-- loading"
  run_measured results/load.json "$SPARKLES" load --loc db data.nt
  echo "-- building the index (M=$M, efConstruction=$EFC)"
  run_measured results/build.json "$SPARKLES" --vector-memory-mb 65536 vector create --loc db \
    --name emb --predicate "$PRED" --dim "$DIM" --m "$M" --ef-construction "$EFC"
  "$SPARKLES" vector status --loc db --name emb > results/status.json
fi

# -------------------------------------------------------------------------- serve
"$SPARKLES" --vector-memory-mb 65536 --result-cache-mb 0 serve --loc bench="$WORK/db" \
  --host 127.0.0.1 --port "$PORT" --timeout 600 > server.log 2>&1 &
SPID=$!
trap 'kill $SPID 2> /dev/null || true; wait $SPID 2> /dev/null || true' EXIT
for _ in $(seq 600); do
  curl -sf "http://127.0.0.1:$PORT/\$/ping" > /dev/null && break
  sleep 0.1
done
# the index is mapped from its file when the store opens
for _ in $(seq 6000); do
  state=$(curl -sf "http://127.0.0.1:$PORT/\$/vector/bench/emb" | python3 -c 'import json,sys; print(json.load(sys.stdin)["state"])' || true)
  [ "$state" = ready ] && break
  sleep 0.1
done
echo "-- index $state"

# ------------------------------------------------- recall and latency (HTTP client)
echo "-- recall@$K and latency over $QUERIES queries"
# shellcheck disable=SC2086 # EFS is a list
python3 - "$PORT" "$PRED" "$K" "$SPID" $EFS << 'EOF'
import http.client, json, sys, time, urllib.parse
port, pred, k, spid, efs = int(sys.argv[1]), sys.argv[2], int(sys.argv[3]), sys.argv[4], [int(x) for x in sys.argv[5:]]
queries = [l.strip() for l in open("queries.jsonl") if l.strip()]
conn = http.client.HTTPConnection("127.0.0.1", port, timeout=600)
def ask(v, opt):
    q = (f'SELECT ?s {{ (?s ?score) <urn:x-sparkles:vectorSearch> '
         f'(<{pred}> "{v}"^^<urn:x-sparkles:vector> {k} "{opt}") }}')
    t = time.perf_counter()
    conn.request("POST", "/bench/sparql", body=q.encode(),
                 headers={"Content-Type": "application/sparql-query", "Accept": "text/tab-separated-values"})
    r = conn.getresponse()
    body = r.read().decode()
    ms = (time.perf_counter() - t) * 1000
    if r.status != 200:
        raise SystemExit(f"{r.status}: {body[:300]}")
    return set(body.split("\n")[1:]) - {""}, ms
def rss_mb():
    for l in open(f"/proc/{spid}/status"):
        if l.startswith("VmRSS:"):
            return int(l.split()[1]) / 1024
def stats(ms):
    ms = sorted(ms)
    return {"meanMs": round(sum(ms) / len(ms), 3), "p50Ms": round(ms[len(ms) // 2], 3),
            "p99Ms": round(ms[min(len(ms) - 1, len(ms) * 99 // 100)], 3)}
for v in queries[:20]:
    ask(v, "exact:true")
truth, ms = zip(*[ask(v, "exact:true") for v in queries])
out = {"queries": len(queries), "k": k, "exact": stats(ms), "hnsw": []}
print(f"   exact        mean {out['exact']['meanMs']:8.2f} ms  p50 {out['exact']['p50Ms']:8.2f}  p99 {out['exact']['p99Ms']:8.2f}")
for ef in efs:
    for v in queries[:20]:
        ask(v, f"ef:{ef}")
    got, ms = zip(*[ask(v, f"ef:{ef}") for v in queries])
    hits = sum(len(g & t) for g, t in zip(got, truth))
    total = sum(len(t) for t in truth)
    row = {"ef": ef, "recall": round(hits / total, 4), **stats(ms)}
    out["hnsw"].append(row)
    print(f"   ef={ef:<4}  recall {row['recall']:.4f}  mean {row['meanMs']:8.2f} ms  p50 {row['p50Ms']:8.2f}  p99 {row['p99Ms']:8.2f}")
out["serverRssMb"] = round(rss_mb(), 1)
json.dump(out, open("results/recall.json", "w"), indent=1)
first = queries[0]
for opt in ["exact:true"] + [f"ef:{ef}" for ef in efs]:
    name = opt.replace(":", "-")
    open(f"q-{name}.rq", "w").write(
        f'SELECT ?s ?score {{ (?s ?score) <urn:x-sparkles:vectorSearch> '
        f'(<{pred}> "{first}"^^<urn:x-sparkles:vector> {k} "{opt}") }}')
EOF

# ------------------------------------------------------------- hyperfine (curl)
echo "-- hyperfine: one top-$K query per mode"
args=()
for f in q-exact-true.rq $(for ef in $EFS; do echo "q-ef-$ef.rq"; done); do
  args+=(-n "${f%.rq}" "curl -sf -o /dev/null -H 'Content-Type: application/sparql-query' -H 'Accept: text/tab-separated-values' --data-binary @$f http://127.0.0.1:$PORT/bench/sparql")
done
hyperfine -N --warmup "$WARMUP" --runs "$RUNS" --export-json results/hyperfine.json "${args[@]}" > /dev/null

# ------------------------------------------------------------------------ summary
python3 - "$N" "$DIM" "$M" "$EFC" > summary.md << 'EOF'
import json, os, sys
n, dim, m, efc = sys.argv[1:5]
b = json.load(open("results/build.json")) if os.path.exists("results/build.json") else {}
st = json.load(open("results/status.json")) if os.path.exists("results/status.json") else {}
r = json.load(open("results/recall.json"))
hf = {x["command"]: x for x in json.load(open("results/hyperfine.json"))["results"]}
mem = st.get("memory", {})
print(f"# Vector search: {n} x {dim}\n")
print(f"Clustered vectors, cosine, HNSW M={m}, efConstruction={efc}, {r['queries']} queries, top {r['k']}.\n")
print("| Build | Peak RSS of the build | Segment | Graph | Index file | Server RSS |")
print("|---|---|---|---|---|---|")
print(f"| {b.get('seconds', '?')} s | {b.get('peakRssMb', '?')} MB | {mem.get('segmentBytes', 0) / 1048576:.0f} MB "
      f"| {mem.get('hnswBytes', 0) / 1048576:.0f} MB | {st.get('files', {}).get('bytes', 0) / 1048576:.0f} MB | {r['serverRssMb']} MB |\n")
print("| Mode | Recall@10 | Client mean | Client p50 | Client p99 | hyperfine (curl) |")
print("|---|---|---|---|---|---|")
e = r["exact"]
h = hf.get("q-exact-true")
fmt = lambda h: f"{h['mean'] * 1000:.2f} ± {h['stddev'] * 1000:.2f} ms" if h else "-"
print(f"| exact | 1.0000 | {e['meanMs']:.2f} ms | {e['p50Ms']:.2f} ms | {e['p99Ms']:.2f} ms | {fmt(h)} |")
for x in r["hnsw"]:
    h = hf.get(f"q-ef-{x['ef']}")
    print(f"| ef={x['ef']} | {x['recall']:.4f} | {x['meanMs']:.2f} ms | {x['p50Ms']:.2f} ms | {x['p99Ms']:.2f} ms | {fmt(h)} |")
EOF
cat summary.md
