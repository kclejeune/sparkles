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
entailments that ELK misses when a property's range meets an individual. HermiT derives
a subsumption through a range on an asserted property and one through a range reached
by `ObjectHasValue`, and it finds an individual in two disjoint ranges inconsistent.
ELK misses all three, and the classifier derives them. A fourth ontology, where one
range clashes with a class assertion, is inconsistent for all of them. The tests in
`crates/sparkles-owl/tests/classify.rs` hold the four cases with HermiT's answers.

<a id="complete-el"></a>

The classifier covers OWL 2 EL apart from literals it cannot compare and property chains
through the top object property, which it uses in part and reports, and it reports no
axiom of the 298 ORE ontologies as used in part. Its taxonomies of GO, ChEBI lite and
the 298 ORE ontologies are byte for byte those of the build the item started from,
commit c9b6ebff. Its classifier is that of commit 80644aec up to two lines without
effect, and those taxonomies equal ELK's. They equal Konclude's on all but GO, whose
difference the next section explains. Before, it used axioms with
individuals, data ranges or literals in part on 18 ORE ontologies, and HermiT's
taxonomy of those ontologies had already shown that nothing it left out changed a
subsumption there.

A suite of 400 random OWL 2 EL ontologies, which
`crates/sparkles-owl/tests/fixtures/el587/generate.py` writes with seed 587, exercises
what the ORE sample does not. Each has six classes, four object properties, three data
properties, four individuals and 8 to 22 axioms. The axioms draw on class and property
assertions, negative assertions, same and different individuals, `ObjectOneOf` and
`ObjectHasValue`, domains and ranges, regular chains, transitive and reflexive
properties, functional data properties, the top object property, data ranges,
numeric, string and date-time literals, and keys. HermiT finds 76 of them
inconsistent. The classifier entails exactly HermiT's subsumptions between named
classes on all 400, reports no axiom as used in part, and agrees with HermiT on which
ontologies are inconsistent. Konclude stopped with a segmentation fault on 10 of them
and differs from HermiT on 4 of the other 390. On `case-074` it misses a subsumption
from a range of `xsd:token` under `rdfs:Literal`, on `case-124` one from the top
object property, and on `case-075` and `case-261` it takes the `owl:rational` value
3/2 for an integer. HermiT's answer follows from the axioms in each case, and the
fixture test keeps the four cases with these reasons. `tests/el_fixtures.rs` checks
the suite on every test run.

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
are entailed, so they are evidence for the inverse properties that roadmap item 603
covers. Konclude and Whelk 1.2.1 both agree with HermiT on the four range test
ontologies, including the two subsumptions and the inconsistency that ELK misses.

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
commands. `sparkles ontology reason`, `reduce` and `relax` take ROBOT 1.9.11's options and
defaults and chain in one process, and `materialize` is roadmap item 636. ROBOT 1.9.11
ran each command with its bundled ELK 0.6.0 on GO and ChEBI lite, from the release's
RDF/XML file to a written functional-style file, and the Sparkles commands ran on the
same files with their default options. `scripts/elk-compare/ofn-axioms.py --all`
compared each pair of outputs after expanding prefixed names and sorting the operands of
n-ary axioms. The outputs are equal axiom for axiom, annotations included, for every
command and for the chain `reason relax reduce`.

| Output | GO axioms | GO logical axioms | ChEBI lite axioms | ChEBI lite logical axioms |
|---|---:|---:|---:|---:|
| `reason` | 622,357 | 96,847 | 1,682,146 | 424,904 |
| `reduce` | 608,460 | 82,950 | 1,658,143 | 400,901 |
| `relax` | 617,214 | 91,704 | 1,666,767 | 409,525 |
| `reason relax reduce` | 622,357 | 96,847 | 1,677,573 | 420,331 |

