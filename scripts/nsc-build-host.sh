#!/usr/bin/env bash
# Where scripts/nsc-bench.sh and scripts/nsc-writes.sh build a revision, sourced by both.
#
# --build-host takes local, auto or a host name. local builds on this machine. A host name
# builds on that host over ssh, and waits for its heavy lock when another process holds
# it. auto probes the hosts that SPARKLES_NSC_BUILD_HOSTS lists, in order, and picks the
# first that is reachable, whose ~/lanes/.heavy.lock is free, that runs no gate-* unit and
# that has SPARKLES_NSC_BUILD_MIN_FREE_GB (default 20) GiB free under its home directory.
# When every reachable host with the disk is busy, the build waits for the heavy lock of
# the first of them, since a build on this machine is what the option exists to avoid, and
# the message says that --build-host local builds here instead. When no listed host can be
# reached, or the list names only the benchmark host, the run stops. Without --build-host the run uses auto when
# SPARKLES_NSC_BUILD_HOSTS is set and local otherwise, so a machine without the setting
# builds as it always has. The variables come from the environment, else from KEY=VALUE
# lines in ${SPARKLES_NSC_BUILD_CONFIG:-~/.config/sparkles/nsc-build.env}. The benchmark
# host (SPARKLES_BENCH_HOST, see scripts/gate/config.sh) never builds and is refused.
#
# A remote build ships the `git archive` of the commit, with the placeholder ui/build that a
# local build gets, to the scratch directory ~/lanes/nsc-build/<commit>.<random> on the
# host. It starts scripts/nsc-build-remote.sh there, which builds with scripts/nsc-build.sh
# under mbx in a memory-capped systemd scope named nsc-build-<commit>-<random>, while it
# holds the host's heavy lock. The scope runs detached from the ssh connection, and this
# side polls it every SPARKLES_NSC_BUILD_POLL_SECONDS (default 15) and copies its log. Each
# remote step can be repeated, so a step whose connection drops is retried up to
# SPARKLES_NSC_BUILD_RETRIES times (default 8), SPARKLES_NSC_BUILD_RETRY_SECONDS apart
# (default 20), and a lost reply never starts a second build. The scope's RuntimeMaxSec is
# SPARKLES_NSC_BUILD_RUNTIME (default 14400), less when SPARKLES_NSC_BUILD_DEADLINE or a
# Boat worker's admitted lifetime leaves less, and a start with under 300 seconds is
# refused. The binary and its build record come back over ssh and the SHA-256 is checked.
# The scope is stopped and the scratch directory removed when the build ends in any way:
# after a failure, and from the caller's EXIT trap (bh_cleanup) after an interrupt or lost
# contact. A Boat worker keeps the directory as evidence. Cleanup touches only the
# directory and scope this run created, and fails with 97 when it cannot confirm that the
# scope has stopped.

BH_SELF_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BH_ROOT=$(cd "$BH_SELF_DIR/.." && pwd)
# shellcheck source=scripts/gate/config.sh
. "$BH_SELF_DIR/gate/config.sh"
# shellcheck source=scripts/remote-executor.sh
. "$BH_SELF_DIR/remote-executor.sh"
BH_UI_PLACEHOLDER='<!doctype html><title>Sparkles</title><p>The UI was not built for this benchmark binary.</p>'
# The --build-host argument, the resolved choice (local or a host) and the remote state
# that bh_cleanup removes.
BH_ARG=
BH_HOST=
BH_RHOST=
BH_RDIR=
BH_RUNIT=
BH_START_REFUSED=
BH_SEND_ATTEMPT=1
BH_ADMISSION=

# bh_ui_placeholder DIR: the server embeds ui/build. Its build script writes a placeholder,
# but does not rerun for a second source tree in a shared target directory.
bh_ui_placeholder() {
  mkdir -p "$1/ui/build"
  echo "$BH_UI_PLACEHOLDER" > "$1/ui/build/index.html"
}

# bh_entry_host_ok BUILD_RECORD: whether a cached binary whose build record is BUILD_RECORD
# suits this run's --build-host. A Boat worker runs the binaries it builds, and its glibc can
# be older than that of the host that built an entry, so a Boat build host accepts only an
# entry built on that worker. Every other choice accepts an entry from any host.
bh_entry_host_ok() {
  case $BH_ARG in
    boat:*) grep -qxF "build host: $BH_ARG" "$1" 2> /dev/null ;;
    *) return 0 ;;
  esac
}

