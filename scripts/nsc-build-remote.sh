#!/usr/bin/env bash
# Builds one benchmark revision on a build host for scripts/nsc-build-host.sh, which copies
# this file, scripts/nsc-build.sh and scripts/cargo-sparkles-task into DIR/bin and the
# revision's `git archive` into DIR/src before it calls it over ssh.
#
#   nsc-build-remote.sh DIR UNIT EXPIRY [feature and dependency-resolution flags...]
#   nsc-build-remote.sh --status DIR UNIT OFFSET
#   nsc-build-remote.sh --cancel DIR UNIT
#   nsc-build-remote.sh --run DIR UNIT EXPIRY [flags...]
#
# The first form checks the host and starts the build detached, and returns once the build
# runs in the transient systemd user scope UNIT. The scope can be listed and stopped by name,
# an out-of-memory kill ends the build alone, and a dropped ssh connection does not end it.
# EXPIRY is the Unix time by which the scope has stopped, its stop included, read on this
# host's clock, which must agree with the caller's. The scope has the gate's memory cap, the
# smaller of 24G and the host's RAM less 2G, with 2G of swap (SPARKLES_NSC_BUILD_MEMORY_MAX
# overrides it). Inside it the build holds ~/lanes/.heavy.lock, which gates and other heavy work on the
# host share, and waits for it when another process holds it. CARGO_BUILD_JOBS defaults to
# the gate's value for the cap. The host's own Boxington cache serves the build, with the
# remote cache settings the caller's environment gives it. The detached process inherits
# that environment, so a Boat job's local-only mbx settings reach the build unchanged.
#
# A start, a status query and a cancellation take DIR/launch.lock. A start claims the build
# by creating DIR/launched, so a repeated start after a lost reply starts nothing, and a start
# after a cancellation (DIR/cancelled) is refused. A refused start, including one whose
# systemd-run fails, records its exit status in DIR/start.exit. The scope's RuntimeMaxSec
# and TimeoutStopSec of 30 seconds are computed at the launch from EXPIRY, and a start with
# less than 300 seconds left is refused. The start waits
# SPARKLES_NSC_BUILD_ACTIVATION_SECONDS (default 30) of elapsed time, with each query
# bounded, for the scope to hold the build. When it does not, the start cancels the build
# itself, stops any scope that appeared meanwhile and is refused with 75. The build checks
# DIR/cancelled and EXPIRY when its scope begins, so a scope that a delayed launcher
# creates after a cancellation builds nothing, and a watchdog inside the scope stops the
# scope at EXPIRY less the stop time however late it began.
#
# --status prints one state line and then the build log from byte OFFSET, at most 1 MiB.
# The state is `running`, `exited N` with the build's exit status, `refused N` for a refused
# start, `gone` when the build was claimed but neither runs nor recorded an exit status, or
# `unlaunched`. --cancel fences any later start, stops the scope and succeeds only when the
# scope is no longer active.
#
# --run is the build inside the scope. Holding the heavy lock, it moves DIR/src to the stable
# path ~/lanes/nsc-build-work/<commit>[-<hash of the flags>]/src, builds there and moves the
# tree back when it ends. Every process of the build inherits the lock, so the lock lasts as
# long as any of them. The binary goes to DIR/out/sparkles with its SHA-256 in
# DIR/out/sha256. DIR/out/info.txt gets the host's cc, c++ and rustc versions in the form of
# build.txt, DIR/out/rustc.txt and cargo.txt the verbose versions, and DIR/out/env.txt the
# build variables that were set. Its exit status goes to DIR/exit. The caller removes DIR.
set -euo pipefail

fail() {
  echo "nsc-build-remote ($(uname -n)): $*" >&2
  exit 3
}
lock=$HOME/lanes/.heavy.lock

if [ "${1:-}" = --status ]; then
  [ "$#" = 4 ] || fail 'usage: nsc-build-remote.sh --status DIR UNIT OFFSET'
  dir=$2 unit=$3 offset=$4
  case $offset in '' | *[!0-9]*) fail "bad log offset $offset" ;; esac
  # A start in progress holds the lock, so its state is read once it has finished.
  exec 8>> "$dir/launch.lock"
  flock -s -w 60 8 || fail "the start in $dir holds its lock"
  # The scope writes DIR/exit before it ends, so a scope seen inactive has its status
  # written when it finished normally.
  if systemctl --user is-active --quiet "$unit.scope"; then
    echo running
  elif [ -f "$dir/exit" ]; then
    echo "exited $(cat "$dir/exit")"
  elif [ -f "$dir/start.exit" ]; then
    echo "refused $(cat "$dir/start.exit")"
  elif [ -e "$dir/launched" ]; then
    echo gone
  else
    echo unlaunched
  fi
  # head ends the pipe after the chunk, so tail's SIGPIPE is not a failure.
  if [ -f "$dir/build.log" ]; then { tail -c +$((offset + 1)) "$dir/build.log" | head -c 1048576; } || true; fi
  exit 0
