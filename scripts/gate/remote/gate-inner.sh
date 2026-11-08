#!/usr/bin/env bash
# Runs inside the systemd user unit gate-<name> on a gate host, after gate-start.sh has made
# the worktree. Holds ~/gate/.lock, a symlink to ~/lanes/.heavy.lock, for the whole run, so
# gates and lane heavy runs on the same host take turns.
#
# Usage: gate-inner.sh <full-sha> <name> <all|ci|conformance|link> <ci-jobs>
#
# A whole gate (part all) writes gate.exit. A half (part ci or conformance) writes
# part.exit instead, and scripts/gate/gate.sh on the laptop merges both halves and writes
# gate.exit, since the gate hosts cannot reach each other.
#
# GATE_BASE, when set, names the commit the batch is compared with. The run then chooses
# its checks with the commit's scripts/gate/select-checks.py and writes the choice to
# selection.txt, and the task cache of mise skips a check whose inputs already passed.
# GATE_FULL=1 runs every check with --force, as the nightly gate does. A commit without
# the selection script, or a run without a base, runs every check without --force.
#
# Part link runs one step, the server artifact link check, which builds the server in the
# pinned manylinux_2_28 image with podman. It needs neither mise nor the selection, and the
# nightly gate runs it beside the full gate (scripts/gate/nightly.sh).
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
  if [ "$part" = all ] || [ "$part" = link ]; then echo "$code" > "$out/gate.exit"; else echo "$code" > "$out/part.exit"; fi
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
# py:test fails, and does not skip, the stub test on the oldest supported Python without uv.
export SPARKLES_PY_OLDEST=required
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

if [ "$part" = link ]; then
  echo "the server artifact link check only" > "$out/selection.txt"
  {
    echo "host=$host commit=$full part=$part"
    podman --version 2> /dev/null
    python3 --version
  } > "$out/env.txt" 2>&1
  step server-link scripts/server-artifact-link-check.sh
  fail=0
  [ "$(cat "$out/server-link.exit")" = 0 ] || fail=1
  cat "$cg/memory.peak" > "$out/mempeak.final" 2> /dev/null
  log "done fail=$fail peak=$(cat "$out/mempeak.final" 2> /dev/null)"
  cd "$G" || finish 3
  rm -rf "$wt/target" "$wt/.artifacts/server-link-check"
  if [ $fail = 0 ]; then
    git -C "$G/repo.git" worktree remove --force "$wt" && log "worktree removed"
  else
    log "worktree sources kept at $wt"
  fi
  finish $fail
fi

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

# skip NAME REASON: record a step that the selection left out.
skip() {
  log "skip $1 ($2)"
  printf '%-20s skipped        %s\n' "$1" "$2" >> "$out/steps.txt"
}

# The checks to run. Every check by default; a selection narrows them.
force=()
ci_args=()
conf="w3c shacl shex"
spill="plain encrypted"
if [ "${GATE_FULL:-0}" = 1 ]; then
  force=(--force)
  echo "every check with --force (full gate)" > "$out/selection.txt"
elif [ -z "${GATE_BASE:-}" ]; then
  echo "every check: no base commit was given" > "$out/selection.txt"
elif [ ! -f scripts/gate/select-checks.py ]; then
  echo "every check: this commit has no scripts/gate/select-checks.py" > "$out/selection.txt"
else
  # The conformance half needs no package selection, which reads cargo metadata.
  only=()
  [ "$part" != conformance ] || only=(--paths-only)
  if python3 scripts/gate/select-checks.py --base "$GATE_BASE" "${only[@]}" > "$out/selection.txt" 2> "$out/selection.log"; then
    tasks=$(sed -n 's/^ci //p' "$out/selection.txt")
    conf=$(sed -n 's/^conformance *//p' "$out/selection.txt")
    spill=$(sed -n 's/^spill *//p' "$out/selection.txt")
    pkgs=$(sed -n 's/^packages //p' "$out/selection.txt")
    if [ "$tasks" != all ]; then
      read -ra ci_args <<< "$tasks"
    fi
    # ci.sh --changed recomputes the same packages from the clean worktree.
    if [ -n "$pkgs" ] && [ "$pkgs" != all ]; then
      ci_args=(--changed "$GATE_BASE" "${ci_args[@]}")
    fi
  else
    log "select-checks.py failed, so every check runs; see selection.log"
    echo "every check: select-checks.py failed" > "$out/selection.txt"
  fi
fi
log "selection: $(tr '\n' ';' < "$out/selection.txt")"

if [ "$part" != conformance ]; then
  step ci scripts/ci.sh "${force[@]}" --jobs "$jobs" "${ci_args[@]}"
  # New CI revisions retain actual JUnit under their original lease. Copy only that
  # retained evidence here, after ci.sh has returned; never recover it from a target.
  # shellcheck disable=SC2329 # step invokes the function passed as its command.
  retain_ci_receipts() {
    mkdir "$out/ci-receipts" || return
    if [ -d .artifacts/ci ]; then
      cp -a .artifacts/ci/. "$out/ci-receipts/" || return
    else
      cp -a target/ci/. "$out/ci-receipts/" || return
    fi
    if [ -f scripts/ci-retain-junit.py ]; then
      python3 scripts/ci-retain-junit.py verify --directory "$out/ci-receipts/junit"
    else
      echo 'legacy source does not provide JUnit retention' > "$out/ci-receipts/junit-unsupported.txt"
    fi
  }
  step ci-receipts retain_ci_receipts
fi
if [ "$part" != ci ]; then
  for t in w3c shacl shex; do
    if [[ " $conf " == *" $t "* ]]; then
      step "$t" mise run "${force[@]}" "test:$t"
    else
      skip "$t" "no change to the code its suite checks"
    fi
  done
  # The cursor batch sizes are separate tests, which run in parallel. Each query has its
  # own spill directory and budget.
  for m in plain encrypted; do
    if [[ " $spill " == *" $m "* ]]; then
      step "w3c-spill-$m" env SPARKLES_W3C_SPILL=$m mbx test -p sparkles-core --test w3c sparql_query_cursors -- --nocapture
    else
      skip "w3c-spill-$m" "no change to the streaming executor or spill"
    fi
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
