# U12: Ontology workbench

> **Status:** partially complete. The ontology editor and thesaurus editor phases are built,
> and the DL Query tab and the editable schema diagram are planned.
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

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: VocBench, PoolParty, EDG Taxonomy, Protégé, WebProtégé, Protégé DL Query, Stardog Designer and OntoGraf. No code from another project is copied.

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

### Thesaurus editor

The SKOS and SKOS-XL thesaurus editor phase is built as the UI's **Thesaurus** page,
`/ui/thesaurus`. It reads and writes one graph of a dataset with SPARQL queries and
`DELETE DATA` and `INSERT DATA` updates, so it needs no routes of its own beyond the
built-in shapes.

**Schemes and the concept tree.** The page lists the graph's concept schemes, and the tree
of the chosen scheme starts at its top concepts, from `skos:hasTopConcept` and
`skos:topConceptOf`. Concepts of the scheme without a broader concept are roots too, marked
when they are not top concepts. Each level is read when it opens, from `skos:broader` and
`skos:narrower` in both directions, with the number of narrower and broader concepts of
each child, so the tree reads one level of a large thesaurus at a time. A concept with
several broader concepts sits under each of them. A level shows its first 2,000 concepts
and the roots their first 5,000.

**Moves.** Dragging a concept onto another removes its link to the old broader concept and
adds the new one, writing `skos:broader` and `skos:narrower` together. Holding Ctrl or Alt
keeps the old link, which builds a polyhierarchy, and the top zone makes a concept a top
concept. A move under one of the concept's own narrower concepts is refused before the
preview.

**Labels.** The label table shows the preferred, alternative and hidden labels by language,
with SKOS-XL labels marked. Adding or changing a label refuses a second preferred label in
one language (S14) and a label that is already another kind of label (S13), counting the
literal forms of SKOS-XL labels as S55 to S57 imply. A label converts between its plain
and SKOS-XL forms, and a new SKOS-XL label gets an IRI minted from the resource's IRI.
Deleting a SKOS-XL label removes its label resource.

**Links.** The relations tab edits broader, narrower, related and the five mapping
relations. Broader, narrower and related links are written in both directions, and a link
stated in one direction only is marked with a repair that adds the other. The picker finds
concepts by label in any scheme of the dataset or in another dataset the caller can read,
or takes an IRI.

**Collections, notes and search.** Collections and ordered collections list their members,
and their members and `skos:memberList` are edited through the shape-driven form of
[U02](../U02-shape-driven-forms/README.md) with its list editor. Notes and documentation
are edited through the same form over the built-in SKOS shapes, grouped as notes and
documentation. The search matches a substring of any label, SKOS-XL literal forms
included, in all languages or one, and answers its first 100 matches.

**Integrity shapes.** The built-in SKOS shapes, served by `GET /$/skos/shapes` and read by
forms with `shapes=skos`, check S9, S13, S14, S27, S37 and S46 of the SKOS Reference with
S51, S52 and S58 of the SKOS-XL appendix. They live in the Rust library as
`sparkles::skos`, so a write guard enforces them like any shapes, and the page's checks
tab runs them over the graph and installs them as the dataset's guard with grandfathered
existing results. Each SPARQL constraint reads one property path or one basic graph
pattern from the focus node, so a form check finds the resources an edit affects without
validating the whole graph. The shapes graph does not hold the SKOS vocabulary, so the
disjointness checks name the types the SKOS properties imply, ordered collections count
as collections, the hierarchical and related mappings count for S27, and S46 follows the
transitive closure of `skos:exactMatch`. Linked resources without their `rdf:type` are
reported at `sh:Info` severity, since the Reference's own examples leave their resources
untyped.

**Sessions.** The page reuses the ontology editor's session through a shared module. Every
edit of the tree, labels and links is a dry run with `If-Match` and a commit with the same
precondition, undo and redo revert commits, **Start session** creates a
`skos.<user>.<date>` branch, the change feed reloads the page after another writer's
commit, and a protected branch or a reader's access makes the page read-only. The page
reads its tag from the graph it edits and is read-only when it cannot, so no edit leaves
without a precondition. The notes and members forms save through the forms of U02, whose
saves check the stored values of the fields they change instead of the commit. The page
locks them when a value, a nested node or the form's target graph is outside the graph it
edits, and locks a list while the dataset keeps any other graph, since a form names the
graph of a list's members but not of its cells. Until the head and its tag are read, the
page and its undo and redo are read-only.

**Label edits.** A SKOS-XL label resource that another resource still refers to stays when
one concept drops it or is deleted. Making a label preferred moves every alternative and
hidden representation of that literal to preferred and demotes the language's other
preferred labels, plain or SKOS-XL, to alternative labels. A conversion that waits for the
graph's answer is dropped when the page has moved to another resource, graph or dataset. A new SKOS-XL label's IRI is
checked against the whole graph. Blank-node SKOS-XL labels of one kind with the same
literal form are shown without edit buttons, since an update can find a blank node only
by its triples. Commit messages that are not ASCII are sent percent-encoded
as UTF-8, which also fixed the ontology editor's labels in other scripts.

The Rust tests check every condition against the consistent and inconsistent examples of
the SKOS Reference and its SKOS-XL appendix, the forms over the shapes, form checks of
edits that break S13, S14, S27 and S46, and the HTTP route. The UI's unit tests cover the
label rules, the queries and the updates against an in-browser store. Playwright covers
the tree, search, labels, links, moves, collections, notes, undo, stale edits and live
refresh against the mock backend, and edits on a session branch against a real server,
which leave main and the unrelated triples unchanged.

Roadmap item NEW-ITEM-1 covers paged trees and an indexed label search for large
thesauri, NEW-ITEM-2 labels for mapping targets in other datasets and mappings stated by
the other concept, NEW-ITEM-3 the editing of `skosxl:labelRelation`, NEW-ITEM-4 a write
guard that keeps a dataset's shapes beside the SKOS shapes, and NEW-ITEM-5 forms that
read and write one graph and name the commit a page shows.
