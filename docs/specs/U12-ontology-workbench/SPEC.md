# U12: Ontology workbench

> **Status:** partially complete. The ontology editor, DL Query and thesaurus editor
> phases are built, and the editable schema diagram is planned.
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

**Ontology browser.** Browse an ontology or thesaurus as linked articles, taking cues from the term pages of the EBI Ontology Lookup Service, including its graph view of a term, and Ontobee while keeping a clean, uncluttered layout. Search is a typeahead with typo tolerance over labels, synonyms, short forms and definitions in any language, ranking exact and prefix matches of preferred labels first, from an entity index that each commit updates and that answers within 20 ms on the server for ontologies of ChEBI's size. A command palette opened with `Ctrl+K` or `Cmd+K` reaches the same search, with datasets, saved queries, pages and actions, from anywhere in the UI. Each entity's page shows its label, IRI and short form, definition, synonyms and cross-references, an infobox of types and key annotations, a collapsible hierarchy tree centred on the entity with a graph view as the alternative, which draws the entity's superclasses, subclasses and related entities on the Explore canvas with its lenses and opens into Explore, its axioms rendered as prose with links, instances, the ontologies that reuse the term, a "what links here" list and downloads of the entity in each RDF format. An edit mode lets a caller with write access change the page in place, with the ontology editor's previewed commits, session branches and undo. The editor, the DL Query tab and the term pages of [C32](../C32-linked-data-publishing/README.md) reuse these pages.

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

The ontology editor phase is built as the UI's **Ontology** page, `/ui/workbench/ontology`. It reads
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
asserted hierarchy is drawn once and not followed. The class and property trees read one
level at a time from the hierarchy route, and every tree draws only the rows in view, so
ontologies of millions of classes open and scroll at the speed of a screenful. The
individuals tree has no hierarchy route and reads its individuals, up to 50,000, and their
types when its tab opens.

**Reads.** Opening the editor reads the summary and the first level of the tree on screen.
The other trees read when their tab opens. Names come from the hierarchy answers and from
batched reads of the labels of the IRIs a panel names for the first time. The entity lists
that the add fields' pickers and the new-entity check use are read once an entity, the
header, the DL Query tab or the new-entity dialog needs them, for each kind of up to 5,000
entities. The Manchester fields complete a longer kind through the entity search and the
server's Manchester reader resolves its names. This is item 725, whose measurements are in
the [web UI scale report](../../benchmarks/web-ui-scale/README.md).

**Inferred tree.** The **Inferred** switch shows the class taxonomy of the EL classifier
of [F24](../F24-owl-classification/README.md) beside the asserted tree. Its nodes are the
classifier's equivalence classes, so equivalent classes share a node, and the
unsatisfiable classes sit under `owl:Nothing`. Property and individual trees show the
asserted hierarchy only.

**Frame.** Selecting an entity opens its frame. The frame lists the entity's annotations,
its characteristics as checkboxes for properties, and its axioms in sections such as
SubClass Of, Equivalent To, Disjoint With, Domains, Ranges, Inverse Of, Types and the
property assertions of an individual. Class expressions are rendered in Manchester
syntax with labels, quoted where a label holds a space or a keyword, so the rendering
reads back as the same expression. Each axiom row shows its annotations and can be
removed. An add field per section takes an entity name, an IRI, a class expression or
data range in Manchester syntax, or one in functional-style syntax with the dataset's
prefixes. The fields of class expressions and data ranges are Manchester editors that
complete entity names, labels and keywords and underline the span of a problem the
server's reader reports, with the candidates of an ambiguous name. An **Add an axiom**
field at the end of the frame takes a whole axiom in Manchester or functional-style
syntax. Axioms that fit no section are listed last.

**Annotations.** The annotation editor adds, changes and removes annotations of an entity
or of one axiom. A value is text with a BCP 47 language tag or a datatype, or an IRI. An
annotation of an axiom is written as an annotated axiom, so the edit removes the plain
axiom and adds it with its annotations in one commit.