fi

if [ "${1:-}" = --cancel ]; then
  [ "$#" = 3 ] || fail 'usage: nsc-build-remote.sh --cancel DIR UNIT'
  dir=$2 unit=$3
  exec 8>> "$dir/launch.lock"
  flock -w 60 8 || fail "the start in $dir holds its lock"
  : > "$dir/cancelled"
  systemctl --user stop "$unit.scope" > /dev/null 2>&1 || true
  if systemctl --user is-active --quiet "$unit.scope"; then exit 1; fi
  exit 0
fi

if [ "${1:-}" = --run ]; then
  dir=$2 unit=$3 expiry=$4
  shift 4
  # A cancellation writes DIR/cancelled before it stops the scope, so a scope that begins
  # after the cancellation stops here and one that began before it is stopped.
  if [ -e "$dir/cancelled" ]; then
    echo "the build in $dir was cancelled before its scope began"
    exit 75
  fi
  left=$((expiry - 30 - $(date +%s)))
  if [ "$left" -le 0 ]; then
    echo "the build in $dir began after its expiry"
    exit 75
  fi
  # The scope's RuntimeMaxSec counts from its activation, so the watchdog holds the build
  # to EXPIRY. It runs in the scope as a process group of its own, which the build's end
  # kills, so that the scope ends with the build.
  # shellcheck disable=SC2016 # the watchdog's shell expands its own arguments
  setsid bash -c 'sleep "$1"; echo "the build reached its expiry; stopping $2.scope"; echo 124 > "$3/exit.tmp"; mv "$3/exit.tmp" "$3/exit"; systemctl --user stop --no-block "$2.scope"' \
    watchdog "$left" "$unit" "$dir" < /dev/null &
  watchdog=$!
  # One revision with one set of flags always builds at the same path, which the commit
  # and the flags name. mbx records each checkout's predictions of its compiler actions,
  # and a rebuild at the same path finds its own. A rebuild at a new path takes the latest
  # predictions of the same command, which name another revision's crates when revisions
  # alternate, and the crates it then compiles embed the new path, so their dependents
  # miss as well.
  key=${dir##*/}
  key=${key%.*}
  [ "$#" = 0 ] || key=$key-$(printf '%s\n' "$@" | sha256sum | cut -c1-16)
  work=$HOME/lanes/nsc-build-work/$key
  # shellcheck disable=SC2329 # the EXIT trap calls it
  finish() {
    local code=$?
    kill -- "-$watchdog" 2> /dev/null || true
    # The tree goes back to DIR, so each build keeps its own source as before.
    if [ -d "$work/src" ] && [ ! -e "$dir/src" ]; then mv -T "$work/src" "$dir/src" && rm -d -- "$work"; fi
    echo "$code" > "$dir/exit.tmp"
    mv "$dir/exit.tmp" "$dir/exit"
  }
  trap finish EXIT
  export PATH="$dir/bin:$PATH"
  exec 9> "$lock"
  if ! flock -n 9; then
    echo "waiting for $lock on $(uname -n), which another process holds"
    flock 9
  fi
  # Holding the lock, no other build uses the path. A tree left there by a build that was
  # stopped before it could move its tree back is removed.
  rm -rf -- "$work"
  mkdir -p "$work"
  mv -T "$dir/src" "$work/src"
  echo "building in $work/src"
  cd "$work/src"
  bash "$dir/bin/nsc-build.sh" "$dir/out/sparkles" "$@"
  {
    echo "cc: $(cc --version 2> /dev/null | head -1)"
    echo "c++: $(c++ --version 2> /dev/null | head -1)"
    rustc -V
  } > "$dir/out/info.txt"
  rustc -vV > "$dir/out/rustc.txt"
  cargo -V > "$dir/out/cargo.txt"
  for n in RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTUP_TOOLCHAIN CARGO_BUILD_TARGET RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER \
    MBX_REMOTE_URL MBX_GC_MAX_TOTAL_SIZE MBX_GC_MIN_FREE_SIZE; do
    [ -z "${!n+x}" ] || printf '%s=%s\n' "$n" "${!n}"
  done > "$dir/out/env.txt"
  sha256sum "$dir/out/sparkles" | cut -d' ' -f1 > "$dir/out/sha256"
  exit 0
fi

if [ "$#" -lt 3 ]; then
  echo 'usage: nsc-build-remote.sh DIR UNIT EXPIRY [cargo flags...]' >&2
  exit 2
fi
dir=$1
unit=$2
expiry=$3
shift 3
[ -d "$dir/bin" ] || fail "$dir has no bin directory"
exec 8>> "$dir/launch.lock"
flock -w 60 8 || fail "another start in $dir holds its lock"
if [ -e "$dir/launched" ]; then
  echo "the build in scope $unit was already started"
  exit 0
fi
# shellcheck disable=SC2329 # the EXIT trap calls it
refused() {
  local code=$?
  [ "$code" = 0 ] || [ -f "$dir/start.exit" ] || echo "$code" > "$dir/start.exit"
}
trap refused EXIT
[ ! -e "$dir/cancelled" ] || fail "the build in $dir was cancelled"
[ -d "$dir/src" ] || fail "$dir has no src directory"
case $expiry in '' | *[!0-9]*) fail "bad expiry $expiry" ;; esac
activation=${SPARKLES_NSC_BUILD_ACTIVATION_SECONDS:-30}
case $activation in '' | *[!0-9]* | 0) fail "bad SPARKLES_NSC_BUILD_ACTIVATION_SECONDS $activation" ;; esac
cd "$dir/src"
# The copied scripts come first, so the build uses the caller's nsc-build.sh and finds the
# cargo subcommand behind `mbx sparkles-task`.
export PATH="$dir/bin:$PATH"
for t in mbx rustc cargo flock systemd-run systemctl setsid nohup sha256sum; do command -v "$t" > /dev/null || fail "$t is not on PATH"; done
have=$(mbx --version | awk '{print $2}')
systemd-run --user --scope -q --collect true 2> /dev/null || fail "systemd-run --user --scope does not work"

