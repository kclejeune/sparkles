# OWL 2 EL classification results

This report is a manual import of the retained run records. Read the
[methodology](README.md) with each result.

## Agreement

<a id="agreement"></a>

The classifier's taxonomies equal ELK 0.6.0's on the Gene Ontology, ChEBI lite and all
298 ontologies of the ORE 2015 EL classification sample. `scripts/el-oracle.py`
compared each pair over the same classes and found no class whose node differs, no
difference in the unsatisfiable classes and no missing or extra direct edge. The ORE
ontologies hold 11,183,153 classes and 13,144,288 direct edges in all, and the largest
has 981,153 classes. ELK and the classifier both find `ore_ont_6720` inconsistent, and
both find the same 13, 1 and 18 unsatisfiable classes in `ore_ont_1325`, `ore_ont_2497`
and `ore_ont_7868`.

No difference needed an explanation, including on the ontologies that use axioms
outside the calculus. The classifier reported axioms used in part in 18 ORE ontologies,
and ELK reported possible incompleteness in 31, from object property ranges, nominals,
`ObjectHasValue`, property assertions and data property axioms. Thirteen ontologies are
in both groups. The taxonomies agree on these ontologies too, so the axioms that each
reasoner leaves out or uses in part do not separate the two on this sample. GO and ChEBI
lite each have one `InverseObjectProperties` axiom, which both reasoners leave out and
report. The classifier's taxonomy is the same on one and eight threads.

<a id="complete-reasoner"></a>

HermiT 1.4.5.519, a complete OWL 2 DL reasoner, checked the 36 ORE ontologies where
either EL reasoner reports axioms it uses in part or possible incompleteness. Its
taxonomy equals the classifier's and ELK's on all 36, so neither EL reasoner misses a
subsumption or an unsatisfiable class there. Four small test ontologies show the
entailments that both EL reasoners miss when a property's range meets an individual.
HermiT derives a subsumption through a range on an asserted property and one through a
range reached by `ObjectHasValue`, and it finds an individual in two disjoint ranges
inconsistent. ELK and the classifier miss all three, and the classifier reports the
axioms involved as used in part. A fourth ontology, where one range clashes with a class
assertion, is inconsistent for all three reasoners. The tests in
`crates/sparkles-owl/tests/classify.rs` hold these cases as known gaps of roadmap item
587.

The classifier's direct hierarchy of the Gene Ontology release of 2026-07-26 is also the
hierarchy that the release asserts, which GO's pipeline computes with ELK. Both sides
have 57,456 direct subsumptions between named classes, and none is missing or extra.
For the ChEBI lite release 256, every one of the 300,224 asserted subsumptions between
named classes is entailed, and every one of the classifier's 296,173 direct
subsumptions is asserted. ChEBI asserts some edges that other edges imply, so the
classifier's direct hierarchy is the transitive reduction of the asserted one.
`scripts/el-oracle.py --asserted` made both of these comparisons.

SNOMED CT is not available to the project and was not used.

## Against ELK

<a id="against-elk"></a>

ELK 0.6.0 and the classifier of commit aa0a41a6 ran on forge, the benchmark host, one
process at a time under the bench lock. Each process ran five classifications. Times are
the median of the five in ms. ELK's classification excludes the OWL API's load of the
file, which the load column gives, and its first run, which includes the compilation of
the JVM's hot code, is listed apart. The classifier's read is its single parse of the same
file in each process, so it is one measurement and not a median.
Peak memory is each process's peak resident set in MiB, which for the classifier
includes the axioms read and for ELK the OWL API ontology. ELK ran with a 4 GiB heap,
and its live heap is the heap in use after a full collection with the reasoner open.

| Ontology | Threads | Classifier | Read | Peak RSS | ELK | ELK first run | OWL API load | ELK with load | ELK peak RSS | ELK live heap |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| GO 2026-07-26 | 1 | 577 | 103 | 350 | 1,210 | 1,674 | 758 | 2,054 | 2,449 | 150 |
| | 8 | 354 | 105 | 363 | 553 | 1,061 | 741 | 1,281 | 2,429 | 150 |
| ChEBI lite 256 | 1 | 4,023 | 453 | 1,403 | 6,694 | 7,202 | 3,551 | 10,245 | 4,414 | 688 |
| | 8 | 2,967 | 445 | 1,643 | 2,548 | 2,969 | 3,549 | 6,138 | 4,369 | 688 |

The classifier classifies GO 2.1 times as fast as ELK on one thread and 1.6 times as fast
on eight, and ChEBI lite 1.7 times as fast on one thread. On eight threads ELK classifies
ChEBI lite in 2,548 ms against the classifier's 2,967, so ELK is 1.16 times as fast. The
classifier's saturation scales as ELK's does, but its indexing and its taxonomy, about
1.8 s of the 3 s, run on one thread, which roadmap item 592 follows. With the reading of
the file counted on both sides, the classifier takes 459 ms on GO and 3.4 s on ChEBI
lite on eight threads, against ELK's 1.3 s and 6.1 s.

ELK's resident memory follows its heap setting. With a 1 GiB heap for GO and 2 GiB for
ChEBI lite on eight threads, ELK classified in 539 ms and 3.0 s at peak resident sets of
1,271 MiB and 2,426 MiB. The classifier's peak is 363 MiB on GO and 1,643 MiB on ChEBI
lite, which includes its own copy of the axioms, as the section below describes. Both
reasoners give the same taxonomy on one and eight threads, and the ELK taxonomies of the
run agree with the classifier's.

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

The [ELK run record](runs/2026-11-02-elk-forge-aa0a41a6/record.json) holds the same for
the comparison with ELK on forge, with both tools' output lines, ELK's warnings and the
oracle's reports. The [ORE agreement record](runs/2026-11-02-ore-elk-agreement-aa0a41a6/record.json)
and the [HermiT agreement record](runs/2026-11-02-hermit-agreement-aa0a41a6/record.json)
hold the comparisons on the ORE 2015 sample and the range test ontologies. Their times
come from a loaded laptop and are not measurements.
