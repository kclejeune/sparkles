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
#   --host H        SSH host or admitted boat:bx_ID; run ci on H (default GATE_PRIMARY or the first gate host)
#   --helper H      run the conformance half on H while ci runs on the primary host, or
#                   "none" to run the whole gate on the primary host
#   --auto          pick the hosts from their heavy locks and gate queues (see below)
#   --base REV      compare the commit with its merge base with REV and run only the
#                   checks the changed files call for (default GATE_BASE or origin/main)
#   --full          run every check with --force and no selection, as the nightly gate does
#   --jobs N        pass --jobs N to scripts/ci.sh (default 1)
#   --cargo-jobs N  set CARGO_BUILD_JOBS on every host of the run (see below)
#   --tag T         name the run <short>-T, so a second run of the same commit can coexist
#   --dry-run       print the chosen hosts and checks and stop before pushing or starting
#   --link-only     run only the server artifact link check (scripts/server-artifact-link-check.sh)
#                   on the host that --host names, which needs podman. The nightly gate
#                   runs it beside the full gate (scripts/gate/nightly.sh).
#
# The gate host chooses the checks from the files that changed between the merge base and
# the commit, with the commit's scripts/gate/select-checks.py, and records them in
# selection.txt. The task cache of mise then skips a chosen check whose inputs already
# passed on that host. Build configuration, the CI scripts and files that no rule covers
# run everything. --full and the nightly gate (scripts/gate/nightly.sh) run every check.
#
# scripts/ci.sh runs on the primary host. When the batch changes code that the
# conformance suites (test:w3c, test:shacl, test:shex and the plain and encrypted spill
# matrices) check, they run on a second gate host for the same commit, the first one
# that is free: its heavy lock is free, it runs no gate unit and it has the disk for a
# conformance half. With no free host, or with --helper none, they run on the primary host
# after ci. If the helper cannot be reached, everything runs on the primary host in
# sequence and progress.log says so. The gate hosts cannot reach each other, so --status, --fetch and the waiting mode
# merge the two halves into one directory with one steps.txt and one gate.exit, which is
# 0 only when both halves pass. A fetch of a finished split gate also copies the merged
# files back into the primary host's ~/gate/out/<name>/.
#
# Host roles: SPARKLES_BENCH_HOST (default forge) names the benchmark host, which never
# runs gate work, not even when named with --host. GATE_HOSTS (default "wally") lists the hosts that may run gates, and the benchmark host is dropped from it.
# GATE_PRIMARY (or the older WALLY_HOST) sets the default primary host. GATE_HELPER names
# a helper to use whether or not it is free, GATE_HELPER=none disables the split, and a
# GATE_HELPER that does not resolve exercises the unreachable-helper fallback.
#
# Compile jobs: --cargo-jobs applies to every host. Otherwise GATE_CARGO_JOBS_HOSTS, such
# as "hostA=6 hostB=8", sets them per host, and GATE_CARGO_JOBS sets them for the other
# hosts. Without any of these, each host derives them from its gate memory cap
# (scripts/gate/remote/gate-start.sh).
#
# These settings may also live in ~/.config/sparkles/gate.env (or $SPARKLES_GATE_CONFIG)
# as KEY=VALUE lines, and the environment takes precedence.
# Swapping the benchmark role from forge to atlas is SPARKLES_BENCH_HOST=atlas there, after
# scripts/gate/setup-host.sh forge has prepared forge.
#
# --auto checks each gate host's ~/gate/.lock (the same file as ~/lanes/.heavy.lock) and
# its gate-* units. With the primary and helper both idle it splits. Otherwise it runs the
# whole gate on an idle host that has the free disk for it. With no idle host it puts ci on
# the host with the fewest gate units and splits only onto a helper whose queue is no
# longer.
#
# Before it starts, the driver checks each chosen host's free disk: GATE_NEED_CI_GB
# (default 30) for a ci half or a whole gate and GATE_NEED_CONF_GB (default 15) for a
# conformance half. A host that is short runs `mbx gc` first, and the driver refuses to
# start when it is still short.
#
# Before it starts, the driver also prints the newest finished run of each GitHub workflow
# on main (scripts/gate/github-runs.py) and warns when one failed, since those workflows
# run on GitHub and not in the gate. GATE_GITHUB_RUNS=0 skips the report.
set -euo pipefail

