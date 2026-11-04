#!/bin/sh
# second pass: Explore on the 90-ary hub for both builds, then the 1M taxon of after3 with more memory
cd /home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725-measure/ui
export SPARKLES_BIN=$HOME/lanes-local/explore-scale-725/bin/sparkles PERF_OUT=/home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725/.artifacts/lanes/explore-scale-725/measure PERF_REPEAT=3
export PERF_COMMIT=4ab93ecb/a199a8ae PERF_SERVER=release-4ab93ecb PERF_NOTE="laptop, synthetic taxon, software WebGL" PERF_BROWSER_MEM=4G PERF_TAXON_TIMEOUT_MS=180000
U=$HOME/lanes-local/explore-scale-725/ui-after3/ui/build
PERF_SERVER_MEM=11G PERF_UIS="baseline=/home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725-measure/ui/build:explore after3=$U:explore" sh scripts/perf-run.sh
PERF_SERVER_MEM=13G PERF_UIS="after3=$U:taxon1m,scroll-taxon1m" sh scripts/perf-run.sh
echo SECOND_DONE
