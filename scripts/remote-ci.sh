#!/usr/bin/env bash
# Run a lane's `scripts/ci.sh --fast --changed BASE`, or a batch's `scripts/premerge.sh`, on a
# gate host or an admitted Boat worker instead of the laptop, and bring the results back.
#
# Usage: scripts/remote-ci.sh [--host H] [--base REV] [--premerge] [--no-wait] [REV]
#        scripts/remote-ci.sh [--host H] --status NAME | --fetch NAME
#   --host      boat:bx_ID or an ssh gate host such as wally (default SPARKLES_REMOTE_CI_HOST)
#   --base      the base of --changed (default origin/main), of which the job uses the merge
#               base with REV; ignored with --premerge
#   --premerge  run scripts/premerge.sh instead of ci.sh
#   --no-wait   start the job and return; --status and --fetch follow it
#   REV         the commit to check (default HEAD); it must be committed
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
# Under the heavy lock, a job on a Boat worker first needs REMOTE_CI_MIN_FREE_GIB free after
# the disk cleanup, 85 by default, and refuses with exit 4 otherwise. A cold workspace test
# build of the lane CI grew the disk by 67.8 GiB on the large worker, and 85 adds 25%.
# Results land in .artifacts/remote-ci/NAME, with ci.exit, ci.log and the ci or premerge logs.
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
  rx_exec "$host" "o=\$HOME/gate/out/$1; cat \"\$o/progress.log\" 2> /dev/null; [ ! -f \"\$o/ci.exit\" ] || echo \"exit \$(cat \"\$o/ci.exit\")\"; systemctl --user is-active gate-$1 || true"
}
fetch() {
  local dest=$root/.artifacts/remote-ci/$1
  mkdir -p "$dest"
  rx_sync -- -a "$host:gate/out/$1/" "$dest/"
  echo "results in $dest"
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
  fetch "$name"
  exit 0
fi

full=$(git rev-parse --verify "$rev^{commit}")
if [ "$mode" = ci ]; then
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
    ;;
  *)
    git push -q "ssh://$host/home/$USER/gate/repo.git" "$full:refs/heads/ci-${full:0:8}" "$bfull:refs/heads/ci-base-${bfull:0:8}"
    end=$((now + 21600))
    executor=ssh
    ;;
esac
min=${REMOTE_CI_MIN_SECONDS:-600}
case ${REMOTE_CI_MIN_FREE_GIB:-85} in '' | *[!0-9]* | 0)
  echo "remote-ci.sh: REMOTE_CI_MIN_FREE_GIB must be a positive number of GiB" >&2
  exit 2
  ;;
esac
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
  [ -z \"\${!n+x}\" ] || forward+=(\"--setenv=\$n=\${!n}\")
done
p=\$PATH; [ ! -d \"\$G/py-venv/bin\" ] || p=\$G/py-venv/bin:\$p
systemd-run --user --unit gate-$name --collect -q \"\${forward[@]}\" \\
  -p MemoryMax=\${GATE_MEMORY_MAX:-13G} -p MemorySwapMax=2G -p OOMPolicy=continue -p RuntimeMaxSec=\$rt -p TimeoutStopSec=60 -p UMask=0022 \\
  --setenv=HOME=\"\$HOME\" --setenv=USER=\"\$USER\" --setenv=PATH=\"\$p\" --setenv=SPARKLES_REMOTE_EXECUTOR=$executor \\
  --setenv=CARGO_BUILD_JOBS=6 --setenv=CARGO_INCREMENTAL=0 \\
  --setenv=NEED_GIB=${REMOTE_CI_MIN_FREE_GIB:-85} --setenv=NAME=$name --setenv=FULL=$full --setenv=BASE=$bfull --setenv=MODE=$mode --setenv=END=$end \\
  /bin/bash \"\$o/ci-job.sh\"
echo \"started gate-$name, ending by \$(date -u -d @$end +%FT%TZ)\"" < "$here/gate/remote/ci-job.sh"
[ "$wait" = 1 ] || {
  echo "follow with: scripts/remote-ci.sh --host $host --status $name"
  exit 0
}
start=$(date +%s)
while :; do
  s=$(status "$name" 2> /dev/null || true)
  case $s in *"exit "[0-9]*) break ;; esac
  case $(printf '%s\n' "$s" | tail -n 1) in inactive | failed)
    printf '%s\n' "$s" | grep -q '^exit ' || {
      echo "gate-$name ended without an exit status" >&2
      fetch "$name"
      exit 1
    }
    ;;
  esac
  [ "$(date +%s)" -lt $((end + 120)) ] || {
    echo "gate-$name passed its end" >&2
    exit 1
  }
  sleep 30
done
fetch "$name"
code=$(cat "$root/.artifacts/remote-ci/$name/ci.exit")
echo "gate-$name exit $code in $(($(date +%s) - start)) s"
[ ! -f "$root/.artifacts/remote-ci/$name/ci.log" ] || tail -n 30 "$root/.artifacts/remote-ci/$name/ci.log"
[ -f "$root/.artifacts/remote-ci/$name/ci.log" ] || tail -n 5 "$root/.artifacts/remote-ci/$name/progress.log"
exit "$code"
