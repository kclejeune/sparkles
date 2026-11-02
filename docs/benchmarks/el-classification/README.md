# OWL 2 EL classification

These measurements cover the OWL 2 EL classifier of `sparkles-owl` on two large
biomedical ontologies, the Gene Ontology and ChEBI, and its agreement with the hierarchy
those ontologies publish. The classifier and its limits are described in
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
whose asserted hierarchy also holds some redundant edges. Without its `--asserted` flag
the script compares a taxonomy with the functional-style output of ELK's command line,
`-c -o FILE`, over the classes both name.

## Reproducing a run

Download the GO release `go.owl` and the ChEBI release `chebi_lite.owl`, write their
functional-style forms with `el_classify --ofn-out FILE INPUT`, and run
`el_classify --threads N --repeat 5 --taxonomy OUT INPUT.ofn` for each thread count on a
quiet host. Each record's `run.sh.txt` is the exact script.
