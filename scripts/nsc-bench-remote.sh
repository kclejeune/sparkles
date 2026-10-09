#!/usr/bin/env bash
# The remote half of scripts/nsc-bench.sh: runs on the Namespace instance, in the directory
# that holds it, with bash, busybox, curl and zstd only.
#
#   bash nsc-bench-remote.sh   (settings come from ./run.env)
#
# Expects bin-A.zst and bin-B.zst (the two binaries), data.nt.zst and queries/*.rq next to
# it. Both binaries load their own store from the same data, so a change of the store format
# between A and B does no harm. Then ROUNDS rounds of A, B, B, A run, each variant in a fresh
# server process pinned to SERVER_CPU, with this script and its curl client pinned to
# CLIENT_CPU. In each server process every query gets WARMUP untimed requests and REPS timed
# ones. results/samples.tsv gets one line per timed request with the server's execution time
# (execMs of application/x-sparkles+json, unrounded) and curl's wall time. results/answers.tsv
# gets a checksum of each variant's sorted TSV answer to each query, and results/load.tsv the
# load times. Progress goes to results/progress.log and results/done marks the end.
#
# With SPIN=1 (the default), a busy loop at the lowest priority (SCHED_IDLE) runs on the
# server and client CPUs during the timed part. It keeps their vCPUs from halting between
# requests, which in a VM costs a hypervisor exit and a cold wake-up, and it yields to the
# server and client as soon as they run. In a noise test with one binary, it halved the
# spread within a server process and between server processes.
set -euo pipefail
cd "$(dirname "$0")"
# shellcheck source=/dev/null
source ./run.env
: "${ROUNDS:=4}" "${REPS:=20}" "${WARMUP:=3}" "${SERVER_CPU:=2}" "${CLIENT_CPU:=4}"
: "${RAYON_THREADS:=1}" "${PORT:=3999}" "${SERVE_ARGS:=}" "${MAX_TIME:=300}" "${SPIN:=1}"
R=results
mkdir -p "$R"
# this script, its curl client and the log writer run on the client CPU, each server on its
# own and the loads on all of them
NCPU=$(grep -c ^processor /proc/cpuinfo)
ALL=0-$((NCPU - 1))
taskset -cp "$CLIENT_CPU" $$ > /dev/null
exec > >(tee -a "$R/progress.log") 2>&1
log() { echo "$(date -u +%H:%M:%S) $*"; }
fail() {
  log "FAILED: $*"
  echo "failed: $*" > "$R/done"
  exit 1
}
trap 'fail "line $LINENO"' ERR

# The binaries are built on another Linux and name the build host's glibc loader, so a
# dynamically linked one (DYNAMIC_A, DYNAMIC_B) runs under this image's loader.
# BIN[v] is the command that runs variant v.
declare -A BIN
for v in A B; do
  d=DYNAMIC_$v
  if [ "${!d:-1}" = 1 ]; then BIN[$v]="/lib64/ld-linux-x86-64.so.2 $v/sparkles"; else BIN[$v]=$v/sparkles; fi
done

{
  echo "kernel: $(uname -r)"
  echo "cpu: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//')"
  echo "cpus: $NCPU"
  echo "memory: $(free -m | awk '/Mem:/ {print $2}') MiB"
  echo "smt siblings: $(sort -u /sys/devices/system/cpu/cpu*/topology/thread_siblings_list | tr '\n' ' ')"
  echo "loader: $(/lib64/ld-linux-x86-64.so.2 --version | head -1)"
} > "$R/env.txt"
# the steal and total jiffies of the whole machine: summed before and after the timed part
stat_cpu() { awk '/^cpu / {t = 0; for (i = 2; i <= NF; i++) t += $i; print $9, t}' /proc/stat; }

log "unpacking"
for v in A B; do
  mkdir -p "$v"
  zstd -q -d -f "bin-$v.zst" -o "$v/sparkles"
  chmod +x "$v/sparkles"
done
zstd -q -d -f data.nt.zst -o data.nt
log "data: $(wc -l < data.nt) triples"

: > "$R/load.tsv"
for v in A B; do
  rm -rf "stores/$v"
  mkdir -p stores
  t0=$EPOCHREALTIME
  # shellcheck disable=SC2086 # BIN is a command line
  taskset -c "$ALL" ${BIN[$v]} load --loc "stores/$v" data.nt > "$R/load-$v.log" 2>&1 || fail "load $v, see load-$v.log"
  t1=$EPOCHREALTIME
  printf '%s\t%s\t%s\n' "$v" "$(awk "BEGIN {print $t1 - $t0}")" "$(du -sk "stores/$v" | cut -f1)" >> "$R/load.tsv"
  log "$v loaded in $(awk "BEGIN {printf \"%.2f\", $t1 - $t0}") s"