The logical axioms are every axiom but the declarations and the annotation axioms. In
every output, each asserted subsumption between named classes is entailed by the
classifier's taxonomy, and each direct edge of the taxonomy is asserted. `reduce` leaves
exactly the 57,456 direct subsumptions of GO and the 296,173 of ChEBI lite between named
classes, which `scripts/el-oracle.py --asserted` compared.

On forge, the benchmark host, each command ran three times end to end from the release's
RDF/XML file to the written functional-style file. The table gives the median time in
seconds and the median peak resident memory in MiB. ROBOT's `reason` figures come from
the [reference reasoners run](runs/2026-11-03-reasoners-forge-ea96e8f5/record.json) on
the same host and files, with an 8 GiB heap.

| Run | GO time | GO peak RSS | ChEBI lite time | ChEBI lite peak RSS |
|---|---:|---:|---:|---:|
| `robot reason` | 14.04 | 3,597 | 30.63 | 8,287 |
| `sparkles ontology reason` | 3.45 | 1,525 | 7.74 | 2,516 |
| `sparkles ontology reduce` | 3.49 | 1,521 | 8.30 | 2,508 |
| `sparkles ontology relax` | 3.04 | 1,492 | 5.70 | 2,499 |
| `sparkles ontology reason relax reduce` | 3.87 | 1,527 | 10.27 | 2,516 |
| `sparkles ontology classify` | 2.77 | 1,374 | 5.61 | 2,563 |

`sparkles ontology reason` writes the same ontology as `robot reason` 4.1 times as fast
on GO and 4.0 times on ChEBI lite, in 42% and 30% of its peak resident memory. The
chain `reason relax reduce` takes 12% more time than `reason` alone on GO and 33% more
on ChEBI lite. `reduce` classifies the relaxed ontology again in memory, and no file is
read or written between the commands. `classify` writes the taxonomy only. The
[run record](runs/2026-11-03-robot-commands-forge-71fcf893/record.json) keeps every
round, the host, the binary's checksum and the axiom comparison.

ROBOT's other commands ran once each on a loaded laptop, so their times only show the
scale and are not measurements. Each is one end-to-end run in seconds, with the peak
resident memory in MiB. Roadmap item NEW-ITEM-4 takes them on the benchmark host.

| Run | GO time | GO peak RSS | ChEBI lite time | ChEBI lite peak RSS |
|---|---:|---:|---:|---:|
| `robot reason` | 21.2 | 3,743 | 40.3 | 7,361 |
| `robot reason` from the functional-style file | 7.1 | 1,255 | 21.1 | 4,549 |
| `robot reduce` | 12.5 | 2,080 | 44.0 | 7,570 |
| `robot relax` | 9.0 | 1,491 | 14.8 | 2,279 |
| `robot materialize` | 14.4 | 2,106 | 37.7 | 7,509 |
| `robot reason relax reduce` | 23.2 | 7,088 | 68.4 | 8,612 |

ROBOT ran with an 8 GiB heap. The materialized property was part of, `BFO:0000050`, for
GO and has role, `RO:0000087`, for ChEBI lite.

## File classification

<a id="file-classification"></a>

`sparkles ontology classify FILE` reads a file of one graph into memory without a store,
and the taxonomy sorts its classes, builds its nodes and looks up a class's node on
several threads. On forge, five interleaved rounds of the build of commit 62704d5b and of
the build of commit c9b6ebff, which loads the file into a store first, classified GO and
ChEBI lite from the release's RDF/XML file and from the functional-style file to a
written taxonomy, on every core and on one thread. The table gives the median time in
seconds from the start of the process to its end, and the median peak resident set in
MiB. In the last round the two builds wrote byte-identical taxonomies of every file on
every thread count.

