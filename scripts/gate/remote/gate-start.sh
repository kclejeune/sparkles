#!/usr/bin/env bash
# Starts one gate run on a gate host for a commit already pushed to ~/gate/repo.git.
# scripts/gate/gate.sh installs this file and gate-inner.sh into ~/gate/bin/v-<hash>/ on
# the host and calls it over ssh. It creates the detached worktree ~/gate/wt-<name> and
# the systemd user unit gate-<name>, which survives the ssh session because linger is on,
# prints the unit name and returns at once.
#
# Usage: gate-start.sh <full-sha> [--name NAME] [--part all|ci|conformance|link] [--jobs N]
#                      [--cargo-jobs N] [--base SHA] [--full] [--note TEXT]
#   --name        unit, worktree and out-directory name (default: the first 8 hex digits)
#   --part        all runs every step, ci only scripts/ci.sh, conformance only the suites,
#                 link only the server artifact link check (scripts/server-artifact-link-check.sh)
#   --jobs        passed to scripts/ci.sh --jobs (default 1)
#   --cargo-jobs  CARGO_BUILD_JOBS for the run (default: from the memory cap, see below)
#   --base        the commit the batch is compared with, so that the run selects its checks
#                 from the changed files (scripts/gate/select-checks.py)
#   --full        every check with --force, with no selection, as the nightly gate runs
#   --note        a line to record in progress.log before the run is queued
#
# The memory cap is the smaller of 24G and the host's RAM less 2G, with 2G of swap.
# GATE_MEMORY_MAX overrides it. The default number of compile jobs is the cap in GiB less
# one, halved, from 2 to 8, which gives 6 at a 13G cap and 8 at a 24G cap. A Rust test build
# at 4 jobs peaked at about 9 GiB of process memory.
set -euo pipefail
full=$1
shift
name=${full:0:8}
part=all
jobs=1
cargo_jobs=
base=
all_checks=0
note=
runtime=32400
executor=ssh
while [ $# -gt 0 ]; do
  case $1 in
    --name)
      name=$2
      shift 2
      ;;
    --part)
      part=$2
      shift 2
      ;;
    --jobs)
      jobs=$2
      shift 2
      ;;
    --cargo-jobs)
      cargo_jobs=$2
      shift 2
      ;;
    --base)
      base=$2
      shift 2
      ;;
    --full)
      all_checks=1
      shift
      ;;
    --runtime)
      runtime=$2
      shift 2
      ;;
    --executor)
      executor=$2
      shift 2
      ;;
    --note)
      note=$2
      shift 2
      ;;
    *)
      echo "gate-start.sh: unknown argument $1" >&2
      exit 2
      ;;
  esac
done
case $part in all | ci | conformance | link) ;; *)
  echo "gate-start.sh: bad --part $part" >&2
  exit 2
  ;;
esac
case $jobs in '' | *[!0-9]*)
  echo "gate-start.sh: bad --jobs $jobs" >&2
  exit 2
  ;;
esac
case $name in '' | *[!A-Za-z0-9._-]*)
  echo "gate-start.sh: bad --name $name" >&2
  exit 2
  ;;
esac
case $cargo_jobs in *[!0-9]* | 0)
  echo "gate-start.sh: bad --cargo-jobs $cargo_jobs" >&2
  exit 2
  ;;
esac
case $base in *[!0-9a-f]*)
  echo "gate-start.sh: bad --base $base" >&2
  exit 2
  ;;
esac

case $runtime in '' | *[!0-9]* | 0)
  echo 'invalid gate runtime' >&2
  exit 2
  ;;
esac
case $executor in ssh | boat) ;; *)
  echo 'invalid gate executor' >&2
  exit 2
  ;;
esac

bin=$(cd "$(dirname "$0")" && pwd)
G=$HOME/gate
wt=$G/wt-$name
out=$G/out/$name
unit=gate-$name

if systemctl --user is-active -q "$unit"; then
  echo "unit $unit is already running; see $out/progress.log" >&2
  exit 1
fi
systemctl --user reset-failed "$unit" 2> /dev/null || true
if [ -d "$out" ]; then mv "$out" "$out.old-$(date +%s)"; fi
mkdir -p "$out"
git -C "$G/repo.git" cat-file -e "$full^{commit}"
[ -z "$base" ] || git -C "$G/repo.git" cat-file -e "$base^{commit}"
git -C "$G/repo.git" worktree prune
# A failed run keeps its worktree for inspection. Recreate it, so that nothing left there
# can change a rerun.
if [ -d "$wt" ]; then git -C "$G/repo.git" worktree remove --force "$wt" 2> /dev/null || rm -rf "$wt"; fi
git -C "$G/repo.git" worktree prune
git -C "$G/repo.git" worktree add -q --detach "$wt" "$full"
date +%s > "$out/queued_at"
if [ -n "$note" ]; then echo "$(date -u +%FT%TZ) $note" >> "$out/progress.log"; fi

total_g=$(awk '$1 == "MemTotal:" { print int($2 / 1048576) }' /proc/meminfo)
mem_g=$((total_g - 2))
[ "$mem_g" -gt 24 ] && mem_g=24
mem=${mem_g}G
if [ -n "${GATE_MEMORY_MAX:-}" ]; then
  mem=$GATE_MEMORY_MAX
  case $mem in *G) mem_g=${mem%G} ;; esac
fi
if [ -z "$cargo_jobs" ]; then
  cargo_jobs=$(((mem_g - 1) / 2))
  [ "$cargo_jobs" -lt 2 ] && cargo_jobs=2
  [ "$cargo_jobs" -gt 8 ] && cargo_jobs=8
fi
mode=selected
[ -n "$base" ] || mode="every check, no base"
[ "$all_checks" = 0 ] || mode="every check with --force"
echo "$(date -u +%FT%TZ) queued $full part=$part jobs=$jobs cargo_jobs=$cargo_jobs memory=$mem base=${base:-none} checks=$mode host=$(hostname)" >> "$out/progress.log"

# A service unit starts with an empty environment. A Boat job's environment builds with
# mbx in local-only mode, so its mbx and Rust settings are forwarded when the caller set
# them, even to an empty value. An ssh host sets none of them and keeps its own mode.
forward=()
for n in MBX_REMOTE_URL MBX_CACHE_DIR MBX_GC_MAX_TOTAL_SIZE MBX_GC_MIN_FREE_SIZE RUSTUP_TOOLCHAIN RUSTUP_AUTO_INSTALL; do
  [ -z "${!n+x}" ] || forward+=("--setenv=$n=${!n}")
done
systemd-run --user --unit "$unit" --collect -q "${forward[@]}" \
  -p MemoryMax="$mem" -p MemorySwapMax=2G -p OOMPolicy=continue -p RuntimeMaxSec="$runtime" \
  --setenv=HOME="$HOME" --setenv=USER="$USER" --setenv=PATH="$PATH" \
  --setenv=MBX_REMOTE_TOKEN_FILE="${MBX_REMOTE_TOKEN_FILE:-}" --setenv=SPARKLES_REMOTE_EXECUTOR="$executor" \
  --setenv=GATE_CARGO_JOBS="$cargo_jobs" --setenv=GATE_BASE="$base" --setenv=GATE_FULL="$all_checks" \
  timeout --signal=TERM --kill-after=30s "$runtime" "$bin/gate-inner.sh" "$full" "$name" "$part" "$jobs"
echo "$unit"