# bh_entry_key_suffix: the suffix of a write-screen cache key for this run's --build-host. A
# Boat worker keeps entries of its own, for the reason bh_entry_host_ok gives.
bh_entry_key_suffix() {
  case $BH_ARG in boat:*) echo "-${BH_ARG#boat:}" ;; esac
}

# bh_check_arg: refuse a malformed --build-host before any work
bh_check_arg() {
  case $BH_ARG in
    '' | local | auto) ;;
    boat:bx_*) python3 "$RX_DIR/remote-executor.py" validate "$BH_ARG" > /dev/null || die 'invalid or expired Boat admission' ;;
    -* | *[!A-Za-z0-9._@-]*) die "--build-host is local, auto or a host name, not $BH_ARG" ;;
    *) if is_bench "$BH_ARG"; then die "$BH_ARG is the benchmark host (SPARKLES_BENCH_HOST) and never builds"; fi ;;
  esac
}

bh_load_config() {
  local config=${SPARKLES_NSC_BUILD_CONFIG:-$HOME/.config/sparkles/nsc-build.env} k v
  [ -f "$config" ] || return 0
  while IFS='=' read -r k v; do
    case $k in
      SPARKLES_NSC_BUILD_HOSTS | SPARKLES_NSC_BUILD_MIN_FREE_GB)
        [ -n "${!k+x}" ] || export "$k=$v"
        ;;
    esac
  done < <(grep -E '^[A-Z_]+=' "$config")
}

# bh_probe HOST: "<free|held> <gate units> <free GiB>", or "unreachable"
bh_probe() {
  # shellcheck disable=SC2016
  rx_exec "$1" '
    if [ -e "$HOME/lanes/.heavy.lock" ] && ! flock -n "$HOME/lanes/.heavy.lock" true; then l=held; else l=free; fi
    u=$(systemctl --user list-units "gate-*" --state=active --plain --no-legend 2> /dev/null | wc -l)
    d=$(df -BG --output=avail "$HOME" | tail -n1 | tr -dc 0-9)
    echo "$l $u $d"' < /dev/null 2> /dev/null || echo unreachable
}

