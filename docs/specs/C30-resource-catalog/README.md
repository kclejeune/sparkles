# Resource catalog

This spec covers a server-level catalog of versioned sources, vocabularies and datasets with a content-addressed source store, recipes and harvesters, load manifests, pinned imports, mirrors, IRI minting and licence records. Phase 1 is implemented, and
[roadmap item 458](../../ROADMAP.md) holds the remaining phases.

Phase 1 keeps versioned resources with pinned references, SPDX licence classes with
propagated obligations, a NOTICE and a licence gate that warns by default, vocabulary
namespaces and prefix installs, described in DCAT 3. The
[server guide](../../server/README.md#resource-catalog) and the
[API reference](../../API.md#resource-catalog) describe its use.

Read the [design](SPEC.md), its [candidate phases](SPEC.md#candidate-phases), the
[Phase 1 plan](SPEC.md#phase-1-catalog-model-licences-and-vocabularies) and its
[acceptance gates](SPEC.md#acceptance-gates). The [Outcome](SPEC.md#outcome) records the
delivery and its [Provenance](SPEC.md#provenance) the design sources. The export gates
and the Croissant export are 525 and 526.
