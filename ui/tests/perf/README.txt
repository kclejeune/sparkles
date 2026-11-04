Scale measurements of the Workbench and Explore (ROADMAP item 725).

1. Serve the datasets (loads them once into ~/.cache/sparkles-ui-seed/perf-data):

     SPARKLES_BIN=/path/to/sparkles-release SPARKLES_UI_DIR=/path/to/ui/build \
       node scripts/perf-seed.mjs                 # prints "ready http://127.0.0.1:PORT"

   Datasets: chebi (ChEBI lite), taxon (synthetic, NCBITaxon-shaped, 2.6 M classes), hub
   (Explore: a 100-ary tree and a resource with 20,000 neighbours), unesco (thesaurus).
   SPARKLES_UI_DIR picks the UI build under test without rebuilding the server.

2. Run (one build, label = the name of its output directory):

     PERF_URL=http://127.0.0.1:PORT PERF_OUT=DIR PERF_LABEL=after PERF_COMMIT=abc123 \
       pnpm exec playwright test --config playwright.perf.config.ts [workbench|explore]

   PERF_REPEAT (default 3), PERF_ONLY=open-chebi,... (workbench cases),
   PERF_EXPLORE_TARGETS=1000,5000,20000, PERF_LOAD_TIMEOUT_MS, PERF_NOTE.

3. Tables of medians, builds side by side:

     node scripts/perf-summary.mjs DIR/baseline DIR/after

The pages' structure is known only to tests/perf/adapter.ts. The changed UI needs
  - rows as [role=tree] li[role=treeitem][data-id=IRI] with aria-rowcount on the tree,
  - window.__perfHook (nodeCount, drawnCount, nodeIds, expand, renderer) on Explore.
The baseline builds the hook from the Cytoscape instance.