**View strip.** The entity kinds, **DL Query** and **Header** share one strip of tabs
above the panes. The strip wraps onto a second line when the window is too narrow for
it, so every tab keeps its whole label at phone width.

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
through a form remains for later. Rename covers the one graph the page
reads and does not reach IRIs inside triple terms. Real-time co-editing stays with the
U05 §1 open question. The page decides whether the caller can write from its level on
the dataset, so write grants on a branch alone leave the page read-only, roadmap item
611. Roadmap item 607 moves the trees onto the
[ontology browser](#ontology-browser)'s hierarchy route for large ontologies, 609
inferred property trees and 610 an IRI policy kept on the server with
the resource catalog of [C30](../C30-resource-catalog/README.md).

### Manchester input

Roadmap item 608 added the Manchester reader that the add fields use. The OWL crate's
`sparkles_owl::manchester` reads class expressions, data ranges, individuals, literals
and axioms in the W3C OWL 2 Manchester Syntax against a vocabulary of the ontology's
prefixes, local names and `rdfs:label` and `skos:prefLabel` values. Operator precedence
follows the note, with `or` below `and` and `that` and both below `not`, and a
restriction's filler is a primary. A name that several entities share is an error that
lists them, and every error carries its span, which the HTTP answer gives in UTF-16 code
units for the browser. Axioms are single frame lines, with the keywords of the class,
property and individual frames, property chains, characteristics and facts. Its
renderer writes a label only where the label names one entity, and property tests check
that its output reads back as the same value. `POST /$/ontology/{ds}/parse` and the
bindings' `parse` read text with the caller's grants
([API](../../API.md#ontology)). Whole Manchester documents with `Prefix:` and frame
blocks are not read, and stay with F21's syntaxes phase.

### DL Query

The DL Query phase is built as the **DL Query** tab of the Ontology page and the
`POST /$/ontology/{ds}/dl-query` route, with `ds.ontology().dl_query()` in the library
and `dl_query` or `dlQuery` in the Python, Node.js and JVM bindings. A query answers a
class expression's equivalent classes, direct and inferred superclasses and subclasses,
and instances. The tab's Manchester editor completes names, `Enter` runs the query, and
the query is kept in the page's address so that a link runs it again. The answers are
lists of names that open each entity's frame beside them, and the commit the page shows
runs the query again when it changes.

Inside OWL 2 EL the EL classifier of [F24](../F24-owl-classification/SPEC.md#outcome)
answers. The dataset's kept saturation reads `EquivalentClasses(urn:x-sparkles:dl-query C)`
beside the ontology's axioms, which it reads in place, saturates again only what the
axiom and any change since its last run reach, and gives the answers from the saturated
contexts without building a taxonomy. The next run over the kept state lets the axiom go,
so the ontology's classification is reused and its taxonomy left as it was. Outside EL the OWL 2 RL rules answer by demand reasoning over a copy in
memory of the terminology, the defining axioms and the assertions. Automatic reasoning
chooses EL when the expression and every logical axiom are inside it. Every answer
carries its reasoning label, which names the engine and why it was chosen, says whether
the class answers and the instances are complete, and lists the axioms outside the
profile. The tab shows the label as badges, with the gaps in a collapsed list. The RL
class answers are always marked possibly incomplete, since the RL rules are incomplete
for classification, and their instances are complete for an ontology inside OWL 2 RL.

The library tests check that the answers equal the kept taxonomy for every named class
of a small ontology with chains and transitivity, that a query leaves the kept
classification unchanged, and that an ontology with a range that meets an individual,
the case of item 591, reports the EL answer as incomplete. The server tests cover both
engines, the three syntaxes of an expression, the error spans and the caller's grants.
Playwright covers the tab, links that run a query, the underline of an unknown name,
completion and the Manchester add fields against the mock backend and a real server.
The instances need a classification with the assertions, which the dataset does not
keep, so an instance query classifies the ontology again. Roadmap item 588 can reuse
the same engine to answer the entity accessors from the kept classification.

### Thesaurus editor

The SKOS and SKOS-XL thesaurus editor phase is built as the UI's **Thesaurus** page,
`/ui/workbench/thesaurus`. It reads and writes one graph of a dataset with SPARQL queries and
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
lets them edit only a dataset that keeps no graph but the one it edits, and only when the
form's target is that graph. A form reads every graph and deletes a removed value's
nested nodes and list cells wherever they are, and its model names the graph of a list's
members but not of its cells, so no narrower check could keep a save inside one graph.
The check reads the stored default graph by its Jena name, `urn:x-arq:DefaultGraph`, so a
stored copy of a named graph's triple counts as another graph, and a default graph that the
server serves as the union of the named graphs does not. A caller whose grants cover only
some graphs or triples cannot see every graph the check needs, so the forms are read-only
for it. `/$/whoami` reports these limits for a dataset's main branch only, so with
authentication on, the forms are read-only on other branches for every caller but a local
one or a server administrator, until roadmap item 611 answers a caller's access per
branch.
Until the head and its tag are read, the page and its undo and redo are read-only.

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

Roadmap item 638 covers paged trees and an indexed label search for large
thesauri, 639 labels for mapping targets in other datasets and mappings stated by
the other concept, 640 the editing of `skosxl:labelRelation`, 641 a write
guard that keeps a dataset's shapes beside the SKOS shapes, and 642 forms that
read and write one graph and name the commit a page shows.

### Ontology browser search

The server part of item 616's ontology browser is built as an entity search,
`GET /$/ontology/{ds}/search`, with the Rust call `Ontologies::search` and `search` on the
ontology handles of Python, Node.js and the JVM. The browser's pages are described
under [Ontology browser](#ontology-browser).

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
characters with up to two typos answered within 8.3 ms at the 99th percentile on ChEBI
lite and 4.9 ms on GO, against the 20 ms target. ChEBI lite's index of 238,190 entities
took 1.3 s to build and about 233 MiB by its own estimate, 292 MiB resident, and GO's of
52,057 entities 0.64 s and 116 MiB resident. That memory, held per searched branch, is
the price of answering typeahead without reading the store. The search after each of 50
small commits applied the commit in under 3 ms with no rebuild. The
[entity search benchmark](../../benchmarks/entity-search/README.md) records the method
and the run.

**Remaining work.** A persisted index that avoids the build at the first search after a start is item
657, searches at a past commit or with inferred kinds are item 658, and
word segmentation for scripts written without spaces is item 659. The ontology
editor's tree filter, the DL Query entity completion and the thesaurus editor's label
search can move to this search under item 660.

### Command palette

Item 631 builds the command palette on the top bar's search of item 632, in
`ui/src/lib/launcher.svelte.ts`, `palette.svelte.ts`, `fuzzy.ts` and `recents.ts`.
`Ctrl+K` or `Cmd+K` opens it from anywhere, and `/` opens it outside a text field except
where a page registers a search of its own, which `/` then focuses. With nothing typed it
lists the viewer's recent picks, the open page's commands and the pages. A query searches
the entities through the server's ranking and ranks commands, pages, datasets, branches
and saved queries in the browser, and the prefixes `>`, `@`, `#` and `?` show one kind.
Pages register commands while mounted through `useCommands`, each with a label, an
optional shortcut, keywords, an `active` predicate and a run function. The recent picks
live in the browser's local storage, and the palette works without it. The command list
and its behaviour are described in the [visualization guide](../../visualization/README.md#app-shell).

**Remaining work.** The browser pages of item 616 register their search, which `/`
focuses, and their edit mode commands of item 630. Item 633's design
review adds commands to the pages that have none, such as Explore, Similar, Memory and
Embedding, under 689. The shortcuts the palette shows are written beside each
command and not read from the keymaps that bind them, which 690 unifies.

### Ontology browser

Item 616's pages are built as the UI's **Browse** page, `/ui/browse`, on two new routes,
`GET /$/ontology/{ds}/hierarchy` and `GET /$/ontology/{ds}/term`, with the Rust calls
`Ontologies::hierarchy` and `Ontologies::term`, the `hierarchy` and `term` operations of
the wire dispatcher, and `hierarchy` and `term` on the ontology handles of Python,
Node.js and the JVM. The hierarchy route is the lazy hierarchy that item 607 asked for.
The ontology editor's trees still read the whole graph, and item 607 now covers moving
them onto it.

**Why two routes.** The entity and summary routes open the whole ontology through the
reverse mapping, which reads every quad of GO at the first call and is not cached for a
caller with graph grants. Both new routes read the store through index scans around one
entity, so their cost follows the entity's neighbourhood and not the ontology's size.
They take `graph`, `imports`, `mode`, `branch` and `at` as the other ontology routes do,
read through the caller's masked view, answer the commit they read in the body and the
`ETag`, and refuse `inferences`.

**Hierarchy.** One request answers one level: the roots, or the children of a node, each
with its label, short form, kinds, deprecation and counts of children and parents.
Classes follow `rdfs:subClassOf` between named classes, properties
`rdfs:subPropertyOf`, and concepts `skos:broader` and `skos:narrower` in both directions.
Roots are the ontology header's `IAO_0000700` root terms when it names them, then the
top concepts of a scheme, and otherwise the entities with no parent of their kind, found
by one scan of the parent links. Children are ordered with deprecated ones last, then by
label and IRI, so pages are stable, and a level of more than 20,000 children falls back
to IRI order. `ancestors=true` adds the ancestors as a graph of nodes and edges up to the
roots, capped at 2,000 nodes, with a cycle drawn once.

**Term.** The route reads a fragment around the entity and maps it through the same
reverse mapping as the full read, so its axioms are the full read's axioms about the
entity. The fragment is every quad with the entity as subject in the graphs read, the
closure of the blank nodes those reach, the reified axioms whose source is the entity,
the axioms of other entities whose blank nodes hold it, found by walking up from each
blank-node subject that points at it, and the `rdf:type` quads of every IRI the fragment
uses, so the lenient mapping types the properties right. The other statements that
point at the entity come back as counts by property with the first subjects of each,
and a property's own uses as counts with its first subjects. `names` gives the label,
short form, kinds and deprecation of every IRI in the answer, so the page renders its
links without further calls. A test compares the fragment's axioms about each entity of
the test ontologies with those of the full read. `format=ttl`, `nt`, `rdfxml`, `jsonld`
or `ofn` answers the entity's own quads, or its axioms in functional-style syntax, as a
download.

**Pages.** The page's components, in `ui/src/lib/components/browse`, take their data as
properties: the header, the infobox, the lineage tree, the axioms as Manchester prose,
the synonyms, notes, labels and cross-references, what links here, the reuse in other
datasets, the graph view and the downloads. The prose comes from `manchesterTokens`,
which `manchester()` now joins, so the editor's text and the browser's links come from
one renderer. The landing page reads the header, the counts by kind and the graphs that
hold an ontology or scheme by SPARQL, and its search is the entity search. The edit
button switches the page into the edit mode of item 630, described below.

**Measurements.** A script timed the two calls an entity page waits for, `term` and then
`hierarchy` with its ancestors, for 500 random classes of each ontology, one after the
other over HTTP on the same machine. It ran against a release build of commit b23a2077
on a development laptop while other builds ran, so the numbers are an upper bound
for a quiet host. On GO, with 51,986 classes, a page took 2.6 ms at the median, 6.6 ms at
the 95th percentile and 10 ms at the 99th. On ChEBI lite, with 238,150 classes, it took
4.7 ms, 8.9 ms and 17.6 ms. The heaviest GO pages, the three top classes and `part of`
with its 150 KB of uses, answered within 60 ms on the first call. The landing page's
roots, which ChEBI lite does not name and the route finds by one scan of the parent links,
took 183 ms on the first call and 97 ms after it, for 19,430 roots. Every page stays far
under the 300 ms target.

The design review of item 633 reshaped the pages without a server change. The Definition
card lists every definition, description, comment and scope note under a heading that names
its kind and predicate, an Axioms card shows the entity's frame in Manchester syntax with a
classification's inferences, and SKOS statements are kept out of axioms. The header, the
counts as one row of chips and a More menu for buttons that do not fit are shared by the pages
of the UI. The Hierarchy card draws the graph in its own body. The Workbench editors' trees
show labels before short forms and keep their ancestors in view, and the two columns of
cards balance their heights. The Axioms card shows inferences only
after a **Classify** click, since no read-only route says whether a kept classification
exists, which item 726 adds.

Item 734 gave the term page a right rail without a server change. The rail holds Metadata, with
the cross-references as Identifiers by database and Download as the card's last part, and then
Hierarchy, which keeps its place across Tree and Graph and no longer moves between the columns.
Its Graph shows the parents and first children at the rail's width and Expand opens the whole
neighbourhood in a dialog. See also, the mappings, What links here, Replaces and In other
datasets are one References card under stronger sub-headings, the counts are one line of text
links, and the rail sticks even when it is taller than the window. Two questions stay with item
734, whether the main column keeps its cards and how to thin out the Definition card.

**Remaining work.** Inferred axioms and hierarchies beyond the card's click are items 622 and
726, search at a past state item 658, rendering preferences item 625, metrics item 621
and other servers' term pages item 461.

### Ontology browser edit mode

Item 630 lets a caller who can write to a dataset change a browser page in place. The
page offers edit mode when the caller has write access, the page reads the head of a
branch and not a past commit, the term route answered an entity tag, and the entity is a
SKOS concept or an OWL entity other than a datatype. `&edit=1` in the address opens it,
and the address drops it for a caller who may not edit. The **Edit** button, `E` and the
palette's **Edit this page** enter it, and the button hides while the edit bar is shown,
so the bar is the one sign of the mode.

**One write path.** Each change is one operation of the editors' `EditSession`, previewed
by `OntoPreview` as a dry run with `If-Match` on the term route's entity tag and committed
with the same tag. The term route's tag names the commit it read, so the axioms and
update routes accept it without a server change. OWL pages write `axioms` operations of
annotation assertions and hierarchy axioms, and SKOS pages write SPARQL updates built by
the thesaurus editor's functions. Undo and redo use the editors' history keys for the
dataset and graph, so a change made on a browser page can be undone in the editor and
the other way round. A protected branch disables the fields and offers **Start session**,
which creates the same session branch as the editors and moves the page onto it.

**What changes.** The definition is a text field with its language and its sources as
chips. A definition or synonym edit removes the old annotation assertion and adds the new
one with the axiom annotations that are not sources kept as they were, so a change of
kind or text never loses a cross-reference. The term route merges the annotations of
every reification of a triple into one statement and leaves out nested ones, so an edit
removes the annotation assertions of the term's axioms that state it, each as the store
keeps it, and adds one assertion with all their annotations. A SKOS statement with axiom
annotations is changed the same way, so its reification follows the new value. Synonyms, annotations and parents get edit
and remove buttons with an add row. Parents are picked through the entity search and
written as `SubClassOf`, `SubObjectPropertyOf`, `SubDataPropertyOf` or
`SubAnnotationPropertyOf`. Axioms are added in the frame's sections with the Manchester
editor's completion and parsed by the server, whose errors underline the text. A SKOS
page reuses the thesaurus editor's label table and concept pickers for labels, broader,
narrower, related and mapping concepts. Statements of other graphs than the edited one
are shown without buttons. The ontology editor's `describe` moved to `describeAxiom` in
`ontology.ts`, so the preview names the axioms in the same words.

**Errors.** A 403 from a write names the dataset and graph the caller can read but not
write, and a stale tag offers **Reload**, which reads the term and hierarchy again and
keeps the open forms' text. A definition keeps a typed draft when another writer changes
it, and a synonym being edited that another writer changes moves its draft to the add
row. A source typed and not added counts in the preview. `Esc` and **Done** ask before
leaving any form with an unsaved change, the shared annotation and label forms included,
and so does navigating to another entity, dataset, branch, graph or state.

**Keyboard and phone.** `Mod+S` previews the field with focus and prevents the browser's
save. `E`, `S`, `P` and `A` fire only when focus is outside text fields, contenteditable
elements and CodeMirror and no palette or dialog is open. Below 760 px the bar moves to
the bottom of the screen with **Undo**, **Redo**, a menu and **Done**, the hierarchy comes
before the article, and the thesaurus label table scrolls inside its section.

**Checks.** Playwright runs edit mode against the mock for each edit kind, undo and redo,
a session branch from a protected main, a reader, a refused and a stale write, keyboard
use, the palette and the phone, and against a real server for sources kept through an
edit, the Manchester parser, a stale tag after another writer's commit, a session branch
and a grant that leaves out the ontology's graph. The design captures of GO and UNESCO
pages in edit mode at 1440, 900 and 390 px in both themes show no sideways scroll. The
first preview on GO took about 5 s on a debug build because the axioms route opens the
whole ontology, and later previews took 0.16 to 0.4 s.

**Remaining work.** SKOS-XL labels and collection members are edited in the thesaurus
editor, and property assertions, keys and chains have no form of their own, 706.
The first preview's wait on a large ontology is 707. Phone dialogs dock as bottom
sheets after the design review of item 633.
