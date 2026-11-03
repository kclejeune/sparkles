# Changelog

## Unreleased

### Compatibility

The adopted HTTP, CLI and public library compatibility policy is documented.
HTTP descriptions and the standard CLI command tree have compatibility guards against
a commit-labeled development baseline. No published release baseline exists.
Library and binding checks, broader machine-readable contracts and deprecation
signals remain incomplete.

`GET /$/ready` and `GET /$/ready/{ds}` can report the status `degraded`, which spec
C01 reserved. A server reports it when a dataset failed to open at startup. Such a
dataset, including one in a format this release does not read, now makes the server
not ready unless `serve --ready-ignore-failed` is set. `/$/ping` answers as soon as the
listener is bound, before the datasets open, so it no longer means ready to serve.
Health checks follow one model. Liveness, and a startup check that waits for the
process, use `/$/ping`, so that nothing restarts a slow or degraded server. Readiness
uses `/$/ready`. Deployments with several datasets that should keep serving when one
fails set `--ready-ignore-failed` and use `/$/ready/{ds}` for the datasets they route or
monitor. Scripts, clients and probes that used `/$/ping` as ready to serve move to
`/$/ready`. The Helm chart's startup probe now asks for `/$/ping`.

### Added

`sparkles ontology reason`, `reduce` and `relax` do what ROBOT 1.9.11's commands of the
same names do, with ROBOT's options, defaults, errors and exit statuses (roadmap items
634 and 635). They read a file without a database or a database with `--loc`, write
functional-style syntax, RDF/XML, Turtle or N-Triples, and chain in one process with
`classify` and `check`, as in `sparkles ontology reason --input x.owl relax reduce
--output y.owl` (item 637). On GO and ChEBI lite their outputs and the chain's equal
ROBOT's with ELK axiom for axiom.

The web UI's top-bar search is a command palette (roadmap item 631). `Ctrl+K` or
`Cmd+K` opens it anywhere, and with nothing typed it lists the viewer's recent picks,
kept in the browser, the open page's commands and the pages. A query searches entities
through the server's typo-tolerant ranking and ranks commands, pages, datasets, branches
and saved queries in the browser, and the prefixes `>`, `@`, `#` and `?`, or the chips
above the results, show one kind alone. Pages register their commands while mounted
through `useCommands`, so the Query page runs, formats, explains and saves queries, the
Ontology and Thesaurus pages start sessions, create entities, undo and redo, and the
shell switches the dataset or branch, sets the theme and opens the settings and server
pages. `/` focuses a page's own search where it has one, such as the ontology editor's
tree filter, and opens the palette elsewhere. The Query page opens a saved query from
`?saved=NAME`.

`sparkles ontology materialize` does what ROBOT 1.9.11's `materialize` does (roadmap item
636). It asserts each class's direct superclasses of the form `ObjectSomeValuesFrom(p C)`
for the properties of `--term` and `--term-file`, which take full IRIs and the OBO
prefixes of ROBOT's default context, or for every property when none is given. It runs
from a file or a database and joins chains such as `reason materialize reduce`. On GO
with part of and ChEBI lite with has role its output equals ROBOT's axiom for axiom, and
on the benchmark host it takes 3.31 s and 8.32 s, against 3.47 s and 8.77 s for `reason`.
A term is kept only when it is a URN or a URL that OpenJDK 17's `java.net.URL` accepts, as
ROBOT keeps it.

The ontology browser has an edit mode (spec U12, roadmap item 630). A caller who can write
to a dataset switches an entity's page into edit mode and changes its definition and
sources, synonyms, annotations, parents and axioms in place, or a concept's labels,
broader, narrower, related and mapping concepts. Each change is a previewed commit through
the editors' write path, with their session branches, undo history and access checks, and
a refused or stale write is explained in the preview. On a phone the edit bar sits at the
bottom of the screen. A 403 in any editor's preview now names the dataset and graph that
the caller can read but not write.

The web UI has an ontology browser (spec U12, roadmap item 616). `/ui/browse` gives each
entity of a dataset's ontologies and thesauri a read-only page with its label, short form,
IRI, definition and sources, synonyms by kind, notes, SKOS labels by language,
cross-references, an infobox, a lineage tree with a graph view on the Explore canvas, its
axioms as linked Manchester prose, what links here, its use in other datasets and
downloads in Turtle, RDF/XML, OWL functional syntax and JSON-LD. Each ontology gets a
landing page with its header, counts, top classes and search, and the top bar's search
now opens these pages for every kind of entity, where it opened the editors and Explore
before. Two routes serve the pages and read the store's indexes around one entity
instead of the whole ontology. `GET /$/ontology/{ds}/hierarchy` answers one level of a
class, property or concept hierarchy with child counts and, on request, the ancestors,
and `GET /$/ontology/{ds}/term` answers an entity's statements, axioms, mentions,
incoming statements and the names of every IRI it uses, or a download with `format=`.
The Rust library, the wire dispatcher and the ontology handles of Python, Node.js and
the JVM call them as `hierarchy` and `term`.

The web UI has one app shell and design system (roadmap item 632). A top bar on every
page holds the dataset switcher and a search of the open dataset's entities through
`GET /$/ontology/{ds}/search`, which `Ctrl+K`, `Cmd+K` and `/` focus, and the pages sit
in a rail that shows labels from 1200 pixels, icons below that and a drawer from 760
pixels down, where the search expands over the bar. The search is the launcher that
the command palette of item 631 extends. Light and dark tokens for colour, type,
spacing, control heights, radii and shadows replace one-off styles, every text colour
meets WCAG AA contrast, and the pages share panels, buttons, count badges, tables, form
rows and list and detail panes of equal height. The Ontology and Thesaurus pages lose
their own dataset menus to the top bar, their graph, branch and language choices move
to a row under the title, the query page's branch, state, history and row limit fold
behind an Options button on a phone, and the ontology editor's trees show labels on
ontologies larger than the 50,000 axioms it lists, such as the Gene Ontology.

Ontologies and thesauri have a typo-tolerant entity search for typeahead (spec U12,
roadmap item 616). `GET /$/ontology/{ds}/search?q=` matches the labels, synonyms, short
forms and definitions of OWL, RDFS and SKOS entities in any language, with up to two
edits in longer words, and ranks exact labels, label prefixes, synonyms and short forms,
word matches, matches with edits and definitions in that order. Each result carries the
matched field, text and highlight, the preferred label and the entity's kinds. The
ontology handles of Python, Node.js and the JVM call it as `search`. Each branch keeps
the index in memory and applies later commits at its next search, about 292 MiB
resident for ChEBI lite.

Editing forms edit nested blank-node children, SHACL 1.2 lists and `sh:or` or `sh:xone`
value alternatives (spec U02, roadmap item 21). `GET /$/forms/{ds}?nested=true` returns
the children that `sh:node` fields reach and the empty forms of new ones, and check and
save bodies take `nested` node changes and `list` field changes, which commit with the
root's changes in one guarded update. The save rechecks every child's anchor under the
writer lock and answers a conflict on the parent field when a child is gone. Removing a
child deletes its triples when nothing else refers to it. The browser shows nested forms
with **Add**, an ordered list editor and a **Kind of** select for alternatives, and the
Rust library and Python's `ds.forms.model(…, nested=True)` expose the same model. Change
bodies now accept blank node terms, a conflict on a child's field carries the child's
`node`, and a form model's values carry a list's `items`.

The web UI has a SKOS and SKOS-XL thesaurus editor, the **Thesaurus** page (spec U12,
roadmap item 457). It shows the concept tree of a scheme one level at a time with its
polyhierarchy, searches labels in any language, and edits multilingual and SKOS-XL
labels, broader, narrower, related and mapping links, collections and ordered collections,
and notes, each as a previewed commit with `If-Match`. Dragging a concept moves it under
another or adds a broader concept. The page shares the ontology editor's session branches,
undo by revert and live refresh. The built-in SKOS shapes check the integrity conditions
of the SKOS Reference, S9, S13, S14, S27, S37 and S46 with S51, S52 and S58 of SKOS-XL.
`GET /$/skos/shapes` serves them, forms read them with `shapes=skos`, and the Rust library
exposes them as `sparkles::skos`, so a write guard can enforce them. The ontology editor's
commit messages in other scripts are now sent percent-encoded as UTF-8 instead of failing.

The web UI has an ontology editor, the **Ontology** page (spec U12, roadmap item 457). It
shows trees of classes, object, data and annotation properties and individuals, an entity
frame with its axioms in Manchester syntax, an annotation editor with language tags and
annotations of axioms, a header editor, an axiom-level usage view and the EL classifier's
inferred class tree beside the asserted one. It creates entities with IRIs from a
per-dataset IRI policy, renames and deletes them, and previews every edit as a dry run
before committing it with `If-Match`. **Start session** edits on a branch of its own, an
undo stack undoes and redoes commits by reverting them, and the change feed reloads the
views after another writer's commit. For the editor, the `/$/ontology/{ds}` routes take `?branch=` and
the `/{ds}@{branch}` form, `DELETE /$/ontology/{ds}/entity` removes an entity with its
axioms, edits accept axioms in functional-style syntax over HTTP, and an edit's answer
lists the axioms it added and removed as `addedAxioms` and `removedAxioms` in every
surface that answers the edit body. The OpenAPI description of an edit's `add` and
`remove` takes functional-style strings beside the JSON form.

The ontology editor has a **DL Query** tab (spec U12, roadmap item 457). It answers a
class expression in Manchester syntax with its equivalent classes, direct and inferred
superclasses and subclasses, and instances, as Protégé's DL Query does. Inside OWL 2 EL
the dataset's kept EL classification answers the query incrementally and is left as it
was, and outside EL the OWL 2 RL rules answer it by demand reasoning. Every answer says
which reasoning gave it, whether it is complete and which axioms lie outside the
profile. `ds.ontology().dl_query()`, `POST /$/ontology/{ds}/dl-query` and the Python,
Node.js and JVM bindings' `dl_query` or `dlQuery` run it.

The ontology editor's add fields take Manchester syntax with completion of entity names
and labels, and an axiom field takes whole axioms (roadmap item 608). The new reader,
`sparkles_owl::manchester`, reads class expressions, data ranges, individuals, literals
and one-line axioms against an ontology's prefixes, local names and labels. A name that
several entities share is an error that lists them, and every error carries its span.
`POST /$/ontology/{ds}/parse` and the bindings' `parse` read text with the caller's
grants. The editor's Manchester rendering now quotes labels that hold a space or a
keyword, such as `'teaching assistant'`, so it reads back as the same expression. The
entity-kind tabs, DL Query and Header share one strip that wraps in a narrow window
instead of cutting off a label.

An OWL 2 EL classifier computes the inferred class hierarchy and the unsatisfiable
classes of an ontology (spec F24, roadmap item 580). `sparkles_owl::classify` follows
ELK's consequence-based calculus on several threads, keeps its state so that a run after
added or removed axioms saturates again only what they reach, and reports the axioms it
leaves out as outside OWL 2 EL or uses only in part. `ds.ontology().classify()`,
`POST /$/ontology/{ds}/classify`, `sparkles ontology classify` and the Python, Node.js
and JVM bindings run it. A run can write the taxonomy to a named graph in one commit with
PROV-O provenance on the graph's header, and the dataset keeps its last classification
so that the next one is incremental.

The OWL 2 EL classifier's taxonomies equal those of ELK 0.6.0 on GO, ChEBI lite and the
298 ontologies of the ORE 2015 EL classification sample, and those of HermiT 1.4.5.519 on
the 36 ORE ontologies with individuals (roadmap item 591). `scripts/elk-compare/elk.sh`
runs ELK or HermiT through the OWL API from pinned Maven Central jars, and the EL
classification report gives times and memory beside ELK's on the benchmark host.

The OWL 2 EL classifier covers OWL 2 EL (roadmap items 587 and 605). It reasons
with individuals in class expressions and assertions, ranges that meet them, the
datatypes, literals and data ranges of the profile, functional data properties, data
property ranges, datatype definitions, keys, negative property assertions and the top
and bottom properties, with the rules of EL++. A class whose instances reach an
individual through a context that holds only where the class has an instance is
classified again in a copy-on-write world of its own. The classifier no longer reports
these axioms as used in part, and it reports only literals and datatypes it cannot
compare and property chains through the top object property. On 400 random OWL 2 EL
ontologies its subsumptions equal HermiT 1.4.5.519's, and its taxonomies of GO, ChEBI
lite and the ORE 2015 EL sample are unchanged. An incremental run after a change to an
axiom with an individual or a key now saturates everything again, as one after a change
to a data axiom already did.

The classifier takes Horn axioms outside OWL 2 EL in ontologies without individuals
(roadmap item 603), with Kazakov's consequence-based rules for Horn-SHIQ. It covers
inverse properties, universal restrictions, at-most-one and at-least restrictions,
functional and inverse-functional properties, self restrictions, disjoint, asymmetric
and irreflexive properties and transitive properties and chains through automata, and
creates contexts for the conjunctions that universal and at-most restrictions need.
`Stats` reports the tier of rules a run needed, the axioms outside OWL 2 EL it took and
the conjunctions it created. GO's taxonomy now has the 14 subsumptions that its inverse
properties entail, as Konclude's does, and GO classifies 17 to 18% slower on the
benchmark host. `sparkles ontology reason` and `reduce` with `--reasoner ELK`, and the
library's `ClassifyRequest::el_only` and `Limits::el_only`, classify by the OWL 2 EL
rules alone, as ELK does, so the ROBOT-compatible outputs equal ROBOT's with ELK. Without
`--reasoner` they take the Horn axioms too. The functional-syntax reader can pass over SWRL rules and count them
with `ofn_read::Options::skip_rules`, and refuses them otherwise as before. A run with
the Horn rules has a memory budget of 64 times its index and axioms, at least 1 GiB, and
one that passes it classifies again by the EL rules alone and sets
`Stats::horn_over_budget`. Two Uberon-based ORE 2015 ontologies that ran out of memory
now classify in 4.2 s with the EL answer.

