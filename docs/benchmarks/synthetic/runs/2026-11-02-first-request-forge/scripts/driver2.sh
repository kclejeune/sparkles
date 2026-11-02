#!/usr/bin/env bash
# driver2.sh: reruns of the flagged warm queries, under one hold of the bench and heavy locks
set -uo pipefail
cd ~/lanes/cold-sparse-4
exec 9> "$HOME/lanes/.bench.lock" 8> "$HOME/lanes/.heavy.lock"; flock 9; flock 8; export LOCKS_HELD=1
echo "locks held $(date -u +%FT%TZ)"
QF="minus regex-iri lang-filter contains expr-agg-arg optional-chain not-exists"
A="21da069d|||warm" B="b1f206b9|||warm" ./ab.sh warmflag 10m 21 4 $QF; echo "warmflag exit $?"
A="b1f206b9||--no-warm-up --no-code-prefault|warm" B="b1f206b9|||warm" ./ab.sh warmoff 10m 21 2 $QF; echo "warmoff exit $?"
echo DRIVER2DONE