done

mapfile -t QUERIES < <(cd queries && for f in *.rq; do echo "${f%.rq}"; done)
log "queries: ${QUERIES[*]}"

URL=localhost:$PORT/bench/sparql
SPID=
start_server() { # <variant> <epoch>
  local env=()
  [ "$RAYON_THREADS" != default ] && env=(RAYON_NUM_THREADS="$RAYON_THREADS")
  rm -rf "srv"
  # shellcheck disable=SC2086 # BIN is a command line and SERVE_ARGS a list of flags
  taskset -c "$SERVER_CPU" env "${env[@]}" ${BIN[$1]} --result-cache-mb 0 serve --data srv \
    --loc bench="stores/$1" --port "$PORT" --timeout 600 --no-access-log $SERVE_ARGS > "$R/server-$2-$1.log" 2>&1 &
  SPID=$!
  for _ in $(seq 300); do
    curl -sf -o /dev/null "localhost:$PORT/\$/ping" && return
    kill -0 "$SPID" 2> /dev/null || fail "server $1 exited, see server-$2-$1.log"
    sleep 0.1
  done
  fail "server $1 did not start"
}
stop_server() {
  kill "$SPID"
  wait "$SPID" 2> /dev/null || true
  SPID=
}
trap '[ -n "$SPID" ] && kill "$SPID" 2> /dev/null; [ ${#SPINNERS[@]} -eq 0 ] || kill "${SPINNERS[@]}" 2> /dev/null; true' EXIT
SPINNERS=()

request() { # <query> -> "execMs wallSeconds", or "error"
  local w e
  if ! w=$(curl -sS -f --max-time "$MAX_TIME" -o resp.json -w '%{time_total}' -H 'Accept: application/x-sparkles+json' \
    --data-urlencode "query@queries/$1.rq" "$URL" 2> /dev/null); then
    echo error
    return
  fi
  e=$(grep -o '"execMs":[0-9.eE+-]*' resp.json | head -1 | cut -d: -f2)
  echo "${e:-error} $w"
}

SPINNERS=()
if [ "$SPIN" = 1 ]; then
  for c in "$SERVER_CPU" "$CLIENT_CPU"; do
    taskset -c "$c" chrt -i 0 sh -c 'while :; do :; done' > /dev/null 2>&1 &
    SPINNERS+=($!)
  done
fi
printf 'round\tepoch\tvariant\tquery\trep\texec_ms\twall_ms\n' > "$R/samples.tsv"
: > "$R/answers.tsv"
read -r steal0 total0 < <(stat_cpu)
epoch=0
checked=" "
for round in $(seq "$ROUNDS"); do
  for v in A B B A; do
    epoch=$((epoch + 1))
    start_server "$v" "$epoch"
    if [[ $checked != *" $v "* ]]; then
      checked+="$v "
      for q in "${QUERIES[@]}"; do
        if curl -sS -f --max-time "$MAX_TIME" -o ans.tsv -H 'Accept: text/tab-separated-values' \
          --data-urlencode "query@queries/$q.rq" "$URL" 2> /dev/null; then
          printf '%s\t%s\t%s\t%s\n' "$v" "$q" "$(($(wc -l < ans.tsv) - 1))" "$(sort ans.tsv | sha256sum | cut -c1-16)" >> "$R/answers.tsv"
        else
          printf '%s\t%s\terror\terror\n' "$v" "$q" >> "$R/answers.tsv"
        fi
      done
    fi
    for q in "${QUERIES[@]}"; do
      for _ in $(seq "$WARMUP"); do request "$q" > /dev/null; done
      for rep in $(seq "$REPS"); do
        read -r e w <<< "$(request "$q")"
        if [ "$e" = error ]; then
          printf '%s\t%s\t%s\t%s\t%s\terror\terror\n' "$round" "$epoch" "$v" "$q" "$rep" >> "$R/samples.tsv"
        else
          printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$round" "$epoch" "$v" "$q" "$rep" "$e" "$(awk "BEGIN {print $w * 1000}")" >> "$R/samples.tsv"
        fi
      done
    done
    stop_server
    log "round $round epoch $epoch ($v) done"
  done
done
read -r steal1 total1 < <(stat_cpu)
[ ${#SPINNERS[@]} -eq 0 ] || kill "${SPINNERS[@]}"
SPINNERS=()
echo "steal during the timed part: $(awk "BEGIN {printf \"%.2f%%\", 100 * ($steal1 - $steal0) / ($total1 - $total0)}")" >> "$R/env.txt"
log "finished"
echo ok > "$R/done"
