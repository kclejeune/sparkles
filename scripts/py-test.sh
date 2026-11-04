#!/usr/bin/env bash
# Build the Python extension (crates/sparkles-py) with cargo, assemble the `sparkles`
# package in target/py, and run the pytest suite on it (`mise run py:test`).
#
# The build shares the root target directory, so it reuses the workspace's compiled
# dependencies. The tests run with the `python3` on PATH when it has the pinned versions
# of crates/sparkles-py/tests/requirements-test.txt; otherwise with a virtual environment
# in target/py-venv, where pip installs them. A missing mypy or rdflib fails the run.
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

# The pinned test dependencies, which the wheel workflow installs too. The python3 on
# PATH runs the tests when it already has them at those versions, and a virtual
# environment in target/py-venv otherwise.
reqs="$crate/tests/requirements-test.txt"
pinned_in() {
  "$1" - "$reqs" << 'PY' 2> /dev/null
import importlib.metadata as m
import sys

for line in open(sys.argv[1]):
    line = line.split("#")[0].strip()
    if line:
        name, version = line.split("==")
        if m.version(name) != version:
            sys.exit(1)
PY
}
python=python3
if ! pinned_in "$python"; then
  venv=target/py-venv
  python="$venv/bin/python"
  if [ ! -x "$python" ]; then
    echo "creating $venv for the pinned test dependencies" >&2
    python3 -m venv "$venv"
  fi
  if ! pinned_in "$python"; then
    "$python" -m pip install --quiet --disable-pip-version-check -r "$reqs"
  fi
fi

# no bytecode or pytest cache in the source tree
SPARKLES_PY_TEST_DEPS=required PYTHONDONTWRITEBYTECODE=1 PYTHONPATH="$PWD/$out" exec "$python" -m pytest -p no:cacheprovider "$crate/tests" "$@"
