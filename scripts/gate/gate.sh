#!/usr/bin/env bash
# Run the Sparkles batch gate for a commit on the gate hosts and bring the results back.
#
# Usage (from any checkout or worktree of the sparkles repo on the laptop):
#   gate.sh [options] <rev>            push, start, wait, fetch results, print the summary
#   gate.sh [options] --no-wait <rev>  push and start, then return (for agents with bounded waits)
#   gate.sh [--tag T] --status <rev>   print progress, steps and the last memory sample
#   gate.sh [--tag T] --fetch <rev>    copy and merge the out directories into
#                                      <main checkout>/.artifacts/gates/main-gate-<name>-<host>/
#   gate.sh --hosts                    print each gate host's lock, queue and free disk
#
# Options:
#   --host H     run the whole gate on H, with no conformance half elsewhere
#   --helper H   run the conformance half on H while ci runs on the primary host, or "none"
#   --auto       pick the hosts from their heavy locks and gate queues (see below)
#   --jobs N     pass --jobs N to scripts/ci.sh (default 1)
#   --tag T      name the run <short>-T, so a second run of the same commit can coexist
#
# By default scripts/ci.sh runs on wally while the conformance suites (test:w3c, test:shacl,
# test:shex and the plain and encrypted spill matrices) run on atlas for the same commit.
# If atlas cannot be reached, everything runs on wally in sequence and progress.log says
# so. The gate hosts cannot reach each other, so --status, --fetch and the waiting mode
# merge the two halves into one directory with one steps.txt and one gate.exit, which is
# 0 only when both halves pass. A fetch of a finished split gate also copies the merged
# files back into the primary host's ~/gate/out/<name>/.
#
# Host roles: SPARKLES_BENCH_HOST (default forge) names the benchmark host, which never
# runs gate work, not even when named with --host. GATE_HOSTS (default "wally atlas
# forge") lists the hosts that may run gates, and the benchmark host is dropped from it.
# GATE_PRIMARY (or the older WALLY_HOST) sets the default primary host, and GATE_HELPER
# the default helper, which is otherwise the first other gate host. GATE_HELPER=none
# disables the split, and a GATE_HELPER that does not resolve exercises the
# unreachable-helper fallback. These settings may also live in ~/.config/sparkles/gate.env
# (or $SPARKLES_GATE_CONFIG) as KEY=VALUE lines, and the environment takes precedence.
# Swapping the benchmark role from forge to atlas is SPARKLES_BENCH_HOST=atlas there, after
# scripts/gate/setup-host.sh forge has prepared forge.
#
# --auto checks each gate host's ~/gate/.lock (the same file as ~/lanes/.heavy.lock) and
# its gate-* units. With the primary and helper both idle it splits. Otherwise it runs the
# whole gate on an idle host that has the free disk for it, and with no idle host it
# splits as by default.
set -euo pipefail

self=$(readlink -f "${BASH_SOURCE[0]}")
here=$(dirname "$self")
# shellcheck source=scripts/gate/config.sh
. "$here/config.sh"
primary=${GATE_PRIMARY:-${WALLY_HOST:-${gate_hosts[0]:-wally}}}
default_helper() {
  local h
  for h in "${gate_hosts[@]}"; do [ "$h" = "$1" ] || {
    echo "$h"
    return
  }; done
  echo none
}
helper=${GATE_HELPER-$(default_helper "$primary")}
mode="wait"
auto=0
jobs=1
tag=
host_set=0
helper_set=0
ssh_opts=(-o BatchMode=yes -o ConnectTimeout=10)

usage() { sed -n '2,/^set -euo/p' "$self" | sed '$d; s/^# \{0,1\}//'; }
die() {
  echo "gate.sh: $*" >&2
  exit 2
}

while [ $# -gt 0 ]; do
  case $1 in
    --no-wait) mode=nowait ;;
    --status) mode=status ;;
    --fetch) mode=fetch ;;
    --hosts) mode=hosts ;;
    --auto) auto=1 ;;
    --host)
      primary=${2:?--host needs a host}
      host_set=1
      shift
      ;;
    --helper)
      helper=${2:?--helper needs a host or none}
      helper_set=1
      shift
      ;;
    --jobs)
      jobs=${2:?--jobs needs a number}
      shift
      ;;
    --tag)
      tag=${2:?--tag needs a name}
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    --*) die "unknown option $1" ;;
    *) break ;;
  esac
  shift
