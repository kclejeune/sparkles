#!/usr/bin/env bash
# Interleaved A/B of Dataset.load from a file object: main vs lane, release wheels.
set -u
cd ~/lanes/python-reader-438
D=data; R=results.jsonl; rounds=${ROUNDS:-3}
for r in $(seq 1 $rounds); do
  for c in "nt auto" "nt buffered" "ttl auto" "rt auto" "nt path"; do
    set -- $c
    for side in main lane; do
      [ "$2" = path ] && [ "$side" = main ] && continue
      rm -rf db; out=$(timeout 2400 venv-$side/bin/python one.py $D/data.$1 $1 $2 db 2>&1 | tail -1)
      echo "{\"round\": $r, \"side\": \"$side\", \"run\": $out}" | tee -a $R
    done
  done
done
rm -rf db
echo AB-DONE
