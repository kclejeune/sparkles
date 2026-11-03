# Ontology entity search results

This report is a manual import of the retained run record. Read the
[methodology](README.md) with each result.

<a id="typeahead-latency"></a>

## Typeahead latency

The record [2026-11-03-chebi-go-atlas-e3056dda](runs/2026-11-03-chebi-go-atlas-e3056dda/record.json)
ran on atlas, an Intel Core i5-13500 with 20 logical CPUs and 15 GiB of memory, with
nothing else running. Each ontology had 1,560 queries, 60 for each prefix length and
typo count, each timed five times. Every search answered within 20 ms, and the 99th
percentile stayed under 11 ms in every bucket of ChEBI lite and under 7 ms in every
bucket of GO.

| Ontology | Typos | Searches | p50 | p95 | p99 | Max |
|---|---|---|---|---|---|---|
| ChEBI lite | 0 | 3,600 | 0.09 ms | 5.77 ms | 7.59 ms | 10.88 ms |
| ChEBI lite | 1 | 2,700 | 1.71 ms | 6.16 ms | 8.51 ms | 11.06 ms |
| ChEBI lite | 2 | 1,500 | 4.31 ms | 6.68 ms | 8.92 ms | 18.26 ms |
| ChEBI lite | all | 7,800 | 1.07 ms | 6.18 ms | 8.34 ms | 18.26 ms |
| GO | 0 | 3,600 | 0.09 ms | 2.88 ms | 3.58 ms | 4.62 ms |
| GO | 1 | 2,700 | 2.32 ms | 4.47 ms | 5.75 ms | 6.77 ms |
| GO | 2 | 1,500 | 2.99 ms | 4.38 ms | 5.66 ms | 6.32 ms |
| GO | all | 7,800 | 0.99 ms | 4.07 ms | 4.87 ms | 6.77 ms |

The slowest ChEBI lite queries are long prefixes of chemical names with typos, digits
and punctuation, such as `1-pwxmitole` and `4,4,4-turif`, whose words are short and match
many words within their edits. ChEBI lite's maximum is one of five rounds of
`1-pwxmitole`, and the next slowest search took 11.9 ms. Queries of one to three
characters take no typo and answered within 1 ms at the 99th percentile on both
ontologies.

The earlier record
[2026-11-03-chebi-go-atlas-d48c4d74](runs/2026-11-03-chebi-go-atlas-d48c4d74/record.json),
taken before the review fixes that bound the ranker's candidates and check SKOS-XL forms'
graphs, gave 8.42 ms and 4.94 ms at the 99th percentile, so the fixes left the latency
unchanged within the run-to-run spread.

<a id="index-build-and-memory"></a>

## Index build and memory

| Ontology | Quads | Entities | Indexed values | Words | Build | Estimate | Resident growth |
|---|---|---|---|---|---|---|---|
| ChEBI lite | 1,994,673 | 238,190 | 731,179 | 324,140 | 1.32 s | 233 MiB | 292 MiB |
| GO | 1,445,043 | 52,057 | 326,642 | 83,432 | 0.64 s | 99 MiB | 116 MiB |

The first search of a branch builds the index and waits for it, so it took 1.3 s on
ChEBI lite. The estimate counts the index's own maps and strings, and the resident growth
also holds the allocator's slack. Each branch that is searched holds its own index, so a
dataset with several searched branches of the same ontology holds that much memory per
branch.

<a id="maintenance"></a>

## Maintenance across commits

Fifty commits on each ontology were applied to the index by the search after each one,
with no full build after the first. The search after a commit took 1.10 ms at the
median and 2.19 ms at most on ChEBI lite, and 0.25 ms and 1.39 ms on GO, including the
update. Commits do not wait for the index, which reads their changes only at the next
search.