done
if [ $mode = hosts ]; then :; elif [ $# -ne 1 ]; then
  usage
  exit 0
fi
case $jobs in '' | *[!0-9]* | 0) die "bad --jobs $jobs" ;; esac
case $tag in *[!A-Za-z0-9._-]*) die "bad --tag $tag" ;; esac
if [ $host_set = 1 ] && [ $helper_set = 0 ]; then helper=none; fi
[ "$helper" = "$primary" ] && helper=none

root=$(cd "$(git rev-parse --path-format=absolute --git-common-dir)/.." && pwd)
statedir=$root/.artifacts/gates/state

# Lock state, gate units and free disk of a gate host as "<lock> <units> <free-GiB>", where
# lock is free or held. Prints "unreachable" when ssh fails.
probe() {
  # shellcheck disable=SC2016
  ssh "${ssh_opts[@]}" "$1" '
    if flock -n "$HOME/gate/.lock" true; then l=free; else l=held; fi
    u=$(systemctl --user list-units "gate-*" --state=active --plain --no-legend | wc -l)
    d=$(df -BG --output=avail "$HOME" | tail -n1 | tr -dc 0-9)
    echo "$l $u $d"' 2> /dev/null || echo unreachable
}

if [ $mode = hosts ]; then
  echo "benchmark host: $bench (never runs gates)"
  for h in "${gate_hosts[@]}"; do printf '%-6s %s\n' "$h" "$(probe "$h")"; done
  exit 0
fi

rev=$1
full=$(git rev-parse --verify "$rev^{commit}")
short=${full:0:8}
name=$short${tag:+-$tag}
statefile=$statedir/$name.env

# Status and fetch read the hosts recorded when the gate started. A gate started by the
# older wally-gate.sh has no record. It ran whole on WALLY_HOST when that is set, and
# otherwise on the gate host whose out directory for it is newest.
if [ $mode = status ] || [ $mode = fetch ]; then
  if [ -f "$statefile" ]; then
    # shellcheck disable=SC1090
    . "$statefile"
  else
    helper=none
    hname=
    if [ -z "${GATE_PRIMARY:-${WALLY_HOST:-}}" ]; then
      best=0
      for h in "${gate_hosts[@]}"; do
        # shellcheck disable=SC2029
        t=$(ssh "${ssh_opts[@]}" "$h" "stat -c %Y ~/gate/out/$name/progress.log" 2> /dev/null) || continue
        if [ "${t:-0}" -gt "$best" ]; then
          best=$t
          primary=$h
        fi
      done
    fi
  fi
fi

refuse_bench() {
  local h
  for h in "$primary" "$helper"; do
    if is_bench "$h"; then die "$h is the benchmark host (SPARKLES_BENCH_HOST) and never runs gate work"; fi
  done
}
refuse_bench

pout=gate/out/$name
hname=${hname:-$name-conf}
hout=gate/out/$hname
# The results directory names the host that ran ci. Older drivers always used -wally.
dest=$root/.artifacts/gates/main-gate-$name-$primary