| File | Threads | Store path | File path | Store path peak RSS | File path peak RSS |
|---|---:|---:|---:|---:|---:|
| GO, functional-style | all | 0.91 | 0.25 | 657 | 286 |
| | 1 | 1.09 | 0.40 | 600 | 281 |
| GO, RDF/XML | all | 4.15 | 2.76 | 1,659 | 1,395 |
| | 1 | 4.34 | 2.92 | 1,667 | 1,382 |
| ChEBI lite, functional-style | all | 3.36 | 1.13 | 1,863 | 931 |
| | 1 | 3.85 | 1.57 | 1,790 | 901 |
| ChEBI lite, RDF/XML | all | 8.19 | 5.93 | 2,860 | 2,503 |
| | 1 | 8.54 | 6.59 | 2,765 | 2,482 |

ChEBI lite classifies from its functional-style file in 1.13 s, three times as fast as
before. The read parses the document on several threads and keeps its axioms, so it maps
nothing to triples and back. From RDF/XML the parse runs on several threads, and the
reverse mapping of the triples, which runs on one, takes most of what remains. A
phase-timed run on `atlas` gives 0.57 s for GO's parse and 1.4 s for its reverse
mapping in a run of 2.9 s, and 0.81 s and 2.9 s in ChEBI lite's 5.7 s, which roadmap
item 668 follows. The file path holds no store, so its peak resident set is
lower on every file, by half from the functional-style files.

