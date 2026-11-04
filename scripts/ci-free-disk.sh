#!/usr/bin/env bash
# Free disk on a GitHub-hosted Ubuntu runner for the CI test build and its shards, and
# record the least free space the job reaches.
#
# Usage: scripts/ci-free-disk.sh start | wait | report
#
# `start` removes, in the background, preinstalled toolchains that the Rust jobs never
# use: the Android SDK, .NET, Haskell, Swift, PowerShell, Julia, the browsers, CodeQL
# and the cached Docker images, which together hold about 30 GB of the image. It also
# starts a sampler that writes the free space of the workspace's file system every five
# seconds. `wait` blocks until the removal ends, for at most ten minutes, and `report`
# prints the free space now and the least the sampler saw. On any other runner, such as
# a persistent larger runner, `start` removes nothing and only starts the sampler.
set -euo pipefail

dir="${RUNNER_TEMP:?RUNNER_TEMP is not set}/free-disk"
mkdir -p "$dir"

case "${1:-}" in
  start)
    df -h "${GITHUB_WORKSPACE:-.}"
    (
      while :; do
        df --output=avail -B1M "${GITHUB_WORKSPACE:-.}" | tail -n1 >> "$dir/samples"
        sleep 5
      done
    ) > /dev/null 2>&1 &
    if [ "${RUNNER_ENVIRONMENT:-}" != github-hosted ] || [ "$(uname -s)" != Linux ]; then
      echo "not a GitHub-hosted Linux runner, so nothing is removed"
      touch "$dir/done"
      exit 0
    fi
    paths=(
      /usr/local/lib/android /usr/share/dotnet /opt/ghc /usr/local/.ghcup
      /usr/share/swift /usr/local/share/powershell /usr/local/share/chromium
      /opt/microsoft /opt/google /usr/local/julia* "${AGENT_TOOLSDIRECTORY:-/opt/hostedtoolcache}/CodeQL"
    )
    # shellcheck disable=SC2016 # the inner shell expands its arguments
    setsid nohup bash -c '
      dir=$1
      shift
      started=$(date +%s)
      printf "%s\0" "$@" | sudo xargs -0 -P 8 -n 1 rm -rf
      docker image prune --all --force > /dev/null 2>&1 || true
      echo "removed in $(($(date +%s) - started)) s" > "$dir/log"
      touch "$dir/done"
    ' free-disk "$dir" "${paths[@]}" > "$dir/nohup.log" 2>&1 < /dev/null &
    ;;
  wait)
    for _ in $(seq 120); do
      if [ -e "$dir/done" ]; then
        cat "$dir/log" 2> /dev/null || true
        df -h "${GITHUB_WORKSPACE:-.}"
        exit 0
      fi
      sleep 5
    done
    echo "::warning::the preinstalled toolchains were still being removed after ten minutes"
    ;;
  report)
    df -h "${GITHUB_WORKSPACE:-.}"
    if [ -s "$dir/samples" ]; then
      echo "least free space during the job: $(sort -n "$dir/samples" | head -n1) MiB"
    fi
    ;;
  *)
    echo "usage: $0 start | wait | report" >&2
    exit 2
    ;;
esac