# bh_resolve: set BH_HOST to local or a build host, once per run, and say which
bh_resolve() {
  [ -z "$BH_HOST" ] || return 0
  local want=$BH_ARG h p l u d min listed fallback='' hosts=()
  case $want in boat:*) rx_admit "$want" --minimum "${SPARKLES_REMOTE_MIN_LIFETIME:-1800}" > /dev/null || die "Boat build admission failed" ;; esac
  bh_load_config
  min=${SPARKLES_NSC_BUILD_MIN_FREE_GB:-20}
  if [ -z "$want" ]; then
    if [ -n "${SPARKLES_NSC_BUILD_HOSTS:-}" ]; then want=auto; else want=local; fi
  fi
  case $want in
    local)
      BH_HOST=local
      ;;
    auto)
      for h in ${SPARKLES_NSC_BUILD_HOSTS:-}; do
        if is_bench "$h"; then
          echo "build host $h: skipped, it is the benchmark host"
        else
          hosts+=("$h")
        fi
      done
      # Hosts that were listed but are all the benchmark host do not fall back to a local
      # build, which the setting exists to avoid.
      listed=${SPARKLES_NSC_BUILD_HOSTS:-}
      if [ ${#hosts[@]} = 0 ] && [ -n "${listed// /}" ]; then
        die "SPARKLES_NSC_BUILD_HOSTS lists no host but the benchmark host; name a build host or pass --build-host local"
      fi
      if [ ${#hosts[@]} = 0 ]; then
        echo "build host: none configured (SPARKLES_NSC_BUILD_HOSTS), building on this machine"
        BH_HOST=local
        return 0
      fi
      for h in "${hosts[@]}"; do
        p=$(bh_probe "$h")
        if [ "$p" = unreachable ]; then
          echo "build host $h: unreachable"
          continue
        fi
        read -r l u d <<< "$p"
        if [ "${d:-0}" -lt "$min" ]; then
          echo "build host $h: $d GiB free, $min needed"
        elif [ "$l" = free ] && [ "$u" = 0 ]; then
          echo "build host: $h (heavy lock free, $d GiB free)"
          BH_HOST=$h
          return 0
        else
          echo "build host $h: busy (heavy lock $l, $u gate units)"
          [ -n "$fallback" ] || fallback=$h
        fi
      done
      [ -n "$fallback" ] || die "no build host among ${hosts[*]} is reachable with $min GiB free; pass --build-host local to build on this machine"
      echo "build host: $fallback, every build host is busy, so the build waits for its heavy lock (--build-host local builds on this machine instead)"
      BH_HOST=$fallback
      ;;
    *)
      p=$(bh_probe "$want")
      [ "$p" != unreachable ] || die "build host $want is unreachable over ssh; pass --build-host local to build on this machine"
      read -r l u d <<< "$p"
      if [ "$l" = held ]; then
        echo "build host: $want, whose heavy lock is held, so the build waits for it"
      else
        echo "build host: $want"
      fi
      BH_HOST=$want
      ;;
  esac
}

# bh_rx SECONDS HOST COMMAND: run COMMAND on HOST, ending the attempt after SECONDS. The
# transport gets SIGINT first, so a Boat transfer tears down its owned process group.
bh_rx() {
  timeout -s INT -k 10 "$1" python3 "$RX_DIR/remote-executor.py" exec "$2" "$3"
}

# bh_retry DESCRIPTION OUTPUT COMMAND...: run COMMAND until it succeeds, at most
# SPARKLES_NSC_BUILD_RETRIES times (default 8), waiting SPARKLES_NSC_BUILD_RETRY_SECONDS
# (default 20) between attempts. Each attempt writes its standard output to a file of its
# own. Only the successful attempt's output reaches OUTPUT, a file, or standard output for
# -, where a failed attempt's output goes to standard error.
# Every remote step it wraps can be repeated after a dropped connection without changing
# what an earlier attempt did.
bh_retry() {
  local what=$1 output=$2 n=1 max=${SPARKLES_NSC_BUILD_RETRIES:-8} wait=${SPARKLES_NSC_BUILD_RETRY_SECONDS:-20} code attempt
  shift 2
  if [ -n "${BH_CLEANUP_RETRY:-}" ]; then
    # Cleanup must finish inside a caller's hard-kill grace.
    max=4
    [ "${wait%.*}" -le 5 ] || wait=5
  fi
  attempt=$(mktemp)
  while :; do
    code=0
    "$@" > "$attempt" || code=$?
    if [ "$code" = 0 ]; then
      if [ "$output" != - ]; then
        mv -- "$attempt" "$output"
      else
        cat -- "$attempt"
        rm -f -- "$attempt"
      fi
      return 0
    fi
    [ "$output" != - ] || cat -- "$attempt" >&2
    if [ "$n" -ge "$max" ]; then
      rm -f -- "$attempt"
      echo "$what failed $n times, the last with exit $code" >&2
      return "$code"
    fi
    echo "$what failed with exit $code (attempt $n of $max), retrying" >&2
    n=$((n + 1))
    sleep "$wait"
  done
}

# bh_cancel HOST DIR UNIT: fence later starts of the build and stop its scope. A scratch
# directory that never received the scripts has no build to stop.
bh_cancel() {
  # shellcheck disable=SC2029 # the unit and directory are this run's and expand here
  bh_rx 30 "$1" "if [ -f $2/bin/nsc-build-remote.sh ]; then bash $2/bin/nsc-build-remote.sh --cancel $2 $3; else systemctl --user stop $3.scope > /dev/null 2>&1; ! systemctl --user is-active --quiet $3.scope; fi" < /dev/null
}

# bh_cleanup: stop the scope and remove the scratch directory of an unfinished remote build.
# It retries at most 4 times, 5 seconds apart, with 30 seconds for each attempt, so it ends
# within about 3 minutes even when the host cannot be reached.
bh_cleanup() {
  [ -n "$BH_RDIR" ] || return 0
  if ! BH_CLEANUP_RETRY=1 bh_retry "stopping $BH_RUNIT.scope" - bh_cancel "$BH_RHOST" "$BH_RDIR" "$BH_RUNIT"; then
    echo "could not confirm scope closure at $BH_RHOST:$BH_RDIR; keep its evidence and reconcile $BH_RUNIT.scope" >&2
    return 97
  fi
  case $BH_RHOST in boat:*) echo "retained source/build evidence at $BH_RHOST:$BH_RDIR" ;;
  *) BH_CLEANUP_RETRY=1 bh_retry "removing $BH_RDIR" - bh_rx 30 "$BH_RHOST" "rm -rf -- $BH_RDIR" < /dev/null ;;
  esac
  BH_RDIR=
  BH_RUNIT=
}

