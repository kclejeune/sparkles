#!/usr/bin/env bash
# The remote half of scripts/nsc-bench.sh: runs on the Namespace instance, in the directory
# that holds it, with bash, busybox, curl and zstd only.
#
#   bash nsc-bench-remote.sh   (settings come from ./run.env)
#
# Expects bin-A.zst and bin-B.zst (the two binaries, or bin-A and bin-B uncompressed), data.nt.zst and queries/*.rq next to
# it. Both binaries load their own store from the same data, so a change of the store format
# between A and B does no harm, unless SHARE_STORE=1 serves B from A's store. Then ROUNDS rounds of A, B, B, A run, each variant in a fresh
# server process pinned to SERVER_CPU, with this script and its curl client pinned to
# CLIENT_CPU. In each server process every query gets WARMUP untimed requests and REPS timed
# ones. results/samples.tsv gets one line per timed request with the server's execution time
# (execMs of application/x-sparkles+json, unrounded) and curl's wall time. results/answers.tsv
# gets a checksum of each variant's sorted TSV answer to each query, results/queries.txt the
# names of the queries the run times, and results/load.tsv the
# load times. With EXPECTED_ANSWER_CHANGE set to a comma-separated list of queries and
# EXPECTED_ANSWER_CHANGE_REASON to why their answers change, results/expected-answer-change.txt
# gets both, and the summary reports a change of those queries' answers instead of refusing it. With ORDER_DIGEST set to a command, such as `python3 scripts/bench-gate.py
# order-digest`, it is run with the query file and the TSV answer, and its output, a digest
# of the ORDER BY keys in the returned order or "-", is a fifth column of answers.tsv. With
# WARM_DIGEST set to a command, such as `python3 scripts/bench-gate.py native-digest`, it is
# run with the query file and the query's timed responses, after its timed requests, and
# prints a line for each; results/warm-answers.tsv gets them, so that the timed format's
# answers are checked too. Progress goes to results/progress.log and results/done marks the end.
#
# VARIANTS names the variants, "A B" by default. A run of more variants, such as the four
# arms of scripts/bench-executors.sh with a base and a tip binary, names them there, and
# BINARY_OF_<v> and STORE_OF_<v> let variant v run another variant's binary and serve from
# its store, so only the variants that own a binary need bin-<v> and only those that own a
# store load one. SHARE_STORE=1 is STORE_OF_B=A. ORDERS lists the order of each round's
# server processes as sequences separated by semicolons, "A B B A" by default, and round r
# takes the sequences in turn. Every sequence names every variant. EXECUTION is one mode
# for every variant or a comma-separated mode per variant in the order of VARIANTS, where
# an empty mode sends no execution field.
#
# With SPIN=1 (the default), a busy loop at the lowest priority (SCHED_IDLE) runs on the
# server and client CPUs during the timed part. It keeps their vCPUs from halting between
# requests, which in a VM costs a hypervisor exit and a cold wake-up, and it yields to the
# server and client as soon as they run. In a noise test with one binary, it halved the
# spread within a server process and between server processes. SPIN_LOOP picks the loop.
# With user, the default, it is `while :; do :; done`, which never enters the kernel. A kernel
# with lazy preemption, such as one that logs "Dynamic Preempt: lazy", then lets a server or
# client woken from another CPU wait for that CPU's next tick, so every request's wall time
# gains up to a millisecond per wake. With syscall it is `while :; do : > /dev/null; done`,
# whose system calls let the woken task run within microseconds. Runs under different loops
# are never compared. env.txt records the loop.
#
# REQUEST_CLIENT picks the client of the warm-up and timed requests. With curl (the
# default), each request forks curl, grep and awk, which costs about 19 ms of client time
# per request and opens a new connection each time, and wall_ms is curl's time_total. With
# persistent, one python3 process per server process sends every query's requests over one
# kept-alive connection, reconnecting only when the server closes it, and wall_ms runs from
# sending the request to reading the last byte of the answer. The request, its form fields
# and headers, and the execMs read from the answer are the same with either client, so
# exec_ms measures the same server work. The answer checks and the readiness probe use
# curl with either client. REQUEST_CLIENT_<v> sets variant v's client, so that a run of one
# binary as A and B can compare the two clients. Back-to-back requests find the server's
# caches warmer than curl's, so very short queries read faster with the persistent client,
# and REQUEST_GAP_MS (0 by default) makes it wait that long before each request.
# REQUEST_GAP_MS_<v> sets variant v's gap.
set -euo pipefail
cd "$(dirname "$0")"
# shellcheck source=/dev/null
source ./run.env
: "${ROUNDS:=4}" "${REPS:=20}" "${WARMUP:=3}" "${SERVER_CPU:=2}" "${CLIENT_CPU:=4}"
: "${RAYON_THREADS:=1}" "${PORT:=3999}" "${SERVE_ARGS:=}" "${EXECUTION:=}" "${MAX_TIME:=300}" "${SPIN:=1}" "${SPIN_LOOP:=user}"
# SERVE_ARGS_A and SERVE_ARGS_B add flags for one variant. SHARE_STORE=1 serves B from A's
# store, which compares settings of one binary without the differences between two stores.
# scripts/nsc-bench.sh does not write these three, and scripts/x08-gates/latency.sh does.
: "${SERVE_ARGS_A:=}" "${SERVE_ARGS_B:=}" "${SHARE_STORE:=0}"
: "${VARIANTS:=A B}" "${ORDERS:=A B B A}" "${REQUEST_CLIENT:=curl}" "${REQUEST_GAP_MS:=0}"
read -r -a VARIANT_LIST <<< "$VARIANTS"
# PREPARE_A and PREPARE_B are sparkles commands run once on each variant's store after its
# load, and PREPARE_REPEAT one run PREPARE_REPS times after that. Each run is timed into
# results/prepare.tsv as variant, command, seconds and the store's size in KiB after it.
: "${PREPARE_A:=}" "${PREPARE_B:=}" "${PREPARE_REPEAT:=}" "${PREPARE_REPS:=5}"
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