self=$(readlink -f "${BASH_SOURCE[0]}")
here=$(dirname "$self")
# shellcheck source=scripts/gate/config.sh
. "$here/config.sh"
# shellcheck source=scripts/remote-executor.sh
. "$here/../remote-executor.sh"
primary=${GATE_PRIMARY:-${WALLY_HOST:-${gate_hosts[0]:-wally}}}
default_helper() {
  local h
  for h in "${gate_hosts[@]}"; do [ "$h" = "$1" ] || {
    echo "$h"
    return
  }; done
  echo none
}
helper=${GATE_HELPER:-}
mode="wait"
auto=0
dry=0
jobs=1
cargo_jobs=
base_rev=${GATE_BASE:-origin/main}
all_checks=0
link_only=0
tag=
helper_set=0

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
    --dry-run) dry=1 ;;
    --host)
      primary=${2:?--host needs a host}
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
    --cargo-jobs)
      cargo_jobs=${2:?--cargo-jobs needs a number}
      shift
      ;;
    --base)
      base_rev=${2:?--base needs a revision}
      shift
      ;;
    --full) all_checks=1 ;;
    --link-only) link_only=1 ;;
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
case $cargo_jobs in *[!0-9]* | 0) die "bad --cargo-jobs $cargo_jobs" ;; esac
case $tag in *[!A-Za-z0-9._-]*) die "bad --tag $tag" ;; esac
if [ $link_only = 1 ]; then
  [ $auto = 0 ] || die "--link-only runs on the host that --host names, so it does not take --auto"
  [ $mode != hosts ] || die "--link-only does not take --hosts"
  helper=none
  helper_set=1
fi
case $primary in boat:*) if [ "$helper_set" = 0 ] && [ -z "${GATE_HELPER+x}" ]; then
  helper=none
  helper_set=1
fi ;; esac

# A helper named by --helper or GATE_HELPER is used as given. Otherwise the run picks a
# free one below, once it knows whether the batch needs the conformance suites.
pick_helper=0
if [ $helper_set = 0 ] && [ -z "${GATE_HELPER+x}" ]; then pick_helper=1; fi
[ "$helper" = "$primary" ] && helper=none

root=$(cd "$(git rev-parse --path-format=absolute --git-common-dir)/.." && pwd)
statedir=$root/.artifacts/gates/state

# Lock state, gate units and free disk of a gate host as "<lock> <units> <free-GiB>", where
# lock is free or held. Prints "unreachable" when ssh fails.
probe() {
  # shellcheck disable=SC2016
  rx_exec "$1" '
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
        t=$(rx_exec "$h" "stat -c %Y ~/gate/out/$name/progress.log" 2> /dev/null) || continue
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
  tar -C "$here/remote" -cf - gate-start.sh gate-inner.sh | rx_exec "$h" "
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
  local h=$1 n=$2 p=$3 note=${4:-} bin args cj
  mkdir -p "$statedir"
  printf 'host=%s\nname=%s\npart=%s\nsource=%s\n' "$h" "$n" "$p" "$full" > "$statedir/start-intent-$n"
  case $h in
    boat:*)
      rx_admit "$h" --minimum "${SPARKLES_REMOTE_MIN_LIFETIME:-7200}" > "$statedir/admission-$n.json" || return
      rx_bundle "$h" "$full" "$mb" --record "$statedir/source-$n.json" || return
      ;;
    *) git push -q "ssh://$h/home/$USER/gate/repo.git" "$full:refs/heads/gate-$short" ;;
  esac
  bin=$(install_remote "$h")
  args="--name $n --part $p --jobs $jobs"
  case $h in boat:*)
    local runtime
    runtime=$(rx_runtime "$statedir/admission-$n.json" --reserve 60) || return
    args+=" --executor boat --runtime $runtime"
    ;;
  esac
  cj=$(cargo_jobs_for "$h")
  case $cj in '' | *[!0-9]* | 0) ;; *) args+=" --cargo-jobs $cj" ;; esac
  [ -z "$mb" ] || args+=" --base $mb"
  [ $all_checks = 0 ] || args+=" --full"
  # shellcheck disable=SC2029
  printf 'phase=start-requested\n' >> "$statedir/start-intent-$n"
  rx_exec "$h" "\$HOME/$bin/gate-start.sh $full $args --note $(printf '%q' "$note")"
}

