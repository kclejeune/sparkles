#!/usr/bin/env bash
# The crash runs of X09 sections 10.2 and 10.4 (docs/specs/X09-group-commit-by-default.md)
# on one file system, with a built `sparkles` and scripts/crash-harness.rs.
#
#   scripts/power-loss.sh logwrites --fs ext4|xfs --sparkles BIN --harness BIN --work DIR
#       [--points 1000] [--seconds 20] [--clients 16] [--jobs 8] [--flags "..."]
#   scripts/power-loss.sh kill9 --fs ext4|xfs|DIR --sparkles BIN --harness BIN --work DIR
#       [--kills 200] [--clients 16] [--flags "..."]
#
# logwrites needs root and the device-mapper log-writes target. It puts a file system on a
# loop device behind log-writes, loads a small store, serves it and runs the harness's
# 16 writers for --seconds with a mark in the log every 50 ms, then kills the server with
# SIGKILL. It replays --points crash images from the log, each holding the writes before
# its last flush, its FUA writes and a random part of the rest, and checks each with
# `crash-harness verify`: `sparkles check` passes, the head is at or after the last
# receipt acknowledged before the image's mark, every receipt up to the head has its triple
# and message, and the next commit continues the sequence.
#
# kill9 kills the server with SIGKILL at a random moment of a 16-writer load, --kills
# times, and after each restart checks every receipt logged so far in the store. It starts
# a new store every 20 kills, so that each check reads a bounded history. With --fs ext4 or xfs
# it needs root to make and mount the file system on a loop device. With --fs DIR it runs
# in that directory, on whatever file system holds it, without root.
#
#   scripts/power-loss.sh selftest --fs DIR --sparkles BIN --harness BIN --work DIR
#
# selftest needs no root. It checks that the driver fails a load whose writes the server
# refuses and passes a working one. kill9 fails a kill whose driver reports failed writes
# or stops with an error, and a store that took no receipt over its 20 kills. logwrites
# refuses to replay a load without receipts or with failed writes.
#
# --flags are global flags given to `sparkles` before `serve`, to run the crashes with
# and without group commit. Results go to DIR/results.tsv and DIR/summary.json, and the
# exit status is 0 only when every check passed.
set -euo pipefail

MODE=${1:-}
[ -n "$MODE" ] && shift
FS='' BIN='' HARNESS='' WORK='' POINTS=1000 KILLS=200 SECONDS_RUN=20 CLIENTS=16 JOBS=8 FLAGS=''
PORT=${PORT:-3931}
POS=()
while [ $# -gt 0 ]; do
  case $1 in
    --fs) FS=$2 ;;
    --sparkles) BIN=$(readlink -f "$2") ;;
    --harness) HARNESS=$(readlink -f "$2") ;;
    --work) WORK=$2 ;;
    --points) POINTS=$2 ;;
    --kills) KILLS=$2 ;;
    --seconds) SECONDS_RUN=$2 ;;
    --clients) CLIENTS=$2 ;;
    --jobs) JOBS=$2 ;;
    --flags) FLAGS=$2 ;;
    -*)
      echo "power-loss: unknown option $1" >&2
      exit 2
      ;;
    *)
      POS+=("$1")
      shift
      continue
      ;;
  esac
  shift 2
done
die() {
  echo "power-loss: $*" >&2
  exit 1
}
[ -x "$BIN" ] && [ -x "$HARNESS" ] && [ -n "$WORK" ] && [ -n "$FS" ] || die "--fs, --sparkles, --harness and --work are required"
mkdir -p "$WORK"
WORK=$(readlink -f "$WORK")
log() { echo "$(date -u +%H:%M:%S) $*" | tee -a "$WORK/progress.log"; }

mkfs_on() { # <fs> <device>
  case $1 in
    ext4) mkfs.ext4 -q -F -E lazy_itable_init=0,lazy_journal_init=0 "$2" ;;
    xfs) mkfs.xfs -q -f "$2" ;;
    *) die "unknown file system $1" ;;
  esac
}
mount_opts() { # xfs refuses two mounted copies with one UUID
  if [ "$1" = xfs ]; then echo "-o nouuid"; else echo ""; fi
}

seed() { # <file>: 2,000 triples to start from
  for i in $(seq 1 2000); do echo "<urn:x09:seed:$i> <urn:x09:p> \"seed $i\" ."; done > "$1"
}

SPID=
start_server() { # <store>
  # the test file systems are small, so the server keeps only 64 MiB free instead of 1 GiB
  # shellcheck disable=SC2086 # the flags are a command line
  "$BIN" $FLAGS serve --data "$WORK/srv" --loc "ds=$1" --port "$PORT" --no-access-log \
    --min-free-disk-mb "${MIN_FREE_MB:-64}" > "$WORK/server.log" 2>&1 &
  SPID=$!
  for _ in $(seq 1 300); do
    curl -sf "http://127.0.0.1:$PORT/\$/ping" > /dev/null 2>&1 && return 0
    kill -0 "$SPID" 2> /dev/null || die "the server exited, see $WORK/server.log"
    sleep 0.1
  done
  die "the server did not start"
}
kill_server() {
  [ -n "$SPID" ] && kill -9 "$SPID" 2> /dev/null || true
  [ -n "$SPID" ] && wait "$SPID" 2> /dev/null || true
  SPID=
}