# the variant each variant takes its binary and its store from
declare -A BINARY_OF STORE_OF MODE
for v in "${VARIANT_LIST[@]}"; do
  [[ $v =~ ^[A-Z]$ ]] || fail "variant names are single capital letters, not $v"
  b=BINARY_OF_$v s=STORE_OF_$v
  BINARY_OF[$v]=${!b:-$v}
  STORE_OF[$v]=${!s:-${BINARY_OF[$v]}}
done
[ "$SHARE_STORE" = 1 ] && STORE_OF[B]=A
for v in "${VARIANT_LIST[@]}"; do
  [ -n "${BINARY_OF[${BINARY_OF[$v]}]:-}" ] && [ "${BINARY_OF[${BINARY_OF[$v]}]}" = "${BINARY_OF[$v]}" ] ||
    fail "BINARY_OF_$v names ${BINARY_OF[$v]}, which is not a variant with its own binary"
  [ -n "${STORE_OF[${STORE_OF[$v]}]:-}" ] && [ "${STORE_OF[${STORE_OF[$v]}]}" = "${STORE_OF[$v]}" ] ||
    fail "STORE_OF_$v names ${STORE_OF[$v]}, which is not a variant with its own store"
done
# EXECUTION's fields, keeping empty ones
modes=()
rest=$EXECUTION,
while [ -n "$rest" ]; do
  modes+=("${rest%%,*}")
  rest=${rest#*,}
done
if [ ${#modes[@]} -le 1 ]; then
  for v in "${VARIANT_LIST[@]}"; do MODE[$v]=$EXECUTION; done
elif [ ${#modes[@]} = ${#VARIANT_LIST[@]} ]; then
  for i in "${!VARIANT_LIST[@]}"; do MODE[${VARIANT_LIST[$i]}]=${modes[$i]}; done
else
  fail "EXECUTION has ${#modes[@]} modes for ${#VARIANT_LIST[@]} variants"
fi
IFS=';' read -r -a ORDER_LIST <<< "$ORDERS"
for o in "${ORDER_LIST[@]}"; do
  for v in $o; do [ -n "${MODE[$v]+x}" ] || fail "ORDERS names $v, which is not a variant"; done
  for v in "${VARIANT_LIST[@]}"; do [[ " $o " == *" $v "* ]] || fail "the order '$o' leaves out $v"; done
done
declare -A CLIENT_OF GAP_OF
PERSISTENT=0
for v in "${VARIANT_LIST[@]}"; do
  c=REQUEST_CLIENT_$v g=REQUEST_GAP_MS_$v
  CLIENT_OF[$v]=${!c:-$REQUEST_CLIENT}
  GAP_OF[$v]=${!g:-$REQUEST_GAP_MS}
  [[ ${GAP_OF[$v]} =~ ^[0-9]+([.][0-9]+)?$ ]] || fail "REQUEST_GAP_MS of $v is not a number of milliseconds: ${GAP_OF[$v]}"
  case ${CLIENT_OF[$v]} in
    curl) ;;
    persistent) PERSISTENT=1 ;;
    *) fail "REQUEST_CLIENT is curl or persistent, not ${CLIENT_OF[$v]} for $v" ;;
  esac
done
[ "$PERSISTENT" = 0 ] || command -v python3 > /dev/null || fail "REQUEST_CLIENT=persistent needs python3"
case $SPIN_LOOP in
  user) SPIN_BODY='while :; do :; done' ;;
  syscall) SPIN_BODY='while :; do : > /dev/null; done' ;;
  *) fail "SPIN_LOOP is user or syscall, not $SPIN_LOOP" ;;