The OWL 2 ontology handle reaches the bindings. Python's `Dataset.ontology`, Node.js's
`dataset.ontology` and the JVM's `DatasetGraphSparkles.ontology()` read summaries and
axioms, check profiles, edit axioms, remove entities and diff commits in the JSON forms
of the `/$/ontology` routes. Python adds `Axiom`, a frozen axiom with structural
equality, and Node.js lists axioms as an async iterable in batches.
`SparklesOnt.model` runs Jena's `jena-ontapi` over a Sparkles graph that one native read
copies into memory, with writes sent to the graph in the caller's transaction, and
programs that call it add `jena-ontapi`. A read with `inferences` includes the
materialized inferences and says whether they are stale. A materialization under
`owl-rl` warns about input axioms outside OWL 2 RL and about axioms whose rules the rule
set covers only up to a size (spec F21 Phase 2).

Sparkles reads the OWL 2 functional-style syntax. A `.ofn` file, or a body sent as
`text/owl-functional`, loads as the triples its axioms map to, through `sparkles load`,
`sparkles ontology check FILE`, the file loaders of the library and the bindings,
SPARQL `LOAD`, Graph Store writes and uploads. `GET /$/ontology/{ds}/diff` lists the
axioms that differ between two commits, with the assertions that the changed triples
carry when asked. The MCP tool `ontology_axioms` lists axioms, and on a server that
allows updates adds and removes them with previews, `ifHead` and PROV-O provenance
written as axiom annotations (spec F21 Phase 3).

The ontology handle answers entity accessors (roadmap item 540). Rust's read ontology
lists entities by kind and answers the superclasses, subclasses, equivalent and disjoint
classes of a class, the domains, ranges, hierarchy and characteristics of a property and
the types of an individual, and the open handle streams the instances of a class and an
individual's assertions from the store. `ds.ontology().graph(iri)` builds a read and
takes a closure that edits in one commit. `GET /$/ontology/{ds}/entities` and
`GET /$/ontology/{ds}/entity` serve the accessors, as `entities` and `entity` do in
Python, Node.js and on the JVM. Python's `sparkles.owl` and Node.js's `owl` namespace
build axioms with typed constructors named after their functional-style forms.

The OWL 2 DL check, and so every profile report, checks literals. A literal of a
datatype in the OWL 2 datatype map needs a lexical form of that datatype, a datatype
restriction needs facets and values that its datatype supports, and a datatype that a
datatype definition introduces types no literal and takes no restriction. An ontology
with an ill-typed literal, such as `"300"^^xsd:byte`, that conformed before now fails
the check (roadmap item 541).

The reasoner can keep witnesses for each fact of a materialization, the sets of input
quads from which the rules derive it, with the provenance algebra of incremental views.
`ReasonOptions::witnesses` sets the bound per fact, and `Cache::witnesses` and
`Cache::witness_dependents` read them from the closure kept in memory. Incremental runs
maintain them, deletions included, and a fact whose minimal supports fit the bound keeps
exactly those supports. They are off by default, and protections that show inferences
by their support will turn them on (spec F18 §7.1). Keeping them costs memory and time,
574 MiB and ten times the materialization time for LUBM with 10 universities under RDFS.

The server keeps a resource catalog of versioned sources, vocabularies and datasets
under `/$/resources`, for server administrators. A version never changes once it is
added, and its imports and graph sources pin versions that exist. Licences are SPDX
expressions in four classes, obligations propagate through references to each graph of
a dataset version, and `GET /$/resources/{id}/notice` writes the NOTICE. The licence
gate of `serve --license-policy` warns by default and refuses only under `enforce`, and
the export routes do not consult it yet. Restricted terms need a recorded acceptance. A
vocabulary owns its namespaces, its header can be compared with the catalog, and
`POST /$/resources/{id}/install/{ds}` installs its prefixes. The catalog is described in
DCAT 3 with PROV-O, SPDX, VANN and VoID. Nothing is fetched, and the source store,
loading and mirrors remain under item 458.

### Performance gates

The W3C SPARQL 1.0, 1.1 and 1.2 query evaluation suites, Jena's CDT and property
function tests and Oxigraph's GeoSPARQL tests run with `execution=streaming` in
`mise run test` and alone in `mise run test:w3c:streaming`, which reports the queries
that fall back to eager execution. `scripts/bench-gate.py` writes versioned gate records
and compares a candidate's record with a recorded baseline.

`mise run bench:executors` runs the 31 synthetic queries with eager and streaming
execution at 1.05M and 10.5M triples, with 12 threads and with one, checks that both
modes agree and records a forge baseline for the unified executor.

`mise run bench:reasoning-latency` times derivation on demand and RDFS on read against
materialized inferences on the 14 LUBM queries and checks every answer. Its forge
baseline shows OWL RL derivation on demand taking about two seconds per query at 10
universities.

`mise run bench:incr` runs the incremental-view benchmarks in five processes, checks each
view's rows against its query and records a forge baseline.

`mise run bench:insn` counts the instructions of vocabulary scans and WAL replay with the
CPU's performance counters, checks every result and records a forge baseline.

### Changed

The planner estimates more joins from statistics of how predicates occur together (spec
X15, roadmap item 454). A bulk load or compaction keeps 32,768 subject characteristic sets
instead of 10,000 and counts the subjects outside them as a remainder, so a star whose
predicates the kept sets cover poorly is estimated from the sets too. It also counts the
characteristic sets of nodes over both directions, which estimate joins on an object and
the links of chains. A join on several keys divides by the key tuples of the larger side,
with exponential back-off between the largest count and the product of the counts, and a
key sample that finds no rows is no longer taken as an empty join. Over the join nodes of
the recorded plans, the geometric mean of the q-error falls from 8.80 to 3.98 on WatDiv
at scale 100, from 4.84 to 1.96 on an 8.2M-triple DBpedia slice and from 3.52 to 1.81
on LUBM(10), and answers are unchanged. The switches are `set_remainders`, `node_sets` and
`key_backoff`. The statistics grow `stats.json`, from 5.0 MB to 44.3 MB on the DBpedia
slice, and the node pass adds about 0.2 s to a WatDiv load and 0.7 s to the DBpedia
load. On atlas, opening the DBpedia slice takes 0.30 s instead of 0.07 s and the server
holds 214 MB instead of 133 MB once ready. The node pass keeps the same sets at any
thread count. On forge, WatDiv's `C` queries run 2.2 to 4.9 times as fast, BSBM BI
`q1-3` 4.4 times and LUBM `q2` 2.0 times. WatDiv `S4-2` and `S4-4` take 1.6 times as long
after a plan change, and planning takes 1 to 90 microseconds longer on short queries and
WatDiv `F4`.

`sparkles ontology classify FILE` and `sparkles ontology check FILE` read a file of one
graph into memory without a store and give the answers that a store of the loaded file
gives (spec F24, roadmap item 649). RDF/XML, Turtle, N-Triples and functional-style
documents parse on several threads, the last cut at its axioms and RDF/XML between the
top-level elements of `rdf:RDF`. A file that holds named graphs, a strict read of a
functional-style document, a file that fails to parse and a file whose answer only a
store reproduces still go through a store in memory. On the benchmark host ChEBI lite
classifies from its functional-style file in 1.13 s against 3.36 s and from RDF/XML in
5.9 s against 8.2 s, GO in 0.25 s and 2.8 s, at a lower peak resident set, and the
outputs are byte for byte the same. The EL taxonomy sorts its classes, builds its nodes
and finds a class's node on several threads, with the same taxonomy on every thread
count, which takes the taxonomy stage of `ore_ont_868` from 372 to 249 ms on eight
threads (roadmap item 646). `sparkles_owl::read::Ontology::from_axioms` builds an open
ontology from axioms, and the reverse mapping sorts annotation subjects without making
their text again for each comparison.

The OWL 2 EL classifier normalizes large ontologies on several threads and scales its
saturation on ChEBI lite (spec F24, roadmap items 644, 645, 648 and 665). Chunks of the
axioms normalize in parallel and merge in order, so every thread count gives the ids
and taxonomy of one thread, and each IRI is hashed and held once. Saturation workers
deliver their messages in batches grouped by destination, share the contexts that a
long activation wakes and count pending work without a shared counter per context.
Small contexts keep their subsumers in lists, and ontologies without individuals run
the rules without their checks. On forge on eight threads ChEBI lite classifies in
393 ms against 912 ms, `ore_ont_868` in 599 ms against 862 ms, `ore_ont_13482` in
368 ms against 588 ms and GO in 77 ms against 88 ms, and GO takes 279 ms against 288 ms
on one thread. The chunks of a parallel normalization count against the memory budget
and the pool, and a run whose chunks would pass the budget normalizes on one thread.
They hold the names they meet until the merge, and the outboxes hold up to 4,096
messages per worker, which raises ChEBI lite's peak resident set on eight threads from
911 to 988 MiB, while the ORE ontologies' falls.

The OWL 2 EL classifier classifies GO, ChEBI lite and the largest ORE 2015 EL ontologies 2.5 to 13.3 times as fast as ELK 0.6.0 on forge on one and eight threads, where ELK had been faster on ChEBI lite on eight threads and on the ORE ontologies of a million classes (roadmap items 602 and 592). A
`Saturation` reads the caller's axioms in place, so the dataset's kept classification
shares the open ontology's axioms instead of copying them. The profile check and the
taxonomy run on several threads and give the same taxonomy for every thread count, the
index keeps one record per expression in flat arrays, and saturation workers take roots
in rounds from a shared cursor and batch their shared counters. The memory estimate now
counts the taxonomy and its links and comes within 14% of a counted peak heap, and
`Stats::peak_memory` gives the peak. `el_classify` uses the server's mimalloc allocator,
reads the axioms in place unless `--owned` is given and counts the heap with `--heap`.

Ask's draft rules tell the model to select only the variables that answer the question,
without label, predicate or helper columns unless the question asks for them, and never
to add a variable only to fill the draft's `graph` member. Over three runs of 51
QALD-9-plus validation questions the mean strict score rose from 27.0 to 28.7 with Haiku
5.5 and from 29.0 to 29.7 with GPT-6 Luna, and answers with columns beyond the gold's
fell from 27 to 8 and from 59 to 10 over the three runs. On 150 held-out QALD-9-plus
questions Haiku answers 102 and Luna 101, within the noise of GPT-6.1 Sol and short of
70%, and no model moved beyond the noise of its run with the earlier build.

Ask declares the well-known vocabulary prefixes that a draft uses without declaring,
such as `dbo` and `dbr`, before the check, unless the query or the dataset defines them.
A draft that used one of them failed the check with a syntax error and needed a repair.

Eager queries now count against their memory budget the query state that only cursors
counted before. The decoded value cache, which holds at most 64 MiB or an eighth of the
budget, the decoding of dictionary keys, the terms that expressions compute, the memos of
blank nodes, callback terms, geometries and EXISTS, retained scratch and the programs of
regular expressions are charged in both execution modes. An eager query close to its
memory budget can therefore fail with the memory budget where it used to answer. Regular
expressions no longer stay in a process-wide cache of up to 1,000 programs with their
search caches. Their compiled programs are kept in a table of at most 64 programs and 16
MiB, each query charges the bounded search caches it uses, and the workers' warm caches
of a kept program wait in the table for the next query, up to another 16 MiB. A cursor's
account reserves from the query pool in steps of 64 KiB and can hold about 1 MiB of the
pool above its live bytes, and cursors keep their decoded values across batches (spec X14
stage 4). Per-value reuse in BIND and FILTER, ORDER BY keys, top k and the pre-decode of
FILTER, BIND and ORDER BY columns choose their algorithms by the size of their input in
both execution modes, so an eager table of up to 16,384 rows runs them as a cursor batch
does, and an eager top-k scan reads its pieces in morsels into one heap.

Short queries no longer pay the fixed costs that the charged query state added to every
request (spec X14, roadmap items 679 and 681). A query makes its regular-expression map
and the 64 shards of its value cache only when it uses them, so a query without patterns
neither replaces nor frees a map, and FILTER, BIND and ORDER BY skip the checks of their
larger-input paths on a few rows. A small sort's key column takes its payload charge a step
of at most 64 KiB ahead, exactly when the step is refused, instead of once per row. The
whole request of `count-all`, `distinct-obj` and `path-plus` now takes fewer instructions
than before the charged query state, and `types-grouped` about 1,200 more. With one thread
`star-lookup` runs 1.9% longer than before stage 4, since the build places the index join's
unchanged search loop 16 bytes into its cache line, and roadmap item 696 follows the
alignment of the join loops. A sort of more
than 4,096 distinct dictionary terms decodes them in one parallel pass that admits each
value before it builds it, and a large sorted key column is freed on the pool, which takes
eager `order-by-full` at 1.05M triples with 12 threads from 13.1 ms to 8.9 ms on the
benchmark host. Each decoding piece and each key column can hold up to one step of
charged memory above its values until the sort ends.

`sparkles ask` takes its question argument as optional, since `--questions FILE` reads
questions from a batch file instead. A command line that passes a question parses and
behaves as before, and one that gives neither a question nor `--questions` is refused.

The count and filter reads that visit index blocks in parallel now merge uncompacted
commits into the blocks they change, instead of reading the whole range in order. On forge,
`two-hop-count` on a store with 5,000 uncompacted commits went from 8.2 to 6.2 ms at 1.05M
and from 35.8 to 21.6 ms at 10.5M. A read whose ranges hold more than 65,536 delta keys
still merges them in order.

Streaming cursors run FOLD and registered aggregates over the key runs of their input,
sorting it on the group keys when it does not arrive that way, and DESCRIBE describes
each batch of solutions as it arrives, so the strict policy admits both. A LIMIT without
OFFSET reaches a SERVICE that reads no input and a full-text search with a constant query,
and a registered property function that declares `prefix_output` runs a batch of input
at a time. A request with `execution=streaming` commits its response after the cursor's
first batch instead of after 1 MiB. Both executors answer an EXISTS in BIND or an
OPTIONAL condition from the key set of its pattern, and automatic execution streams such
a BIND. On an 8-vCPU Namespace instance at 10.5M quads, automatic execution of a COUNT
grouped by BIND(EXISTS) went from 860 ms to 8.4 ms.

