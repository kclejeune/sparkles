#!/usr/bin/env bash
# Run a lane's `scripts/ci.sh --fast --changed BASE`, or a batch's `scripts/premerge.sh`, on a
# gate host or an admitted Boat worker instead of the laptop, and bring the results back.
#
# Usage: scripts/remote-ci.sh [--host H] [--base REV] [--premerge] [--no-wait] [REV]
#        scripts/remote-ci.sh [--host H] [--worktree] [--no-wait] --cmd 'COMMAND' [REV]
#        scripts/remote-ci.sh [--host H] --status NAME | --fetch NAME
#   --host      boat:bx_ID or an ssh gate host such as wally (default SPARKLES_REMOTE_CI_HOST)
#   --base      the base of --changed (default origin/main), of which the job uses the merge
#               base with REV; ignored with --premerge and --cmd
#   --premerge  run scripts/premerge.sh instead of ci.sh
#   --cmd       run COMMAND with bash -c in the checkout instead of ci.sh, such as
#               'mbx check' or 'mbx nextest run -p sparkles-core --test csvw_conformance'
#   --worktree  check the working tree as it is, with its uncommitted and untracked files,
#               instead of REV
#   --no-wait   start the job and return; --status and --fetch follow it
#   REV         the commit to check (default HEAD); it must be committed
#
# With --cmd the job checks out the whole commit, rust-toolchain.toml and mise.toml
# included, exports RUSTUP_TOOLCHAIN from rust-toolchain.toml, runs the command under
# `mise exec` so the tools that mise.toml pins apply, and writes the `rustc --version` and
# `mbx --version` it used at the top of its log, so its result matches a local run. The
# job's name is cmd-<rev>-<hash of the command>, and the script prints the command's whole
# log, or its last REMOTE_CI_LOG_LINES lines, and exits with its code. With --worktree the commit is a temporary one whose parent is
# HEAD, written from a private index, so neither the branch nor the index changes.
#
# The job runs in the systemd user unit gate-NAME on the host, NAME being ci-<rev>-<base> or
# pm-<rev> with 8-digit hashes, from scripts/gate/remote/ci-job.sh. Everything it does,
# its worktree and the cleanup of a full disk included, happens inside that unit, which
# holds the host's heavy lock ~/gate/.lock while it builds, so one heavy job runs at a time
# and later jobs wait in their own units. On a Boat worker the job's end is fixed before the
# start at REMOTE_CI_RESERVE seconds before the admitted end, 1200 by default, so the
# closure watch's 900-second lead finds it closed. The host computes the unit's runtime
# from that end immediately before systemd-run and refuses to start with under
# REMOTE_CI_MIN_SECONDS left, 600 by default. On an ssh host the job may run 6 hours.
# Under the heavy lock, a job on any host first needs REMOTE_CI_MIN_FREE_GIB free, 85 by
# default, and refuses with exit 4 otherwise. A cold workspace test build of the lane CI grew
# the disk by 67.8 GiB on the large Boat worker, about 60 GiB of it in the mbx cache, and 85
# adds 25%. A Boat worker frees finished work with its disk cleanup first, and an ssh host
# runs `mbx gc` with gc.min_free_size raised to the floor, which evicts cache objects and
# managed targets until the floor is free. On an ssh host the job also reads the free space
# every REMOTE_CI_WATCH_SECONDS while the build and tests run, 30 by default, and once more
# when they end, and stops them with exit 5 when it falls under REMOTE_CI_RUN_FLOOR_GIB, 4
# by default, since the server's tests keep 1 GiB free.
# Either refusal writes disk-below-floor to the job's outcome file, and this script then
# says that the host's disk was below its floor.
# Results land in .artifacts/remote-ci/NAME.ID, ID being the start's id, with ci.exit, ci.log
# and the ci or premerge logs, and .artifacts/remote-ci/NAME links to the newest. A job whose
# run's processes cannot be shown ended exits 6 and leaves its worktree on the host.
# A Boat worker needs SPARKLES_BOAT_PROFILE and SPARKLES_BOAT_ADMISSION, as every Boat
# command does. The host needs ~/gate from scripts/gate/setup-host.sh.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=scripts/remote-executor.sh
. "$here/remote-executor.sh"
host=${SPARKLES_REMOTE_CI_HOST:-}
base=origin/main
mode=ci
wait=1
action=start
name=
rev=HEAD
cmd=
worktree=0
while [ $# -gt 0 ]; do
  case $1 in
    --host)
      host=$2
      shift 2
      ;;
    --base)
      base=$2
      shift 2
      ;;
    --premerge)
      mode=premerge
      shift
      ;;
    --cmd)
      mode=cmd
      cmd=${2:?--cmd needs a command}
      shift 2
      ;;
    --worktree)
      worktree=1
      shift
      ;;
    --no-wait)
      wait=0
      shift
      ;;
    --status | --fetch)
      action=${1#--}
      name=$2
      shift 2
      ;;
    -h | --help)
      sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
      exit 0
      ;;
    -*)
      echo "remote-ci.sh: unknown option $1" >&2
      exit 2
      ;;
    *)
      rev=$1
      shift
      ;;
  esac
