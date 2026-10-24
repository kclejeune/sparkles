#!/usr/bin/env bash
# Regenerates the expected tokens of the analyzers that follow Lucene's, which
# crates/sparkles-core's test lucene_analyzers_agree_with_lucene checks:
# crates/sparkles-core/tests/data/lucene-analyzers/NAME.out holds the tokens that Lucene
# makes of each line of NAME.in.
#
#   scripts/text-lucene-compare/tokens.sh
#
# Lucene comes from the fuseki-server.jar of the flake's pinned Fuseki 5.1.0 (Lucene 9.11.1),
# and javac and java from the JDK its wrapper uses. FUSEKI overrides the fuseki-server
# executable. The inputs are words of the Snowball project's test vocabularies (English,
# French, Spanish, Italian, Portuguese, German and Russian), words of the OpenTaal Dutch
# and the DSSO Swedish dictionaries, and generated lines that mix those words with
# punctuation, numbers and CJK text.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
# shellcheck source=scripts/bench-tools.sh
source "$ROOT/scripts/bench-tools.sh"
has() { [ "$1" = jena ]; }
resolve_tools
FHOME=$(dirname "$(readlink -f "$FUSEKI")")
[ -f "$FHOME/fuseki-server.jar" ] || FHOME=$(dirname "$FHOME")
JAR=$FHOME/fuseki-server.jar
JDK=$(grep -o "/nix/store/[^'\":]*openjdk[^/'\":]*" "$FUSEKI" | head -1)
"$JDK/bin/javac" -proc:none -cp "$JAR" -d "$WORK" "$ROOT/scripts/text-lucene-compare/LuceneTokens.java"
DATA=$ROOT/crates/sparkles-core/tests/data/lucene-analyzers
while read -r name class; do
  "$JDK/bin/java" -cp "$JAR:$WORK" LuceneTokens "org.apache.lucene.analysis.$class" \
    < "$DATA/$name.in" > "$DATA/$name.out"
  echo "$name: $(wc -l < "$DATA/$name.out") lines"
done << 'LIST'
english-lucene en.EnglishAnalyzer
french-light fr.FrenchAnalyzer
spanish-light es.SpanishAnalyzer
italian-light it.ItalianAnalyzer
portuguese-light pt.PortugueseAnalyzer
german-light de.GermanAnalyzer
dutch-lucene nl.DutchAnalyzer
russian-lucene ru.RussianAnalyzer
swedish-lucene sv.SwedishAnalyzer
cjk-lucene cjk.CJKAnalyzer
LIST