`sparkles serve` runs one query that reads no data on a scratch in-memory dataset before it
reports ready, and maps its executable's code into memory on a background thread, so that
the first request after a start does not build the query path or fault in its code. The
mapping adds about 100 MB of shared file pages to the resident set. `--no-warm-up` and
`--no-code-prefault` turn the steps off, and the embedded library takes neither. On forge
the first cold `path-plus` takes 1.80 ms at 10.5M and 2.49 ms at 1.05M instead of 3.52 and
4.15 ms, which meets roadmap item 4's target against Oxigraph.

On Linux, a server on a processor with performance and efficiency cores runs the threads
that serve requests on the performance cores, and its query pool leaves one logical CPU
free for every four efficiency cores. Before, the pool held every logical CPU, and a
request that waited for it often finished its sequential work on an efficiency core.
`serve --request-cpus all` keeps the earlier placement, and `RAYON_NUM_THREADS` still
sets the pool's size. The default pool on such a processor is smaller, 18 threads instead
of 20 on an Intel Core i5-13500, which a query bound by its parallel work can feel.
Request work runs on the performance cores only while at most one request per physical
performance core is in flight, and on every CPU when more are. With every request on the
performance cores, sixteen concurrent readers and a writer on an Intel Core i5-13500
answered about 0.86 of the reads and 0.66 of the writes a second of
`--request-cpus all`, since the performance cores were full while the efficiency cores
idled.

A cursor batch of BIND or FILTER rows whose expressions are expensive, such as date
functions and decimal arithmetic, is mapped over the query pool instead of on the
request thread, and the rows it holds while it does count against the query's memory
budget. A query text whose automatic cursor answers whole in 10 to 50 ms has its next
run tried eagerly, once, and settles in the faster mode. Before, DBpedia's
`births-by-decade` could stay a cursor of about 15 ms for a server's whole life where an
eager run takes 2 ms.

An OWL 2 ontology API reads the axioms of a dataset's graphs through the OWL 2 Mapping to
RDF Graphs, checks them against OWL 2 DL, EL, QL and RL, and edits them in one commit.
The new `sparkles-owl` crate holds the axiom model, both mappings with lenient and strict
reads, a functional-style writer and the profile checks. The library's `ontology`
feature adds `Dataset::ontology()`, and the server, which builds it by default, serves
`GET /$/ontology/{ds}`, `GET /$/ontology/{ds}/axioms`, `POST /$/ontology/{ds}/check` and
`POST /$/ontology/{ds}/axioms` with the caller's grants. The CLI adds
`sparkles ontology summary|axioms|check|edit` and `sparkles ontology check FILE`, which
exits with status 1 when the ontology is outside a profile it names. `mise run test:owl`
runs the W3C OWL 2 test cases.

The library builds the options of a query with `sparkles::QueryRequest`, which applies a
`sparkles::Budgets` of timeout, rows, memory, rows produced and spill bytes, a caller's
lower budgets, an inference view and a cancellation flag. The server builds every query
endpoint's options through one caller context, and the MCP tools and the bindings apply
their limits through `Budgets`. A GraphRAG build now runs under the server's `--max-rows`,
query memory, rows-produced and spill budgets and `--update-timeout`, and the
`/$/graphrag/{ds}` read routes under `--timeout` and the same budgets. Before, neither had
a server timeout or budget, so a large build or report read could outrun the limits that
every other query meets.

`spk:hybridSearch` and the MCP memory tools fuse ranked lists with one library function,
`sparkles::rank::fuse_rrf`, and read full-text hits through one `text:query` call,
`sparkles::handles::text_call`, and one reader, `TextIndex::hits`, which reads a given
snapshot. Their results and order are unchanged. A text search without highlighting, in
the library and over `GET /{ds}/text?highlight=false`, now returns a matching literal as
stored. It used to lose the private-use characters U+E000 and U+E001, which mark a
highlighted match, from the literal.

The library names the reasoning of a query with `sparkles::reasoning::ReasoningMode`
and selects its inferred data with one `InferenceView` from `Dataset::inference_view`.
The view gives the query options, the input of incremental views and the source of an
explanation. The server's `reasoning` parameter, the CLI's `--reasoning` and the MCP
tools' `reasoning` argument read a mode and apply its view, and each reads the data it
read before.

The library chooses how a query runs in one function. `sparkles::sparql::choose_execution`
takes a `Consumer`, which states the mode a caller asked for, the mode it runs in without
one and what its consumer needs, and returns the execution mode or a refusal of a cursor
inside a write transaction. `ExecutionMode::parse` reads the names `eager`, `streaming`
and `auto`. The HTTP query endpoint, `sparkles query`, the Node addon and the FFI use it,
and each chooses the mode it chose before.

Incremental circuits can annotate each row with provenance (spec F13 §5). An internal
circuit that keeps rows takes `count`, `lineage` or `why` through
`InternalSpec::with_provenance`, and `annotated_rows()` returns each row with the number
of its derivations, the atoms its derivations use, or its minimal witnesses, at most
`maxWitnesses` (8 by default). Input quads are named by a 128-bit `FactKey` over their
terms' vocabulary keys, which `resolve` maps back to quads, and a row that `MINUS`,
`NOT EXISTS` or an unmatched `OPTIONAL` keeps names the condition that has no match.
Deletions are exact. A row whose witnesses did not fit in the bound is reported as
truncated, its reported witnesses are always real, and it is exact again once the extra
derivations are removed. Circuits without a provenance setting are unchanged. On an
8-vCPU Namespace instance at 200,000 people, the median annotated pass of three runs was
at most 0.025 ms above the plain one for each benchmark view. `lineage` made a state up
to 80 times larger and `why` up to 8 times, and an annotated build of a three-pattern
join took 2.2 seconds against 9 ms.

Incremental views maintain property paths with `+` and `*` (spec F13 §4.3) instead of
evaluating them again at each commit. A fixed-point operator keeps each reachable pair
with the one derivation that first reached it, follows only the walks that reach a
changed step, and deletes by backward and forward rederivation, so a pair that another
path still reaches is never removed. The step can be any path of IRIs, inverses,
sequences and alternatives, under `GRAPH` and inside `EXISTS`, `NOT EXISTS`, `MINUS` and
`OPTIONAL`, and `*` adds its zero-length rows as the query engine does. Paths with `?`,
negated property sets or nested closures are still evaluated again. The operator's state
takes about 40 bytes for each reachable pair. The reasoner's forward rules plug into the
same operator through `sparkles_reasoner::fixpoint::RuleProgram`. On an 8-vCPU
Namespace instance at 200,000 people, a pass that changed a path took 0.02 to 0.24 ms at
the 95th percentile, against 2.3 to 82 ms for evaluating the path again, and the state
was up to 11% larger. Building a path view with a constant end took up to 3.3 times as
long.

Server code can attach internal circuits to a dataset's incremental views runtime
(spec F13 §3.6) with `Runtime::attach_internal`, or `Dataset::attach_internal` in the
library. Such a circuit keeps a query's rows, or counts the changed facts per key in each
commit through an accumulator sink, so that a fact added by one commit and removed by a
later one counts 2, a replacement within one commit counts 2 and a deletion counts 1, as
the change feed lists them. The runtime applies an accumulator's circuit one commit at a
time, also when it is behind, and keeps the counts per commit, so a read of the counts
after any commit they cover is exact and a commit whose changes are not known is
reported as unknown. Internal circuits read through an `InternalAccess` that only the
embedding process constructs. They have no `view.json`, are not listed under
`/$/views/{ds}`, are not read by `SERVICE view:read`, and no route, binding or MCP tool
reaches them. `RuntimeStats` counts them as `internal_circuits`.
`scripts/nsc-writes.sh` and `mise run bench:nsc-writes` take `--views none|async:N`, which
defines N table views on every store before the writes. `commit:Q` and `graph:N` are
refused until the server maintains views at commit and keeps graph views.