# Installs scripts/gate/remote/ on a host under ~/gate/bin/v-<hash>/ and prints that path.
# A content-addressed directory leaves running gates and older callers' scripts untouched.
install_remote() {
  local h=$1 v
  v=$(cat "$here/remote/gate-start.sh" "$here/remote/gate-inner.sh" | sha256sum | cut -c1-12)
  # shellcheck disable=SC2029
  tar -C "$here/remote" -cf - gate-start.sh gate-inner.sh | ssh "${ssh_opts[@]}" "$h" "
    d=\$HOME/gate/bin/v-$v
    if [ ! -x \"\$d/gate-start.sh\" ]; then
      t=\$(mktemp -d \"\$HOME/gate/bin/.v-$v.XXXXXX\")
      tar -C \"\$t\" -xf - && chmod +x \"\$t\"/*.sh && { mv -T \"\$t\" \"\$d\" 2> /dev/null || rm -rf \"\$t\"; }
    else
      cat > /dev/null
    fi
    test -x \"\$d/gate-start.sh\""
  echo "gate/bin/v-$v"
}

# start_on HOST NAME PART [NOTE]
start_on() {
  local h=$1 n=$2 p=$3 note=${4:-} bin
  git push -q "ssh://$h/home/$USER/gate/repo.git" "$full:refs/heads/gate-$short"
  bin=$(install_remote "$h")
  # shellcheck disable=SC2029
  ssh "${ssh_opts[@]}" "$h" "\$HOME/$bin/gate-start.sh $full --name $n --part $p --jobs $jobs --note $(printf '%q' "$note")"
}

# 0 when the run NAME on HOST has finished: it wrote its exit file or its unit stopped.
finished() {
  # shellcheck disable=SC2029
  ssh "${ssh_opts[@]}" "$1" "test -e ~/gate/out/$2/gate.exit || test -e ~/gate/out/$2/part.exit || ! systemctl --user is-active -q gate-$2"
}

status_of() {
  # shellcheck disable=SC2029
  ssh "${ssh_opts[@]}" "$1" "cat ~/gate/out/$2/progress.log; echo; cat ~/gate/out/$2/steps.txt 2>/dev/null; tail -n1 ~/gate/out/$2/mempeak.log 2>/dev/null; systemctl --user is-active gate-$2 || true"
}

status() {
  if [ "$helper" = none ]; then
    status_of "$primary" "$name"
    return
  fi
  local ps hs
  ps=$(status_of "$primary" "$name" || echo "unreachable")
  hs=$(status_of "$helper" "$hname" || echo "unreachable")
  # Once both halves have stopped, merge them so that the primary holds gate.exit.
  if [ "$(echo "$ps" | tail -n1)" != active ] && [ "$(echo "$hs" | tail -n1)" != active ]; then
    fetch > /dev/null 2>&1 || true
    ps=$(status_of "$primary" "$name" || echo "unreachable")
  fi
  echo "== ci on $primary"
  echo "$ps" | sed '$d'
  echo "== conformance on $helper"
  echo "$hs" | sed '$d'
  # The last line stays the unit state, as before: active while either half runs.
  if [ "$(echo "$ps" | tail -n1)" = active ] || [ "$(echo "$hs" | tail -n1)" = active ]; then
    echo active
  else
    echo "$ps" | tail -n1
  fi
}

# Elapsed seconds from the earliest queued_at to the latest done_at in the given dirs.
wall() {
  local q=() d=() dir
  for dir in "$@"; do
    [ -f "$dir/queued_at" ] && q+=("$(cat "$dir/queued_at")")
    [ -f "$dir/done_at" ] && d+=("$(cat "$dir/done_at")")
  done
  [ ${#q[@]} -gt 0 ] && [ ${#d[@]} -gt 0 ] || return 0
  local qmin dmax
  qmin=$(printf '%s\n' "${q[@]}" | sort -n | head -n1)
  dmax=$(printf '%s\n' "${d[@]}" | sort -n | tail -n1)
  echo "wall $((dmax - qmin))s ($(((dmax - qmin) / 60)) min) from queue to last step"
}

fetch() {
  mkdir -p "$dest"
  if [ "$helper" = none ]; then
    # --delete keeps an earlier run's gate.exit from standing in for a rerun's result.
    rsync -a --delete "$primary:$pout/" "$dest/"
    wall "$dest" > "$dest/wall.txt"
    echo "results in $dest"
    return 0
  fi
  # A split gate whose merge was already copied back to the primary needs no new merge.
  # shellcheck disable=SC2029
  if ssh "${ssh_opts[@]}" "$primary" "test -e ~/$pout/gate.exit"; then
    rsync -a --delete "$primary:$pout/" "$dest/"
    echo "results in $dest"
    return 0
  fi
  local tmp conf code=
  tmp=$(mktemp -d "$root/.artifacts/gates/.merge-$name.XXXXXX")
  conf=$tmp/conf
  mkdir -p "$conf"
  rsync -a "$primary:$pout/" "$tmp/"
  if ! rsync -a "$helper:$hout/" "$conf/"; then
    rsync -a "$tmp/" "$dest/"
    rm -rf "$tmp"
    echo "conformance half on $helper is unreachable; ci results in $dest" >&2
    return 98
  fi
  # The helper's step logs and exit files sit beside the primary's. Its progress, memory
  # and environment records keep a -conf suffix, and steps.txt lists both halves.
  for f in "$conf"/*.log "$conf"/*.exit; do
    [ -e "$f" ] || continue
    case ${f##*/} in progress.log | mempeak.log | part.exit) ;; *) cp -a "$f" "$tmp/" ;; esac
  done
  for f in progress.log mempeak.log env.txt mempeak.final; do
    [ -f "$conf/$f" ] && cp -a "$conf/$f" "$tmp/${f%.*}-conf.${f##*.}"
  done
  [ -f "$tmp/steps.txt" ] && mv "$tmp/steps.txt" "$tmp/steps-ci.txt"
  cat "$tmp/steps-ci.txt" "$conf/steps.txt" > "$tmp/steps.txt" 2> /dev/null || true
  {
    echo "split: ci on $primary (gate-$name), conformance on $helper (gate-$hname)"
    wall "$tmp" "$conf"
  } > "$tmp/wall.txt"
  if [ -f "$tmp/part.exit" ] && [ -f "$conf/part.exit" ]; then
    code=0
    for f in "$tmp/part.exit" "$conf/part.exit"; do [ "$(cat "$f")" = 0 ] || code=1; done
  elif finished "$primary" "$name" && finished "$helper" "$hname"; then
    # Both units have stopped, but a half may have written its exit file after the copy
    # above. Look again before taking a missing file as a run that stopped without one.
    # shellcheck disable=SC2029
    if ssh "${ssh_opts[@]}" "$primary" "test -e ~/$pout/part.exit" &&
      ssh "${ssh_opts[@]}" "$helper" "test -e ~/$hout/part.exit"; then
      rm -rf "$tmp"
      fetch
      return
    fi
    code=99
  fi
  if [ -n "$code" ]; then
    echo "$code" > "$tmp/gate.exit"
    # Keep the primary's out directory complete for anyone reading it on the host.
    rsync -a "$tmp/" "$primary:$pout/"
  fi
  rsync -a --delete "$tmp/" "$dest/"
  rm -rf "$tmp"
  echo "results in $dest"
}

case $mode in
  status)
    status
    exit 0
    ;;
  fetch)
    fetch
    exit 0
    ;;
