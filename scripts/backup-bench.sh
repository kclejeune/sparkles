#!/usr/bin/env bash
# Backup repository cost on an existing database, to an `fs` repository:
#   * full: the first backup into an empty repository (time, logical size, throughput);
#   * incremental: a backup after COMMITS single-quad commits (time, bytes added);
#   * restore: the incremental backup into a new directory, with the default quick check;
#   * verify: the incremental backup at level `data` (every blob read and hashed).
#
#   scripts/backup-bench.sh DB [WORKDIR]
#
# DB is not changed: every round copies it (it must not be open in a server), backs the
# copy up into a fresh repository, commits to the copy through a server on PORT (stopped
# before the incremental backup: the CLI works on closed databases), and so on. Times are
# wall clock, process start included. Run it alone on a quiet machine, on the file
# system the repository should live on (WORKDIR). Results go to
# WORKDIR/results/backup.{json,md}.
# Env: RUNS (default 3), COMMITS (default 1000), SPARKLES (binary, default
# target/release/sparkles), PORT (default 3938).
set -euo pipefail
export LC_ALL=C

DB=${1:?usage: scripts/backup-bench.sh DB [WORKDIR]}
WORK=${2:-/tmp/sparkles-bench-backup}
RUNS=${RUNS:-3}
COMMITS=${COMMITS:-1000}
PORT=${PORT:-3938}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
DB=$(cd "$DB" && pwd)
mkdir -p "$WORK/results"
WORK=$(cd "$WORK" && pwd)
cd "$WORK"
# the CLI's manifest cache, per work directory
export XDG_CACHE_HOME=$WORK/cache

if [ ! -f "$DB/CURRENT" ]; then
  echo "$DB is not a Sparkles database" >&2
  exit 1
fi
if [ -f "$DB/sparkles.lock" ] && ! flock -n "$DB/sparkles.lock" true; then
  echo "$DB is open in another process (a server?): stop it first" >&2
  exit 1
fi
if ss -ltn | grep -q ":$PORT "; then
  echo "port $PORT is in use (set PORT)" >&2
  exit 1
fi

SPID=
# stop the server we started, and whatever still listens on its port; either may be gone
# already, so a failed lookup or kill must not fail the run (set -e, pipefail)
stop_server() {
  if [ -n "$SPID" ]; then
    kill "$SPID" 2> /dev/null || true
    wait "$SPID" 2> /dev/null || true
    SPID=
  fi
  pid=$(ss -ltnp 2> /dev/null | grep ":$PORT " | sed -n 's/.*pid=\([0-9]*\).*/\1/p' | head -1 || true)
  [ -n "$pid" ] && { kill "$pid" 2> /dev/null || true; }
  true
}
trap stop_server EXIT
start_server() { # start_server <db>
  rm -rf server
  "$SPARKLES" serve --data server --loc bench="$1" --port "$PORT" --no-access-log > server.log 2>&1 &
  SPID=$!
  for _ in $(seq 1 240); do
    curl -sf "localhost:$PORT/\$/ping" > /dev/null 2>&1 && return 0
    kill -0 "$SPID" 2> /dev/null || break
    sleep 0.5
  done
  echo "the server did not start:" >&2
  cat server.log >&2
  exit 1
}

# timed <json-out> <command…>: run the command with its stdout in <json-out> and its
# progress lines in the matching .log; the wall clock seconds go to $SECS
SECS=
timed() {
  local out=$1 t0 t1
  shift
  t0=$EPOCHREALTIME
  if ! "$@" > "$out" 2> "${out%.json}.log"; then
    cat "${out%.json}.log" >&2
    exit 1
  fi
  t1=$EPOCHREALTIME
  SECS=$(awk -v a="$t0" -v b="$t1" 'BEGIN { printf "%.3f", b - a }')
}

