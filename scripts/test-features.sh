#!/usr/bin/env bash
# Run the server's unit and integration tests with the default features off and one
# optional feature set on, for each set in turn. The ordinary test run builds the server
# with its default features only, so a test that assumes a route, command or flag of a
# feature that is off (or one that is on) passes there and fails here. `mise run
# lint:features` builds the same sets with Clippy but runs no test.
#
# Usage: scripts/test-features.sh [set...]
# Each set is a comma-separated feature list, or `none` for no feature. With no argument
# the sets are none, auth, mcp, memory, graphql and shacl. Every set runs even after an
# earlier one fails, a line for each set shows its result and time, and the exit status is
# non-zero when any set failed. The sets share the workspace target directory, so
# dependencies compile once, and CARGO_BUILD_JOBS limits the jobs as for any cargo run.
# The tests run under cargo-nextest when it is installed and under cargo test otherwise.
# Each set's output is in target/test-features/<set>.log.
# `mise run test:features`.
set -euo pipefail

cd "$(dirname "$0")/.."

sets=("$@")
[ ${#sets[@]} -gt 0 ] || sets=(none auth mcp memory graphql shacl)

logs=target/test-features
mkdir -p "$logs"

# cargo-nextest when it is installed, as `mise run test` uses it: a test that hangs, such
# as one that waits for a connection its own build can never make, is stopped after the
# time that .config/nextest.toml allows instead of holding the run forever. Plain cargo
# test is the fallback.
if cargo nextest --version > /dev/null 2>&1; then
  runner=(cargo nextest run --no-fail-fast)
else
  runner=(cargo test --no-fail-fast)
fi

results=()
failed=0
for s in "${sets[@]}"; do
  features=$s
  [ "$s" != none ] || features=
  log="$logs/${s//,/+}.log"
  echo "==> ${runner[*]} sparkles-server [${features:-no features}]" >&2
  start=$SECONDS
  # --bins and --tests build the unit-test binary and every integration test, and the
  # integration tests run the server binary built with the same features. Doctests are
  # left to the ordinary run, since the server has no library target.
  if "${runner[@]}" -p sparkles-server --no-default-features --features "$features" \
    --bins --tests 2>&1 | tee "$log" >&2; then
    status=pass
  else
    status=FAIL
    failed=1
  fi
  results+=("$(printf '%-4s %5ds  %s' "$status" "$((SECONDS - start))" "$s")")
  if [ "$status" = FAIL ]; then
    grep -E '^test .* FAILED$|^ +(FAIL|TIMEOUT|SIGSEGV|SIGABRT) +\[' "$log" | sed 's/^/     /' >&2 || true
  fi
done

echo >&2
echo "Feature test summary" >&2
printf '%s\n' "${results[@]}" >&2
exit "$failed"
