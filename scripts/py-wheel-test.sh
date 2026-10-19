#!/usr/bin/env bash
# Install a built wheel (or the sdist) of the Python bindings into a fresh virtual
# environment for each given interpreter, and run the pytest suite against the
# installation. The release workflow (.github/workflows/python-wheels.yml) runs it on
# every platform it builds for.
#
# Usage: scripts/py-wheel-test.sh <package file> <python>...
# The tests run from a copy outside the checkout, so that they import the installed
# package. The copy keeps the checkout's layout, because some tests read fixtures of
# other crates by relative path (Jena's patch files under crates/sparkles-core/tests, and
# the type stub and the binding inventory that the parity test compares).
# TEST_DEPS (default: pytest rdflib mypy) are installed next to it.
set -euo pipefail

package="$1"
shift
deps="${TEST_DEPS:-pytest rdflib mypy}"
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/crates/sparkles-py/python/sparkles" "$work/crates/sparkles-core/tests" "$work/crates/sparkles"
cp -r "$root/crates/sparkles-py/tests" "$work/crates/sparkles-py/tests"
cp -r "$root/crates/sparkles-core/tests/patch" "$work/crates/sparkles-core/tests/patch"
cp "$root/crates/sparkles-py/python/sparkles/_sparkles.pyi" "$work/crates/sparkles-py/python/sparkles/"
cp "$root/crates/sparkles/bindings.toml" "$work/crates/sparkles/"

for python in "$@"; do
  venv="$work/venv"
  rm -rf "$venv"
  "$python" -m venv "$venv"
  if [ -x "$venv/bin/python" ]; then
    py="$venv/bin/python"
  else
    py="$venv/Scripts/python"
  fi
  echo "== $("$py" --version) =="
  # shellcheck disable=SC2086 # the dependencies are a word list
  "$py" -m pip install --quiet "$package" $deps
  (cd "$work" && "$py" -m pytest -p no:cacheprovider crates/sparkles-py/tests)
done