esac

# The binaries are built on another Linux and name the build host's glibc loader, so a
# dynamically linked one (DYNAMIC_A, DYNAMIC_B) runs under this image's loader.
# BIN[v] is the command that runs variant v.
declare -A BIN
for v in "${VARIANT_LIST[@]}"; do
  o=${BINARY_OF[$v]}
  d=DYNAMIC_$o
  if [ "${!d:-1}" = 1 ]; then BIN[$v]="/lib64/ld-linux-x86-64.so.2 $o/sparkles"; else BIN[$v]=$o/sparkles; fi
done

{
  echo "kernel: $(uname -r)"
  echo "cpu: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//')"
  echo "cpus: $NCPU"
  echo "memory: $(free -m | awk '/Mem:/ {print $2}') MiB"
  echo "smt siblings: $(sort -u /sys/devices/system/cpu/cpu*/topology/thread_siblings_list | tr '\n' ' ')"
  echo "loader: $( (/lib64/ld-linux-x86-64.so.2 --version 2> /dev/null || echo none) | head -1)"
  if [ "$SPIN" = 1 ]; then echo "busy loops: $SPIN_LOOP ($SPIN_BODY)"; else echo "busy loops: none"; fi
  echo "request client: $(for v in "${VARIANT_LIST[@]}"; do [ "${CLIENT_OF[$v]}" = curl ] && printf '%s=curl ' "$v" || printf '%s=persistent(gap %s ms) ' "$v" "${GAP_OF[$v]}"; done)$([ "$PERSISTENT" = 0 ] || python3 --version 2>&1)"
} > "$R/env.txt"
# the steal and total jiffies of the whole machine: summed before and after the timed part
stat_cpu() { awk '/^cpu / {t = 0; for (i = 2; i <= NF; i++) t += $i; print $9, t}' /proc/stat; }

log "unpacking"
# A binary that scripts/nsc-bench.sh copied here on a Boat worker arrives as bin-A or bin-B,
# uncompressed, and is linked rather than copied where it can be. results/binaries.sha256
# records what runs.
OWN_BINARIES=()
for v in "${VARIANT_LIST[@]}"; do
  [ "${BINARY_OF[$v]}" = "$v" ] || continue
  OWN_BINARIES+=("$v/sparkles")
  mkdir -p "$v"
  if [ -f "bin-$v" ]; then ln -f "bin-$v" "$v/sparkles" 2> /dev/null || cp -f "bin-$v" "$v/sparkles"; else zstd -q -d -f "bin-$v.zst" -o "$v/sparkles"; fi
  chmod +x "$v/sparkles"
