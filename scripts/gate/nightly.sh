#!/usr/bin/env bash
# Run the nightly full gate on origin/main: every check of the batch gate with --force and
# no selection from the changed files, on the gate hosts that scripts/gate/gate.sh --auto
# finds idle. Batch gates choose their checks from the batch's diff, and this run covers
# what that selection leaves out.
#
# Usage (on the laptop, from any checkout of the sparkles repo):
#   nightly.sh             fetch origin, gate origin/main unless it already passed, wait
#   nightly.sh --dry-run   print the commit, the last passing commit and the chosen hosts
#   nightly.sh --install   install and start a systemd user timer that runs this nightly
#   nightly.sh --uninstall stop and remove that timer
#
# Results go to <main checkout>/.artifacts/gates/nightly/: one log per run, last-pass with
# the last commit that passed and, after a failure, FAILED with the commit range between
# the last pass and the failing commit and the steps that failed, or NOT-STARTED when the
# gate could not start. A later pass removes both. github-runs.txt holds the newest finished
# run of each GitHub workflow on main (scripts/gate/github-runs.py), and GITHUB-FAILED is a
# copy of it while one of them failed. The gate's own results are in
# .artifacts/gates/main-gate-<short>-nightly-<host>/.
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
if [ "${1:-}" = --dry-run ]; then
  echo "nightly: origin/main is $short, last pass ${last:-none}"
  exec "$here/gate.sh" --auto --full --tag nightly --dry-run "$rev"
fi
if [ "$rev" = "$last" ]; then
  echo "nightly: $short already passed"
  exit 0
fi

log=$state/$(date -u +%Y%m%dT%H%M%SZ)-$short.log
code=0
GATE_GITHUB_RUNS=0 "$here/gate.sh" --auto --full --tag nightly "$rev" > "$log" 2>&1 || code=$?
if [ "$code" = 0 ]; then
  echo "$rev" > "$state/last-pass"
  rm -f "$state/FAILED" "$state/NOT-STARTED"
  echo "nightly: $short passed, log $log"
  exit 0
fi
# A run that never started, such as one whose ssh agent was locked, is not a failure of
# the commit.
if ! grep -q "^gate-.* started" "$log"; then
  echo "nightly gate on $rev could not start (exit $code) at $(date -u +%FT%TZ); log: $log" | tee "$state/NOT-STARTED"
  exit "$code"
fi
rm -f "$state/NOT-STARTED"
{
  echo "nightly gate failed on $rev (exit $code) at $(date -u +%FT%TZ)"
  echo "range: ${last:-<no earlier pass>}..$rev"
  echo "failed steps:"
  grep -E 'exit=[1-9]' "$log" || echo "  (none recorded; see the log)"
  echo "log: $log"
  [ -z "$last" ] || {
    echo "merges in the range:"
    git log --first-parent --format='  %h %s' "$last..$rev"
  }
} > "$state/FAILED"
cat "$state/FAILED"
exit "$code"
