#!/usr/bin/env bash
# driver.sh: the final forge steps of the lane, all under one hold of the bench and heavy locks
set -uo pipefail
cd ~/lanes/cold-sparse-4
exec 9> "$HOME/lanes/.bench.lock" 8> "$HOME/lanes/.heavy.lock"; flock 9; flock 8; export LOCKS_HELD=1
echo "locks held $(date -u +%FT%TZ)"
Q28="class-predicate-list class-predicates contains count-all distinct-join distinct-obj employee-docs exists-join export-500k expr-agg-arg expr-bind-group expr-order-key group-avg knows-reach lang-filter minus not-exists optional-chain optional-count order-by-full path-plus predicate-counts predicate-subjects range-topk regex-iri star-join star-lookup subquery-agg two-hop-count types-grouped values-star"
Q28=$(echo $Q28)
Q27=${Q28/export-500k /}
export A="21da069d|||cold" B="b1f206b9|||cold"
./ab.sh suite10 10m 7 2 $Q28; echo "suite10 exit $?"
./ab.sh suite1 1m 7 1 $Q28; echo "suite1 exit $?"
A="21da069d|||warm" B="b1f206b9|||warm" ./ab.sh warm10 10m 21 1 $Q27; echo "warm10 exit $?"
./engines.sh eng 2 "21da069d b1f206b9" "path-plus star-lookup values-star employee-docs star-join"; echo "engines exit $?"
echo DRIVERDONE
