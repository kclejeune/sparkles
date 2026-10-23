# Graphalytics reference adapter

`scripts/graphalytics-reference.py` prepares RDF and SPARQL queries for small
Graphalytics examples and compares saved Sparkles SELECT JSON with their reference
outputs. It supports BFS, WCC and PageRank. It never downloads data, starts an engine
or measures performance. Its unit tests validate the adapter alone. A separate script,
`scripts/graphalytics-native.sh`, runs a Sparkles binary on the prepared cases, and the
section on native validation below records its results.

This is part of [F14](../../docs/specs/F14-graph-algorithms.md#outcome) and
[X06 acceptance A13](../../docs/specs/X06-benchmark-coverage.md). It is not the official
Graphalytics driver, a certified result or a complete implementation of its six
algorithms and benchmark process.

## Input provenance

The unchanged files in `upstream/` come from the public
[Graphalytics repository](https://github.com/ldbc/ldbc_graphalytics/tree/7b8bde76cf7aab5e90b25ecd4b38829e2f98b292),
at commit `7b8bde76cf7aab5e90b25ecd4b38829e2f98b292`. The two example graphs have 10
vertices and 17 edges, and 9 vertices and 12 edges. Their configurations select two
PageRank iterations with damping 0.85, and BFS sources 1 and 2. The third edge column
is a weight for SSSP. These three algorithms are unweighted, so preparation checks that
column and omits it from RDF.

`provenance.json` records each original repository path and SHA256. The upstream
Apache-2.0 license is retained in [upstream/LICENSE](upstream/LICENSE). The vertex,
edge, configuration and reference files are copied without modification. New Python
code and adapter metadata are part of Sparkles. The independent algorithm definitions
and comparison rules come from the
[Graphalytics specification](https://ldbcouncil.org/ldbc_graphalytics_docs/graphalytics_spec.pdf),
sections 2.2–2.4, and the pinned upstream validator rules.

## Preparation and comparison

Run from the repository root. Preparation requires an output directory that does not
exist and never overwrites an input or existing directory.

```sh
python3 scripts/graphalytics-reference.py prepare example-directed /tmp/graphalytics-directed
```

The directory contains `graph.nt`, `bfs.rq`, `wcc.rq`, `pr.rq` and a versioned
`preparation.json` with input parameters and file hashes. Every vertex has a type
triple, and each query selects that class so isolated vertices remain in the projection.
Vertex IRIs retain the exact unsigned 64-bit identifier. Queries select the declared
direction, fixed PageRank iteration count and one algorithm thread. WCC uses its native
undirected semantics without a `graphalytics` parameter, which that service does not
accept.

To run one query by hand, the Sparkles CLI accepts this form:

```sh
sparkles query --data /tmp/graphalytics-directed/graph.nt --query /tmp/graphalytics-directed/bfs.rq --results json > /tmp/graphalytics-directed/bfs.json
python3 scripts/graphalytics-reference.py validate example-directed bfs /tmp/graphalytics-directed/bfs.json
```

Use fresh result files and require a successful engine exit before comparison. The
adapter's success receipt hashes the supplied JSON and manifest. It does not verify that
an engine produced the JSON and explicitly reports that limitation.
`scripts/graphalytics-native.sh [binary] [output-directory]` does the whole sequence for
both example graphs and all three algorithms. It prepares each case, runs the binary,
compares every vertex, keeps the stdout, stderr and receipts in the output directory and
exits nonzero on any failure.

Comparison requires one binding per reference vertex and rejects duplicates, missing
or extra vertices, unexpected IRIs, unbound values and incorrect RDF types. BFS compares
integer depths exactly and maps Sparkles' string `inf` to the reference sentinel
9223372036854775807. WCC compares component partitions with a two-way label mapping.
PageRank requires finite nonnegative doubles with relative error at most 0.0001 of
the reference value and no absolute-error allowance. A zero reference must match zero.
Failures exit nonzero without a success receipt.

## Supported input subset

The manifest and generated metadata carry a format kind and version. The manifest
reader refuses unknown versions and fields. Files are pinned by SHA256 and must stay
inside the manifest directory, including through symlinks. Parameters must agree with
the retained Graphalytics properties file. Inputs are ASCII EVLP with sorted unique
vertices and edges, no loops or parallel edges, no vertex properties, and either no
edge property or one finite nonnegative real `weight`. An undirected edge lists its
lower identifier first. Unsupported shapes and algorithms are refused.

This adapter materializes small reference data with explicit limits of 16 MiB per
input file, 100,000 vertices and 1,000,000 edges. Large S/M benchmark runs, Parquet,
other property schemas, arbitrary result variable names and damping 1 are outside
this subset. Native PageRank currently accepts damping below 1. Preparing an input
does not establish that it fits the native engine's separate resource limits.

## Native validation

On 2026-10-22 `scripts/graphalytics-native.sh` ran a debug build of Sparkles from commit
b7b6f760 on both example graphs. The binary's SHA256 was
5e37ddb235067143748f706709d4eee392aa0e9ffb24efcec61341adeaddd9ba. All six runs exited
successfully and matched the reference outputs for every vertex. That covers BFS, WCC
and PageRank on the directed example of 10 vertices and on the undirected example of 9
vertices. The directed BFS reference has four unreachable vertices, and Sparkles' `inf`
mapped to the reference sentinel for all four. The runs measured no time. They show that
Sparkles agrees with the official reference files on two tiny graphs. They are not the
official Graphalytics driver, not a certified result and not a run on its benchmark
datasets. CDLP, LCC and SSSP have no reference run in this harness.

## Tests

Run the deterministic adapter tests without an engine:

```sh
python3 -m unittest discover -s scripts/bench-tests -p test_graphalytics_reference.py -v
```
