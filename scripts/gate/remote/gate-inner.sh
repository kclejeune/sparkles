#!/usr/bin/env bash
# Runs inside the systemd user unit gate-<name> on a gate host, after gate-start.sh has made
# the worktree. Holds ~/gate/.lock, a symlink to ~/lanes/.heavy.lock, for the whole run, so
# gates and lane heavy runs on the same host take turns.
#
# Usage: gate-inner.sh <full-sha> <name> <all|ci|conformance> <ci-jobs>
#
# A whole gate (part all) writes gate.exit. A half (part ci or conformance) writes
# part.exit instead, and scripts/gate/gate.sh on the laptop merges both halves and writes
# gate.exit, since the gate hosts cannot reach each other.
set -uo pipefail
full=$1
name=$2
part=$3
jobs=$4
G=$HOME/gate
wt=$G/wt-$name
out=$G/out/$name
host=$(hostname)
mkdir -p "$out"
log() { echo "$(date -u +%FT%TZ) $*" >> "$out/progress.log"; }

exec 9> "$G/.lock"
log "waiting for lock $(readlink -f "$G/.lock")"
flock 9
log "lock acquired"
date +%s > "$out/started_at"

# Memory watcher, every 30 s. memory.peak counts page cache, so with a tight cap it reaches
# the cap during the Rust tests while reclaim keeps the run going. anon is the process
# memory, and anonmax its highest sample.
cg=/sys/fs/cgroup$(cut -d: -f3 < /proc/self/cgroup)
(
  amax=0
  while sleep 30; do
    anon=$(awk '$1 == "anon" { print $2 }' "$cg/memory.stat" 2> /dev/null)
    file=$(awk '$1 == "file" { print $2 }' "$cg/memory.stat" 2> /dev/null)
    [ "${anon:-0}" -gt "$amax" ] && amax=$anon
    echo "$(date -u +%FT%TZ) peak=$(cat "$cg/memory.peak" 2> /dev/null) current=$(cat "$cg/memory.current" 2> /dev/null) anon=$anon file=$file anonmax=$amax swap=$(cat "$cg/memory.swap.current" 2> /dev/null) oom_kill=$(awk '$1 == "oom_kill" { print $2 }' "$cg/memory.events" 2> /dev/null)" >> "$out/mempeak.log"
  done
) &
watcher=$!
trap 'kill $watcher 2> /dev/null' EXIT

finish() {
  local code=$1
  date +%s > "$out/done_at"
  if [ "$part" = all ]; then echo "$code" > "$out/gate.exit"; else echo "$code" > "$out/part.exit"; fi
  exit "$code"
}

cd "$wt" || {
  log "no worktree $wt"
  finish 3
}
test "$(git rev-parse HEAD)" = "$full" || {
  echo "HEAD is not $full" > "$out/abort"
  log abort
  finish 3
}

export PATH="$G/py-venv/bin:$HOME/.local/bin:$HOME/.cargo/bin:/run/wrappers/bin:$HOME/.nix-profile/bin:/etc/profiles/per-user/$USER/bin:/nix/var/nix/profiles/default/bin:/run/current-system/sw/bin"
export CARGO_BUILD_JOBS=${GATE_CARGO_JOBS:-4}
export CARGO_INCREMENTAL=0
set -a
# shellcheck disable=SC1091
source "$G/conformance.env"
set +a

step() {
  local name=$1 t0 c
  shift
  log "start $name"
  t0=$SECONDS
  "$@" > "$out/$name.log" 2>&1
  c=$?
  echo "$c" > "$out/$name.exit"
  log "end $name exit=$c secs=$((SECONDS - t0))"
  printf '%-20s exit=%-3s %6ss  %s\n' "$name" "$c" "$((SECONDS - t0))" "$host" >> "$out/steps.txt"
}

# The repo's mise tools (node, pnpm, java 17 and 21, gradle, nextest, shfmt and others).
# A conformance half names its install step apart, so the merged out directory keeps both.
mise trust -q "$wt" > /dev/null 2>&1 || true
if [ "$part" = conformance ]; then mi=mise-install-conf; else mi=mise-install; fi
step "$mi" mise install
eval "$(mise env -s bash)"
# The venv's pytest first, as on the laptop.
export PATH="$G/py-venv/bin:$PATH"
{
  echo "host=$host commit=$full part=$part jobs=$jobs"
  rustc -V 2> /dev/null
  mbx --version 2> /dev/null
  mise --version 2> /dev/null | head -1
  node --version
  java -version 2>&1 | head -1
  python3 --version
} > "$out/env.txt" 2>&1

if [ "$part" != conformance ]; then
  step ci scripts/ci.sh --force --jobs "$jobs"
  # scripts/ci.sh keeps its summary and task logs in .artifacts/ci, and older revisions in
  # target/ci. Copy them before the cleanup below removes the worktree.
  if [ -d .artifacts/ci ]; then
    cp -a .artifacts/ci "$out/ci-receipts" 2> /dev/null
  else
    cp -a target/ci "$out/ci-receipts" 2> /dev/null
  fi
fi
if [ "$part" != ci ]; then
  for t in w3c shacl shex; do step "$t" mise run --force "test:$t"; done
  for m in plain encrypted; do
    step "w3c-spill-$m" env SPARKLES_W3C_SPILL=$m mbx test -p sparkles-core --test w3c sparql_query_cursors -- --nocapture --test-threads=1
  done
fi

fail=0
for f in "$out"/*.exit; do [ "$(cat "$f")" = 0 ] || fail=1; done
cat "$cg/memory.peak" > "$out/mempeak.final" 2> /dev/null
log "done fail=$fail peak=$(cat "$out/mempeak.final" 2> /dev/null) $(tail -n1 "$out/mempeak.log" 2> /dev/null | grep -o 'anonmax=[0-9]*')"

# Free the build dirs and keep the out dir. A passing run drops the whole worktree, and a
# failing one keeps its sources for inspection.
cd "$G" || finish 3
rm -rf "$wt/target" "$wt/jvm/build" "$wt/jvm/.gradle" "$wt/jvm/"*/build
find "$wt" -name node_modules -type d -prune -exec rm -rf {} + 2> /dev/null
if [ $fail = 0 ]; then
  git -C "$G/repo.git" worktree remove --force "$wt" && log "worktree removed"
else
  log "worktree sources kept at $wt"
fi
finish $fail