REPO=file://$WORK/repo
RUNS_FILE=results/backup-runs.jsonl
rm -f results/full-* results/incr-* results/restore-* results/verify-*
: > "$RUNS_FILE"
echo "database: $DB ($(du -sh "$DB" | cut -f1)); $RUNS rounds, $COMMITS commits each"
for r in $(seq 1 "$RUNS"); do
  rm -rf db repo restored cache
  cp -a "$DB" db
  rm -f db/sparkles.lock

  timed "results/full-$r.json" "$SPARKLES" backup create --loc db --repo "$REPO" --name full --dataset bench --format json
  full_s=$SECS
  full_repo=$(du -sb repo | cut -f1)

  start_server "$WORK/db"
  for i in $(seq 1 "$COMMITS"); do
    curl -sf -o /dev/null --data-urlencode \
      "update=INSERT DATA { <http://example.org/backup-bench/s$r-$i> <http://example.org/backup-bench/p> \"$i\" }" \
      "localhost:$PORT/bench/update"
  done
  stop_server

  timed "results/incr-$r.json" "$SPARKLES" backup create --loc db --repo "$REPO" --name incr --dataset bench --format json
  incr_s=$SECS
  incr_repo=$(du -sb repo | cut -f1)

  timed "results/restore-$r.json" "$SPARKLES" backup restore --repo "$REPO" incr --to "$WORK/restored" --format json
  restore_s=$SECS

  timed "results/verify-$r.json" "$SPARKLES" backup verify --repo "$REPO" incr --level data --format json
  verify_s=$SECS

  printf '{"run":%d,"full":%s,"incremental":%s,"restore":%s,"verify":%s,"repoBytesAfterFull":%s,"repoBytesAfterIncremental":%s}\n' \
    "$r" "$full_s" "$incr_s" "$restore_s" "$verify_s" "$full_repo" "$incr_repo" >> "$RUNS_FILE"
  echo "round $r: full ${full_s} s, incremental ${incr_s} s, restore ${restore_s} s, verify (data) ${verify_s} s"
done
rm -rf db restored

python3 - "$DB" "$COMMITS" "$RUNS_FILE" results/backup.json results/backup.md << 'EOF'
import json, statistics, sys
db, commits, runs_file, out_json, out_md = sys.argv[1:6]
runs = [json.loads(l) for l in open(runs_file) if l.strip()]
load = lambda kind, r: json.load(open(f"results/{kind}-{r}.json"))

def stats(xs):
    return {"seconds": xs, "median": statistics.median(xs), "min": min(xs), "max": max(xs)}

last = runs[-1]["run"]
full, incr = load("full", last), load("incr", last)
verify = load("verify", last)
cases = {
    "full": stats([r["full"] for r in runs]),
    "incremental": stats([r["incremental"] for r in runs]),
    "restore": stats([r["restore"] for r in runs]),
    "verifyData": stats([r["verify"] for r in runs]),
}
cases["full"].update(
    logicalBytes=full["logicalBytes"], addedBytes=full["addedBytes"],
    files=full["stats"]["files"], blobs=full["stats"]["blobs"],
    mbPerSecond=full["logicalBytes"] / 1e6 / cases["full"]["median"],
    repositoryBytes=runs[-1]["repoBytesAfterFull"])
cases["incremental"].update(
    logicalBytes=incr["logicalBytes"], addedBytes=incr["addedBytes"],
    newBlobs=incr["stats"]["newBlobs"], reusedBlobs=incr["stats"]["reusedBlobs"],
    repositoryBytesAdded=runs[-1]["repoBytesAfterIncremental"] - runs[-1]["repoBytesAfterFull"])
cases["verifyData"].update(requests=verify["requests"], status=verify["status"])
result = {
    "database": db,
    "quads": full["commit"]["quads"],
    "commits": int(commits),
    "runs": len(runs),
    "cases": cases,
    "rounds": runs,
}
json.dump(result, open(out_json, "w"), indent=1)

mb = lambda b: f"{b / 1e6:.1f} MB" if b >= 1e5 else f"{b / 1e3:.1f} KB"
span = lambda c: f'{c["median"]:.2f} ({c["min"]:.2f}–{c["max"]:.2f})'
out = [
    f'Backup repository (`fs`) on {db}: {result["quads"]} quads, {len(runs)} rounds '
    f"(seconds: median, min–max; wall clock with process start)",
    "",
    "| Case | Seconds | Size |",
    "|---|---:|---|",
    f'| full backup | {span(cases["full"])} | {mb(full["logicalBytes"])} logical, '
    f'{mb(full["addedBytes"])} stored, {cases["full"]["mbPerSecond"]:.0f} MB/s |',
    f'| incremental after {commits} commits | {span(cases["incremental"])} | '
    f'{mb(incr["addedBytes"])} added ({incr["stats"]["newBlobs"]} new blobs, '
    f'{incr["stats"]["reusedBlobs"]} reused) |',
    f'| restore (quick check) | {span(cases["restore"])} | {mb(incr["logicalBytes"])} |',
    f'| verify `data` | {span(cases["verifyData"])} | {verify["requests"]["get"]} GETs, '
    f'{verify["status"]} |',
]
open(out_md, "w").write("\n".join(out) + "\n")
print("\n".join(out))
EOF