esac

if [ $auto = 1 ]; then
  # idle[h]=1 when h is reachable with its lock free and no gate units; full[h]=1 when h has
  # the disk for a whole gate and half[h]=1 when it has the disk for a conformance half.
  declare -A idle=() full=() half=()
  report=
  for h in "${gate_hosts[@]}"; do
    p=$(probe "$h")
    report+="$h [$p] "
    read -r l u d <<< "$p" || true
    idle[$h]=0 full[$h]=0 half[$h]=0
    [ "$l" = unreachable ] && continue
    [ "$l" = free ] && [ "${u:-0}" = 0 ] && idle[$h]=1
    [ "${d:-0}" -ge "${GATE_MIN_FREE_GB:-40}" ] && full[$h]=1
    [ "${d:-0}" -ge "${GATE_MIN_FREE_HALF_GB:-20}" ] && half[$h]=1
  done
  chosen=
  if [ "$helper" != none ] && [ "${idle[$primary]:-0}" = 1 ] && [ "${full[$primary]:-0}" = 1 ] &&
    [ "${idle[$helper]:-0}" = 1 ] && [ "${half[$helper]:-0}" = 1 ]; then
    chosen="split"
  else
    for h in "$primary" "${gate_hosts[@]}"; do
      if [ "${idle[$h]:-0}" = 1 ] && [ "${full[$h]:-0}" = 1 ]; then
        primary=$h helper=none chosen=whole
        break
      fi
    done
  fi
  if [ -z "$chosen" ] && [ "$helper" != none ] && [ "${half[$helper]:-0}" = 0 ]; then helper=none; fi
  echo "auto: ${report}-> primary $primary helper $helper"
  refuse_bench
  dest=$root/.artifacts/gates/main-gate-$name-$primary
fi

note=
if [ "$helper" != none ] && ! ssh "${ssh_opts[@]}" "$helper" true 2> /dev/null; then
  note="helper $helper is unreachable; running the conformance suites on $primary after ci"
  echo "$note" >&2
  helper=none
fi
if [ "$helper" != none ]; then
  if ! start_on "$helper" "$hname" conformance >&2; then
    note="starting the conformance half on $helper failed; running it on $primary after ci"
    echo "$note" >&2
    helper=none
  fi
fi
if [ "$helper" = none ]; then
  [ -n "$note" ] || note="whole gate on $primary"
  start_on "$primary" "$name" all "$note"
else
  start_on "$primary" "$name" ci "split: conformance half runs on $helper as gate-$hname"
fi
mkdir -p "$statedir"
printf 'primary=%s\nhelper=%s\nhname=%s\njobs=%s\n' "$primary" "$helper" "$hname" "$jobs" > "$statefile"
if [ "$helper" = none ]; then
  echo "gate-$name started on $primary; status: $0 ${tag:+--tag $tag }--status $short"
else
  echo "gate-$name started: ci on $primary, conformance on $helper as gate-$hname; status: $0 ${tag:+--tag $tag }--status $short"
fi

if [ $mode = nowait ]; then
  echo "fetch when done: $0 ${tag:+--tag $tag }--fetch $short"
  exit 0
fi
while ! finished "$primary" "$name" || { [ "$helper" != none ] && ! finished "$helper" "$hname"; }; do
  sleep 120
done
fetch
cat "$dest/steps.txt" 2> /dev/null
cat "$dest/wall.txt" 2> /dev/null
code=$(cat "$dest/gate.exit" 2> /dev/null || echo 99)
echo "gate exit $code"
exit "$code"
