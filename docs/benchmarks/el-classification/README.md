# OWL 2 EL classification

These measurements cover the OWL 2 EL classifier of `sparkles-owl` on two large
biomedical ontologies, the Gene Ontology and ChEBI, its agreement with the hierarchy
those ontologies publish and with ELK, the Java EL reasoner, and its time and memory
beside ELK's. The classifier and its limits are described in
[F24](../../specs/F24-owl-classification/SPEC.md#outcome) and the
[reasoning guide](../../reasoning/README.md#owl-2-ontologies).

Read the [results](RESULTS.md) and the [run catalog](RUNS.json). The result report is a
manual import of the retained run records.

## What is measured

The harness is the example `crates/sparkles-owl/examples/el_classify.rs`, built with
`mbx build --release -p sparkles-owl --example el_classify`. It reads an ontology file,
classifies it with a fresh classifier `--repeat` times in one process and prints one JSON
line. The line gives the counts of axioms, classes, nodes and unsatisfiable classes, the
axioms outside OWL 2 EL and those used in part, the wall time of each classification, the
median, the times of indexing, saturation and the taxonomy in the last run, the number of
conclusions, the classifier's own memory estimate and the peak resident memory of the
process after reading and at the end. `--taxonomy FILE` writes the direct taxonomy as
sorted `EQ` and `SUB` lines, so that two runs or two reasoners can be compared.

A classification time starts with the axioms in memory and ends with the taxonomy built.
It includes normalization and indexing, saturation and the transitive reduction, and it
excludes reading the file. The peak resident memory includes the axioms read, which the
caller holds in any case, and so it is an upper bound for the classifier.

The inputs are functional-style files that `el_classify --ofn-out` wrote from each
release's RDF/XML file, with the annotation assertions left out. The classifier ignores
annotations, so both reasoners read the same logical axioms from a smaller file. The
records give each file's SHA-256.

## The ELK harness

`scripts/elk-compare/elk.sh` runs ELK 0.6.0 through OWL API 5.1.20. Its `fetch` step
downloads the jars that `scripts/elk-compare/pins.tsv` lists from Maven Central, checks
each against its pinned SHA-256 and Maven Central's SHA-1, and compiles
`scripts/elk-compare/ElkClassify.java`. Its `run` step starts the JVM with the heap of
`ELK_HEAP` as both `-Xms` and `-Xmx`, the G1 collector and a memory-capped systemd scope.
ElkClassify reads the same functional-style file as `el_classify`, refuses a file with
imports, and repeats a load and a classification `--repeat` times in one JVM. Each run
loads the file with a fresh OWL API manager, creates an ELK reasoner with `--threads`
workers and precomputes the class hierarchy. The JSON line it prints gives the load time
and the classification time of every run, the live heap after a full collection with the
reasoner still open, the peak resident memory of the process and the JVM's arguments.

ELK's classification time runs from the creation of the reasoner to the end of
`precomputeInferences`. It includes ELK's own reading of the axioms from the OWL API
ontology and the construction of its taxonomy, as the classification time of
`el_classify` includes indexing and the taxonomy. Reading the file is the OWL API's load
for ELK and `el_classify`'s parse for Sparkles, and both are reported apart. The first
run in a JVM includes the compilation of its hot code, so the tables give the median of
five runs in one process and the first run separately. The JVM's peak resident memory
follows its heap setting more than its live data, so the records also give the live
heap and a run with a smaller heap.

`--taxonomy` writes ELK's direct hierarchy in the functional-style form that
`scripts/el-oracle.py` reads, with the unsatisfiable classes in the node of
`owl:Nothing`. ELK stops at an inconsistent ontology, and the harness then writes one
node of every class, as the classifier does.

`--reasoner hermit` runs HermiT 1.4.5.519 in place of ELK over the same OWL API jars, with
the jars that `scripts/elk-compare/hermit-pins.tsv` pins. HermiT is a complete OWL 2 DL
reasoner, so it checks the entailments that an EL reasoner may miss on ontologies with
individuals.

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
