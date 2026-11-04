#!/usr/bin/env bash
# Build the Python extension (crates/sparkles-py) with cargo, assemble the `sparkles`
# package in target/py, and run the pytest suite on it (`mise run py:test`).
#
# The build shares the root target directory, so it reuses the workspace's compiled
# dependencies. The tests run with the `python3` on PATH when it has the pinned versions
# of crates/sparkles-py/tests/requirements-test.txt; otherwise with a virtual environment
# in target/py-venv, where pip installs them. A missing mypy or rdflib fails the run.
#
# The stub test also runs on the oldest Python that the package supports, which
# crates/sparkles-py/pyproject.toml gives as requires-python. uv makes a virtual
# environment for it in target/py-venv-<version> with the same pinned dependencies.
# Without uv the run prints a warning and goes on, unless SPARKLES_PY_OLDEST=required,
# which the batch gate sets, and then it fails.
#
# Usage: scripts/py-test.sh [pytest args]
# PY_PROFILE=release tests a release build. The pytest args apply to the full suite and
# not to the stub test on the oldest Python.
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

# The oldest supported Python, from the package metadata.
oldest=$(sed -n 's/^requires-python *= *">=\([0-9][0-9.]*\)".*/\1/p' "$crate/pyproject.toml")
if [ -z "$oldest" ]; then
  echo "py-test: no requires-python = \">=X.Y\" in $crate/pyproject.toml" >&2
  exit 1
fi

# The stub test on the oldest Python. It found errors that appear only there, such as a
# class whose __slots__ differ on CPython 3.10.
stubtest_oldest() {
  local current msg venv
  current=$("$python" -c 'import sys; print("%d.%d" % sys.version_info[:2])')
  # On the oldest Python itself the full suite has run, unless the caller's arguments
  # filtered the stub test out, so the dedicated run uses this interpreter and needs no uv.
  if [ "$current" = "$oldest" ]; then
    SPARKLES_PY_TEST_DEPS=required PYTHONDONTWRITEBYTECODE=1 PYTHONPATH="$PWD/$out" \
      "$python" -m pytest -p no:cacheprovider "$crate/tests/test_stubs.py" -k test_stubtest
    return
  fi
  if ! command -v uv > /dev/null; then
    msg="uv is not installed, so the stub test on Python $oldest did not run. Install uv (https://docs.astral.sh/uv/)."
    if [ "${SPARKLES_PY_OLDEST:-}" = required ]; then
      echo "py-test: $msg" >&2
      return 1
    fi
    echo "py-test: warning: $msg" >&2
    return 0
  fi
  venv=target/py-venv-$oldest
  echo "== stub test on Python $oldest ==" >&2
  uv venv --quiet --allow-existing --python "$oldest" "$venv" &&
    uv pip install --quiet --python "$venv/bin/python" -r "$reqs" &&
    SPARKLES_PY_TEST_DEPS=required PYTHONDONTWRITEBYTECODE=1 PYTHONPATH="$PWD/$out" \
      "$venv/bin/python" -m pytest -p no:cacheprovider "$crate/tests/test_stubs.py" -k test_stubtest
}

# no bytecode or pytest cache in the source tree
status=0
SPARKLES_PY_TEST_DEPS=required PYTHONDONTWRITEBYTECODE=1 PYTHONPATH="$PWD/$out" "$python" -m pytest -p no:cacheprovider "$crate/tests" "$@" || status=$?
stubtest_oldest || status=$?
exit "$status"
