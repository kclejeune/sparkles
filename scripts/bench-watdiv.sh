#!/usr/bin/env bash
# WatDiv basic-testing benchmark: Sparkles vs Apache Jena (TDB2 + Fuseki) vs QLever vs
# Fluree vs Oxigraph on the Waterloo SPARQL Diversity Test Suite, via hyperfine.
#
#   scripts/bench-watdiv.sh [SCALE] [WORKDIR]
#
# WatDiv (https://dsg.uwaterloo.ca/watdiv/) generates an e-commerce dataset whose entities
# are unevenly structured, and its 20 basic query templates cover linear (L1-L5), star
# (S1-S7), snowflake (F1-F5) and complex (C1-C3) shapes. Scale factor 1 is about 112k
# triples, and the default 100 about 11.0M, of which 10.9M are distinct.
#
# The generator is WatDiv v0.6, built from its checksum-pinned source release with
# scripts/bench-watdiv/watdiv.nix against the nixpkgs revision in flake.lock and cached in
# WORKDIR/watdiv-gen. The build patches the release so that its output depends only on
# its inputs: WATDIV_SEED seeds every random generator and the word list is pinned. The
# dataset (WORKDIR/data.nt) and INSTANCES query instances per template
# (WORKDIR/queries/<template>-<i>.rq) are then generated once and reused. The generator
# often draws the same instance twice, so the script keeps the first INSTANCES distinct
# ones out of 4 x INSTANCES draws. The templates without placeholders (C1-C3) have one.
#
# Every engine loads and serves the data exactly as in scripts/bench.sh: over HTTP with
# TSV results, Sparkles with --result-cache-mb 0, QLever with -e 0B and its cache cleared
# before every timed run, Fuseki with -Xmx8G, Fluree's release binary with the same
# environment, and Oxigraph's bulk load followed by `optimize`. Before timing, every
# engine's answer to every instance is fingerprinted (scripts/bench-answers.py). Then each
# instance is timed with hyperfine. scripts/bench-watdiv/summary.py writes
# WORKDIR/results/summary.md with the geometric mean over instances per template, the
# geometric mean per category and over all templates, and WORKDIR/results/instances.md
# with every instance. An engine whose answer to an instance differs from the majority is
# footnoted and not ranked for that template.
#
# Env: WARMUP (default 2), RUNS (default 10), INSTANCES (default 5), WATDIV_SEED (default
# 1), SKIP_LOAD=1 to reuse existing indexes, ANSWERS_ONLY=1 to re-check answers and rebuild
# the summary without timing anything, ENGINES="sparkles jena qlever fluree oxigraph"
# (default) to run a subset (results merge per engine as in scripts/bench.sh; ENGINES=""
# only generates the data and the queries),
# QUERIES="L1 S3-2 …" to limit the check and timings to those templates or instances,
# PORT_BASE (default 3940; the engines listen on PORT_BASE+1 to PORT_BASE+5), MAX_TIME
# (seconds per request, default 300), WATDIV (a WatDiv prefix with bin/watdiv and
# share/watdiv, instead of the nix build), SPARKLES (the binary, default
# target/release/sparkles), FLUREE and FLUREE_VERSION as in scripts/bench.sh.
set -euo pipefail
# Servers run inside a transient systemd scope limited to SERVER_MEM_MAX (for example 12G)
# when it is set, so an engine that runs out of memory is killed alone (as in bench.sh).
CAP=()
if [ -n "${SERVER_MEM_MAX:-}" ]; then
  command -v systemd-run > /dev/null || {
    echo "SERVER_MEM_MAX needs systemd-run" >&2
    exit 1
  }
  CAP=(systemd-run --user --scope --quiet -p "MemoryMax=$SERVER_MEM_MAX" -p MemorySwapMax=0 --)
fi

SCALE=${1:-100}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${2:-$ROOT/target/bench-watdiv}
WARMUP=${WARMUP:-2}
RUNS=${RUNS:-10}
INSTANCES=${INSTANCES:-5}
WATDIV_SEED=${WATDIV_SEED:-1}
PORT_BASE=${PORT_BASE:-3940}
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
export WATDIV_SEED
mkdir -p "$WORK/results" "$WORK/queries"
WORK=$(cd "$WORK" && pwd)
cd "$WORK"

TEMPLATES=(L1 L2 L3 L4 L5 S1 S2 S3 S4 S5 S6 S7 F1 F2 F3 F4 F5 C1 C2 C3)

