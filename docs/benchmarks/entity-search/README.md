# Ontology entity search

These measurements cover the entity search of `GET /$/ontology/{ds}/search` on two large
biomedical ontologies, ChEBI lite and the Gene Ontology. They give the latency of
typeahead queries with and without typos, the time and memory of building the index, and
the cost of keeping it current across commits. The search and its limits are described
in the [search guide](../../search/README.md#entity-search) and in
[U12](../../specs/U12-ontology-workbench/SPEC.md#ontology-browser-search).

Read the [results](RESULTS.md), a
[manual report](../README.md#evidence-and-generated-reports) of the retained run record,
and the [run catalog](RUNS.json).

## What is measured

The harness is the example `crates/sparkles/examples/entity_search_bench.rs`, built with
`mbx build --release -p sparkles --features ontology --example entity_search_bench`. It
loads an RDF/XML ontology into a persistent dataset in a temporary directory, runs one
search to build the index, and reports the build time, the index's own estimate of its
memory and the change in the process's resident memory across the build.

The queries are drawn with a seeded generator from the labels and synonyms of the
ontology. Each is a prefix of 1 to 12 characters of a text, with no typo, one typo from 4
characters or two typos from 8, where a typo is a substitution, a deletion, an insertion
or a transposition at a random position. These are the edits that the search tolerates
for a word still being typed. `--per-bucket` queries are drawn for each length and typo
count. Each query runs through `wire::call(ds, "search", …)`, the call that the HTTP
route and the bindings make, so the time includes building the JSON answer and excludes
only HTTP itself. After one warm-up pass, every query runs `--rounds` times, and the
report gives the percentiles of all timed searches, by typo count and by bucket.

The report also gives how often the entity a query was drawn from is among the first ten
hits. A short prefix is shared by many labels, so this share is low for one to three
characters and grows with the length. It is a check that typo queries still find their
source, not a measure of ranking quality, since an exact label or a shorter label that
the prefix also matches rightly ranks above the source.

Last, `--commits` small commits each add a labelled class or remove a label, and the
search after each commit is timed. That search applies the commit's changes to the
index, so the report counts the updates and full builds and gives the commit's and the
search's latency.

## Inputs

The inputs are the RDF/XML files of ChEBI lite release 256 and GO release 2026-07-26, the
same files as the [OWL 2 EL classification](../el-classification/README.md) benchmark.
The record gives each file's URL and SHA-256.

## Running it

```sh
mbx build --release -p sparkles --features ontology --example entity_search_bench
target/release/examples/entity_search_bench chebi_lite.owl \
    --per-bucket 60 --rounds 5 --seed 1 --commits 50 > chebi_lite.json
```

`--query TEXT` runs one query and prints its hits and time, which helps to explain a slow
query. The retained run used the script kept beside its record as `run.sh.txt`, which
waits for the host's heavy-work lock so that no build, test or other benchmark runs at
the same time.
