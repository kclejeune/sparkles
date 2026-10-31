#!/usr/bin/env bash
# Starts one gate run on a gate host for a commit already pushed to ~/gate/repo.git.
# scripts/gate/gate.sh installs this file and gate-inner.sh into ~/gate/bin/v-<hash>/ on
# the host and calls it over ssh. It creates the detached worktree ~/gate/wt-<name> and
# the systemd user unit gate-<name>, which survives the ssh session because linger is on,
# prints the unit name and returns at once.
#
# Usage: gate-start.sh <full-sha> [--name NAME] [--part all|ci|conformance] [--jobs N]
#                      [--note TEXT]
#   --name   unit, worktree and out-directory name (default: the first 8 hex digits)
#   --part   all runs every step, ci only scripts/ci.sh, conformance only the suites
#   --jobs   passed to scripts/ci.sh --jobs (default 1)
#   --note   a line to record in progress.log before the run is queued
#
# The memory cap is the smaller of 24G and the host's RAM less 2G, with 2G of swap.
# GATE_MEMORY_MAX overrides it.
set -euo pipefail
full=$1
shift
name=${full:0:8}
part=all
jobs=1
note=
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
case $part in all | ci | conformance) ;; *)
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
git -C "$G/repo.git" worktree prune
# A failed run keeps its worktree for inspection. Recreate it, so that nothing left there
# can change a rerun.
if [ -d "$wt" ]; then git -C "$G/repo.git" worktree remove --force "$wt" 2> /dev/null || rm -rf "$wt"; fi
git -C "$G/repo.git" worktree prune
git -C "$G/repo.git" worktree add -q --detach "$wt" "$full"
date +%s > "$out/queued_at"
if [ -n "$note" ]; then echo "$(date -u +%FT%TZ) $note" >> "$out/progress.log"; fi
echo "$(date -u +%FT%TZ) queued $full part=$part jobs=$jobs host=$(hostname)" >> "$out/progress.log"

if [ -n "${GATE_MEMORY_MAX:-}" ]; then
  mem=$GATE_MEMORY_MAX
else
  total_g=$(awk '$1 == "MemTotal:" { print int($2 / 1048576) }' /proc/meminfo)
  mem_g=$((total_g - 2))
  [ "$mem_g" -gt 24 ] && mem_g=24
  mem=${mem_g}G
fi

systemd-run --user --unit "$unit" --collect -q \
  -p MemoryMax="$mem" -p MemorySwapMax=2G \
  --setenv=HOME="$HOME" --setenv=USER="$USER" \
  --setenv=GATE_CARGO_JOBS="${GATE_CARGO_JOBS:-4}" \
  /run/current-system/sw/bin/timeout 9h "$bin/gate-inner.sh" "$full" "$name" "$part" "$jobs"
echo "$unit"
