# U02: Editing forms generated from SHACL shapes

Forms are partially complete. Literal and IRI editors, previews, branch saves, conflict
handling, DASH hints, new instances and entity search are available. Checks discover
shape-dependent targets beyond two hops and refuse incomplete admission. The Rust
library can read declared nested blank-node models with shared identities and visible
graph anchors; atomic nested editing and its HTTP and browser consumers remain planned.

## Documents

* [Design and acceptance](SPEC.md).
* [Delivery record](OUTCOME.md).
* [Provenance](SPEC.md#provenance).

## User guides

* [Editing forms](../../USAGE.md#editing-forms).
* [Forms API](../../API.md#editing-forms).
* [Capabilities and limits](../../visualization/README.md#web-ui).

[Item 21](../../ROADMAP.md) tracks nested blank-node forms, list and choice editors and
remaining target-node expression admission. Item 395 supplies the controlled physical
readers required for stricter reader-budget coverage. Ordinary SHACL validation has its
own supported language and resource contract.
