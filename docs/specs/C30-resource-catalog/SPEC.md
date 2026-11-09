# C30: Resource catalog

> **Status:** partially implemented (Phase 1).
>
> **Roadmap:** [item 458](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

The design covers a server-level catalog of versioned sources, vocabularies and datasets with a content-addressed source store, recipes and harvesters, load manifests, pinned imports, mirrors, IRI minting and licence records.

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

## Phase 1: catalog model, licences and vocabularies

Phase 1 builds the parts of the unified resource catalog, the licence records, the
vocabulary resource and the prefix bundles that need neither durable jobs nor storage
backends. Nothing in it fetches a file, so the outbound policy, the source store and
update monitoring stay with their phases.

### Model

The catalog is one file of the data directory, `resources.json`. A resource has an id,
a kind, a title and an ownership policy, and it may carry a description, a homepage, a
licence and an attribution text. An id has 1 to 64 letters, digits, `.`, `_` and `-` and
starts with a letter or digit. The kind is `source`, `vocabulary` or `dataset`.

* The ownership policy is `owned`. A `mirrored` resource needs the immutable mirrors
  that build on item 81, and a `synced` resource needs the update monitoring of item
  339. The catalog refuses both and names the dependency, rather than recording a
  policy that nothing enforces.
* A vocabulary resource owns namespaces, each a prefix and a namespace IRI, and lists
  the dataset graphs that hold it. A namespace IRI and a prefix belong to at most one
  vocabulary resource on the server. Other kinds refuse namespaces and graph locations.
* A dataset resource may name the server dataset it describes.
* A version has a version string of 1 to 64 letters, digits, `.`, `_`, `+` and `-`,
  unique in its resource. It may carry an issue date, notes, a licence that overrides
  the resource's licence, distributions and pinned imports. A distribution has a
  download URL, a media type, a byte size and a SHA-256 digest, and only the URL is
  required. An import names an existing resource and one of its existing versions.
* A version of a dataset resource lists its graphs, each with the source versions it
  was built from. These references are pinned in the same way as imports.
* A version cannot change once it is added, so a pin always names the same record. A
  correction is a new version. Removing a version or a resource that another version
  references is refused with `409 resource-in-use`. A version can reference only
  versions that already exist, so references never form a cycle.

The catalog records who created and last changed each resource and when, and who
added each version and when.

### Licences

A licence is an SPDX licence expression of identifiers, `LicenseRef-` references,
`AND`, `OR`, `WITH`, `+` and parentheses. A built-in table sorts the common data and
software licences into the four classes of open, attribution, share-alike and
restricted. An expression that names an identifier outside the table, or a
`LicenseRef-`, needs an explicit class. A class given for an expression that the table
classifies, or for `NOASSERTION` and `NONE`, must restate the table's class, so a record
cannot loosen it. `WITH` follows a single licence. `A AND B` takes the more restrictive class and
`A OR B` the less restrictive one, and `WITH` keeps the class of its licence. A missing
licence, `NOASSERTION` and `NONE` have the class unknown. Unknown and restricted data is
not redistributable.

A restricted resource needs administrator acceptance, which
`POST /$/resources/{id}/acceptance` records with the principal and the time. A change
of the resource's licence withdraws the acceptance, and so does `DELETE` on the same
route.

Obligations propagate through references. The obligations of a version are its own
licence and, transitively, the licences of the versions it imports. Each graph of a
dataset version adds the obligations of its source versions. The class of a graph is the
most restrictive class among its obligations, in the order open, attribution,
share-alike, restricted and unknown. `GET /$/resources/{id}/license` reports the
obligations of a version for each graph, whether the data is redistributable and the
verdict of the licence gate. `GET /$/resources/{id}/notice` writes the NOTICE as plain
text, with each licence, its attribution, its URL and the graphs it covers.

The gate is `serve --license-policy`, which is `warn` by default. Under `warn`, a version
that is not redistributable gets the verdict `warn`. Only `enforce`, which the operator
sets explicitly, gives the verdict `refuse`. A restricted resource without acceptance is
always reported. The export and publication surfaces apply this verdict in item 525,
since the dump, Graph Store downloads, shared backups, git export in item 460 and
linked-data publication in item 461 each need their own refusal.

### Description in DCAT 3, PROV-O, SPDX, VANN and VoID

`GET /$/resources` and `GET /$/resources/{id}` answer JSON by default and RDF on
`Accept` or `format=`. The catalog is `urn:x-sparkles:resources`, a `dcat:Catalog`, and
each resource is `urn:x-sparkles:resource:{id}`, a `dcat:Dataset` with `dct:type` naming
its kind. Each version is `urn:x-sparkles:resource:{id}/{version}`, linked with
`dcat:hasVersion`, `dcat:hasCurrentVersion` and `dcat:previousVersion` and carrying
`dcat:version`. A distribution is a `dcat:Distribution` with `dcat:downloadURL`,
`dcat:mediaType`, `dcat:byteSize` and an `spdx:checksum`. A licence is `dct:license`,
which is the SPDX licence page for a single listed identifier and a
`dct:LicenseDocument` labelled with the expression otherwise. Imports and graph sources
are `prov:wasDerivedFrom`. Each graph of a dataset version is a `void:Dataset` of that
version, linked with `dct:hasPart` and naming its graph with `dct:identifier`, so two
versions that reuse a graph IRI keep their own sources. A
namespace is `vann:preferredNamespacePrefix` and `vann:preferredNamespaceUri`, and a
vocabulary's graph location is a `void:Dataset` with `void:inDataset`. The Croissant
export for ML consumers is item 526.

### Vocabulary header and prefix bundles

`GET /$/resources/{id}/header` reads the header of each graph that holds a vocabulary,
which is the `owl:Ontology` or `skos:ConceptScheme` with its `owl:versionIRI`,
`owl:versionInfo`, `dct:license`, `dct:title`, `vann:preferredNamespacePrefix` and
`vann:preferredNamespaceUri`. It runs the read through the caller context of the
graph's dataset, so it reads only what the caller may query, under the server's query
timeout and budgets, and the rate limits and memory admission count it as a query.
`owl:versionInfo` must equal the current version, and without it one segment of the
version IRI's path, or its fragment, must. SPDX licence pages compare by identifier, and any other licence IRI
compares exactly. It reports where the header disagrees with the current version
record, and it changes nothing.

`POST /$/resources/{id}/install/{ds}` installs the namespaces of a vocabulary, and of
the vocabularies its current version imports transitively, as prefixes of the dataset in
one write. The write goes through the prefixes settings kind, so it honours the
operator's locks and the prefix limit. A prefix that the dataset already binds to a
different IRI is a `409 prefix-conflict` unless the request sets `replace=true`. The
write carries the entity tag of the prefixes the install read, so a concurrent prefix
write makes the install plan again instead of overwriting a binding it did not see.
Loading the vocabulary graphs themselves and the C20 prefix sets of prefix.cc and
Bioregistry needs the fetch of the source store and stays with the bundle phase.

### Routes and access

Every `/$/resources` route needs server `admin`, except the install route, which needs
`admin` on its dataset. Writes answer `403 server-read-only` on a read-only server. A
resource's JSON carries an `ETag`, and `PUT` and `DELETE` honour `If-Match`. `PUT`
replaces the descriptive members and refuses `versions`, which change only through
`POST /$/resources/{id}/versions` and `DELETE /$/resources/{id}/versions/{version}`.

### Format

`resources.json` carries `format` 1 and is registered in `sparkles::formats`. A file
with an unknown format, or one that does not parse, is never rewritten. Every catalog
route then answers `503 catalog-unreadable` with the reason, and the server keeps
serving its datasets. A missing file is an empty catalog, and a server without a data
directory keeps its catalog in memory. The file is written atomically. Each write checks
the whole catalog. A version names each import and each graph source once. The catalog refuses more than 4,096 resources, 1,000 versions of a
resource, 256 namespaces, distributions, imports or graphs on one record and a file
larger than 16 MiB, which it reads no further than the limit.

### Tests and gates

Unit tests cover the licence table and expressions, the class order, the model's checks,
the propagation of obligations, the NOTICE and the RDF description. Router tests cover
the routes, `ETag` and `If-Match`, the read-only refusal, references that block removal,
namespace ownership, acceptance withdrawal, both licence policies, the header read and
its disagreements, and installation with and without conflicts. A
format test writes a file of format 2 and checks that every route refuses it and that
the file is unchanged. The OpenAPI description, the generated client types and
`api-compat.py --check-manifest` cover the new routes.

The phase meets the third acceptance gate for the verdict. Licence verdicts warn by
default and refuse only under `--license-policy enforce`, and the surfaces that act on
the verdict are item 525. The first two gates need the source store and stay open.

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

Phase 1 was written from the W3C Recommendations DCAT 3 (Data Catalog Vocabulary,
Version 3), PROV-O and the VoID Vocabulary, the VANN vocabulary for annotating
vocabulary descriptions, and the SPDX specification's License Expression syntax, License
List identifiers and checksum terms. The licence table classifies SPDX License List
identifiers by the terms of their licence texts. No Fluree source or documentation was
used, and no code from another project is copied.

## Outcome

Phase 1 has shipped. [Item 458](../../ROADMAP.md) keeps the remaining phases.

The server keeps the catalog in `resources.json`, format 1, which `sparkles::formats`
registers as a server settings file. The code is `crates/sparkles-server/src/resources`.
The model, the licences, the propagation of obligations, the NOTICE, the DCAT
description, the vocabulary header read and the prefix install follow the plan above.
The header read runs through `CallerContext::read_options`, so it meets the server's
query timeout and budgets. The install writes through the prefixes settings kind. The
catalog reads its file on each request under one lock, which keeps it current after an
operator edits the file and costs one parse of at most 16 MiB per request. That is
small for an administrative catalog of at most 4,096 resources.

A version of a dataset resource records its graphs and their sources by hand in this
phase. Recording them when data is loaded, and reloading from sources, belong to the load
manifest phase, which needs the source store for reload. The licence gate reports its
verdict, and the export and publication routes apply it under item 525. The Croissant
export is item 526. Only server administrators read the catalog, and a filtered read
for dataset administrators belongs to the dataset manager phase.

The catalog is per server, as the non-goals require. A vocabulary resource names the
dataset graphs that hold the vocabulary, and each dataset keeps its own term dictionary.
An ontology read of that dataset resolves an `owl:imports` of one of the resource's
namespaces to those graphs, as U12's
[import resolution](../U12-ontology-workbench/SPEC.md#import-resolution) describes.
A dictionary shared across datasets is not adopted, because it would couple the
lifecycles of the datasets, force renumbering when one changes, reveal through shared
ids which terms another dataset holds and widen the reach of every change to its format.

Unit tests cover the licence expressions and table, the class order, the model's checks,
references, propagation, the NOTICE, the DCAT description and the file format. Router
tests cover every route, the entity tags, the read-only refusal, an unreadable file on
every route, both licence policies, acceptance, the header read and the install with and
without conflicts.