# 0 when the run NAME on HOST has finished: it wrote its exit file or its unit stopped.
finished() {
  # shellcheck disable=SC2029
  rx_exec "$1" "test -e ~/gate/out/$2/gate.exit || test -e ~/gate/out/$2/part.exit || ! systemctl --user is-active -q gate-$2"
}

status_of() {
  # shellcheck disable=SC2029
  rx_exec "$1" "cat ~/gate/out/$2/progress.log; echo; cat ~/gate/out/$2/steps.txt 2>/dev/null; tail -n1 ~/gate/out/$2/mempeak.log 2>/dev/null; systemctl --user is-active gate-$2 || true"
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
  case $primary in
    boat:*) python3 "$RX_DIR/remote-executor.py" invalidate-mirror "$dest" || return 97 ;;
    *) mkdir -p "$dest" ;;
  esac
  if [ "$helper" = none ]; then
    # --delete keeps an earlier run's gate.exit from standing in for a rerun's result.
    rx_sync -- -a --delete "$primary:$pout/" "$dest/" || return 97
    wall "$dest" > "$dest/wall.txt"
    echo "results in $dest"
    return 0
  fi
  # A split gate whose merge was already copied back to the primary needs no new merge.
  # shellcheck disable=SC2029
  if rx_exec "$primary" "test -e ~/$pout/gate.exit"; then
    rx_sync -- -a --delete "$primary:$pout/" "$dest/" || return 97
    echo "results in $dest"
    return 0
  fi
  local tmp conf code=
  tmp=$(mktemp -d "$root/.artifacts/gates/.merge-$name.XXXXXX")
  conf=$tmp/conf
  mkdir -p "$conf"
  # Each transfer checks its own result, since a caller may run fetch where errexit is off.
  if ! rx_sync -- -a "$primary:$pout/" "$tmp/"; then
    rm -rf "$tmp"
    echo "could not copy the ci half from $primary" >&2
    return 97
  fi
  if ! rx_sync -- -a "$helper:$hout/" "$conf/"; then
    rx_sync -- -a "$tmp/" "$dest/"
    rm -rf "$tmp"
    echo "conformance half on $helper is unreachable; ci results in $dest" >&2
    return 98
  fi
  # The helper's step logs and exit files sit beside the primary's. Its progress, memory
  # and environment records keep a -conf suffix, and steps.txt lists both halves.
  for f in "$conf"/*.log "$conf"/*.exit; do
    [ -e "$f" ] || continue
    case ${f##*/} in progress.log | mempeak.log | selection.log | part.exit) ;; *) cp -a "$f" "$tmp/" ;; esac
  done
  for f in progress.log mempeak.log env.txt mempeak.final selection.txt selection.log; do
    [ -f "$conf/$f" ] && cp -a "$conf/$f" "$tmp/${f%.*}-conf.${f##*.}"
  done
  # After an earlier merge was partly copied back, steps.txt on the primary is that merge
  # and steps-ci.txt keeps the ci half's own steps.
  if [ -f "$tmp/steps-ci.txt" ]; then
    rm -f "$tmp/steps.txt"
  elif [ -f "$tmp/steps.txt" ]; then
    mv "$tmp/steps.txt" "$tmp/steps-ci.txt"
  fi
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
    local pe he
    # shellcheck disable=SC2029
    pe=$(rx_exec "$primary" "test -e ~/$pout/part.exit && echo yes || echo no") || pe=
    # shellcheck disable=SC2029
    he=$(rx_exec "$helper" "test -e ~/$hout/part.exit && echo yes || echo no") || he=
    if [ -z "$pe" ] || [ -z "$he" ]; then
      rm -rf "$tmp"
      echo "could not recheck the exit files; not merging yet" >&2
      return 97
    fi
    # Retry only when a file missing from the snapshot has appeared since.
    if { [ "$pe" = yes ] && [ ! -f "$tmp/part.exit" ]; } || { [ "$he" = yes ] && [ ! -f "$conf/part.exit" ]; }; then
      rm -rf "$tmp"
      fetch
      return
    fi
    code=99
  fi
  if [ -n "$code" ]; then
    echo "$code" > "$tmp/gate.exit"
    # Keep the primary's out directory complete for anyone reading it on the host.
    # gate.exit goes last, since a fetch that finds it there trusts the merge.
    if ! rx_sync -- -a --exclude gate.exit "$tmp/" "$primary:$pout/" ||
      ! rx_sync -- -a "$tmp/gate.exit" "$primary:$pout/gate.exit"; then
      rm -rf "$tmp"
      echo "could not copy the merge back to $primary" >&2
      return 97
    fi
  fi
  if ! rx_sync -- -a --delete "$tmp/" "$dest/"; then
    rm -rf "$tmp"
    return 97
  fi
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

