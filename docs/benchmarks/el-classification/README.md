# OWL 2 EL classification

These measurements cover the OWL 2 EL classifier of `sparkles-owl` on two large
biomedical ontologies, the Gene Ontology and ChEBI, and on the largest ontologies of the
ORE 2015 EL classification sample, its agreement with the hierarchy
those ontologies publish and with ELK, the Java EL reasoner, and its time and memory
beside ELK's. The classifier and its limits are described in
[F24](../../specs/F24-owl-classification/SPEC.md#outcome) and the
[reasoning guide](../../reasoning/README.md#owl-2-ontologies).

Read the [results](RESULTS.md), a
[manual report](../README.md#evidence-and-generated-reports) of the retained run records,
and the [run catalog](RUNS.json).

## What is measured

The harness is the example `crates/sparkles-owl/examples/el_classify.rs`, built with
`mbx build --release -p sparkles-owl --example el_classify`, with the server's mimalloc
allocator. It reads an ontology file, classifies it `--repeat` times in one process and
prints one JSON line. Each classification is a fresh `Saturation` that reads the axioms
in place, as the server's does from an open ontology, and `--owned` gives them instead
to a `Classifier` that keeps its own copy. The line gives the counts of axioms, classes, nodes and unsatisfiable classes, the
axioms outside OWL 2 EL and those used in part, the wall time of each classification, the
median, the times of indexing, saturation and the taxonomy in the last run, the number of
attempted conclusion applications, including duplicate messages, the classifier's own
memory estimate and its peak, and the peak resident
memory of the process after reading and at the end. `--heap` also counts the bytes that
the last classification held at once beyond the axioms read, which slows the run and
checks the estimate. `--taxonomy FILE` writes the direct taxonomy as
sorted `EQ` and `SUB` lines, so that two runs or two reasoners can be compared.

A classification time starts with the axioms in memory and ends with the taxonomy built.
It includes the collection of the axioms' references, normalization and indexing,
saturation and the transitive reduction, and it excludes reading the file. The peak resident memory includes the axioms read, which the
caller holds in any case, and so it is an upper bound for the classifier.

The inputs are functional-style files that `el_classify --ofn-out` wrote from each
release's RDF/XML file, with the annotation assertions left out. The classifier ignores
annotations, so both reasoners read the same logical axioms from a smaller file. The
records give each file's SHA-256.

An end-to-end time is the wall time of one `sparkles ontology classify FILE --format ofn`
process from its start to its exit, with the taxonomy written to a file, and its peak
resident memory is the process's. It includes reading the file, the selection of the
terminology, the classification and the output. A file classification compares two
builds in interleaved rounds that alternate which build runs first, through
`timed.py`, which records the wall time, the peak resident set and the exit status of
each process. The record keeps the outputs' comparison, which must find them byte for
byte equal.

## The reference reasoners

`scripts/elk-compare/elk.sh` runs the reference reasoners that `nix/reasoners` packages
from checksum-pinned releases. The flake builds them as `.#elk`, `.#hermit`, `.#whelk`,
`.#konclude` and `.#robot`. ELK 0.6.0 and HermiT 1.4.5.519 run on OWL API 5.1.20, and
Whelk 1.2.1 runs on OWL API 4.5.22 and Scala 2.13. Their jars come from Maven Central
and are checked against the SHA-256 sums of `nix/reasoners/elk.tsv`, `hermit.tsv` and
`whelk.tsv`. Konclude 0.7.0-1138 is the project's Linux x86_64 release binary, which links
Qt statically and is patched for Nix's C libraries, and it exists only on x86_64 Linux. ROBOT 1.9.11 is its release jar, which
bundles OWL API 4.5.29, ELK 0.6.0 and HermiT. Each Java package has a wrapper that names
nixpkgs' JDK 17, so a systemd unit with a minimal PATH runs it as it is.
`elk.sh path NAME` builds a package and prints its executable, and `ELK_CLASSIFY`,
`HERMIT_CLASSIFY`, `WHELK_CLASSIFY`, `KONCLUDE` and `ROBOT` name an executable built
elsewhere for a host that runs no Nix build.

`elk.sh run --reasoner elk|hermit|whelk|konclude` classifies a file. A JVM gets the heap
of `ELK_HEAP` as both `-Xms` and `-Xmx` and the G1 collector, and every reasoner runs in
a memory-capped systemd scope. The Java reasoners run
`scripts/elk-compare/ElkClassify.java`, which each package compiles against its own jars.
It reads the same functional-style file as `el_classify`, refuses a file with imports,
and repeats a load and a classification `--repeat` times in one JVM. Each run loads the
file with a fresh OWL API manager, creates the reasoner, with `--threads` workers for
ELK, and precomputes the class hierarchy. HermiT and Whelk classify on one thread. The
JSON line it prints gives the load time and the classification time of every run, the
live heap after a full collection with the reasoner still open, the peak resident memory
of the process and the JVM's arguments.

`--taxonomy` writes the reasoner's direct hierarchy in the functional-style form that
`scripts/el-oracle.py` reads, with the unsatisfiable classes in the node of
`owl:Nothing`. ELK stops at an inconsistent ontology, and the harness then writes one
node of every class, as the classifier does.

Konclude runs through `scripts/elk-compare/konclude.py`, one process per classification
with `-w` set to `--threads`. Its JSON line gives each process's wall time, which covers
the parse, the classification and the written hierarchy, the parse and total times that
Konclude's log reports and each process's peak resident memory. The script converts
Konclude's class hierarchy into the form above. It groups the classes by the cycles of
the subclass graph, since Konclude can write an equivalence as two subclass axioms, a
class equivalent to `owl:Thing` as `SubClassOf(owl:Thing A)` and an inconsistent
ontology as `SubClassOf(owl:Thing owl:Nothing)`.

HermiT and Konclude are complete OWL 2 DL reasoners, so they check the entailments that an
EL reasoner may miss on ontologies with individuals. Whelk is a second OWL 2 EL reasoner,
and ROBOT is the OWL API tool that OBO release pipelines classify with.

## Correctness evidence

The property tests in `crates/sparkles-owl/tests/classify.rs` compare the classifier with
an independent naive implementation of the EL++ completion rules on random ontologies,
and compare incremental runs with full runs after random additions and removals.

The Gene Ontology's release pipeline classifies the editors' file with ELK and writes the
direct inferred hierarchy back as asserted subclass axioms. The classifier's direct
hierarchy of the release is therefore compared with those asserted axioms.
`scripts/el-oracle.py --asserted ONTOLOGY.ofn TAXONOMY` counts the asserted subsumptions
between named classes that the taxonomy does not entail and the direct edges of the
taxonomy that are not asserted. Both are zero when the asserted hierarchy is exactly the
classifier's direct hierarchy, up to equivalent classes. The same check runs on ChEBI,
whose asserted hierarchy also holds some redundant edges.

Without its `--asserted` flag the script compares a taxonomy with ELK's, written by the
harness above or by ELK's command line with `-c -o FILE`. It reduces both to the same
partition of the classes into nodes and the direct edges between the nodes, and counts
the classes either side names alone, the classes whose nodes differ, the differences in
unsatisfiable classes and the missing and extra edges. The comparison covers GO, ChEBI
lite and all 298 ontologies of the ORE 2015 EL classification sample.

## Reproducing a run

Download the GO release `go.owl` and the ChEBI release `chebi_lite.owl`, write their
functional-style forms with `el_classify --ofn-out FILE INPUT`, and run
`el_classify --threads N --repeat 5 --taxonomy OUT INPUT.ofn` for each thread count on a
quiet host. For ELK, run `ELK_HEAP=4g scripts/elk-compare/elk.sh run --threads N
--repeat 5 --taxonomy OUT.ofn INPUT.ofn`, and compare the two taxonomies with
`scripts/el-oracle.py OUT.ofn OURS.txt`. Each record's `run.sh.txt` is the exact script.
The ORE 2015 sample is `ore2015_sample.zip` of the ORE 2015 Reasoner Competition
Corpus, Zenodo record 18578. The 298 ontologies that
`pool_sample/el/classification/fileorder.txt` lists are functional-style files under
`pool_sample/files`, which both tools read as they are.
