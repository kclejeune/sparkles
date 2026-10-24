#!/usr/bin/env bash
# Regenerates the expected tokens of the analyzers that follow Lucene's, which
# crates/sparkles-core's test lucene_analyzers_agree_with_lucene checks:
# crates/sparkles-core/tests/data/lucene-analyzers/NAME.out holds the tokens that Lucene
# makes of each line of NAME.in, and standard-tokenizer.out holds the tokens, offsets and
# types that Lucene's StandardTokenizer makes of each line of standard-tokenizer.in
# (which standard-inputs.py writes). lucene-dict-analyzers/ja-dict.out and ko-dict.out hold
# the tokens that Kuromoji's JapaneseAnalyzer and Nori's KoreanAnalyzer make of each line
# of ja-dict.in and ko-dict.in.
#
#   scripts/text-lucene-compare/tokens.sh
#
# Lucene comes from the fuseki-server.jar of the flake's pinned Fuseki 5.1.0 (Lucene 9.11.1),
# and javac and java from the JDK its wrapper uses. FUSEKI overrides the fuseki-server
# executable. The inputs are words of the Snowball project's test vocabularies (English,
# French, Spanish, Italian, Portuguese, German and Russian), words of the OpenTaal Dutch
# dictionary, Swedish words written for these tests, and generated lines that mix those
# words with punctuation, numbers and CJK text. The Japanese and Korean inputs are
# sentences, words of IPADIC and mecab-ko-dic, and generated lines that join those words
# with Latin, Hangul, Kana, Han, symbols and spaces. The Kuromoji and Nori jars of Lucene
# 9.11.1 come from Maven Central and are checked against their SHA-256.
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
MAVEN=https://repo1.maven.org/maven2/org/apache/lucene
while read -r module sha; do
  curl -fsSL -o "$WORK/$module.jar" "$MAVEN/lucene-analysis-$module/9.11.1/lucene-analysis-$module-9.11.1.jar"
  echo "$sha  $WORK/$module.jar" | sha256sum -c --quiet
done << 'JARS'
kuromoji e48721f62cf61ab2084c6ad504194a1bfcf87ba7d3d0274a50466d9a2c5e0f4f
nori de2092a41c4aeb135e8e240b977dabddf44e558926a836af0c8f3d81b99042dd
JARS
"$JDK/bin/javac" -proc:none -cp "$JAR" -d "$WORK" "$ROOT/scripts/text-lucene-compare/LuceneTokens.java" \
  "$ROOT/scripts/text-lucene-compare/StandardTokens.java"
DATA=$ROOT/crates/sparkles-core/tests/data/lucene-analyzers
"$JDK/bin/java" -cp "$JAR:$WORK" StandardTokens \
  < "$DATA/standard-tokenizer.in" > "$DATA/standard-tokenizer.out"
echo "standard-tokenizer: $(wc -l < "$DATA/standard-tokenizer.out") lines"
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
DICT=$ROOT/crates/sparkles-core/tests/data/lucene-dict-analyzers
for pair in ja-dict:ja.JapaneseAnalyzer:kuromoji ko-dict:ko.KoreanAnalyzer:nori; do
  IFS=: read -r name class module <<< "$pair"
  "$JDK/bin/java" -cp "$JAR:$WORK/$module.jar:$WORK" LuceneTokens "org.apache.lucene.analysis.$class" \
    < "$DICT/$name.in" > "$DICT/$name.out"
  echo "$name: $(wc -l < "$DICT/$name.out") lines"
done