done
[ -n "$host" ] || {
  echo "remote-ci.sh: no host; pass --host or set SPARKLES_REMOTE_CI_HOST" >&2
  exit 2
}
# shellcheck source=scripts/gate/config.sh
. "$here/gate/config.sh"
if is_bench "$host"; then
  echo "remote-ci.sh: $host is the benchmark host (SPARKLES_BENCH_HOST) and never runs CI" >&2
  exit 2
fi
root=$(git rev-parse --show-toplevel)
cd "$root"

status() {
  # shellcheck disable=SC2016
  rx_exec "$host" "o=\$HOME/gate/out/$1; cat \"\$o/progress.log\" 2> /dev/null; [ ! -f \"\$o/run-id\" ] || echo \"run-id=\$(cat \"\$o/run-id\")\"; [ ! -f \"\$o/ci.exit\" ] || echo \"ci.exit=\$(cat \"\$o/ci.exit\")\"; systemctl --user is-active gate-$1 || true"
}
# fetch NAME [RUN_ID]: copy the job's results into a staging directory, check the copy and,
# with RUN_ID, that it is that start's own, and publish it, setting res to its path. Each copy
# goes to a path of its own, NAME.RUN_ID or, for a fetch by name, NAME.fetch-TIME-PID, which
# nothing replaces, and NAME becomes a link to it by an atomic rename. An older NAME that is a
# directory is exchanged with the new link atomically, where mv can, and is otherwise left in
# place, so NAME is never missing. Every step checks its own result, since a caller may run
# fetch where errexit is off.
fetch() {
  local base=$root/.artifacts/remote-ci stage link id=${2:-fetch-$(date +%s)-$$}
  res=
  mkdir -p "$base" || return 1
  stage=$(mktemp -d "$base/.fetch-$1.XXXXXX") || return 1
  case $stage in "$base"/.fetch-*) ;; *)
    echo "remote-ci.sh: bad staging directory" >&2
    return 1
    ;;
  esac
  if ! rx_sync -- -a "$host:gate/out/$1/" "$stage/"; then
    rm -rf "$stage"
    echo "remote-ci.sh: could not copy the results of gate-$1" >&2
    return 1
  fi
  if [ -n "${2:-}" ] && [ "$(cat "$stage/run-id" 2> /dev/null)" != "$2" ]; then
    rm -rf "$stage"
    echo "remote-ci.sh: another start of gate-$1 replaced this one's results on $host" >&2
    return 1
  fi
  if ! mv -T "$stage" "$base/$1.$id"; then
    rm -rf "$stage"
    return 1
  fi
  res=$base/$1.$id
  # One publication of a name at a time, so two fetches never both migrate an older copy.
  # Without the lock the name keeps its earlier copy, and the new copy stays at res.
  local fd='' linked=0
  if ! exec {fd}>> "$base/.lock-$1"; then
    fd=
    echo "remote-ci.sh: could not open the publication lock of $1" >&2
  elif ! flock -w 60 "$fd"; then
    exec {fd}>&-
    fd=
    echo "remote-ci.sh: another fetch of $1 held its lock for 60 s" >&2
  fi
  link=$(mktemp -u "$base/.link-$1.XXXXXX")
  if [ -z "$fd" ] || ! ln -s "$1.$id" "$link"; then
    :
  elif [ -d "$base/$1" ] && [ ! -L "$base/$1" ]; then
    if mv --exchange -T "$link" "$base/$1" 2> /dev/null; then
      linked=1
      mv -T "$link" "$base/$1.kept-$(date +%s)-$$" 2> /dev/null || echo "remote-ci.sh: the older copy of $1 is at $link" >&2
    else
      rm -f "$link"
    fi
  elif mv -Tf "$link" "$base/$1"; then
    linked=1
  else
    rm -f "$link"
  fi
  [ -z "$fd" ] || exec {fd}>&-
  echo "results in $res"
  if [ "$linked" = 0 ]; then
    echo "remote-ci.sh: $base/$1 could not point to the new copy and still holds the earlier one" >&2
    # A fetch by name exists to update the name, so it fails. The waiter reads its own copy.
    [ -n "${2:-}" ] || return 1
  fi
}
case $name in '' | *[!A-Za-z0-9._-]*) [ "$action" = start ] || {
  echo "remote-ci.sh: bad job name" >&2
  exit 2
} ;; esac
if [ "$action" = status ]; then
  status "$name"
  exit 0
