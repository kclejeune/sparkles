#!/usr/bin/env bash
# Runs inside the systemd user unit gate-NAME on a gate host or Boat worker, started by
# scripts/remote-ci.sh. Everything the job does happens inside the unit, its preparation
# included, so the unit's RuntimeMaxSec ends all of it, and a cleanup of finished work
# (scripts/boat/disk_clean.py) never takes its worktree while the unit is active.
#
# The unit's environment names the job: NAME, FULL (the commit), BASE (the base commit for
# ci), MODE (ci, premerge or cmd), CMD_B64 (the base64 of a cmd job's command) and END (Unix
# seconds by which the job has ended). It holds
# ~/gate/.lock, the host's heavy lock, while it builds, so one heavy job runs at a time and
# others wait in their own units.
#
# The unit's RuntimeMaxSec counts from its activation, so the job also bounds itself by END.
# A watchdog stops the unit at END, every wait and step takes its limit from END, and the
# unit's TimeoutStopSec bounds the shutdown. The worktree is created under the cleanup's
# lock ~/gate/.disk-clean.lock, so a cleanup whose list of active units predates this unit
# has finished before the worktree exists, and a later one sees the unit.
#
# A job that ends for want of disk writes disk-below-floor to $out/outcome, and its exit is
# 4 at admission and 5 during the run. Under the heavy lock the job needs NEED_GIB free. A
# Boat worker frees finished work with scripts/boat/disk_clean.py first. An ssh host runs
# `mbx gc` with gc.min_free_size raised to NEED_GIB, which evicts managed targets and
# cache objects until that much is free, and removes nothing else. On an ssh host a watch
# reads the free space every WATCH_SECONDS while the run goes, 30 by default, and once more
# when a failed run ends, and stops or marks the run when it is under RUN_FLOOR_GIB, 4 by
# default, so tests do not fail against the server's own free-disk reserve for a reason
# that is not theirs. The run's TMPDIR there is .artifacts/tmp in the job's worktree, so
# its temporary files go with it.
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
  [ -z "${dw:-}" ] || kill "$dw" 2> /dev/null
  [ -z "${wd:-}" ] || kill "$wd" 2> /dev/null
  exit "$1"
}
until_end() { echo $((END - $(date +%s) - $1)); }
# free_kib prints the KiB free on the filesystem of the home, or nothing when df fails.
free_kib() {
  local f
  f=$(df -Pk "$HOME" 2> /dev/null | awk 'NR == 2 { print $4 }')
  case $f in '' | *[!0-9]*) ;; *) echo "$f" ;; esac
}
# ssh_admit NEED_GIB SECONDS: succeed when NEED_GIB are free, after an `mbx gc` bounded by
# SECONDS that frees the shortfall when they are not. An unreadable disk counts as full.
ssh_admit() {
  local need=$1 f c
  f=$(free_kib)
  [ "${f:-0}" -lt $((need * 1048576)) ] || return 0
  log "$((${f:-0} / 1048576)) GiB free and the job needs $need GiB, so mbx gc frees the shortfall"
  MBX_GC_MIN_FREE_SIZE=${need}GiB timeout --kill-after=10s "$2" mbx gc > "$out/gc-admit.log" 2>&1
  c=$?
  [ "$c" = 0 ] || log "mbx gc exited $c"
  f=$(free_kib)
  [ "${f:-0}" -lt $((need * 1048576)) ] || return 0
  log "host disk below floor: $((${f:-0} / 1048576)) GiB free after mbx gc, under the $need GiB floor; not starting"
  return 1
}
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
need=${NEED_GIB:-85}
if [ "${SPARKLES_REMOTE_EXECUTOR:-ssh}" = boat ]; then
  if [ ! -f "$G/bin/disk_clean.py" ]; then
    log "no ~/gate/bin/disk_clean.py for the admission check; run scripts/gate/setup-host.sh"
    finish 4
  fi
  if ! python3 -I "$G/bin/disk_clean.py" --need-free-gib "$need" --deadline-seconds 240 --log "$G/disk-clean.jsonl" > "$out/disk-clean-admit.json" 2>&1; then
    log "host disk below floor: under $need GiB free after the cleanup; not starting"
    echo disk-below-floor > "$out/outcome"
    finish 4
  fi
