#!/usr/bin/env bash
# Run Apache Jena's own HTTP clients (RDFConnectionRemote, RDFConnectionFuseki, GSP, DSP,
# QueryExecHTTP, UpdateExecHTTP) and Fuseki-style admin calls against a Sparkles server
# that serves a temporary data directory (testsuite/jena-clients/JenaClients.java).
#
# Usage: scripts/test-jena-clients.sh
#   SPARKLES_BIN  the sparkles binary (default: a debug build, built here)
#   JENA_HOME     an Apache Jena distribution (default: nixpkgs' apache-jena)
#   JAVA          a java of version 21 or later (default: nixpkgs' jdk)
#   PORT          the server's port (default 5230)
# `mise run test:jena-clients`; the flake's `jena-clients` check runs it too.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
port="${PORT:-5230}"

bin="${SPARKLES_BIN:-}"
if [ -z "$bin" ]; then
  cargo build --manifest-path "$root/Cargo.toml" -p sparkles-server
  bin="$root/target/debug/sparkles"
fi

if [ -z "${JENA_HOME:-}" ] || [ -z "${JAVA:-}" ]; then
  mapfile -t paths < <(nix build --inputs-from "$root" nixpkgs#apache-jena nixpkgs#jdk \
    --no-link --print-out-paths)
  JENA_HOME="${JENA_HOME:-${paths[0]}}"
  JAVA="${JAVA:-${paths[1]}/bin/java}"
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/sparkles-jena-clients.XXXXXX")"
server_pid=""
# shellcheck disable=SC2329 # run by the EXIT trap
cleanup() {
  if [ -n "$server_pid" ]; then
    kill "$server_pid" 2> /dev/null || true
    wait "$server_pid" 2> /dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

"$bin" serve --data "$work/data" --host 127.0.0.1 --port "$port" --gsp-direct-naming \
  --no-access-log > "$work/server.log" 2>&1 &
server_pid=$!

for _ in $(seq 1 200); do
  if curl -fsS "http://127.0.0.1:$port/\$/ping" > /dev/null 2>&1; then
    break
  fi
  if ! kill -0 "$server_pid" 2> /dev/null; then
    cat "$work/server.log" >&2
    echo "the server exited" >&2
    exit 1
  fi
  sleep 0.1
done

status=0
"$JAVA" -cp "$JENA_HOME/lib/*" -Dlog4j2.level=WARN \
  "$root/testsuite/jena-clients/JenaClients.java" "http://127.0.0.1:$port" || status=$?
if [ "$status" -ne 0 ]; then
  echo "--- server log (last 40 lines)" >&2
  tail -n 40 "$work/server.log" >&2
fi
exit "$status"