# The GitHub workflows (the server artifact link, the wheel tests, the CI shards) run on
# GitHub and not in this gate, so a failure there on the last push is reported here before
# the next batch goes out. GATE_GITHUB_RUNS=0 skips the report.
if [ "${GATE_GITHUB_RUNS:-1}" != 0 ] && [ $link_only = 0 ]; then
  python3 "$here/github-runs.py" >&2 ||
    echo "gate.sh: a GitHub workflow failed on main; fix it in this batch or say why it can wait" >&2
fi

# The merge base the gate host compares the commit with, and whether the batch needs the
# conformance suites, from the same selection the host makes (scripts/gate/select-checks.py).
mb=
needs_conf=1
if [ $link_only = 1 ]; then
  needs_conf=0
  echo "checks: the server artifact link check only (--link-only)"
elif [ $all_checks = 0 ]; then
  if mb=$(git merge-base "$base_rev" "$full" 2> /dev/null); then
    sel=$(python3 "$here/select-checks.py" --base "$mb" --rev "$full" --paths-only 2> /dev/null) || sel=
    if [ -n "$sel" ] && ! echo "$sel" | grep -Eq '^(conformance|spill) [a-z]'; then needs_conf=0; fi
    echo "checks: selected from the changes since $(git rev-parse --short "$mb") ($base_rev); conformance $([ $needs_conf = 1 ] && echo needed || echo 'not needed')"
  else
    echo "gate.sh: $full has no merge base with $base_rev, so every check runs" >&2
    mb=
  fi
else
  echo "checks: every check with --force (--full)"
fi
if [ $needs_conf = 0 ]; then
  helper=none
  pick_helper=0
fi
# --auto starts from the first other gate host as the candidate helper.
if [ $pick_helper = 1 ] && [ $auto = 1 ]; then helper=$(default_helper "$primary"); fi

