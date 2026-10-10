#!/usr/bin/env bash
# Clippy (warnings are errors) over the feature combinations that `mise run lint` does not
# build: the server with no optional feature, with each default feature on its own, and
# with the pairs whose code is shared (the MCP validation and format tools), and the
# library crates with their own features off. Code used only under some feature is gated
# on it, so an item that is dead without it fails here.
#
# Usage: scripts/lint-features.sh [extra cargo clippy args]
# `mise run lint:features`; every combination shares the workspace target directory.
set -euo pipefail

cd "$(dirname "$0")/.."

server=(
  ""
  reasoning
  shacl
  shex
  text
  geo
  geo-epsg
  otel
  auth
  mcp
  memory
  backup
  fmt
  tls
  graphql
  pdf
  pdf-ocr
  "mcp,shacl"
  "mcp,shex"
  "mcp,fmt"
  "graphql,shacl"
)

failed=()
run() {
  local what="$1"
  shift
  echo "==> clippy $what" >&2
  if ! cargo clippy "$@" --no-default-features "${extra[@]}" -- -D warnings; then
    failed+=("$what")
  fi
}

extra=("$@")
for f in "${server[@]}"; do
  run "sparkles-server [${f:-no features}]" -p sparkles-server --all-targets --features "$f"
done
run "sparkles-core [no features]" -p sparkles-core --all-targets
run "sparkles [no features]" -p sparkles --all-targets
run "sparkles-fmt [no features]" -p sparkles-fmt --all-targets
# the Rust client without its blocking facade
run "sparkles-client [no features]" -p sparkles-client --all-targets
# the library without a repository backend (its tests need the `fs` one)
run "sparkles-backup [no features, lib]" -p sparkles-backup --lib

if [ "${#failed[@]}" -gt 0 ]; then
  printf 'clippy failed for: %s\n' "${failed[@]}" >&2
  exit 1
fi
