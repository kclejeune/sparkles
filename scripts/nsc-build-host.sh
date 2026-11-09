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
# SPARKLES_NSC_BUILD_RUNTIME (default 14400), or less for a Boat worker with less admitted
# lifetime. The binary and its build record come back over ssh and the SHA-256 is checked.
# The scratch directory is removed when the build ends in any way: after a failure, and
# from the caller's EXIT trap (bh_cleanup) after an interrupt or lost contact, which also
# stops the scope. Cleanup touches only the directory and scope this run created.

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

# bh_retry DESCRIPTION COMMAND...: run COMMAND until it succeeds, at most
# SPARKLES_NSC_BUILD_RETRIES times (default 8), waiting SPARKLES_NSC_BUILD_RETRY_SECONDS
# (default 20) between attempts. Every remote step it wraps can be repeated after a dropped
# connection without changing what an earlier attempt did.
bh_retry() {
  local what=$1 n=1 max=${SPARKLES_NSC_BUILD_RETRIES:-8} code
  shift
  while :; do
    code=0
    "$@" || code=$?
    [ "$code" != 0 ] || return 0
    if [ "$n" -ge "$max" ]; then
      echo "$what failed $n times, the last with exit $code" >&2
      return "$code"
    fi
    echo "$what failed with exit $code (attempt $n of $max), retrying" >&2
    n=$((n + 1))
    sleep "${SPARKLES_NSC_BUILD_RETRY_SECONDS:-20}"
  done
}

# bh_cleanup: stop the scope and remove the scratch directory of an unfinished remote build
bh_cleanup() {
  [ -n "$BH_RDIR" ] || return 0
  # shellcheck disable=SC2029 # the unit and directory are this run's and expand here
  if ! bh_retry "stopping $BH_RUNIT.scope" rx_exec "$BH_RHOST" "systemctl --user stop $BH_RUNIT.scope > /dev/null 2>&1; ! systemctl --user is-active --quiet $BH_RUNIT.scope" < /dev/null; then
    echo "could not confirm scope closure at $BH_RHOST:$BH_RDIR; keep its evidence and reconcile $BH_RUNIT.scope" >&2
    return 97
  fi
  case $BH_RHOST in boat:*) echo "retained source/build evidence at $BH_RHOST:$BH_RDIR" ;;
  *) bh_retry "removing $BH_RDIR" rx_exec "$BH_RHOST" "rm -rf -- $BH_RDIR" < /dev/null ;;
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
  rx_exec "$1" "mkdir -p \$HOME/lanes/nsc-build && mkdir -p \$HOME/$2 && cd \$HOME/$2 && pwd -P" < /dev/null
}

# bh_send_scripts HOST DIR TMP: copy the build scripts into DIR/bin.
bh_send_scripts() {
  # shellcheck disable=SC2029
  tar -C "$3" -cf - . | rx_exec "$1" "mkdir -p $2/bin && tar -x -C $2/bin"
}

# bh_send_source HOST DIR COMMIT: extract the commit's archive into DIR/src. Each attempt
# extracts into its own directory and renames it to DIR/src only when complete, so an
# attempt whose connection dropped can neither leave a partial tree nor disturb a later one.
bh_send_source() {
  local part=$2/src.part.$BH_SEND_ATTEMPT
  BH_SEND_ATTEMPT=$((BH_SEND_ATTEMPT + 1))
  # Source mtimes stay current, as in a local build.
  # shellcheck disable=SC2029
  git -C "$BH_ROOT" archive "$3" | rx_exec "$1" \
    "if [ -d $2/src ]; then cat > /dev/null; exit 0; fi; rm -rf -- $part && mkdir $part && tar -x -m -C $part && mkdir -p $part/ui/build && cp $2/bin/ui-index.html $part/ui/build/index.html && mv -T $part $2/src"
}

# bh_fetch HOST DIR OUT: copy DIR/out back into OUT.
bh_fetch() {
  # shellcheck disable=SC2029
  rx_exec "$1" "tar -C $2/out -cf - ." < /dev/null | python3 "$RX_DIR/remote-executor.py" unpack "$3"
}

# bh_start HOST DIR UNIT ARGS: start the detached build. When the reply is lost, the build's
# state decides. A build that started, even one that already ended, counts as started, and a
# start that the host refused sets BH_START_REFUSED to its exit status, so neither is
# started again.
bh_start() {
  local code=0 state tmp
  # shellcheck disable=SC2029
  rx_exec "$1" "bash $2/bin/nsc-build-remote.sh $4" < /dev/null || code=$?
  [ "$code" != 0 ] || return 0
  tmp=$(mktemp)
  # shellcheck disable=SC2029
  if rx_exec "$1" "bash $2/bin/nsc-build-remote.sh --status $2 $3 0" < /dev/null > "$tmp"; then
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
# connection drops, is repeated with bh_retry's limits and never restarts the build. A scope
# that ended without an exit status, seen on two polls, is a failed build.
bh_wait() {
  local host=$1 dir=$2 unit=$3 offset=0 gone=0 tmp state got
  tmp=$(mktemp)
  while :; do
    # shellcheck disable=SC2029
    if ! bh_retry "polling the build on $host" rx_exec "$host" "bash $dir/bin/nsc-build-remote.sh --status $dir $unit $offset" < /dev/null > "$tmp"; then
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
    sleep "${SPARKLES_NSC_BUILD_POLL_SECONDS:-15}"
  done
}

bh_build_steps() {
  local host=$1 sha=$2 out=$3 tmp args got want suffix runtime
  shift 3
  BH_RHOST=$host
  runtime=${SPARKLES_NSC_BUILD_RUNTIME:-14400}
  case $host in boat:*)
    mkdir -p "$out"
    rx_admit "$host" --minimum "${SPARKLES_REMOTE_MIN_LIFETIME:-1800}" > "$out/admission.json" || return
    # The scope ends no later than the admitted lifetime.
    got=$(rx_runtime "$out/admission.json" --reserve 60) || return
    [ "$got" -ge "$runtime" ] || runtime=$got
    ;;
  esac
  # The name is chosen here, so a retry after a lost reply reuses the same directory.
  suffix=$(LC_ALL=C tr -dc 'A-Za-z0-9' < /dev/urandom | head -c 8) || true
  BH_RDIR=$(bh_retry "creating the scratch directory on $host" bh_scratch "$host" "lanes/nsc-build/$sha.$suffix") || {
    BH_RDIR=
    echo "could not create the scratch directory on $host" >&2
    return 1
  }
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
  if ! bh_retry "copying the build scripts to $host" bh_send_scripts "$host" "$BH_RDIR" "$tmp"; then
    rm -rf -- "$tmp"
    echo "copying the build scripts to $host failed" >&2
    return 1
  fi
  rm -rf -- "$tmp"
  BH_SEND_ATTEMPT=1
  bh_retry "copying the source of $sha to $host" bh_send_source "$host" "$BH_RDIR" "$sha" || {
    echo "copying the source of $sha to $host failed" >&2
    return 1
  }
  args=$(printf '%q ' "$BH_RDIR" "$BH_RUNIT" "$runtime" "$@")
  # A start repeated after a lost reply finds DIR/launched and starts nothing.
  BH_START_REFUSED=
  bh_retry "starting the build on $host" bh_start "$host" "$BH_RDIR" "$BH_RUNIT" "$args" || {
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
  bh_retry "copying the binary back from $host" bh_fetch "$host" "$BH_RDIR" "$out" || {
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