if [ $auto = 1 ]; then
  # idle[h]=1 when h is reachable with its lock free and no gate units; roomy[h]=1 when h has
  # the disk for a whole gate and half[h]=1 when it has the disk for a conformance half.
  declare -A idle=() roomy=() half=() units=()
  # GATE_HELPER=none or --helper none keeps every choice below a whole gate.
  split_ok=1
  [ "$helper" = none ] && split_ok=0
  report=
  for h in "${gate_hosts[@]}"; do
    p=$(probe "$h")
    report+="$h [$p] "
    read -r l u d <<< "$p" || true
    idle[$h]=0 roomy[$h]=0 half[$h]=0 units[$h]=999
    [ "$l" = unreachable ] && continue
    units[$h]=${u:-0}
    [ "$l" = free ] && [ "${u:-0}" = 0 ] && idle[$h]=1
    [ "${d:-0}" -ge "${GATE_MIN_FREE_GB:-40}" ] && roomy[$h]=1
    [ "${d:-0}" -ge "${GATE_MIN_FREE_HALF_GB:-20}" ] && half[$h]=1
  done
  chosen=
  if [ "$helper" != none ] && [ "${idle[$primary]:-0}" = 1 ] && [ "${roomy[$primary]:-0}" = 1 ] &&
    [ "${idle[$helper]:-0}" = 1 ] && [ "${half[$helper]:-0}" = 1 ]; then
    chosen="split"
  else
    for h in "$primary" "${gate_hosts[@]}"; do
      if [ "${idle[$h]:-0}" = 1 ] && [ "${roomy[$h]:-0}" = 1 ]; then
        primary=$h helper=none chosen=whole
        break
      fi
    done
  fi
  if [ -z "$chosen" ]; then
    # No host is idle. ci goes to the host with the shortest gate queue, and the split is
    # kept only when a helper's queue is no longer, since the merge waits for both halves.
    best=
    for h in "$primary" "${gate_hosts[@]}"; do
      [ "${roomy[$h]:-0}" = 1 ] || continue
      if [ -z "$best" ] || [ "${units[$h]}" -lt "${units[$best]}" ]; then best=$h; fi
    done
    [ -n "$best" ] && primary=$best
    helper=none
    for h in "${gate_hosts[@]}"; do
      [ $split_ok = 1 ] && [ "$h" != "$primary" ] && [ "${half[$h]:-0}" = 1 ] || continue
      if [ "${units[$h]}" -le "${units[$primary]:-999}" ] && { [ "$helper" = none ] || [ "${units[$h]}" -lt "${units[$helper]}" ]; }; then
        helper=$h
      fi
    done
  fi
  echo "auto: ${report}-> primary $primary helper $helper"
  refuse_bench
  dest=$root/.artifacts/gates/main-gate-$name-$primary
fi

# The default helper is the first other gate host that is free now.
if [ $pick_helper = 1 ] && [ $auto = 0 ]; then
  helper=none
  for h in "${gate_hosts[@]}"; do
    [ "$h" != "$primary" ] || continue
    read -r l u d <<< "$(probe "$h")" || true
    if [ "$l" = free ] && [ "${u:-1}" = 0 ] && [ "${d:-0}" -ge "${GATE_MIN_FREE_HALF_GB:-20}" ]; then
      helper=$h
      break
    fi
  done
  [ "$helper" != none ] || echo "no other gate host is free, so the conformance suites run on $primary after ci"
  refuse_bench
fi

# cargo_jobs_for HOST: the CARGO_BUILD_JOBS to pass to HOST, or nothing for its default.
cargo_jobs_for() {
  local kv
  if [ -n "$cargo_jobs" ]; then
    echo "$cargo_jobs"
    return
  fi
  for kv in ${GATE_CARGO_JOBS_HOSTS:-}; do
    if [ "${kv%%=*}" = "$1" ]; then
      echo "${kv#*=}"
      return
    fi
  done
  echo "${GATE_CARGO_JOBS:-}"
}

if [ $dry = 1 ]; then
  echo "dry run: gate-$name primary $primary helper $helper jobs $jobs cargo-jobs ${cargo_jobs:-per host} base ${mb:-none} full $all_checks results $dest"
  exit 0
fi

# Validate Boat tools and lifetime before any cache collection or other mutation.
for admit_host in "$primary" "$helper"; do
  case $admit_host in boat:*)
    mkdir -p "$statedir"
    rx_admit "$admit_host" --minimum "${SPARKLES_REMOTE_MIN_LIFETIME:-7200}" > "$statedir/preflight-$name-${admit_host#boat:}.json" || die "Boat gate admission failed"
    ;;
  esac
done

