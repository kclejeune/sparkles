Scale measurement harness of lane explore-scale-725 (tracked scripts: branch
lane/explore-scale-725-measure of /home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725-measure, files ui/scripts/perf-*.{mjs,sh} and ui/tests/perf).

SERVER (one server holds the data directory at a time, so start either one; they differ only
in the UI they serve; the data is loaded once into ~/.cache/sparkles-ui-seed/perf-data):

  # baseline UI (4ab93ecb), port 18725
  cd /home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725-measure/ui && SPARKLES_BIN=$HOME/lanes-local/explore-scale-725/bin/sparkles \
    SPARKLES_UI_DIR=/home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725-measure/ui/build SPARKLES_SEED_PORT=18725 SPARKLES_PERF_MEM=11G \
    node scripts/perf-seed.mjs        # prints "ready http://127.0.0.1:18725"

  # changed UI (lane a199a8ae, built snapshot ui-after3), port 18726
  cd /home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725-measure/ui && SPARKLES_BIN=$HOME/lanes-local/explore-scale-725/bin/sparkles \
    SPARKLES_UI_DIR=$HOME/lanes-local/explore-scale-725/ui-after3/ui/build \
    SPARKLES_SEED_PORT=18726 SPARKLES_PERF_MEM=11G node scripts/perf-seed.mjs

  Datasets: chebi, taxon (synthetic 2.6 M classes), taxon1m (synthetic 1 M), hub (Explore),
  unesco. SPARKLES_PERF_DATASETS=hub,chebi limits them. To snapshot a new UI build of the lane:
    git -C <lane> archive <commit> ui | tar x -C DIR; ln -s /home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725-measure/ui/node_modules DIR/ui/node_modules;
    (cd DIR/ui && pnpm build)

ONE RUN (UI under test = whatever the server serves):

  cd /home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725-measure/ui && PERF_URL=http://127.0.0.1:18725 PERF_OUT=/home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725/.artifacts/lanes/explore-scale-725/measure PERF_LABEL=after3 \
    PERF_REPEAT=3 pnpm exec playwright test --config playwright.perf.config.ts workbench.perf
  (PERF_ONLY=open-chebi,... PERF_EXPLORE_TARGETS=1000,5000 PERF_REP_BASE=N)

INTERLEAVED, BASELINE AND CHANGED ALTERNATING, A FRESH SERVER PER GROUP (what produced the tables):

  sh /home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725/.artifacts/lanes/explore-scale-725/measure/run-interleaved.sh      # edit PERF_UIS for the builds and groups; takes about 25 min; run-second.sh is the Explore and taxon1m pass

TABLES:  node /home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725-measure/ui/scripts/perf-summary.mjs /home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725/.artifacts/lanes/explore-scale-725/measure/baseline /home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725/.artifacts/lanes/explore-scale-725/measure/after3 > /home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725/.artifacts/lanes/explore-scale-725/measure/summary.md

The page structure lives in ui/tests/perf/adapter.ts only. The changed Explore must set
window.__perfHook (nodeCount, drawnCount, nodeIds, expand, renderer, viewport) as that file says.
Host conditions are recorded per run (loadavg in each JSON). Software WebGL (SwiftShader).
