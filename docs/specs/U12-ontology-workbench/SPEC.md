# U12: Ontology workbench

> **Status:** partially complete. The ontology editor, DL Query, thesaurus editor and visual SHACL editor
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
  The [import resolution](#import-resolution) catalog upload and fetch are the
  exceptions. They are confirmed actions with the same access checks and conflict
  handling, because their content comes from files or the network.
* Round trips through the axiom model leave unrelated triples unchanged.

## Non-goals

* A desktop application. The workbench is part of the web UI.

## Design constraints

The ontology-editing exclusions of U02 §1 and U07 §1 are lifted for this spec.

## Import resolution

Item 624 resolves `owl:imports` within the dataset. Imported content is always an
ordinary named graph of the dataset, so history, exports, backups, grants and protected
branches treat it like any other graph, and no read fetches anything.

### Import map

Each dataset keeps an import map in its `ontology-imports.json`, a settings file with a
format version that a reader refuses when the version is newer than it knows. The map
sends an import IRI, or a version IRI that an `owl:imports` names, to one named graph of
the dataset. Each entry records its source, which is `manual`, `catalog` or `fetch`. A
fetched entry also records the location fetched, the branch and commit that the fetch
wrote, the response's ETag, the SHA-256 digest of the bytes and the time of the fetch.
The map belongs to the dataset, so every branch shares it, and a read of an earlier
commit uses the current map. A map holds at most 4,096 entries, and each IRI and graph
name is at most 4 KiB.

The map is not a C19 settings kind, because the settings routes answer every reader
with the whole object. `GET /$/ontology/{ds}/imports` answers the map with an entity
tag of its revision. A caller with `admin` on the dataset sees every entry. Any other
caller sees only the entries whose graph holds triples that the caller's masked view of
the branch shows, so a graph whose triples are all hidden counts as absent.
`PATCH /$/ontology/{ds}/imports` sets and removes entries with `admin` and an `If-Match`
that names the map's revision. A change without that header or with `*` is refused with
`428`, and a set value may be a whole entry so that an unwritten entry can be replayed. Every write of the map holds the dataset's import-map
lock, so map writes are serialized.

### Resolution order

A read with imports takes each `owl:imports` IRI of the graphs it reads and tries three
sources in turn.

1. The import map's entry for the IRI resolves it to the entry's graph.
2. A graph of the dataset named by the IRI resolves it, as F21 reads did before.
3. The C30 vocabulary resources resolve it when one of their namespaces is the IRI, or
   the IRI followed by `#` or `/`. Several resources can match, and the import then
   resolves to every graph they list in this dataset, each once, in the order of the
   resources' ids.

A candidate graph counts only when the caller's masked view shows triples in it. An
import that no source resolves is unresolved, and an answer never names a graph the
caller cannot read. The server reads the resource catalog for this lookup itself, and an
answer shows only the resolved graphs, never a resource or another dataset's
locations. The read follows the imports of every graph it adds, up to the 100 imports
F21 allows, and a graph is read once however many imports lead to it. Without map
entries or matching vocabulary resources, a read resolves exactly as before. Reasoning
runs keep their own `locationMapping` and do not consult the map.

### Closure and caching

The ontology summary gains a `closure` member with one edge for each `owl:imports`
triple of a graph read. An edge names the importing graph, the import IRI, the graphs it
resolved to and the source that resolved it, which is `map`, `graph` or `resource`. It
also says whether it closes a cycle, which it does when one of its graphs is the
importing graph or imports that graph through other edges. An unresolved edge names no
graph.

A change to the map or to the vocabulary resources changes what a read resolves
without a commit. Every read therefore takes the map and resource entries it could use
as part of its request. The ontology cache keys a kept read by a digest of those
entries, and the entity tags of the summary, the hierarchy and the term reads add the
digest after the format, as in `owl.<digest>`. The tag parser of conditional writes
reads the format before the dot, so a tag that names the commit still satisfies an
edit's `If-Match` whatever its digest, because an edit checks the commit.

### XML catalogs

`POST /$/ontology/{ds}/imports/catalog` takes a multipart body. Its `catalog` part is an
OASIS XML Catalog, such as the `catalog-v001.xml` that Protégé writes, and each further
part is a local file that the catalog names, keyed by the path the catalog gives. The
server parses the catalog without resolving anything outside the body. A document type
declaration or an entity declaration is refused, element nesting is bounded at 32
levels, and a relative path names a part of the body and nothing else.

Each `uri` entry, at the top of the catalog or in a `group`, maps its `name` to a graph.
When the entry's `uri` is a relative path and its file is part of the body, the file
replaces the graph named by the entry's `name`. The file's syntax comes from its media
type or its extension. When the entry's `uri` is an absolute IRI, the name maps to the
graph of that IRI and nothing is loaded. The answer lists the entries it skipped and
why. These include `rewriteURI`, `nextCatalog` and other entry types, an entry under an
`xml:base`, and a relative `uri` whose file is not in the body. A catalog that is not
well-formed XML, or that has no usable entry, is refused before anything changes. The
catalog is at most 1 MiB with at most 1,000 entries, and the whole body stays within the
server's upload limit.

### Fetch into graph

`POST /$/ontology/{ds}/imports/fetch` takes the import IRI, and optionally a location
and a graph. The location defaults to the map entry's location and then to the IRI, and
it must be an `http` or `https` URL. The graph defaults to the map entry's graph and
then to the IRI. The server fetches the document through its outbound policy, under the
policy's destinations, timeout and response ceiling, and replaces the graph with the
parsed document in one commit on the branch. It then writes the map entry with source
`fetch`, the location, the branch and commit, the ETag, the digest and the time.

A refresh is the same action, and it always downloads the document again without a
conditional request, since the recorded ETag belongs to one branch and one location.
The map entry is always written in full with the graph, location, branch, head commit,
ETag, digest and time of this fetch. When the document has no blank nodes and equals
the graph's current triples on the requested branch, the graph is left alone and no
commit is made. A document with blank nodes replaces the graph's triples in a commit. A refused
destination, a timeout or a document that does not parse changes nothing. Reads still
never fetch, and item 559 keeps fetching on read.

### Writes, grants and failures

The catalog upload and the fetch change the map, so they need `admin` on the dataset.
They follow the branch's write rules, so a protected branch refuses them before anything
changes, and they take an optional `If-Match` that names the branch's head commit, as
other writes do. Each one holds the import-map lock from before its commit until its map
write ends. It checks the new entries against the map's bounds before the commit. If the
map write still fails after the commit, the answer carries the commit and the entries it
did not write, the loaded graphs stay, and a `PATCH` of the map with those entries
finishes the work.

### Editor

The ontology editor gains an **Imports** tab. It draws the closure as a tree from the
selected graph, marks cycles and unresolved imports plainly with their IRIs, and lists
the map with each entry's source, location, ETag, digest and time. An administrator adds
and removes entries, uploads a catalog with its files and fetches or refreshes an
import. A catalog upload and a fetch are confirmed actions rather than previews, since
their content comes from files or the network, and their commits show in the history
and the diff like any other. Readers and protected branches see the closure and the map
without the actions.

A toolbar switch includes or excludes imported axioms in the editor's trees, frames,
usage, metrics and DL queries, by sending `imports=true` or `imports=false` on every
read, and the page keeps the choice in its URL. Imported axioms are included by
default, as before. Edits keep writing to the selected graph.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: VocBench, PoolParty, the EBI Ontology Lookup Service, Ontobee, EDG Taxonomy, EDG Tagger, TopBraid Composer, Protégé, WebProtégé, Protégé DL Query, Cellfie, ROBOT template, DOSDP, Stardog Designer and OntoGraf. No code from another project is copied.

The visual SHACL editor follows the [W3C SHACL Recommendation](https://www.w3.org/TR/shacl/)
for shape definitions, Core parameters, RDF lists and property paths. Its additional list
parameters follow Sparkles' supported SHACL 1.2 draft contract in
[G03](../G03-shaclc.md). RDF editing, exact patch preconditions, branch reverts and sample
validation reuse Sparkles' own F09, F10, C15 and SHACL implementations.

## Outcome

### Visual SHACL shape editor

Item 617 adds **SHACL shapes** to the Workbench at `/ui/workbench/shapes`, beside the
dataset's existing Turtle and SHACLC text validation experience. It reads one complete
stored graph at an exact branch revision and edits the RDF directly. Node and property
shape trees include anonymous, nested and shared definitions. A spanning forest draws
recursive and repeated references as link leaves and reuses the virtualized tree.

Forms author the four Core target predicates and implicit class targets, severity and
descriptive metadata, the Recommendation's Core constraints and the engine's four
SHACL 1.2 draft list constraints. Shape references and ordered lists express logical,
qualified and nested constraints. Paths compose predicates, inverse, sequence,
alternative, zero-or-more, one-or-more and optional forms recursively. Unsupported RDF
remains visible and unchanged, and malformed structures require explicit replacement.

Field edits accumulate in a local draft with bounded undo and redo. Commits apply only
the RDF delta through the existing patch endpoint, with the source branch UUID and
commit in `H prev`. The same captured patch is previewed and committed. This preserves
stored blank nodes and unrelated triples, including shared path and list structures.
The strict `expect` commit IRI also checks the source UUID, so a foreign replication
`prev` cannot bypass authoring preconditions after a dataset or branch is recreated.
Committed undo and redo use reverts. A refresh retains active field buffers and dirty
drafts at their original revision, and a moved or failed source disables submission.
Discard and reload is explicit. Every accepted refresh remounts the field forms, so a
form never keeps a list or path buffer from an earlier revision. Change-feed refreshes
wait while a preview is open, so a commit whose answer arrives after the feed announced
it is still recorded for undo. Protected branches use session branches, and the server
retains its graph, endpoint and write authorization.

Live sample checks use the complete draft and explicit focus nodes or the declared
targets in a selected sample graph at the captured revision. The latter mode includes
literal target nodes. Data-graph selection, deadlines and cancellation are independent
of the shapes graph. The existing
validation route gained opt-in `storedBlankNodes=true` for inline RDF drafts, so their
stored blank constants remain comparable with the data. Inline drafts remain distinct
from shapes already stored in the snapshot and do not bind `$shapesGraph`. Ordinary
inline parsing retains its previous document-local blank-node behavior. The UI checks
the response source UUID and commit before reading a sample report, so replacement
branches at the same numeric head cannot approve a draft from an earlier incarnation.
Reserved default and union query selectors are refused as named authoring sources;
the stored default graph is selected by leaving the graph field empty.

The editor refuses graph reads above 5,000 triples or a 2 MiB decoded response before
JSON materialization, as well as incomplete reads and unconfirmed triple visibility.
Paths nest at most 64 levels and expand to at most 1,000 steps, which bounds acyclic
paths that reuse shared operands. A replacement path or list is checked against the
triple limit after the structure it replaces is collected. It retains at most 30 draft states, validates at most 100
explicit sample nodes, displays at most 100 sample results and previews 50 changed
triples. The full graph remains the basis of every patch. A larger graph is refused
rather than opened as an editable partial definition. The extra session memory grows
with the retained draft states and graph size.

The [visualization guide](../../visualization/README.md#visual-shacl-shape-editor) describes
the controls and bounds. Text transfer explicitly replaces the saved Turtle draft and
does not install a guard or commit a shape graph. General classifier-backed entailments
and justifications remain separate work under F24.

### Entity history

Item 620 adds a **History** view to the ontology editor's entity frame, beside **Frame**
and **Usage**. It lists the commits of the branch that changed a triple naming the entity
as subject, predicate or object in the editor's graph, newest first. The view asks
`GET /{ds}/history` once for each position with `order=desc`, the graph, at most 200
changes and the same `to`, which is the head the editor shows for the first page, so a
commit that lands between the three lookups cannot be counted by only some of them. It keeps only the commits above the oldest commit at which a cut lookup
stopped, so a commit is never shown with part of its changes. The next page reads up to
that commit again. A single commit larger than one page is shown alone with its counts
marked partial, and the next page starts below it. A triple that names the entity twice
is counted once. Bulk loads, gaps and commits older than the change log are named at the
end of the list.

Opening a commit reads `GET /$/ontology/{ds}/diff` from its parent to it, without
imports and with assertions, and the route's new `entity` parameter keeps only the
axioms that mention the entity, as the usage view reads them. At most 100 axioms are
listed on each side, with the full diff linked for the rest. A caller who can edit the
branch reverts a commit through the editor's preview, which runs `POST /$/revert/{ds}`
as a dry run against the head the page shows and commits it with the same expectation.
The revert enters the session's undo stack. The server still requires write access to
the whole branch for a revert. Readers, protected branches and callers whose grants cover
only some graphs see no revert button. An opened commit's answer that arrives after the
view moved to another entity, graph, branch or head is dropped, and unrecorded ranges
are kept across pages.

The mock backend answers the entity diff from its recorded states. Unit tests cover the
page assembly and the lookups, the server test covers the entity filter, and Playwright
covers the list, an opened commit, a revert, an empty history, a reader without the
revert and the phone layout against the mock, and the same flow against a real server.
A commit that changed only blank-node triples of a restriction or an annotated axiom is
not found by the three lookups, and item 765 covers following those structures.

### Discussions

Item 618 adds a **Discussion** view to the entity frame, beside **Frame**, **Usage** and
**History**, and a **Discussions** tab to the ontology editor. The view lists the threads
on the entity and on its axioms with their replies, and a caller who can write the branch
starts a thread, replies, and resolves or reopens a thread. The tab lists every thread of
the ontology graph and opens a thread's entity in the Discussion view. Both read with one
SELECT of at most 1,000 comments and replies, oldest first, and say when it stopped there.
Later comments and replies are then not listed, and paging them is left open.

Threads are stored as the [C31](../C31-checks-and-protected-merges/SPEC.md#outcome)
comments phase specifies, as W3C Web Annotation RDF. The maintainer chose the simple
storage. The annotations live in an ordinary named graph per ontology graph on the
branch, whose IRI is the ontology graph's whole IRI under
`urn:x-sparkles:discussions:` with `%`, `[` and `]` percent-encoded, since an IPv6 host's
brackets may not stand in a URN, or `urn:x-sparkles:discussions:default` for the default
graph. A name such as `<graph>/discussions` was set aside, because it either merges the
threads of `onto`, `onto/` and `onto#` or can fall on a real ontology graph. The namespace
keeps one discussion graph for each ontology graph and lets the editor recognize
discussion graphs by their prefix. A thread is an
`oa:Annotation` motivated by `oa:commenting`. Its `oa:hasTarget` is the entity's IRI, or
for an axiom an `oa:SpecificResource` whose `oa:hasSource` is the entity and whose
`oa:TextQuoteSelector` holds the axiom in functional-style syntax as the frame shows it.
A reply is an `oa:Annotation` motivated by `oa:replying` whose target is the thread. Each
annotation has an `oa:TextualBody` with `rdf:value` and `dcterms:format`, a
`dcterms:creator` IRI under `urn:x-sparkles:user:`, a `dcterms:created` time and one
`schema:mentions` IRI for each `@name` in its text. A thread carries
`urn:x-sparkles:discussion#resolved` as a boolean, and a resolve adds
`urn:x-sparkles:discussion#resolvedBy` and `dcterms:modified`. Annotation IRIs are fresh
`urn:uuid:` IRIs. No discussion triple uses `rdfs:comment`, and no U05 system graph or
separate store is involved.

Each comment, reply, resolve and reopen is one `POST /{ds}/update` on the branch with a
commit message, so it is an ordinary commit. These consequences are accepted. Comments
appear in the branch's history and in exports like any other graph, protected branches
refuse them like any write, and writing needs `write` on the dataset, or a grant on the
discussion graph for a caller whose grants cover only some graphs. Readers and protected
branches see the threads without a composer or actions. The editor reads only the
ontology graph and its imports for frames and trees, and leaves discussion graphs out of
its graph list, so its axiom views never show the annotations. A comment's commit moves
the branch head, and the editor reads its views again so the next edit names the new
head. The session's undo stack does not take comments back.

Unit tests run the writes and the SELECT against an in-memory store. Playwright covers a
thread on the entity and one on an axiom, a mention, a reply, a resolve and a reopen as
five commits, the ontology's tab, a reader and a protected branch against the mock, and a
writer, a reader and a protected branch against a real server. Mentions are recorded but
notify no one until the notifications of item 46 exist. Item 769 covers an optional
discussion store outside versioning, with comments allowed on protected branches and
kept out of exports.

### Watches

Item 619 adds watches. A watch belongs to an identity and names an entity IRI in one
graph of one branch, alone or with its asserted subtree, with an optional depth for a
subtree. The server keeps watches and their matched commits in `<dataDir>/watches.json`,
outside every dataset, so they are neither committed nor exported. The routes are
`/$/watches`, which [API: Watches](../../API.md#watches) describes.

These points settle what the candidate phase left open. The maintainer decided the
first five.

- An owner learns of matches in the web UI, read with the owner's live access. A
  delivery outside the UI goes only through an email channel that a server
  administrator marks for watchers, to the owner's verified address. The ordinary
  routes of C21 never carry a watch.
- A subtree is the asserted hierarchy of `rdfs:subClassOf`, `rdfs:subPropertyOf`,
  `skos:broader` and the inverse of `skos:narrower`. A follower thread reads each
  watched branch's commits in order. It walks upward from each IRI that a commit's
  changes name, in the hierarchy before the commit and after it, and matches the
  subtree watches whose entity the walk reaches. The cost of a commit is its changes
  times the ancestors each walk visits, and it does not depend on the size of a subtree.
  The hierarchy of each watched graph is kept in memory, about 100 bytes per edge, and
  is kept current from the commits, so the state before and after each commit is exact.
  This memory buys exact matching without reading past states.
- A watch is refused while triple protections limit what its owner reads, because
  History and the change feed do not support them.
- Commits by the owner are listed and marked `own`. Watches made through a token belong
  to the token's owner and outlive the token. A watch keeps 200 matches.
- The bounds `maxAncestors`, `largeCommit`, `identityMaxAgeDays` and `batchSecs` are the
  `watches` member of the `notifications` kind.
- Every route and every match is authorized again. The follower computes the owner's
  current grants from the policy with the groups of the owner's last use of a watch
  route, and an owner who is gone or can no longer read the graph matches nothing. A
  watch stores its branch's id, so it follows a rename and is removed with the branch.
- A commit whose changes are not listed, such as a bulk load, and a gap the change feed
  no longer holds, match every watch of the branch as unverified.

Tests in `crates/sparkles-server/src/watches/tests.rs` cover entity matches in the three
positions, own commits, unread counts and marking read, a class moved into and out of a
subtree between two runs of the follower, root changes, deletions, a depth limit, a
summary entry for a large commit, a renamed and a deleted branch, graph grants, triple
protections, an owner who no longer exists, a refused file, and a restart that keeps the
matches and the cursor.

### Bulk edits

Item 626 adds a **Bulk edits** tab to the ontology editor, beside **Discussions**, for the
bulk part of the "Patterns, imports, rendering and bulk edits" phase. It offers find and
replace, setting and removing an annotation, and moving classes. Each builds one
`POST /$/ontology/{ds}/axioms` edit from the axioms the editor read and hands it to the
editor's preview, so the dry run, the shared change set, `If-Match` on the shown commit
and the undo stack are the ones every other edit uses. No server route was added. The
annotation edit declares an annotation property that the ontology does not declare yet, so
its values read back as annotations, and the subjects of annotation assertions are not
declared. Setting a property that the ontology or its imports type as another kind, such
as a data property, is refused before the preview, since its declaration as an annotation
property would change how the existing assertions read. A move declares nothing, and its new
parent must be a class of the ontology or its imports, or `owl:Thing`. Each moved entity
must be a class or an IRI the ontology does not know, since a `SubClassOf` would otherwise
turn a property or an individual into a class. The kinds of the set
property and of the new parent come from `GET /$/ontology/{ds}/entity` at the shown commit
with imports, so they follow the ontology's lenient read. The kinds of the moved entities,
up to 1,000, come from their `rdf:type` values in the graphs the ontology reads, through one
SPARQL query for each 200 IRIs, since an entity report for each would map the ontology's
axioms again for each IRI. A declaration type gives its kind, a property characteristic or
`rdf:Property` a property, and any other type an individual. An entity that the lenient read
knows only from its use, such as an untyped subject of a data property assertion, is not
caught by this check, and item 771 covers a batched kinds read. A calculation that a new
input or leaving the tab supersedes aborts its queries. It reads and writes the graph
without its imports, as it was planned, so an imported copy of a new value does not stop the
server from adding that value to the edited graph.

Find and replace reads the graph's annotation assertions without its imports at the
displayed commit through `GET /$/ontology/{ds}/axioms?type=AnnotationAssertion&at=`. It
matches plain and language-tagged strings only, so IRIs and typed literals such as
booleans and dates are never rewritten. Matching is literal or a JavaScript regular
expression with the Unicode flag, with or without case. A regular expression's
replacement may use groups, and a literal replacement is inserted as written. A pattern
that matches empty text is refused, since it would insert the replacement between
characters. This covers a pattern that matches only empty text and one, such as a lookahead
or a word boundary, that matches empty text inside a value it is applied to. The property scope is the labels (`rdfs:label`, `skos:prefLabel`,
`skos:altLabel` and `skos:hiddenLabel`), every annotation property or one property. The
language filter is any language, untagged values, or one tag by the basic filtering of
RFC 4647, so `en` matches `en-GB`. The entity scope is the whole graph or the entity list.
Each change removes the stored annotation assertion with its axiom annotations and adds
the new value with the same language and annotations. A value that the replacement would
leave empty is left out and counted, because removal has its own form.

Set annotation adds a value to every entity of the list. With replace, it also removes
the property's other values in the new value's language, compared without case, or every
other value when the new value is an IRI. Values compare as RDF terms, with language tags
in any case and a plain literal equal to an `xsd:string`, so an entity that already holds
the value is counted and left alone with its axiom annotations. Remove annotation removes
every value of a property from the listed entities, the values in one language, or one
value. A value given as an IRI removes only that IRI, and one given as text removes only
literals of that text.

Move classes reads the graph's `SubClassOf` axioms. Each listed class loses the axioms
that state a named superclass other than the new parent, annotated ones included, and
gains `SubClassOf(class, parent)` unless it is already stated. Superclass expressions such
as restrictions stay. Keeping the old parents only adds the new one, and moving to the top
removes the named superclasses and adds nothing. A parent that is one of the classes or
below one of them through the graph's named `SubClassOf` axioms is refused before the
preview, naming the class it is below. A new parent's annotations are not copied from the
old axioms, because they described the old relation.

The entity list takes an entity from the entity search, a class with its subclasses
through the graph's named `SubClassOf` axioms, or pasted IRIs and prefixed names, and
reports text that names neither. The panel lists every change with its entity, property
and old and new value, showing the first 200 rows.

Four bounds apply. An edit changes at most 1,000 annotation values or classes, and above
that the panel names the count and the bound and offers no preview. The bound keeps the
preview readable. A move also refuses to add and remove more than 10,000 axioms in all,
which a few classes with many named parents could otherwise reach, so the edit's answer
lists every axiom it changed. The entity list holds
at most 1,000 entities. A read of more than 100,000 axioms of one type refuses the edit
rather than editing from a partial read that could miss matches. The read holds the
graph's axioms of that type in the browser for the tab's lifetime at one commit, which
costs memory in proportion to the graph's annotations and is the price of matching in
the browser without a new server route. A commit at another head, graph or branch clears
the listed changes. Readers and protected branches list changes but get no preview, and
the server still refuses their writes.

A calculation still reading the graph is dropped when the operation, the commit, the graph
or the branch changes, and every input of a plan is taken before the read, so a plan never
mixes the fields of two operations.

Unit tests cover matching, the filters, the replacements, empty matches, set, remove, value
identity, the move plan, the cycle refusal, the bounds and the entity list. Playwright covers find and replace with its
undo as one commit and its writes without imports, a regular expression that keeps a
label's axiom annotation, an empty match and an invalid pattern, an operation switched while
the graph is read, set and remove, moves with
an annotated `SubClassOf` and a refused cycle, an edit above the bound, a reader at phone
width and a protected branch against the mock, and against a real server a find and
replace on a branch that keeps an annotated label's annotation, adds a value that an imported
graph already holds, and leaves main and the imported graph alone, a move whose undo restores
an annotated axiom's reification, the bound, a protected branch and a reader under
authentication. Item 770 covers matching on the server and splitting larger edits.

### Import resolution

Item 624 resolves imports within the dataset, as the
[Import resolution](#import-resolution) design describes. The maintainer chose this design
over a server-wide store of pinned source versions loaded on demand into a cached layer
outside the branch's history. That store would keep imported copies out of exports and
share them across datasets, but it needs the C30 source store, the durable jobs of item
339 and a cache format of its own.

`sparkles_reasoner::inputs::resolve_closure` resolves an ontology read's imports through
`ImportTargets`, the map's entries and the vocabulary resources' graphs, and records one
edge per `owl:imports` triple with its cycle flag. `resolve_readable` calls it without
targets, so reasoning runs and reads without targets resolve as before. The ontology
handle's cache key and the entity tags of the summary, the hierarchy, the individuals and
the term reads carry a stable digest of the targets, and `If-Match` reads the format before
the digest. The server keeps the map in the main dataset's `ontology-imports.json`, format
1, in `crates/sparkles-server/src/http/ontology/imports.rs`, and reads the resource catalog
on every ontology read, which costs one parse of `resources.json` per read while the file
exists.

The design's equal-document rule applies only to documents without blank nodes, since
their blank nodes cannot be compared without canonical labelling. A catalog upload or a
fetch of a document with blank nodes replaces the graph's triples and commits. A refresh
always downloads again, because the recorded ETag belongs to one branch and one location.
The catalog reader takes `uri` entries only. The editor offers file choice for a catalog's
files, keyed by file name or by the path below a chosen folder.

Unit tests cover the resolution order, readable and empty candidates, the resource union,
the closure's cycles and its edges past the import bound, the digest, blank nodes in triple
terms and composite literals, the catalog reader's refusals of document types, entity
references, deep nesting, truncated documents, skipped entries over the bound and leaving
paths, the catalog plan and the map's bounds. Router
tests cover the map with its required revisions, replayed entries and the changed tags, a digest-bearing `If-Match`,
a catalog that loads, maps, skips and then changes nothing, a refused catalog and file that
change nothing, a protected branch, a fetch and an unchanged refresh, a refused file
location, an outbound refusal that changes nothing, and under auth the admin requirement
and the reader projection, which hides entries whose graph is empty on the branch. Playwright covers the closure with a cycle and an unresolved
import, a map entry, a catalog load, a catalog folder with a file in a subfolder, a fetch,
the switch and a map change that read the tree, the filtered tree and the frame again, a
reader at phone width and a protected branch against the mock, and against a real server a map entry that makes no
commit, a catalog on a branch that main does not see, a refused fetch, a protected branch
and a writer who is not an administrator. Item 772 connects reasoning runs to the map, item
782 covers more of the XML catalog standard, and item 783 covers per-branch maps.

### Ontology metrics and expressivity

Item 621 adds an on-demand metrics panel to the ontology Header view and the ontology
browser landing page. It shows axiom counts by type, distinct signature entities and
EL, QL, RL and DL reports from one authorized read, including streamed assertions. The
Header pins its displayed commit and includes dataset-local imports. The browser uses
its selected graph and revision without imports, matching its landing page. A changed
scope clears the panel and cancels its previous read.

The panel labels its DL notation as a conservative containing logic rather than minimum
expressivity. Simple conjunctions and existential restrictions use EL. Advanced
constructors extend ALC through SROIQ(D). Mapping errors, unresolved imports, stale
inferences, keys outside the notation or failed DL checks leave overall expressivity
unknown and show the reason. An observed fragment bound remains available when sound.
The profile reports apply to the mapped scope, without claiming an unresolved closure
is complete.

The opt-in summary read and the shared binding operation are documented in
[the ontology API](../../API.md#ontology). Terminology counts are distinct axioms and
streamed assertions count occurrences, with separate columns so repeats across graphs
are explicit. Entities include undeclared references and built-ins and count once per
kind and IRI. The read streams assertions and keeps an additional entity signature set,
so its memory grows with the signature. It refuses scopes above one million mapped
axioms and assertion triples or signature entities, and returns errors on a limit,
deadline or cancellation rather than a partial profile proof. At most 100 violations
per profile are listed after the full selected scope is checked. Ordinary summary
reads do none of this additional work.


### Ontology editor

The ontology editor phase is built as the UI's **Ontology** page, `/ui/workbench/ontology`. It reads
and edits one graph of a dataset, the default graph unless the caller picks another, with
the graphs that graph imports. Every read and write goes through the `/$/ontology/{ds}`
routes of [F21](../F21-ontology-api.md#outcome), with the caller's grants, and through
`/{ds}/update` for renames and header edits.

Mounted frame drafts retain their original target, graph, entity and commit through a
refresh. A stale or failed refresh disables submission at the form and central dispatch
boundaries. When replacement data arrives, active drafts keep their older capped frame
and text until explicitly discarded, while inactive frames update automatically. Delayed
parse results cannot become edits at another source, and preview/commit share the
precondition captured when an edit was accepted.

**Trees.** The page has trees of classes, object properties, data properties,
annotation properties and individuals. Each tree shows the asserted hierarchy from the
`SubClassOf`, `SubObjectPropertyOf`, `SubDataPropertyOf` and `SubAnnotationPropertyOf`
axioms, and the individuals are listed with their asserted classes beside each name. Entities are named by
their `rdfs:label` or `skos:prefLabel` in the browser's language, with the prefixed name
beside a label. A filter keeps the matching entities and their ancestors. A cycle in the
asserted hierarchy is drawn once and not followed. The class and property trees read one
level at a time from the hierarchy route, and every tree draws only the rows in view, so
ontologies of millions of classes open and scroll at the speed of a screenful. The
Individuals tab reads type buckets and named members through the paged individuals route.
Each type expands independently, and Untyped holds named individuals without an asserted
named class. Multiple types place an individual once under each asserted class. Anonymous
individuals remain outside this named-entity tree. The first page uses the displayed
summary commit, and later reads stay at that revision. A link locates its individual without reading preceding
pages. Type buckets use separate row identities, so a class that is also an individual
does not create a cycle. Names use the shared rendering and ordered language preferences.
This is item 607. Item 749 covers the repeated discovery, lexical-page selection and
exact-total scans that remain behind these bounded responses. Server root discovery and
Untyped pages scan the selected scope, and the route retains F21's cold terminology
mapping cost tracked by item 736.

The Individuals filter searches names at the current branch head and reads each candidate's
memberships at the displayed commit. It skips candidates absent from that revision, keeps
valid matches and displays hard read failures. Historical name search remains under item
658.

**Reads.** Opening the editor reads the summary and the first level of the tree on screen.
The other trees read when their tab opens. Names come from the hierarchy answers and from
batched reads of the labels of the IRIs a panel names for the first time. The entity lists
that the add fields' pickers and the new-entity check use are read once an entity, the
header, the DL Query tab or the new-entity dialog needs them, for each kind of up to 5,000
entities. The Manchester fields complete a longer kind through the entity search and the
server's Manchester reader resolves its names. This is item 725, whose measurements are in
the [web UI scale report](../../benchmarks/web-ui-scale/README.md).

**Inferred tree.** The **Inferred** switch reads kept named-class roots and expanded
levels beside the asserted tree without computing. Equivalent classes share a node,
and unsatisfiable classes sit under `owl:Nothing`. A deliberate **Classify tree** action
retains the full-taxonomy POST flow with the actual EL, Horn or disjunctive result of
[F24](../F24-owl-classification/README.md). Matching fresh RDFS or OWL RL materializations
also supply bounded property hierarchies and named individual types. General
classifier-backed property and individual entailments remain under items 588 and 609.

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
611. Roadmap item 609 adds inferred property and individual trees, and 610 adds an IRI
policy kept on the server with
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
Node.js and the JVM. The ontology editor reads its class and property levels through
this hierarchy route. Item 607 adds the separate paged individuals route for its
Individuals tab, with matching native reads on the three binding handles.

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
cards balance their heights. Item 726 adds the bounded read-only kept-classification
route used by Browse and editor named-class frames. A page reads availability and then
direct-parent/equivalence pages at its displayed branch/commit, with one immutable result
pin and labels from that source. Automatic loading, retry, paging and toggles never POST
to classify. Empty frames keep their outcome region visible, and partial page errors,
replacement, exclusions, unsatisfiability and inconsistency have distinct notices.

A deliberate **Classify** click asks for one bounded frame projection when no matching
result is available. Restricted views retain useful authorized clauses from that explicit
computation, with actual producer/source/outcome and bounded diagnostic samples. They do
not advertise a shared continuation or silently recompute for more rows. An eligible
publication supports bounded GET continuation. The separate explicit full-tree action
retains its legacy multi-POST transfer, pins every page and name batch to its original
commit and input, and compares complete deterministic content/outcome fingerprints
before assembling pages. The full-tree transfer is not claimed bounded by frame limits.
Each frame part has a 1,000-row browser display cap and reports unread totals. Historical
names fall back to complete IRIs when no trustworthy prefix map is available.

The inferred class hierarchy now reads kept roots and expanded children in bounded GET
pages. Equivalent selections reveal their canonical group while retaining the actual
selected IRI. Browse and Workbench also show materialized object/data-property hierarchies,
individual named-type groups and read-only derived frame rows from a matching fresh RDFS
or OWL RL producer. The hierarchy reduces transitive closure edges between equivalence
groups. Regions at one displayed revision share a producer pin and refuse replacement,
while source and entity changes abort and discard prior reads. Missing or stale sources
remain visible failures and never start implicit computation. Actual inferred-graph
provenance and duplicate asserted facts are shown. Profile completeness, consistency and
arbitrary-axiom explanations remain unknown or unavailable. General classifier-backed
entailments and matching F24 justifications remain in items 588, 622 and 609. The existing
mapper and exact-total scans can cost more than a page's output size, and this phase
establishes no new cold or warm latency claim.

Item 734 reworked the term page's layout without a server change. A right rail of cards holds
Metadata, with the cross-references as Identifiers by database and Download as the card's last
part, and then Hierarchy, which keeps its place across Tree and Graph and no longer moves between
the columns. Its Graph shows the parents and first children at the rail's width and Expand opens
the whole neighbourhood in a dialog. The rail sticks even when it is taller than the window. The
main column has no cards and no Definition heading. One text leads under the counts, and the other
definitions, descriptions, comments and notes wait behind one disclosure per kind on a wrapping
row, with a chip of the property at the end of every value. `schema:description` joined the
properties that can lead. Logical definition, Synonyms, Labels, Axioms, See also, Mappings, What
links here and In other datasets are sections on the page background with headings of one size,
and Replaces stays a smaller group within What links here. In other datasets waits for the
dataset list and for its answers, and a server with no other dataset has no such section. Counts of
zero are hidden, a term with no text shows no line about it, and a concept's labels are rows by
kind in place of a table, with hidden labels behind their toggle. Identifiers say the best name the
page already has for them, and keep the exact value as their title. Typed literals show their exact value with a datatype chip,
and copied Manchester text keeps its syntax. A phone puts Metadata and Hierarchy before Axioms.

**Remaining work.** Item 622 still covers general classifier-backed inferred axiom
frames and explanation links dependent on matching F24 justifications. Classifier-backed
property and individual entailments remain in item 609, general entity and axiom reads
with classification in item 588, search at a past state in item 658 and other servers'
term pages in item 461. The kept named-class and strict materialization-backed slices
do not complete these parent items, compute on a read miss or use RL witnesses as
classification explanations.

### Rendering preferences

Item 625 adds a shared viewer setting for labels, prefixed names or complete IRIs in the
ontology and thesaurus editors, browser pages and query results. The top bar and phone
menu open the same preferences form. The browser keeps a versioned record of the mode
and language order in local storage and migrates the earlier full-IRI setting. Invalid
or unsupported records use the defaults, and unavailable storage leaves the choice usable
for the page's lifetime.

Labels use the ordered languages, with a regional tag followed by its primary language,
then untagged values, English and another available language. Empty language preferences
use the browser's languages. Missing labels use a matching dataset prefix and then the
complete IRI. A prefixed-name choice also falls back to the complete IRI. The thesaurus
editor's explicit language in its URL overrides the shared language list for that page.

Changing the mode redraws names without reloading an editor or clearing its undo stack.
Changing the language order reloads browser names in place and editor hierarchy levels,
preserving unsubmitted edits even if a name refresh fails. Complete multilingual label
batches in the selected graph remain available, while hierarchy names use the server's
choice over imported graphs too. A blank hierarchy choice leaves usable local candidates
available. Expanded graph nodes retain their names. Scheme pickers
redraw their names without losing the chosen scheme. Query
tables request only visible entities' labels, including those inside triple terms, in
batches of 200 at the result's dataset, branch and commit. A replaced result cancels and
ignores its old requests. Failed lookups leave prefixed names or complete IRIs visible.
Graph results use label literals they already contain. A label statement remains visible
when its literal is not shown in the subject's caption, unless literals are hidden.
Display choices leave navigation,
RDF identity, copied IRIs and generated query syntax unchanged.

Unit tests cover preference records, language ranking, all modes and fallbacks, unchanged
RDF terms, result sorting and nested triple terms. Browser tests cover a change across
the browser, editor and query results, reload persistence and the phone form.

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