# Free GiB under the host's home directory, or nothing when ssh fails.
free_gib() {
  # shellcheck disable=SC2016
  rx_exec "$1" 'df -BG --output=avail "$HOME" | tail -n1 | tr -dc 0-9' 2> /dev/null
}

# ensure_disk HOST NEED_GIB WHAT: refuse to start WHAT on HOST unless the host has NEED_GIB
# free. A host that is short first runs `mbx gc`, which collects idle managed targets and
# evicts cache objects down to their budgets and to gc.min_free_size. A build holds its
# target lease for the whole run, so space freed after the start comes too late.
ensure_disk() {
  local h=$1 need=$2 what=$3 d
  d=$(free_gib "$h")
  [ -n "$d" ] || return 0
  [ "$d" -ge "$need" ] && return 0
  echo "gate.sh: $h has $d GiB free and the $what needs $need GiB, so mbx gc runs there first" >&2
  rx_exec "$h" 'mbx gc' >&2 || echo "gate.sh: mbx gc on $h failed" >&2
  d=$(free_gib "$h")
  if [ -z "$d" ] || [ "$d" -lt "$need" ]; then
    die "$h has ${d:-an unknown number of} GiB free after mbx gc and the $what needs $need GiB. Free space there, choose another host with --host or --helper, or lower GATE_NEED_CI_GB or GATE_NEED_CONF_GB."
  fi
  echo "gate.sh: $h has $d GiB free after mbx gc" >&2
}

need_ci=${GATE_NEED_CI_GB:-30}
need_conf=${GATE_NEED_CONF_GB:-15}
case $need_ci in '' | *[!0-9]*) die "bad GATE_NEED_CI_GB $need_ci" ;; esac
case $need_conf in '' | *[!0-9]*) die "bad GATE_NEED_CONF_GB $need_conf" ;; esac
# A whole gate runs the suites in the ci half's worktree after it, so it needs what the ci
# half needs. An unreachable helper is left to the fallback below.
ensure_disk "$primary" "$need_ci" "ci half"
if [ "$helper" != none ] && rx_exec "$helper" true 2> /dev/null; then
  ensure_disk "$helper" "$need_conf" "conformance half"
fi
note=
if [ "$helper" != none ] && ! rx_exec "$helper" true 2> /dev/null; then
  case "$primary $helper" in *boat:*) die "helper unreachable; explicit Boat placement never crosses providers" ;; esac
  note="helper $helper is unreachable; running the conformance suites on $primary after ci"
  echo "$note" >&2
  helper=none
fi
if [ "$helper" != none ]; then
  if ! start_on "$helper" "$hname" conformance >&2; then
    if grep -qx 'phase=start-requested' "$statedir/start-intent-$hname"; then
      die "helper acceptance unknown; reconcile start-intent-$hname before retrying or falling back"
    fi
    case "$primary $helper" in *boat:*) die "helper setup failed; explicit Boat placement never crosses providers" ;; esac
    note="starting the conformance half on $helper failed; running it on $primary after ci"
    echo "$note" >&2
    helper=none
  fi
fi
if [ "$helper" = none ]; then
  [ -n "$note" ] || note="whole gate on $primary"
  part=all
  if [ $link_only = 1 ]; then
    part="link"
    note="server artifact link check on $primary"
  fi
  start_on "$primary" "$name" "$part" "$note"
else
  start_on "$primary" "$name" ci "split: conformance half runs on $helper as gate-$hname"
fi
mkdir -p "$statedir"
printf 'primary=%s\nhelper=%s\nhname=%s\njobs=%s\nbase=%s\nfull_checks=%s\n' "$primary" "$helper" "$hname" "$jobs" "$mb" "$all_checks" > "$statefile"
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
  sleep 30
done
fetch
cat "$dest/selection.txt" 2> /dev/null || true
cat "$dest/steps.txt" 2> /dev/null || true
cat "$dest/wall.txt" 2> /dev/null || true
code=$(cat "$dest/gate.exit" 2> /dev/null || echo 99)
echo "gate exit $code"
exit "$code"
