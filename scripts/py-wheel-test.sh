#!/usr/bin/env bash
# Install a built wheel (or the sdist) of the Python bindings into a fresh virtual
# environment for each given interpreter, and run the pytest suite against the
# installation. The release workflow (.github/workflows/python-wheels.yml) runs it on
# every platform it builds for.
#
# Usage: scripts/py-wheel-test.sh <package file> <python>...
# The tests run from a copy outside the checkout, so that they import the installed
# package. TEST_DEPS (default: pytest rdflib mypy) are installed next to it.
set -euo pipefail

package="$1"
shift
deps="${TEST_DEPS:-pytest rdflib mypy}"
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cp -r "$root/crates/sparkles-py/tests" "$work/tests"

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
  (cd "$work" && "$py" -m pytest -p no:cacheprovider tests)
done