done
sha256sum "${OWN_BINARIES[@]}" > "$R/binaries.sha256"
zstd -q -d -f data.nt.zst -o data.nt
log "data: $(wc -l < data.nt) triples"

: > "$R/load.tsv"
for v in "${VARIANT_LIST[@]}"; do
  rm -rf "stores/$v"
  mkdir -p stores
  if [ "${STORE_OF[$v]}" != "$v" ]; then
    ln -s "${STORE_OF[$v]}" "stores/$v"
    continue
  fi
  t0=$EPOCHREALTIME
  # shellcheck disable=SC2086 # BIN is a command line
  taskset -c "$ALL" ${BIN[$v]} load --loc "stores/$v" data.nt > "$R/load-$v.log" 2>&1 || fail "load $v, see load-$v.log"
  t1=$EPOCHREALTIME
  printf '%s\t%s\t%s\n' "$v" "$(awk "BEGIN {print $t1 - $t0}")" "$(du -sk "stores/$v" | cut -f1)" >> "$R/load.tsv"
  log "$v loaded in $(awk "BEGIN {printf \"%.2f\", $t1 - $t0}") s"
done

: > "$R/prepare.tsv"
prepare() { # <variant> <command…>
  local v=$1 t0 t1
  shift
  t0=$EPOCHREALTIME
  # shellcheck disable=SC2086 # BIN is a command line
  taskset -c "$ALL" ${BIN[$v]} "$@" --loc "stores/$v" >> "$R/prepare-$v.log" 2>&1 || fail "prepare $v: $*, see prepare-$v.log"
  t1=$EPOCHREALTIME
  printf '%s\t%s\t%s\t%s KiB\n' "$v" "$*" "$(awk "BEGIN {print $t1 - $t0}")" "$(du -sk "stores/$v" | cut -f1)" >> "$R/prepare.tsv"
  log "$v: $* took $(awk "BEGIN {printf \"%.2f\", $t1 - $t0}") s"
}
PREPARED=()
for v in "${VARIANT_LIST[@]}"; do [ "${STORE_OF[$v]}" = "$v" ] && PREPARED+=("$v"); done
for v in "${PREPARED[@]}"; do
  p=PREPARE_$v
  # shellcheck disable=SC2086 # the commands are word lists
  [ -z "${!p:-}" ] || prepare "$v" ${!p:-}
