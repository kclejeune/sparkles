#!/usr/bin/env bash
# Build the seed corpora of the formatter's fuzz targets (corpus/<target>/, git-ignored)
# from the golden inputs and, when present, the W3C, SHACL and JSON-LD test suites. The
# suites are read where they are checked out, never copied into the repository. Each seed
# is the text after a five-byte options header (see src/invariants.rs): all zero (the
# default options), with the variant bit for TriG and N-Quads.
#
# Usage: seed.sh [target...]   (default: every target)
# Corpora (the same variables as the test suites; each one is optional):
#   SPARKLES_W3C_DIR       the SPARQL suite (rdf-tests-cg/sparql); the RDF suite is the
#                          `rdf` directory next to it, and Jena's RIOT tests (JSON-LD
#                          documents) two levels up; its JSON results seed JSON-LD too
#   SPARKLES_RDF_TESTS_DIR the RDF suite, when it is elsewhere
#   SPARKLES_SHACL_TESTS   the SHACL suite (Turtle shapes graphs)
#   SPARKLES_JSONLD_TESTS  the JSON-LD API suite's tests directory
set -euo pipefail

fuzz="$(cd "$(dirname "$0")" && pwd)"
golden="$fuzz/../tests/golden"
w3c="${SPARKLES_W3C_DIR:-}"
rdf="${SPARKLES_RDF_TESTS_DIR:-${w3c:+$w3c/../rdf}}"
riot="${w3c:+$w3c/../../RIOT}"
shacl="${SPARKLES_SHACL_TESTS:-}"
jsonld="${SPARKLES_JSONLD_TESTS:-}"
# seeds above this size only slow the fuzzer down
max_bytes=65536

# seed TARGET HEADER (printf %b escapes) DIR PATTERN...: one seed per file
seed() {
  local target="$1" header="$2" dir="$3"
  shift 3
  [ -n "$dir" ] && [ -d "$dir" ] || return 0
  local out="$fuzz/corpus/$target" args=() p f tmp n=0
  mkdir -p "$out"
  for p in "$@"; do
    args+=(-o -name "$p")
  done
  tmp="$(mktemp)"
  while IFS= read -r -d '' f; do
    [ "$(wc -c < "$f")" -le "$max_bytes" ] || continue
    {
      printf '%b' "$header"
      cat "$f"
    } > "$tmp"
    cp "$tmp" "$out/$(sha1sum "$tmp" | cut -c1-40)"
    n=$((n + 1))
  done < <(find -L "$dir" -type f \( -false "${args[@]}" \) -print0)
  rm -f "$tmp"
  echo "$target: $n seeds from $dir"
}

plain='\0\0\0\0\0'
variant='\0\0020\0\0\0'

targets=("$@")
[ ${#targets[@]} -gt 0 ] || targets=(fmt_sparql fmt_turtle fmt_lines fmt_jsonld)
for target in "${targets[@]}"; do
  case "$target" in
    fmt_sparql)
      seed "$target" "$plain" "$golden/sparql" '*.in.rq' '*.in.ru'
      seed "$target" "$plain" "$w3c" '*.rq' '*.ru'
      ;;
    fmt_turtle)
      seed "$target" "$plain" "$golden/turtle" '*.in.ttl'
      seed "$target" "$variant" "$golden/trig" '*.in.trig'
      seed "$target" "$plain" "$rdf" '*.ttl'
      seed "$target" "$variant" "$rdf" '*.trig'
      seed "$target" "$plain" "$shacl" '*.ttl'
      ;;
    fmt_lines)
      seed "$target" "$plain" "$golden/ntriples" '*.in.nt'
      seed "$target" "$variant" "$golden/nquads" '*.in.nq'
      seed "$target" "$plain" "$rdf" '*.nt'
      seed "$target" "$variant" "$rdf" '*.nq'
      ;;
    fmt_jsonld)
      seed "$target" "$plain" "$golden/jsonld" '*.in.jsonld'
      seed "$target" "$plain" "$riot" '*.jsonld'
      # SPARQL JSON results: plain JSON documents
      seed "$target" "$plain" "$w3c" '*.srj' '*.json'
      seed "$target" "$plain" "$jsonld" '*.jsonld' '*.json'
      ;;
    *)
      echo "seed.sh: unknown target $target" >&2
      exit 2
      ;;
  esac
done
