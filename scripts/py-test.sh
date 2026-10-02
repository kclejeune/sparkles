#!/usr/bin/env bash
# Build the Python extension (crates/sparkles-py) with cargo, assemble the `sparkles`
# package in target/py, and run the pytest suite on it (`mise run py:test`).
#
# The build shares the root target directory, so it reuses the workspace's compiled
# dependencies. The tests run with the `python3` on PATH when it has pytest, as the dev
# shell's does; otherwise with a virtual environment in target/py-venv, where pytest is
# installed with pip on first use.
#
# Usage: scripts/py-test.sh [pytest args]
# PY_PROFILE=release tests a release build.
set -euo pipefail

cd "$(dirname "$0")/.."

crate=crates/sparkles-py
profile="${PY_PROFILE:-dev}"
dir=debug
[ "$profile" = dev ] || dir="$profile"

# maturin sets this for its builds; with it, PyO3 links like an extension module
PYO3_BUILD_EXTENSION_MODULE=1 cargo build --manifest-path "$crate/Cargo.toml" \
  --target-dir target --profile "$profile"

lib="target/$dir/lib_sparkles.so"
[ -e "$lib" ] || lib="target/$dir/lib_sparkles.dylib"
out=target/py
rm -rf "$out"
mkdir -p "$out"
cp -r "$crate/python/sparkles" "$out/sparkles"
cp "$lib" "$out/sparkles/_sparkles.abi3.so"

python=python3
if ! "$python" -c 'import pytest' 2> /dev/null; then
  venv=target/py-venv
  if [ ! -x "$venv/bin/python" ]; then
    echo "pytest is not installed: creating $venv" >&2
    python3 -m venv "$venv"
    "$venv/bin/python" -m pip install --quiet pytest
  fi
  python="$venv/bin/python"
fi

# no bytecode or pytest cache in the source tree
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH="$PWD/$out" exec "$python" -m pytest -p no:cacheprovider "$crate/tests" "$@"
