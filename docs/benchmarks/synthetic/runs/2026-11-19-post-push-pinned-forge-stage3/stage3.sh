#!/usr/bin/env bash
# stage3.sh: under ~/lanes/.bench.lock and ~/lanes/.heavy.lock, the pinned checks owed by the
# screens of batches 33 and 35 and the interleaved A/Bs of the batch-32/33 matrix flags above
# 0.3 ms, at 1.05M triples unless DATA names the 10.5M file. Pairs, in order:
#   sqa-b35-auto    ffb18866 1cbc23cd  subquery-agg (batch 35 flag) + cross-checks, 1 thread, auto
#   sqa-b35-stream  ffb18866 1cbc23cd  subquery-agg, types-grouped, 1 thread, streaming
#   tg-b33-auto     eda22fce bccc5f7b  types-grouped (batch 33 flag) + cross-checks, 1 thread, auto
#   tg-b33-eager    eda22fce bccc5f7b  types-grouped, 1 thread, eager (fixed mode, as the auto servers may settle differently)
#   tg-b33-stream   eda22fce bccc5f7b  types-grouped, 1 thread, streaming
#   big-b32-12t-stream eda22fce bccc5f7b  order-by-full class-predicates optional-count optional-chain export-500k, 12 threads, streaming
#   big-b32-12t-eager  eda22fce bccc5f7b  order-by-full, 12 threads, eager
#   big-b32-1t-stream  eda22fce bccc5f7b  export-500k optional-chain, 1 thread, streaming
#   big-b32-10m-12t-stream eda22fce bccc5f7b order-by-full group-avg expr-order-key optional-chain, 10.5M, 12 threads, streaming
#   big-b32-10m-12t-eager  eda22fce bccc5f7b order-by-full, 10.5M, 12 threads, eager
#   drift-cc94-1t-stream cc94ac7f 71c277b0 (current main) lang-filter regex-iri optional-chain optional-count export-500k, 1 thread, streaming
#     (the X14 drift since the fixed baseline, before any bisect)
# Last line is STAGE3-DONE ok, or STAGE3-FAILED <reason>.
set -uo pipefail
W=$HOME/lanes/forge-backlog
exec 8> "$HOME/lanes/.bench.lock"; flock 8
exec 9> "$HOME/lanes/.heavy.lock"; flock 9
echo "STAGE3 locks held $(date -u +%FT%TZ) load $(cat /proc/loadavg)"
if pgrep -x rustc >/dev/null || pgrep -x cargo >/dev/null || pgrep -x sparkles >/dev/null || pgrep -f 'qlever-server|fuseki-server|oxigraph serve|el_classify|nmo ' >/dev/null; then
  echo "busy:"; ps -eo pid,args --sort=-pcpu | head -8 | cut -c1-150
  echo "STAGE3-FAILED refused: something else is running"; exit 1
fi
for r in ffb18866 1cbc23cd eda22fce bccc5f7b cc94ac7f 71c277b0; do
  [ -f $W/bin/$r-matrix.zst ] || { echo "STAGE3-FAILED no binary $r"; exit 1; }
done
fail=""
run() { # name A B rounds reps
  echo "pair $1 $2 $3 $(date -u +%T) load $(cut -d' ' -f1-3 /proc/loadavg)"
  if bash $W/ab3.sh "$1" "$2-matrix" "$3-matrix" "$4" "$5"; then
    echo "$2 $3" > $W/runs/$1/revs.txt
    echo "pair $1 done $(date -u +%T) $(cat $W/runs/$1/results/done 2>/dev/null)"
  else echo "PAIR-FAILED $1"; fail="$fail $1"; fi
}
QS="subquery-agg types-grouped count-all distinct-obj minus" EXECUTION= run sqa-b35-auto ffb18866 1cbc23cd 24 200
QS="subquery-agg types-grouped" EXECUTION=streaming run sqa-b35-stream ffb18866 1cbc23cd 24 200
QS="types-grouped subquery-agg path-plus predicate-counts predicate-subjects" EXECUTION= run tg-b33-auto eda22fce bccc5f7b 24 200
QS="types-grouped" EXECUTION=eager run tg-b33-eager eda22fce bccc5f7b 24 200
QS="types-grouped" EXECUTION=streaming run tg-b33-stream eda22fce bccc5f7b 24 200
QS="order-by-full class-predicates optional-count optional-chain export-500k not-exists" EXECUTION=streaming RAYON_THREADS=12 SERVER_CPU=0-11 CLIENT_CPU=12 SPIN=0 WARMUP=3 run big-b32-12t-stream eda22fce bccc5f7b 12 20
QS="order-by-full" EXECUTION=eager RAYON_THREADS=12 SERVER_CPU=0-11 CLIENT_CPU=12 SPIN=0 WARMUP=3 run big-b32-12t-eager eda22fce bccc5f7b 12 20
QS="export-500k optional-chain" EXECUTION=streaming WARMUP=3 run big-b32-1t-stream eda22fce bccc5f7b 12 20
QS="order-by-full group-avg expr-order-key optional-chain" EXECUTION=streaming RAYON_THREADS=12 SERVER_CPU=0-11 CLIENT_CPU=12 SPIN=0 WARMUP=3 DATA=$W/data/people-1000000.nt.zst run big-b32-10m-12t-stream eda22fce bccc5f7b 8 15
QS="order-by-full" EXECUTION=eager RAYON_THREADS=12 SERVER_CPU=0-11 CLIENT_CPU=12 SPIN=0 WARMUP=3 DATA=$W/data/people-1000000.nt.zst run big-b32-10m-12t-eager eda22fce bccc5f7b 8 15
QS="lang-filter regex-iri optional-chain optional-count export-500k" EXECUTION=streaming WARMUP=3 run drift-cc94-1t-stream cc94ac7f 71c277b0 12 20
python3 $W/analyze.py $W/runs/{sqa-b35-auto,sqa-b35-stream,tg-b33-auto,tg-b33-eager,tg-b33-stream,big-b32-12t-stream,big-b32-12t-eager,big-b32-1t-stream,big-b32-10m-12t-stream,big-b32-10m-12t-eager,drift-cc94-1t-stream} > $W/runs/stage3-summary.json 2> $W/runs/stage3-summary.txt
cat $W/runs/stage3-summary.txt
if [ -z "$fail" ]; then echo "STAGE3-DONE ok"; else echo "STAGE3-FAILED$fail"; exit 1; fi
