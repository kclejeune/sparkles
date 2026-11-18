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
lite and the 298 ORE ontologies on four threads, and the same ones on one thread. The build
of commit 65a7591d, which normalizes on several threads, gives taxonomies byte for byte
equal to those of commit 790cffae on GO, ChEBI lite and the 298 ORE ontologies on one
and eight threads, in two sweeps on `atlas`.

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
commands. `sparkles ontology reason`, `reduce`, `relax` and `materialize` take ROBOT
1.9.11's options and defaults and chain in one process. ROBOT 1.9.11
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
resident memory in MiB. Roadmap item 688 takes them on the benchmark host.

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

### Materialize

`sparkles ontology materialize` with part of on GO and has role on ChEBI lite writes the
same ontology as `robot materialize` with ELK, axiom for axiom. The outputs hold 608,460
axioms on GO and 1,666,767 on ChEBI lite, and neither command adds an axiom, since both
releases already assert every direct existential superclass for those properties. Other
choices of properties add axioms. Regulates, `RO:0002211`, adds 310 on GO through its
sub-properties, and every object property of ChEBI lite adds 13, all with
`RO:0018036`. Every object property of GO adds none. Roadmap item 688 compares these
runs with ROBOT's outputs and times `robot materialize` on the benchmark host.

On forge each run went three times end to end from the release's RDF/XML file to the
written functional-style file. The table gives the median time in seconds and the
median peak resident memory in MiB.

| Run | GO time | GO peak RSS | ChEBI lite time | ChEBI lite peak RSS |
|---|---:|---:|---:|---:|
| `sparkles ontology reason` | 3.47 | 1,520 | 8.77 | 2,506 |
| `sparkles ontology materialize` with one property | 3.31 | 1,522 | 8.32 | 2,824 |
| `sparkles ontology materialize` with every property | 4.20 | 1,681 | 14.89 | 5,084 |
| `sparkles ontology materialize --term RO:0002211` | 3.42 | 1,519 | | |
| `sparkles ontology reason materialize reduce` | 4.14 | 1,530 | | |

One property takes about as long as `reason`, because the classifier runs once with a
class named for each restriction of that property. Each named restriction holds
memory, so one property on ChEBI lite adds 318 MiB of peak resident memory to
`reason`'s, and each of its other eight properties adds about 280 MiB more. On GO, which
also has nine properties, all of them together add 161 MiB. The
[run record](runs/2026-11-03-materialize-forge-81851812/record.json) keeps every round,
the host, the binary's checksum and the axiom comparison.

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
the class hierarchy. Whelk and HermiT classify on one thread. The classifier of commit
65a7591d, ELK and Konclude ran in the [run of the classifier's speed
work](runs/2026-11-03-el-scaling-forge-65a7591d/record.json), and Whelk and HermiT in the
[earlier run](runs/2026-11-03-reasoners-forge-ea96e8f5/record.json). Every taxonomy equals
the classifier's, except that Konclude and HermiT both give the 7 GO edges that come
from GO's inverse property, as [above](#whelk-and-konclude).

| Ontology | Threads | Classifier | ELK | Konclude | Whelk | HermiT |
|---|---:|---:|---:|---:|---:|---:|
| GO | 1 | 0.28 | 1.18 | 1.32 | 3.37 | 28.97 |
|  | 8 | 0.08 | 0.58 | 1.32 |  |  |
| ChEBI lite | 1 | 1.05 | 6.61 | 4.24 | 16.47 | 111.55 |
|  | 8 | 0.39 | 2.59 | 4.16 |  |  |

The ORE columns give the thread count after each tool's name.

| Ontology | Classes | Classifier 1 | Classifier 8 | ELK 1 | ELK 8 | Konclude 1 | Konclude 8 | Whelk | HermiT |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `ore_ont_868` | 981,151 | 1.41 | 0.60 | 11.77 | 5.53 | 12.39 | 12.23 | 28.46 | 39.31 |
| `ore_ont_10689` | 981,148 | 1.38 | 0.59 | 11.26 | 5.15 | 12.10 | 11.94 | 26.97 | 38.85 |
| `ore_ont_9674` | 981,148 | 1.40 | 0.57 | 11.53 | 5.14 | 12.19 | 12.32 | 26.26 | 40.22 |
| `ore_ont_8486` | 903,617 | 1.24 | 0.55 | 10.86 | 4.63 | 10.89 | 11.07 | 24.27 | 37.03 |
| `ore_ont_14459` | 847,760 | 1.16 | 0.46 | 9.75 | 4.29 | 10.36 | 10.55 | 23.26 | 33.77 |
| `ore_ont_16008` | 733,100 | 1.07 | 0.43 | 8.41 | 3.63 | 8.81 | 8.93 | 19.89 | 29.75 |
| `ore_ont_14042` | 517,023 | 1.10 | 0.47 | 6.96 | 8.09 | 7.11 | 7.02 | 14.63 | 513.47 |
| `ore_ont_11395` | 517,023 | 1.09 | 0.49 | 6.96 | 7.03 | 7.09 | 7.09 | 15.14 | 525.19 |

Konclude's time is the target that roadmap item 603 sets. On these OWL 2 EL inputs it
classifies GO in 1.3 s, ChEBI lite in 4.2 s and the largest ORE ontology in 12.4 s, on
one thread or eight. The classifier is ahead of ELK and Konclude on every ontology and
thread count, with ELK taking 4.2 to 8.8 times its time on one thread and 6.6 to 17 times on
eight on these ontologies, and Konclude 4.0 to 23 times. On `ore_ont_14042` and
`ore_ont_11395` ELK is slower on eight threads than on one. HermiT takes 29 s on GO,
112 s on ChEBI lite and up to 525 s on the ORE ontologies.

Peak memory is each process's peak resident set in MiB, on eight threads where the tool
uses them. The JVMs ran with fixed heaps of 4 GiB for ELK on GO and ChEBI lite and
10 GiB on ORE, and 8 to 10 GiB for Whelk and HermiT, so their resident sets follow the
heap more than their live data.

| Ontology | Classifier | ELK | Konclude | Whelk | HermiT |
|---|---:|---:|---:|---:|---:|
| GO | 295 | 2,398 | 713 | 5,167 | 6,011 |
| ChEBI lite | 988 | 4,452 | 2,233 | 8,372 | 7,351 |

The ORE ontologies' peak memory follows.

| Ontology | Classifier | ELK | Konclude | Whelk | HermiT |
|---|---:|---:|---:|---:|---:|
| `ore_ont_868` | 1,688 | 9,185 | 5,952 | 11,015 | 10,146 |
| `ore_ont_10689` | 1,721 | 9,478 | 5,811 | 10,952 | 10,141 |
| `ore_ont_9674` | 1,656 | 9,700 | 5,809 | 11,011 | 10,638 |
| `ore_ont_8486` | 1,526 | 8,977 | 5,373 | 10,938 | 10,468 |
| `ore_ont_14459` | 1,488 | 8,183 | 5,060 | 10,975 | 9,648 |
| `ore_ont_16008` | 1,296 | 7,659 | 4,413 | 10,770 | 9,895 |
| `ore_ont_14042` | 1,383 | 7,225 | 4,212 | 9,873 | 8,271 |
| `ore_ont_11395` | 1,406 | 6,645 | 4,283 | 9,918 | 8,242 |

The run records hold every output line, the oracle's reports, the taxonomy hashes and
the scripts that ran.

## Against ELK

<a id="against-elk"></a>

ELK 0.6.0 and the classifier of commit 65a7591d ran on forge, the benchmark host, which
the script held under the bench lock for the whole run, so nothing else ran beside it.
Each process classified GO and ChEBI lite five times and each ORE ontology three times.
Times are the median classification time in ms. The classifier's time starts with the
axioms read and ends with the taxonomy built. ELK's starts at the creation of the
reasoner over the OWL API ontology and ends with its class hierarchy, so it leaves out
the OWL API's load of the file, as the classifier's leaves out its parse. Peak memory is
each process's peak resident set in MiB, which for the classifier includes the axioms
read and for ELK the OWL API ontology. ELK ran with a fixed heap of 4 GiB for GO and
ChEBI lite and 10 GiB for the ORE ontologies. The ORE ontologies are the largest of the
ORE 2015 EL sample by classification time, and `ore_ont_868`, `ore_ont_9674` and
`ore_ont_10689` are versions of one ontology.

| Ontology | Classes | Threads | Classifier | Peak RSS | ELK | ELK peak RSS | ELK time over the classifier's |
|---|---:|---:|---:|---:|---:|---:|---:|
| GO 2026-07-26 | 51,986 | 1 | 279 | 196 | 1,182 | 2,402 | 4.2 |
| | | 8 | 77 | 295 | 579 | 2,398 | 7.6 |
| ChEBI lite 256 | 238,150 | 1 | 1,049 | 650 | 6,611 | 4,457 | 6.3 |
| | | 8 | 393 | 988 | 2,591 | 4,452 | 6.6 |
| `ore_ont_13482` | 58,376 | 1 | 897 | 1,294 | 2,766 | 5,260 | 3.1 |
| | | 8 | 368 | 1,491 | 3,244 | 7,985 | 8.8 |
| `ore_ont_868` | 981,151 | 1 | 1,405 | 1,535 | 11,772 | 9,627 | 8.4 |
| | | 8 | 599 | 1,688 | 5,532 | 9,185 | 9.2 |
| `ore_ont_9674` | 981,148 | 1 | 1,397 | 1,535 | 11,530 | 9,347 | 8.3 |
| | | 8 | 574 | 1,656 | 5,142 | 9,700 | 9.0 |
| `ore_ont_10689` | 981,148 | 1 | 1,379 | 1,537 | 11,259 | 9,700 | 8.2 |
| | | 8 | 587 | 1,721 | 5,147 | 9,478 | 8.8 |
| `ore_ont_1559` | 264,022 | 1 | 993 | 1,776 | 8,166 | 10,171 | 8.2 |
| | | 8 | 425 | 1,853 | 6,873 | 10,465 | 16.2 |
| `ore_ont_15059` | 142,884 | 1 | 1,132 | 2,039 | 9,336 | 6,684 | 8.2 |
| | | 8 | 467 | 2,217 | 8,162 | 8,866 | 17.5 |
| `ore_ont_3524` | 123,311 | 1 | 563 | 1,422 | 7,563 | 6,784 | 13.4 |
| | | 8 | 250 | 1,539 | 5,073 | 6,104 | 20.3 |
| `ore_ont_3836` | 58,887 | 1 | 569 | 1,182 | 4,829 | 5,545 | 8.5 |
| | | 8 | 225 | 1,202 | 4,621 | 5,847 | 20.5 |

The classifier is at least 3.1 times as fast as ELK on every ontology and thread count,
and the closest case is `ore_ont_13482` on one thread, an ontology of 58,376 classes
whose 689,540 axioms give 659,619 small contexts. On eight threads it is 6.6 times as
fast on ChEBI lite and 8.8 to 20.5 times as fast on the ORE ontologies. Its peak
resident set is below ELK's on every run. The taxonomies of both reasoners agree on
every ontology and thread count, and each reasoner gives the same taxonomy on one and
eight threads.

ELK's resident memory follows its heap setting more than its live data, as the
[earlier ELK record](runs/2026-11-02-elk-forge-aa0a41a6/record.json) shows with smaller
heaps. Its live heap after a full collection is 150 MiB for GO and 688 MiB for ChEBI
lite there.

<a id="scaling"></a>

The same run timed the build of commit 790cffae, before roadmap items 644, 645, 648 and
665, each in its own process just before the newer build's. The newer build is faster on every
ontology and thread count, most on eight threads, where chunks of the axioms normalize
in parallel and the saturation's workers deliver messages in batches. Both builds give
the same taxonomy, byte for byte, of every ontology on one and eight threads. On eight
threads the peak resident set rises on some ontologies, by up to 13% on `ore_ont_14042`
and `ore_ont_11395` and by 8% on ChEBI lite, since each chunk holds the names it meets
until the merge and each worker's outbox holds up to 4,096 messages, and it falls on the
largest ones. Times are medians in ms and peak memory in MiB.

| Ontology | Threads | Before | After | Ratio | Peak RSS before | Peak RSS after |
|---|---:|---:|---:|---:|---:|---:|
| GO | 1 | 288 | 279 | 0.97 | 199 | 196 |
| | 8 | 88 | 77 | 0.87 | 279 | 295 |
| ChEBI lite | 1 | 1,139 | 1,049 | 0.92 | 669 | 650 |
| | 8 | 912 | 393 | 0.43 | 911 | 988 |
| `ore_ont_13482` | 1 | 1,086 | 897 | 0.83 | 1,352 | 1,294 |
| | 8 | 588 | 368 | 0.63 | 1,518 | 1,491 |
| `ore_ont_868` | 1 | 1,772 | 1,405 | 0.79 | 1,735 | 1,535 |
| | 8 | 862 | 599 | 0.69 | 1,739 | 1,688 |
| `ore_ont_1559` | 1 | 1,207 | 993 | 0.82 | 1,776 | 1,776 |
| | 8 | 750 | 425 | 0.57 | 1,786 | 1,853 |
| `ore_ont_15059` | 1 | 1,340 | 1,132 | 0.84 | 2,039 | 2,039 |
| | 8 | 871 | 467 | 0.54 | 2,071 | 2,217 |
| `ore_ont_3524` | 1 | 611 | 563 | 0.92 | 1,422 | 1,422 |
| | 8 | 340 | 250 | 0.73 | 1,522 | 1,539 |
| `ore_ont_14042` | 1 | 1,301 | 1,096 | 0.84 | 1,135 | 1,034 |
| | 8 | 799 | 468 | 0.59 | 1,228 | 1,383 |

The [run record](runs/2026-11-03-el-scaling-forge-65a7591d/record.json) has the other
ORE ontologies, which follow the same pattern. ChEBI lite's saturation on eight threads
fell from 737 to 273 ms, and the index stage of `ore_ont_868` on eight threads from 495
to 263 ms. ChEBI lite's axioms in the order of its RDF/XML file saturate about 40%
slower on eight threads than in the order of its functional-style file, with the same
conclusions, in a Namespace A/B of both builds, which roadmap item 683 follows.

## Classification time and memory

