# C30: Resource catalog

> **Status:** planned.
>
> **Roadmap:** [item 458](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

A server-level catalog of versioned sources, vocabularies and datasets with a content-addressed source store, recipes and harvesters, load manifests, pinned imports, mirrors, IRI minting and licence records.

## Problem and goal

Loading a public dataset means finding its files, fetching them, choosing graphs and
index settings and remembering which version built which graph. Vocabularies have no
managed lifecycle, imports are not pinned, and the licence of a source is not recorded.

The goal is one server-level catalog of versioned resources that makes loading
reproducible and source upgrades safe.

## Scope

A resource has a kind, which is source, vocabulary or dataset, an ownership policy,
which is owned, mirrored or synced, versions and pinned imports. The catalog is
described in DCAT 3, PROV and SPDX, with a Croissant export for ML consumers. Raw sources
live in a content-addressed store on the backup backend, so each download is stored once.
Vocabularies are per server.

## Candidate phases

**Content-addressed raw source store.** Keep a server-wide sources area of blobs by SHA-256 of the raw and decompressed streams, with per-source versions, URL, ETag, licence, size, pins and GC by reference, on the F05 backend trait so it works on a directory or S3 with ranged reads into the loader. Fetch with resume and digest checks through the outbound policy. It is the foundation of the dataset manager and stores each download once. The model store of F12 already has a verified, resumable fetch. It depends on items 61 and 339.

**Curated catalog with recipes and harvesters.** Ship a versioned catalog of datasets and vocabularies with distributions, file globs, graph assignment and recommended index settings, harvest OBO Foundry, Bioregistry, LOV, the Databus and CKAN or DCAT-AP portals, and read Qleverfiles. Recipes make loading one click, and Qleverfiles are a proven declarative model.

**Dataset manager UI and API.** Add a sources page to browse the catalog, add sources from a URL, upload, S3, Hugging Face Hub or git, check for updates, pin and delete, plus create-dataset-from-sources dialogs, a `/$/sources` API, a sparkles source CLI, MCP tools and a server-wide view of storage used per source.

**Load manifests and load provenance.** Record which source versions built which graphs in a versioned manifest and in commit metadata, and offer reload-from-sources and upgrade-source as one replace-mode commit. It makes datasets reproducible and source upgrades safe.

**Lockfile of pinned imports.** Record each resource's imports with resource, version and digest, and propose lock bumps on a branch with diffs and gate results. It makes imports reproducible and upgrades safe.

**Immutable mirrors.** Register a dataset's graphs as managed under a mirror owner, update them only by pulling a new upstream version, and tag a snapshot per upstream version. External datasets become usable but never edited in place. Managed graphs already refuse every write path. It depends on item 81.

**Vocabulary resource with namespaces.** Make a vocabulary a catalog resource that owns namespaces and prefixes, its graphs, header metadata (version, licence, VANN) and release history, scoped per server. It turns shared vocabularies into managed units.

**IRI minting service and policies.** Mint IRIs by label-slug template, UUID or ULID, padded sequence or OBO ID ranges per editor, with collision checks and reservation in the commit, for forms, the editor and imports. Every editor needs it and OBO practice depends on ID ranges.

**Term editing forms from a built-in term shape.** Ship SHACL term shapes for RDFS and OWL classes and properties and SKOS concepts so U02 forms edit vocabulary terms without user-written shapes. It reuses U02 for the common case instead of a new editor. It depends on item 21.

**Cross-dataset term usage index.** Aggregate every dataset's schema report into a server-level usage index keyed by term. It powers usage pages, breaking-change gates and search, and every dataset already keeps exact class and predicate counts.

**Unified resource catalog.** Keep a server-level catalog of versioned resources of kind source, vocabulary or dataset, each with an ownership policy (owned, mirrored, synced), versions and pinned imports, described in DCAT 3, PROV and SPDX with Croissant export for ML consumers. One model serves the dataset manager, vocabulary manager and mirrors, and it supersedes the asset-collection and registry pieces.

**Linkset sources for cross-dataset identity.** Offer published linksets such as DBpedia to Wikidata sameAs and Bioregistry mappings as catalog entries loaded as separate graphs for item 9's canonical equality. Semantic deduplication across datasets belongs in linksets, not storage. It depends on items 9 and 396.

**Prefix and vocabulary bundles.** Install C20 prefix sets (prefix.cc, Bioregistry) and common vocabulary graphs (schema.org, SKOS, PROV, DCAT, QUDT) into a dataset in one step. It lowers the friction of every new dataset.

**Documented library-plus-branches sharing pattern.** Document one library dataset with user branches as the pattern for shared public knowledge graphs, with grant presets and branch compaction guidance. It gives most of the sharing benefit with shipped F09 features.

**Deprecation and obsolescence lifecycle.** Deprecate with replacement, reason and consider, refuse deletion or reuse of issued IRIs, and apply the obsolete-label convention by policy. OBO and general vocabulary hygiene require it.

**Licence records, propagation and policy gates.** Record an SPDX identifier or LicenseRef on every source, treating unknown as no redistribution, and sort licences into four classes (open, attribution, share-alike, restricted). Ship catalog recipes but never data, with recorded administrator acceptance and the administrator's own credentials for restricted sources. Propagate obligations per graph through imports with a generated NOTICE, apply gates only from explicit operator policy that warns by default, and allow publication or export only when the data is redistributable, sharing restricted sources as recipes only. Several catalog sources carry share-alike, non-commercial or restricted terms (DBpedia, OSM, IMDb, HPO).

**Overlays on mirrors.** Let an owned dataset pin a mirror and add graphs, first as a branch of the mirror upgraded by merging the new upstream, later as a union view over a multi-base reference. It allows local additions without forking upstream.

**Source update monitoring.** Check ETag, Last-Modified or Databus and OBO version IRIs on a schedule, notify through C21 and optionally fetch under a pin policy. It keeps catalogued sources current. It depends on item 339.

**Per-server vocabulary scope.** Record in a spec Outcome why a vocabulary shared across datasets is not adopted (lifecycle coupling, renumbering, term-existence leak, format blast radius). Sharing one vocabulary across datasets would couple their lifecycles, force renumbering, leak which terms exist and widen the reach of a format change, so each dataset keeps its own.

**Canonical content hashing of sources.** Hash sorted canonical quads after parsing, with RDFC-1.0 for small blank-node graphs, to detect the same data across formats and mirrors. LOD Laundromat shows the value. The formatter already canonicalizes.

## Dependencies

* The policy and plumbing stage of [item 453](../../ROADMAP.md), items 480 to 483, lands first, so that
  catalog surfaces call one request builder and one execution policy.
* Item 339 supplies durable jobs for fetching and update monitoring, item 61 the storage
  backends and item 81 the managed graphs that mirrors use.

## Acceptance gates

* A dataset built from the catalog records the source versions behind each graph, and
  reload from sources reproduces it.
* A fetch verifies its digest, resumes after interruption and goes through the outbound
  policy.
* Licence gates warn by default and refuse only under explicit operator policy.

## Non-goals

* A vocabulary shared across datasets. Sharing one vocabulary would couple the
  lifecycles of datasets, force renumbering, leak which terms exist and widen the reach of
  a format change.
* Federation between servers. Vocabularies are per server and would be replicated with
  high availability if that lands.

## Design constraints

Each source records an SPDX identifier or a LicenseRef, and an unknown licence
means no redistribution. Licences fall into four classes, which are open, attribution,
share-alike and restricted. The catalog ships recipes and never data. A restricted source
needs recorded administrator acceptance and the administrator's own credentials.
Obligations propagate per graph through imports with a generated NOTICE. Gates apply only
from explicit operator policy and warn by default. Publication and export are allowed only
for redistributable data, and restricted sources are shared as recipes only.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: DVC, DBpedia Databus, Hugging Face Hub, qlever-control, Databus, CKAN, OBO Foundry, LOV, GraphDB server files, Kaggle, Virtuoso load list, Qleverfile MULTI_INPUT_JSON, ODK import refresh, QLever Qleverfiles, TopBraid EDG, VocBench, OBO idranges, EDG, WebProtégé forms, OLS, OntoPortal, MLCommons Croissant, Databus linksets, SSSOM, prefix.cc, Bioregistry, OBO conventions, SPDX, TopBraid EDG includes and LOD Laundromat. No code from another project is copied.

## Outcome

Nothing is built. [Item 458](../../ROADMAP.md) tracks the work.