if [ -n "${SPARKLES_NSC_BUILD_MEMORY_MAX:-}" ]; then
  mem=$SPARKLES_NSC_BUILD_MEMORY_MAX
  mem_g=${mem%G}
  case $mem_g in '' | *[!0-9]*) fail "SPARKLES_NSC_BUILD_MEMORY_MAX is $mem, not a whole number of G" ;; esac
else
  mem_g=$(awk '$1 == "MemTotal:" { m = int($2 / 1048576) - 2; if (m > 24) m = 24; print m }' /proc/meminfo)
  mem=${mem_g}G
fi
jobs=${CARGO_BUILD_JOBS:-$(((mem_g - 1) / 2))}
[ "$jobs" -ge 2 ] || jobs=2
[ "$jobs" -le 8 ] || jobs=8

mkdir -p "$HOME/lanes" "$dir/out"
runtime=$((expiry - 30 - $(date +%s)))
[ "$runtime" -ge 300 ] || fail "only $runtime seconds of runtime remain, too little to start the build"
echo "building in scope $unit on $(uname -n): MemoryMax=$mem, CARGO_BUILD_JOBS=$jobs, RuntimeMaxSec=$runtime, mbx $have"
: > "$dir/launched"
# The build runs in its own session with no tie to this connection, and its output goes
# to DIR/build.log, which --status reads. The launcher does not hold the start lock, and
# setsid runs in place, so its PID becomes the build's process inside the scope.
setsid nohup systemd-run --user --scope -q --collect --unit "$unit" \
  -p MemoryMax="$mem" -p MemorySwapMax=2G -p OOMPolicy=continue -p RuntimeMaxSec="$runtime" -p TimeoutStopSec=30 -- \
  env CARGO_BUILD_JOBS="$jobs" bash "$dir/bin/nsc-build-remote.sh" --run "$dir" "$unit" "$expiry" "$@" \
  > "$dir/build.log" 2>&1 < /dev/null 8>&- &
launcher=$!
# Return once the scope holds the build, so that the end of this connection's session can
# no longer reach a process still on its way into the scope. A launcher that ends before
# the scope is active, with no exit status of the build, is a refused start.
until_t=$(($(date +%s) + activation))
while [ "$(date +%s)" -lt "$until_t" ]; do
  if timeout 5 systemctl --user is-active --quiet "$unit.scope" || [ -f "$dir/exit" ]; then
    echo "started"
    exit 0
  fi
  if ! kill -0 "$launcher" 2> /dev/null; then
    code=0
    wait "$launcher" || code=$?
    if [ "$code" != 0 ] && [ ! -f "$dir/exit" ] && ! timeout 5 systemctl --user is-active --quiet "$unit.scope"; then
      echo "$code" > "$dir/start.exit"
      echo "systemd-run could not start the scope $unit (exit $code); see $dir/build.log" >&2
      exit "$code"
    fi
  fi
  sleep 0.25
done
# A scope that has not begun is cancelled, and one that began in the meantime is stopped.
: > "$dir/cancelled"
timeout 60 systemctl --user stop "$unit.scope" > /dev/null 2>&1 || true
echo "the scope $unit did not start within $activation seconds, so the build was cancelled; see $dir/build.log" >&2
exit 75