Administrators record a dataset's evidence settings with `GET` and `PUT
/$/evidence/{ds}/settings` and stop protecting one graph with `POST
/$/evidence/{ds}/detach`. Rust applications use `sparkles::evidence::configure` and
`sparkles::evidence::detach`. The settings add `decisions`, `supports`, `identity` and
`projections` roles and named review `policies` to `sourceRecords` and `proposals`.
Enabled settings make every role graph a managed graph of the evidence owner, so
ordinary writes to it are refused, and a configuration never stops protecting a graph.
`Store::enable_managed_at` and `Store::detach_managed_at` change the managed-graph
registry only at an expected revision. `Store::managed_snapshot` returns the registry
and its owners' settings from one load, and `Store::managed_settings` returns one
owner's recorded settings.

Durable source holds keep the commits that evidence references name readable.
Administrators list, acquire and release them with `GET` and `POST
/$/evidence/{ds}/holds` and `DELETE /$/evidence/{ds}/holds/{id}`, and Rust applications
use `sparkles::evidence::holds`, `acquire_hold`, `release_hold` and `reconcile_holds` or
`Store::acquire_hold` and its companions. History trimming, the retention window and
its limits, compaction and generation retirement keep every held commit, and an
in-memory dataset keeps its held states. A branch that a hold names is refused deletion
with `409 branch-held` unless `DeleteOptions::release_holds`, `?releaseHolds=true`, which
needs `admin`, or `sparkles branch delete --release-holds` releases its holds first.
`sparkles check` counts holds among retained generations. The Rust client's
`BranchDeleteOptions::release_holds` sends the new parameter.

`POST /{ds}/claims/resolve` verifies evidence references without writing. A stored fact
names its dataset and branch by identity and is read at exactly its commit, through the
caller's view of that dataset and branch, and a commit at or below a branch's starting
point is read from the upstream that serves it. A record resolves only as an
authenticated record of the dataset's protected evidence graphs, with a receipt that
names this dataset and one of its branches, and a passage's selector is checked against
its rendition's retained text. A missing, unretained and unreadable reference all answer
`422 evidence-unavailable`. The other outcomes are `422 incomplete-evidence`,
`unanchored-evidence`, `span-mismatch` and `origin-not-reproduced` and `409
immutable-record-conflict`. One resolution shares a 64 MiB allowance with the request
deadline and cancellation. Rust applications use `sparkles::evidence::resolve::Resolution`,
and `Store::snapshot_of_branch_limited` reads a branch by identity at an exact commit
within a cap without opening a cold branch store. `Store::listed_branch_name` names a
listed branch by its identity.

`POST /{ds}/claims` and `sparkles::evidence::propose` publish authenticated evidence
proposals. One batch of source revisions, renditions, passages, references, claims,
links and assessments is one commit of the evidence owner, and claims never assert
their propositions. The route needs `write` through the update endpoint, update grants
on the written graphs and read access to the referenced records and facts. Record IRIs
derive from the dataset, the caller and the record's keys. The server records each
record's receipt, creation activity and `ev:recordDigest`, each source revision's
`ev:firstAvailableAt` and the operation, an `ev:Operation` whose agent is the caller. A
repeated `idempotencyKey` returns the first receipt, and the codes `409 head-changed`,
`409 idempotency-conflict`, `409 immutable-record-conflict` and `503 authority-changed`
are new. The stored facts of the dataset that a proposal references are held before
its commit, and holds of operations that never published are released after an hour.
`GET /$/server` advertises `capabilities.claims`. `managed.json` is now format 2, which
gives each managed graph a protection nonce. Older releases refuse format 2, and this
release reads format 1 as graphs that take no typed writes until they are protected
again. A proposal records the nonce on the operation and on each record, and resolution
requires both, so a record written while its graph was detached answers
`origin-not-reproduced`. Evidence resolution reports each record's `recordDigest`.
Revoking a token, ending a session and a login that evicts sessions now wait for the
typed writes those credentials authorized that are publishing, and releasing a source
hold waits for a commit that is publishing a reference to it. The
vocabulary adds `ev:Operation`, `ev:graphProtection`, `ev:payloadDigest`,
`ev:idempotencyKey`, `ev:sourceHold`, `ev:recordDigest`, `ev:retainedBytes`,
`ev:locator` and `ev:reportedRights`.

`POST /{ds}/claims/review` and `sparkles::evidence::review` append immutable review
decisions under a named policy of the evidence settings, and a decision supersedes the
target's current decision by naming it. An acceptance of a positive claim of an admitted
kind with the required scope records an `ev:AcceptanceSupport` and projects the claim's
proposition into the policy's output graph. Each occurrence owns its own support, and a
projected triple leaves the output graph when no live support owns it.
`POST /{ds}/claims/withdraw` and `sparkles::evidence::withdraw` record an
`ev:Withdrawal` of claim occurrences, which ends their supports. A reviewer must read
an acceptance's evidence, and a hidden record answers as an unavailable target. Every
read of a review is made at its required `ifHead`. The codes `409 policy-changed`,
`409 target-changed`, `409 decision-superseded`, `409 target-withdrawn`,
`409 evidence-conflict` and `503 projection-unavailable` are new, and
`capabilities.claims` lists both routes. The vocabulary and shapes add
`ev:supersedes`, `ev:AcceptanceSupport`, `ev:Withdrawal`, `ev:Projection` and their
properties (spec C25 Phase 2).

Identity proposals are reviewed into reversible resolution maps (spec C25 §6, roadmap
item 81). A claims batch takes `identities`, which name two to 64 entity IRIs as a
possible match or as distinct entities and assert no `owl:sameAs`, and `identityGraph`.
Evidence settings take `matchingPolicies`, each with its version, boundary, decisions,
maps and output graphs, as an additive amendment of `evidence.json` format 1 that a
build without it refuses as unreadable settings. `POST /{ds}/claims/review` under a
matching policy decides proposals, and the policy's output graph holds the current map
as `ev:resolvesTo` triples from each member of a cluster to its representative. An
accepted separation keeps entities apart, and joining them is the new `409
identity-conflict`. A split is a superseding decision, and every change of the
decisions a map follows writes an immutable `ev:ResolutionMap` revision with the
decisions it added and ended. The review answer adds `mapRevision` and `changed.map`
with the entities whose selected entity changed. `POST /{ds}/claims/identities` and
`sparkles::evidence::resolve_entities` read the current map, and `capabilities.claims`
lists the route. Evidence links and assessments may target identity proposals. A
revision records the identity graphs of the boundary it was built from, and a map one of
them has left, or whose graph was protected again, answers `projection-unavailable`.
Identity reviews need read grants on every identity graph of the boundary, and one
operation retains at most 64 MiB of entity identifiers. The resolution read leaves out
a proposal or decision whose triples the caller's view hides in part, so it neither joins
a cluster nor reveals its decision. The vocabulary and shapes add
`ev:preferredEntity`, `ev:ResolutionMap`, `ev:mapRevision`, `ev:adds`, `ev:ends`,
`ev:resolvesTo`, `ev:mapsProtection` and `ev:boundaryProtection`.

Assessments name the records they consumed, and the assessments that depend on changed
evidence can be found (spec C25 Phase 3, roadmap item 81). An assessment's `inputs`
names up to 64 claims, assessments, identity proposals, source revisions, review
decisions, acceptance supports or resolution map revisions, written as `prov:used`, and
an assessment without them keeps its digests. `POST /{ds}/claims/dependents` and
`sparkles::evidence::find_dependents` answer the authenticated assessments in readable
proposal graphs whose target or inputs name a changed record, optionally transitively,
and `Changed::identities` gathers what an operation changed. `capabilities.claims` lists
the route.

Materialization closes transitive relations, `rdfs:subClassOf`, `rdfs:subPropertyOf`
and `owl:sameAs` with closure operators instead of joins, and derives the same facts.
An operator condenses a relation's strongly connected components and computes each
component's reachable set once, and `owl:sameAs` closes its classes with union-find.
On forge, `sparkles infer` materializes OWL2Bench with 2 universities under OWL 2 RL in
5.0 to 5.3 s instead of 53 s, and OWL2Bench with 5 universities in 44 s at 9.5 GiB of
RSS, where it ran out of 14 GiB before. Programs with a rule that is not monotonic, programs that read lists and
build them, incremental updates and query-time reasoning still evaluate these rules as
joins.

`sparkles serve --prewarm` records the decoded blocks and resident file pages that each
persistent dataset used, at shutdown and every `--prewarm-interval` seconds, and reads them
again in the background after the next start is ready. On forge at 10.5M, with the list of
a warm pass, cold `star-lookup` took 3.3 ms instead of 11.4 ms after a background read of
15 ms, and the time to ready stayed at 52 ms. It is off by default.

An index join over index format 3 reads a block whole at its first cold segment when its
ranges in that block span more than a quarter of the block's segments. Cold `star-join` at
10.5M on forge went from 85.7 to 76.9 ms, in line with unsegmented blocks. The lookahead
runs out of line from the join's read loop, because inlining it slowed unrelated queries.
Warm point lookups run 1.6% to 2.5% more instructions, which roadmap item 516 covers.

Weighted `path:allShortest` between one pair, and the cheapest weighted cycles through a
node in both shortest modes, run Dijkstra's algorithm from both ends. Weighted path
searches add costs in double-double arithmetic everywhere, so paths whose weights have
equal sums tie whatever order the weights come in while the weights lie within a factor
of 2^28 of each other, and `path:cost` is the sum rounded to the nearest double. A search could return one of two such paths before, as with weights
0.1, 0.2 and 0.3 added in opposite orders, and paths with weights of very different sizes
can report a slightly different cost than before. Among equally cheap paths the order of
`path:pathIndex` can differ from earlier releases.

An OPTIONAL whose right side is a triple pattern reads it only for the distinct keys of
an evaluated left side when its cost model says probing is cheaper (`probed_optional`).
A FILTER comparison of a column with numeric, `xsd:date` or `xsd:dateTime` constants
decides most rows on their ids (part of `range_pushdown`). An ORDER BY key equal to a
BIND or SELECT expression orders by its variable (`bound_order_keys`). On forge at 10M,
BSBM Explore `q7` and `q8` now take 3.8 ms instead of 5.2 and 5.8 ms, ahead of
Oxigraph, and BI `q3` takes 31.5 ms instead of 41.4 ms. A recheck of SP²Bench `q11`
at 10M on forge measured 3.46 ms against QLever's 43.56 ms, so it leaves the loss list.

Group commit fences write through. Writers leave the log to the leader of their fence,
which writes the whole prefix's records and new terms through the same `O_DIRECT` and
`O_DSYNC` handles as an ordinary commit when they fit in preallocated space, so a fence
persists without a device cache flush on a drive with FUA. A larger prefix is written
with buffered writes and `fdatasync` as before. The next fence synchronizes while the
last one publishes and applies its full-text changes. Staged full-text documents count
in the queue's 4 MiB limit, and the quota and the free-space reserve count queued records
and terms, and a fence's preallocation, until their fence writes them. Queued records
now wait on the heap, at most 4 MiB per dataset, until their fence writes them. The delta
vocabulary writes a large set of terms as several chunks of at most 1 MiB, or of one
longer term, so a fence with more than 4 GiB of new terms stays readable. On `forge` the fences issued 0.001 to 0.005 device cache flushes per commit instead of
1.00 to 1.14, and four writers on the spatial dataset at 1.05M committed 2.05 times as many
transactions/s with the drive in its slow state. Flushing fences were faster than
write-through ones in three of ten pairs. The reading that the data fit, which the
fence histograms of later runs support, is that those flushing processes ran with the
drive in its fast state and the write-through ones spent part of their time in each.

Group commit fences write their commits' messages and digests through as well. Grouped
commits queue their annotation records, and the fence's leader writes them to
`annotations.bin` at the same time as the prefix's records and terms, through an
`O_DIRECT` and `O_DSYNC` handle when they fit in preallocated space, so a fence of
annotated commits no longer flushes the device cache. Ordinary commits write their
record the same way before their log records. The file grows by zero bytes in steps of
64 to 256 KiB, which the quota does not count, and a close cuts them. The format of the
file is unchanged. On `forge`, four grouped writers whose commits all carry a message
committed 2.10 times as many transactions/s on the spatial dataset at 1.05M and 2.21 times
as many on the full-text dataset, and serial commits with a message reached their p50 in
0.62 times the time, with about 0.005 device cache flushes per commit instead of 0.5 for
the writers and 1.1 for serial commits. Probes on `forge` that alternate a write-through fence and a flushing
fence every second found the write-through fence faster with the drive in each state,
at 0.24 against 0.34 ms in its fast state and 1.61 against 1.71 ms in its slow state.

`anthropic` model providers use Anthropic's prompt cache by default. Every call marks
its system text for the cache, Ask also marks the grounding that every question of a
dataset shares and the question with its own grounding, which repairs read back, and
ingestion marks the vocabulary that every chunk shares. Ask's draft prompt now puts the
dataset's common prefixes, classes and predicates first and the question after its
grounding, and each list continues under its own heading in the question's part, so
the model reads the same lines in a different order. `promptCaching: {"enabled":
false}` turns caching off for a provider, and `promptCaching.ttl: "1h"` keeps prefixes
for an hour. Token budgets, the daily provider budget, GraphRAG build ledgers and the
evaluation proxy's cost cap price a cache write at 1.25 times uncached input, or 2
times with the 1-hour TTL, and a cache read at 0.1 times. Step records and usage objects
add `cacheWriteInputTokens`, `cacheWrite1hInputTokens`, `cacheReadInputTokens` and
`budgetTokens` when a provider used its cache, `pricing` takes cache prices, and
`sparkles_ask_tokens_total` adds the `cache_write` and `cache_read` directions. The
`tool` structured-output level of the `anthropic` kind offers its strict tool with
`tool_choice: auto`, since current Claude models refuse a forced tool choice.

Change feeds, live queries and long polls end with `access-changed` when the session
or token they were opened with expires, including a session's idle timeout. OIDC
sign-ins decide admission and the session lifetime under the policy current after the
provider exchange and create the session only if no reload completed meanwhile.

A view whose stored `view.json` asks for an option that only a later phase supports,
such as a `graph` view, maintenance at commit, other branches, `keepCommits` or
provenance, no longer runs as a `table` view on `main`. The views are checked when they
load and on every attach, and such a view is listed as failed with `unsupported-option`
until it is defined again. The other views of the dataset open and run as before.
A view definition may now name `onFailure` and `declassify`, the members that spec F13
adds for commit-time and graph views. Their defaults, `suspend` and `false`, run as
before and are not written to `view.json`, and other values are refused with
`unsupported-option`. A stored `view.json` of
format 1 with such a member used to make every view of the dataset unreadable and now
marks only that view failed.

A view or live query with a `GRAPH` inside another `GRAPH` now evaluates that group
again at each commit that touches it. The outer graph variable was unbound in every row
before, and a constant outer graph was not required to exist. View checkpoints of the
older compiler are built again from the data.

The planner moves a VALUES table into a `GRAPH` that holds a UNION, joins a table and a
UNION that are both under a named `GRAPH` in the same group, and moves a table through a
FILTER or BIND that reads its variables when the pattern under it binds them in every
solution. The memory reader's union of the default graph and the named graphs, with its
graphs in a table, now looks up the table's rows inside each branch. A table that names
the graph variable, that the pattern may leave unbound or that has an UNDEF cell the
FILTER reads stays outside, so answers do not change. On 10.1 million quads on atlas,
these shapes took 42 to 522 ms before the change and under 0.5 ms after it.

`sparkles fmt`, `POST /$/format`, the MCP `format` tool, `sparkles lsp`, the
WebAssembly module and the shapes editor's Format button now format SHACL Compact
Syntax. The formatter keeps every comment, checks that its output reads back to an
isomorphic shapes graph and that formatting it again changes nothing, and applies
`prune-prefixes` and `align-values`. `.shaclc` and `.shc` files and the media type
`text/shaclc` name the language, and the new `shaclc` value joins the `language`
fields of the HTTP and MCP formatters. Directory walks now format `.shc` files, which
they skipped before. The SHACLC lexer, reader and writer moved to the new
`sparkles-shaclc` crate, which `sparkles-shacl` re-exports as `compact`.

`ORDER BY ?v LIMIT k` over one triple pattern reads the IRIs of the base vocabulary in
value order, and in ascending order reads plain strings from the low end of the
vocabulary. A literal's vocabulary key ends its lexical form with a 0xFF byte, so a form
sorts after the longer forms it begins. The reader looks up the prefixes of the last
string it read, which are the only unread strings that can still rank ahead. SP²Bench
`q11` at 10M reads 120 rows instead of ranking 603,577 decoded strings, and took 0.13 ms
against 127 to 141 ms in a local server A/B. The planner keeps the plain sort for literal
columns whose first rows are not plain strings, such as language-tagged labels.

Jena's TriX, RDF Thrift, RDF Protobuf and RDF/JSON now work wherever the W3C syntaxes do.
`Dataset::load_str`, `load_str_into` and `dump_graph` take any syntax, and the new
`Dataset::load_bytes` loads the binary ones from memory. SPARQL `LOAD` reads all four
from files and over HTTP, and asks for them in its `Accept` header. The JVM binding loads
them from a stream or a file, and the Node.js binding accepts them in `load`, `dump`,
`parse` and `serialize`. The loader reads these syntaxes straight from the file or body
through their streaming readers, so `sparkles load`, `Dataset::load_file`, uploads, Graph
Store writes and the Python `Dataset.load` no longer write a temporary N-Quads copy. A
malformed RDF Thrift or RDF Protobuf body now reports how many statements were read
before the fault. The decompressed-size cap of the server still bounds a Thrift, Protobuf,
RDF/JSON or TriX body by the size of its quads as N-Quads, as the rewrite did, and it
also counts the bytes read. A body that expands through a prefix is refused with `413`.
Loading a file in one of these syntaxes takes 11 to 22 percent less time and no longer
needs the disk room of the copy.

`sparkles lint`, `POST /$/lint`, `sparkles lsp`, the WebAssembly module and the new MCP
`lint` tool now check N-Triples, N-Quads, JSON-LD and SHACL Compact Syntax as well as
SPARQL, Turtle and TriG. The term rules apply to every language. SHACLC also gets the
prefix rules, and JSON-LD gets `unused-context-term`, a hint whose fix removes the term,
and `unmapped-key`, a warning for a key that no context maps. In Turtle, TriG and JSON-LD
shapes, the SPARQL in `sh:select`, `sh:ask`, `sh:construct` and `sh:update` literals is
linted with the prefixes of `sh:declare` and SHACL's pre-bound variables, and each finding
is placed inside the literal. The shapes editor lints Turtle and SHACLC shapes while you
type. `sparkles lint` walks `.nt`, `.nq`, `.jsonld`, `.shaclc` and `.shc` files, and
`--list-rules` shows the languages of each rule. A `--fix` on a Turtle or SHACLC document
with an undefined prefix used to fail with `unsafe-fix` and now applies its fixes. The MCP
tool takes rule levels as camelCase `options`. JSON-LD documents are expanded with
`oxjsonld` to check that a fix keeps their dataset. `POST /$/lint` now answers `415` with
`unsupported-language` for RDF/XML instead of `400`. A document with many findings is
placed in one pass, and the lint checks its deadline on every path, so it keeps to its
timeout. JSON-LD queries come from the document's expansion, every JSON-LD fix is checked
against the expansion before it is offered, and an embedded query whose holder is
ambiguous, or whose source string cannot be proved, is skipped. The bounded isomorphism
check of the SHACLC formatter and the linter no longer canonicalizes small graphs exactly,
so a few symmetric blank nodes cannot run past the deadline. The query editor no longer shows the
findings of a lint that answered after the query changed or linting was turned off.

The Python `Dataset.load` reads a file object once, as a stream, in every syntax and
parse mode. Into an empty dataset it runs the bulk loader with its parallel encoders,
where it used to insert the quads one at a time into a transaction, and into a dataset
that holds data it uses one transaction. Automatic and streaming mode no longer copy the
file object to a temporary file, and Jena's syntaxes no longer pass through an N-Quads
pipe. Buffered mode keeps the stream in memory up to `auto_buffer_bytes` and continues
into a temporary file past it. `Dataset.replace` from a file object replaces a long
stream through the bounded bulk path, and it now stops for Ctrl-C, its cancel token and
its timeout. A load of a 5M-triple N-Triples file object takes 2.2 s instead of 21 s and
peaks at 1.2 GiB instead of 3 GiB of RSS, and a stream that ends within 8 MiB still
goes into an empty dataset by a transaction. N-Triples and N-Quads streams are parsed in
parallel blocks, at most two at a time, which hold more memory than a load of the same
file by its path. A block buffer grows as it fills, which also spares small compressed
line-based files a 256 MiB buffer. An exception that the file object's `read` raises now
propagates as itself. Ctrl-C stops the load at the next read, and the store runs the
main thread's signal handlers right before it publishes, so an interrupt during the load
no longer lets it commit. A handler's write to the dataset that is loading raises
`ConflictError` at once instead of waiting for the load's writer lock. In Rust, `SourceData::Reader` and `Source::from_reader` give
the loader a stream that it reads once, and `WriteOptions::before_publish` takes a hook
that the store runs right before it publishes a write. Code that matches every variant of
`SourceData`, or builds a `WriteOptions` without `..Default::default()`, needs the new
variant or field.

Signing in, minting a token and expiry pruning no longer move the shared authority
generation. Concurrent logins of other users succeed, cached Basic credentials stay
cached, and other users' change feeds, live queries and long polls keep running.
Logout and token revocation end only the streams of the revoked credential. An
authentication that keeps overlapping a policy reload now gets `503` with code
`authority-changed` and `Retry-After: 1` instead of `403` or `401`, and the refusal
spends none of the pre-authentication budget.

Backups and persistent clones carry the views' definitions, captured under the store's
writer lock with the data. Manifest entries name each definition's kind and file
version, and the manifest requires the `definitions` feature. Older releases refuse to
restore or verify such backups, and their garbage collection keeps the blobs. Restored
and cloned views stay suspended in a `view.json` of format 2, which older releases
refuse, until a rebuild resumes them. Restores refuse unsafe, duplicate, unknown,
oversized or damaged definition entries before publishing, and a backup whose
definitions a restore would refuse fails before it uploads anything. View definitions
are limited to 1 MiB, and a `--read-only` server refuses view rebuilds.

Node graph overviews retain one absolute deadline across their native subqueries
instead of renewing the timeout for each query.

Graph overview groups and facets in the Rust library start a relative timeout once
for the whole answer, so every binding and caller shares one deadline across the
overview's queries. GraphQL history selection honors a caller's absolute deadline.

Controlled permission preparation refuses a snapshot that another policy already
masked, unless the mask is the requested policy's own. A policy that hides no
triples keeps the existing stricter mask.

Controlled permission preparation now charges a view only for the policy and mask
storage it retains, not for the input snapshot or the delta it shares. Delta
vocabulary lookup probes the index instead of scanning entries, and a cold mask
writes its permutations in parallel. On a 100,000-quad uncompacted delta the smallest
admitting cap fell from 431 MB to 41 MB in a release diagnostic. A cached GraphRAG
disclosure index is no longer charged to each request, and a cold one skips
disclosure rows that repeat a recent graph and footprint, which took a 20,000-row
index from 24 ms to 0.18 ms.

Catalog disposal now refuses raw shared state, Store and BranchSet aliases and preserves
exact owners on refusal. Resident source leases retain real catalog openings without
changing StoreHandle variants or granting managed-write authority. Built-in close links,
member drop witnesses and final family admission protect rename, delete and backup swaps.
Recovery checks existing roots and original identities, and uncertain delete moves retain
their original bytes instead of creating an empty replacement.

A dataset close callback that calls a catalog management method during a delete, rename
or in-place restore now gets a `catalog-disposing` conflict at once instead of waiting
forever on the catalog's management lock. The new `Catalog::defer` queues such a call to run
on the same thread once the disposing call has released the catalog. Deferred calls are
kept only in memory, and a panicking one is logged without stopping the others. A
reserved clone or restore publication refused this way changes no files.

A close callback that panics during catalog delete or rename no longer drops the dataset
from `config.json` on the next save. The dataset stays registered and closed until the
catalog reopens. Catalog startup refuses only a registered dataset whose directory or
CURRENT file is missing and serves the others. Retained delete bytes are named
`.kept-<name>-<token>`.

An in-place restore no longer fails with "the source family still has live owners" when
a views worker round briefly holds the dataset as the swap closes it. The swap retries
its ownership proof until the drain time ends.

Spilled ORDER BY can retry a smaller sort piece after a transient decoding reservation
fails. Infallible expression failures still stop the query, and memory caps stay enforced.

The successful dataset rename response is described under OpenAPI's `application/json`
media type. Its `name` and `renamedFrom` fields and wire behavior are unchanged.

Persistent datasets remain locked while an embedded caller retains a branch Store
or BranchSet after main closes, including when the branch was removed or retired.
The root lock stays held through those owners' resource destruction.

Dataset and branch administration emit audit events across HTTP, local commands and
MCP. Clone events follow actual registration and preserve the initiating actor and
selected source commit. Successful scratch expiry records a system actor, while failed
authorized operations record bounded codes without request payloads or credentials.

Rust Store snapshots and trusted Dataset history handles accept per-read memory,
cancellation and deadline limits while preserving exact retained branch origins.
Cold inherited reads prepare retained metadata and WAL state without recovery writes.
Bounded WAL replay polls cancellation and deadlines while verifying transaction checksums.
Pruned branch-time reads refuse unresolved states with the original selector and no
invented commit number, including equal timestamps after clock clamping.

History queries report the overlapping part of a recorded gap when the requested
window ends before the gap's final commit. The same inclusive bounds apply to
ascending, descending and filtered requests.

Background tasks release their server handles and task slots before publishing a final
status, including cancelled tasks whose work never ran. Closing the last server handle
after task completion releases the catalog lock without racing the worker's teardown.

The version 1 valid-time resource profile supplies RDF vocabulary, SHACL shapes, an
import normalization contract and typed active-claim and overlapping-claim stored-query
definitions. It preserves uncertain endpoints and temporal verdicts without asserting
the claim's proposition. C27 Phase 1 remains incomplete until server-owned availability
and backdating refusal pass A4; ordinary RDF timestamps do not authenticate that boundary.

Rust query callback bookkeeping remains available while thread-local owners are
destroyed. Late snapshots and callbacks retain the existing family and captured-view
checks, including sticky failures for a transaction retained by an earlier owner.

Dropping the last dataset handle now waits for its incremental views worker to
release the store before returning. Worker acquisition and shutdown are synchronized,
so a temporary host reference cannot leave the dataset directory locked after close.
A handle released by a host callback finishes shutdown after its enclosing view
operation releases its metadata locks. Synchronous callbacks, nested operations and
host replacement follow the same lifetime protocol. View operation tracking remains
available during thread-local object destruction.

Indexed nearest-neighbour ordering can join candidate batches with independent query
components. It continues searching when joins or filters discard an entire batch and
preserves duplicate solutions. Connected filters run before unrelated cross products.
Index-probe filters keep their original binding scope, with generic fallback for volatile
filters or filters that could observe new bindings after a move.

ARQ UUID aliases, unresolved dynamic function calls and effectful ARQ calls are excluded
from result-cache reuse and repeated independent nearest-neighbour inputs. These paths
share expression-purity checks while result caching retains its stricter treatment of
query-clock values.

The full-text integrity check retries segment replacements made by merges even when the
commit stamp stays the same. If the index keeps changing throughout its three attempts,
it requests another check instead of reporting missing files from superseded metadata.

The product of a decimal and a nonzero integer and the quotient of two integers are now
computed in one 128-bit operation, with the same results and overflow errors as before.
Decimal results take their inline ids without being written as text and parsed again,
and YEAR, MONTH, DAY, HOURS and MINUTES read the fields of an inline date or dateTime.
DBpedia's `births-by-decade` at the 50M slice takes 4.7 ms warm and 29.8 ms cold on
atlas instead of 48.9 and 64.5 ms, and on the full 1.24-billion-triple store it takes
58.9 ms warm on forge instead of 161.7 ms.

Eager results are now serialized into an 8 KiB buffer of the engine's own and written to
the response in pieces of that size, where the serializers used to write a few bytes at a
time through the response writer's checks. On forge the 300,191 rows of `optional-chain`
at 10.5M reach the client in 150.8 ms instead of 220.4 ms, `order-by-full` and `star-join`
take 19% and 30% less client time, and a plain SPARQL JSON answer of a small query takes
up to a third fewer instructions. The serializers are now compiled
with the engine, so their speed no longer moves with unrelated changes to the server. The
rows reported for an answer that stops partway are still the rows the client received.

The query route no longer names the query's patterns in the plan of an answer that does
not show the plan, which is every format but the Sparkles format, and an in-process
SERVICE call no longer names them in its dropped plan. The explain routes, MCP and the
library keep them. A plain SPARQL JSON request of a small query runs up to 5.8 thousand
fewer instructions.

`scripts/nsc-bench.sh` now records the compilers of each binary and refuses to compare
two binaries built with different ones unless `--allow-mixed-compilers` is given.

`scripts/nsc-bench.sh` reruns the queries it flags, at an execution-time ratio of 1.10 or
more, on the same instance in 8 rounds of 40 timed requests, and reports both passes.
`mise run test:partitioned` runs the nextest tests in shares on several machines from one
archive and checks that the shares ran every selected test. `scripts/ci.sh --changed
BASE` narrows a lane's Clippy and tests to the packages its change can affect.
`scripts/premerge.sh` checks the generated clients, binding lock files and Python parity
list of a merged batch. `scripts/gate/gate.sh` refuses to start on a host that lacks the
free disk after `mbx gc`. nextest retries the tests that `.config/nextest.toml` lists as
flaky and reports each retry. The four longest tests run as one test per configuration.

The batch gate selects its checks from the batch's diff. `scripts/gate/select-checks.py`
maps the files that changed since the merge base with `origin/main` to the
`scripts/ci.sh` tasks, the conformance suites and the spill matrices that they call for,
and the gate runs those without `--force`, so mise's task cache skips a check whose inputs
already passed on the gate host. Build configuration, the CI scripts and files that no
rule covers run everything, and `scripts/gate/nightly.sh` runs every check with `--force`
on `origin/main` each night. The conformance half goes to a second gate host only when
one is free. `gate.sh --cargo-jobs`, `GATE_CARGO_JOBS_HOSTS` and a default from each
host's memory cap set the compile jobs. `mise run test:w3c` and the spill matrices run
their tests in parallel, which took the W3C suite from 433 to 82 seconds on a 20-core gate
host. The binding checks key their task cache on the crates the bindings compile, and
seven more slow tests run as parallel parts with the same cases.

The first run of a query text that the server does not remember now runs on the thread
that received it under the request's timeout when the server has been idle for 10 ms,
instead of on the blocking pool. On forge that took about 1.8 ms off a fresh server's
first request, and the BSBM explore mix, whose texts are all new, kept its speed. Such a run cannot be cancelled by a client disconnect, only by its timeout, and a
text whose run is slow or fails, or that contains `SERVICE`, runs on the pool. A SERVICE
IRI now names this server only when it is written in the URL parser's own spelling, so
forms such as `127.1` or a path with `..` are ordinary HTTP calls.

Automatic execution now streams groupings and counts over an estimated 20,000 input rows
or more whose EXISTS and NOT EXISTS are FILTER conjuncts that a key set answers. On a
Namespace instance at 10.5M quads these ran 0.68 to 0.99 times as long as eagerly. An
EXISTS in BIND, an OPTIONAL condition or a larger expression keeps the plan eager.

A finished eager SELECT or ASK now keeps only its result's estimate in the query memory
pool instead of its query's peak, so a held result or a Python or Node result object no
longer reserves the memory of the tables it was built from. CONSTRUCT and
DESCRIBE results keep their peak as before.

MCP tool calls and resource reads now wait for the query memory pool like other queries
and answer `503` with `Retry-After` when the admission wait passes, where they used to fail
with `server-memory`. The queries of `sparkles ask`, the GraphQL draft, reasoner
diagnostics, the Graph Store DELETE and tabular uploads now charge the grant their request
was admitted with instead of taking a second one or none.

With the query memory pool on, the server now admits a query or update after its body has
arrived, so a client that sends slowly no longer holds 16 MiB of the pool while it sends.
Uploads, patches and Graph Store writes no longer take an initial grant or wait for the
pool, and a release of memory wakes only the waiters that fit.

A SERVICE call from the SPARQL query or update endpoint to this server now runs in
process under the caller's principal, grant and deadline, where it used to wait for the
memory pool behind its caller and fail with `503`. Only an IRI that names the configured
public URL or the bound listener exactly counts as this server, and such calls nest at
most eight deep.

Automatic execution now streams a plan without spill when its eager output is expected
to outgrow the memory the query may use while the cursor's own state fits. BSBM BI's q4
with ProductType6, a `FILTER NOT EXISTS` over a Cartesian product, now answers at 10M in
15.1 s with 282 MB of server memory under the default pool, where the pool refused it. A
request that needs more than the whole query memory pool now answers `507` without
`Retry-After`, since a retry cannot succeed.

In the JVM library, `abort()` or a timeout that raced the end of a query could throw
`IllegalStateException` because the query's native object had just been released. Such a
cancel is now ignored, since the query has already finished.

The reasoner's limit of 50 million inferred triples is now a setting. `sparkles infer
--max-inferred N`, `serve --reason-max-inferred N`, the `maxInferred` field, form field
or query parameter of `POST /$/reason/{ds}`, `ReasonRequest::max_inferred`, Python's
`max_inferred=`, Node's `maxInferred` and the JVM's `ReasonOptions.maxInferred` set it,
and 0 removes it. A request may only lower the server's limit, and a higher one is
refused with `400`. A run past its limit now fails with the error code `inferred-limit`
instead of an invalid-input error, so Python raises `SparklesError` with that code where
it raised `InvalidInputError`, and the message names the rule that produced the most
candidates and the settings that raise the limit.

The `small` memory profile no longer slows cold lookups. A prefetch request may add an
eighth of the block cache, but at least 64 MiB or a quarter of the cache, whichever is
smaller, so the profile's 256 MiB cache admits the parallel decode that a cold
`star-lookup` at 10.5M needs. Caches of 512 MiB and more keep the eighth.

Group commit now covers datasets with a full-text, spatial or embedding index, which used
to take the ordinary path. On a Namespace instance whose synchronizations were slowed to
4 ms each, four writers on such a dataset committed about twice as fast and sixteen
writers about eight times as fast. On the instance's own synchronization of about 0.1 ms,
four writers gained nothing and sixteen writers about 1.6 times. On `forge` and `atlas`,
whose drives synchronize in 0.25 to 1.7 ms, four writers committed 1.5 to 2.4 times as fast,
single commits stayed within 5% of the ordinary path's latency, and readers of a full-text
or spatial index kept theirs under a paced load, which meets every gate of spec X09
section 17.9. A fence costs more than an ordinary commit when the drive is slow, because it
ends with two device cache flushes where an ordinary commit writes through. A commit falls
back only while a full-text index recovers or rebuilds or while a spatial index builds or after its build
failed, and `sparkles_group_commit_fallbacks_total` reports these as `text-recovering`,
`text-rebuilding` and `geo-unavailable` in place of the old `text`, `geo` and `embed`
reasons. A full-text search on a transaction that started from a commit still in flight
waits for that commit's fence. `--no-group-commit` and `SPARKLES_GROUP_COMMIT=off` still
turn grouping off.

On a store of more than ten million quads, the schema reports of the MCP tools,
`check_query` and `sparkles ask` take the counts of predicates and classes from the
statistics of the index build, corrected exactly for the changes since and for graphs
outside the selection, and say `"basis": "statistics"`. A selection that leaves out
graphs with more than a million quads is still scanned.
The kinds of a predicate's objects are measured when a tool needs them, from a sample of
8,192 stored literals above 200,000 quads. On DBpedia's billion quads a `check_query`
took 52.7 seconds and 11.1 GiB and now takes 0.3 seconds and under 500 MiB, and an
Ask takes about two seconds instead of 100. `--schema-statistics-above QUADS|off` on
`serve`, `mcp` and `ask` sets the threshold. `/{ds}/schema` and `sparkles schema` still
scan. A dataset keeps up to four schema reports, one per selection, instead of one.
`describe_schema` leaves `maxPerSubject` out of a report from the statistics and adds
`objectsSampled` to a predicate whose objects were sampled.

The planner moves a VALUES table that shares variables with a UNION of the same join
into each branch, so each branch looks its rows up by index instead of reading every
quad of its pattern. It moves at most 10,000 rows, keeps the table outside a BIND or
FILTER that calls a non-deterministic function, and leaves a UNION with a SERVICE, a
LATERAL or a property function call as written. `link_entities` and the terms list of `check_query` failed on the
2 GiB query budget on DBpedia for this reason. `sparkles ask` gained
`--query-memory-mb`, 2048 by default as in `sparkles mcp`.

The server's query memory pool is on by default. It holds half of the memory the process
may use, less the configured caches, and at least 256 MiB. Each query and update request
reserves 16 MiB when it is admitted, so a host whose pool sits at the 256 MiB floor serves
at most 16 concurrent queries and updates. Further requests wait for
`--query-admission-wait` and then answer `503` with `Retry-After`.
`--total-query-memory-mb 0` turns the pool off.

`--cache-mb`, `--result-cache-mb`, `--service-cache-mb` and `--history-cache-mb` now size
one cache for the whole server instead of one per dataset. The default result cache fell
from 512 MiB to 256 MiB and the default history cache from 1 GiB to 512 MiB.
`StoreOptions::default` takes the same result and history sizes, which changes the
defaults for library users. `--memory-profile small` sets smaller sizes for constrained
hosts.

A plain query takes memory from the query pool in steps of 64 KiB, so it can hold up to
64 KiB of the pool beyond its estimate. That removed most of the pool's cost on queries of
a few microseconds. The server and the Python, JVM and Node.js libraries call mimalloc
through its plain entry points, so a full decoded block column takes 256 KiB of heap
instead of 320 KiB. A server with ten datasets at `--cache-mb 256` used 493 MiB of
anonymous memory after the suite instead of 573 MiB.

Queries over HTTP, in the local CLI, in Node and in the JVM library now run in automatic
mode by default. A plain scan that is expected to return at least a thousand rows from
immutable blocks streams as a cursor, and so does a grouping that reads an estimated
20,000 rows or more, counts over OPTIONAL and MINUS included, since the cursor's column
kernels measured 0.50 to 0.91 times the eager time on such queries. Every other query
runs eagerly as before. A streamed
answer that fails after its first 1 MiB aborts the transfer after a `200` status, so
only a completed transfer is a complete answer. A streamed answer reports no total in
advance. The native metadata of a scan of a thousand rows or more therefore reports an
unknown total with `send`, and a Node result of such a scan has no `size`, where both
used to appear for every answer under a million rows. For Rust callers
`ExecutionMode::default()` is now `Auto`, and `Dataset::query_execution` uses it, while
`Dataset::query` stays eager. A request opts out with `execution=eager`, a server with
`serve --default-execution eager`, the CLI with `--execution eager`, Node with
`execution: 'eager'` and Jena with `Sparkles.STREAMING_EXECUTION` set to false. A server
remembers a streamed answer that completed in one body in under 10 ms as a quick query,
as it does an eager one, and runs its next request eagerly in place.

A Jena result that streams can throw from `hasNext()` partway through, and Jena queries in
a write transaction run eagerly. A Jena query iterator now throws
`JenaTransactionException` when it is read after its transaction has ended, eager or
streamed, where an eager result used to hand out its remaining rows. This covers an
iterator created in a read transaction and first read after the transaction ended, and
a final `hasNext()` after the last row when it comes after the end of the transaction.

The deadline of a streamed answer now counts the query's own time. The time a query spends
blocked on its reader, which is the response channel over HTTP and the gap between two
pulls in Node, no longer counts against `timeout`, so a slow but steady reader of a large
export is not cut off. Over HTTP a client that reads nothing for longer than the
request's timeout ends the stream as a timeout. This applies to `execution=streaming` as
well. A local `sparkles query --timeout` still counts the time its output is blocked.

`sparkles serve` spills to disk by default, and `--spill off` turns spill off. A streamed sort without LIMIT, a grouping, a
DISTINCT, a hash join and a long run of equal keys in a merge join write what does not
fit in their memory budget to `--spill-dir` (default `<data>/.spill`) and finish. The disk is limited by `--spill-max-mb` for the
server and `--query-spill-mb` for one query, a request may lower the latter with
`spill-mb`, and a query over it fails with the new `spill` budget and `507`. With spill
on, automatic execution also streams a query that eager execution would hold in more
than half its budget, and runs an eager SELECT that exceeds its memory budget once more
as a cursor unless it calls SERVICE anywhere. In `SELECT DISTINCT … ORDER BY` the
DISTINCT runs below the sort, numbering its input rows so that the sort keeps the order,
and may spill. A spilled hash join keeps its probe input's order only when an operator
above it may rely on that order, and otherwise emits its partitions in turn.
`--spill encrypted` seals every spill frame with AES-256-GCM under a random key for each
file held only in memory, so a changed, moved, reordered or truncated frame fails the
query. When the spill directory cannot be used, sits on tmpfs or ramfs, is not owned by
the server's user or is writable by others, the server fails to start with `--spill on`
or `--spill encrypted` given, and without `--spill` it logs a warning and runs without
spill. `--spill off` leaves the spill directory unopened. `--spill-max-mb 0` is refused. `/$/server/memory` adds a `spill` object and
`/$/metrics` the `sparkles_spill_*` series, `sparkles_spill_eager_retries_total`
among them. A streamed grouping over a join can concatenate a `GROUP_CONCAT` in another
order than an eager one, which SPARQL allows, and an ordered subquery fixes the order in
both. Rust callers set
`QueryOptions::spill` and `max_spill_bytes`, and `BudgetKind::Spill` is a new variant.

A cursor operator that reaches its end gives back what it and the operators below it
hold for reading, such as the decoded block that each scan keeps charged, and a later
pass of a spilled grouping or DISTINCT sizes itself from the memory it has when it starts.
A spilled hash join below a grouping of COUNT, SUM, AVG, MIN and MAX, or below a FILTER
under such a grouping, no longer writes its output a second time to restore its probe
input's order, so a spilled SUM or AVG of doubles may round differently in its last bits
than in memory. A SUM or AVG of integers keeps a 128-bit running sum and is unbound only
when the final sum leaves the `xsd:integer` range, where it used to fail whenever a
partial sum did. A
DISTINCT over input ordered on some of its columns holds one run of the first order
column at a time instead of every row it emitted.

At 10.5M on forge with eight threads, the grouping above a spilled join takes 1.5 s
under a 4 MiB budget where it took 16.4 s, and writes 101 MB where it wrote 444 MB. A
DISTINCT over input ordered on its first column holds 1.2 MB where it held 479 MB, and
the cursor takes 174 ms where it took 800 ms. A server with spill on and the same server with spill
off returned the same answers to all 30 synthetic queries, and their server times had
geometric means of 0.991 and 0.995 for spill on over spill off.

A streamed FILTER over an index scan reads the scan in morsels of whole index blocks and
filters each morsel in parallel when every conjunct is a key test on the scan's key
column, and a STRSTARTS filter reads only the blocks its prefix can match. At 10.5M with
eight threads on an 8-vCPU AMD Namespace instance,
CONTAINS, STRSTARTS and LANGMATCHES filters over names and titles now take 0.91 to 0.95
times their eager time, where they took 1.48 to 7.99 times. More threads hold more morsel
scratch at once, 13.5 MB in the ledger at eight threads against 11.8 MB before.

A streamed range top-k no longer reads the parts of its range whose ids do not follow
value order into memory. It ranks them in parallel morsels of one index block and keeps
only its best rows, so its memory no longer grows with the range. A default graph made of
several graphs still holds each triple once across the morsels. At 10.5M with eight
threads on 8-vCPU AMD Namespace instances it takes 0.89 to 1.06 times its eager time over
several runs, where it took 1.33 times.

A streamed answer's term cache sorts its vocabulary ids with a stable sort that merges
the columns already in index order. With the parallel filter, a STRSTARTS filter over
names in native JSON at one thread takes 1.01 to 1.03 times its eager time, where it
took 1.30 times.

`sparkles migrate --check` exits 1 for a database with an interrupted run, which a server
refuses until `sparkles migrate` runs without `--check`, and its JSON report adds `opens`.
`sparkles migrate` keeps the mode and owner of every file it copies, and it must run as
the database's owner or as root.

Group commit is on by default for persistent stores. Concurrent commits of a dataset share
one synchronization of the log, and each commit is still acknowledged only once it is
durable. On `forge` four writers commit about 1.8 times as fast as before and sixteen
writers about 5.4 times as fast, while a single writer's latency is unchanged. A dataset
with a full-text, spatial or embedding index keeps committing on its own. The global flag
`--no-group-commit` or `SPARKLES_GROUP_COMMIT=off`, `false`, `no` or `0` turns grouping
off, and an unrecognised value of the variable logs a warning and keeps it on. Node's
`Dataset.open` and `Catalog.open` take `groupCommit`, and Python's `Dataset`,
`Dataset.open` and `Catalog` take `group_commit`, both `true` by default. The JVM library
follows the default and has no option. Embedded Rust sets `StoreOptions::group_commit`,
which now defaults to `true`. Grouping costs memory. Each persistent dataset may hold up
to 64 queued commits or 4 MiB of log records, plus the snapshots of commits in flight,
whose size was not measured. The hidden `--experimental-group-commit` flag is removed, and
passing it is a usage error.

The closure a dataset keeps for incremental reasoning runs holds only the derived triples
and reads the input graphs from the store's indexes. On LUBM-style data with 13.1 million
triples it took 918 MiB instead of 2,267 MiB for RDFS and 1,226 MiB instead of 2,654 MiB
for OWL 2 RL, and a run that reads the closure back from a persistent dataset took 40% to
73% of its previous time. This trades speed for memory. A change of 1,000 triples with a
kept closure took 1.3 to 4.9 times longer at 16 threads, up to 142 ms on that data,
because it reads the input graphs from compressed blocks. The kept closure now remembers
the answers of its lookups of the input graphs between runs, up to 64 MiB charged to the
memory budget, and the searches of a deletion round run in parallel. That brings such a
change to 12 to 31 ms for RDFS and 51 to 88 ms for OWL 2 RL at 16 threads on that data, 1.0
to 1.6 and 1.5 to 2.4 times its time before the closure dropped the explicit facts, for 10
to 18 MiB more kept memory. The goal of roadmap item 87 is 1.2 times, so the item stays
open. A read that the remembered answers serve counts in `baseScans` as the scan it
saves. `serve --reason-cache-triples` now counts derived triples only, so the
same limit keeps the closure of a larger input, and `baseScans` of an incremental run
counts every read of the input graphs.

`literal-not-in-datatype` finds ill-typed literals by a scan of the vocabulary that it
remembers per generation, instead of a query over every literal value, which took 11 to 12 s
on 13.1 million triples and now takes under 20 ms. The generation keeps the ids of at most
262,145 ill-typed literals, 2 MiB, and frees them with itself. With the inferences included,
it also reports the literal typings among the generalized triples of an RDFS or OWL 2 RL
materialization, read from the closure the dataset keeps or from the generalized triples a
persistent dataset saved, with the basis `uses-inferences`. They are reported for the
literals that the checked graphs hold. For Rust callers,
`sparkles_reasoner::diagnostics::diagnose_with` takes these typings, and
`sparkles_reasoner::literal_typings` reads them.

The Ask evaluation harness converts Text2SPARQL'25 (`scripts/text2sparql-convert`, the
DBpedia set and the CK25 corporate set) and LC-QuAD 2.0 (`scripts/lcquad2-convert`).
Every converter that recomputes gold answers on a database now also compares them with
the answers the source publishes and records the load manifest of that database. The
paid wrapper `scripts/eval-ask-anthropic` lists the ledger's charges that stand at an
upper bound with `--held` and replaces one with the billed amount and its evidence with
`--reconcile`. That is how the $1.002560 that the 2026-10-15 pilot left unresolved can
be settled once the billed amount is known. An amount above the held charge needs
`--allow-raise`, and the evidence is 10 to 1000 printable characters. The wrapper
decodes a reply that comes back gzip or deflate encoded, settles any fault of a request
at its bound, and charges Sonnet 5.5 cache reads at the published $0.20 per million
tokens. The DBpedia gold rewrites leave literals and comments alone, never reuse a
variable of the query and also rewrite `COUNT(*)`. A sampled LC-QuAD 2.0 set records how
many questions it sampled.

Ask grounding now reads from the data which predicates the linked entities use as
subject and as object, and which a sample of 200 instances of the two classes that best
match the question use, and lists them by namespace with their counts. The queries run
as the caller, each within five seconds and the MCP limits, and search the readable
named graphs only when the caller can read one. `link_entities` takes `popularity`,
which orders label matches by how many triples name each one, counted up to 2000, and
reports that count as `uses`. Ask asks for it, so a well-connected entity comes before
a namesake that shares its label. Mention detection keeps a capitalized "The" inside a
question and a number after a name. The draft prompt says that the context lists only
part of the vocabulary and asks for one predicate per relation instead of a UNION or a
`VALUES` list of candidates. A first-draft prompt grows by about 8%, and each Ask runs
two to three more bounded queries per linked entity and one per matched class.

The Ask evaluation scores every case under the guarded secondary policy
`rdf-term-set-projected-guarded-lenient-v3` beside the strict score. It applies the gold
errata `gold-errata-v1`, drops empty-string rows, compares numbers by value, matches an
unambiguous label to the gold IRI it names, fails an answer with more than three extra
columns and sets apart passes that rely on a predicate column or on rows the projection
drops. An Ask child that SIGKILL or a failed allocation ends is the failed case `killed`
instead of a harness fault. `scripts/qald9plus-convert --split train` converts the
QALD-9-plus training split, and `eval_support/grounding_recall.py` measures from saved
prompts whether each gold term reached the prompt. The dry-run fake provider reads
prompts that arrive as cached text blocks.

`sparkles ask --questions FILE` asks every question of a JSON-lines file with one store
and one model configuration and prints each result in input order, and `--jobs N` runs
up to N questions at once. Each question keeps its own deadline and budgets, and a
failed line gets its own error result. `scripts/eval-ask-anthropic` asks each phase
through one such child in a 16G systemd scope, holds each case's projected cost before
its line is sent and keeps the outputs in question order. Caches stay warm across a
phase's questions, which changes latency but not answers.

Ask has four repair options. The exploration, on by default, drafts once more after a
draft gives no query or an empty result, with further link candidates and the predicates
the entities use. The shape check repairs once when a result does not fit the
question's form, `drafts` asks up to five first drafts and keeps the checked, complete
result most of them share, and type hints link mentions with the classes the words
around them name. These three are off by default. `assistant.json` sets the options
under `repairs`, `POST /{ds}/ask` takes `explore`, `shapeCheck`, `drafts` and
`typeHints`, and `sparkles ask` takes `--explore`, `--shape-check`, `--drafts N` and
`--type-hints` with `--no-` forms. A request or a command line overrides the dataset. An
exploration's sample values and a shape repair's rows reach the model only when the
dataset's `send` policy lets the draft and repair providers receive rows.

An `openai` provider takes `promptCaching` with `enabled` only, false by default. When it
is true, an Ask repair goes as the first draft's user message, unchanged, followed by a
second user message with the repair and the format. OpenAI writes a cached prefix only
where the latest user message ends, so the repair then reads the prefix that the first
draft wrote, where before every OpenAI repair missed the cache and paid the write price
again. The settings form still offers prompt caching for the `anthropic` kind only.

The `openai` provider preset starts with GPT-6 Luna in place of `gpt-5-mini`. In a
comparison of six Claude and OpenAI models on 150 held-out QALD-9-plus questions, Luna
answers 106 at $0.0021 a question, within the noise of the best model's 113, and is the
cheapest per correct answer. The `anthropic` preset keeps Sonnet 5.5 by the same rule.
Both hosted presets give their model the settings the comparison used, a 200,000-token
context and effort `medium` with its output limit, in the CLI and in the Add provider
form.

`scripts/eval-ask-paid`, which `scripts/eval-ask-anthropic` still runs, takes a run
file's `provider`, so a paid run can ask OpenAI's GPT-6 Luna, GPT-6.1 Terra and GPT-6.1
Sol through the Chat Completions API under a wallet of its own, capped at USD 50 per
ledger. The proxy prices OpenAI's cache reads and writes, checks the model that served
each reply for both providers, and its ledger, now format version 3, names its provider.
Model configurations take `reasoningEffort`, which an `openai` model sends as
`reasoning_effort` with `max_completion_tokens` and an `anthropic` model as
`output_config.effort`, and an OpenAI answer's cached and written prompt tokens count
as cache reads and writes. Sonnet 5.5 cache reads cost $0.10 per million in the proxy,
as the Console export billed them.

The Ask evaluation scores under `rdf-term-set-projected-columns-v2`. An answer with more
columns than the gold, such as a name beside each IRI, is projected onto the gold's
columns and its rows compared as a set, where the first policy scored any extra column
as wrong. Each row also reports the accuracy without projection and how many correct
answers needed one, and `--scoring-policy rdf-term-set-global-columns-v1` scores under
the first policy. `scripts/eval-ask-rescore` scores a run's saved answers again under the
current policy without calling a model, and the paid wrapper records every request and
reply, key redacted, in `replies.jsonl`.

RML-Core join conditions can be written as expression maps. The `rml:childMap` and
`rml:parentMap` of a join condition take a reference, a constant, a template or a
function, and the join compares the strings they make. A join with an expression map runs
in memory, and one with plain references still runs in SQL when both tables are in one
SQLite database. The `rml:UnsafeIRI` term type now inserts template values without
percent-encoding and fails the run when the result is not a valid IRI. All RML-Core test
cases except RMLTC0027b pass, whose expected output holds IRIs with spaces.

The W3C CSVW test suite is vendored under `testsuite/csvw` and run by
`mise run test:csvw`. All 270 RDF tests pass in minimal and standard mode. To get there
the converter checks UAX 35 number patterns, date, time and `dateTime` bounds, binary
lengths in octets and the consistency of datatype and metadata properties, and it
converts common properties that are JSON-LD node objects or value objects to RDF, where
it failed before. It adds `rowTitles`, schema references given as URLs, foreign keys that
reference their own table and `_col.N` names for columns the metadata does not describe.
Library users can set `tabular::Options.lenient` with `csvw::parse_with` to follow
CSVW's processing rules, which warn about a problem and use the default and keep a cell
that fails its datatype as a plain string. The default stays strict, and the command
line and uploads do not expose the mode.

A bulk commit and a compaction switch to a new generation through one shared function,
and the compaction's last catch-up sync uses the store writer's vocabulary-sync worker
instead of starting a thread for each sync. A bulk commit reads the size of its new
log before it switches `CURRENT`, so a failed read no longer leaves the writer on the
old log after the switch (X14, item 474).

The `pruned_join_order` optimization switch is gone, because the planner now orders
joins with the program on cost summaries only. `SPARKLES_DISABLE_OPTIMIZATIONS` warns
about the name as it does about any unknown one. A group of up to 12 inputs that share
more than 128 variables now keeps the greedy plan instead of the exhaustive program's
(X14, item 475).

The optimization switches `flat_hash_join`, `selective_columns`, `delta_statistics`,
`filter_key_ranges` and `filter_id_ranges` are gone, and their optimizations always
apply. `SPARKLES_DISABLE_OPTIMIZATIONS` warns about these names, and
`Optimizations::NONE` now uses the flat hash table, selective column decoding, the
corrected statistics and the key ranges of a fixed string start (X14, item 476).

Every reader of RDF syntax names, file extensions and media types uses one registry,
so the aliases that one place accepted work everywhere. `--format`, `format=` and
syntax names accept `n-triples`, `n-quads`, `rdf/xml`, `rdf-xml` and `xml` wherever
they name a syntax, `.owl` and `.xml` files read as RDF/XML in ShEx schemas and Fuseki
configuration files, and `.json` configuration files read as JSON-LD. Durations accept
`ms` and compound terms such as `1d 12h` everywhere the server or the backup policy reads
one, and the auth configuration accepts weeks. The change-log index syncs its directory
after it replaces its file. `sparkles map draft` escapes line breaks and quotes in table
and column names, including in the template of a table without a primary key, which
could produce invalid Turtle, and escapes backslashes in template column names (X14,
item 477).

The environment variables `SPARKLES_NUMERIC_COLUMN` and `SPARKLES_WRITEBACK` and the
hidden server flags `--no-wal-direct-writes` and `--no-group-commit-idle-direct` are
gone. They selected an implementation for comparisons, and the numeric column, steady
writeback, direct log writes and the idle group bypass now always apply outside tests
(X14, item 478).

### Fixed

`check_query` no longer fails with `budget-memory` when a draft uses a predicate that no
instance of its typed variable's class has. On the DBpedia 2022.12.01 database the
class lookup's estimate passed the 2 GiB query memory budget, which failed the whole
check, so Ask lost the draft instead of repairing it. The lookup now writes its terms
into the query and leaves the pattern unjudged when a lookup still passes the budget.
Its suggestions profile a sample of the class through the index and rank the sample's
predicates by their share of it and their rarity in the dataset, with the namespace of
the draft's predicate first, so that a link every page has ranks below a predicate of
the class.

`scripts/premerge.sh` and the development tasks no longer fail when Boxington has
collected a checkout's managed target and left `target` as a dangling link. Premerge now
places a fresh target with `mbx sparkles-task` before its checks, with a one-line
message. The crash harness's test binary, the feature-test logs, the fetched test
suites, the GeoSPARQL benchmark checkout, the Graphalytics results and the JVM Nix
dependency script now live under `.artifacts`, where collection does not reach them.
`mise run ui:wasm` runs inside a target lease, so the module that wasm-bindgen reads
cannot be collected after the build. The benchmark and probe scripts copy a binary that
lies in the managed target to their work directory before they start it.

A rename or delete of a persistent dataset no longer fails with `409` and "still has
live handles" because the dataset's own views worker started a round at the wrong
moment. Each open dataset runs a views worker, and a round of that worker holds the
dataset until it ends. The first round of a new dataset could start after the server's
wait for holders had seen the dataset free and before the catalog checked it, which
happened on loaded hosts right after a dataset was created. The catalog now keeps the
views workers of the dataset and its open branches from starting a round while it checks
for other handles and closes the dataset, and waits up to two seconds for a round in
progress to end. Handles of clients and requests still refuse the change, and a round
that outlasts the wait answers `409` with "is still updating its views".

Zero-or-more, one-or-more and zero-or-one property paths stay within the query memory
budget and the timeout in eager execution. A path with two unbound ends, such as
`?cat skos:broader* ?c`, listed its start nodes by copying every quad of the active
graph into memory, with no budget charge and no deadline check. A sequence or
alternative path built its edge relation the same way, and the visited set and frontier
of each closure were charged only in a cursor. On DBpedia 2022.12.01, the `ASK` that
`why_empty` runs for such a pattern alone grew one `sparkles ask` process past 28 GB
under a 2 GiB budget and a 6 s timeout. Start nodes are now read a few thousand at a
time from the indexes, skipping past the quads of a node already taken. The visited set,
the frontier and the edge relation count against the budget in both executors, which
check the deadline while they scan and between index lookups. An `ASK` or a `LIMIT`
over such a path walks one start node at a time and stops once it has its rows, so the
DBpedia `ASK` answers in under a second. A closure over an alternative of predicates and
inverse predicates, such as `(dbo:wikiPageRedirects|dbo:wikiPageDisambiguates)*`, walks
from a bound or constant end by index lookups instead of first reading every edge of
the alternative. `why_empty` reports a check that exceeds the memory or row budget as
unknown, as it does a check past its time, instead of failing the whole diagnosis.

A PDF converted in a child process no longer fails with "the converter stopped on it"
on a host with many cores. The PDF parser's first parallel loop started rayon's global
pool with a thread per core, and each new thread made glibc reserve a malloc arena of
64 MiB of address space, because Rust's standard library allocates with glibc's malloc
when it installs a thread's stack-overflow handler. On a host with 20 cores the child
reserved 1.6 GiB, so it ran out of a 1 GiB limit and panicked, and a server with the
default limit of 2 GiB would fail the same way on about 32 cores. The child now caps
glibc at two arenas and its rayon pool at four threads, which it starts under its limits
before it reads the file, so the startup probe also finds a limit that the threads do not
fit. The same PDF now reserves about 400 MiB in a debug build. A refusal for a child that
ended without a result quotes the end of the child's error output, such as a panic's
message.

A delete or rename of a dataset waits up to two seconds for the requests and workers
that still hold it, such as the first round of a new dataset's view worker, before it
answers `409` with "still has live requests". Right after a create or clone, such a call
no longer fails now and then.

A view or live query whose `FILTER EXISTS` or `FILTER NOT EXISTS` holds a path that
matches the empty path, such as `:p*` or `:p?`, from a variable of the outer rows now
gives the query's answer (roadmap item 439). The circuit joined the outer rows with
the path evaluated alone, which binds the variable only to nodes of the active graph,
while `EXISTS` substitutes the outer value and the zero-length step matches it whether
or not the graph holds it. `FILTER NOT EXISTS { GRAPH ?g { ?x :knows* ?y } }` kept a row
for an `?x` that no named graph held. Such an `EXISTS` is now evaluated again by the
engine at each pass that touches its footprint. The compiler version changes, so view
checkpoints of an older release are refused and their views are built again.

A FILTER of a group no longer runs before the group's other patterns can bind its
variables. The planner placed a FILTER on part of the join as soon as that part could
bind every variable of it, even when a UNION branch, an OPTIONAL, a failed BIND or an
`UNDEF` of `VALUES` left one of them unbound and a later pattern of the group bound it.
The FILTER then saw the variable unbound and dropped rows that SPARQL keeps. Such a
FILTER now runs once every pattern that binds its variables has joined. An equality
among them still keys the join of two parts of the group that share no variable, so
the fix adds no Cartesian product, and every other FILTER is placed as before.

`{ ?x :p 1 } UNION { ?y :q 1 } ?z :r ?x FILTER(sameTerm(?x, ?y))` returns the row in
which the second branch's `?y` meets the `?x` of `?z :r ?x` (roadmap item 433). The same
holds for `=`, `!=`, `STR`, `BOUND` and conjunctions of them, and for an OPTIONAL group
in place of the UNION.

`VALUES ?x { UNDEF } ?x :knows+ ?y FILTER(BOUND(?x))` returns the rows of the path,
which binds `?x` (roadmap item 434). The same holds for the `*`, `?`, sequence and
alternative paths and for `VALUES` tables that mix `UNDEF` and bound rows.

A dataset can set how long its scratch branches may sit idle before they expire, with
`memory.scratchBranches.ttl`. The value is a duration from `1m` to `3650d`, such as
`24h`, or `off`. It overrides `--mcp-scratch-branch-ttl` for that dataset, so the flag
is now the default for datasets without the setting. The server reads the settings at
each pass, which starts whenever `--mcp` runs with `--mcp-allow-update`, and
`list_branches` and `create_branch` report `expires` from the dataset's value.
`sparkles mcp --loc --allow-update` now expires idle scratch branches too, from the
datasets' setting alone, once when it opens the databases and every ten minutes after
that. `--no-scratch-expiry` turns it off for a session. A stdio session without the
setting behaves as before.

On `serve`, idle scratch-branch expiry now needs `--mcp-allow-update`. Before, `--mcp
--mcp-scratch-branch-ttl DURATION` deleted idle scratch branches without it. Operators
who run that combination no longer get expiry, and must also pass `--mcp-allow-update` to
keep it. The same applies to a dataset's `memory.scratchBranches.ttl`. A server without
`--mcp-allow-update` never deletes a scratch branch, and one with `--read-only` never did.

### Behavior

The `owl-rl` profile now has every rule of the OWL 2 RL/RDF tables except `eq-ref`,
`dt-type2`, `dt-eq` and `dt-diff`, so OWL 2 RL materialization derives more facts. It
adds `prp-key` for keys of 1 and 2 properties, `cls-maxqc4`, the `owl:equivalentClass`
and `owl:Nothing` conclusions of `scm-cls`, `scm-op`, `scm-dp`, and the axioms of
`prp-ap` and `dt-type1`. Property chains still stop at 3 properties.

`sparkles ask` decides whether a draft calls SERVICE from the parsed query, including
the patterns inside EXISTS and subqueries, instead of scanning its text. A query whose
class, variable or prefix is named like the keyword, such as `pv:Service` or `?service`,
now runs, and a query that hides the keyword from a text scan is still left unrun.

A SERVICE inside an EXISTS that the query evaluates per row is called once for each
distinct binding of the variables it shares with the outer rows, and once in all when it
shares none. A streamed query whose memory budget was nearly spent used to call it once
per row.

Open deletes a generation directory newer than the one `CURRENT` names when it is an
unfinished build of the dataset, and the deletion is permanent. A power loss during a
compaction or a bulk load can leave such a directory with an empty build marker, which
earlier releases kept until the next rebuild reused its number. Any other directory above
`CURRENT`, such as one that a `CURRENT` restored from an older copy no longer names, is
renamed to `gen-N.set-aside-<ms>` and kept, and `sparkles check` reports it. A bulk build
that a crash stopped before its switch is always set aside, as a full copy of the indexes.
Set-aside directories count toward the byte quota and appear in `sparkles migrate`'s list
of generations until an operator deletes them or renames one back. Open also
deletes the store's own temporary files, such as a `CURRENT.tmp` left by an interrupted
switch, and leaves every other `.tmp` file alone. A directory that holds generations but
no `CURRENT` is now refused instead of opened as a new, empty database.

A commit whose own log synchronization fails keeps its message, change digest and merge
record. Such a commit can still come back after a restart, and it used to come back
without them, which left a hole in the digest chain of a dataset with `--commit-digests`.
The writer stops taking commits after the failure as before, and open drops the records
of a commit that did not survive.

FastRP follows the meaning of `iterationWeights` in Neo4j GDS. The list has one weight
for each propagation, and the new `nodeSelfInfluence` parameter weights the random
starting vectors. The default `(0.0, 1.0, 1.0)` now propagates three times where it
propagated twice, so FastRP embeddings written by earlier builds differ from new ones and
a default call costs about a third more. A call whose weights are all zero with a zero
`nodeSelfInfluence` is refused. A write-back graph algorithm run now has the update
timeout instead of the query timeout.

A query plan deeper than 256 levels, as `/{ds}/explain`, the Sparkles result format and
the cursor write it, is in plan format 2. Its root has `"planFormat": 2` and its nodes
have no `id`, which a client derives from the shape. Shallower plans are unchanged.
Planning a basic graph pattern near the plan limit is faster by 1.1 to 33 times
depending on its shape, 3 to 4 times for a plain chain or its reverse. A star on one
frequent predicate gains the least. Planning observes the request's timeout and cancellation
throughout.

### Format

The planner statistics in `gen-*/stats.json` gain the member `correlations`, which
carries its own version, 1, and is registered as the derived kind `planner-statistics`.
It holds the remainder of the subject characteristic sets and the node characteristic
sets with their remainder. A member of another version is refused with a recorded reason,
`sparkles check` reports it, and the planner estimates as it did before. Releases without
it ignore the member, and stores loaded before it keep their estimates until the next
bulk load or full compaction.

The resource catalog is the server file `resources.json`, format 1, registered as a
server settings file. A file of another format, or one that does not parse or breaks the
catalog's rules, is never rewritten, and every catalog route answers
`503 catalog-unreadable` while the datasets keep serving.

Internal circuits are checkpointed to `circuits/{id}/main/state-{commit}.bin`, the new
derived kind `internal-circuit-state` version 1 with the magic `SPKCIRC\0`. A file of
another version is left in place and the circuit is built from the data, and an
accumulator built so counts from its build. Releases without internal circuits ignore
the directory.

`serve --prewarm` writes `prewarm.json` version 1 into each persistent dataset's database
directory. The file names the generation it was recorded for, the decoded index columns of
the block cache and the resident page ranges of the generation's other files. A start reads
only a list of version 1 for the current generation. It ignores a stale list, and it
refuses a list of another version or one that names a file outside the generation with a
warning while the dataset serves as usual. The file is derived state, and deleting it only
loses the prewarm.

`internal-circuit-state` version 2 holds an internal circuit's provenance annotations
after its state. A circuit with a provenance setting writes version 2 and one without
writes version 1, and each reads only its own. A checkpoint of a version this release
does not read is left in place while the circuit is built from the data.

`view-state` version 2 holds the state of a view whose circuit has a fixed-point
operator, after the other operators' states. A view with a property path that the
operator maintains writes version 2, and other views keep writing version 1, so their
checkpoints stay readable by older releases. Each view reads only its own version, and a
checkpoint of a version this release does not read is left in place while the view is
built from the data. `internal-circuit-state` versions 3 and 4 do the same for internal
circuits without and with provenance annotations. The incremental compiler version
changed, so views with property paths are built again once after the upgrade.

The dataset reader capability 6 marks protected managed graphs. Enabling a profile
owner raises `minimumReader` to 6 durably, writes the owner's settings file such as
`evidence.json`, and then writes `managed.json` format 1 as the commit point. That file
holds a `revision` and, for each owner, its `profile`, `settingsDigest`, `graphs`,
`visibility` and `merge` members. Releases that read only up to reader 5 refuse such a
dataset. This build keeps the dataset readable and refuses every write when
`managed.json` has an unknown format, owner or profile, a reserved `visibility` of
`hidden` or `merge` of `exclude`, or an owner settings file whose SHA-256 digest does
not match. An owner settings file of a `format` or `profile` this build does not read
refuses writes the same way. A newer `managed.json` therefore does not refuse the open,
and a backup or clone of a registry with an unknown format or owner is refused. Branches share
the registry without copying the file, and backups, restores and clones carry both
files.

`holds.json` format 1 at a dataset root records the durable source holds of the dataset
and its branches, each with its `id`, `owner`, `created` time and `commits` of
`branchId`, `seq` and, when it differs, the serving `storeId`. The first hold raises
`minimumReader` to 7, so releases that read only up to reader 6 refuse the dataset. A
file of a newer format refuses the open. A file that cannot be read or belongs to
another dataset keeps every past state and refuses hold changes. Backups and clones do
not carry it.

`evidence.json` format 1 and profile 1 now hold all six roles and `policies`, written
canonically with sorted sets and keys. A file that names only `sourceRecords` and
`proposals` still reads, with the other roles empty. An unknown member is refused.

`memory.json` is format 2 while it holds `scratchBranches`, and it stays format 1 without
that member, so datasets that do not use the setting remain readable by older releases.
A release that reads only format 1 refuses a dataset with the setting by naming both
versions, instead of ignoring the member and keeping scratch branches that the operator
meant to expire.

Index generations are written in index format 3. Each column of a permutation block is a
run of independent LZ4 frames of 2,048 rows, and a new file `<perm>.seg` beside each
permutation holds every frame's first key and compressed lengths, under the magic
`SPKSEG01` and the frame size. A point lookup, a short range scan or an index join then
decodes the frames that can hold its rows instead of whole 32,768-row columns. This build
opens format 2 generations as before, and the next compaction rewrites them in format 3
in a new generation directory that replaces the old one in one step. Releases that read
only format 2 refuse a format 3 store with an error that names both versions. At 10.5M
triples a generation grows by 3.0%, from 300.6 MB to 309.7 MB.

A bulk load or a full compaction run with the new global flag `--vocab-codec fsst`
writes index format 4. Its `meta.json` lists the feature `fsst-vocab`, and the new file
`vocab.fsst`, the primary kind `vocab-fsst` version 1 with the magic `SPKFSST\0`, holds
the symbol tables that code the vocabulary's key suffixes. `vocab.dat` shrinks by 29% on
the DBpedia 50M slice and 32% at 10.5M, and warm queries that decode many terms run up to
13% slower, so a new database stays front-coded and in format 3. This build refuses a
generation that lists an unknown feature and names it, and releases that read only
formats 2 and 3 refuse format 4 as newer. A compaction keeps the codec of the generation
it replaces unless the flag asks for another, and then it rebuilds in full. Backup
manifests carry the generation's features as `indexFeatures`, and a restore refuses an
unknown one. `sparkles stats` names the vocabulary codec, and `sparkles check` verifies the
tables.

The Ask evaluation cost ledger is in version 2. Reserve and settle lines carry a time,
and a reconcile line replaces a charge held at its bound. A reconcile line that raises
a charge carries `"raise": true`. A reader refuses a ledger of version 1, which no paid
run wrote, and of any later version. Replay also refuses a negative or non-integer
amount, a request ID that is not a positive integer, a settle without its reserve, a
carryover line anywhere but right after the header, and a reconcile that takes the
ledger past USD 25.

Spill files are registered as the derived format `spill` version 2. A file has the magic
`SPKSPL01`, the version as a u16, the operator kind, the codec, the encryption, the
column count and the file's number, then frames of up to 4,096 rows with a CRC-32 over
each frame's header and columns. An encrypted frame holds its columns sealed with
AES-256-GCM, with the file and frame numbers as the nonce and the frame's kind, place and
header as associated data, and the CRC covers the sealed bytes. Only the process that
wrote a file reads it, and a reader refuses another version, kind, codec or encryption.
Version 1 files are refused, which costs nothing, since spill files never outlive their
process. A spill directory holds a `SPARKLES-SPILL`
marker that names the format and its version.

The ShEx write guard's persisted typing, `validation-typing.bin`, is registered as the
derived format `validation-typing` version 1. The version is the u32 after the `SPXTYP01`
magic. A file of another version is discarded and the typing is rebuilt by a full
validation on the next write. The file's header also covers a digest of the schema copy.

Checked migration state in `.migrate-checked` uses JSON format 1. It records the cleanup
mode and exact old sibling directory so recovery by a later release preserves unrelated
kept copies. Unknown formats, malformed markers and bare legacy mode markers refuse
automatic recovery without changing the migration copies. Bare legacy state requires
inspection of the original directories and manual recovery or restore.

Runtime model secrets are sealed by default in builds with the default `backup` feature.
The first start seals existing plaintext values and generates `secrets.key` if no key
source was configured. Older releases require offline `sparkles secrets unseal --data
DIR` before downgrade. A data directory owned by another user needs `--secrets-key
SOURCE` or `--secrets-key none` until ownership is corrected. The format registry now
records the existing sealed v1 envelope and raw generated key without changing their
bytes or requiring a dataset migration. Unknown envelope versions are refused.