The stages of the last classification in the same forge run, in ms, are the
normalization and indexing, the saturation and the taxonomy. The memory estimate is the
classifier's own peak estimate in MiB, and the counted heap is the most that a
classification held at once beyond the axioms read, measured with `el_classify --heap`
on a Namespace instance on four threads with the build of commit 82d64aed.

| Ontology | Threads | Index | Saturation | Taxonomy | Estimate | Counted heap |
|---|---:|---:|---:|---:|---:|---:|
| GO | 1 | 28 | 225 | 25 | 64 | 66 |
| | 8 | 22 | 36 | 9 | | |
| ChEBI lite | 1 | 116 | 741 | 237 | 430 | 385 |
| | 8 | 72 | 273 | 68 | | |
| `ore_ont_13482` | 1 | 328 | 544 | 19 | 564 | 565 |
| | 8 | 202 | 139 | 20 | | |
| `ore_ont_868` | 1 | 356 | 375 | 662 | 1,132 | 990 |
| | 8 | 263 | 54 | 267 | | |
| `ore_ont_1559` | 1 | 629 | 227 | 113 | 390 | 370 |
| | 8 | 307 | 48 | 50 | | |

On eight threads the index stage is now the largest on the ORE ontologies. Its chunks
normalize in parallel, and the merge of their names and expressions and `Index::build`
run on one thread, which roadmap item 684 follows. The taxonomy's IRI
comparisons are item 647. `ore_ont_13482` pays for 659,619 small contexts, item 648. The
estimate comes within 15% of the counted heap on each ontology, so a memory budget
bounds what it states. The build before items 644 and 645 estimated 67, 443, 566, 1,187
and 367 MiB on the same ontologies against counted heaps of 69, 404, 620, 1,085 and
397 MiB. The classifier reads the axioms in place, as the server's kept classification
reads those of the open ontology, so the peak resident set above holds one copy of the
axioms.

<a id="complete-el-cost"></a>