# bh_build HOST COMMIT OUTDIR [cargo flags...]: build COMMIT on HOST into OUTDIR, which gets
# sparkles, info.txt (build host, cc, c++ and rustc versions), rustc.txt, cargo.txt and
# env.txt. Prints its progress, so the caller redirects it to the build log. The scratch
# directory goes whether the build succeeds or fails.
bh_build() {
  local rc=0
  bh_build_steps "$@" || rc=$?
  local cleanup_code=0
  bh_cleanup || cleanup_code=$?
  [ "$rc" != 0 ] || rc=$cleanup_code
  return "$rc"
}

# bh_scratch HOST REL: create ~/REL on HOST and print its absolute path. A repeat after a
# lost reply finds the directory this run named and prints it again.
bh_scratch() {
  # shellcheck disable=SC2029 # the path expands here
  bh_rx 60 "$1" "mkdir -p \$HOME/lanes/nsc-build && mkdir -p \$HOME/$2 && cd \$HOME/$2 && pwd -P" < /dev/null
}

# bh_published DIR NAME: shell text that, holding DIR/stage.lock on fd 7, succeeds when an
# attempt has published DIR/NAME. DIR/NAME.claim records a finished publication and stays
# when the build moves DIR/src away. A tree renamed into place by an attempt that ended
# before it wrote the claim is a finished publication too, and gets its claim here.
bh_published() {
  echo "{ [ -e $1/$2.claim ] || { [ -e $1/$2 ] && mkdir $1/$2.claim; }; }"
}

# bh_publish DIR NAME PART: shell text that renames the extracted PART to DIR/NAME and then
# writes the claim, under DIR/stage.lock, unless an earlier attempt published it, in which
# case PART is removed. A late attempt never publishes a second tree.
bh_publish() {
  echo "exec 7>> $1/stage.lock && flock 7 && if $(bh_published "$1" "$2"); then rm -rf -- $3; else mv -T $3 $1/$2 && mkdir $1/$2.claim; fi"
}

# bh_skip_published DIR NAME: shell text that ends an attempt at once, discarding its input,
# when an earlier attempt published DIR/NAME. The check takes DIR/stage.lock, so it waits
# for a publication in progress.
bh_skip_published() {
  echo "if (exec 7>> $1/stage.lock && flock 7 && $(bh_published "$1" "$2")); then cat > /dev/null; exit 0; fi"
}

# bh_send_scripts HOST DIR TMP: copy the build scripts into DIR/bin. Each attempt extracts into
# its own directory, so an attempt whose connection dropped cannot change a published tree.
bh_send_scripts() {
  local part=$2/bin.part.$BH_SEND_ATTEMPT
  BH_SEND_ATTEMPT=$((BH_SEND_ATTEMPT + 1))
  # shellcheck disable=SC2029
  tar -C "$3" -cf - . | bh_rx 120 "$1" \
    "$(bh_skip_published "$2" bin); rm -rf -- $part && mkdir $part && tar -x -C $part && $(bh_publish "$2" bin "$part")"
}

# bh_send_source HOST DIR COMMIT: extract the commit's archive into DIR/src, in the same way.
bh_send_source() {
  local part=$2/src.part.$BH_SEND_ATTEMPT
  BH_SEND_ATTEMPT=$((BH_SEND_ATTEMPT + 1))
  # Source mtimes stay current, as in a local build.
  # shellcheck disable=SC2029
  git -C "$BH_ROOT" archive "$3" | bh_rx 900 "$1" \
    "$(bh_skip_published "$2" src); rm -rf -- $part && mkdir $part && tar -x -m -C $part && mkdir -p $part/ui/build && cp $2/bin/ui-index.html $part/ui/build/index.html && $(bh_publish "$2" src "$part")"
}

# bh_fetch HOST DIR OUT: copy DIR/out back into OUT. The archive is extracted only when it
# arrived whole, so a repeat replaces the same files.
bh_fetch() {
  # shellcheck disable=SC2029
  bh_rx 600 "$1" "tar -C $2/out -cf - ." < /dev/null | python3 "$RX_DIR/remote-executor.py" unpack "$3"
}

# BH_LAUNCH_ALLOWANCE: how much later than this side's clock reading a scope can end. The
# start command arrives within bh_rx's 130 seconds or not at all, nsc-build-remote.sh counts
# RUNTIME from its arrival and lets the scope begin within 30 seconds, and the scope's stop
# takes at most its TimeoutStopSec of 30 seconds.
BH_LAUNCH_ALLOWANCE=190

