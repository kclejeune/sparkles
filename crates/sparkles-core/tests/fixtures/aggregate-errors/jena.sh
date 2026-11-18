#!/usr/bin/env bash
# One query per aggregate, DISTINCT and argument form. Prints tab-separated aggregate, group and result.
# Run with Jena 6.2.0 on Java 21: ARQ=/path/to/apache-jena-6.2.0/bin/arq ./jena.sh > jena-6.2.0.tsv
ARQ=${ARQ:-arq}
VALUES='VALUES (?g ?v) {
 ("good" 1) ("good" 2) ("good" 2)
 ("unb" 1) ("unb" UNDEF) ("unb" 3)
 ("allunb" UNDEF) ("allunb" UNDEF)
 ("str" 1) ("str" "x") ("str" 3)
 ("iri" <http://ex/i>) ("iri" 2)
 ("lang" "a"@en) ("lang" "b")
}'
for agg in COUNT SUM AVG MIN MAX SAMPLE GROUP_CONCAT; do
  for d in "" "DISTINCT "; do
    for arg in '?v' '(?v * 1)'; do
      q="PREFIX : <http://ex/> SELECT ?g (${agg}(${d}${arg}) AS ?r) WHERE { $VALUES } GROUP BY ?g ORDER BY ?g"
      $ARQ --results=tsv --query=<(echo "$q") 2>&1 | tail -n +2 | sed "s/^/${agg}(${d}${arg})\t/"
      q="SELECT (${agg}(${d}${arg}) AS ?r) WHERE { VALUES ?v { 1 } FILTER(false) }"
      $ARQ --results=tsv --query=<(echo "$q") 2>&1 | tail -n +2 | sed "s/^/${agg}(${d}${arg})\t\"empty\"\t/"
    done
  done
done