fi
if [ "$action" = fetch ]; then
  fetch "$name" || exit 1
  exit 0
fi

if [ "$worktree" = 1 ]; then
  # The working tree goes into a commit object of its own, through a private index, so the
  # lane's branch, index and stash are left alone.
  idx=$(mktemp)
  trap 'rm -f "$idx"' EXIT
  cp "$(git rev-parse --git-path index)" "$idx"
  GIT_INDEX_FILE=$idx git add -A
  tree=$(GIT_INDEX_FILE=$idx git write-tree)
  rm -f "$idx"
  rev=$(git commit-tree --no-gpg-sign -p HEAD -m "remote-ci working tree of $(git rev-parse --short HEAD)" "$tree")
fi
full=$(git rev-parse --verify "$rev^{commit}")
if [ "$mode" = cmd ]; then
  bfull=$full
  name=cmd-${full:0:8}-$(printf '%s' "$cmd" | sha256sum | cut -c1-8)
elif [ "$mode" = ci ]; then
  # The changed set is taken from the merge base, which the tip's own history holds, so the
  # host has it whenever it has the tip, even when the base branch moved on.
  bfull=$(git merge-base "$full" "$(git rev-parse --verify "$base^{commit}")")
  name=ci-${full:0:8}-${bfull:0:8}
else
  bfull=$full
  name=pm-${full:0:8}
fi
now=$(date +%s)
case $host in
  boat:*)
    obs=$(mktemp)
    trap 'rm -f "$obs"' EXIT
    rx_admit "$host" --minimum "${SPARKLES_REMOTE_MIN_LIFETIME:-2400}" > "$obs"
    if [ "$bfull" = "$full" ]; then rx_bundle "$host" "$full" > /dev/null; else rx_bundle "$host" "$full" "$bfull" > /dev/null; fi
    runtime=$(rx_runtime "$obs" --reserve "${REMOTE_CI_RESERVE:-1200}")
    end=$(($(date +%s) + runtime))
    executor=boat
    # Qualification admits a Boat worker for a job of BOAT_JOB_MEMORY_GIB in
    # scripts/boat/metadata-probe.py, so the job's cap is that figure.
    memory_max=12G
    cargo_jobs=6
    delegate=
    ;;
  *)
    git push -q "ssh://$host/home/$USER/gate/repo.git" "$full:refs/heads/ci-${full:0:8}" "$bfull:refs/heads/ci-base-${bfull:0:8}"
    end=$((now + 21600))
    executor=ssh
    # The host expands this, so its own GATE_MEMORY_MAX applies.
    # shellcheck disable=SC2016
    memory_max='${GATE_MEMORY_MAX:-13G}'
    cargo_jobs=6
    # The job runs its check in a child cgroup, so that a stop ends every process of it.
    delegate='-p Delegate=yes'
    ;;
esac
min=${REMOTE_CI_MIN_SECONDS:-600}
# Each start has its own id, which the job records first, so the wait and the fetch take only
# this start's results even when a later start of the same name replaces them.
run_id=$(date +%s%N)-$$
for n in REMOTE_CI_MIN_FREE_GIB REMOTE_CI_RUN_FLOOR_GIB REMOTE_CI_WATCH_SECONDS; do
  case ${!n:-1} in '' | *[!0-9]* | 0 | 0*)
    echo "remote-ci.sh: $n must be a positive whole number" >&2
    exit 2
    ;;
  esac