`robot reason` took 14.0 s on GO and 30.6 s on ChEBI lite on the same host from the
RDF/XML file, as [above](#robot), so the file path classifies the same files 5.1 and
5.2 times as fast. ROBOT writes the whole ontology with the inferred axioms, while
`sparkles ontology classify` writes only the taxonomy.

`el_classify` measured the classification alone in the same rounds, with the axioms read,
as the median of three classifications in each of five rounds, in ms. The rows give the
classification and its taxonomy stage before and after the taxonomy's sort, nodes and
lookup ran on several threads.

| Ontology | Threads | Before | After | Taxonomy before | Taxonomy after |
|---|---:|---:|---:|---:|---:|
| GO | 8 | 85 | 82 | 14 | 11 |
| ChEBI lite | 8 | 962 | 888 | 91 | 71 |
| `ore_ont_868` | 8 | 992 | 878 | 372 | 249 |
| `ore_ont_9674` | 8 | 989 | 883 | 369 | 252 |
| `ore_ont_1559` | 8 | 778 | 744 | 88 | 60 |
| `ore_ont_13482` | 8 | 517 | 511 | 17 | 14 |
| `ore_ont_868` | 1 | 1,855 | 1,835 | 751 | 714 |

On one thread every classification and its peak resident set are within 3% of before.
On eight threads the peak resident set of `el_classify` is within 2% of before on every
ontology but GO, where it rises from 242 to 256 MiB. An interleaved run of three rounds
on `atlas` gives the same picture, with `ore_ont_868` at 990 and 861 ms and its taxonomy
at 391 and 246 ms on eight threads.

The taxonomies that `el_classify` writes were byte for byte the same in the build of
commit c9b6ebff and the build of commit e196d28b, whose taxonomy stage is that of
62704d5b, on GO, ChEBI lite and the 298 ORE ontologies on one and eight threads. The
command line's taxonomies on one and eight threads, its JSON answers without the times
and its `check --format json` answers were compared between the builds of 62704d5b and
c9b6ebff on 904 files. These are GO, ChEBI lite and the 298 ORE ontologies, each in
functional-style, RDF/XML and Turtle syntax, and four cut or malformed files, whose
errors and exit codes must match. All 3,616 comparisons gave the same bytes. A file that
ran out of memory with five comparisons at once on `atlas` was compared again alone with
12 GiB. The
[run record](runs/2026-11-03-file-classification-forge-62704d5b/record.json) holds every
time, the binaries' hashes, the environment, the outputs' comparison and these checks.

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
GO's inverse property, as [above](#whelk-and-konclude). The classifier in this run was
built from the sources of commit 1e0760c9, before item 602, so its times here are
slower than the current classifier's in [Against ELK](#against-elk).

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
threads is ahead of this build on ChEBI lite and the six largest ORE ontologies, while
the current classifier is ahead of ELK there, as [Against ELK](#against-elk) shows. On `ore_ont_14042` and `ore_ont_11395` ELK is slower on
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
resident set is below ELK's on every run. ChEBI lite on one thread took 2.1 s in this
run against 1.1 s on `atlas`, a host of the same model, while the other ontologies took
the same time on both hosts. A rerun on forge under the same lock took 1,158 and
1,181 ms unpinned, 1,155 ms pinned to a performance core and 1,832 ms pinned to an
efficient core, so the first process most likely ran on an efficient core. The table
keeps the first figure, and the rerun gives ELK 5.7 times the classifier's time there. The taxonomies of both reasoners agree on every ontology and thread count, and
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

Normalization and interning run on one thread and take most of a run on eight threads
on the ORE ontologies, which roadmap item 644 follows. The taxonomy's sort, node
construction and lookup ran on one thread in this build, and the
[file classification](#file-classification) section measures them on several.
ChEBI lite's saturation scales least. A stage trace on `atlas` shows its workers
spending 4.8 s in all on eight threads on the work that one thread does in 0.72 s, and
one activation of a hub context taking 422 ms after the roots run out, which item
645 follows. `ore_ont_13482` pays for 659,619 small contexts, item 648. The estimate comes within 14% of the counted heap on each
ontology, so a memory budget bounds what it states. The classifier reads the axioms in
place, as the server's kept classification reads those of the open ontology, so the
peak resident set above holds one copy of the axioms.

<a id="complete-el-cost"></a>

The rules of item 587 cost little where they have nothing to do. An interleaved A/B on
forge ran the build the item started from, commit c9b6ebff, and the item's build, commit
c4df06fe, five rounds each on one and eight threads with the bench and heavy locks held
for the whole run. On the eight largest ORE 2015 EL ontologies the item's build takes
0.97 to 1.03 times the time of the base, and on ChEBI lite 1.03 times on one thread and
the same on eight. GO takes 287 ms instead of 281 on one thread and 81 ms instead of 79
on eight, 2 to 3% more, almost all of it in the saturation, which roadmap item NEW-
ITEM-3 follows. Both builds give the same taxonomy of every ontology in the run, and the
peak resident set of the large ontologies changes by under 3%. On seven small ORE
ontologies with individuals the item's build uses every axiom in full where the base
used some in part, derives up to 28% more conclusions and takes 2 to 14 ms, within a few
milliseconds of the base.

| Ontology | Threads | Base ms | Item 587 ms | Ratio |
|---|---:|---:|---:|---:|
| GO | 1 | 281 | 287 | 1.02 |
| | 8 | 79 | 81 | 1.03 |
| ChEBI lite | 1 | 1,106 | 1,136 | 1.03 |
| | 8 | 910 | 907 | 1.00 |
| `ore_ont_13482` | 1 | 1,027 | 1,036 | 1.01 |
| | 8 | 507 | 521 | 1.03 |
| `ore_ont_868` | 1 | 1,858 | 1,831 | 0.99 |
| | 8 | 990 | 980 | 0.99 |
| `ore_ont_1559` | 1 | 1,195 | 1,202 | 1.01 |
| | 8 | 764 | 754 | 0.99 |

## Records

The [record of item 587's A/B](runs/2026-11-03-el-complete-forge-c4df06fe/record.json)
holds every classification time of both builds, the stage times, the taxonomy hashes and
the script that ran, and the [agreement record of item
587](runs/2026-11-03-el-complete-agreement-c4df06fe/record.json) holds the comparisons on
GO, ChEBI lite and the ORE 2015 sample and the reference reasoners' outputs on the 400
fixtures.

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

The [reference reasoner agreement record](runs/2026-11-02-reasoners-agreement-91a7b160/record.json)
holds Whelk's and Konclude's output lines and comparisons on the range test ontologies,
GO, ChEBI lite and the ORE 2015 sample, the ROBOT runs and their checks, and the trace of
Konclude's extra GO subsumptions. Its times also come from a loaded laptop.
