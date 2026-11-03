# Ontology entity search results

This report is a manual import of the retained run record. Read the
[methodology](README.md) with each result.

<a id="typeahead-latency"></a>

## Typeahead latency

The record [2026-11-03-chebi-go-atlas-d48c4d74](runs/2026-11-03-chebi-go-atlas-d48c4d74/record.json)
ran on atlas, an Intel Core i5-13500 with 20 logical CPUs and 15 GiB of memory, with
nothing else running. Each ontology had 1,560 queries, 60 for each prefix length and
typo count, each timed five times. Every search answered within 20 ms, and the 99th
percentile stayed under 11 ms in every bucket of ChEBI lite and under 8 ms in every
bucket of GO.

| Ontology | Typos | Searches | p50 | p95 | p99 | Max |
|---|---|---|---|---|---|---|
| ChEBI lite | 0 | 3,600 | 0.09 ms | 5.97 ms | 7.83 ms | 10.85 ms |
| ChEBI lite | 1 | 2,700 | 1.72 ms | 6.36 ms | 8.66 ms | 10.59 ms |
| ChEBI lite | 2 | 1,500 | 4.33 ms | 6.80 ms | 8.69 ms | 10.65 ms |
| ChEBI lite | all | 7,800 | 1.08 ms | 6.32 ms | 8.42 ms | 10.85 ms |
| GO | 0 | 3,600 | 0.09 ms | 2.94 ms | 3.66 ms | 4.57 ms |
| GO | 1 | 2,700 | 2.24 ms | 4.46 ms | 5.75 ms | 6.70 ms |
| GO | 2 | 1,500 | 3.06 ms | 4.42 ms | 5.87 ms | 16.99 ms |
| GO | all | 7,800 | 0.99 ms | 4.13 ms | 4.94 ms | 16.99 ms |

The slowest ChEBI lite queries are long prefixes of chemical names with digits and
punctuation, such as `3,7R,11R,15-` and `CID 9184681n`, whose words are short and match
many words within their edits. GO's maximum is one of five rounds of `B-jcellc o`, whose
other rounds took under 6.1 ms. Queries of one to three characters take no typo and
answered within 0.9 ms at the 99th percentile on both ontologies.

<a id="index-build-and-memory"></a>

## Index build and memory

| Ontology | Quads | Entities | Indexed values | Words | Build | Estimate | Resident growth |
|---|---|---|---|---|---|---|---|
| ChEBI lite | 1,994,673 | 238,190 | 731,179 | 324,140 | 1.33 s | 233 MiB | 292 MiB |
| GO | 1,445,043 | 52,057 | 326,642 | 83,432 | 0.64 s | 99 MiB | 116 MiB |

The first search of a branch builds the index and waits for it, so it took 1.3 s on
ChEBI lite. The estimate counts the index's own maps and strings, and the resident growth
also holds the allocator's slack. Each branch that is searched holds its own index, so a
dataset with several searched branches of the same ontology holds that much memory per
branch.

<a id="maintenance"></a>

## Maintenance across commits

Fifty commits on each ontology were applied to the index by the search after each one,
with no full build after the first. The search after a commit took 0.57 ms at the
median and 2.85 ms at most on ChEBI lite, and 0.19 ms and 1.03 ms on GO, including the
update. Commits do not wait for the index, which reads their changes only at the next
search.
