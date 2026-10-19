#!/usr/bin/env bash
# Regenerate the expected answers of the RDFS-on-read comparison with Apache Jena (a
# developer tool, not run in CI). For each suite directory of
# crates/sparkles-core/tests/rdfs, JenaRdfs.java wraps data.trig with RDFS on read over the
# suite's schema.ttl, as `ja:DatasetRDFS` and Fuseki's `--rdfs` do, and runs every query of
# its queries.txt. The SPARQL JSON results go to expected.json, by suite and query name.
#
#   scripts/rdfs-jena-expected.sh
#
# $JENA_HOME is a Jena distribution (default: this flake's apache-jena) and $JAVA a Java 17 or
# later (default: `java` on PATH, else the locked nixpkgs jdk).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$ROOT/target/rdfs-jena
# shellcheck source=scripts/bench-tools.sh
source "$ROOT/scripts/bench-tools.sh"
jena=${JENA_HOME:-$(nixpkg apache-jena)}
java=${JAVA:-$(nixbin jdk java)}
cd "$ROOT/crates/sparkles-core/tests/rdfs"

suites=()
for d in */; do suites+=("${d%/}"); done
"$java" -cp "$jena/lib/*" JenaRdfs.java "${suites[@]}" 2> /dev/null | jq -S . > expected.json
echo "wrote $(pwd)/expected.json" >&2