done
# The job's script travels on stdin. Under the host's start lock, so that two starts of one
# name cannot both pass the check, the host checks that the unit of that name has fully
# stopped, rather than only left the active state, stores the
# script beside the job's results and starts the unit, whose runtime it takes from the end.
# shellcheck disable=SC2016
rx_exec "$host" "set -e; umask 022; G=\$HOME/gate; o=\$G/out/$name
exec 8>> \$G/.remote-ci-start.lock
flock -w 120 8 || { echo 'another remote-ci start holds the start lock' >&2; exit 4; }
st=\$(systemctl --user show -p ActiveState --value gate-$name)
case \$st in inactive | failed) ;; *) echo \"gate-$name is \$st; it must have stopped first\" >&2; exit 1 ;; esac
systemctl --user reset-failed gate-$name 2> /dev/null || true
if [ -d \"\$o\" ]; then mv \"\$o\" \"\$o.old-\$(date +%s)\"; fi
mkdir -p \"\$o\"; cat > \"\$o/ci-job.sh\"
git -C \"\$G/repo.git\" cat-file -e $full^{commit}
git -C \"\$G/repo.git\" cat-file -e $bfull^{commit}
rt=\$(($end - \$(date +%s)))
if [ \"\$rt\" -lt $min ]; then echo \"only \$rt s left before the job's end; not starting\" >&2; exit 3; fi
forward=()
for n in MBX_REMOTE_URL MBX_CACHE_DIR MBX_GC_MAX_TOTAL_SIZE MBX_GC_MIN_FREE_SIZE RUSTUP_TOOLCHAIN RUSTUP_AUTO_INSTALL; do
  eval \"s=\\\${\$n+x} v=\\\${\$n-}\"
  [ -z \"\$s\" ] || forward+=(\"--setenv=\$n=\$v\")
done
p=\$PATH; [ ! -d \"\$G/py-venv/bin\" ] || p=\$G/py-venv/bin:\$p
systemd-run --user --unit gate-$name --collect -q \"\${forward[@]}\" \\
  -p MemoryMax=$memory_max -p MemorySwapMax=2G -p OOMPolicy=continue -p RuntimeMaxSec=\$rt -p TimeoutStopSec=60 -p UMask=0022 $delegate \\
  --setenv=HOME=\"\$HOME\" --setenv=USER=\"\$USER\" --setenv=PATH=\"\$p\" --setenv=SPARKLES_REMOTE_EXECUTOR=$executor \\
  --setenv=CARGO_BUILD_JOBS=$cargo_jobs --setenv=CARGO_INCREMENTAL=0 \\
  --setenv=NEED_GIB=${REMOTE_CI_MIN_FREE_GIB:-85} --setenv=RUN_FLOOR_GIB=${REMOTE_CI_RUN_FLOOR_GIB:-4} --setenv=WATCH_SECONDS=${REMOTE_CI_WATCH_SECONDS:-30} \\
  --setenv=NAME=$name --setenv=FULL=$full --setenv=BASE=$bfull --setenv=MODE=$mode --setenv=END=$end --setenv=RUN_ID=$run_id --setenv=CMD_B64=$(printf '%s' "$cmd" | base64 -w0) \\
  /bin/bash \"\$o/ci-job.sh\"
echo \"started gate-$name, ending by \$(date -u -d @$end +%FT%TZ)\"" < "$here/gate/remote/ci-job.sh"
[ "$wait" = 1 ] || {
  echo "follow with: scripts/remote-ci.sh --host $host --status $name"
  exit 0
}
start=$(date +%s)
# done_here STATUS: 0 when STATUS holds this start's id and an exit record, which are lines
# of their own that no line of progress.log can be; 2 when another start's id is there.
done_here() {
  local id
  id=$(printf '%s\n' "$1" | sed -n 's/^run-id=//p' | tail -n 1)
  [ -z "$id" ] || [ "$id" = "$run_id" ] || return 2
  [ "$id" = "$run_id" ] && printf '%s\n' "$1" | grep -Eqx 'ci\.exit=[0-9]+'
}
while :; do
  s=$(status "$name" 2> /dev/null || true)
  done_here "$s" && break
  [ $? != 2 ] || {
    echo "remote-ci.sh: another start of gate-$name replaced this one on $host" >&2
    exit 1
  }
  case $(printf '%s\n' "$s" | tail -n 1) in inactive | failed)
    # The unit can stop between the read of the exit record and the read of its state, so
    # a stopped unit's record is read once more before it counts as missing.
    s=$(status "$name" 2> /dev/null || true)
    done_here "$s" && break
    echo "gate-$name ended without an exit status" >&2
    fetch "$name" "$run_id" || true
    exit 1
    ;;
  esac
  [ "$(date +%s)" -lt $((end + 120)) ] || {
    echo "gate-$name passed its end" >&2
    exit 1
  }
  sleep "${REMOTE_CI_POLL_SECONDS:-30}"
done
fetch "$name" "$run_id" || exit 1
code=$(cat "$res/ci.exit")
echo "gate-$name exit $code in $(($(date +%s) - start)) s"
if [ "$(cat "$res/outcome" 2> /dev/null)" = disk-below-floor ]; then
  echo "remote-ci.sh: host disk below floor on $host, so the job did not finish; its result says nothing about $full" >&2
  grep 'host disk below floor' "$res/progress.log" >&2 || true
fi
if [ "$mode" = cmd ]; then
  # The whole log, unless REMOTE_CI_LOG_LINES asks for its last lines.
  if [ -n "${REMOTE_CI_LOG_LINES:-}" ]; then
    tail -n "$REMOTE_CI_LOG_LINES" "$res/ci.log" 2> /dev/null || true
  else
    cat "$res/ci.log" 2> /dev/null || true
  fi
else
  [ ! -f "$res/ci.log" ] || tail -n 30 "$res/ci.log"
fi
[ -f "$res/ci.log" ] || tail -n 5 "$res/progress.log"
exit "$code"
