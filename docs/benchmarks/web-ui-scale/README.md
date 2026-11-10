# Web UI at scale

These measurements cover the Workbench ontology and thesaurus trees and the Explore graph on
large inputs, in a browser against a release server. They give the time from opening a page
to its first usable row, the requests and bytes of the open, the frame times of scrolling a
tree through 10,000 rows, the browser's JavaScript heap, and the time, frames and memory of
Explore as the explored graph grows to 1,000, 5,000 and 20,000 nodes. The pages and their
limits are described in the [visualization guide](../../visualization/README.md#explore).

Read the [results](RESULTS.md), a
[manual report](../README.md#evidence-and-generated-reports) of the retained run record,
and the [run catalog](RUNS.json).

## What is measured

The harness is in `ui/tests/perf` and `ui/scripts/perf-*`. A seed script loads the datasets
once into a persistent data directory, starts the release server on them and serves the UI
build under test, so that two builds can be compared against the same server and data. A
Playwright runner then drives headless Chromium through each case and writes one JSON record
for each case and repetition.

The Workbench cases open the ontology editor on a dataset and wait for the first row of the
class tree. They record the first visible row, the end of the last main-thread task longer
than 50 ms, the time the network goes quiet, every API request with its route, bytes on the
wire and decoded bytes, the long tasks, the JavaScript heap after a garbage collection and
the number of rows and DOM nodes. The reveal cases open a link to a class deep in the tree
and to a class past the first page of a wide level. The scroll case reads a family of
children to 10,038 rows by its **Show more** rows and scrolls down and back with the wheel
for the frame times, taken from the browser's frame timestamps.

The Explore cases open a resource of a 90-ary tree and expand the nodes that the page
draws, one at a time, until the graph holds the target number of nodes or no expandable node
is left. They record the first graph, the latency of each expansion, the nodes held and
drawn, the long tasks and the heap. They then pan and zoom for five seconds and record the
frame times. A separate case opens a resource with 20,000 neighbours for the group node.

The builds take turns, three repetitions each, and each group of cases starts a fresh
server. The one-minute load average at the start of each run is part of its record.

## Inputs

ChEBI lite release 256 is the real file, as in the
[ontology entity search](../entity-search/README.md) benchmark. The NCBI Taxonomy release is
on no benchmark host, so `ui/scripts/perf-gen-taxon.mjs` writes a deterministic taxonomy of
the same shape, with OBO IRIs, a depth of 40, families of 10,000 to 100,000 children, a
scientific-name label on every class, a synonym on one in seven and a rank on every class.
It is written at 300,000, 1,000,000 and 2,600,000 classes, and 2.6 million is about the size
of the real taxonomy. `ui/scripts/perf-gen-hub.mjs` writes the Explore data. The UNESCO
Thesaurus of the design seed gives the thesaurus case. The record gives the checksum of each
file.

## Limits of these runs

WebGL runs in software through SwiftShader in headless Chromium, so frame times of the WebGL
view are CPU times and are far slower than on a graphics card. The Cytoscape canvas of the
older build is also drawn on the CPU, so the two compare only as measured here. The pan and
zoom is a synthetic drag and wheel. The host was a laptop that ran other work, which put the
load average between 2 and 12 in the runs, so timings carry noise of that order.
The taxonomy is synthetic and the largest size was not opened in the changed build, because
the server's ontology summary alone holds 4 GB at 1 million classes.

## Running it

```sh
cd ui
SPARKLES_BIN=/path/to/sparkles-release SPARKLES_UI_DIR=/path/to/ui/build \
  node scripts/perf-seed.mjs
PERF_URL=http://127.0.0.1:PORT PERF_OUT=DIR PERF_LABEL=after \
  pnpm exec playwright test --config playwright.perf.config.ts
node scripts/perf-summary.mjs DIR/baseline DIR/after
```

`ui/tests/perf/README.txt` lists the variables that pick cases, repetitions and targets, and
`scripts/perf-run.sh` runs the groups of two builds in turn on fresh servers. Run it on a
quiet host with no other build or test running.

## Individuals paging

`ui/scripts/perf-individuals.mjs` compares two production UI builds against one server
and a synthetic ontology with 50,250 named members of one type, a second asserted type,
three untyped members and an anonymous assertion subject. It checks group counts,
duplicate-free continuation, direct lookup of the final member, incoming-only entity
selection and the number of rows drawn by the actual Individuals tab.

The first root request records cold type-bucket discovery separately from the warm typed
member and continuation requests. Browser trials then use fresh contexts against a
shared warm terminology mapping. Both builds take turns, and the record distinguishes
the first usable type buckets from the first displayed individual. It records API
request bodies, request and response sizes, decoded bytes and exact source identities.
Transfer accounting covers finite ontology and query reads and excludes the long-lived
change feed. Failed finite reads remain in the raw record, with unavailable sizes marked
as null. Partial trials retain their errors and completed milestones. The harness saves
incomplete trials before rejecting the run and reports full correctness only for complete
runs.
The earlier UI's capped listing is the baseline. A member page bounds transfer and the
browser's loaded rows, while bucket discovery and Untyped reads still scan their scope,
and typed member pages scan that type's membership. The existing terminology mapping
cost applies to this route too.

Run the server alone on its host and provide its receipt with the host, compiler, source
commit and binary checksum. Use a benchmark-host run for published timings. The harness
refuses to overwrite a raw record.

```sh
cd ui
INDIVIDUALS_BACKEND=http://127.0.0.1:45200 \
INDIVIDUALS_BEFORE_UI=/path/to/before/ui/build \
INDIVIDUALS_AFTER_UI=/path/to/after/ui/build \
INDIVIDUALS_BEFORE_SOURCE=BASE_COMMIT \
INDIVIDUALS_AFTER_SOURCE=TIP_COMMIT \
INDIVIDUALS_SERVER_RECEIPT=/path/to/server-receipt.json \
INDIVIDUALS_OUT=/path/to/new-run.json node scripts/perf-individuals.mjs
```

The explicit Rust diagnostic counts index rows visited by the individuals module on the
same fixture shape. It prints root discovery, first and subsequent typed pages, direct
lookup and Untyped separately. The counters exclude terminology mapping, bounded name
reads and existence probes through the shared quad helper, so they describe part of the
server work. They are compiled only into tests and do not add work to the measured
server binary.

```sh
mbx test -p sparkles --features ontology --lib measure_individual_scan_work -- --ignored --nocapture
```
