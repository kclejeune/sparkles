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

<a id="whelk-and-konclude"></a>

Konclude 0.7.0-1138, a complete OWL 2 DL reasoner, gives the classifier's taxonomy on all
298 ORE ontologies and on ChEBI lite, and finds `ore_ont_6720` inconsistent as the
classifier does. On the Gene Ontology its taxonomy has 7 direct edges that the
classifier's and ELK's do not, such as filopodium membrane `GO_0031527` under organelle
membrane `GO_0031090`, and it entails 14 subsumptions between named classes that they do
not. All 14 follow from `InverseObjectProperties(part_of has_part)`, the one GO
axiom outside OWL 2 EL, which both EL reasoners leave out and report. Part of is
transitive, and a membrane-bounded organelle is defined as an organelle that has a
membrane as a part. A filopodium membrane is part of a filopodium, which is part of the
actin cytoskeleton, an organelle. The inverse then makes that cytoskeleton a
membrane-bounded organelle and the membrane an organelle membrane. These subsumptions
are entailed, so they are evidence for the inverse properties that roadmap items 587 and
603 cover. Konclude and Whelk 1.2.1 both agree with HermiT on the four range test
ontologies, including the two subsumptions and the inconsistency that ELK and the
classifier miss.

Whelk, a second OWL 2 EL reasoner, gives the classifier's taxonomy on GO, ChEBI lite and
296 of the ORE ontologies. On `ore_ont_412`, a BioTop ontology with 18 two-property chains
and no axiom outside OWL 2 EL, Whelk entails 20,401 subsumptions between named classes
where the classifier, ELK and Konclude entail 20,802, and none that they do not, so Whelk
misses 401 there. Whelk
exceeded the 12 GiB memory cap of the laptop run on `ore_ont_3524`. The
[agreement record](runs/2026-11-02-reasoners-agreement-91a7b160/record.json) keeps every
comparison, the trace of the GO subsumptions and the script that counts entailed
subsumptions.

## ROBOT workflows

<a id="robot"></a>

OBO release pipelines classify with ROBOT's `reason`, `reduce`, `relax` and `materialize`
commands. ROBOT 1.9.11 ran each with its bundled ELK 0.6.0 on GO and ChEBI lite, from the
release's RDF/XML file to a written functional-style file. In every output, each asserted
subsumption between named classes is entailed by the classifier's taxonomy, and each
direct edge of the taxonomy is asserted. `reason` asserts every direct subsumption of the
classifier's hierarchy in the ontology. `reduce` leaves exactly the 57,456 direct subsumptions of GO
and the 296,173 of ChEBI lite between named classes, which `scripts/el-oracle.py
--asserted` compared.

`sparkles ontology classify FILE --format ofn` writes the taxonomy without the rest of
the ontology, and its output equals `el_classify`'s on both files. Sparkles has no
counterpart of the other commands. Roadmap items NEW-ITEM-1 to NEW-ITEM-3 add
`sparkles ontology reason`, `reduce`, `relax` and `materialize` with ROBOT's options,
item NEW-ITEM-4 chains them in one process, and this comparison checks their output.

On forge, the benchmark host, `robot reason` with ELK took a median of 14.0 s on GO and
30.6 s on ChEBI lite over three runs, from the release's RDF/XML file to the written
functional-style file, at peak resident sets of 3,597 MiB and 8,287 MiB with an 8 GiB
heap. `sparkles ontology classify` took 4.3 s and 9.8 s on the same files, at 1,850 MiB
and 3,587 MiB, so it is 3.3 times as fast on GO and 3.1 times on ChEBI lite. It writes
the taxonomy only, while `robot reason` writes the whole ontology with the inferred
axioms, so the comparison counts the larger output against ROBOT.

The other commands ran once each on a loaded laptop, so their times only show the scale
and are not measurements. Each is one end-to-end run in seconds, with the peak resident
memory in MiB.

| Run | GO time | GO peak RSS | ChEBI lite time | ChEBI lite peak RSS |
|---|---:|---:|---:|---:|
| `robot reason` | 21.2 | 3,743 | 40.3 | 7,361 |
| `robot reason` from the functional-style file | 7.1 | 1,255 | 21.1 | 4,549 |
| `robot reduce` | 12.5 | 2,080 | 44.0 | 7,570 |
| `robot relax` | 9.0 | 1,491 | 14.8 | 2,279 |
| `robot materialize` | 14.4 | 2,106 | 37.7 | 7,509 |
| `robot reason relax reduce` | 23.2 | 7,088 | 68.4 | 8,612 |
| `sparkles ontology classify` | 8.0 | 1,684 | 16.4 | 3,499 |
| `sparkles ontology classify` from the functional-style file | 2.0 | 694 | 11.3 | 2,402 |

ROBOT ran with an 8 GiB heap. The materialized property was part of, `BFO:0000050`, for
GO and has role, `RO:0000087`, for ChEBI lite.

## The reference reasoners on the benchmark host

<a id="reference-reasoners"></a>

