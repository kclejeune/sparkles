#!/usr/bin/env bash
# Run the nightly full gate on origin/main: every check of the batch gate with --force and
# no selection from the changed files, on the gate hosts that scripts/gate/gate.sh --auto
# finds idle. Batch gates choose their checks from the batch's diff, and this run covers
# what that selection leaves out. Beside the full gate it runs the server artifact link
# check (scripts/server-artifact-link-check.sh) with gate.sh --link-only on a gate host
# that has podman, atlas first, since the GitHub workflow that builds the release archive
# in the pinned manylinux_2_28 image does not run in any gate. GATE_LINK_HOST names the
# host. The link check runs first and the full gate after it.
#
# Usage (on the laptop, from any checkout of the sparkles repo):
#   nightly.sh             fetch origin, gate origin/main unless it already passed, wait
#   nightly.sh --dry-run   print the commit, the last passing commit and the chosen hosts
#   nightly.sh --install   install and start a systemd user timer that runs this nightly
#   nightly.sh --uninstall stop and remove that timer
#
# Results go to <main checkout>/.artifacts/gates/nightly/: one log per run, last-pass with
# the last commit that passed and, after a failure, FAILED with the commit range between
# the last pass and the failing commit and the steps that failed, or NOT-STARTED when
# neither gate could start. A passing full gate does not hide a link check that failed or
# could not run. A later pass removes both. github-runs.txt holds the newest finished
# run of each GitHub workflow on main (scripts/gate/github-runs.py), and GITHUB-FAILED is a
# copy of it while one of them failed. The gate's own results are in
# .artifacts/gates/main-gate-<short>-nightly-<host>/ and the link check's in
# .artifacts/gates/main-gate-<short>-nightly-link-<host>/.
# docs/DEVELOPMENT.md (Nightly gate) describes the bisect and fix-forward that a failure
# starts.
#
# GATE_NIGHTLY_AT (default "*-*-* 02:30:00", a systemd OnCalendar value) sets the time of
# the timer, in the environment or in ~/.config/sparkles/gate.env. The timer's service
# runs with the user's systemd environment, so the ssh agent that the ssh configuration
# names must be reachable from there, as it is with an IdentityAgent socket at a fixed path.
set -euo pipefail

self=$(readlink -f "${BASH_SOURCE[0]}")
here=$(dirname "$self")
# shellcheck source=scripts/gate/config.sh
. "$here/config.sh"
root=$(cd "$(git -C "$here" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)
state=$root/.artifacts/gates/nightly
unit=sparkles-gate-nightly
units_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user

case ${1:-} in
  --install)
    mkdir -p "$units_dir"
    cat > "$units_dir/$unit.service" << EOF
[Unit]
Description=Sparkles nightly full gate on origin/main

[Service]
Type=oneshot
WorkingDirectory=$root
ExecStart=$root/scripts/gate/nightly.sh
EOF
    cat > "$units_dir/$unit.timer" << EOF
[Unit]
Description=Sparkles nightly full gate on origin/main

[Timer]
OnCalendar=${GATE_NIGHTLY_AT:-*-*-* 02:30:00}
Persistent=true

[Install]
WantedBy=timers.target
EOF
    systemctl --user daemon-reload
    systemctl --user enable --now "$unit.timer"
    systemctl --user list-timers "$unit.timer" --no-pager
    exit 0
    ;;
  --uninstall)
    systemctl --user disable --now "$unit.timer" 2> /dev/null || true
    rm -f "$units_dir/$unit.service" "$units_dir/$unit.timer"
    systemctl --user daemon-reload
    exit 0
    ;;
  --dry-run | '') ;;
  -h | --help)
    sed -n '2,/^set -euo/p' "$self" | sed '$d; s/^# \{0,1\}//'
    exit 0
    ;;
  *)
    echo "nightly.sh: unknown argument $1" >&2
    exit 2
    ;;
esac

