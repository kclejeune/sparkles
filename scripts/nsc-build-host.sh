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
# reached, the run stops. Without --build-host the run uses auto when
# SPARKLES_NSC_BUILD_HOSTS is set and local otherwise, so a machine without the setting
# builds as it always has. The variables come from the environment, else from KEY=VALUE
# lines in ${SPARKLES_NSC_BUILD_CONFIG:-~/.config/sparkles/nsc-build.env}. The benchmark
# host (SPARKLES_BENCH_HOST, see scripts/gate/config.sh) never builds and is refused.
#
# A remote build ships the `git archive` of the commit, with the placeholder ui/build that a
# local build gets, to the scratch directory ~/lanes/nsc-build/<commit>.<random> on the
# host. It runs scripts/nsc-build-remote.sh there, which builds with scripts/nsc-build.sh
# under mbx in a memory-capped systemd scope named nsc-build-<commit>-<random>, while it
# holds the host's heavy lock. The binary and its build record come back over ssh and the
# SHA-256 is checked. The scratch directory is removed when the build ends in any way:
# after a failure, and from the caller's EXIT trap (bh_cleanup) after an interrupt, which
# also stops the scope. Cleanup touches only the directory and scope this run created.

BH_SELF_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BH_ROOT=$(cd "$BH_SELF_DIR/.." && pwd)
# shellcheck source=scripts/gate/config.sh
. "$BH_SELF_DIR/gate/config.sh"
BH_UI_PLACEHOLDER='<!doctype html><title>Sparkles</title><p>The UI was not built for this benchmark binary.</p>'
bh_ssh_opts=(-o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=30)
# The --build-host argument, the resolved choice (local or a host) and the remote state
# that bh_cleanup removes.
BH_ARG=
BH_HOST=
BH_RHOST=
BH_RDIR=
BH_RUNIT=

# bh_ui_placeholder DIR: the server embeds ui/build. Its build script writes a placeholder,
# but does not rerun for a second source tree in a shared target directory.
bh_ui_placeholder() {
  mkdir -p "$1/ui/build"
  echo "$BH_UI_PLACEHOLDER" > "$1/ui/build/index.html"
}

# bh_check_arg: refuse a malformed --build-host before any work
bh_check_arg() {
  case $BH_ARG in
    '' | local | auto) ;;
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
  ssh "${bh_ssh_opts[@]}" "$1" '
    if [ -e "$HOME/lanes/.heavy.lock" ] && ! flock -n "$HOME/lanes/.heavy.lock" true; then l=held; else l=free; fi
    u=$(systemctl --user list-units "gate-*" --state=active --plain --no-legend 2> /dev/null | wc -l)
    d=$(df -BG --output=avail "$HOME" | tail -n1 | tr -dc 0-9)
    echo "$l $u $d"' < /dev/null 2> /dev/null || echo unreachable
}

# bh_resolve: set BH_HOST to local or a build host, once per run, and say which
bh_resolve() {
  [ -z "$BH_HOST" ] || return 0
  local want=$BH_ARG h p l u d min fallback='' hosts=()
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

bh_mbx_pin() {
  sed -n 's/^mr-boxington = "\(.*\)"$/\1/p' "$BH_ROOT/mise.toml"
}

# bh_cleanup: stop the scope and remove the scratch directory of an unfinished remote build
bh_cleanup() {
  [ -n "$BH_RDIR" ] || return 0
  # shellcheck disable=SC2029 # the unit and directory are this run's and expand here
  ssh "${bh_ssh_opts[@]}" "$BH_RHOST" "systemctl --user stop $BH_RUNIT > /dev/null 2>&1; rm -rf -- $BH_RDIR" < /dev/null ||
    echo "could not remove $BH_RHOST:$BH_RDIR; remove it and stop the unit $BH_RUNIT there" >&2
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
  bh_cleanup
  return "$rc"
}

bh_build_steps() {
  local host=$1 sha=$2 out=$3 pin tmp args got want
  shift 3
  pin=$(bh_mbx_pin)
  BH_RHOST=$host
  # shellcheck disable=SC2029 # the commit expands here
  BH_RDIR=$(ssh "${bh_ssh_opts[@]}" "$host" "mkdir -p \$HOME/lanes/nsc-build && mktemp -d \$HOME/lanes/nsc-build/$sha.XXXXXXXX" < /dev/null) || {
    BH_RDIR=
    echo "could not create the scratch directory on $host" >&2
    return 1
  }
  # The path is used unquoted in remote commands and removed at the end, so it must be
  # exactly the directory mktemp made.
  if ! [[ $BH_RDIR =~ ^/[A-Za-z0-9._/-]+/lanes/nsc-build/$sha\.[A-Za-z0-9]+$ ]]; then
    echo "unexpected scratch directory '$BH_RDIR' on $host; remove it there by hand" >&2
    BH_RDIR=
    return 1
  fi
  BH_RUNIT=nsc-build-${sha:0:12}-${BH_RDIR##*.}
  echo "remote scratch $host:$BH_RDIR, scope $BH_RUNIT"
  tmp=$(mktemp -d)
  cp "$BH_SELF_DIR/nsc-build.sh" "$BH_SELF_DIR/nsc-build-remote.sh" "$BH_SELF_DIR/cargo-sparkles-task" "$tmp/"
  echo "$BH_UI_PLACEHOLDER" > "$tmp/ui-index.html"
  # shellcheck disable=SC2029
  if ! tar -C "$tmp" -cf - . | ssh "${bh_ssh_opts[@]}" "$host" "mkdir $BH_RDIR/bin $BH_RDIR/src && tar -x -C $BH_RDIR/bin"; then
    rm -rf -- "$tmp"
    echo "copying the build scripts to $host failed" >&2
    return 1
  fi
  rm -rf -- "$tmp"
  # Source mtimes stay current, as in a local build.
  # shellcheck disable=SC2029
  git -C "$BH_ROOT" archive "$sha" | ssh "${bh_ssh_opts[@]}" "$host" \
    "tar -x -m -C $BH_RDIR/src && mkdir -p $BH_RDIR/src/ui/build && mv $BH_RDIR/bin/ui-index.html $BH_RDIR/src/ui/build/index.html" || {
    echo "copying the source of $sha to $host failed" >&2
    return 1
  }
  args=$(printf '%q ' "$BH_RDIR" "$BH_RUNIT" "$pin" "$@")
  # shellcheck disable=SC2029
  ssh "${bh_ssh_opts[@]}" "$host" "bash $BH_RDIR/bin/nsc-build-remote.sh $args" < /dev/null || {
    echo "the build on $host failed" >&2
    return 1
  }
  mkdir -p "$out"
  # shellcheck disable=SC2029
  ssh "${bh_ssh_opts[@]}" "$host" "tar -C $BH_RDIR/out -cf - ." < /dev/null | tar -x -C "$out" || {
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