log() { echo "[$(date +%H:%M:%S)] $*" >&2; }
die() {
  log "error: $*"
  exit 1
}
nixbin() { # nixbin <pkg> <bin>
  if command -v "$2" > /dev/null; then command -v "$2"; else echo "$(nix build "nixpkgs#$1" --no-link --print-out-paths | tail -1)/bin/$2"; fi
}
ENGINES=${ENGINES-sparkles jena qlever fluree oxigraph}
has() { [[ " $ENGINES " == *" $1 "* ]]; }

# ---------------------------------------------------------------------------- generator
if [ -z "${WATDIV:-}" ]; then
  WATDIV=$WORK/watdiv-gen
  if [ ! -x "$WATDIV/bin/watdiv" ]; then
    log "building the WatDiv generator (scripts/bench-watdiv/watdiv.nix)"
    # nixpkgs at the revision locked in flake.lock, so the compiler and Boost are pinned too
    nix build --impure --out-link "$WATDIV" --expr "
      let
        n = (builtins.fromJSON (builtins.readFile $ROOT/flake.lock)).nodes.nixpkgs.locked;
        pkgs = import (builtins.fetchTarball {
          url = \"https://github.com/\${n.owner}/\${n.repo}/archive/\${n.rev}.tar.gz\";
          sha256 = n.narHash;
        }) { };
      in import $ROOT/scripts/bench-watdiv/watdiv.nix { inherit pkgs; }"
  fi
fi
MODEL=$WATDIV/share/watdiv/model/wsdbm-data-model.txt

# ---------------------------------------------------------------------------- dataset
# the generator writes saved.txt next to the data, and query instantiation reads it back
PARAMS="scale=$SCALE seed=$WATDIV_SEED"
if [ -f gen/params ] && [ "$(cat gen/params)" != "$PARAMS" ]; then
  die "$WORK holds a dataset generated with $(cat gen/params), not $PARAMS; use another workdir"
fi
if [ ! -f data.nt ] || [ ! -f gen/saved.txt ]; then
  log "generating the WatDiv dataset ($PARAMS)"
  mkdir -p gen
  (cd gen && "$WATDIV/bin/watdiv" -d "$MODEL" "$SCALE" > ../data.nt.tmp)
  mv data.nt.tmp data.nt
  echo "$PARAMS" > gen/params
fi
log "dataset: $(wc -l < data.nt) triples"