# --------------------------------------------------------------- check-image ------
# Internal: mount one crash image and verify the store in it.
if [ "$MODE" = check-image ]; then
  img=${POS[0]} mark=${POS[1]}
  mnt=$(mktemp -d "$WORK/mnt.XXXXXX")
  # shellcheck disable=SC2046
  if ! mount -o loop $(mount_opts "$FS") "$img" "$mnt" 2> "$mnt.err"; then
    echo "{\"ok\": false, \"problems\": [\"mount failed: $(tr '\n' ' ' < "$mnt.err")\"]}"
    rmdir "$mnt"
    exit 1
  fi
  rc=0
  if [ -d "$mnt/db" ]; then
    "$HARNESS" verify --sparkles "$BIN" --loc "$mnt/db" --receipts "$WORK/receipts.txt" --mark "$mark" --write || rc=$?
  elif [ "$mark" = start ] || [ "$mark" = - ]; then
    echo '{"ok": true, "note": "before the store existed"}'
  else
    echo '{"ok": false, "problems": ["the store is missing"]}'
    rc=1
  fi
  umount "$mnt" && rmdir "$mnt"
  rm -f "$mnt.err"
  exit $rc
fi

cleanup() {
  kill_server
  if [ -n "${MNT:-}" ] && mountpoint -q "$MNT"; then umount "$MNT" || true; fi
  if [ -n "${DM:-}" ]; then dmsetup remove "$DM" 2> /dev/null || true; fi
  for d in ${LOOPS:-}; do losetup -d "$d" 2> /dev/null || true; done
}
trap cleanup EXIT

