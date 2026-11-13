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
set -uo pipefail
umask 022
G=$HOME/gate
out=$G/out/$NAME
wt=$G/wt-$NAME
log() { echo "$(date -u +%FT%TZ) $*" >> "$out/progress.log"; }
finish() {
  echo "$1" > "$out/ci.exit"
  date +%s > "$out/done_at"
  log "end exit=$1"
  cd "$G" || exit "$1"
  rm -rf "$wt"
  git -C "$G/repo.git" worktree prune
  exit "$1"
}
mkdir -p "$out"
log "queued $MODE $FULL base ${BASE:-none} host $(hostname)"
if [ -x "$G/bin/disk_clean.py" ] || [ -f "$G/bin/disk_clean.py" ]; then
  python3 -I "$G/bin/disk_clean.py" --if-above 90 --deadline-seconds 240 --log "$G/disk-clean.jsonl" > "$out/disk-clean-start.json" 2>&1 || true
fi
exec 9> "$G/.lock"
log "waiting for the heavy lock"
flock 9
log "lock acquired"
date +%s > "$out/started_at"
git -C "$G/repo.git" worktree prune
rm -rf "$wt"
git -C "$G/repo.git" worktree add -q --detach "$wt" "$FULL" || finish 2
cd "$wt" || finish 2
mise trust -q . > /dev/null 2>&1 || true
left=$((END - $(date +%s) - 60))
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
