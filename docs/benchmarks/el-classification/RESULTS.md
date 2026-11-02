# OWL 2 EL classification results

This report is a manual import of the retained run records. Read the
[methodology](README.md) with each result.

## Agreement

<a id="agreement"></a>

The classifier's direct hierarchy of the Gene Ontology release of 2026-07-26 is the
hierarchy that the release asserts, which GO's pipeline computes with ELK. Both sides
have 57,456 direct subsumptions between named classes, and none is missing or extra.
For the ChEBI lite release 256, every one of the 300,224 asserted subsumptions between
named classes is entailed, and every one of the classifier's 296,173 direct
subsumptions is asserted. ChEBI asserts some edges that other edges imply, so the
classifier's direct hierarchy is the transitive reduction of the asserted one. Neither
ontology has an unsatisfiable class. Each has one axiom outside OWL 2 EL, an
`InverseObjectProperties` axiom, which the classifier leaves out and reports, as ELK
also ignores inverse properties. The taxonomy is the same on one, four and eight
threads. `scripts/el-oracle.py --asserted` made both comparisons.

All 298 ontologies of the ORE 2015 EL classification sample classify, with taxonomies
that do not change between the builds of the record below and the build with the
review fixes. One of them is inconsistent, four have unsatisfiable classes and 18 have
axioms the classifier uses only in part. Each classifies within 21 s on four threads of
a loaded laptop, the largest having 981,151 classes, which is a completion check and
not a timing.

The classifier has not been compared with ELK itself on these ontologies, and its time
and memory have not been measured against ELK's. Roadmap item 591 covers both.
SNOMED CT is not available to the project and was not used.

## Classification time and memory

The run on forge, the benchmark host, took the bench lock and classified each ontology
five times in one process for each thread count. Times are the median of the five
classifications in ms, with the stages of the last one. Peak memory is the process's
peak resident set in MiB and includes the axioms read, which the next column gives.
The build is commit 8e73284a.

| Ontology | Threads | Classify | Index | Saturation | Taxonomy | Peak RSS | After read |
|---|---:|---:|---:|---:|---:|---:|---:|
| GO 2026-07-26, 51,986 classes, 135,031 axioms | 1 | 583 | 159 | 280 | 90 | 428 | 77 |
| | 4 | 410 | 166 | 94 | 90 | 438 | 77 |
| | 8 | 377 | 160 | 68 | 94 | 436 | 77 |
| ChEBI lite 256, 238,150 classes, 647,721 axioms | 1 | 3,912 | 913 | 1,742 | 916 | 1,561 | 269 |
| | 4 | 2,966 | 924 | 809 | 827 | 1,660 | 269 |
| | 8 | 2,840 | 858 | 739 | 868 | 1,803 | 269 |

Saturation scales with the threads. Indexing and the taxonomy run on one thread and
take most of the time on four threads or more, which roadmap item 592 follows.
The classifier holds a copy of the axioms of its own, so its share of the peak is about
350 MiB on GO and 1.3 to 1.5 GiB on ChEBI. That copy is what lets a kept classifier
reclassify incrementally after a change, which trades memory for fast
reclassification. The memory estimate that this build reported was 52 MiB on GO and
357 MiB on ChEBI. Commit 552fdbdd counts the copy of the axioms too, so the estimate
that the budget and the dataset's kept classification use is closer to the resident
memory, and later builds drop a transient copy of the changed axioms on a full run.
Reading the functional-style files took 100 to 111 ms for GO and 429 to 489 ms for
ChEBI, outside the classification times.

### Saturation scheduling

Before commit 8e73284a every activation of a context took the shared queue's lock and
woke a waiting worker. The same ontologies on forge with the previous build, 6aff910a,
gave these medians in ms.

| Ontology | Threads | 6aff910a | 8e73284a |
|---|---:|---:|---:|
| GO | 1 | 604 | 583 |
| | 4 | 504 | 410 |
| | 8 | 466 | 377 |
| ChEBI lite | 1 | 4,234 | 3,912 |
| | 4 | 4,157 | 2,966 |
| | 8 | 6,005 | 2,840 |

ChEBI saturated in 3,871 ms on eight threads with the previous build and in 739 ms with
the new one. An A/B on atlas measured 6.2 million voluntary context switches and 12.8 s
of system time on eight threads before the change and 0.7 million and 3.2 s after it.
The previous build's harness kept two classifiers alive at once, so its peak memory is
not comparable and is left out.

## Records

The [run record](runs/2026-11-02-go-chebi-forge-8e73284a/record.json) holds the host,
the binary and data hashes, the settings, every classification time, the taxonomy
hashes and the agreement checks. Its directory keeps the raw metrics, the environment
and the script that ran.