case $MODE in
  logwrites)
    [ "$(id -u)" = 0 ] || die "logwrites needs root"
    modprobe dm-log-writes 2> /dev/null || true
    log "logwrites on $FS: $POINTS points, $SECONDS_RUN s of $CLIENTS writers, flags '$FLAGS'"
    truncate -s 512M "$WORK/data.img"
    truncate -s 16G "$WORK/log.img"
    LD=$(losetup -f --show "$WORK/data.img")
    LL=$(losetup -f --show "$WORK/log.img")
    LOOPS="$LD $LL"
    DM=x09lw$$
    dmsetup create "$DM" --table "0 $(blockdev --getsz "$LD") log-writes $LD $LL"
    mkfs_on "$FS" "/dev/mapper/$DM"
    MNT=$WORK/mnt
    mkdir -p "$MNT" "$WORK/srv"
    mount "/dev/mapper/$DM" "$MNT"
    seed "$WORK/seed.nt"
    "$BIN" load --loc "$MNT/db" "$WORK/seed.nt" > "$WORK/load.log" 2>&1
    sync
    # images at the start mark need no receipt yet
    echo "M start 0" > "$WORK/receipts.txt"
    dmsetup message "$DM" 0 mark start
    start_server "$MNT/db"
    drc=0
    "$HARNESS" drive --addr "127.0.0.1:$PORT" --ds ds --receipts "$WORK/receipts.txt" \
      --clients "$CLIENTS" --seconds "$SECONDS_RUN" --mark-cmd "dmsetup message $DM 0 mark" \
      > "$WORK/drive.json" || drc=$?
    cat "$WORK/drive.json"
    # a load without receipts, or with failed writes, would make every image pass vacuously
    [ "$drc" = 0 ] || die "the driver failed with status $drc: $(cat "$WORK/drive.json")"
    kill_server
    umount "$MNT"
    dmsetup remove "$DM"
    DM=
    for d in $LOOPS; do losetup -d "$d"; done
    LOOPS=
    log "replaying"
    rc=0
    "$HARNESS" replay --log "$WORK/log.img" --size "$(stat -c %s "$WORK/data.img")" --out "$WORK/replay" \
      --points "$POINTS" --from-mark start --jobs "$JOBS" --seed "${SEED:-$$}" \
      -- "$0" check-image --fs "$FS" --sparkles "$BIN" --harness "$HARNESS" --work "$WORK" > "$WORK/summary.json" || rc=$?
    cp "$WORK/replay/results.tsv" "$WORK/results.tsv"
    rm -f "$WORK/log.img" "$WORK/data.img"
    log "done: $(cat "$WORK/summary.json")"
    exit $rc
    ;;
  kill9)
    if [ -d "$FS" ]; then
      MNT=$(readlink -f "$FS")/x09-kill9-$$
      mkdir -p "$MNT"
      MNT_OWNED=0
    else
      [ "$(id -u)" = 0 ] || die "kill9 on a new $FS file system needs root"
      truncate -s 2G "$WORK/data.img"
      mkfs_on "$FS" "$WORK/data.img"
      MNT=$WORK/mnt
      mkdir -p "$MNT"
      mount -o loop "$WORK/data.img" "$MNT"
      MNT_OWNED=1
    fi
    mkdir -p "$WORK/srv"
    log "kill9 in $MNT: $KILLS kills of $CLIENTS writers, flags '$FLAGS'"
    seed "$WORK/seed.nt"
    : > "$WORK/results.tsv"
    fails=0 total=0
    for i in $(seq 1 "$KILLS"); do
      # a new store every 20 kills, so a check reads a bounded history
      n=$(((i - 1) / 20))
      db=$MNT/db-$n receipts=$WORK/receipts-$n.txt
      if [ ! -d "$db" ]; then
        "$BIN" load --loc "$db" "$WORK/seed.nt" > "$WORK/load.log" 2>&1
        : > "$receipts"
      fi
      start_server "$db"
      "$HARNESS" drive --addr "127.0.0.1:$PORT" --ds ds --receipts "$receipts" \
        --clients "$CLIENTS" --seconds 30 --run "k$i" > "$WORK/drive.json" 2> "$WORK/drive.err" &
      DPID=$!
      # a random moment between 0.2 and 2.2 seconds into the load
      sleep "$(awk -v r="$RANDOM" 'BEGIN { printf "%.3f", 0.2 + (r % 2000) / 1000 }')"
      kill_server
      # The driver ends on its own when the server dies. Status 3 with failed writes, or
      # any other failure, fails the kill. Status 3 without receipts only means the kill
      # came before the first acknowledgement, which the per-store check below bounds.
      drc=0
      wait "$DPID" || drc=$?
      dfail=$(sed -n 's/.*"failed": \([0-9]*\).*/\1/p' "$WORK/drive.json")
      if [ "$drc" = 3 ] && [ "${dfail:-0}" = 0 ]; then
        log "kill $i: no receipt before the kill"
      elif [ "$drc" != 0 ]; then
        log "kill $i: the driver failed with status $drc: $(cat "$WORK/drive.json" "$WORK/drive.err")"
        fails=$((fails + 1))
      fi
      # every receipt logged in this store so far, after each restart
      out=$("$HARNESS" verify --sparkles "$BIN" --loc "$db" --receipts "$receipts") || fails=$((fails + 1))
      printf '%s\t%s\n' "$i" "$out" >> "$WORK/results.tsv"
      [ $((i % 20)) = 0 ] && log "kill $i: $out"
    done
    # every store must have taken writes, or its checks verified nothing
    for f in "$WORK"/receipts-*.txt; do
      n=$(grep -c '^R ' "$f" || true)
      if [ "$n" = 0 ]; then
        log "$f: no receipts in the whole store"
        fails=$((fails + 1))
      fi
    done
    total=$(cat "$WORK"/receipts-*.txt | grep -c '^R ' || true)
    echo "{\"kills\": $KILLS, \"failed\": $fails, \"receipts\": $total}" | tee "$WORK/summary.json"
    if [ "$MNT_OWNED" = 1 ]; then
      umount "$MNT"
      rm -f "$WORK/data.img"
      MNT=
    else rm -rf "$MNT"; fi
    [ "$fails" = 0 ]
    ;;
  selftest)
    # The driver must fail a run in which the server refuses every write, as the 507s of a
    # free-space reserve larger than the file system did once, and pass a working one.
    [ -d "$FS" ] || die "selftest needs --fs DIR"
    db=$(readlink -f "$FS")/x09-selftest-$$
    mkdir -p "$WORK/srv"
    seed "$WORK/seed.nt"
    "$BIN" load --loc "$db" "$WORK/seed.nt" > "$WORK/load.log" 2>&1
    MIN_FREE_MB=1000000000 start_server "$db"
    drc=0
    timeout 60 "$HARNESS" drive --addr "127.0.0.1:$PORT" --ds ds --receipts "$WORK/selftest-refused.txt" \
      --clients 2 --seconds 2 > "$WORK/drive.json" 2> "$WORK/drive.err" || drc=$?
    kill_server
    [ "$drc" != 0 ] || die "the driver passed a run in which every write was refused: $(cat "$WORK/drive.json")"
    log "refused writes: driver status $drc, as it should be"
    start_server "$db"
    drc=0
    timeout 60 "$HARNESS" drive --addr "127.0.0.1:$PORT" --ds ds --receipts "$WORK/selftest-ok.txt" \
      --clients 2 --seconds 2 > "$WORK/drive.json" 2> "$WORK/drive.err" || drc=$?
    kill_server
    rm -rf "$db"
    [ "$drc" = 0 ] || die "the driver failed a working run with status $drc: $(cat "$WORK/drive.json" "$WORK/drive.err")"
    log "working run: $(cat "$WORK/drive.json")"
    ;;
  *)
    sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
    exit 2
    ;;
esac
