#!/usr/bin/env bash
# The remote half of scripts/nsc-writes.sh: runs on the Namespace instance, in the directory
# that holds it, with bash, busybox, curl and zstd only.
#
#   bash nsc-writes-remote.sh   (settings come from ./run.env)
#
# Expects bin-<id>.zst (the binaries), data.nt.zst and write-bench next to it. Each binary
# loads its own store from the same data. Then ROUNDS rounds run, in which every variant of
# VARIANTS runs every scenario of SCENARIOS once, each run in a fresh server process pinned
# to SERVER_CPUS on a fresh copy of the variant's store, with the client pinned to
# CLIENT_CPUS. The order of the variants alternates between rounds, and so does the order
# of the scenarios. results/runs.tsv gets one line per run: the round, the variant, the
# scenario and write-bench's JSON. results/metrics-<run>.txt keeps the server's commit
# counters after the run. results/progress.log has the progress and results/done marks the
# end.
set -euo pipefail
cd "$(dirname "$0")"
# shellcheck source=/dev/null
source ./run.env
: "${PORT:=3999}"
R=results
mkdir -p "$R"
NCPU=$(grep -c ^processor /proc/cpuinfo)
ALL=0-$((NCPU - 1))
taskset -cp "${CLIENT_CPUS}" $$ > /dev/null
exec > >(tee -a "$R/progress.log") 2>&1
log() { echo "$(date -u +%H:%M:%S) $*"; }
fail() {
  log "FAILED: $*"
  echo "failed: $*" > "$R/done"
  exit 1
}
trap 'fail "line $LINENO"' ERR

loader() { # <dynamic flag> <path>
  if [ "$1" = 1 ]; then echo "/lib64/ld-linux-x86-64.so.2 $2"; else echo "$2"; fi
}
{
  echo "kernel: $(uname -r)"
  echo "cpu: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//')"
  echo "cpus: $NCPU"
  echo "memory: $(free -m | awk '/Mem:/ {print $2}') MiB"
  echo "loader: $(/lib64/ld-linux-x86-64.so.2 --version | head -1)"
  echo "root file system: $(df -T / | awk 'NR==2 {print $2, $1}')"
} > "$R/env.txt"

log "unpacking"
for f in bin-*.zst; do
  id=${f#bin-}
  id=${id%.zst}
  mkdir -p "$id"
  zstd -q -d -f "$f" -o "$id/sparkles"
  chmod +x "$id/sparkles"
done
zstd -q -d -f data.nt.zst -o data.nt
chmod +x write-bench
log "data: $(wc -l < data.nt) triples"

declare -A LOADED
mkdir -p stores
for v in $VARIANTS; do
  b=V_${v}_BIN
  b=${!b}
  d=DYNAMIC_$b
  if [ -z "${LOADED[$b]:-}" ]; then
    LOADED[$b]=1
    rm -rf "stores/$b"
    # shellcheck disable=SC2046 # the loader is a command line
    taskset -c "$ALL" $(loader "${!d}" "$b/sparkles") load --loc "stores/$b" data.nt > "$R/load-$b.log" 2>&1 || fail "load $b, see load-$b.log"
    log "$b loaded"
  fi
done

# a paced run sends the writers' requests at WRITE_RATE per second in total
: "${WRITE_RATE:=0}"
PACE=
[ "$WRITE_RATE" = 0 ] || PACE="--write-rate $WRITE_RATE"
scenario_args() { # <scenario> -> write-bench arguments
  case $1 in
    serial-literal) echo "serial --op literal --changes $CHANGES" ;;
    serial-iri) echo "serial --op iri --changes $CHANGES" ;;
    serial-delete) echo "serial --op delete --changes $CHANGES" ;;
    writers) echo "writers --clients 4 --changes $WRITER_CHANGES $PACE" ;;
    writers16) echo "writers --clients 16 --changes $WRITER_CHANGES $PACE" ;;
    mixed) echo "mixed --clients 4 --readers 2 --changes $MIXED_CHANGES $PACE" ;;
    serial-message) echo "serial --op literal --message a-commit-message --changes $CHANGES" ;;
    writers-message) echo "writers --clients 4 --message a-commit-message --changes $WRITER_CHANGES $PACE" ;;
    writers16-message) echo "writers --clients 16 --message a-commit-message --changes $WRITER_CHANGES $PACE" ;;
    *) fail "unknown scenario $1" ;;
  esac
}

SPID=
trap '[ -n "$SPID" ] && kill "$SPID" 2> /dev/null; true' EXIT
printf 'round\tvariant\tscenario\tresult\n' > "$R/runs.tsv"
run=0
for round in $(seq "$ROUNDS"); do
  order=$VARIANTS
  if [ $((round % 2)) = 0 ]; then
    order=$(echo "$VARIANTS" | tr ' ' '\n' | tac | tr '\n' ' ')
  fi
  for sc in $SCENARIOS; do
    for v in $order; do
      run=$((run + 1))
      b=V_${v}_BIN
      b=${!b}
      f=V_${v}_FLAGS
      flags=
      venv=()
      # the token delay-us=N adds N microseconds to every sync and write-through write of
      # the store's files (tests/support/iofault.c), standing in for a slower device
      for tok in ${!f}; do
        case $tok in
          delay-us=*) venv=(LD_PRELOAD="$PWD/libiofault.so" SPARKLES_IO_ROOT="$PWD/work" SPARKLES_IO_DELAY_US="${tok#delay-us=}") ;;
          *) flags+=" $tok" ;;
        esac
      done
      d=DYNAMIC_$b
      rm -rf srv work
      mkdir -p work
      cp -r "stores/$b" work/store
      # shellcheck disable=SC2046,SC2086 # the loader and the flags are command lines
      taskset -c "$SERVER_CPUS" env ${venv[@]+"${venv[@]}"} $(loader "${!d}" "$b/sparkles") --result-cache-mb 0 $flags serve --data srv \
        --loc bench=work/store --port "$PORT" --timeout 600 --no-access-log > "$R/server-$run.log" 2>&1 &
      SPID=$!
      for _ in $(seq 300); do
        curl -sf -o /dev/null "localhost:$PORT/\$/ping" && break
        kill -0 "$SPID" 2> /dev/null || fail "server $v exited, see server-$run.log"
        sleep 0.1
      done
      # shellcheck disable=SC2046,SC2086 # scenario_args is a list of arguments
      out=$(taskset -c "$CLIENT_CPUS" $(loader "$DYNAMIC_CLIENT" ./write-bench) --addr "127.0.0.1:$PORT" --ds bench --people "$PEOPLE" $(scenario_args "$sc") | tail -1) || {
        log "run $run ($v $sc) FAILED: $out"
        out="{\"error\":true}"
      }
      curl -s "localhost:$PORT/\$/metrics" | grep -E '^sparkles_(group_commit|commit_sync)' > "$R/metrics-$run.txt" || true
      kill "$SPID"
      wait "$SPID" 2> /dev/null || true
      SPID=
      printf '%s\t%s\t%s\t%s\n' "$round" "$v" "$sc" "$out" >> "$R/runs.tsv"
      log "round $round $sc $v done"
    done
  done
done
log "finished"
echo ok > "$R/done"