# query instances: the first INSTANCES distinct instances of each template, out of 4 x
# INSTANCES drawn by the generator (its draws repeat often), one file each
QPARAMS="$PARAMS instances=$INSTANCES draws=$((4 * INSTANCES))"
if [ ! -f queries/instances ] || [ "$(head -1 queries/instances)" != "$QPARAMS" ]; then
  log "instantiating $INSTANCES queries per template"
  # answers and timings of the previous instances no longer apply
  rm -f queries/*.rq queries/instances results/answers.json results/[LSFC][0-9]-*.json
  for t in "${TEMPLATES[@]}"; do
    (cd gen && "$WATDIV/bin/watdiv" -q "$MODEL" "$WATDIV/share/watdiv/testsuite/$t.txt" $((4 * INSTANCES)) 1) > "gen/$t.txt"
  done
  python3 - "$QPARAMS" "$INSTANCES" "${TEMPLATES[@]}" > queries/instances.tmp << 'EOF'
import sys
print(sys.argv[1])
k = int(sys.argv[2])
for t in sys.argv[3:]:
    seen = []
    for q in open(f"gen/{t}.txt").read().split("\n\n"):
        q = q.strip()
        if q and q not in seen:
            seen.append(q)
    if not seen:
        sys.exit(f"WatDiv produced no instances of {t}")
    for i, q in enumerate(seen[:k], 1):
        open(f"queries/{t}-{i}.rq", "w").write(q + "\n")
        print(f"{t}-{i}")
EOF
  mv queries/instances.tmp queries/instances
fi
mapfile -t NAMES < <(tail -n +2 queries/instances)
if [ -z "${ENGINES// /}" ]; then
  log "no engines selected: the data and the queries are ready in $WORK"
  exit 0
fi
selected() { # selected <instance>: QUERIES lists templates or instances
  [ -z "${QUERIES:-}" ] || [[ " $QUERIES " == *" $1 "* ]] || [[ " $QUERIES " == *" ${1%-*} "* ]]
}

# ---------------------------------------------------------------------------- engines
if has jena; then
  TDBLOADER=$(nixbin apache-jena tdb2.tdbloader)
  FUSEKI=$(nixbin apache-jena-fuseki fuseki-server)
fi
if has qlever; then
  QINDEX=$(nixbin qlever qlever-index)
  QSERVER=$(nixbin qlever qlever-server)
fi
if has oxigraph; then OXIGRAPH=$(nixbin oxigraph oxigraph); fi

FLUREE_VERSION=${FLUREE_VERSION:-4.2.2}
if has fluree && [ -z "${FLUREE:-}" ]; then
  case "$(uname -sm)" in
    "Linux x86_64") FT=x86_64-unknown-linux-gnu ;;
    "Linux aarch64") FT=aarch64-unknown-linux-gnu ;;
    "Darwin arm64") FT=aarch64-apple-darwin ;;
    "Darwin x86_64") FT=x86_64-apple-darwin ;;
    *) die "no Fluree release binary for $(uname -sm); set FLUREE" ;;
  esac
  FDIR="fluree-$FLUREE_VERSION"
  FLUREE="$WORK/$FDIR/fluree-db-cli-$FT/fluree"
  if [ ! -x "$FLUREE" ]; then
    mkdir -p "$FDIR"
    FURL="https://github.com/fluree/db/releases/download/v$FLUREE_VERSION/fluree-db-cli-$FT.tar.xz"
    curl -sfL "$FURL" -o "$FDIR/fluree.tar.xz"
    want=$(curl -sfL "$FURL.sha256" | cut -d' ' -f1)
    got=$(sha256sum "$FDIR/fluree.tar.xz" 2> /dev/null || shasum -a 256 "$FDIR/fluree.tar.xz")
    got=${got%% *}
    [ "$want" = "$got" ] || die "Fluree checksum mismatch ($got != $want)"
    tar xJf "$FDIR/fluree.tar.xz" -C "$FDIR"
  fi
fi

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

# ----------------------------------------------------------------------------- load
# the same commands as scripts/bench.sh
if [ -z "${SKIP_LOAD:-}" ]; then
  LOAD=()
  if has sparkles; then LOAD+=(--prepare 'rm -rf sparkles.db' --command-name sparkles "$SPARKLES load --loc sparkles.db data.nt"); fi
  if has jena; then LOAD+=(--prepare 'rm -rf jena.db' --command-name jena-tdb2 "$TDBLOADER --loc jena.db data.nt"); fi
  if has qlever; then
    mkdir -p qlever-index
    echo '{"num-triples-per-batch": 1000000}' > qlever-index/settings.json
    LOAD+=(--prepare 'rm -f qlever-index/bench.*' --command-name qlever "cd qlever-index && $QINDEX -i bench -F nt -f ../data.nt -p true -s settings.json")
  fi
  if has fluree; then LOAD+=(--prepare 'rm -rf fluree' --command-name fluree "mkdir -p fluree && cd fluree && $FLUREE init -q && $FLUREE --memory-budget-mb 8192 create bench --from ../data.nt --chunk-size-mb 16"); fi
  if has oxigraph; then LOAD+=(--prepare 'rm -rf oxigraph.db' --command-name oxigraph "$OXIGRAPH load --location oxigraph.db --file data.nt && $OXIGRAPH optimize --location oxigraph.db"); fi
  hyperfine --runs 1 --style basic "${LOAD[@]}" --export-json results/load.new.json
  merge results/load.new.json results/load.json
fi

# ---------------------------------------------------------------------------- servers
SPORT=$((PORT_BASE + 1))
QPORT=$((PORT_BASE + 2))
JPORT=$((PORT_BASE + 3))
FPORT=$((PORT_BASE + 4))
OPORT=$((PORT_BASE + 5))
PIDS=()
declare -A NAME URL PORT
# on exit, also stop whatever listens on a selected engine's port: `fuseki-server` is a
# wrapper script whose JVM outlives it
stop_all() {
  [ ${#PIDS[@]} -gt 0 ] && { kill "${PIDS[@]}" 2> /dev/null || true; }
  for p in "${PORT[@]}"; do
    pid=$(ss -ltnp 2> /dev/null | grep ":$p " | sed -n 's/.*pid=\([0-9]*\).*/\1/p' | head -1 || true)
    [ -n "$pid" ] && { kill "$pid" 2> /dev/null || true; }
  done
  true
}
trap stop_all EXIT
wait_for() {
  for _ in $(seq 1 240); do
    curl -sf "$1" > /dev/null 2>&1 && return 0
    sleep 0.5
  done
  die "timeout waiting for $1"
}
for p in $SPORT $QPORT $JPORT $FPORT $OPORT; do
  if ss -ltn 2> /dev/null | grep -q ":$p "; then die "port $p is in use (set PORT_BASE)"; fi
