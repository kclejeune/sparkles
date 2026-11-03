#!/usr/bin/env bash
# Fetch the external conformance suites that the Rust tests read, at pinned commits, as
# shallow sparse checkouts of only the directories the tests use (about 35 MB), and the
# W3C OWL 2 test cases as one file pinned by its digest (about 2 MB).
#
# The tests look for checkouts of apache/jena and oxigraph/oxigraph next to this
# repository and skip a suite they cannot find. This script lays the two out under DIR
# in the same shape (DIR/apache/jena and DIR/oxigraph/oxigraph) and prints the variables
# that point the tests at them, one NAME=value per line, for $GITHUB_ENV or for
# `export $(scripts/fetch-test-suites.sh DIR)`.
#
#   SPARKLES_W3C_DIR        the W3C SPARQL suites (rdf-tests-cg/sparql), which the engine,
#                           formatter and JSON-LD corpus tests read
#   SPARKLES_RDF_TESTS_DIR  the W3C RDF suites (rdf-tests-cg/rdf: Turtle, TriG, N-Triples,
#                           N-Quads, RDF/XML)
#   SPARKLES_SHACL_TESTS    the W3C SHACL suite as Jena vendors it, whose sibling
#                           directories hold the SHACL compact syntax tests
#   SPARKLES_SHEX_TESTS     the shexTest suite as Jena vendors it
#   SPARKLES_OXIGRAPH_DIR   Oxigraph's own JSON-LD documents
#   SPARKLES_OWL2_TESTS     the approved OWL 2 test cases of the W3C OWL Working Group,
#                           one RDF/XML file downloaded from w3.org and checked against
#                           its pinned SHA-256
#
# The JSON-LD API suite (SPARKLES_JSONLD_TESTS) is opt-in and not fetched.
#
# Usage: scripts/fetch-test-suites.sh [DIR]   (default: .artifacts/test-suites)
# A checkout already at its pinned commit is kept, so a second run fetches nothing new.
set -euo pipefail

# The pins. Raise them together with any known-failure lists the new commits change.
jena_commit=b1dcba53b57c51c5a735af1b5cf1884bfb61d31a
oxigraph_commit=e0f286b0ad0ab10f58c292377abf800b7f40301b
owl2_tests_url=https://www.w3.org/2009/11/owl-test/approved/all.rdf
owl2_tests_sha256=6236dab71c2264903131042808e79e4aa41afab528f7cf6e633d2e3926b89b04

root="$(cd "$(dirname "$0")/.." && pwd)"
dir="${1:-$root/.artifacts/test-suites}"
mkdir -p "$dir"
dir="$(cd "$dir" && pwd)"

# sparse_checkout <url> <commit> <dest> <path>...: a checkout of <commit> holding only the
# given directories, fetched without history and without the blobs outside them.
sparse_checkout() {
  local url="$1" commit="$2" dest="$3"
  shift 3
  if [ "$(git -C "$dest" rev-parse HEAD 2> /dev/null)" = "$commit" ]; then
    # the directories may differ from the last run's, and new ones are fetched lazily
    git -C "$dest" sparse-checkout set "$@"
    return 0
  fi
  rm -rf "$dest"
  mkdir -p "$dest"
  git -C "$dest" init -q
  git -C "$dest" remote add origin "$url"
  git -C "$dest" sparse-checkout set "$@"
  git -C "$dest" fetch -q --depth 1 --filter=blob:none origin "$commit"
  git -C "$dest" -c advice.detachedHead=false checkout -q FETCH_HEAD
}

jena="$dir/apache/jena"
oxigraph="$dir/oxigraph/oxigraph"
sparse_checkout https://github.com/apache/jena.git "$jena_commit" "$jena" \
  jena-arq/testing/rdf-tests-cg/sparql \
  jena-arq/testing/rdf-tests-cg/rdf \
  jena-arq/testing/RIOT/jsonld \
  jena-arq/testing/RIOT/jsonld11 \
  jena-arq/testing/RIOT/StreamManager \
  jena-arq/testing/RIOT/Reader \
  jena-shacl/src/test/files \
  jena-shex/src/test/files/spec >&2
sparse_checkout https://github.com/oxigraph/oxigraph.git "$oxigraph_commit" "$oxigraph" \
  testsuite/oxigraph-tests/jsonld >&2

# sha256 <file>: the hex digest, with sha256sum or, on macOS, shasum
sha256() {
  if command -v sha256sum > /dev/null; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# fetch_pinned <url> <sha256> <dest>: a download kept while its digest matches the pin
fetch_pinned() {
  local url="$1" sha="$2" dest="$3"
  if [ -f "$dest" ] && [ "$(sha256 "$dest")" = "$sha" ]; then
    return 0
  fi
  mkdir -p "$(dirname "$dest")"
  curl -fsSL --retry 3 -o "$dest.part" "$url"
  local got
  got="$(sha256 "$dest.part")"
  if [ "$got" != "$sha" ]; then
    rm -f "$dest.part"
    echo "fetch-test-suites: $url has SHA-256 $got, not the pinned $sha" >&2
    return 1
  fi
  mv "$dest.part" "$dest"
}

owl2_tests="$dir/w3c/owl2-tests/all.rdf"
fetch_pinned "$owl2_tests_url" "$owl2_tests_sha256" "$owl2_tests"

cat << EOF
SPARKLES_W3C_DIR=$jena/jena-arq/testing/rdf-tests-cg/sparql
SPARKLES_RDF_TESTS_DIR=$jena/jena-arq/testing/rdf-tests-cg/rdf
SPARKLES_SHACL_TESTS=$jena/jena-shacl/src/test/files/std
SPARKLES_SHEX_TESTS=$jena/jena-shex/src/test/files/spec
SPARKLES_OXIGRAPH_DIR=$oxigraph
SPARKLES_OWL2_TESTS=$owl2_tests
EOF
