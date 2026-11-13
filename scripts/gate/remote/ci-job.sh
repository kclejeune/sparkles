#!/usr/bin/env bash
# Runs inside the systemd user unit gate-NAME on a gate host or Boat worker, started by
# scripts/remote-ci.sh. Everything the job does happens inside the unit, its preparation
# included, so the unit's RuntimeMaxSec ends all of it, and a cleanup of finished work
# (scripts/boat/disk_clean.py) never takes its worktree while the unit is active.
#
# The unit's environment names the job: NAME, FULL (the commit), BASE (the base commit for
# ci), MODE (ci or premerge) and END (Unix seconds by which the job has ended). It holds
# ~/gate/.lock, the host's heavy lock, while it builds, so one heavy job runs at a time and
# others wait in their own units.
#
# The unit's RuntimeMaxSec counts from its activation, so the job also bounds itself by END.
# A watchdog stops the unit at END, every wait and step takes its limit from END, and the
# unit's TimeoutStopSec bounds the shutdown. The worktree is created under the cleanup's
# lock ~/gate/.disk-clean.lock, so a cleanup whose list of active units predates this unit
# has finished before the worktree exists, and a later one sees the unit.
set -uo pipefail
umask 022
G=$HOME/gate
out=$G/out/$NAME
wt=$G/wt-$NAME
log() { echo "$(date -u +%FT%TZ) $*" >> "$out/progress.log"; }
# The watchdog stays up through the cleanup, which is bounded too, and finish() ends it last.
finish() {
  echo "$1" > "$out/ci.exit"
  date +%s > "$out/done_at"
  log "end exit=$1"
  if cd "$G"; then
    timeout --kill-after=10s 120 rm -rf "$wt"
    timeout --kill-after=10s 60 git -C "$G/repo.git" worktree prune
  fi
  [ -z "${wd:-}" ] || kill "$wd" 2> /dev/null
  exit "$1"
}
until_end() { echo $((END - $(date +%s) - $1)); }
mkdir -p "$out"
log "queued $MODE $FULL base ${BASE:-none} host $(hostname)"
# A job whose end has passed has created nothing yet, so it records its exit and stops.
[ "$(until_end 120)" -gt 0 ] || {
  log "the job's end has passed"
  echo 3 > "$out/ci.exit"
  date +%s > "$out/done_at"
  exit 3
}
# The watchdog stops the unit at END whatever the job is doing. finish() ends it, and when
# the job's main process exits systemd ends the rest of the unit's processes with it.
(
  trap 'kill "$s" 2> /dev/null; exit 0' TERM
  sleep "$(until_end 0)" &
  s=$!
  wait "$s"
  echo "$(date -u +%FT%TZ) the job's end passed; stopping gate-$NAME" >> "$out/progress.log"
  systemctl --user stop --no-block "gate-$NAME"
) &
wd=$!
# A stop of the unit, by its watchdog, the disk cleanup's hard limit or an operator, still
# records an exit and removes the worktree.
trap 'log "stopped by a signal"; finish 143' TERM
if [ -x "$G/bin/disk_clean.py" ] || [ -f "$G/bin/disk_clean.py" ]; then
  python3 -I "$G/bin/disk_clean.py" --if-above 90 --deadline-seconds 240 --log "$G/disk-clean.jsonl" > "$out/disk-clean-start.json" 2>&1 || true
fi
exec 9> "$G/.lock"
log "waiting for the heavy lock"
w=$(until_end 120)
if [ "$w" -le 0 ] || ! flock -w "$w" 9; then
  log "the heavy lock did not come free before the job's end"
  finish 3
fi
log "lock acquired"
# The admission check runs under the heavy lock, so no other job's build grows meanwhile. A
# disk with under NEED_GIB free after the cleanup refuses the job rather than fill up.
need=${NEED_GIB:-110}
if [ -f "$G/bin/disk_clean.py" ] &&
  ! python3 -I "$G/bin/disk_clean.py" --need-free-gib "$need" --deadline-seconds 240 --log "$G/disk-clean.jsonl" > "$out/disk-clean-admit.json" 2>&1; then
  log "under $need GiB free after the cleanup; not starting"
  finish 4
fi
date +%s > "$out/started_at"
exec 7>> "$G/.disk-clean.lock"
flock -w 300 7 || {
  log "a disk cleanup held its lock for 300 s"
  finish 3
}
git -C "$G/repo.git" worktree prune
rm -rf "$wt"
timeout 300 git -C "$G/repo.git" worktree add -q --detach "$wt" "$FULL" || finish 2
exec 7>&-
cd "$wt" || finish 2
mise trust -q . > /dev/null 2>&1 || true
left=$(until_end 60)
[ "$left" -gt 60 ] || {
  log "only $left s left before the job's end"
  finish 3
}
case $MODE in
  ci) timeout --signal=TERM --kill-after=30s "$left" scripts/ci.sh --fast --changed "$BASE" > "$out/ci.log" 2>&1 ;;
  premerge) timeout --signal=TERM --kill-after=30s "$left" scripts/premerge.sh > "$out/ci.log" 2>&1 ;;
  *) finish 2 ;;
esac
c=$?
for d in ci premerge; do [ ! -d ".artifacts/$d" ] || cp -r ".artifacts/$d" "$out/$d"; done
finish "$c"