# bh_runtime: the scope's RuntimeMaxSec for a start now. It is SPARKLES_NSC_BUILD_RUNTIME
# (default 14400). When SPARKLES_NSC_BUILD_DEADLINE (a Unix time) is set, the scope ends,
# stop included, by that deadline. For a Boat worker it ends by the admitted lifetime less
# 960 seconds, the closure watch's 900-second lead and a minute, so a build ends before the
# closure check whether or not its cleanup can reach the worker.
bh_runtime() {
  local runtime=${SPARKLES_NSC_BUILD_RUNTIME:-14400} left
  if [ -n "${SPARKLES_NSC_BUILD_DEADLINE:-}" ]; then
    left=$((SPARKLES_NSC_BUILD_DEADLINE - $(date +%s) - BH_LAUNCH_ALLOWANCE))
    [ "$left" -ge "$runtime" ] || runtime=$left
  fi
  if [ -n "$BH_ADMISSION" ]; then
    left=$(rx_runtime "$BH_ADMISSION" --reserve 960 2> /dev/null) || left=0
    left=$((left - BH_LAUNCH_ALLOWANCE))
    [ "$left" -ge "$runtime" ] || runtime=$left
  fi
  echo "$runtime"
}

# bh_start HOST DIR UNIT ARGS: start the detached build. When the reply is lost, the build's
# state decides. A build that started, even one that already ended, counts as started, and a
# start that the host refused sets BH_START_REFUSED to its exit status, so neither is
# started again. A start with less than 300 seconds of runtime left is refused here.
bh_start() {
  local code=0 state tmp runtime
  runtime=$(bh_runtime)
  if [ "$runtime" -lt 300 ]; then
    echo "only $runtime seconds remain for the build, which is too little to start it" >&2
    BH_START_REFUSED=75
    return 0
  fi
  echo "starting the build with RuntimeMaxSec=$runtime"
  # shellcheck disable=SC2029
  bh_rx 120 "$1" "bash $2/bin/nsc-build-remote.sh $2 $3 $runtime $4" < /dev/null || code=$?
  [ "$code" != 0 ] || return 0
  tmp=$(mktemp)
  # shellcheck disable=SC2029
  if bh_rx 90 "$1" "bash $2/bin/nsc-build-remote.sh --status $2 $3 0" < /dev/null > "$tmp"; then
    state=$(head -n 1 "$tmp")
  else
    state=unknown
  fi
  rm -f -- "$tmp"
  case $state in
    'refused '*)
      BH_START_REFUSED=${state#* }
      return 0
      ;;
    running | 'exited '* | gone) return 0 ;;
  esac
  return "$code"
}

# bh_wait HOST DIR UNIT: follow the detached build until it records its exit status, and
# return that status. The build log is copied as it grows. A poll that fails, as when the
# connection drops, is repeated with bh_retry's limits and never restarts the build. A build
# that was claimed but neither runs nor recorded a status, seen on two polls, has failed.
bh_wait() {
  local host=$1 dir=$2 unit=$3 offset=0 gone=0 tmp state got
  tmp=$(mktemp)
  while :; do
    # shellcheck disable=SC2029
    if ! bh_retry "polling the build on $host" "$tmp" bh_rx 90 "$host" "bash $dir/bin/nsc-build-remote.sh --status $dir $unit $offset" < /dev/null; then
      rm -f -- "$tmp"
      echo "lost contact with the build on $host; it may still be running in $unit.scope" >&2
      return 1
    fi
    state=$(head -n 1 "$tmp")
    got=$(($(wc -c < "$tmp") - ${#state} - 1))
    [ "$got" -le 0 ] || tail -c "$got" "$tmp"
    [ "$got" -le 0 ] || offset=$((offset + got))
    case $state in
      running) gone=0 ;;
      'exited '* | 'refused '*)
        # Copy the rest of the log before reporting the outcome.
        [ "$got" -lt 1048576 ] || continue
        rm -f -- "$tmp"
        return "${state#* }"
        ;;
      gone | unlaunched)
        gone=$((gone + 1))
        if [ "$gone" -ge 2 ]; then
          rm -f -- "$tmp"
          echo "the build scope $unit on $host ended without recording an exit status ($state)" >&2
          return 1
        fi
        ;;
      *)
        rm -f -- "$tmp"
        echo "unexpected build state '$state' from $host" >&2
        return 1
        ;;
    esac
    # A full chunk means more of the log is waiting.
    [ "$got" -ge 1048576 ] || sleep "${SPARKLES_NSC_BUILD_POLL_SECONDS:-15}"
  done
}

