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
report. The classifier's taxonomy is the same on one and eight threads. The build of commit
80644aec gives taxonomies byte for byte equal to those of commit aa0a41a6 on GO, ChEBI
lite and the 298 ORE ontologies on four threads, and the same ones on one thread.

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

ELK 0.6.0 and the classifier of commit 80644aec ran on forge, the benchmark host, which
the script held under the bench lock for the whole run, so nothing else ran beside it.
Each process classified GO and ChEBI lite five times and each ORE ontology three times.
Times are the median classification time in ms. The classifier's time starts with the
axioms read and ends with the taxonomy built. ELK's starts at the creation of the
reasoner over the OWL API ontology and ends with its class hierarchy, so it leaves out
the OWL API's load of the file, as the classifier's leaves out its parse. Peak memory is
each process's peak resident set in MiB, which for the classifier includes the axioms
read and for ELK the OWL API ontology. ELK ran with a fixed heap of 4 GiB, or of 6 to
10 GiB for the ORE ontologies that need more. The ORE ontologies are the largest of the
ORE 2015 EL sample by classification time, and `ore_ont_868`, `ore_ont_9674` and
`ore_ont_10689` are versions of one ontology.

| Ontology | Classes | Threads | Classifier | Peak RSS | ELK | ELK peak RSS | ELK time over the classifier's |
|---|---:|---:|---:|---:|---:|---:|---:|
| GO 2026-07-26 | 51,986 | 1 | 280 | 211 | 1,216 | 2,393 | 4.3 |
| | | 8 | 83 | 252 | 608 | 2,444 | 7.3 |
| ChEBI lite 256 | 238,150 | 1 | 2,112 | 670 | 6,694 | 4,406 | 3.2 |
| | | 8 | 843 | 930 | 2,540 | 4,517 | 3.0 |
| `ore_ont_13482` | 58,376 | 1 | 1,100 | 1,346 | 2,746 | 4,294 | 2.5 |
| | | 8 | 516 | 1,538 | 2,616 | 4,265 | 5.1 |
| `ore_ont_868` | 981,151 | 1 | 1,866 | 1,735 | 11,743 | 4,552 | 6.3 |
| | | 8 | 1,004 | 1,729 | 5,505 | 4,551 | 5.5 |
| `ore_ont_9674` | 981,148 | 1 | 1,867 | 1,735 | 11,782 | 4,562 | 6.3 |
| | | 8 | 987 | 1,728 | 5,404 | 4,584 | 5.5 |
| `ore_ont_10689` | 981,148 | 1 | 1,858 | 1,735 | 11,798 | 4,596 | 6.4 |
| | | 8 | 999 | 1,750 | 5,449 | 4,542 | 5.5 |
| `ore_ont_1559` | 264,022 | 1 | 1,214 | 1,775 | 8,870 | 7,694 | 7.3 |
| | | 8 | 764 | 1,803 | 7,317 | 7,729 | 9.6 |
| `ore_ont_15059` | 142,884 | 1 | 1,299 | 2,176 | 8,982 | 7,574 | 6.9 |
| | | 8 | 873 | 2,207 | 8,811 | 9,017 | 10.1 |
| `ore_ont_3524` | 123,311 | 1 | 634 | 1,419 | 7,258 | 5,689 | 11.5 |
| | | 8 | 375 | 1,492 | 4,975 | 5,925 | 13.3 |
| `ore_ont_3836` | 58,887 | 1 | 596 | 1,187 | 4,744 | 4,154 | 8.0 |
| | | 8 | 362 | 1,193 | 4,339 | 4,822 | 12.0 |

The classifier is at least 2.5 times as fast as ELK on every ontology and thread count,
and the closest case is `ore_ont_13482` on one thread, an ontology of 58,376 classes
whose 689,540 axioms give 659,619 small contexts. On eight threads it is 3.0 times
as fast on ChEBI lite and 5.1 to 13.3 times as fast on the ORE ontologies. Its peak
resident set is below ELK's on every run. ChEBI lite on one thread took 2.1 s on forge
against 1.1 s on `atlas`, a host of the same model, in five runs in one process each,
while the other ontologies took the same time on both hosts. The cause is not known,
and the table gives the forge figure. The taxonomies of both reasoners agree on every ontology and thread count, and
each reasoner gives the same taxonomy on one and eight threads.

ELK's resident memory follows its heap setting more than its live data, as the
[earlier ELK record](runs/2026-11-02-elk-forge-aa0a41a6/record.json) shows with smaller
heaps. Its live heap after a full collection is 150 MiB for GO and 688 MiB for ChEBI
lite there.

## Classification time and memory

The stages of the last classification in the same forge run, in ms, are the
normalization and indexing, the saturation and the taxonomy. The memory estimate is the
classifier's own peak estimate in MiB, and the counted heap is the most that a
classification held at once beyond the axioms read, measured with `el_classify --heap`
on `wally` on four threads.

| Ontology | Threads | Index | Saturation | Taxonomy | Estimate | Counted heap |
|---|---:|---:|---:|---:|---:|---:|
| GO | 1 | 32 | 218 | 28 | 71 | 70 |
| | 8 | 27 | 39 | 15 | | |
| ChEBI lite | 1 | 288 | 1,347 | 441 | 461 | 411 |
| | 8 | 134 | 499 | 94 | | |
| `ore_ont_13482` | 1 | 393 | 601 | 29 | 566 | 612 |
| | 8 | 314 | 133 | 17 | | |
| `ore_ont_868` | 1 | 574 | 509 | 757 | 1,265 | 1,111 |
| | 8 | 507 | 116 | 371 | | |
| `ore_ont_1559` | 1 | 785 | 269 | 139 | 388 | 422 |
| | 8 | 576 | 70 | 84 | | |

Saturation and the taxonomy scale with the threads. Normalization and interning run on
one thread and take most of a run on eight threads on the ORE ontologies, which roadmap
item NEW-ITEM-1 follows. The estimate comes within 14% of the counted heap on each
ontology, so a memory budget bounds what it states. The classifier reads the axioms in
place, as the server's kept classification reads those of the open ontology, so the
peak resident set above holds one copy of the axioms.

## Records

The [run record](runs/2026-11-03-el-speed-forge-80644aec/record.json) holds the host,
the binary, harness and data hashes, the settings, every classification time of both
tools, the taxonomy hashes and the agreement checks of the tables above. Its directory
keeps both tools' output lines, ELK's warnings, the oracle's reports, the environment
with the load when each process started, and the script that ran.

The [record of commit 8e73284a](runs/2026-11-02-go-chebi-forge-8e73284a/record.json) and
the [ELK record of commit aa0a41a6](runs/2026-11-02-elk-forge-aa0a41a6/record.json) hold
the measurements of earlier builds on GO and ChEBI lite. The [ORE agreement record](runs/2026-11-02-ore-elk-agreement-aa0a41a6/record.json)
and the [HermiT agreement record](runs/2026-11-02-hermit-agreement-aa0a41a6/record.json)
hold the comparisons on the ORE 2015 sample and the range test ontologies. Their times
come from a loaded laptop and are not measurements.
