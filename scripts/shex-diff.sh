#!/usr/bin/env bash
# Differential check of ShEx validation against another implementation (a developer
# tool, not run in CI). Validates DATA against SCHEMA with the shape map MAP using
# `sparkles shex validate`, and with an oracle command, then compares the result maps
# (one `<node>@<shape>` or `<node>@!<shape>` line per association, order ignored).
#
#   scripts/shex-diff.sh [--compare-rudof] DATA SCHEMA MAP
#
# The oracle is $SHEX_ORACLE, a command template in which {data}, {schema} and {map}
# are replaced by the file names; it must print a compact result map with absolute IRIs.
# --compare-rudof uses an installed `rudof` binary (never a build dependency):
#   rudof shex-validate --data {data} --schema {schema} --shapemap {map} --result-format compact
# Override $SHEX_ORACLE when the installed version names its options differently.
# $SPARKLES is the sparkles binary (default: target/release/sparkles, else cargo run).
set -euo pipefail

usage() {
  sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
}

oracle="${SHEX_ORACLE:-}"
if [[ ${1:-} == --compare-rudof ]]; then
  shift
  oracle="${SHEX_ORACLE:-rudof shex-validate --data {data} --schema {schema} --shapemap {map} --result-format compact}"
fi
[[ $# -eq 3 ]] || usage
data=$1 schema=$2 map=$3
[[ -n $oracle ]] || {
  echo "no oracle: set SHEX_ORACLE or pass --compare-rudof" >&2
  exit 2
}

root=$(cd "$(dirname "$0")/.." && pwd)
if [[ -n ${SPARKLES:-} ]]; then
  sparkles=("$SPARKLES")
elif [[ -x $root/target/release/sparkles ]]; then
  sparkles=("$root/target/release/sparkles")
else
  sparkles=(cargo run --quiet --release --manifest-path "$root/Cargo.toml" -p sparkles-server --)
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Normalize a compact result map: one association per line, blank lines and comments
# dropped, sorted.
normalize() {
  grep -v -e '^[[:space:]]*$' -e '^[[:space:]]*#' | sed 's/[[:space:]]*$//' | LC_ALL=C sort
}

# sparkles exits 1 when something does not conform: that is a result, not an error
status=0
"${sparkles[@]}" shex validate --data "$data" --schema "$schema" --map "$map" \
  --format smap > "$tmp/sparkles.raw" || status=$?
if [[ $status -gt 1 ]]; then
  echo "sparkles failed (exit $status)" >&2
  exit "$status"
fi
normalize < "$tmp/sparkles.raw" > "$tmp/sparkles"

cmd=${oracle//\{data\}/$(printf %q "$data")}
cmd=${cmd//\{schema\}/$(printf %q "$schema")}
cmd=${cmd//\{map\}/$(printf %q "$map")}
status=0
bash -c "$cmd" > "$tmp/oracle.raw" || status=$?
if [[ $status -gt 1 ]]; then
  echo "the oracle failed (exit $status): $cmd" >&2
  exit "$status"
fi
normalize < "$tmp/oracle.raw" > "$tmp/oracle"

if diff -u --label sparkles --label oracle "$tmp/sparkles" "$tmp/oracle"; then
  echo "identical: $(wc -l < "$tmp/sparkles") associations"
else
  echo "DIFFERENT" >&2
  exit 1
fi