cd "$root"
git fetch -q origin main
rev=$(git rev-parse origin/main)
short=${rev:0:8}
mkdir -p "$state"
last=$(cat "$state/last-pass" 2> /dev/null || true)
# The GitHub workflows of main, which the gate does not run, refreshed every night even
# when the commit already passed. A failure is recorded beside the gate's own result and
# does not change it.
if python3 "$here/github-runs.py" > "$state/github-runs.txt" 2>&1; then
  rm -f "$state/GITHUB-FAILED"
else
  cp "$state/github-runs.txt" "$state/GITHUB-FAILED"
  cat "$state/GITHUB-FAILED"
fi
# The gate host for the server artifact link check, which runs it in a podman container.
link_host=${GATE_LINK_HOST:-}
if [ -z "$link_host" ]; then
  for h in atlas "${gate_hosts[@]}"; do
    is_bench "$h" && continue
    if ssh -o BatchMode=yes -o ConnectTimeout=10 "$h" 'command -v podman' > /dev/null 2>&1; then
      link_host=$h
      break
    fi
  done
fi
if [ "${1:-}" = --dry-run ]; then
  echo "nightly: origin/main is $short, last pass ${last:-none}, server link check on ${link_host:-<no gate host with podman>}"
  exec "$here/gate.sh" --auto --full --tag nightly --dry-run "$rev"
fi
if [ "$rev" = "$last" ]; then
  echo "nightly: $short already passed"
  exit 0
fi

log=$state/$(date -u +%Y%m%dT%H%M%SZ)-$short.log
link_log=${log%.log}-link.log
code=0
link_code=0
# The link check runs first and the full gate after it, so that two gates never start
# on one host's gate repository at once and the full gate finds atlas idle.
if [ -n "$link_host" ]; then
  GATE_GITHUB_RUNS=0 "$here/gate.sh" --host "$link_host" --link-only --tag nightly-link "$rev" > "$link_log" 2>&1 || link_code=$?
else
  echo "no gate host has podman, so the server artifact link check cannot run (set GATE_LINK_HOST)" > "$link_log"
  link_code=1
fi
GATE_GITHUB_RUNS=0 "$here/gate.sh" --auto --full --tag nightly "$rev" > "$log" 2>&1 || code=$?
if [ "$code" = 0 ] && [ "$link_code" = 0 ]; then
  echo "$rev" > "$state/last-pass"
  rm -f "$state/FAILED" "$state/NOT-STARTED"
  echo "nightly: $short passed, log $log, link check log $link_log"
  exit 0
fi
# A run that never started, such as one whose ssh agent was locked, is not a failure of
# the commit. A link check that could not run while the full gate did is a failure, since
# it would otherwise pass unchecked.
gate_started=1
link_started=1
grep -q "^gate-.* started" "$log" || gate_started=0
grep -q "^gate-.* started" "$link_log" || link_started=0
if [ $gate_started = 0 ] && { [ $link_started = 0 ] || [ "$link_code" = 0 ]; }; then
  echo "nightly gate on $rev could not start (exit $code, link check $link_code) at $(date -u +%FT%TZ); logs: $log $link_log" | tee "$state/NOT-STARTED"
  exit "$((code != 0 ? code : link_code))"
fi
rm -f "$state/NOT-STARTED"
{
  echo "nightly gate failed on $rev (exit $code, link check $link_code) at $(date -u +%FT%TZ)"
  echo "range: ${last:-<no earlier pass>}..$rev"
  echo "failed steps:"
  if [ "$code" != 0 ]; then
    grep -E 'exit=[1-9]' "$log" || echo "  (none recorded; see the log)"
  fi
  if [ "$link_code" != 0 ]; then
    grep -E 'exit=[1-9]' "$link_log" || echo "  server-link: did not run or did not report; see $link_log: $(tail -n 1 "$link_log")"
  fi
  echo "log: $log"
  echo "link check log: $link_log"
  [ -z "$last" ] || {
    echo "merges in the range:"
    git log --first-parent --format='  %h %s' "$last..$rev"
  }
} > "$state/FAILED"
cat "$state/FAILED"
exit "$((code != 0 ? code : link_code))"
