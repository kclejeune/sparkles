#!/usr/bin/env bash
# Prepare a host to run gates or conformance halves for scripts/gate/gate.sh. Run it from
# the laptop. It is idempotent and changes only ~/gate and ~/lanes/.heavy.lock on the host.
#
# Usage: scripts/gate/setup-host.sh <host> [suites-source-host]
#
# It creates ~/gate/{bin,out}, the bare repository ~/gate/repo.git, the ~/gate/.lock
# symlink to ~/lanes/.heavy.lock, ~/gate/suites copied from the source host (default
# wally) through the laptop, ~/gate/conformance.env and the pytest venv ~/gate/py-venv.
# It then checks linger, mbx, mise and free disk. The mise tools themselves install in
# each gate's first step. The benchmark host, SPARKLES_BENCH_HOST (default forge, read as
# gate.sh reads it), is refused, so swapping roles means setting that first and then preparing the new gate host.
set -euo pipefail
host=${1:?usage: setup-host.sh <host> [suites-source-host]}
src=${2:-wally}
# shellcheck source=scripts/gate/config.sh
. "$(dirname "$(readlink -f "$0")")/config.sh"
if is_bench "$host"; then
  echo "$host is the benchmark host (SPARKLES_BENCH_HOST) and never runs gate work" >&2
  exit 2
fi
# shellcheck source=scripts/remote-executor.sh
. "$(dirname "$(readlink -f "$0")")/../remote-executor.sh"
case $host in boat:*) rx_admit "$host" --minimum "${SPARKLES_REMOTE_MIN_LIFETIME:-7200}" > /dev/null ;; esac

# shellcheck disable=SC2016
rx_exec "$host" '
  set -e
  mkdir -p "$HOME/gate/bin" "$HOME/gate/out" "$HOME/gate/suites" "$HOME/lanes"
  [ -d "$HOME/gate/repo.git" ] || git init -q --bare "$HOME/gate/repo.git"
  touch "$HOME/lanes/.heavy.lock"
  [ -L "$HOME/gate/.lock" ] || ln -s "$HOME/lanes/.heavy.lock" "$HOME/gate/.lock"'

if [ "$host" != "$src" ]; then
  stage=$(mktemp -d)
  trap 'rm -rf "$stage"' EXIT
  rx_sync -- -a "$src:gate/suites/" "$stage/"
  rx_sync -- -a "$stage/" "$host:gate/suites/"
fi

# shellcheck disable=SC2016
rx_exec "$host" '
  set -e
  s=$HOME/gate/suites
  cat > "$HOME/gate/conformance.env" << EOF
SPARKLES_W3C_DIR=$s/apache/jena/jena-arq/testing/rdf-tests-cg/sparql
SPARKLES_RDF_TESTS_DIR=$s/apache/jena/jena-arq/testing/rdf-tests-cg/rdf
SPARKLES_SHACL_TESTS=$s/apache/jena/jena-shacl/src/test/files/std
SPARKLES_SHEX_TESTS=$s/apache/jena/jena-shex/src/test/files/spec
SPARKLES_OXIGRAPH_DIR=$s/oxigraph/oxigraph
EOF
  if [ ! -x "$HOME/gate/py-venv/bin/pytest" ]; then
    python3 -m venv "$HOME/gate/py-venv"
    "$HOME/gate/py-venv/bin/pip" install -q pytest==9.1.1 iniconfig==2.3.1 packaging==26.3 pluggy==1.7.0 Pygments==2.21.0
  fi
  echo "host=$(hostname) $(loginctl show-user "$USER" -p Linger)"
  echo "mbx: $(mbx --version)  mise: $(mise --version 2> /dev/null | head -1)"
  echo "pytest: $("$HOME/gate/py-venv/bin/pytest" --version 2>&1)"
  echo "free: $(df -h --output=avail "$HOME" | tail -n1) on $HOME"'