done
# the repeated runs alternate A, B and B, A
if [ -n "$PREPARE_REPEAT" ]; then
  for i in $(seq "$PREPARE_REPS"); do
    order=("${PREPARED[@]}")
    [ $((i % 2)) = 0 ] && [ ${#PREPARED[@]} = 2 ] && order=("${PREPARED[1]}" "${PREPARED[0]}")
    for v in "${order[@]}"; do
      # shellcheck disable=SC2086
      prepare "$v" $PREPARE_REPEAT
    done
  done
fi

mapfile -t QUERIES < <(cd queries && for f in *.rq; do echo "${f%.rq}"; done)
log "queries: ${QUERIES[*]}"

URL=localhost:$PORT/bench/sparql
# The mode of the running variant goes in the form body next to the query and is left out
# when it is empty.
EXEC_FIELD=()
SPID=
start_server() { # <variant> <epoch>
  local env=() mode=${MODE[$1]} a=SERVE_ARGS_$1
  local extra=${!a:-}
  EXEC_FIELD=()
  [ -z "$mode" ] || EXEC_FIELD=(--data-urlencode "execution=$mode")
  [ "$RAYON_THREADS" != default ] && env=(RAYON_NUM_THREADS="$RAYON_THREADS")
  rm -rf "srv"
  # shellcheck disable=SC2086 # BIN is a command line and the SERVE_ARGS lists of flags
  taskset -c "$SERVER_CPU" env "${env[@]}" ${BIN[$1]} --result-cache-mb 0 serve --data srv \
    --loc bench="stores/$1" --port "$PORT" --timeout 600 --no-access-log $SERVE_ARGS $extra > "$R/server-$2-$1.log" 2>&1 &
  SPID=$!
  for _ in $(seq 300); do
    curl -sf -o /dev/null "localhost:$PORT/\$/ready" && return
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
  rm -f resp.json
  if ! w=$(curl -sS -f --max-time "$MAX_TIME" -o resp.json -w '%{time_total}' -H 'Accept: application/x-sparkles+json' \
    --data-urlencode "query@queries/$1.rq" "${EXEC_FIELD[@]}" "$URL" 2> /dev/null); then
    echo error
    return
  fi
  e=$(grep -o '"execMs":[0-9.eE+-]*' resp.json | head -1 | cut -d: -f2)
  echo "${e:-error} $w"
}

# The persistent client: one process per server process runs every query's warm-up and
# timed requests in order and appends the same samples.tsv and warm-answers.tsv lines as
# the curl loop below.
if [ "$PERSISTENT" = 1 ]; then
  cat > bench-client.py << 'PY'
import argparse, http.client, os, re, shlex, socket, subprocess, sys, threading, time, urllib.parse

p = argparse.ArgumentParser()
p.add_argument("--port", type=int, required=True)
p.add_argument("--round", required=True)
p.add_argument("--epoch", required=True)
p.add_argument("--variant", required=True)
p.add_argument("--warmup", type=int, required=True)
p.add_argument("--reps", type=int, required=True)
p.add_argument("--max-time", type=float, required=True)
p.add_argument("--execution", default="")
p.add_argument("--digest", default="")
p.add_argument("--gap-ms", type=float, default=0.0)
p.add_argument("--samples", required=True)
p.add_argument("--warm-answers", required=True)
p.add_argument("queries", nargs="+")
a = p.parse_args()
EXEC = re.compile(rb'"execMs":([0-9.eE+-]*)')
HEADERS = {"Accept": "application/x-sparkles+json", "Content-Type": "application/x-www-form-urlencoded"}
conn = None


def form(text):
    # curl --data-urlencode writes a space as + and escapes every other byte but A-Z a-z 0-9
    # - . _ ~, and so does quote_plus
    body = "query=" + urllib.parse.quote_plus(text, safe="")
    if a.execution:
        body += "&execution=" + urllib.parse.quote_plus(a.execution, safe="")
    return body.encode()


# MAX_TIME bounds each whole request, as curl's --max-time does, and 0 means no bound
LIMIT = a.max_time if a.max_time > 0 else None
# the failures of a kept-alive connection that the server closed while it was idle
STALE = (http.client.RemoteDisconnected, ConnectionResetError, ConnectionAbortedError, BrokenPipeError)


def connect():
    return http.client.HTTPConnection("localhost", a.port, timeout=LIMIT)


def attempt(body, t0):
    """One try of a request on the current connection: (status, data, got, fired, error).

    A watchdog shuts the socket down at the deadline, so that a server that trickles its
    headers or its body cannot keep one request alive past MAX_TIME. The socket's own
    timeout only bounds each read. Each try has its own lock, state and socket, so that a
    late watchdog of an earlier try can only find that try done and never touches the
    next one, and the lock makes a try either finish or time out, never both."""
    lock = threading.Lock()
    state = {"done": False, "fired": False}
    timer = None
    got = False
    try:
        if conn.sock is None:
            conn.connect()
        sock = conn.sock

        def expire():
            with lock:
                if state["done"]:
                    return
                state["fired"] = True
            try:
                sock.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass

        if LIMIT is not None:
            left = LIMIT - (time.perf_counter() - t0)
            if left <= 0:
                raise TimeoutError("the request took longer than MAX_TIME")
            sock.settimeout(left)
            timer = threading.Timer(left, expire)
            timer.daemon = True
            timer.start()
        conn.request("POST", "/bench/sparql", body, HEADERS)
        r = conn.getresponse()
        got = True
        # read() returns the whole body, by its length, its chunks or the server's close,
        # and leaves a connection that the server closed for the next request to reopen
        data = r.read()
        with lock:
            state["done"] = True
            fired = state["fired"]
        if fired:
            raise TimeoutError("the request took longer than MAX_TIME")
        return r.status, data, got, False, None
    except (OSError, http.client.HTTPException) as e:
        with lock:
            state["done"] = True
            fired = state["fired"]
        return None, None, got, fired, e
    finally:
        if timer is not None:
            timer.cancel()


def request(body):
    """(execMs text or None, wall seconds, answer bytes or None)"""
    global conn
    if a.gap_ms > 0:
        time.sleep(a.gap_ms / 1000)
    for n in (1, 2):
        # A request on a reused connection that fails as a stale connection does, before any
        # answer arrives, is sent once more on a new connection and timed from there. Every
        # other failure, a timeout among them, gives an error row as curl's does.
        reused = conn.sock is not None
        t0 = time.perf_counter()
        status, data, got, fired, e = attempt(body, t0)
        if e is None:
            break
        conn.close()
        conn = connect()
        if fired or got or not reused or n == 2 or not isinstance(e, STALE):
            return None, None, None
    w = time.perf_counter() - t0
    if status >= 400:
        return None, w, None
    m = EXEC.search(data)
    return (m.group(1).decode() if m and m.group(1) else None), w, data


conn = connect()
with open(a.samples, "a") as samples, open(a.warm_answers, "a") as warm:
    for q in a.queries:
        path = f"queries/{q}.rq"
        with open(path, encoding="utf-8", newline="") as f:
            body = form(f.read())
        for _ in range(a.warmup):
            request(body)
        files = []
        for rep in range(1, a.reps + 1):
            e, w, data = request(body)
            if e is None:
                samples.write(f"{a.round}\t{a.epoch}\t{a.variant}\t{q}\t{rep}\terror\terror\n")
            else:
                samples.write(f"{a.round}\t{a.epoch}\t{a.variant}\t{q}\t{rep}\t{e}\t{w * 1000:.6g}\n")
            if a.digest:
                name = f"timed-{rep}.json"
                files.append(name)
                if data is not None:
                    with open(name, "wb") as out:
                        out.write(data)
                elif os.path.exists(name):
                    os.remove(name)
        samples.flush()
        if a.digest:
            # as in the curl loop, a digest command that fails or cannot start gives "no digest"
            try:
                digests = subprocess.run(shlex.split(a.digest) + [path] + files, stdout=subprocess.PIPE,
                                         text=True).stdout.splitlines()
            except OSError:
                digests = []
            for rep in range(1, a.reps + 1):
                d = (digests[rep - 1] if rep <= len(digests) else "") or "no digest"
                warm.write(f"{a.epoch}\t{a.variant}\t{q}\t{rep}\t{d}\n")
            warm.flush()
            for name in files:
                if os.path.exists(name):
                    os.remove(name)
conn.close()
PY
fi

SPINNERS=()
if [ "$SPIN" = 1 ]; then
  for c in "$SERVER_CPU" "$CLIENT_CPU"; do
    taskset -c "$c" chrt -i 0 sh -c "$SPIN_BODY" > /dev/null 2>&1 &
    SPINNERS+=($!)
  done
fi
printf 'round\tepoch\tvariant\tquery\trep\texec_ms\twall_ms\n' > "$R/samples.tsv"
: > "$R/answers.tsv"
: > "$R/warm-answers.tsv"
# the queries this run times, so that a summary requires each of them even when none of
# its requests leaves a sample
printf '%s\n' "${QUERIES[@]}" > "$R/queries.txt"
# the queries whose answers the run expects to change, and why, as run.env set them
if [ -n "${EXPECTED_ANSWER_CHANGE:-}" ]; then
  printf 'queries\t%s\nreason\t%s\n' "${EXPECTED_ANSWER_CHANGE//,/ }" "${EXPECTED_ANSWER_CHANGE_REASON:-}" > "$R/expected-answer-change.txt"
fi
read -r steal0 total0 < <(stat_cpu)
epoch=0
checked=" "
for round in $(seq "$ROUNDS"); do
  read -r -a ORDER <<< "${ORDER_LIST[$(((round - 1) % ${#ORDER_LIST[@]}))]}"
  for v in "${ORDER[@]}"; do
    epoch=$((epoch + 1))
    start_server "$v" "$epoch"
    if [[ $checked != *" $v "* ]]; then
      checked+="$v "
      for q in "${QUERIES[@]}"; do
        if curl -sS -f --max-time "$MAX_TIME" -o ans.tsv -H 'Accept: text/tab-separated-values' \
          --data-urlencode "query@queries/$q.rq" "${EXEC_FIELD[@]}" "$URL" 2> /dev/null; then
          odig=""
          if [ -n "${ORDER_DIGEST:-}" ]; then
            # shellcheck disable=SC2086
            odig=$'\t'"$($ORDER_DIGEST "queries/$q.rq" ans.tsv)"
          fi
          printf '%s\t%s\t%s\t%s%s\n' "$v" "$q" "$(($(wc -l < ans.tsv) - 1))" "$(sort ans.tsv | sha256sum | cut -c1-16)" \
            "$odig" >> "$R/answers.tsv"
        else
          printf '%s\t%s\terror\terror\n' "$v" "$q" >> "$R/answers.tsv"
        fi
      done
    fi
    if [ "${CLIENT_OF[$v]}" = persistent ]; then
      python3 bench-client.py --port "$PORT" --round "$round" --epoch "$epoch" --variant "$v" \
        --warmup "$WARMUP" --reps "$REPS" --max-time "$MAX_TIME" --execution "${MODE[$v]}" --gap-ms "${GAP_OF[$v]}" \
        --digest "${WARM_DIGEST:-}" --samples "$R/samples.tsv" --warm-answers "$R/warm-answers.tsv" \
        "${QUERIES[@]}" || fail "the request client failed in epoch $epoch ($v)"
      QUERY_LOOP=()
    else
      QUERY_LOOP=("${QUERIES[@]}")
    fi
    for q in ${QUERY_LOOP[@]+"${QUERY_LOOP[@]}"}; do
      for _ in $(seq "$WARMUP"); do request "$q" > /dev/null; done
      for rep in $(seq "$REPS"); do
        read -r e w <<< "$(request "$q")"
        if [ "$e" = error ]; then
          printf '%s\t%s\t%s\t%s\t%s\terror\terror\n' "$round" "$epoch" "$v" "$q" "$rep" >> "$R/samples.tsv"
        else
          printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$round" "$epoch" "$v" "$q" "$rep" "$e" "$(awk "BEGIN {print $w * 1000}")" >> "$R/samples.tsv"
        fi
        # every timed answer is kept and digested after the timed loop
        [ -z "${WARM_DIGEST:-}" ] || mv -f resp.json "timed-$rep.json" 2> /dev/null || :
      done
      if [ -n "${WARM_DIGEST:-}" ]; then
        # one process digests all of the query's timed answers, one line each in order, so
        # that a large answer's check against the input runs once per distinct answer
        files=()
        for rep in $(seq "$REPS"); do files+=("timed-$rep.json"); done
        # shellcheck disable=SC2086
        mapfile -t digests < <($WARM_DIGEST "queries/$q.rq" "${files[@]}" || :)
        for rep in $(seq "$REPS"); do
          printf '%s\t%s\t%s\t%s\t%s\n' "$epoch" "$v" "$q" "$rep" "${digests[$((rep - 1))]:-no digest}" >> "$R/warm-answers.tsv"
        done
        rm -f "${files[@]}"
      fi
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
