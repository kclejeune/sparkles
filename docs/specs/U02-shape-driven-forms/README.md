# U02: Editing forms generated from SHACL shapes

Forms edit literal and IRI values, nested blank-node children, SHACL 1.2 lists and
value-level `sh:or` and `sh:xone` alternatives, with previews, branch saves, conflict
handling, DASH hints, new instances and entity search. Each save is one guarded commit.
Checks discover shape-dependent targets beyond two hops and refuse incomplete admission.

## Documents

* [Design and acceptance](SPEC.md).
* [Delivery record](OUTCOME.md).
* [Provenance](SPEC.md#provenance).

## User guides

* [Editing forms](../../USAGE.md#editing-forms).
* [Forms API](../../API.md#editing-forms).
* [Capabilities and limits](../../visualization/README.md#web-ui).

Roadmap items 612 to 615 track node-level alternatives, complex nested
paths, blank-named graph contexts with model-source admission and the remaining
target-node expression producers. [Item 395](../../ROADMAP.md) supplies the controlled physical
readers required for stricter reader-budget coverage. Ordinary SHACL validation has its
own supported language and resource contract.
