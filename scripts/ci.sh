#!/usr/bin/env bash
# Run the CI tasks of mise.toml with one log per task, a line for each task as it
# finishes, and a summary at the end (`mise run ci`, `mise run ci:fast`).
#
# The shared prerequisites (the UI build, the Node addon and the JVM native library) are
# built first. The checks then run with `mise run --skip-deps`, cheap ones first, so a
# formatting or lint failure shows up within minutes rather than after the test suites.
# Every task runs to the end unless --fail-fast is given, and the summary lists every
# failure with the tail of its log.
#
# While it runs, .artifacts/ci/summary.txt holds each task's state and time, so another
# terminal or an agent can follow it with `cat .artifacts/ci/summary.txt`. Logs are in
# .artifacts/ci/logs/<task>.log.
#
# Checks whose inputs are unchanged since they last passed are skipped through mise's task
# cache (see the task caching section of mise.toml).
#
# Usage: scripts/ci.sh [--fast] [--exhaustive-features] [--fail-fast] [--force] [--jobs N] [task...]
#   --fast       formatting, shell checks, doc paths, workspace Clippy and Rust tests
#   --exhaustive-features
#                also run optional-feature Clippy and the six stripped-server test sets
#   --fail-fast  stop the remaining tasks after the first failure
#   --force      run every check even when the task cache has a pass for it
#   --jobs N     tasks run at once (default 2). Cargo tasks share one build lock, so more
#                jobs mostly help the UI, JVM and Node tasks. `test` and `node:test` never
#                run at the same time, because each links target/debug/sparkles with other
#                features (see links_server below). `test:features` takes turns with them
#                for the same reason.
#   task...      run only these CI tasks
set -euo pipefail

