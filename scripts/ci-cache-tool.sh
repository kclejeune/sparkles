#!/usr/bin/env bash
# Swatinem applies cmd-format to Cargo metadata and Rust identity probes.
set -euo pipefail
tool=$1
shift
case "$tool" in
  cargo)
    # Cache setup must not repair an out-of-date lockfile before the build's gate.
    exec mbx "$@" --locked
    ;;
  rustc | rustup) exec "$tool" "$@" ;;
  *)
    echo "Unsupported cache-action tool: $tool" >&2
    exit 64
    ;;
esac
