#!/usr/bin/env bash
# Interleaved A/B of Dataset.load from a file object: main vs lane, release wheels.
# Cases: "<file> <format> <mode>"; mode "path" passes the path instead of a file object.
set -u
cd ~/lanes/python-reader-438
D=data; R=results.jsonl; rounds=${ROUNDS:-5}
for r in $(seq 1 $rounds); do
  for c in "data.nt nt auto" "data.ttl ttl auto" "one.nt nt auto"; do
    set -- $c
    for side in main lane; do
      rm -rf db; out=$(timeout 2400 venv-$side/bin/python one.py $D/$1 $2 $3 db 2>&1 | tail -1)
      echo "{\"round\": $r, \"side\": \"$side\", \"file\": \"$1\", \"run\": $out}" | tee -a $R
    done
  done
done
rm -rf db
echo AB-DONE
