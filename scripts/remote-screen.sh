#!/usr/bin/env bash
# Shared screen provider boundary. Workload scripts and summaries remain unchanged.
# Boat workers are admitted once by the lifecycle owner and reused without starts.
# shellcheck source=scripts/remote-executor.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/remote-executor.sh"
EXECUTOR=nsc
SCREEN_UNIT=
SCREEN_LOCK=
SCREEN_RUN=0

screen_check() {
  case $EXECUTOR in
    nsc) command -v nsc > /dev/null || die 'nsc is not on PATH' ;;
    boat:*)
      command -v boat > /dev/null || die 'boat is not on PATH'
      python3 "$RX_DIR/remote-executor.py" admit "$EXECUTOR" --minimum "${SPARKLES_REMOTE_MIN_LIFETIME:-1800}" > /dev/null || die 'Boat admission failed'
      ;;
    *) die '--executor takes nsc or boat:bx_ID' ;;
  esac
}

screen_create() {
  local purpose=$1 ttl token dir
  if [ "$EXECUTOR" = nsc ]; then
    nsc create --bare --machine_type "$MACHINE" --duration "$DURATION" --purpose "$purpose" \
      --cidfile "$OUT/instance.id" --output_json_to "$OUT/instance.json" > "$OUT/logs/create.log" 2>&1 || {
      [ ! -s "$OUT/instance.id" ] || ID=$(cat "$OUT/instance.id")
      return 1
    }
    ID=$(cat "$OUT/instance.id")
    return
  fi
  rx_admit "$EXECUTOR" --minimum "${SPARKLES_REMOTE_MIN_LIFETIME:-1800}" > "$OUT/admission.json" || return
  ttl=$(python3 -c 'import json,sys,time; print(int(json.load(open(sys.argv[1]))["expires_at"]-time.time())-60)' "$OUT/admission.json")
  token=$(python3 -c 'import secrets;print(secrets.token_hex(12))')
  SCREEN_LOCK=sparkles-screen-lock-$token
  ID=${EXECUTOR#boat:}
  printf '%s\n' "$ID" > "$OUT/instance.id"
  printf '%s\n' "$SCREEN_LOCK" > "$OUT/lock-unit"
  printf '%s\n' "$token" > "$OUT/acquire-intent"
  # One lock covers the entire comparison, collection and rerun. FIFO readiness
  # acknowledges actual lock acquisition rather than systemd request submission.
  dir=$(rx_exec "$EXECUTOR" "set -e
    mkdir -p \"\$HOME/lanes/remote-screens\"
    d=\$(mktemp -d \"\$HOME/lanes/remote-screens/$token.XXXXXXXX\")
    mkfifo \"\$d/ready\"
    systemd-run --user --unit=$SCREEN_LOCK --collect -q -p RuntimeMaxSec=$ttl \
      bash -c 'exec 9>\"\$HOME/lanes/.heavy.lock\"; if ! flock -n 9; then echo busy > \"\$1/ready\"; exit 75; fi; echo ready > \"\$1/ready\"; exec sleep \"\$2\"' bash \"\$d\" $ttl
    test \"\$(timeout 15 cat \"\$d/ready\")\" = ready
    rm \"\$d/ready\"
    printf '%s\\n' \"\$d\"") || return
  [[ $dir =~ ^/[A-Za-z0-9._/-]+/lanes/remote-screens/$token\.[A-Za-z0-9]+$ ]] || return 97
  REMOTE=$dir
  ID=${EXECUTOR#boat:}
  printf '%s\n' "$ID" > "$OUT/instance.id"
  printf '%s\n' "$REMOTE" > "$OUT/remote-root"
  printf '%s\n' "$SCREEN_LOCK" > "$OUT/lock-unit"
  printf '{"provider":"boat","id":"%s","app_url":"warm worker; lifecycle owned externally"}\n' "$ID" > "$OUT/instance.json"
}

screen_exec() {
  if [ "$EXECUTOR" = nsc ]; then nsc ssh -T "$ID" -- "$@"; else rx_exec "$EXECUTOR" "$1"; fi
}
screen_upload() {
  if [ "$EXECUTOR" = nsc ]; then
    nsc instance upload "$ID" "$1" "$2" --mkdir
  else
    python3 "$RX_DIR/remote-executor.py" put "$EXECUTOR" "$1" "$2"
  fi
}
screen_download() {
  if [ "$EXECUTOR" = nsc ]; then
    nsc instance download "$ID" "$1" "$2"
  else
    python3 "$RX_DIR/remote-executor.py" get "$EXECUTOR" "$1" "$2"
  fi
}
screen_unpack() {
  python3 - "$RX_DIR" "$1" "$2" << 'PY'
import sys
sys.path.insert(0,sys.argv[1])
from importlib.machinery import SourceFileLoader
rx=SourceFileLoader('remote_executor',sys.argv[1]+'/remote-executor.py').load_module()
with open(sys.argv[2],'rb') as f:rx.extract(f,sys.argv[3])
PY
}
# Start once. A lost response is uncertain acceptance, never a reason to relaunch.
screen_launch() {
  local script=$1 logfile=$2 prepare=${3:-:} text
  text="cd $REMOTE && $prepare && exec bash $script > $logfile 2>&1"
  if [ "$EXECUTOR" = nsc ]; then
    screen_exec "cd $REMOTE && $prepare && setsid nohup bash $script > $logfile 2>&1 < /dev/null &"
  else
    SCREEN_RUN=$((SCREEN_RUN + 1))
    SCREEN_UNIT=${SCREEN_LOCK/lock-/run-}-$SCREEN_RUN
    printf '%s\n' "$SCREEN_UNIT" >> "$OUT/workload-units"
    screen_exec "systemd-run --user --unit=$SCREEN_UNIT --collect -q -p MemoryMax=12G -p MemorySwapMax=2G -p OOMPolicy=continue -p RuntimeMaxSec=$(python3 -c 'import json,sys,time;print(max(1,int(json.load(open(sys.argv[1]))["expires_at"]-time.time())-60))' "$OUT/admission.json") bash -c $(printf '%q' "$text")"
  fi
}
screen_destroy() {
  if [ "$EXECUTOR" = nsc ]; then
    [ -z "$ID" ] || nsc destroy --force "$ID"
    return
  fi
  # Stop each exact named workload and confirm quiescence before releasing its
  # host lock. Keep source/results for inspection, including interrupted runs.
  local unit failed=0
  if [ -f "$OUT/workload-units" ]; then
    while read -r unit; do
      screen_exec "systemctl --user stop $unit; ! systemctl --user is-active --quiet $unit" || failed=1
    done < "$OUT/workload-units"
  fi
  [ "$failed" = 0 ] || {
    echo "Boat workload outcome uncertain; retaining $SCREEN_LOCK and $REMOTE" >&2
    return 97
  }
  [ -z "$SCREEN_LOCK" ] || screen_exec "systemctl --user stop $SCREEN_LOCK; ! systemctl --user is-active --quiet $SCREEN_LOCK" || return
  echo "Boat worker $ID retained warm; owned results at $REMOTE"
}