else
  w=$(until_end 600)
  [ "$w" -le 240 ] || w=240
  if [ "$w" -le 0 ]; then
    log "too little time is left before the job's end for the disk check and the run"
    finish 3
  fi
  if ! ssh_admit "$need" "$w"; then
    echo disk-below-floor > "$out/outcome"
    finish 4
  fi
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
  ci) run=(scripts/ci.sh --fast --changed "$BASE") ;;
  premerge) run=(scripts/premerge.sh) ;;
  cmd)
    # A lane's command runs with the commit's pinned compiler and the tools of its
    # mise.toml, and its log names both, so it matches a local run of the same command.
    cmd=$(printf '%s' "${CMD_B64:-}" | base64 -d) || finish 2
    [ -n "$cmd" ] || finish 2
    tc=$(sed -n 's/^channel *= *"\([^"]*\)".*/\1/p' rust-toolchain.toml 2> /dev/null | head -n 1)
    [ -z "$tc" ] || export RUSTUP_TOOLCHAIN=$tc
    # shellcheck disable=SC2016
    run=(mise exec -- bash -c 'echo "remote-ci: $1"; echo "remote-ci: RUSTUP_TOOLCHAIN=${RUSTUP_TOOLCHAIN:-unset}"; echo "remote-ci: $(rustc --version 2>&1)"; echo "remote-ci: $(mbx --version 2>&1)"; exec bash -c "$1"' remote-ci "$cmd")
    log "running the command with ${RUSTUP_TOOLCHAIN:-the default toolchain}"
    ;;
  *) finish 2 ;;
esac
# On an ssh host, where other lanes' jobs share the disk, the run's temporary files live in
# its worktree and a watch stops the run when the disk falls under the run floor. A Boat
# worker keeps its own timer's hard limit instead.
watch=0
if [ "${SPARKLES_REMOTE_EXECUTOR:-ssh}" = ssh ]; then
  watch=1
  export TMPDIR=$wt/.artifacts/tmp
  mkdir -p "$TMPDIR"
fi
floor_kib=$((${RUN_FLOOR_GIB:-4} * 1048576))
# below_floor NOTE: record the outcome and say so when the disk is under the run floor.
below_floor() {
  local f
  f=$(free_kib)
  [ "${f:-0}" -lt "$floor_kib" ] || return 1
  echo disk-below-floor > "$out/outcome"
  log "host disk below floor: $((${f:-0} / 1024)) MiB free $1, under the ${RUN_FLOOR_GIB:-4} GiB floor"
}
# timeout leads its own process group and passes a TERM it receives to the whole group, so
# the disk watch stops the run by signalling timeout.
timeout --signal=TERM --kill-after=30s "$left" "${run[@]}" > "$out/ci.log" 2>&1 &
cpid=$!
if [ "$watch" = 1 ]; then
  (
    trap 'kill "$ds" 2> /dev/null; exit 0' TERM
    ds=
    while kill -0 "$cpid" 2> /dev/null; do
      if below_floor "during the run"; then
        log "stopping the run"
        kill -TERM "$cpid" 2> /dev/null
        exit 0
      fi
      sleep "${WATCH_SECONDS:-30}" &
      ds=$!
      wait "$ds"
    done
  ) &
  dw=$!
fi
wait "$cpid"
c=$?
[ -z "${dw:-}" ] || kill "$dw" 2> /dev/null
[ -z "${dw:-}" ] || wait "$dw" 2> /dev/null
dw=
# timeout exits once its own child has, so a descendant that ignored the TERM can still be
# in its group. The group is killed before the results are copied and the worktree removed.
if kill -0 -- "-$cpid" 2> /dev/null; then
  kill -TERM -- "-$cpid" 2> /dev/null
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    kill -0 -- "-$cpid" 2> /dev/null || break
    sleep 1
  done
  kill -KILL -- "-$cpid" 2> /dev/null
  log "ended the processes the run left in its process group"
fi
# A disk that fell under the floor between the last sample and the run's end still marks it.
[ "$watch" = 0 ] || [ -f "$out/outcome" ] || [ "$c" = 0 ] || below_floor "when the run ended" || true
[ ! -f "$out/outcome" ] || c=5
for d in ci premerge; do [ ! -d ".artifacts/$d" ] || cp -r ".artifacts/$d" "$out/$d"; done
finish "$c"