The classifier, ELK 0.6.0, Konclude 0.7.0-1138, Whelk 1.2.1 and HermiT 1.4.5.519 ran on
forge, one process at a time under the bench lock. Times are the median classification
in seconds, of five runs in one process on GO and ChEBI lite and three on the ORE
ontologies, and HermiT ran three times on GO and ChEBI lite and once on each ORE
ontology. The times exclude reading the file. Konclude starts one process per run, and
its time is the total processing time of its log less its parse, so it includes writing
the class hierarchy. Whelk and HermiT classify on one thread. Every taxonomy equals the
classifier's, except that Konclude and HermiT both give the 7 GO edges that come from
GO's inverse property, as [above](#whelk-and-konclude).

| Ontology | Threads | Classifier | ELK | Konclude | Whelk | HermiT |
|---|---:|---:|---:|---:|---:|---:|
| GO | 1 | 0.58 | 1.28 | 1.32 | 3.37 | 28.97 |
|  | 8 | 0.36 | 0.57 | 1.34 |  |  |
| ChEBI lite | 1 | 4.00 | 6.64 | 4.25 | 16.47 | 111.55 |
|  | 8 | 2.84 | 2.54 | 4.21 |  |  |

The ORE columns give the thread count after each tool's name.

| Ontology | Classes | Classifier 1 | Classifier 8 | ELK 1 | ELK 8 | Konclude 1 | Konclude 8 | Whelk | HermiT |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `ore_ont_868` | 981,151 | 8.26 | 7.43 | 12.05 | 5.94 | 12.25 | 12.25 | 28.46 | 39.31 |
| `ore_ont_10689` | 981,148 | 8.07 | 7.41 | 11.40 | 5.19 | 12.22 | 12.24 | 26.97 | 38.85 |
| `ore_ont_9674` | 981,148 | 8.25 | 7.98 | 11.49 | 5.59 | 12.23 | 11.74 | 26.26 | 40.22 |
| `ore_ont_8486` | 903,617 | 6.48 | 6.07 | 10.78 | 4.76 | 10.97 | 11.17 | 24.27 | 37.03 |
| `ore_ont_14459` | 847,760 | 6.21 | 5.82 | 10.16 | 4.64 | 10.04 | 10.33 | 23.26 | 33.77 |
| `ore_ont_16008` | 733,100 | 5.84 | 5.44 | 8.73 | 3.78 | 8.92 | 8.71 | 19.89 | 29.75 |
| `ore_ont_14042` | 517,023 | 6.66 | 5.25 | 7.11 | 7.80 | 7.03 | 7.12 | 14.63 | 513.47 |
| `ore_ont_11395` | 517,023 | 6.45 | 4.89 | 7.09 | 8.30 | 7.08 | 7.12 | 15.14 | 525.19 |

Konclude's time is the target that roadmap item 603 sets. On these OWL 2 EL inputs it
classifies GO in 1.3 s, ChEBI lite in 4.2 s and the largest ORE ontology in 12.2 s, on
one thread or eight, and its whole process takes 1.9 s, 6.8 s and 19.4 s with the parse.
The classifier is ahead of Konclude on every ontology and thread count. ELK on eight
threads is ahead of the classifier on ChEBI lite and the six largest ORE ontologies,
which roadmap item 602 follows. On `ore_ont_14042` and `ore_ont_11395` ELK is slower on
eight threads than on one. HermiT takes 29 s on GO, 112 s on ChEBI lite and up to 525 s on
the ORE ontologies.

Peak memory is each process's peak resident set in MiB, on eight threads where the tool
uses them. The JVMs ran with fixed heaps of 4 GiB for ELK on GO and ChEBI lite, 8 GiB on
ORE, and 8 to 10 GiB for Whelk and HermiT, so their resident sets follow the heap more
than their live data.

| Ontology | Classifier | ELK | Konclude | Whelk | HermiT |
|---|---:|---:|---:|---:|---:|
| GO | 361 | 2,388 | 713 | 5,167 | 6,011 |
| ChEBI lite | 1,648 | 4,519 | 2,230 | 8,372 | 7,351 |

The ORE ontologies' peak memory follows.

| Ontology | Classifier | ELK | Konclude | Whelk | HermiT |
|---|---:|---:|---:|---:|---:|
| `ore_ont_868` | 4,305 | 8,778 | 5,933 | 11,015 | 10,146 |
| `ore_ont_10689` | 4,284 | 8,803 | 5,806 | 10,952 | 10,141 |
| `ore_ont_9674` | 4,289 | 8,859 | 5,810 | 11,011 | 10,638 |
| `ore_ont_8486` | 2,869 | 8,771 | 5,358 | 10,938 | 10,468 |
| `ore_ont_14459` | 2,801 | 8,618 | 5,061 | 10,975 | 9,648 |
| `ore_ont_16008` | 2,616 | 7,746 | 4,415 | 10,770 | 9,895 |
| `ore_ont_14042` | 2,750 | 6,804 | 4,216 | 9,873 | 8,271 |
| `ore_ont_11395` | 2,617 | 6,435 | 4,278 | 9,918 | 8,242 |

The [forge run record](runs/2026-11-03-reasoners-forge-ea96e8f5/record.json) holds every
output line, the oracle's reports, the taxonomy hashes and the script that ran.

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

The [reference reasoner agreement record](runs/2026-11-02-reasoners-agreement-91a7b160/record.json)
holds Whelk's and Konclude's output lines and comparisons on the range test ontologies,
GO, ChEBI lite and the ORE 2015 sample, the ROBOT runs and their checks, and the trace of
Konclude's extra GO subsumptions. Its times also come from a loaded laptop.
