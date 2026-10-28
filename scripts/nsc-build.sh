#!/usr/bin/env bash
# Build a native release server and copy it while its managed target is leased.
set -euo pipefail

if [ "$#" -lt 1 ]; then
  echo 'usage: nsc-build.sh OUTPUT [native release cargo flags...]' >&2
  exit 2
fi
output=$(realpath -m "$1")
shift
if [ -n "${CARGO_BUILD_TARGET:-}" ]; then
  echo 'nsc-build: CARGO_BUILD_TARGET is unsupported; supply a prebuilt binary instead' >&2
  exit 2
fi
need_features=
for arg in "$@"; do
  if [ -n "$need_features" ]; then
    if [ -z "$arg" ] || [[ "$arg" == -* ]]; then
      echo 'nsc-build: --features requires a value' >&2
      exit 2
    fi
    need_features=
    continue
  fi
  case "$arg" in
    --features | -F) need_features=1 ;;
    --features=?* | -F?* | --no-default-features | --all-features | --locked | --offline | --frozen) ;;
    *)
      echo 'nsc-build: only feature and dependency-resolution flags are supported; supply a prebuilt binary for other settings' >&2
      exit 2
      ;;
  esac
done
if [ -n "$need_features" ]; then
  echo 'nsc-build: --features requires a value' >&2
  exit 2
fi
# Each archived source tree uses its own default target. Boxington normalizes that
# workspace/target pair while sharing immutable compiler results across revisions.
unset CARGO_TARGET_DIR
exec mbx sparkles-task -- bash -euo pipefail -s -- "$output" "$@" << 'BUILD'
output=$1
shift
mbx build --release -p sparkles-server "$@"
mkdir -p "$(dirname "$output")"
cp target/release/sparkles "$output.tmp"
mv "$output.tmp" "$output"
BUILD
