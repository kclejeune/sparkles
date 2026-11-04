#!/bin/sh
# the interleaved baseline and after3 run on the laptop; see README.txt
cd /home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725-measure/ui
export SPARKLES_BIN=$HOME/lanes-local/explore-scale-725/bin/sparkles PERF_OUT=/home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725/.artifacts/lanes/explore-scale-725/measure PERF_REPEAT=3
export PERF_COMMIT=4ab93ecb/a199a8ae PERF_SERVER=release-4ab93ecb PERF_NOTE="laptop, synthetic taxon, software WebGL" PERF_SERVER_MEM=11G PERF_BROWSER_MEM=4G PERF_TAXON_TIMEOUT_MS=180000
export PERF_UIS="baseline=/home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725-measure/ui/build:chebi,explore,taxon300k,scroll-taxon300k after3=$HOME/lanes-local/explore-scale-725/ui-after3/ui/build:chebi,explore,taxon300k,scroll-taxon300k,taxon1m,scroll-taxon1m"
exec sh scripts/perf-run.sh