# The whole script is one function, so bash has parsed all of it before it starts and an
# edit to this file cannot change a run in progress.
main() {
  cd "$(dirname "$0")/.."

  full=(
    fmt:check ui:fmt:check node:fmt:check lint:shell lint:doc-paths test:bench-scripts
    lint ui:check fmt:wasm py:lint jvm:lint node:lint
    test ui:test
    py:test jvm:rust-test jvm:test node:test node:client:check licenses:check
  )
  fast=(fmt:check lint:shell lint:doc-paths lint test)

  jobs=2
  fail_fast=
  force=()
  exhaustive_features=
  tasks=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --fast) tasks=("${fast[@]}") ;;
      --exhaustive-features) exhaustive_features=1 ;;
      --fail-fast) fail_fast=1 ;;
      --force) force=(--force) ;;
      --jobs)
        jobs="$2"
        shift
        ;;
      -h | --help)
        sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
        exit 0
        ;;
      *) tasks+=("$1") ;;
    esac
    shift
  done
  [ ${#tasks[@]} -gt 0 ] || tasks=("${full[@]}")
  if [ -n "$exhaustive_features" ]; then
    for feature_task in lint:features test:features; do
      if [[ " ${tasks[*]} " != *" $feature_task "* ]]; then
        tasks+=("$feature_task")
      fi
    done
  fi

  out=.artifacts/ci
  # One run per checkout: a second run would delete the first one's logs and state.
  mkdir -p .artifacts
  exec 9> .artifacts/ci.lock
  if ! flock -n 9; then
    echo "another CI run is active in this checkout; see $out/summary.txt" >&2
    exit 1
  fi
  rm -rf "$out"
  mkdir -p "$out/logs" "$out/state"
  summary="$out/summary.txt"
  start=$SECONDS
  started=$(date '+%H:%M:%S')
  # Each background task gets its own process group, so stopping it stops its cargo,
  # Gradle or pnpm children too.
  set -m

  # The prerequisites each selected task needs, built once before the checks start, so
  # that parallel tasks do not rebuild ui/build under each other.
  prep=()
  for t in "${tasks[@]}"; do
    case "$t" in
      lint | lint:features | test | test:features | ui:check | ui:test | licenses:check) prep+=(ui:build) ;;
      ui:fmt:check) prep+=(ui:install) ;;
      node:fmt:check | node:client:check) prep+=(node:install) ;;
      node:test) prep+=(node:build) ;;
      jvm:test) prep+=(jvm:native) ;;
    esac
  done
  mapfile -t prep < <(printf '%s\n' "${prep[@]}" | sort -u | sed '/^$/d')

  write_summary() {
    # Local, so that the main loop's task variable survives a summary update.
    local t f
    {
      printf 'CI started %s, %ds elapsed\n\n' "$started" "$((SECONDS - start))"
      for t in "${all[@]}"; do
        f="$out/state/${t//:/_}"
        if [ -f "$f" ]; then
          printf '%-20s %s\n' "$t" "$(cat "$f")"
        else
          printf '%-20s %s\n' "$t" waiting
        fi
      done
    } > "$summary.tmp"
    mv "$summary.tmp" "$summary"
  }

  # run_task <task> [mise flags...]: run one task with its output in its own log and
  # record its state and duration.
  run_task() {
    local t="$1" key log t0 rc
    shift
    key="${t//:/_}"
    log="$out/logs/$key.log"
    t0=$SECONDS
    echo "running since $(date '+%H:%M:%S')" > "$out/state/$key"
    set +e
    mise run --output interleave "$@" "$t" > "$log" 2>&1
    rc=$?
    set -e
    local secs=$((SECONDS - t0))
    if [ $rc -eq 0 ]; then
      echo "pass ${secs}s" > "$out/state/$key"
      printf 'PASS %-20s %5ds\n' "$t" "$secs"
    else
      echo "FAIL ${secs}s (exit $rc) $log" > "$out/state/$key"
      printf 'FAIL %-20s %5ds  log: %s\n' "$t" "$secs" "$log"
    fi
    return $rc
  }

  all=()
  [ ${#prep[@]} -gt 0 ] && all+=(prepare)
  all+=("${tasks[@]}")
  write_summary

  failed=()
  if [ ${#prep[@]} -gt 0 ]; then
    echo "prepare: ${prep[*]}"
    key=prepare
    t0=$SECONDS
    echo running > "$out/state/$key"
    write_summary
    # `mise run a b` passes b to a as an argument; tasks are separated by `:::`.
    local_args=()
    for t in "${prep[@]}"; do
      [ ${#local_args[@]} -eq 0 ] || local_args+=(:::)
      local_args+=("$t")
    done
    if mise run --output interleave "${local_args[@]}" > "$out/logs/$key.log" 2>&1; then
      echo "pass $((SECONDS - t0))s" > "$out/state/$key"
      printf 'PASS %-20s %5ds\n' prepare "$((SECONDS - t0))"
    else
      echo "FAIL $((SECONDS - t0))s $out/logs/$key.log" > "$out/state/$key"
      printf 'FAIL %-20s %5ds  log: %s\n' prepare "$((SECONDS - t0))" "$out/logs/$key.log"
      failed+=(prepare)
      write_summary
      tail -n 60 "$out/logs/$key.log"
      exit 1
    fi
    write_summary
  fi

  declare -A pids=()
  stop=

  # links_server <task>: whether the task links target/debug/sparkles. `test` builds it
  # with the features of the workspace's dev-dependencies (sparkles-fmt's fault-injection
  # among them), which tests such as the formatter's refused-output checks rely on, and
  # `node:test` builds the server alone without them. Cargo replaces the one file with
  # whichever link finishes last, so two such tasks at once would run one's tests on the
  # other's binary. Building each into its own target directory would cost a second
  # build of the server, so they take turns instead. `test:features` links it with one
  # optional feature set after another, so it takes turns too.
  links_server() {
    case "$1" in
      test | test:features | node:test) return 0 ;;
      *) return 1 ;;
    esac
  }
  # can_start <task>: no running task links the server binary when this one does.
  can_start() {
    local pid
    links_server "$1" || return 0
    for pid in "${!pids[@]}"; do
      links_server "${pids[$pid]}" && return 1
    done
    return 0
  }
  # reap: wait for one running task and record its result.
  reap() {
    set +e
    wait -n -p done_pid
    rc=$?
    set -e
    write_summary
    if [ $rc -ne 0 ]; then
      failed+=("${pids[$done_pid]}")
      [ -z "$fail_fast" ] || stop=1
    fi
    unset "pids[$done_pid]"
  }

  # stop_all: end every running task's process group and mark it stopped.
  stop_all() {
    local pid
    for pid in "${!pids[@]}"; do
      kill -TERM -- "-$pid" 2> /dev/null || true
      echo stopped > "$out/state/${pids[$pid]//:/_}"
    done
  }
  trap 'stop_all; write_summary; exit 130' INT TERM
  # Tasks start in their listed order, except that one whose server link would clash
  # with a running task's waits, and the next task that can start takes its turn.
  queue=("${tasks[@]}")
  while [ ${#queue[@]} -gt 0 ] && [ -z "$stop" ]; do
    while [ ${#pids[@]} -ge "$jobs" ]; do
      reap
    done
    [ -z "$stop" ] || break
    next=
    for i in "${!queue[@]}"; do
      if can_start "${queue[$i]}"; then
        next=$i
        break
      fi
    done
    if [ -z "$next" ]; then
      reap
      continue
    fi
    t=${queue[$next]}
    unset "queue[$next]"
    queue=("${queue[@]}")
    run_task "$t" --skip-deps "${force[@]}" &
    pids[$!]="$t"
    write_summary
  done

  if [ -n "$stop" ]; then
    stop_all
    for t in "${tasks[@]}"; do
      [ -f "$out/state/${t//:/_}" ] || echo skipped > "$out/state/${t//:/_}"
    done
    write_summary
  fi
  while [ ${#pids[@]} -gt 0 ]; do
    set +e
    wait -n -p done_pid
    rc=$?
    set -e
    if [ $rc -ne 0 ] && [ "$(cut -d' ' -f1 "$out/state/${pids[$done_pid]//:/_}")" = FAIL ]; then
      failed+=("${pids[$done_pid]}")
    fi
    unset "pids[$done_pid]"
    write_summary
  done

  echo
  cat "$summary"
  if [ ${#failed[@]} -gt 0 ]; then
    for t in "${failed[@]}"; do
      printf '\n==== %s (last 40 lines of %s)\n' "$t" "$out/logs/${t//:/_}.log"
      tail -n 40 "$out/logs/${t//:/_}.log"
    done
    printf '\nCI failed: %s\n' "${failed[*]}"
    exit 1
  fi
  printf '\nCI passed in %ds\n' "$((SECONDS - start))"
}

main "$@"