done
if has sparkles; then
  "${CAP[@]}" "$SPARKLES" --result-cache-mb 0 serve --data sparkles-server --loc bench="$WORK/sparkles.db" --port $SPORT --timeout 600 > sparkles.log 2>&1 &
  PIDS+=($!)
  PORT[sparkles]=$SPORT
  wait_for "localhost:$SPORT/\$/ping"
  NAME[sparkles]=sparkles
  URL[sparkles]=localhost:$SPORT/bench/sparql
fi
if has jena; then
  JVM_ARGS="-Xmx8G" "${CAP[@]}" "$FUSEKI" --update --port $JPORT --loc "$WORK/jena.db" /bench > fuseki.log 2>&1 &
  PIDS+=($!)
  PORT[jena]=$JPORT
  wait_for "localhost:$JPORT/\$/ping"
  NAME[jena]=jena-fuseki
  URL[jena]=localhost:$JPORT/bench/sparql
fi
if has qlever; then
  (cd qlever-index && exec "${CAP[@]}" "$QSERVER" -i bench -p $QPORT -m 8G -c 2G -e 0B -s 600s -a bench -j 16 > server.log 2>&1) &
  PIDS+=($!)
  PORT[qlever]=$QPORT
  wait_for "localhost:$QPORT/?cmd=stats"
  NAME[qlever]=qlever
  URL[qlever]=localhost:$QPORT/
fi
if has fluree; then
  (cd fluree && FLUREE_CACHE_MAX_MB=4096 FLUREE_PATH_MAX_VISITED=20000000 FLUREE_QUERY_TIMEOUT_MS=600000 \
    exec "${CAP[@]}" "$FLUREE" server run --listen-addr 127.0.0.1:$FPORT --storage-path "$WORK/fluree/.fluree/storage" --log-level warn > ../fluree.log 2>&1) &
  PIDS+=($!)
  PORT[fluree]=$FPORT
  wait_for "localhost:$FPORT/health"
  NAME[fluree]=fluree
  URL[fluree]=localhost:$FPORT/v1/fluree/query/bench:main
fi
if has oxigraph; then
  "${CAP[@]}" "$OXIGRAPH" serve --location "$WORK/oxigraph.db" --bind 127.0.0.1:$OPORT --timeout-s 600 > oxigraph.log 2>&1 &
  PIDS+=($!)
  PORT[oxigraph]=$OPORT
  wait_for "localhost:$OPORT/query?query=ASK%7B%7D"
  NAME[oxigraph]=oxigraph
  URL[oxigraph]=localhost:$OPORT/query
fi
# hyperfine arguments for every selected engine: engine_args <command-fn>
engine_args() { for e in $ENGINES; do printf '%s\0' --command-name "${NAME[$e]}" "$($1 "$e")"; done; }
q() { # curl command for endpoint + query name (fails on HTTP errors)
  echo "curl -sf --max-time ${MAX_TIME:-300} -o /dev/null -H 'Accept: text/tab-separated-values' --data-urlencode query@queries/$2.rq $1"
}

# ---------------------------------------------------------------------------- answers
echo
printf '%-8s' rows
for e in $ENGINES; do printf ' %12s' "${NAME[$e]}"; done
echo
for n in "${NAMES[@]}"; do
  selected "$n" || continue
  printf '%-8s' "$n"
  for e in $ENGINES; do
    r=$(python3 "$ROOT/scripts/bench-answers.py" "${URL[$e]}" "queries/$n.rq" results/answers.json "$n" "${NAME[$e]}")
    printf ' %12s' "$r"
  done
  echo
done

# ---------------------------------------------------------------------------- timings
if [ -z "${ANSWERS_ONLY:-}" ]; then
  # clears QLever's result cache before every run (a no-op without QLever)
  CLEAR="true"
  has qlever && CLEAR="curl -sf -o /dev/null 'localhost:$QPORT/?cmd=clear-cache&access-token=bench'"
  for n in "${NAMES[@]}"; do
    selected "$n" || continue
    echo
    echo "== $n"
    qcmd() { q "${URL[$1]}" "$n"; }
    mapfile -d '' ARGS < <(engine_args qcmd)
    hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic --ignore-failure --prepare "$CLEAR" \
      "${ARGS[@]}" --export-json "results/$n.new.json"
    merge "results/$n.new.json" "results/$n.json"
  done
fi

# ---------------------------------------------------------------------------- summary
python3 "$ROOT/scripts/bench-watdiv/summary.py" "$WORK/results" "$(wc -l < data.nt)" "$PARAMS" "${NAMES[@]}"