The rules of item 587 cost little where they have nothing to do. An interleaved A/B on
forge ran the build the item started from, commit c9b6ebff, and the item's build, commit
c4df06fe, five rounds each on one and eight threads with the bench and heavy locks held
for the whole run. On the eight largest ORE 2015 EL ontologies the item's build takes
0.97 to 1.03 times the time of the base, and on ChEBI lite 1.03 times on one thread and
the same on eight. GO takes 287 ms instead of 281 on one thread and 81 ms instead of 79
on eight, 2 to 3% more, almost all of it in the saturation, which roadmap item 665
removed, as [above](#scaling) shows. Both builds give the same taxonomy of every ontology in the run, and the
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

<a id="horn-cost"></a>

GO's `InverseObjectProperties(part_of has_part)` takes the Horn rules of roadmap item
603, which give GO's taxonomy the 14 subsumptions that Konclude entails. An interleaved
A/B on forge ran three sides, each three rounds of five classifications on one and eight
threads with the bench and heavy locks held. The base is commit 17d04145 with the Horn
rules as item 603 left them, the EL side is the same build with `--el-only`, and the
current rules of item 693 are commit 320d4708. The table gives the median over the rounds.

| Ontology | Threads | Base Horn ms | EL rules ms | Current ms | Current / base |
|---|---:|---:|---:|---:|---:|
| GO | 1 | 320 | 274 | 306 | 0.96 |
| | 8 | 87 | 92 | 91 | 1.05 |
| ChEBI lite | 1 | 1,040 | 1,044 | 1,052 | 1.01 |
| | 8 | 440 | 421 | 468 | 1.06 |
| `ore_ont_9663` | 1 | 20,360 | 771 | 3,958 | 0.19 |
| | 8 | 1,475 | 172 | 894 | 0.61 |
| `ore_ont_14817` | 1 | 9,795 | 677 | 2,159 | 0.22 |
| | 8 | 1,185 | 187 | 463 | 0.39 |
| `ore_ont_9855` | 1 | 4,370 | 66 | 1,521 | 0.35 |
| | 8 | 919 | 36 | 324 | 0.35 |
| `ore_ont_3836` | 1 | 558 | 560 | 596 | 1.07 |
| | 8 | 216 | 221 | 218 | 1.01 |
| `ore_ont_13482` | 1 | 905 | 899 | 922 | 1.02 |
| | 8 | 311 | 308 | 307 | 0.99 |

GO classifies 4% faster than with the base's Horn rules on one thread and 12% slower
than with the EL rules alone. It derives 2.9 million conclusions against 3.5 million for
the base's Horn rules and 2.8 million for the EL rules, and its peak resident set on one
thread is 223 MiB against 248 and 196 MiB. On eight threads the three sides differ by
less than the spread of their rounds, which ran from 84 to 108 ms. GO's taxonomy is the
base's with the Horn rules on both thread counts. The remaining cost on GO is accepted
as the tradeoff for its complete taxonomy, and roadmap item 693 follows it.
`--reasoner ELK` and `Limits::el_only` classify by the EL rules alone, with the EL
side's time and taxonomy.

The base's Horn rules pass their memory budget on `ore_ont_9663` and `ore_ont_14817`,
two Uberon-based files of the ORE 2015 DL sample, and classify again by the EL rules.
The current rules finish both with the Horn rules. `ore_ont_9663` takes 4.0 s with a
peak resident set of 1.0 GiB on one thread and 0.89 s with 1.4 GiB on eight, and
`ore_ont_14817` takes 2.2 s with 0.7 GiB and 0.46 s with 1.0 GiB. Konclude 0.7.0 took
8.9 to 9.7 s and 3.5 to 4.2 GB on them on four threads on the laptop in the development
set comparison of [F24](../../specs/F24-owl-classification/SPEC.md#the-horn-rules). The
current taxonomies miss 33 and 27 of Konclude's subsumptions, which follow from axioms
that are not Horn and stay left out, and give none that Konclude does not entail. Both
files still cost several times the EL rules' time. On one thread the Horn rules derive
52.0 million conclusions on `ore_ont_9663` and 29.4 million on `ore_ont_14817`, where
the EL rules derive 18.5 and 20.8 million, and item 693 follows this too.
`ore_ont_9855`, the slowest Horn file of the development set, classifies 2.9 times as
fast as with the base's rules on one and eight threads, with the same taxonomy.

The OWL 2 EL files and ChEBI lite take the EL rules on every side and give the same
taxonomies. `ore_ont_3836`'s index stage takes 434 ms against 398 ms on one thread,
which makes the classification 7% slower, and `ore_ont_13482` is 2% slower. The EL
rules' code did not change, and the builds without and with chain selection, which EL
runs never reach, execute the same number of instructions on `ore_ont_3836` on `atlas`,
within 0.2%. A build of `sparkles-owl` with one codegen unit classifies `ore_ont_3836`
on `atlas` in 543 ms with the current rules and 531 ms without chain selection, against
600 and 559 ms for the same commits built by default, so the difference comes from how
the compiler partitions the crate. Release builds now compile the crate as one unit, as
the [next section](#code-layout) describes. ChEBI lite's eight-thread rounds spread from
411 to 538 ms on every side, which roadmap item 683 follows.

<a id="horn-individuals"></a>

### The Horn rules with individuals

Roadmap item 692 runs the Horn rules on ontologies with individuals, functional data
properties and top properties, which the rules of item 603 left out, as
[F24](../../specs/F24-owl-classification/SPEC.md#the-horn-rules-with-individuals)
describes. Its gate is the Horn stratum of the ORE 2015 DL development set, 73 files, of
which HermiT 1.4.5.519 classifies 68 within 1200 s on the laptop, and the 47 files among
them with individuals, functional data properties or top properties, 44 of which have a
reference, since `ore_ont_11311`, `ore_ont_16744` and `ore_ont_8737` are among the five
without one. On four threads the
classifier's taxonomy equals HermiT's on 34 files and is sound on 33, with no taxonomy
difference against HermiT on any file, where the build before the item was equal on 23
and sound on 44. The 33 files that stay sound leave out only axioms outside the Horn
rules' scope, anonymous individuals in 29 files and datatypes outside OWL 2 EL or data
cardinalities in 4, and the record lists them by file. The reader refuses `ore_ont_9881`
for an invalid IRI in both builds. The 25 Horn files without individuals keep their
taxonomies byte for byte, and so do the 298 ORE 2015 EL files on one and eight threads.
A differential run of 300 generated Horn ontologies with nominals, inverse roles,
functionality, universals, self restrictions and disjoint and asymmetric roles, written
by the dl603 fixture generator with a family of its own and seed 692, equals HermiT's
taxonomy on every file on one and eight threads, where the build before the item left
axioms out on 290 of them.

An interleaved A/B on `atlas` timed the 47 files with individuals, functional data
properties or top properties on four threads: three rounds, the build before the item and
the item's build in turn with the order alternating by round, one process of three
classifications each, under the host's heavy lock in a 13 GiB scope, with the load average
at 5.7 when the run started and 5.4 when it ended. The build before the item left every
Horn axiom of these files out, so the comparison sets the Horn rules' work against the EL
rules alone on the same files, as the comparison of item 603 did, except on
`ore_ont_2775`, whose one inverse property axiom the build before the item took. The table gives the
median over the rounds of each process's median classification for the files above 40 ms
and the two assertion-heavy files below, and the
[record](runs/2026-11-10-horn-individuals-atlas-0f944b5c/record.json) holds every file
and round. Over the 47 files the geometric mean of the ratios is 1.40 and the median 1.24.
Thirty-three files take 0.9 to 1.6 times the base's time, six classify faster by 10% or
more, and eight take more than 1.6 times. Three of the eight finish under 6 ms, among them
`ore_ont_6855`, whose 14 functional data properties take the rules of individuals in 5.8 ms
against 0.5. Four files with 7,856 to 114,957 property assertions, `ore_ont_4510`,
`ore_ont_12901`, `ore_ont_14375` and `ore_ont_10951`, take 2.5 to 3.6 times the base and
stay under half a second: the rules create 3,147 to 42,249 conjunction contexts and derive
6.6 to 9.6 times the conclusions, most of which item 869 removed, as
[conjunctions that hold an individual](#conjunctions-with-an-individual) describes. The eighth,
`ore_ont_16744`, has 3.3 million axioms with RO relations and names the top property in two
tautological property inclusions, which made the build before the item refuse its two Horn
axioms. The item's build takes them, and the Horn rules derive 3.8 billion conclusions in
43 s with a peak resident set of 11.0 GiB, against 546 million in 5.9 s with 3.7 GiB for
the EL rules. On `ore_ont_8737`, 2.7 million axioms of the same kind, the Horn rules pass
their budget and the run finishes by the EL rules in 34 s against 25 s. The build before
the item classifies `ore_ont_16744` without the two inclusions in 51 s on the laptop, so
this cost is the Horn rules' on large OBO files and not the rules of individuals. Both
files keep their taxonomies. Their symmetric transitive property composed each link twice,
which item 962 removed, as [symmetric transitive properties](#symmetric-transitive-properties-with-the-horn-rules)
describes. The files whose axioms with anonymous
individuals the reader leaves out, such as `ore_ont_5481` with 457,662 of them, spend 0.12
to 0.27 s more in the index stage, where the Horn normalization now reads each of them
before it refuses it, which item 869 removed too.

| Ontology | EL rules ms | Item 692 ms | Ratio | Conclusions before | Conclusions after |
|---|---:|---:|---:|---:|---:|
| `ore_ont_16744` | 5,908 | 43,374 | 7.34 | 545,674,901 | 3,771,668,749 |
| `ore_ont_8737` | 25,545 | 34,378 | 1.35 | 2,101,403,004 | 2,118,579,266 |
| `ore_ont_5481` | 642 | 894 | 1.39 | 1,704,715 | 1,704,715 |
| `ore_ont_10073` | 641 | 887 | 1.38 | 1,704,715 | 1,704,715 |
| `ore_ont_3914` | 551 | 863 | 1.57 | 27,651,599 | 30,713,108 |
| `ore_ont_1370` | 334 | 500 | 1.49 | 2,074,571 | 2,074,571 |
| `ore_ont_4755` | 322 | 443 | 1.38 | 1,316,196 | 1,316,196 |
| `ore_ont_4572` | 239 | 324 | 1.36 | 2,206,719 | 4,650,214 |
| `ore_ont_12414` | 196 | 271 | 1.38 | 727,386 | 727,386 |
| `ore_ont_4510` | 120 | 434 | 3.60 | 2,427,529 | 19,150,344 |
| `ore_ont_12448` | 119 | 153 | 1.28 | 822,520 | 822,520 |
| `ore_ont_12901` | 96.8 | 307 | 3.17 | 1,925,362 | 12,716,523 |
| `ore_ont_11110` | 86.4 | 103 | 1.19 | 543,694 | 543,694 |
| `ore_ont_6478` | 71.5 | 86.6 | 1.21 | 82,182 | 82,182 |
| `ore_ont_11311` | 68.2 | 71.0 | 1.04 | 2,598,381 | 1,217,021 |
| `ore_ont_15733` | 45.4 | 35.3 | 0.78 | 2,181,081 | 407,361 |
| `ore_ont_14375` | 20.2 | 59.9 | 2.97 | 364,763 | 2,467,444 |
| `ore_ont_10951` | 13.6 | 33.5 | 2.47 | 176,238 | 1,689,814 |

<a id="code-layout"></a>

### Code layout

Release builds compile `sparkles-owl` as one codegen unit, and each rule set of the
saturation has a worker of its own that inlines that set's activation of a context. With
four units, the time of the classifier's stages moved by several percent with changes to
code they never run. The chain selection of item 693 made `ore_ont_3836` 7% slower on one
thread, and the hooks of the disjunctive rules made GO 0.9% and ChEBI lite 1.4% slower,
with the same work. One worker held the activations of all four rule sets, and whether
the compiler inlined each into it changed with their size.

An A/B on forge ran the base, commit aa1b4502 with four units, and the lane, commit
f93ad3cb, five rounds each, with three rounds of two reference builds and of the base with
one unit beside them in a screen of the same files. Times are the median over the rounds
of each process's median classification in ms.

| Ontology | Threads | Base | Lane | Lane / base | Before chain selection | Before the disjunctive rules |
|---|---:|---:|---:|---:|---:|---:|
| GO | 1 | 303.5 | 301.5 | 0.99 | 359.5 | 303.3 |
| | 8 | 89.5 | 85.9 | 0.96 | 96.9 | 95.7 |
| ChEBI lite | 1 | 1,041 | 1,022 | 0.98 | 1,041 | 1,047 |
| | 8 | 427 | 451 | 1.06 | 443 | 468 |
| `ore_ont_3836` | 1 | 558.5 | 542.7 | 0.97 | 562.1 | 561.1 |
| | 8 | 219.2 | 217.0 | 0.99 | 212.6 | 218.8 |
| `ore_ont_13482` | 1 | 902.1 | 867.8 | 0.96 | 907.4 | 911.6 |
| | 8 | 307.5 | 303.5 | 0.99 | 306.4 | 311.3 |

The reference columns come from the screen. Before chain selection is commit 3acec7c2,
whose Horn rules derive more on GO, and before the disjunctive rules is commit 3bc10115.
At the base the slowdowns of items 711 and 715 no longer show against either reference
on one thread, since the merge of the disjunctive rules moved the layout again, and the
lane is faster than all three builds on one thread on every file, by under 1% on GO. The
index stage gives most of the gain, 378 against 397 ms on `ore_ont_3836`
and 299 against 317 ms on `ore_ont_13482`, and on `atlas` the lane runs 3.8% fewer
user-space instructions than the base on `ore_ont_3836` and 2.0% fewer on GO, so one unit
lets the compiler inline across the crate as well as place its code.

ChEBI lite's eight-thread rounds spread by 15% on both sides of the gate. Six more rounds
with each process pinned to the performance cores 0 to 11 took ChEBI lite in 393 ms on
the lane against 418 ms on the base, with an 11 to 14% spread, and GO in 79.3 against
79.1 ms, so the eight-thread difference is within the noise that roadmap item 683 follows.
The 12 small ORE 2015 DL files of roadmap item 709 take 0.2 to 25 ms on both builds. The
lane is within 0.3 ms of the base on each on one thread, and from 1.5 ms faster to 0.2 ms
slower on eight. Every build gives
the same taxonomy of each file on one and eight threads.

One codegen unit for the base without the workers per rule set made GO about 1% slower
on one thread, since the Horn rules' activation then stayed out of line, and the workers
per rule set alone left the index stage as it was. Two codegen units recovered about half
of one unit's gain, and PERFORMANCE-EXPERIMENTS.md records it.

| Ontology, one thread pinned | Base | Two units | One unit |
|---|---:|---:|---:|
| GO | 301.7 | 304.6 | 301.7 |
| ChEBI lite | 1,042 | 1,031 | 1,021 |
| `ore_ont_3836` | 564.5 | 549.5 | 540.7 |
| `ore_ont_13482` | 899.1 | 881.9 | 867.4 |

On `atlas` the crate takes 25 to 27 s to compile in a release build with one unit, 15 s
with two and 9 to 12 s with four. A release build of `sparkles-server` after a change to
the crate took 713.5 s with one unit against 711.7 s with four, since the crates that
depend on it start from its metadata and its code generation runs beside theirs. A build
of the `el_classify` example after such a change takes about 46 s against 21 s. The
debug and test builds of `scripts/ci.sh` are unchanged.

<a id="small-files"></a>

### Cold runs of small files

A classification on several threads starts its helper threads only once it needs them.
The calling thread runs the saturation first. The others start at once when the run has
4,096 roots or more, and otherwise once the first thread has worked for 0.5 ms, so that a
saturation shorter than the threads' start runs on one thread. The parallel steps of the
normalization split the axioms into chunks of at least 4,096, against 1,024 before, so a
small ontology's index stage runs on one thread. The steps of the taxonomy keep chunks of
at least 1,024 classes, since a class costs more than an axiom. Before these
changes a cold run of `ore_ont_6855` on four threads spent 1.2 of its 1.4 ms in a
saturation that takes 0.1 ms on one thread.

An A/B on forge ran the base, commit ddfd453a, and the lane, commit 6b4c8e87, whose
taxonomy also took chunks of 4,096, on the 12
small ORE 2015 DL files of roadmap item 709 in seven rounds of rotating order, one thread
pinned to CPU 4 and four and eight threads pinned to the performance cores 0 to 11. Each
round ran one process with one classification, whose time is the cold one, and one with
ten, whose last nine give the warm time. The table gives the median over the rounds in ms.

| File | Rules | 1 thread, base and lane | 4 threads, base | 4 threads, lane | 8 threads, base | 8 threads, lane | Warm, 8 threads, base | Warm, 8 threads, lane |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `ore_ont_1445` | disjunctive | 2.7 and 2.7 | 3.2 | 3.0 | 3.5 | 3.7 | 2.0 | 2.5 |
| `ore_ont_2775` | Horn | 0.6 and 0.6 | 1.1 | 0.6 | 1.5 | 0.6 | 0.3 | 0.2 |
| `ore_ont_3231` | disjunctive | 4.5 and 4.6 | 3.9 | 3.8 | 4.8 | 4.7 | 2.6 | 3.0 |
| `ore_ont_5564` | disjunctive | 0.8 and 0.8 | 1.6 | 0.9 | 1.8 | 1.0 | 0.7 | 0.6 |
| `ore_ont_5940` | disjunctive | 1.0 and 1.1 | 1.6 | 1.1 | 1.9 | 1.2 | 0.9 | 0.7 |
| `ore_ont_6855` | Horn | 0.3 and 0.4 | 1.4 | 0.3 | 2.0 | 0.3 | 0.4 | 0.2 |
| `ore_ont_7584` | disjunctive | 3.8 and 3.9 | 3.4 | 3.3 | 4.0 | 4.0 | 2.2 | 2.7 |
| `ore_ont_7663` | Horn | 0.7 and 0.7 | 1.0 | 0.8 | 1.1 | 0.8 | 0.5 | 0.4 |
| `ore_ont_8203` | Horn | 6.2 and 6.2 | 6.8 | 6.3 | 8.0 | 7.3 | 3.3 | 3.7 |
| `ore_ont_8273` | disjunctive | 25.7 and 25.5 | 21.6 | 16.9 | 17.2 | 17.5 | 14.1 | 14.4 |
| `ore_ont_9096` | disjunctive | 1.7 and 1.7 | 2.1 | 1.8 | 2.7 | 1.8 | 1.1 | 1.2 |
| `ore_ont_13581` | Horn | 2.7 and 2.7 | 2.9 | 3.0 | 3.6 | 3.5 | 1.9 | 2.5 |

The cold runs take 0.74 of the base's time in geometric mean on four threads and 0.68 on
eight, from 0.25 to 1.07 times per file on four threads and from 0.17 to 1.05 on eight.
The 12 files take 41.9 ms on four threads against 50.6 ms. A whole process, which reads the file and starts and ends the runtime,
takes 0.86 of the base's time on four and eight threads. On one thread the times are the
base's within 0.1 ms. Five files whose saturation takes 1.8 to 4 ms on one thread became
0.4 to 0.6 ms slower in warm runs on eight threads, since their helper threads start 0.5 ms later.
The [helper starts](#helper-starts) section gives their times since item 720. On four threads the warm runs take 0.86 of the
base's time. Seven more rounds of the base, commit 6b4c8e87 and the final commit
47c13a51 gave the cold runs of 47c13a51 0.74 of the base's time on four threads and 0.67
on eight, against 0.75 and 0.67 for 6b4c8e87 in the same rounds.

A whole cold process of the lane on four threads takes less time on forge than Konclude
0.7.0 took on the same file on the laptop in the comparison of item 709, from 1.9 ms for
`ore_ont_6855` to 21.8 ms for `ore_ont_8273` against Konclude's 11 and 82 ms, and so does
the base's. The hosts differ, so this is not a published comparison. The item's estimates
of cold runs came from the laptop while other work ran on it, and they were 2.4 to 14
times the base's whole processes on forge.

On GO, ChEBI lite, `ore_ont_3836` and `ore_ont_13482`, five rounds of the same sides gave
the following times, in ms, with the same taxonomies.

| Ontology | Threads | Base | Lane | Lane / base |
|---|---:|---:|---:|---:|
| GO | 1 | 303.3 | 302.1 | 1.00 |
| | 8 | 79.6 | 79.6 | 1.00 |
| ChEBI lite | 1 | 1,023 | 1,035 | 1.01 |
| | 8 | 412 | 401 | 0.97 |
| `ore_ont_3836` | 1 | 541.9 | 531.8 | 0.98 |
| | 8 | 205.2 | 201.9 | 0.98 |
| `ore_ont_13482` | 1 | 862.4 | 871.3 | 1.01 |
| | 8 | 302.6 | 298.7 | 0.99 |

On one thread ChEBI lite's saturation is 1.1% slower than the base's, beyond the 0.5%
spread of its rounds, and a second run of five rounds put ChEBI lite 1.5% and
`ore_ont_13482` 1.6% above the base. One thread runs none of the new code. A build whose
one-thread worker leaves out the lazy start's check left them 0.9 and 1.4% slower and
made `ore_ont_3836` 0.5% slower, so the difference comes from code layout, which the
[helper starts](#helper-starts) section measures again on `atlas`. [PERFORMANCE-EXPERIMENTS.md](../../PERFORMANCE-EXPERIMENTS.md)
records that build.

A screen of the 161 development files under 20 MB on a Namespace instance, three rounds
on eight threads, flagged files of 7,646 to 53,085 classes, and five rounds of six of
them on forge on four and eight threads found one difference beyond the spread of the
rounds. With chunks of 4,096 classes the taxonomy of `ore_ont_9151`, which has 7,646
classes, ran in two chunks instead of eight and took 5.5 ms on eight threads against
2.9 ms, and the whole classification 32.2 against 30.0 ms. Commit 47c13a51 took 2.9 and 29.8 ms, which is why the taxonomy keeps
chunks of 1,024 classes. `ore_ont_16680`, the longest of the six, takes 2.6 s on eight
threads on every build.

<a id="saturation-memory"></a>

### Saturation memory

`ore_ont_8737` of the ORE 2015 DL sample has 136,612 classes and 2.5 million existential
restrictions. Its saturation holds about 420 MiB of conclusions at the end, and its peak
memory is the messages waiting in the contexts' to-do lists. Most of them repeat
conclusions that their context already holds, since many classes send the same links to
the same fillers. When the waiting messages take more memory than the stored conclusions
and 256 MiB, a worker that delivers a batch to a context whose state is free checks each
subsumption and backward link against that state and drops those the context holds. The
check stays off while the waiting messages are fewer, so other runs deliver as before.

One classification of each side on `atlas` in a scope with a 13 GiB cap gave the
following times and memory. The peak resident set is that of the process, and the peak
estimate is the classifier's own estimate of its memory. The conclusions are those the
rules took from the to-do lists, repeats included.

| Side | Threads | Time, s | Peak resident set, GiB | Peak estimate, GiB | Conclusions taken, billions |
|---|---:|---:|---:|---:|---:|
| Base | 4 | 32.3 | 10.5 | 5.7 | 3.00 |
| Lane | 4 | 29.0 | 7.5 | 3.8 | 2.10 |
| Base | 1 | 90.5 | 9.4 | 5.8 | 3.00 |
| Lane | 1 | 90.2 | 9.4 | 5.8 | 3.00 |

On four threads the peak resident set falls by 29%, and the run fits the 10 GiB cap of
the development set comparison, which it passed before. A run on one thread delivers no
batches, since its activations put their messages in the to-do lists directly, so its
peak is the base's. On four threads the peak estimate is still nine times the stored
conclusions, and the run takes 29 s, where Konclude 0.7.0 took 11 s with 7 GB on the
laptop. The [helper starts](#helper-starts) section gives the run on one thread since
item 721. Three other ways to bound the waiting
messages measured no better and are in
[PERFORMANCE-EXPERIMENTS.md](../../PERFORMANCE-EXPERIMENTS.md).

<a id="helper-starts"></a>

### Helper starts, one-thread congestion and layout

A process's first helper threads take much longer to start than later ones, whose stacks
it keeps. A saturation of fewer than 4,096 roots on several threads therefore waits 0.5
ms before its first start of the helpers in a process, and after that as long as its
first worker took to start them in the process's last start, between 20 and 500 us.
The first worker looks at the clock after each activation until the helpers start. A
run on one thread sends through its outbox once it is congested, so that its deliveries
drop the subsumptions and backward links that their contexts hold, as those of several
threads do.

These measurements ran on `atlas`, which has the benchmark host's hardware, with no other
benchmark or gate on the host and no swap in use. The base is commit dbac7667, before is
the base without the lazy start of item 709 and the delivery filter of item 695, and the
lane is commit 5dca16bd. The table gives the median over nine rounds of rotating order,
in ms, with four and eight threads pinned to the performance cores 0 to 11, as in the
section above.

| File | Cold, 8 threads, before | Cold, 8 threads, base | Cold, 8 threads, lane | Warm, 8 threads, before | Warm, 8 threads, base | Warm, 8 threads, lane | Warm, 4 threads, before | Warm, 4 threads, base | Warm, 4 threads, lane |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `ore_ont_1445` | 3.47 | 3.78 | 3.73 | 2.04 | 2.60 | 2.25 | 2.50 | 2.46 | 2.01 |
| `ore_ont_2775` | 1.55 | 0.69 | 0.64 | 0.29 | 0.20 | 0.21 | 0.23 | 0.19 | 0.20 |
| `ore_ont_3231` | 4.65 | 4.34 | 4.49 | 2.63 | 3.07 | 2.67 | 3.86 | 2.88 | 2.62 |
| `ore_ont_5564` | 1.83 | 0.96 | 0.95 | 0.76 | 0.64 | 0.67 | 0.68 | 0.64 | 0.66 |
| `ore_ont_5940` | 2.31 | 1.21 | 1.21 | 0.92 | 0.74 | 0.75 | 0.85 | 0.74 | 0.79 |
| `ore_ont_6855` | 1.94 | 0.40 | 0.37 | 0.41 | 0.22 | 0.23 | 0.37 | 0.22 | 0.23 |
| `ore_ont_7584` | 4.05 | 3.90 | 3.74 | 2.17 | 2.73 | 2.30 | 3.58 | 2.58 | 2.21 |
| `ore_ont_7663` | 1.19 | 0.80 | 0.80 | 0.51 | 0.37 | 0.38 | 0.45 | 0.37 | 0.39 |
| `ore_ont_8203` | 8.23 | 7.79 | 7.60 | 3.68 | 3.86 | 3.78 | 4.98 | 4.25 | 3.79 |
| `ore_ont_8273` | 17.23 | 17.39 | 17.20 | 14.22 | 14.56 | 14.15 | 15.94 | 14.60 | 14.31 |
| `ore_ont_9096` | 2.93 | 1.92 | 1.91 | 1.27 | 1.27 | 1.28 | 1.29 | 1.25 | 1.27 |
| `ore_ont_13581` | 3.49 | 3.65 | 3.57 | 2.00 | 2.53 | 2.14 | 2.67 | 2.40 | 2.04 |

Warm runs of the five files whose saturation takes 1.8 to 4 ms on one thread take 0.04 to
0.21 ms longer on eight threads than before, against 0.18 to 0.56 ms for the base, and
on four threads they are faster than both. Cold runs take 0.67 of the time before in
geometric mean on eight threads and 0.77 on four, against 0.69 and 0.78 for the base.
Warm runs of the smallest files, which never start the helpers, take 0.01 to 0.05 ms
longer than the base's, within the spread of their rounds on eight threads, and roadmap
item 819 follows them with the rest of the five files.

On one thread six rounds of the same sides gave the following times in ms, with one
thread pinned to CPU 4.

| Ontology | Before | Base | Lane | Lane / base |
|---|---:|---:|---:|---:|
| GO | 304.0 | 309.0 | 311.8 | 1.009 |
| ChEBI lite | 1,068 | 1,058 | 1,058 | 1.000 |
| `ore_ont_3836` | 581.8 | 588.4 | 585.0 | 0.994 |
| `ore_ont_13482` | 910.4 | 910.9 | 915.9 | 1.006 |
| `ore_ont_7914` | 316.5 | 315.5 | 314.8 | 0.998 |

The rounds spread by 0.5 to 2%. A run on one thread that is not congested reads the batching
flag at each message it sends to another context, as it did before, and adds a comparison
of the waiting messages with the stored conclusions to each check of the bounds. The times
also move with the code's layout. Commit 70a5234c, whose run on one thread is the lane's
and which differs from it only in code that runs on several threads, gave in ten rounds of an earlier run 0.998 of the time before
on GO, 0.998 on ChEBI lite, 1.000 on `ore_ont_3836`, 1.005 on `ore_ont_13482` and 1.003 on
`ore_ont_7914`, with the same user-space cycle counts as before within 0.3%. The base
spent 1.3% more cycles than before on GO and 2.2% more on `ore_ont_3836`. Roadmap item
722 follows the layout. On four and eight threads the lane is within the spread of the
base's rounds on all five files, and ChEBI lite's eight-thread rounds spread by 9 to 13%.

One run of `ore_ont_8737` of each side in a scope with a 13 GiB cap gave the following
times and memory. The lane's runs are those of commit 70a5234c, whose saturation on one
thread and congested runs are the lane's.

| Side | Threads | Time, s | Peak resident set, GiB | Peak estimate, GiB | Conclusions taken, billions |
|---|---:|---:|---:|---:|---:|
| Base | 1 | 95.8 | 9.34 | 5.84 | 3.00 |
| Lane | 1 | 90.7 | 5.88 | 3.21 | 2.24 |
| Base | 4 | 28.5 | 6.77 | 3.51 | 2.13 |
| Lane | 4 | 28.2 | 7.92 | 4.07 | 2.10 |

On one thread the peak resident set falls by 37% and the conclusions the rules take by
25%. A run on four threads does the same work on both builds, and its peak varies from
6.8 to 7.9 GiB between runs of the same code, as the threads' timing decides which
contexts are free when their messages arrive. Konclude 0.7.0 took 11 s with 7 GB on the
laptop, and roadmap item 818 follows the time on several threads. The base's one-thread
run comes from the reproduction before the lane's changes, in the same session on the
same host. Every side gave the same taxonomies in every round. On the laptop the 298 ORE
2015 EL files, GO and ChEBI lite gave the base's taxonomies on one and eight threads, and
`ore_ont_8737` gave the base's taxonomy on one and four threads.

### Link products in runs

Roadmap item 818 measured where the time of `ore_ont_8737` on several threads goes before
it changed anything. The ontology is 2,544,786 axioms `C ⊑ ∃r.D` over one transitive
symmetric role, with no class hierarchy, so its saturation composes a transitive closure
through backward and forward links and derives each link of a component of k classes about
k times. On four threads the base sends 2.98 billion messages, 1.49 billion backward links
and 1.49 billion forward links, for about 32 million stored conclusions, in 738,588
deliveries of about 13 messages per destination. Only 20 million of them, 0.7%, reach a
context that another worker holds, and 61 million reach a context not yet started. The
congested filter of item 695 checked 2.81 billion and dropped 877 million backward links,
and no forward links, since it looked only at subsumptions and backward links. A profile of
the base on four threads on the laptop put 26% of the user cycles in `Run::deliver`, almost
all of it the radix sort of the outbox's messages by destination, 18% in the filter of
`Run::deliver_new`, 12% in `Out::send`, 11% in the hash lookups of the filter and about 30%
in the rules.

Item 818 makes two changes. A congested delivery drops every kind of conclusion that
`State::holds` answers, forward links and loops included. And the rules of `Back`, `Fwd` and
the existential subsumers store their link and leave its products, the subsumer each
propagation gives each source and the links the chains compose from each pair, to the
activation, which sends them once its to-do list is empty, or when idle workers wait,
pairing what is new since its last flush with everything and what is old with what is new.
The products for one source or target leave the context together, the outbox records these
runs and sorts the run headers rather than the messages, and a delivery checks a long run
against one context's state. In the profile of commit bd924609 on four threads `Run::deliver` takes 2%,
the sends 22%, the filter's lookups 29% and the rules about 15%.

These measurements ran on `atlas`, which has the benchmark host's hardware, with no other
benchmark or gate on the host and no swap in use. The base is commit 321508fe, a is the
filter alone, commit 181e9c95, b adds the runs, commit fb894c98, and b3 adds a flush that
returns at once when the activation stored nothing and builds its run in chunks, commit
bd924609, which is the saturation code that landed. A first slot ran the base, a and b, and
a second slot the base and b3. One run of each side and thread count classified
`ore_ont_8737` in a scope with a 13 GiB cap.

| Side | Threads | Saturation, s | Peak resident set, GiB | Peak estimate, GiB | Conclusions taken, billions |
|---|---:|---:|---:|---:|---:|
| Base | 1 | 83.1 | 5.91 | 3.21 | 2.24 |
| a | 1 | 73.9 | 3.61 | 1.69 | 1.62 |
| b | 1 | 44.0 | 3.76 | 1.70 | 1.62 |
| Base | 4 | 28.6 | 7.58 | 4.20 | 2.10 |
| a | 4 | 22.0 | 5.54 | 2.65 | 1.31 |
| b | 4 | 13.2 | 4.66 | 2.06 | 1.27 |
| Base | 12 | 16.9 | 8.44 | 4.36 | 2.14 |
| a | 12 | 12.4 | 6.44 | 2.75 | 1.30 |
| b | 12 | 8.8 | 7.36 | 3.30 | 1.30 |
| Base, second slot | 1 | 83.0 | 5.91 | 3.21 | 2.24 |
| b3 | 1 | 45.0 | 3.76 | 1.70 | 1.62 |
| Base, second slot | 4 | 25.2 | 6.87 | 3.46 | 2.12 |
| b3 | 4 | 13.5 | 5.07 | 2.15 | 1.26 |
| Base, second slot | 12 | 18.5 | 9.91 | 5.39 | 2.16 |
| b3 | 12 | 9.0 | 7.86 | 3.48 | 1.32 |

The base's run on four threads took 28.6 s in the first slot and 25.2 s in the second, with
peaks of 7.6 and 6.9 GiB, as the threads' timing decides which contexts are free when their
messages arrive, and the runs of b and b3 vary less.

Every run gave the taxonomy of the base. Konclude 0.7.0-1138 classified the same file on
`atlas` in 9.49 s with one worker, 9.65 s with four and 9.73 s with twelve, after 13.7 to
14.2 s of parsing, at 6.7 GiB of resident memory, so its classification does not use the
workers. The run of b3 on twelve threads is faster than Konclude's classification, and its
run on four threads takes 1.4 times Konclude's time in three quarters of its memory.

The same slot ran six interleaved rounds of GO, ChEBI lite, `ore_ont_3836` and
`ore_ont_13482` on one, four and twelve threads, five classifications per process, the
one-thread runs pinned to one CPU and the others to twelve. The table gives the median of
the rounds in milliseconds and the ratio of b3 to the base.

| File | Threads | Base, ms | b3, ms | b3 / base |
|---|---:|---:|---:|---:|
| GO | 1 | 308.8 | 308.9 | 1.001 |
| GO | 4 | 106.7 | 105.3 | 0.987 |
| GO | 12 | 78.1 | 78.1 | 1.000 |
| ChEBI lite | 1 | 1057.2 | 1077.2 | 1.019 |
| ChEBI lite | 4 | 436.0 | 439.6 | 1.008 |
| ChEBI lite | 12 | 404.9 | 444.9 | 1.099 |
| `ore_ont_3836` | 1 | 588.7 | 587.2 | 0.998 |
| `ore_ont_3836` | 4 | 250.3 | 248.6 | 0.993 |
| `ore_ont_3836` | 12 | 194.2 | 192.2 | 0.990 |
| `ore_ont_13482` | 1 | 910.6 | 941.7 | 1.034 |
| `ore_ont_13482` | 4 | 391.2 | 403.3 | 1.031 |
| `ore_ont_13482` | 12 | 295.9 | 297.5 | 1.005 |

ChEBI lite's twelve-thread rounds of b3 were 406, 404, 522, 412, 478 and 540 ms against
401 to 414 ms for the base, so three of six rounds are slower, and its one-thread run is
1.9% slower. `ore_ont_13482` is 3.1 to 3.4% slower on one and four threads. Roadmap item
890 follows both files. In the first slot, before the flush
returned at once when nothing is pending, b had been 2.9% slower than the base on ChEBI
lite on one thread and 5.0% on four, and 3.0 and 3.8% slower on `ore_ont_13482`.

Every side gave the same taxonomy on every file and thread count. On the laptop the 298 ORE
2015 EL files, GO and ChEBI lite gave the base's taxonomies with b, commit fb894c98, on one and eight
threads, and `ore_ont_8737` gave it on one, four, eight and twelve threads.

### One copy of each run

After item 818 a profile of `ore_ont_8737` on four threads put 28% of the user cycles on
`atlas` in `Out::send`, which wrote each of a flush's products into the outbox once for
every destination, 34% in the congested filter of `Run::deliver_new`, `State::holds` and
`Small::contains`, and 8% in copies of to-do lists. Roadmap item 889 makes three changes.
A flush writes each chunk of a group's products into the outbox once, and each
destination's run header points at that copy. A congested delivery checks a run against
the destination's tables in one inlined loop that looks up the sources of a backward
link's role once for each change of role. An activation applies the batch it takes from
its to-do list from the list's buffer instead of copying it reversed onto its own
conclusions. Each destination receives the same messages in the same order as before.

These measurements ran on `atlas` with no other benchmark or gate on the host and no swap
in use. The base is commit 3a1b9b04, the lane is commit 1da472f2, and the lane with the
table adds an open-addressing table for the sets of more than 16 members, commit 79b0c707,
which was reverted. Three rounds of rotating order classified `ore_ont_8737` once per side
and thread count in a scope with a 13 GiB cap. On every side the file's Horn attempt passes
its memory budget and the classifier classifies again by the EL rules alone, which roadmap
item 962 follows. The saturation is that second run's, while the classification, the CPU
time and the peak resident set include the attempt. The table gives the medians of the
rounds, and the largest peak.

| Side | Threads | Saturation, s | Classification, s | CPU time, s | Peak resident set, GiB | Conclusions taken, billions |
|---|---:|---:|---:|---:|---:|---:|
| Base | 1 | 45.0 | 67.6 | 71 | 8.50 | 1.62 |
| Lane | 1 | 24.6 | 40.2 | 43 | 8.52 | 1.62 |
| Lane with the table | 1 | 24.5 | 40.9 | 44 | 8.49 | 1.62 |
| Base | 4 | 13.7 | 22.2 | 88 | 10.29 | 1.28 |
| Lane | 4 | 8.9 | 15.6 | 60 | 10.11 | 1.28 |
| Lane with the table | 4 | 8.7 | 15.9 | 62 | 10.83 | 1.31 |
| Base | 12 | 9.3 | 15.5 | 157 | 12.20 | 1.33 |
| Lane | 12 | 7.4 | 13.4 | 127 | 12.19 | 1.33 |
| Lane with the table | 12 | 7.6 | 13.4 | 123 | 12.29 | 1.31 |

Every run gave the taxonomy of the base. The saturation takes 45% less time on one thread,
35% less on four and 20% less on twelve, and on four and twelve threads it takes less time
than Konclude 0.7.0's classification of the file on the same host, 9.5 to 9.7 s. In the
profile of the lane on four threads on the laptop the sends take under 1% of the cycles,
the filter 45%, the rules' inserts into the hash sets 13% and the copies 8%. The filter's
cost is the three billion messages the saturation sends for 32 million conclusions, which
roadmap item 956 follows.

The same slot ran six interleaved rounds of GO, ChEBI lite, `ore_ont_3836` and
`ore_ont_13482` on one, four and twelve threads, five classifications per process, the
one-thread runs pinned to one CPU and the others to twelve. The table gives the median of
the rounds in milliseconds and the ratios to the base.

| File | Threads | Base, ms | Lane, ms | Lane / base | Lane with the table / base |
|---|---:|---:|---:|---:|---:|
| GO | 1 | 312.2 | 303.9 | 0.973 | 0.970 |
| GO | 4 | 107.3 | 105.7 | 0.985 | 0.989 |
| GO | 12 | 78.1 | 77.3 | 0.991 | 1.001 |
| ChEBI lite | 1 | 1079.3 | 926.4 | 0.858 | 0.922 |
| ChEBI lite | 4 | 452.4 | 415.6 | 0.919 | 0.962 |
| ChEBI lite | 12 | 447.0 | 407.9 | 0.913 | 0.980 |
| `ore_ont_3836` | 1 | 597.0 | 586.3 | 0.982 | 1.005 |
| `ore_ont_3836` | 4 | 249.7 | 250.5 | 1.003 | 1.014 |
| `ore_ont_3836` | 12 | 193.3 | 193.9 | 1.004 | 1.014 |
| `ore_ont_13482` | 1 | 949.2 | 943.1 | 0.994 | 1.040 |
| `ore_ont_13482` | 4 | 418.1 | 408.4 | 0.977 | 1.010 |
| `ore_ont_13482` | 12 | 295.6 | 291.9 | 0.988 | 0.997 |

The table measured no faster on `ore_ont_8737` and slower on ChEBI lite, `ore_ont_3836` and
`ore_ont_13482`, so it was reverted, and
[PERFORMANCE-EXPERIMENTS](../../PERFORMANCE-EXPERIMENTS.md) records it. `ore_ont_3836`
classifies 0.3 and 0.4% slower on four and twelve threads, within its spread, which item
890 rechecks. The taxonomies of the last round equal the base's. On the laptop the 298 ORE 2015 EL files, GO, ChEBI lite and
`ore_ont_8737` gave the base's taxonomies on one and eight threads.

### Symmetric transitive properties with the Horn rules

Roadmap item 962 measured `ore_ont_8737` and `ore_ont_16744`, which item 692 let the Horn
rules take. Each has one symmetric transitive property, which is its own inverse, and the
Horn rules add the mirror image of each chain, so the transitivity of the property also
held for its inverse, and each composed link was derived by the property and again by its
inverse. The item's build composes each pair of links into the least properties of a
chain's result. On `atlas` on four threads, three rounds of alternating order with three
classifications per process gave the times below, each the median of the nine
classifications. The conclusions are the median of the three processes' reports and the
peak resident sets the largest. `ore_ont_8737` passed its budget of 7.05 GB on the base
and classified again by the EL rules alone, leaving its symmetric and inverse property
axioms out, and the item's build classifies it with the Horn rules and every axiom. Its
conclusions before are those of the EL rules' second run, and its peak resident set before
is the failed Horn attempt's.

| Ontology | Base ms | Item 962 ms | Ratio | Conclusions before | Conclusions after | Peak RSS before | Peak RSS after |
|---|---:|---:|---:|---:|---:|---:|---:|
| `ore_ont_8737` | 21,876 | 13,911 | 0.64 | 1,257,475,084 | 1,284,400,422 | 10.4 GiB | 6.0 GiB |
| `ore_ont_16744` | 26,204 | 3,857 | 0.15 | 2,228,811,518 | 445,855,316 | 9.6 GiB | 3.0 GiB |

In three further rounds of alternating order the item's build takes 13.4 s on
`ore_ont_8737` with the Horn rules against 13.6 s with the EL rules alone, and 3.80 s on
`ore_ont_16744` against 3.71 s. The eight smaller files of the same family derive the same
conclusions on both builds and classify in 10 to 39 ms by median.

GO and ChEBI lite ran six rounds of rotating order with five classifications per process
on one and four threads, against the reviewed build, commit 2fb430aa, whose reduction
gives the same properties. The ratio is the median of the rounds' ratios. The three rounds
of the first set had put ChEBI lite 4% slower on four threads with the same conclusions,
which the six rounds did not reproduce. ChEBI lite's median on four threads is still 1.9%
above the base's, within the spread of the rounds, and roadmap item 968 follows it.

| Ontology | Threads | Base ms | Item 962 ms | Ratio | Range of the rounds |
|---|---:|---:|---:|---:|---|
| GO | 1 | 313.5 | 307.6 | 0.981 | 0.972 to 0.986 |
| GO | 4 | 119.3 | 116.6 | 0.988 | 0.935 to 1.045 |
| ChEBI lite | 1 | 1,080.8 | 1,084.9 | 1.006 | 0.992 to 1.016 |
| ChEBI lite | 4 | 470.5 | 484.4 | 1.019 | 0.952 to 1.073 |

GO and ChEBI lite give the same taxonomies on both builds. Every file of the ORE 2015 DL
development set classified once by each build with its taxonomy. The 210 files that
classify gave the same bytes on both, the reader refuses `ore_ont_9881`, and
`ore_ont_7192` passed the comparison's 13 GiB cap on both builds. `ore_ont_9663` and
`ore_ont_14817` take the disjunctive rules, whose attempt fails before the Horn rules
answer. Their completed classifications took 10 to 229 s on both builds, the Horn rules'
run included, and one process of the base on `ore_ont_14817` passed the 900 s timeout, so
their times are not compared here. Roadmap item 969 follows them.

### Dominated successors and least unions

Roadmap item 710 lets the Horn rules pass over a successor that another dominates, and item
969 makes a product of the disjunctive rules keep only the least unions of each step of up
to 4,096 unions. On `atlas` four
rounds of alternating order with five classifications per process compared the base,
commit a22f6a35, with the items' build, commit a370d073. Each time below is the median of
the 20 classifications of a side, and each ratio the median of the four rounds' ratios. The
conclusions are the median of the four processes' reports. The two generated ontologies come from the
item's generator of Horn ontologies with inverse properties, at-most restrictions and
nested existential restrictions.

| Ontology | Threads | Base ms | Items 710 and 969 ms | Ratio | Conclusions before | Conclusions after |
|---|---:|---:|---:|---:|---:|---:|
| `g04281` | 1 | 5,062.5 | 69.4 | 0.014 | 209,787,104 | 1,959,164 |
| `g04281` | 4 | 1,716.0 | 33.6 | 0.019 | 193,434,969 | 3,377,311 |
| `g15813` | 1 | 2,980.4 | 49.0 | 0.016 | 115,478,502 | 1,791,747 |
| `g15813` | 4 | 1,001.9 | 25.1 | 0.025 | 147,788,674 | 2,907,862 |

The same rounds on the Horn files that the sort of successors does not prune, or prunes
little, give the times below. The round ratios show the spread.

| Ontology | Threads | Base ms | Items 710 and 969 ms | Ratio | Range of the rounds |
|---|---:|---:|---:|---:|---|
| GO | 1 | 301.2 | 305.0 | 1.012 | 1.011 to 1.013 |
| GO | 4 | 117.4 | 120.0 | 1.025 | 1.016 to 1.048 |
| ChEBI lite | 1 | 917.8 | 943.8 | 1.027 | 1.009 to 1.032 |
| ChEBI lite | 4 | 426.4 | 425.1 | 0.998 | 0.980 to 1.040 |
| `ore_ont_9855` | 1 | 1,408.2 | 1,478.6 | 1.050 | 1.046 to 1.051 |
| `ore_ont_9855` | 4 | 492.3 | 527.6 | 1.062 | 1.040 to 1.097 |
| `ore_ont_8737` | 1 | 24,477.5 | 25,162.8 | 1.030 | 1.021 to 1.033 |
| `ore_ont_8737` | 4 | 8,221.8 | 8,558.3 | 1.031 | 1.022 to 1.071 |
| `ore_ont_16744` | 1 | 8,235.9 | 8,308.6 | 1.009 | 1.008 to 1.010 |
| `ore_ont_16744` | 4 | 2,762.4 | 2,720.7 | 0.997 | 0.986 to 1.004 |
| `ore_ont_4510` | 1 | 925.4 | 944.3 | 1.021 | 1.013 to 1.023 |
| `ore_ont_12901` | 1 | 609.2 | 625.5 | 1.029 | 1.019 to 1.030 |

`ore_ont_9855` derives 21.2 million conclusions against 22.0 million on one thread, and the
sort costs more than the conclusions it saves. GO derives 2,946,303 conclusions against
2,948,787, and the other files the same conclusions on one thread. ChEBI lite runs none of the Horn rules that the items change, and on
`ore_ont_8737` a pinned run on the laptop spends the extra instructions in the worker's
loop, which the items do not change, so code layout accounts for part of these
differences. Roadmap item 991 follows them. On four threads `ore_ont_4510` and
`ore_ont_12901` vary from 0.83 to 1.29 between rounds and are left out of the table.

The disjunctive attempts of `ore_ont_14817` and `ore_ont_9663` pass their budget before the
Horn rules answer. Three processes of each build on four threads gave the classification
times below, the attempt included.

| Ontology | Base s | Items 710 and 969 s |
|---|---|---|
| `ore_ont_14817` | 59.7, 61.9, 79.5 | 19.9, 37.8, 38.9 |
| `ore_ont_9663` | 14.7, 33.9, 44.6 | 12.5, 13.1, 13.3 |

The attempt's length on `ore_ont_14817` still depends on the run, and Konclude 0.7.0
classified the two files in 8.9 to 9.7 s on four threads on the laptop. Roadmap item 990
follows the attempt.

A sweep on the laptop classified GO, ChEBI lite, the 298 ORE 2015 EL files and the 207
files of the DL development set, 499 distinct files, once per build on one and eight
threads with its taxonomy, against commit efca1c26, whose later commits change only the
accounting of memory and the inlining of one function. The 993 pairs that both builds
classified gave the same bytes. On one thread the base passed the 900 s timeout on
`ore_ont_14817`, whose answer from the items' build equals both builds' answer on eight
threads, and both builds passed it on `ore_ont_9663`. Both builds passed the 12 GiB cap on
`ore_ont_7192` on eight threads, and the reader refuses `ore_ont_9881`.

### A bound on the disjunctive attempt

Roadmap item 990 ends a run of the disjunctive rules without counting or the rooted rules
once it has derived 384 conclusions per class of the signature, at least 2,097,152, and
the Horn rules then answer. On `atlas` three rounds of alternating order with five
classifications per process compared the base, commit a370d073, with the item's build on
the 12 files of the ORE 2015 DL development set whose attempt finishes. Each time below is
the median of the 15 classifications of a side, each ratio the ratio of the medians, and
the range that of the three rounds' ratios.

| Ontology | Threads | Base ms | Item 990 ms | Ratio | Range of the rounds |
|---|---:|---:|---:|---:|---|
| `ore_ont_12141` | 1 | 58.6 | 57.8 | 0.987 | 0.981 to 1.011 |
| `ore_ont_12141` | 4 | 24.5 | 24.2 | 0.987 | 0.970 to 1.084 |
| `ore_ont_15491` | 1 | 28.9 | 29.1 | 1.005 | 0.999 to 1.006 |
| `ore_ont_15491` | 4 | 16.7 | 16.6 | 0.992 | 0.944 to 1.117 |
| `ore_ont_5438` | 1 | 618.2 | 614.8 | 0.994 | 0.983 to 1.001 |
| `ore_ont_5438` | 4 | 195.8 | 193.1 | 0.986 | 0.959 to 1.007 |
| `ore_ont_13103` | 1 | 1,957.8 | 1,950.7 | 0.996 | 0.996 to 1.000 |
| `ore_ont_13103` | 4 | 512.4 | 514.6 | 1.004 | 0.981 to 1.039 |
| `ore_ont_16680` | 1 | 5,908.9 | 5,912.9 | 1.001 | 0.999 to 1.003 |
| `ore_ont_16680` | 4 | 1,618.3 | 1,569.2 | 0.970 | 0.965 to 1.031 |
| `ore_ont_6952` | 1 | 876.8 | 1,007.9 | 1.150 | 1.118 to 1.167 |
| `ore_ont_6952` | 4 | 291.5 | 315.6 | 1.083 | 1.075 to 1.096 |
| `ore_ont_10140` | 1 | 904.5 | 1,039.4 | 1.149 | 1.135 to 1.150 |
| `ore_ont_10140` | 4 | 294.3 | 324.7 | 1.103 | 1.095 to 1.114 |
| `ore_ont_11460` | 1 | 1,475.6 | 1,618.4 | 1.097 | 1.091 to 1.106 |
| `ore_ont_11460` | 4 | 443.8 | 469.4 | 1.058 | 1.050 to 1.069 |

`ore_ont_4897`, `ore_ont_5940`, `ore_ont_6423` and `ore_ont_6485` classify in under 10 ms,
and their rounds' ratios range from 0.60 to 2.46, so they are left out of the table. The
files derive the same conclusions on both builds on one thread, and on four the counts of
both builds vary by schedule within the same range. On `ore_ont_6952`, `ore_ont_10140` and
`ore_ont_11460` the extra time is in the saturation outside the attempt's tests, which take
under 10 ms by the reports' `dlTestingMs`, and the attempts derive under 20,000
conclusions. On one thread on `wally`, an Intel Core
i7-12800H, three rounds of the base, the item's build and that build with
`--dl-attempt-work 0` gave the ratios below, each the ratio of the medians of 15
classifications. The build is as slow without the bound, so the check of the bound is not
the cost. Roadmap item 1012 follows it.

| Ontology | Item 990 | Item 990 without the bound |
|---|---:|---:|
| `ore_ont_6952` | 1.092 | 1.148 |
| `ore_ont_10140` | 1.101 | 1.115 |
| `ore_ont_11460` | 1.081 | 1.075 |

Three rounds of alternating order on four threads and one round on one thread classified
the files whose attempt passes its memory budget on the base, one process per side and
round. The times below are the classification times, the attempt included, and the
taxonomies of both builds are the same.

| Ontology | Threads | Base s | Item 990 s |
|---|---:|---|---|
| `ore_ont_9663` | 4 | 12.3, 16.2, 20.6 | 8.8, 8.8, 7.5 |
| `ore_ont_14817` | 4 | 30.3, 10.1, 29.6 | 8.7, 11.3, 7.9 |
| `ore_ont_3215` | 4 | 126.9, 137.9, 110.0 | 4.5, 4.6, 4.4 |
| `ore_ont_9663` | 1 | past the 900 s timeout | 74.5 |
| `ore_ont_14817` | 1 | 345.3 | 57.7 |
| `ore_ont_3215` | 1 | 628.4 | 13.6 |

The peak resident set of `ore_ont_3215` on four threads falls from 7.6 to 7.7 GiB to 1.8
to 2.1 GiB, and that of `ore_ont_14817` from 3.0 to 3.2 GiB to 2.1 to 2.3 GiB. The
bound's time depends on the run, as the memory budget's did, since the attempt keeps the
driver without steps.

A sweep on the laptop classified GO, ChEBI lite, the 298 ORE 2015 EL files and the 207
files of the DL development set once per build on one and eight threads with its taxonomy.
The 993 pairs that both builds classified gave the same bytes, and no file changed its
tier. On one thread the base passed the 900 s timeout on `ore_ont_9663`, whose answer from
the item's build equals both builds' answer on eight threads. Neither build answered on
`ore_ont_7192` within the sweep's limits, and the reader refuses `ore_ont_9881`.

<a id="conjunctions-with-an-individual"></a>

### Conjunctions that hold an individual

Roadmap item 869 makes R3 and R5 of the Horn rules at a context with an instance send an
individual what the conjunction of its successor holds, where they made a context for the
conjunction, as
[F24](../../specs/F24-owl-classification/SPEC.md#the-horn-rules-with-individuals)
argues. On `atlas` four rounds of alternating order compared the base, commit e2709fb7,
with the item's build, commit 3d2d4032, one process per side, file and thread count with
five classifications each. Each time below is the median of the 20 classifications of a
side, each ratio the median of the four rounds' ratios, and the conjunction contexts and
conclusions are the medians of the four processes' reports. The four files are those with
7,856 to 114,957 property assertions of [the Horn rules with individuals](#horn-individuals).

| Ontology | Threads | Base ms | Item 869 ms | Ratio | Range of the rounds | Conjunctions before | Conjunctions after | Conclusions before | Conclusions after |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|
| `ore_ont_4510` | 1 | 944.8 | 405.7 | 0.430 | 0.421 to 0.431 | 42,249 | 116 | 19,150,095 | 6,076,004 |
| `ore_ont_4510` | 4 | 432.6 | 215.1 | 0.529 | 0.451 to 0.567 | 42,249 | 116 | 19,150,563 | 6,092,183 |
| `ore_ont_12901` | 1 | 616.3 | 300.8 | 0.490 | 0.469 to 0.498 | 29,993 | 44 | 12,716,227 | 4,822,498 |
| `ore_ont_12901` | 4 | 283.4 | 172.8 | 0.625 | 0.538 to 0.666 | 29,993 | 44 | 12,716,343 | 4,834,237 |
| `ore_ont_14375` | 1 | 111.4 | 50.4 | 0.454 | 0.449 to 0.458 | 5,678 | 109 | 2,467,393 | 881,264 |
| `ore_ont_14375` | 4 | 56.3 | 32.6 | 0.555 | 0.493 to 0.687 | 5,678 | 109 | 2,467,436 | 886,393 |
| `ore_ont_10951` | 1 | 66.8 | 25.4 | 0.378 | 0.378 to 0.380 | 3,098 | 74 | 1,587,086 | 444,256 |
| `ore_ont_10951` | 4 | 35.9 | 17.5 | 0.468 | 0.454 to 0.615 | 3,116 | 71 | 1,619,795 | 445,703 |

The peak resident set of a process falls with the contexts, from 476 to 385 MiB on
`ore_ont_4510` on one thread and from 373 to 307 MiB on `ore_ont_12901`. Each round also
ran the item's build with the EL rules alone on the four files, which leave the Horn
axioms out. The ratio before is the base's time against the same runs.

| Ontology | Threads | EL rules ms | Item 869 ms | Ratio | Range of the rounds | Ratio before | Conclusions of the EL rules | Conclusions after |
|---|---:|---:|---:|---:|---|---:|---:|---:|
| `ore_ont_4510` | 1 | 185.9 | 405.7 | 2.18 | 2.16 to 2.25 | 5.10 | 2,427,529 | 6,076,004 |
| `ore_ont_4510` | 4 | 121.3 | 215.1 | 1.81 | 1.59 to 2.05 | 3.57 | 2,427,529 | 6,092,183 |
| `ore_ont_12901` | 1 | 144.6 | 300.8 | 2.08 | 2.01 to 2.09 | 4.25 | 1,925,362 | 4,822,498 |
| `ore_ont_12901` | 4 | 99.2 | 172.8 | 1.77 | 1.72 to 1.87 | 2.87 | 1,925,362 | 4,834,237 |
| `ore_ont_14375` | 1 | 25.3 | 50.4 | 2.00 | 1.98 to 2.02 | 4.39 | 364,763 | 881,264 |
| `ore_ont_14375` | 4 | 18.9 | 32.6 | 1.72 | 1.42 to 1.83 | 3.00 | 364,763 | 886,393 |
| `ore_ont_10951` | 1 | 11.9 | 25.4 | 2.12 | 2.11 to 2.13 | 5.60 | 176,238 | 444,256 |
| `ore_ont_10951` | 4 | 10.3 | 17.5 | 1.68 | 1.47 to 1.87 | 3.36 | 176,238 | 445,703 |

At the build of item 869 the Horn rules took 2.0 to 2.2 times the EL rules' time on one
thread and 1.7 to 1.8 times on four, and derived 2.4 to 2.5 times the conclusions, where
the base derived 6.6 to 9.2 times as many. [Assertions under a range](#assertions-under-a-range)
gives the times since item 1025, which follows what remains.

The item also makes the Horn normalization pass over an assertion about an anonymous
individual before it opens a scope for it. The reader leaves such axioms out, and the
four files below hold 243,972 to 457,662 of them.

| Ontology | Threads | Index before ms | Index after ms | Classification before ms | Classification after ms | Ratio | Range of the rounds |
|---|---:|---:|---:|---:|---:|---:|---|
| `ore_ont_5481` | 1 | 989.4 | 744.4 | 1,050.8 | 808.0 | 0.774 | 0.767 to 0.806 |
| `ore_ont_5481` | 4 | 973.5 | 723.4 | 1,004.7 | 752.9 | 0.752 | 0.745 to 0.754 |
| `ore_ont_10073` | 1 | 988.4 | 741.8 | 1,050.3 | 807.1 | 0.770 | 0.767 to 0.771 |
| `ore_ont_10073` | 4 | 969.7 | 718.9 | 1,005.2 | 749.8 | 0.746 | 0.743 to 0.748 |
| `ore_ont_1370` | 1 | 572.3 | 409.4 | 637.7 | 474.6 | 0.743 | 0.736 to 0.748 |
| `ore_ont_1370` | 4 | 544.3 | 405.8 | 587.8 | 429.0 | 0.730 | 0.718 to 0.745 |
| `ore_ont_4755` | 1 | 506.8 | 370.7 | 600.4 | 448.4 | 0.748 | 0.747 to 0.758 |
| `ore_ont_4755` | 4 | 486.4 | 340.3 | 531.2 | 375.6 | 0.711 | 0.701 to 0.716 |

The index stage takes 136 to 251 ms less, and the processes peak at 356 to 586 MiB on
one thread against 472 to 702 MiB. The conclusions do not change.

The same rounds ran two files of the development set with witnesses, whose worlds take up to
15 and 17 ms and which take the disjunctive rules, and four files in which no conjunction
holds an individual. `ore_ont_8737` had two classifications per process.

| Ontology | Threads | Base ms | Item 869 ms | Ratio | Range of the rounds |
|---|---:|---:|---:|---:|---|
| `ore_ont_11378` | 1 | 146.3 | 142.7 | 0.974 | 0.972 to 0.978 |
| `ore_ont_11378` | 4 | 104.1 | 94.1 | 0.912 | 0.799 to 0.945 |
| `ore_ont_9151` | 1 | 266.2 | 254.9 | 0.958 | 0.953 to 0.959 |
| `ore_ont_9151` | 4 | 214.8 | 204.1 | 0.966 | 0.754 to 1.041 |
| GO | 1 | 302.9 | 306.6 | 1.009 | 1.005 to 1.023 |
| GO | 4 | 122.3 | 122.4 | 0.993 | 0.954 to 1.041 |
| ChEBI lite | 1 | 936.7 | 926.3 | 0.989 | 0.988 to 0.993 |
| ChEBI lite | 4 | 432.7 | 423.2 | 0.957 | 0.921 to 1.047 |
| `ore_ont_9855` | 1 | 1,470.6 | 1,479.5 | 1.006 | 1.005 to 1.009 |
| `ore_ont_9855` | 4 | 510.8 | 521.3 | 1.021 | 0.971 to 1.036 |
| `ore_ont_8737` | 1 | 30,981.6 | 31,157.7 | 1.001 | 0.989 to 1.074 |
| `ore_ont_8737` | 4 | 9,695.2 | 9,589.9 | 0.992 | 0.952 to 1.032 |

`ore_ont_11378` and `ore_ont_9151` make 112 and 195 conjunction contexts against 211 and
356. GO, ChEBI lite and `ore_ont_8737` derive the same conclusions as the base on one
thread, and `ore_ont_9855` derives 21,081,967 against 21,156,356. GO and `ore_ont_9855`
are 0.5 to 2.3% and 0.5 to 0.9% slower on one thread in every round, which roadmap item
991 follows with the other costs of the Horn rules on files they do not prune. On four
threads the peak resident set of `ore_ont_8737` follows the order of delivery. It is 5.2
to 5.9 GiB over the base's four processes and 5.4 to 7.0 GiB over the item's, with 1.23 to
1.35 billion conclusions, and on one thread both builds derive 1,619,753,615 conclusions
and peak at 3.8 GiB. A process that classifies `ore_ont_11378` five times on one thread
peaks at 195 MiB against 141 MiB, while its estimate falls from 90.1 to 89.4 MB. A process
that classifies it once peaks at 127 MiB on both builds on the laptop, and on four threads
the peaks are 166 to 184 MiB against 167 to 195 MiB.

`ore_ont_14817` and `ore_ont_9663` have no individuals and take the disjunctive rules
first, whose attempt passes its budget before the Horn rules answer. The item does not
change the disjunctive rules, and the attempt's length follows the order of delivery.
Sixteen processes of each build on four threads, the four rounds above and twelve more of
alternating order, gave the classification times below, the attempt included. The Horn
rules' saturation takes 0.7 to 0.9 s of them on `ore_ont_14817` and 1.2 to 1.6 s on
`ore_ont_9663` on both builds.

| Ontology | Base s, median | Base s, range | Item 869 s, median | Item 869 s, range | Median of the rounds' ratios | Rounds in which the item's build is faster |
|---|---:|---|---:|---|---:|---|
| `ore_ont_14817` | 29.8 | 20.1 to 46.9 | 32.6 | 12.9 to 64.9 | 0.971 | 8 of 16 |
| `ore_ont_9663` | 13.5 | 11.6 to 21.0 | 14.8 | 12.5 to 24.9 | 1.097 | 5 of 16 |

On one thread, which fixes the order, both builds classify `ore_ont_14817` with the same
29,062,079 conclusions, 70,702 conjunction contexts and peak estimate, the item's build in
347.5 s and the base in 340.8 s, one process each. The spreads on four threads overlap on
both files and do not separate the builds. Roadmap item 990 follows the attempt, and its
row keeps these times.

A sweep on the laptop classified GO, ChEBI lite, the 298 ORE 2015 EL files and the 207
files of the DL development set, 499 distinct files, with both builds on one and eight
threads with its taxonomy, in a 10 GiB scope with a timeout of 300 s. Eight files are in
both sets and ran twice. The 991 pairs of a
file and a thread count that both builds classified gave the same bytes. The reader
refuses `ore_ont_9881`. `ore_ont_14817`, `ore_ont_9663` and `ore_ont_3215` pass the timeout
on one thread on both builds. With a timeout of 900 s on `atlas` both builds classify
`ore_ont_14817` and `ore_ont_3215` on one thread with the taxonomies they give on eight,
`ore_ont_3215` in 628.8 s for the item's build and 633.5 s for the base. `ore_ont_7192`
passes the 10 GiB scope on eight threads on both builds, and on one thread the base passes
the timeout and the item's build the scope. In larger scopes on the laptop both builds
classify it on one thread with the same taxonomy and 80,580,920 conclusions, after the
Horn rules pass their budget. The item's build took 405 s with a peak resident set of
11.3 GiB and the base 875 s with 13.0 GiB, one process each at different loads of the
laptop, so the two times are not a comparison.

### Assertions under a range

Roadmap item 1025 gives the restriction of a property assertion over a property with a
range the individual's own context as its successor, where the index made a context of
the individual with the range. Item 1026 makes a flush of the Horn rules merge the
successors of one at-most restriction at a time.
[F24](../../specs/F24-owl-classification/SPEC.md#outcome) records both. On `atlas` four
rounds of alternating order compared the base, commit 7506edf5, with the build of both
items, commit c748d5d5, one process per side, file and thread count with five
classifications each. Each time below is the median of the 20 classifications of a side
and each ratio the median of the four rounds' ratios. The contexts and conclusions are
those of one thread, where the four processes of a side report the same counts, since on
four threads the counts follow the order of delivery. The four files are those with 7,856
to 114,957 property assertions of [the Horn rules with individuals](#horn-individuals).

| Ontology | Threads | Base ms | Items 1025 and 1026 ms | Ratio | Range of the rounds | Contexts before | Contexts after | Conclusions before | Conclusions after |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|
| `ore_ont_4510` | 1 | 401.1 | 239.7 | 0.600 | 0.591 to 0.602 | 73,910 | 60,363 | 6,076,004 | 2,477,970 |
| `ore_ont_4510` | 4 | 212.9 | 148.4 | 0.723 | 0.643 to 0.755 | | | | |
| `ore_ont_12901` | 1 | 302.1 | 180.6 | 0.599 | 0.595 to 0.600 | 60,816 | 49,359 | 4,822,498 | 1,914,884 |
| `ore_ont_12901` | 4 | 168.0 | 122.0 | 0.719 | 0.654 to 0.783 | | | | |
| `ore_ont_14375` | 1 | 50.3 | 31.8 | 0.632 | 0.631 to 0.635 | 11,135 | 9,260 | 881,264 | 382,067 |
| `ore_ont_14375` | 4 | 27.4 | 25.2 | 0.916 | 0.798 to 0.948 | | | | |
| `ore_ont_10951` | 1 | 25.3 | 15.4 | 0.607 | 0.602 to 0.610 | 5,845 | 4,867 | 444,256 | 190,519 |
| `ore_ont_10951` | 4 | 16.7 | 13.4 | 0.845 | 0.717 to 1.001 | | | | |

The saturation of `ore_ont_4510` takes 152.0 against 329.2 ms on one thread and its
index stage 75.9 against 76.7 ms. The processes of the four files peak at 353, 309, 87
and 49 MiB on one thread against 385, 307, 93 and 79 MiB.

The same rounds ran the EL rules alone on the four files with both builds, with
`--el-only`. The last columns divide the time of a build's Horn rules by the time of its
EL rules in each round.

| Ontology | Threads | Base EL ms | EL ms | Ratio | Range of the rounds | Horn over EL, base | Horn over EL | Range of the rounds | EL conclusions before | EL conclusions after |
|---|---:|---:|---:|---:|---|---:|---:|---|---:|---:|
| `ore_ont_4510` | 1 | 187.3 | 123.0 | 0.657 | 0.631 to 0.659 | 2.15 | 1.96 | 1.93 to 1.96 | 2,427,529 | 1,145,939 |
| `ore_ont_4510` | 4 | 123.1 | 78.3 | 0.630 | 0.618 to 0.674 | 1.72 | 1.88 | 1.69 to 2.10 |  |  |
| `ore_ont_12901` | 1 | 144.9 | 94.0 | 0.646 | 0.644 to 0.652 | 2.08 | 1.93 | 1.91 to 1.94 | 1,925,362 | 889,182 |
| `ore_ont_12901` | 4 | 100.2 | 62.0 | 0.625 | 0.591 to 0.637 | 1.69 | 2.00 | 1.66 to 2.02 |  |  |
| `ore_ont_14375` | 1 | 25.3 | 17.8 | 0.707 | 0.696 to 0.714 | 1.99 | 1.79 | 1.78 to 1.79 | 364,763 | 186,103 |
| `ore_ont_14375` | 4 | 19.9 | 13.4 | 0.677 | 0.662 to 0.713 | 1.37 | 1.88 | 1.70 to 2.02 |  |  |
| `ore_ont_10951` | 1 | 12.0 | 8.0 | 0.672 | 0.668 to 0.674 | 2.12 | 1.92 | 1.90 to 1.92 | 176,238 | 85,225 |
| `ore_ont_10951` | 4 | 10.2 | 8.2 | 0.853 | 0.716 to 0.932 | 1.55 | 1.50 | 1.40 to 1.89 |  |  |

On one thread the Horn rules take 1.79 to 1.96 times the EL rules' time, against 1.99 to
2.15 times on the base, and derive 2.05 to 2.24 times the EL rules' conclusions, against
2.42 to 2.52 times. On four threads the EL rules gain more than the Horn rules on three
of the four files, whose ratio of the Horn rules to the EL rules rises from 1.37 to 1.72
to 1.88 to 2.00, and `ore_ont_10951` goes from 1.55 to 1.50. On `ore_ont_4510` on one
thread the Horn rules derive 2,477,970 conclusions against 1,145,939 for the EL rules. A
diagnostic build on the laptop counted the difference as five subsumers that every
context takes from `owl:Thing`, 300 thousand conclusions, the two conclusions that tell
a successor it is real, 548 thousand, the states of the automata, 103 thousand, 218
thousand further repeats of existential restrictions and 40 thousand successors. Roadmap
item 1025 follows them.

The rounds also ran seven files of the development set whose conclusions the sweep below
found changed. `ore_ont_9654` had two classifications per process.

| Ontology | Threads | Base ms | Items 1025 and 1026 ms | Ratio | Range of the rounds | Contexts before | Contexts after | Conclusions before | Conclusions after |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|
| `ore_ont_9654` | 1 | 5,401.2 | 3,072.9 | 0.569 | 0.563 to 0.639 | 145,464 | 120,170 | 101,000,254 | 45,903,674 |
| `ore_ont_9654` | 4 | 1,677.9 | 1,046.0 | 0.629 | 0.550 to 0.668 | | | | |
| `ore_ont_4572` | 1 | 369.4 | 248.7 | 0.673 | 0.671 to 0.676 | 57,347 | 33,028 | 3,728,211 | 1,517,836 |
| `ore_ont_4572` | 4 | 302.5 | 243.5 | 0.809 | 0.751 to 0.913 | | | | |
| `ore_ont_6132` | 1 | 48.0 | 21.6 | 0.449 | 0.447 to 0.450 | 5,820 | 3,841 | 894,166 | 308,111 |
| `ore_ont_6132` | 4 | 29.2 | 15.0 | 0.517 | 0.503 to 0.584 | | | | |
| `ore_ont_10754` | 1 | 23.5 | 12.0 | 0.510 | 0.507 to 0.525 | 5,895 | 3,271 | 411,497 | 135,584 |
| `ore_ont_10754` | 4 | 14.1 | 9.7 | 0.686 | 0.632 to 0.769 | | | | |
| `ore_ont_11378` | 1 | 142.9 | 115.9 | 0.811 | 0.805 to 0.814 | 12,442 | 11,699 | 1,485,418 | 1,217,922 |
| `ore_ont_11378` | 4 | 101.2 | 80.8 | 0.805 | 0.681 to 0.818 | | | | |
| `ore_ont_9151` | 1 | 255.6 | 214.7 | 0.840 | 0.840 to 0.841 | 17,040 | 16,019 | 2,342,780 | 1,946,893 |
| `ore_ont_9151` | 4 | 197.0 | 154.9 | 0.820 | 0.664 to 0.874 | | | | |
| `ore_ont_9855` | 1 | 1,479.3 | 1,459.2 | 0.987 | 0.975 to 0.988 | 22,739 | 22,647 | 21,081,967 | 20,811,316 |
| `ore_ont_9855` | 4 | 526.1 | 521.5 | 0.986 | 0.966 to 1.008 | | | | |

On the laptop a build of item 1025 alone, commit 07820015, reports the counts of the
build of both items on the first six files and the base's counts on `ore_ont_9855`, so
the six change with the successor of an assertion and `ore_ont_9855`, which makes 15,774
conjunction contexts against 15,866, with the flush.

The files below derive the same conclusions on one thread on both builds. `ore_ont_16744`
had one classification per process and `ore_ont_8737` two.

| Ontology | Threads | Base ms | Items 1025 and 1026 ms | Ratio | Range of the rounds |
|---|---:|---:|---:|---:|---|
| GO | 1 | 302.4 | 309.4 | 1.023 | 1.020 to 1.023 |
| GO | 4 | 121.2 | 122.8 | 1.007 | 0.953 to 1.059 |
| ChEBI lite | 1 | 925.3 | 938.6 | 1.009 | 1.006 to 1.022 |
| ChEBI lite | 4 | 424.5 | 438.3 | 1.021 | 0.991 to 1.057 |
| `ore_ont_16744` | 1 | 9,066.1 | 9,206.0 | 1.015 | 1.002 to 1.023 |
| `ore_ont_16744` | 4 | 3,185.4 | 3,144.3 | 0.992 | 0.960 to 1.014 |
| `ore_ont_8737` | 1 | 30,591.2 | 30,772.7 | 1.007 | 0.999 to 1.011 |
| `ore_ont_8737` | 4 | 9,817.3 | 9,723.2 | 0.959 | 0.951 to 1.023 |
| `ore_ont_1370` | 1 | 472.3 | 480.0 | 1.016 | 1.007 to 1.065 |
| `ore_ont_1370` | 4 | 431.9 | 437.2 | 1.016 | 0.998 to 1.023 |
| `ore_ont_4755` | 1 | 446.3 | 447.8 | 1.003 | 0.993 to 1.012 |
| `ore_ont_4755` | 4 | 378.3 | 380.1 | 0.996 | 0.981 to 1.018 |
| `ore_ont_5481` | 1 | 808.9 | 811.6 | 1.001 | 1.000 to 1.071 |
| `ore_ont_5481` | 4 | 756.7 | 761.2 | 1.009 | 0.998 to 1.016 |
| `ore_ont_10073` | 1 | 811.0 | 810.1 | 0.999 | 0.995 to 1.007 |
| `ore_ont_10073` | 4 | 754.1 | 758.4 | 1.005 | 1.004 to 1.014 |

On one thread GO is slower in every round, by 2.0 to 2.3%, and so are `ore_ont_1370`,
`ore_ont_16744` and ChEBI lite, by 0.7 to 6.5%, 0.2 to 2.3% and 0.6 to 2.2%. On four
threads `ore_ont_10073` is slower in every round, by 0.4 to 1.4%. Every other row has
rounds on both sides of 1. On one thread GO's saturation takes 247.4 against 240.7 ms,
`ore_ont_16744`'s 8.18 against 8.02 s and ChEBI lite's 569.8 against 560.4 ms. On
`ore_ont_1370` the index stage takes most of the difference, 414.2 against 408.1 ms,
beside a saturation of 43.9 against 43.2 ms. A run on the laptop pinned to one
performance core counted the instructions of a whole process that reads the file and
classifies it five times on one thread. On GO the build of item 1025 alone and the build
of both items ran 27.55 billion against 27.93 billion on the base, and on ChEBI lite
56.61 billion against 55.92 billion, two processes of each build within 0.01% of each
other. The flush of item 1026 therefore adds no instructions on these files. The counts
move in both directions from the commit of item 1025, which adds no rule to the
saturation, so its build may generate other code for the rules it leaves alone. The
rounds on `atlas` do not time the two commits apart, so the cause of the slowdown there
is not shown. Roadmap item 1028 follows it.

Item 1025 also timed `ore_ont_9663` and `ore_ont_14817` on the build before item 869,
commit be684ce5, against the base, which adds item 869 to it, now that the bound of item
990 ends their disjunctive attempt. Sixteen rounds of alternating order ran one process a
side with one classification on four threads, and four more rounds ran `ore_ont_9663` on
one thread.

| Ontology | Threads | Before item 869 s, median | Before item 869 s, range | With item 869 s, median | With item 869 s, range | Median of the rounds' ratios | Rounds in which the build with item 869 is slower |
|---|---:|---:|---|---:|---|---:|---|
| `ore_ont_9663` | 4 | 8.17 | 7.43 to 9.45 | 8.75 | 7.84 to 10.72 | 1.071 | 12 of 16 |
| `ore_ont_9663` | 1 | 75.76 | 75.70 to 75.83 | 76.27 | 76.09 to 76.40 | 1.007 | 4 of 4 |
| `ore_ont_14817` | 4 | 7.99 | 6.70 to 13.10 | 8.08 | 7.05 to 10.26 | 1.013 | 8 of 16 |

Every process gave the taxonomy of its file that the other build gave. On one thread
both builds derive 50,878,012 conclusions on `ore_ont_9663`. On four threads the Horn
rules' final saturation takes a median of 1.26 against 1.21 s on `ore_ont_9663`, and the
time outside it 7.52 against 6.92 s, with 60.1 against 59.1 million conclusions that
follow the order of delivery. That time holds the attempt of the disjunctive rules up to
the bound and the index and taxonomy stages, which take about 0.1 s of it, and the
record does not time the attempt apart. Item 869 does not change the disjunctive rules,
so most of the slowdown of `ore_ont_9663` is in code that the item leaves alone, and
roadmap item 1027 follows it. The rounds of
`ore_ont_14817` do not separate the builds.

A sweep on the laptop classified GO, ChEBI lite, the 298 ORE 2015 EL files and the 207
files of the DL development set, 499 distinct files, with the base and the build of both
items on one and eight threads with its taxonomy, in an 8 GiB scope with a timeout of
300 s. The 994 pairs of a file and a thread count that both builds classified gave the
same bytes, and no file changed its tier. The reader refuses `ore_ont_9881`, and
`ore_ont_7192` passes the scope on both builds on one and on eight threads.
In a 14 GiB scope both builds classify `ore_ont_7192` on one thread with the same
taxonomy and 80,580,920 conclusions, with the Horn rules past their budget on both,
the outcome that another order of merges could move. The base took 184 s and the build
of both items 140 s, one process each at different loads of the laptop, so the two times
are not a comparison.
On one thread 57 files of the development set change their conclusions. Of them 52
derive fewer, `ore_ont_8941` 34,142 against 180,033, and 5 derive up to 0.6% more. GO,
ChEBI lite and the EL files derive the same.

<a id="reduced-automata"></a>

### Reduced automata of the Horn rules

Roadmap item 693 reduces each automaton before a run with the Horn rules unfolds a
universal or a negative existential restriction by it, as
[F24](../../specs/F24-owl-classification/SPEC.md#the-horn-rules) describes.
`SPARKLES_HORN_AUTOMATA` selects the reductions. Level 0 applies none, level 2 merges the
states that accept the same words and drops the transitions that a superproperty covers,
and level 3, the default, also drops the transitions by properties that no link can have.
On forge six rounds of alternating order compared the base, commit 7e272f4d, with the
build of the reductions, commit 7d064abd, at levels 0, 2 and 3, one process per side, file
and thread count. A process classified five times, twice on `ore_ont_8737` and
`ore_ont_9654` and once on `ore_ont_16744`. Each time below is the median over the six
processes of a side of the process's median classification, each ratio divides level 3's
time by the base's, and the range is that of the six rounds' own ratios. The memory is the
median peak resident set of a side's processes. The conclusions are those of one thread,
where the six processes of a side report the same count. Every taxonomy has the same hash
on the four sides in every round.

| Ontology | Threads | Base ms | Level 0 ms | Level 2 ms | Level 3 ms | Level 3 / base | Range of the rounds | Base MiB | Level 3 MiB | Conclusions, base | Conclusions, level 3 |
|---|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|---:|
| GO | 1 | 309 | 312 | 312 | 311 | 1.01 | 1.00 to 1.02 | 247 | 247 | 2,946,303 | 2,947,343 |
| | 4 | 124 | 121 | 123 | 123 | 0.99 | 0.94 to 1.04 | 272 | 276 | | |
| ChEBI lite | 1 | 933 | 932 | 931 | 932 | 1.00 | 0.99 to 1.02 | 671 | 671 | 26,995,717 | 26,995,717 |
| | 4 | 428 | 428 | 425 | 437 | 1.02 | 0.93 to 1.08 | 765 | 788 | | |
| `ore_ont_14817` | 1 | 58,433 | | | 59,904 | 1.03 | 1.01 to 1.03 | 1,695 | 1,694 | 29,062,079 | 22,090,247 |
| | 4 | 7,687 | | | 8,274 | 1.08 | 0.91 to 1.46 | 2,111 | 2,198 | | |
| `ore_ont_9663` | 1 | 75,337 | | | 76,682 | 1.02 | 1.01 to 1.02 | 1,385 | 1,380 | 50,878,012 | 35,611,566 |
| | 4 | 8,576 | | | 8,188 | 0.95 | 0.93 to 1.05 | 3,117 | 3,115 | | |
| `ore_ont_9654` | 1 | 3,088 | 3,064 | 1,431 | 762 | 0.25 | 0.21 to 0.25 | 1,359 | 665 | 45,903,674 | 9,576,942 |
| | 4 | 1,096 | 1,070 | 625 | 449 | 0.41 | 0.33 to 0.46 | 1,417 | 689 | | |
| `ore_ont_9855` | 1 | 1,460 | 1,458 | 931 | 930 | 0.64 | 0.63 to 0.64 | 371 | 247 | 20,811,316 | 10,859,414 |
| | 4 | 517 | 525 | 348 | 347 | 0.67 | 0.64 to 0.72 | 500 | 370 | | |
| `ore_ont_6132` | 1 | 21.6 | 21.7 | 21.9 | 20.3 | 0.94 | 0.93 to 0.96 | 75 | 77 | 308,111 | 277,366 |
| | 4 | 14.9 | 15.3 | 15.1 | 14.9 | 1.00 | 0.94 to 1.04 | 109 | 111 | | |
| `ore_ont_4510` | 1 | 240 | 241 | 241 | 237 | 0.98 | 0.97 to 1.02 | 353 | 353 | 2,477,970 | 2,357,244 |
| | 4 | 157 | 157 | 154 | 155 | 0.99 | 0.82 to 1.00 | 376 | 376 | | |
| `ore_ont_12901` | 1 | 180 | 182 | 182 | 177 | 0.98 | 0.97 to 1.02 | 309 | 309 | 1,914,884 | 1,816,166 |
| | 4 | 124 | 123 | 124 | 118 | 0.96 | 0.83 to 0.98 | 337 | 339 | | |
| `ore_ont_14375` | 1 | 32.0 | 32.3 | 32.3 | 31.2 | 0.97 | 0.97 to 0.98 | 87 | 95 | 382,067 | 363,547 |
| | 4 | 25.5 | 25.1 | 25.3 | 25.0 | 0.98 | 0.82 to 1.17 | 143 | 143 | | |
| `ore_ont_10951` | 1 | 15.3 | 15.5 | 15.6 | 15.0 | 0.98 | 0.97 to 0.99 | 49 | 49 | 190,519 | 180,785 |
| | 4 | 14.1 | 12.9 | 13.2 | 13.2 | 0.94 | 0.83 to 1.35 | 107 | 95 | | |
| `ore_ont_8737` | 1 | 30,594 | 30,706 | 30,640 | 30,839 | 1.01 | 1.00 to 1.12 | 3,877 | 3,885 | 1,619,753,615 | 1,619,753,615 |
| | 4 | 9,652 | 10,004 | 9,910 | 10,048 | 1.04 | 0.95 to 1.08 | 5,955 | 5,760 | | |
| `ore_ont_16744` | 1 | 9,191 | 9,168 | 9,092 | 9,126 | 0.99 | 0.99 to 1.02 | 2,505 | 2,496 | 592,208,319 | 592,208,319 |
| | 4 | 3,199 | 3,105 | 3,144 | 3,115 | 0.97 | 0.92 to 1.03 | 2,874 | 2,878 | | |
| `ore_ont_10073` | 1 | 807 | 806 | 805 | 808 | 1.00 | 0.98 to 1.05 | 585 | 585 | 1,704,715 | 1,704,715 |
| | 4 | 755 | 754 | 751 | 753 | 1.00 | 0.99 to 1.02 | 596 | 590 | | |
| `ore_ont_1370` | 1 | 478 | 473 | 472 | 472 | 0.99 | 0.98 to 1.03 | 359 | 359 | 2,074,571 | 2,074,571 |
| | 4 | 436 | 427 | 429 | 427 | 0.98 | 0.93 to 0.99 | 397 | 407 | | |
| `ore_ont_4572` | 1 | 248 | 250 | 250 | 249 | 1.00 | 0.98 to 1.03 | 365 | 365 | 1,517,836 | 1,517,836 |
| | 4 | 245 | 243 | 244 | 242 | 0.99 | 0.92 to 1.05 | 427 | 418 | | |
| `ore_ont_10754` | 1 | 12.0 | 12.0 | 12.0 | 12.1 | 1.00 | 0.99 to 1.02 | 53 | 53 | 135,584 | 135,584 |
| | 4 | 9.8 | 10.2 | 9.3 | 9.8 | 0.99 | 0.92 to 1.18 | 88 | 88 | | |

`ore_ont_14817` and `ore_ont_9663`, the two Uberon files, ran their default pipeline on
the base and at level 3 only, with one classification per process. Nearly all of that
time is the bounded attempt of the disjunctive rules, which ends past its bound before
the Horn rules answer. The same rounds ran the two files on all four sides with
`--dl-attempt-work 1`, which ends the attempt at once and so times the Horn rules alone,
with three classifications per process.

| Ontology | Threads | Base ms | Level 0 ms | Level 2 ms | Level 3 ms | Level 3 / base | Range of the rounds | Base MiB | Level 3 MiB | Conclusions, base | Conclusions, level 3 |
|---|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|---:|
| `ore_ont_9663` | 1 | 3,663 | 3,671 | 3,189 | 2,742 | 0.75 | 0.69 to 0.75 | 1,215 | 951 | 50,878,012 | 35,611,566 |
| | 4 | 1,420 | 1,366 | 1,235 | 1,070 | 0.75 | 0.68 to 0.77 | 1,462 | 1,157 | | |
| `ore_ont_14817` | 1 | 2,495 | 2,487 | 2,186 | 1,995 | 0.80 | 0.75 to 0.81 | 808 | 713 | 29,062,079 | 22,090,247 |
| | 4 | 920 | 928 | 836 | 779 | 0.85 | 0.81 to 0.94 | 991 | 832 | | |

Without the attempt the Horn rules classify `ore_ont_14817` in 0.80 times the base's time
on one thread and 0.85 times on four, and `ore_ont_9663` in 0.75 times on both, with peak
resident sets of 713 against 808 MiB and 951 against 1,215 MiB on one thread. Level 2
gives 0.88 and 0.87 on one thread, so the dead properties account for about two fifths
of the gain on the first file and half on the second. `ore_ont_9654` classifies in 0.25 times
the base's time on one thread and 0.41 times on four, with 9.6 against 45.9 million
conclusions and a peak resident set of 665 against 1,359 MiB, and level 2 alone gives
0.46 and 0.57. `ore_ont_9855` classifies in 0.64 and 0.67 times the base's time with
10.9 against 20.8 million conclusions and 247 against 371 MiB, all of it at level 2.
`ore_ont_6132` and the four files of [assertions under a range](#assertions-under-a-range)
derive 5 to 10% fewer conclusions and take 0.94 and 0.97 to 0.98 times the base's time on
one thread. GO derives 1,040 conclusions more at levels 2 and 3 than its 2,946,303 on the
base, in the same time. On one thread level 0 is within 1.2% of the base on every file.

The default run of the two Uberon files shows neither gain. On one thread it takes 59.9
against 58.4 s on `ore_ont_14817` and 76.7 against 75.3 s on `ore_ont_9663`, slower in
each of the six rounds, while the Horn rules' final saturation takes 1.53 against 2.02 s
and 2.57 against 3.51 s. The time outside that saturation, which holds the attempt, is
therefore about 2.0 and 2.3 s longer, 3.5 and 3.2% of it. The attempt sets the peak
resident set, which stays at 1,694 and 1,380 MiB. The rounds have no level 0 side of the
default run, so they do not tell the reductions from the build. A pinned
process on the laptop executes the same instructions outside the Horn rules on the base
and at levels 0 and 3, 705.18, 705.22 and 705.20 billion on `ore_ont_14817`, and 21.4
against 26.9 billion for the Horn rules alone at level 3. The attempt therefore does the
same work, and the longer time on forge is not more of it.
On four threads the two files' rounds range from 0.91 to 1.46 and from 0.93 to 1.05 of
the base, so the medians of 1.08 and 0.95 are not effects.

The files whose conclusions the reductions leave alone take 0.99 to 1.01 times the base's
time on one thread. On `ore_ont_8737` on one thread the first round ran 9 to 22% longer than
the later ones on every side and the second 6 and 9% longer on the base and level 3, and
rounds three to six put level 3 within 1.2% of the base. On
four threads its three levels take 1.03 to 1.04 times the base's time, and level 0 is
slower than the base in each of the six rounds, so the difference belongs to the build
and not to the reductions. It lies within the base's own rounds, which range from 9.47
to 10.17 s. Roadmap item 1090 follows it with the Uberon files' time outside the
saturation. The peak resident set of `ore_ont_14375` on one thread is 95 against 87 MiB.

## Records

The [record of item 693's reduced automata](runs/2026-11-17-horn-automata-forge-7d064abd/record.json)
holds every classification of the six rounds on forge of the base and of the build at
levels 0, 2 and 3, the taxonomy hashes, the binary and data hashes, the pinned instruction
counts from the laptop and the scripts that ran.

The [record of items 1025 and 1026](runs/2026-11-17-horn-range-successors-atlas-c748d5d5/record.json)
holds every classification of the rounds on `atlas` of the base, the build of both items,
the prototype of the realness conclusions and the EL rules alone, the twenty rounds of
`ore_ont_9663` and `ore_ont_14817` on the builds before and with item 869, the sweep's
taxonomy hashes, the pair of `ore_ont_7192`, the counts by commit and the pinned
instruction counts from the laptop, the prototype's patch and the scripts that ran.

The [record of item 869](runs/2026-11-16-horn-nominal-successors-atlas-3d2d4032/record.json)
holds every classification of the rounds on `atlas`, the twelve further rounds and the
one-thread runs of `ore_ont_14817` and `ore_ont_3215`, the sweep's
taxonomy hashes, the scan of witnesses and worlds of item 868, the runs of `ore_ont_7192`
and `ore_ont_11378` on the laptop and the scripts that ran.

The [record of item 990](runs/2026-11-15-disjunctive-bound-atlas-0decd1de/record.json)
holds every classification of the rounds on `atlas` of both builds, the attempts that pass
the bound, the runs on `wally` with and without the bound, the attempts' conclusions on
the laptop with the diagnostic build, the generated ontologies and fixtures, the A/B of the
bound in steps, the sweep's taxonomy hashes and the scripts that ran.

The [record of items 710 and 969](runs/2026-11-15-horn-dominance-atlas-a370d073/record.json)
holds every classification of the rounds on `atlas` of the items' first build, of the build
with the cheaper sort before its charges were batched and of the final build, the disjunctive attempts of both builds,
the sweep's taxonomy hashes and the diagnostic notes from the laptop and the scripts that
ran.

The [record of item 962](runs/2026-11-14-horn-symmetric-atlas-78010ca4/record.json) holds
every classification of the timing rounds, the sweep's reports and taxonomy hashes on both
builds, the taxonomies of GO and ChEBI lite, the runs with and without the EL rules alone,
the rounds on GO and ChEBI lite of the reviewed build and the scripts that ran on `atlas`.

The [message cost record](runs/2026-11-14-el-messages-atlas-1da472f2/record.json) holds the
rounds of `ore_ont_8737` of every side on one, four and twelve threads, the rounds on GO,
ChEBI lite, `ore_ont_3836` and `ore_ont_13482` with their taxonomy hashes, the profiles
taken on `atlas`, the scripts that ran there, and the profiles, rounds and taxonomy sweeps
taken on the laptop.

The [link products record](runs/2026-11-10-el-threads-atlas-bd924609/record.json) holds the
runs of `ore_ont_8737` of every side on one, four and twelve threads, the Konclude runs, the
rounds on GO, ChEBI lite, `ore_ont_3836` and `ore_ont_13482` with their taxonomy hashes, the
scripts that ran on `atlas`, and the profiles and message counters taken on the laptop.

The [helper starts record](runs/2026-11-10-el-cost-atlas-5dca16bd/record.json) holds every
cold and warm time of the 12 small files, the rounds on GO, ChEBI lite, `ore_ont_3836`,
`ore_ont_13482` and `ore_ont_7914` on one, four and eight threads with their taxonomy
hashes, the instruction and cycle counts, the runs of `ore_ont_8737`, the variants that
measured no better and the scripts that ran on `atlas`.

The [small files and memory record](runs/2026-11-04-el-small-files-forge-47c13a51/record.json)
holds every cold and warm time of the 12 small files on one, four and eight threads, the
rounds on GO, ChEBI lite, `ore_ont_3836` and `ore_ont_13482` with their taxonomy hashes,
the run with the experimental one-thread worker, the rounds of six larger development
files and of the final commit, the runs of `ore_ont_8737` on `atlas`, the
taxonomy hashes of the identity sweep of 502 files on `atlas` and the scripts that ran.

The [code layout record](runs/2026-11-04-el-layout-forge-f93ad3cb/record.json) holds every
classification time of the gate, the screen with the reference builds, the pinned
eight-thread rounds and the pinned two-unit rounds, the taxonomy hashes, the scripts that
ran, and the build times and instruction counts taken on `atlas`.

The [record of item 693's A/B](runs/2026-11-03-horn-cost-forge-320d4708/record.json)
holds every classification time of the three sides, the stage times, the conjunction
contexts, the taxonomy hashes and the script that ran. The [record of item 603's
A/B](runs/2026-11-03-horn-forge-8966216c/record.json) holds the same for the Horn rules
as item 603 left them, against the build that item started from.

The [materialize record](runs/2026-11-03-materialize-forge-81851812/record.json) holds
every round of `materialize`, `reason` and the chain on GO and ChEBI lite, the binary
and data hashes, the comparison with ROBOT's materialize outputs and the warnings.

The [record of item 587's A/B](runs/2026-11-03-el-complete-forge-c4df06fe/record.json)
holds every classification time of both builds, the stage times, the taxonomy hashes and
the script that ran, and the [agreement record of item
587](runs/2026-11-03-el-complete-agreement-c4df06fe/record.json) holds the comparisons on
GO, ChEBI lite and the ORE 2015 sample and the reference reasoners' outputs on the 400
fixtures.

The [scaling record](runs/2026-11-03-el-scaling-forge-65a7591d/record.json) holds the
host, the binaries, harness and data hashes, every classification time of the build
before items 644, 645, 648 and 665, the build after them, ELK and Konclude on GO, ChEBI
lite and 13 ORE ontologies, the stage times, the taxonomy checks and the oracle's
reports. Its directory keeps every output line, the environment with the load when
each process started, and the script that ran.

The [run record of commit 80644aec](runs/2026-11-03-el-speed-forge-80644aec/record.json) holds the host,
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
