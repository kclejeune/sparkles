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

## Records

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
