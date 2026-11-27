#!/usr/bin/env bash
# waits for both binaries without the lock, then measures under it
d=$HOME/lanes/onto-summary-735
until [ -x $d/sparkles-0779d666 ] && [ -x $d/sparkles-bd3bcea4 ]; do sleep 30; done
exec flock $d/../.heavy.lock bash $d/measure.sh $d/sparkles-0779d666 $d/sparkles-bd3bcea4 $d/out-bd3bcea4 3