bh_build_steps() {
  local host=$1 sha=$2 out=$3 tmp args got want suffix
  shift 3
  BH_RHOST=$host
  BH_ADMISSION=
  case $host in boat:*)
    mkdir -p "$out"
    rx_admit "$host" --minimum "${SPARKLES_REMOTE_MIN_LIFETIME:-1800}" > "$out/admission.json" || return
    BH_ADMISSION=$out/admission.json
    ;;
  esac
  # The name is chosen here, so a retry after a lost reply reuses the same directory.
  suffix=$(LC_ALL=C tr -dc 'A-Za-z0-9' < /dev/urandom | head -c 8) || true
  tmp=$(mktemp)
  if ! bh_retry "creating the scratch directory on $host" "$tmp" bh_scratch "$host" "lanes/nsc-build/$sha.$suffix"; then
    rm -f -- "$tmp"
    echo "could not create the scratch directory on $host" >&2
    return 1
  fi
  BH_RDIR=$(cat "$tmp")
  rm -f -- "$tmp"
  # The path is used unquoted in remote commands and removed at the end, so it must be
  # exactly the directory named above.
  if ! [[ $BH_RDIR =~ ^/[A-Za-z0-9._/-]+/lanes/nsc-build/$sha\.$suffix$ ]]; then
    echo "unexpected scratch directory '$BH_RDIR' on $host; remove it there by hand" >&2
    BH_RDIR=
    return 1
  fi
  BH_RUNIT=nsc-build-${sha:0:12}-$suffix
  echo "remote scratch $host:$BH_RDIR, scope $BH_RUNIT"
  tmp=$(mktemp -d)
  cp "$BH_SELF_DIR/nsc-build.sh" "$BH_SELF_DIR/nsc-build-remote.sh" "$BH_SELF_DIR/cargo-sparkles-task" "$tmp/"
  echo "$BH_UI_PLACEHOLDER" > "$tmp/ui-index.html"
  BH_SEND_ATTEMPT=1
  if ! bh_retry "copying the build scripts to $host" - bh_send_scripts "$host" "$BH_RDIR" "$tmp"; then
    rm -rf -- "$tmp"
    echo "copying the build scripts to $host failed" >&2
    return 1
  fi
  rm -rf -- "$tmp"
  bh_retry "copying the source of $sha to $host" - bh_send_source "$host" "$BH_RDIR" "$sha" || {
    echo "copying the source of $sha to $host failed" >&2
    return 1
  }
  # printf would print a quoted empty word for no flags.
  args=
  [ "$#" = 0 ] || args=$(printf '%q ' "$@")
  # A start repeated after a lost reply finds DIR/launched and starts nothing.
  BH_START_REFUSED=
  bh_retry "starting the build on $host" - bh_start "$host" "$BH_RDIR" "$BH_RUNIT" "$args" || {
    echo "starting the build on $host failed" >&2
    return 1
  }
  if [ -n "$BH_START_REFUSED" ]; then
    echo "the build host $host refused the build with exit $BH_START_REFUSED" >&2
    return "$BH_START_REFUSED"
  fi
  local build_code=0
  bh_wait "$host" "$BH_RDIR" "$BH_RUNIT" || build_code=$?
  if [ "$build_code" != 0 ]; then
    echo "the build on $host failed with exit $build_code" >&2
    return "$build_code"
  fi
  mkdir -p "$out"
  bh_retry "copying the binary back from $host" - bh_fetch "$host" "$BH_RDIR" "$out" || {
    echo "copying the binary back from $host failed" >&2
    return 1
  }
  want=$(cat "$out/sha256" 2> /dev/null) || want=
  got=$(sha256sum "$out/sparkles" 2> /dev/null | cut -d' ' -f1) || got=
  if [ -z "$want" ] || [ "$got" != "$want" ]; then
    echo "the binary copied from $host does not match its SHA-256 there" >&2
    return 1
  fi
  { echo "build host: $host" && cat "$out/info.txt"; } > "$out/info.txt.tmp" && mv "$out/info.txt.tmp" "$out/info.txt"
  echo "built on $host, sha256 $got"
}
