# U12: Ontology workbench

> **Status:** partially complete. The ontology editor phase is built, and the SKOS editor,
> the DL Query tab and the editable schema diagram are planned.
>
> **Roadmap:** [item 457](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

The design covers an ontology editor in the web UI with class, property and individual trees, entity frames, a SKOS thesaurus editor, a DL Query tab and an editable schema diagram.

## Problem and goal

The web UI browses schemas and edits instance data through shape-driven forms, but it
cannot edit an ontology. Property hierarchies are not shown, SKOS concept schemes have no
editor, and there is no way to ask which classes satisfy a class expression.

The goal is an ontology workbench in the web UI over the F21 axiom model.

## Scope

The workbench edits entities, axioms and annotations through the axiom model of
[F21](../F21-ontology-api.md) and shows asserted and inferred trees side by side from
[F24](../F24-owl-classification/README.md). Term editing reuses the forms of
[U02](../U02-shape-driven-forms/README.md) for the common case.

## Candidate phases

**SKOS and SKOS-XL thesaurus editor.** Edit concept schemes, top concepts, collections, broader, narrower and related links, multilingual labels, SKOS-XL labels and mapping relations in a tree. SKOS is more common than OWL in enterprise taxonomies. No support exists. It depends on item 21.

**Ontology editor UI.** Add editable class, object, data and annotation property and individual trees, an entity frame with axioms, an annotation editor with language tags and annotated axioms, a header editor, an axiom-level usage view, side-by-side asserted and inferred trees, IRI policy hookup, create, rename and delete, a session undo stack, and live refresh of open views from the change feed, all writing previewed commits on a branch. Real-time co-editing stays with the U05 §1 open question. Protégé and WebProtégé users expect these views. Property hierarchies are missing entirely. It depends on items 21 and 71.

**DL Query tab.** Evaluate a Manchester class expression for sub- and superclasses, equivalents and instances, first under an RL approximation through demand reasoning, then under the EL or DL classifier. Protégé users expect it. It depends on item 71.

**Editable schema diagram.** Draw the schema diagram from declared axioms as well as observed data, with restrictions as labelled edges, an optional VOWL notation and SVG, Mermaid and PlantUML export, and turn it into a modeling canvas that adds classes, properties and restrictions by drawing. Stardog Designer and Gra.fo offer the same canvas. It depends on item 31.

**Ontology browser.** Browse an ontology or thesaurus as linked articles, taking cues from the term pages of the EBI Ontology Lookup Service and Ontobee while keeping a clean, uncluttered layout. Search is a typeahead with typo tolerance over labels, synonyms, short forms and definitions in any language, ranking exact and prefix matches of preferred labels first, from an entity index that each commit updates and that answers within 20 ms on the server for ontologies of ChEBI's size. A command palette opened with `Ctrl+K` or `Cmd+K` reaches the same search, with datasets, saved queries, pages and actions, from anywhere in the UI. Each entity's page shows its label, IRI and short form, definition, synonyms and cross-references, an infobox of types and key annotations, a collapsible hierarchy tree centred on the entity, its axioms rendered as prose with links, instances, the ontologies that reuse the term, a "what links here" list and downloads of the entity in each RDF format. An edit mode lets a caller with write access change the page in place, with the ontology editor's previewed commits, session branches and undo. The editor, the DL Query tab and the term pages of [C32](../C32-linked-data-publishing/README.md) reuse these pages.

**Visual SHACL shape editor.** Author node and property shapes as trees and forms, with targets, paths, constraints and severities, validated live against sample data and written as previewed commits. TopBraid Composer and EDG offer it.

**Discussions, watches and entity history.** Comment threads on entities and axioms with mentions and resolution, watches that notify a user of changes to an entity or its subtree, and a history tab with each commit that changed the entity, as WebProtégé offers.

**Metrics and inferred frames.** Show axiom and entity counts by type and the DL expressivity, and show the classifier's inferences inline in entity frames with an explanation link from F24's justifications, as Protégé does.

**Patterns, imports, rendering and bulk edits.** Generate axioms from spreadsheet rows through templates and design patterns, as Cellfie, ROBOT template and DOSDP do. Resolve `owl:imports` through local copies, the resource catalog and XML catalogs. Let users choose rendering by label, prefixed name or IRI with a language order, and apply find-and-replace and annotation changes to many entities in one commit.

**Governance, tagging and co-editing.** Add vocabulary roles and change requests that gather edits on a branch for review and approval, suggest a thesaurus's concepts for ingested documents, and show other editors' presence with live merging of concurrent edits, which the U05 §1 open question leaves undecided.

## Dependencies

* Item 71 supplies the axiom model, and the classifier of F24 supplies inferred trees.
* Item 21 supplies the shape-driven forms, and item 31 the schema diagram this extends.
* The resource catalog of [C30](../C30-resource-catalog/README.md) supplies IRI minting
  and vocabulary resources.

## Acceptance gates

* Every edit is a previewed commit through the existing write path, with the same
  access checks and conflict handling as other writes.
* Round trips through the axiom model leave unrelated triples unchanged.

## Non-goals

* A desktop application. The workbench is part of the web UI.

## Design constraints

The ontology-editing exclusions of U02 §1 and U07 §1 are lifted for this spec.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: VocBench, PoolParty, the EBI Ontology Lookup Service, Ontobee, EDG Taxonomy, EDG Tagger, TopBraid Composer, Protégé, WebProtégé, Protégé DL Query, Cellfie, ROBOT template, DOSDP, Stardog Designer and OntoGraf. No code from another project is copied.

## Outcome

### Ontology editor

The ontology editor phase is built as the UI's **Ontology** page, `/ui/ontology`. It reads
and edits one graph of a dataset, the default graph unless the caller picks another, with
the graphs that graph imports. Every read and write goes through the `/$/ontology/{ds}`
routes of [F21](../F21-ontology-api.md#outcome), with the caller's grants, and through
`/{ds}/update` for renames and header edits.

**Trees.** The page has trees of classes, object properties, data properties,
annotation properties and individuals. Each tree shows the asserted hierarchy from the
`SubClassOf`, `SubObjectPropertyOf`, `SubDataPropertyOf` and `SubAnnotationPropertyOf`
axioms, and the individuals are listed with their asserted classes beside each name. Entities are named by
their `rdfs:label` or `skos:prefLabel` in the browser's language, with the prefixed name
beside a label. A filter keeps the matching entities and their ancestors. A cycle in the
asserted hierarchy is drawn once and not followed. The trees read every entity and
hierarchy axiom of the graph in pages of 10,000, up to 50,000 of each, so very large
ontologies load slowly and a lazy hierarchy route remains a follow-up.

**Inferred tree.** The **Inferred** switch shows the class taxonomy of the EL classifier
of [F24](../F24-owl-classification/README.md) beside the asserted tree. Its nodes are the
classifier's equivalence classes, so equivalent classes share a node, and the
unsatisfiable classes sit under `owl:Nothing`. Property and individual trees show the
asserted hierarchy only.

**Frame.** Selecting an entity opens its frame. The frame lists the entity's annotations,
its characteristics as checkboxes for properties, and its axioms in sections such as
SubClass Of, Equivalent To, Disjoint With, Domains, Ranges, Inverse Of, Types and the
property assertions of an individual. Class expressions are rendered in Manchester
syntax with labels. Each axiom row shows its annotations and can be removed. An add
field per section takes an entity name, an IRI or a class expression in functional-style
syntax with the dataset's prefixes. Axioms that fit no section are listed last.

**Annotations.** The annotation editor adds, changes and removes annotations of an entity
or of one axiom. A value is text with a BCP 47 language tag or a datatype, or an IRI. An
annotation of an axiom is written as an annotated axiom, so the edit removes the plain
axiom and adds it with its annotations in one commit.

**Header.** The header tab edits the ontology IRI, the version IRI, the imports and the
header's annotations, and shows the graph's axiom counts by type and the read's
diagnostics. Header edits are `DELETE DATA` and `INSERT DATA` updates with their exact
inverse.

**Usage.** The usage tab lists every axiom that mentions the entity, grouped by axiom
type, including the assertions about it.

**Create, rename and delete.** **New** creates an entity of the tree's kind with its
declaration and label, under the selected entity when there is one. Its IRI is minted by
the IRI policy, which takes a namespace and builds the local name from the label, from a
UUID or from a prefix and a zero-padded number. The policy is kept in the browser per
dataset. The dialog refuses an IRI the graph already uses. Rename replaces an IRI in every
triple of the graph through a SPARQL update that renames nothing when the new IRI is
already in use. Delete removes the entity's declarations, its axioms and the assertions
about it through `DELETE /$/ontology/{ds}/entity`.

**Previews and sessions.** Every edit opens a preview that runs it as a dry run with
`If-Match` naming the commit the page shows. The preview lists the axioms the edit adds
and removes, the triples it changes and the write's outcome, and **Commit** writes it
with the same precondition, so a commit by another writer in between refuses the edit
with `412` and the page reloads. **Start session** creates a branch named
`onto.<user>.<date>` from the current branch and edits there, and the Merge link opens the
branch's merge page. A protected branch, or a caller without write access, makes the page
read-only.

**Undo.** The page keeps an undo stack of its commits for the browser session. An undo
reverts the commit through `POST /$/revert/{ds}`, a three-way merge that restores exactly
the triples and blank nodes the commit changed, and a redo reverts that revert. Undo and
redo are previewed with a dry run that expects the head the page shows, so another
writer's commit in between refuses them, and a later commit that changed the same triples
makes the revert conflict. Ctrl+Z and Ctrl+Shift+Z run them. A revert needs write access
to the whole branch, so a caller whose grants cover only some graphs can edit but not
undo. A first version built each commit's inverse from the axioms the edit answer lists.
That inverse also removed the part of an n-ary axiom that was present before the edit and
gave a restored restriction a new blank node when another axiom shared the old one, so
undo uses reverts.

**Live refresh.** The page follows `GET /{ds}/changes` for the branch it edits. A commit
by another writer reloads the trees and the frame and shows a notice with the commit
number.

The server gained what the editor needs. The ontology routes take `?branch=` and the
`/{ds}@{branch}` form, and `DELETE /$/ontology/{ds}/entity` removes an entity with
preview, precondition and receipt. Edits accept axioms in functional-style syntax, and an
edit's answer lists the axioms it added and removed, up to 10,000 of each. The server
tests check that an edit and its inverse leave unrelated triples unchanged, and the
real-server Playwright test checks that an undo on a branch leaves its triple count as it
was. The UI's unit tests cover the tree, frame, Manchester rendering, IRI minting,
rename and undo logic. Playwright covers each view, the edits on a session branch, undo
and redo, the refusal of a stale edit and the refresh after another writer's commit
against the mock backend, and the trees, the inferred tree, edits on a branch and the
unchanged main branch against a real server.

The editor reuses neither the shape-driven forms of U02 nor their field editors. Its
annotation and axiom editors are its own components, so editing an entity's terms
through a form remains for later. Manchester syntax is rendered but not parsed, so class
expressions are typed in functional-style syntax. Rename covers the one graph the page
reads and does not reach IRIs inside triple terms. Real-time co-editing stays with the
U05 §1 open question. The page decides whether the caller can write from its level on
the dataset, so write grants on a branch alone leave the page read-only, roadmap item
611. Roadmap item 607 covers a lazy hierarchy route for large
ontologies, 608 Manchester input of class expressions, 609 inferred
property trees and 610 an IRI policy kept on the server with the resource catalog
of [C30](../C30-resource-catalog/README.md).

### Ontology browser search

The server part of item 616's ontology browser is built as an entity search,
`GET /$/ontology/{ds}/search`, with the Rust call `Ontologies::search` and `search` on the
ontology handles of Python, Node.js and the JVM. The browser's pages are not built, and
item 616 stays open for them.

**What is indexed.** An entity is an IRI with a type fact for an OWL class, property,
named individual or datatype, an RDFS class, property or datatype, or a SKOS concept,
concept scheme or collection. Its labels are `rdfs:label`, `skos:prefLabel` and SKOS-XL
preferred labels. Its synonyms are `skos:altLabel`, `skos:hiddenLabel`, their SKOS-XL
forms and the five oboInOwl synonym properties. Its short forms are `oboInOwl:id`,
`skos:notation` and the local name of the IRI, and its definitions are `skos:definition`
and `IAO_0000115`. Each value keeps its graph, language and property, so a request that
reads some graphs or that a caller's grants limit sees only those values.

**Matching.** Texts are folded to lower case without diacritics or punctuation through a
table generated from Unicode's decompositions by `scripts/gen-search-fold-table.py`.
Whole labels and synonyms are kept in sorted maps for exact and prefix matches, and their
words, split also at camel case and digits in local names, in a sorted map of postings
for word matches. Misspelt words are found by Levenshtein automata from the
`levenshtein_automata` crate walked over those sorted keys, skipping each subtree whose
prefix the automaton rejects. A prefix automaton stops descending once its distance is
settled, so a short prefix does not enumerate every word below it. The text index of
[F03](../F03-full-text-search/README.md) expands fuzzy terms with automata from the same
crate over Tantivy's term dictionary. It indexes quads and not entities, keeps no state
per branch and commits on its own schedule, so the entity index is separate and shares
only the automaton crate.

**Ranking.** The eight tiers of the API reference are fixed, and within a tier hits are
ordered by the number of edits, the field, deprecation, the property's rank within the
field, the language, the matched text's length, the text and the IRI. Each entity
appears once, at its best match. The ranking keeps a bounded heap of the best hits so
far and skips candidates that cannot enter it, which keeps typo queries over short
prefixes within the latency target.

**Maintenance.** Each branch holds its index in its dataset state. The first search
builds it from the head, and a later search reads the commits since its own through the
change ring or the change feed and applies their added and removed triples. A change
record that is not available, more than 500,000 changed triples, or a head behind the
index, as after a restore, rebuild it. An aborted transaction leaves no commit and so no
change. The search's deadline and cancellation stop a build or a wait for another
caller's update, and a stopped build leaves the previous index in place. Commits do not touch the index, so the write path is
unchanged. Nothing is persisted, so there is no format to version, and a restart
rebuilds the index at the first search. Tests compare the index after random commits, SKOS-XL relinks, writes
on a branch, a merge, its revert and a reopen with one built from scratch.

**Measurements.** On atlas, an Intel Core i5-13500, typeahead queries of 1 to 12
characters with up to two typos answered within 8.4 ms at the 99th percentile on ChEBI
lite and 4.9 ms on GO, against the 20 ms target. ChEBI lite's index of 238,190 entities
took 1.3 s to build and about 233 MiB by its own estimate, 292 MiB resident, and GO's of
52,057 entities 0.64 s and 116 MiB resident. That memory, held per searched branch, is
the price of answering typeahead without reading the store. The search after each of 50
small commits applied the commit in under 3 ms with no rebuild. The
[entity search benchmark](../../benchmarks/entity-search/README.md) records the method
and the run.

**Remaining work.** The browser's entity pages, hierarchy and graph views stay in item
616. A persisted index that avoids the build at the first search after a start is item
NEW-ITEM-1, searches at a past commit or with inferred kinds are item NEW-ITEM-2, and
word segmentation for scripts written without spaces is item NEW-ITEM-3. The ontology
editor's tree filter, the DL Query entity completion and the thesaurus editor's label
search can move to this search under item NEW-ITEM-4.
