# C02: Schema discovery and export API

> **Status:** implemented in part
>
> **Phases:** Phase 1 shipped: the `sparkles::schema` library, `GET /$/schema/{ds}` with
> paginated class and predicate listings, `sparkles schema`, and the UI schema browser.
> Most of Phase 2 shipped: the VoID/Turtle export, the SHACL constraints layer with
> `GET /$/schema/{ds}/constraints`, and `detail=subjectClasses`. The GSPO-driven scan for
> small named graphs and the `/$/stats` class counts of Phase 2 are not built. Phase 4
> shipped on 2026-10-02: shapes drafted from the data, as `GET /$/schema/{ds}/shapes`,
> `sparkles schema --draft-shapes`, the MCP tool `draft_shapes` and the schema browser's
> Draft shapes dialog. Phase 3 is not built.
>
> **User docs:** [API: Schema discovery](../API.md#schema-discovery) ·
> [API: Constraints layer](../API.md#constraints-layer) ·
> [API: Subject classes](../API.md#subject-classes) ·
> [API: Drafted shapes](../API.md#drafted-shapes) ·
> [Usage: Constraints next to the counts](../USAGE.md#constraints-next-to-the-counts) ·
> [Usage: Drafting shapes](../USAGE.md#drafting-shapes-from-the-data) ·
> [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This is a clean-room spec. It depends only on the engine and storage correctness fixes
that preceded it. It will use durable commit identity ([CI](CI-commit-identity.md)) once
that lands.

## 1. Summary

The explorer's schema tab builds the class hierarchy, property declarations and instance
counts in the browser, in `loadSchema` (`ui/src/lib/explore.ts`). It runs four
independent SPARQL SELECTs, and that approach has five problems:

- **Silent truncation.** Class and property rows stop at `LIMIT 20000`, class counts at
  `LIMIT 5000`, and the ontology header at `LIMIT 1`. Past those limits the tree is
  incomplete, and nothing tells the user.
- **No single snapshot.** If an update lands between the four requests, they read
  different snapshots.
- **No observed statistics per predicate.** The UI gets no datatypes, languages, object
  kinds or distinct counts per predicate. It also cannot select a graph.
- **No reuse.** The CLI, the dataset page and later adapters such as GraphQL and MCP
  ([C11](C11-mcp-server.md)) cannot call the logic.
- **Inconsistent `/$/stats` numbers.** `/$/stats` lists only the top 100 predicates and
  classes. The meaning of its `classes[].instances` depends on the delta
  (`http.rs::stats`). With an empty delta it counts *distinct subjects* from the build
  statistics; otherwise it counts *`rdf:type` quads*. Its `distinctSubjects` and
  `distinctObjects` come from the base generation only.

**Goals**

1. A library module, `sparkles::schema`, computes one schema report from one `Snapshot`.
   The server exposes it as `GET /$/schema/{ds}` and the CLI as `sparkles schema`.
2. The report has three separate layers:
   - **declared**: what the RDFS/OWL vocabulary in the data asserts.
   - **observed**: exact counts over the selected graph.
   - **constraints**: SHACL shapes (Phase 2). They are validated on request only, never
     enforced.
3. The caller selects the graph (`default`, `union` or a named graph) and whether
   inferences count. The report names the snapshot it was computed at.
4. Pagination is complete and cursor-based. Every list reports its `total`, and nothing
   is truncated without saying so.
5. A timeout and an entry cap bound the work. When either is exceeded, the request fails
   explicitly. It never returns a partial report.
6. The explorer's schema tab uses the endpoint.

**Non-goals.** The schema API does not learn shapes or generate SHACL. It does no OWL
reasoning of its own: inferences come only from the materialized graph. Statistics are
exact, never approximate or sampled. Per-class property profiles are left to Phase 3. No
guarantee is derived from observations.

## 2. User-visible behavior

### 2.1 HTTP

| Method | Path | Result |
|---|---|---|
| GET | `/$/schema/{ds}` | `SchemaSummary`: the snapshot, totals, the ontology header, the hierarchy, and the first page of classes and predicates |
| GET | `/$/schema/{ds}/classes` | `Page<ClassEntry>` |
| GET | `/$/schema/{ds}/predicates` | `Page<PredicateEntry>` |

All parameters are optional:

| Param | Values | Default | Meaning |
|---|---|---|---|
| `graph` | `default`, `union` or a graph IRI. `urn:x-arq:DefaultGraph` and `urn:x-arq:UnionGraph` also work. | `default` | The graph whose triples are counted (§4.1). |
| `declaredGraph` | Same as `graph` | Same as `graph` | The graph read for declarations. Ontologies often live in their own named graph. |
| `reasoning` | `true`/`false` | `true` if the dataset has materialized inferences | Whether `default` and `union` include `urn:x-sparkles:inferred` in the observed counts. |
| `declared` | `asserted`, `all` | `asserted` | `all` also reads declarations from the inferred graph. |
| `limit` | 1–10000 | 1000 | The page size. The summary uses it for both first pages. |
| `cursor` | Opaque | None | The continuation token from `next`. |
| `timeout` | Seconds | The server's query timeout | The time budget for computing the report. |

The response types:

```ts
type SchemaSummary = {
  schemaFormat: 1;                        // version of this JSON shape
  dataset: string;
  snapshot: { version: number; generation: string; commit?: number; datasetId?: string /* once CI exists */;
              computedAt: string /* RFC 3339 */ };
  selection: { graph: "default" | "union" | string; declaredGraph: string;
               reasoning: boolean; declared: "asserted" | "all" };
  totals: { triples: number; classes: number; predicates: number;
            anonymousTypeTargets: number;     // rdf:type objects that are blank nodes
            anonymousClassExpressions: number };// blank-node objects of rdfs:subClassOf etc.
  ontology: { iri: string; labels: Lit[]; versionInfo: Lit[]; comments: Lit[] }[];  // all owl:Ontology nodes
  hierarchy: { roots: string[]; cycles: string[][] };
  classes: Page<ClassEntry>;
  predicates: Page<PredicateEntry>;
};
type Page<T> = { items: T[]; total: number; next: string | null };
type Lit = { value: string; lang?: string };

type ClassEntry = {
  iri: string;
  builtin: boolean;                        // rdf:, rdfs:, owl:, xsd:, sh: namespace
  observed: { instances: number };         // distinct subjects with rdf:type C in the selection
  declared: {
    types: string[];                       // subset of rdfs:Class, owl:Class, rdfs:Datatype
    superClasses: string[];                // asserted IRI objects of rdfs:subClassOf, excluding C itself
    equivalentClasses: string[]; disjointWith: string[];
    labels: Lit[]; comments: Lit[];
  };
};
type PredicateEntry = {
  iri: string;
  builtin: boolean;
  observed: {
    triples: number; distinctSubjects: number; distinctObjects: number;
    maxPerSubject: number;                 // OBSERVED maximum, not a contract
    subjectsWithMultiple: number;          // subjects with ≥ 2 distinct objects
    objects: {
      iri?: KindCount; blank?: KindCount; tripleTerm?: KindCount;
      literals: { datatype: string; triples: number; distinct: number;
                  languages?: { lang: string; direction?: "ltr" | "rtl"; triples: number }[] }[];
    };
  };
  declared: {
    types: string[];                       // rdf:Property, owl:ObjectProperty, owl:DatatypeProperty,
                                           // owl:AnnotationProperty, owl:FunctionalProperty, ...
    domains: string[]; ranges: string[]; superProperties: string[]; inverseOf: string[];
    labels: Lit[]; comments: Lit[];
  };
};
type KindCount = { triples: number; distinct: number };
```

The lists contain only classes and predicates that are declared or used. A predicate is
listed when it occurs in the selection or is declared in the declared selection. A
predicate that is declared but never used has `observed.triples = 0`. A class is listed
when it is:

- an IRI object of `rdf:type` in the selection;
- a subject declared with `rdf:type rdfs:Class`, `owl:Class` or `rdfs:Datatype`;
- an IRI subject or object of `rdfs:subClassOf`, `owl:equivalentClass` or
  `owl:disjointWith` in the declared selection.

Errors use the standard `{ "error", "detail"? }` body:

| Status | When |
|---|---|
| 400 | A parameter value is bad, or the cursor is malformed. |
| 404 | The dataset is unknown, or `graph`/`declaredGraph` names a graph with no quads. |
| 408 | The report did not finish within `timeout`. The message names the phase, for example `"schema discovery exceeded 60s while scanning predicates (412/9031); narrow graph= or raise timeout="`. |
| 409 | The cursor belongs to an older snapshot. The message is `"snapshot changed since the first page (version 17 → 18); restart from the first page"`. |
| 413 | There are more than `--schema-max-entries` classes or predicates. The message is, for example, `"dataset has 1204331 classes (limit 1000000)"`. |

### 2.2 CLI

```
sparkles schema --loc DB [--graph default|union|IRI] [--declared-graph G]
                [--no-inferences] [--declared asserted|all] [--format json|text]
```

The command prints the complete report without pagination. With `json` the output is a
`SchemaSummary` that holds every item and has `next: null`. With `text` it prints one line
per class, with `instances` and superclasses, and one line per predicate, with `triples`,
kinds and `max/subject`. It exits with status 2 on a budget error.

### 2.3 Rust API

```rust
// crates/sparkles/src/schema.rs
pub struct SchemaOptions {
    pub graph: GraphSelection, pub declared_graph: Option<GraphSelection>,
    pub include_inferred: bool, pub declared_from_inferred: bool,
    pub deadline: Option<Instant>, pub cancel: Option<Arc<AtomicBool>>,
    pub max_entries: usize,
}
pub enum GraphSelection { Default, Union, Named(NamedNode) }
pub fn discover(snap: &Arc<Snapshot>, opts: &SchemaOptions) -> Result<SchemaReport>;
// SchemaReport { snapshot, totals, ontology, hierarchy, classes: Vec<ClassEntry>,
//                predicates: Vec<PredicateEntry> } — sorted by IRI, serde camelCase
```

The caller passes in the name of the inferred graph. The library does not know
`urn:x-sparkles:inferred`; the server and the CLI supply it.

### 2.4 UI

The explorer's **Schema** tab calls `api.schema(ds, {graph, reasoning})`. The helper
follows `next` until it is `null`, keeps the first page's `snapshot.version`, and
restarts if it gets a 409. `reduceSupers` and the root and cycle logic move to the
server. The UI still computes the transitive reduction for drawing.

- **Header.** It reads "as of version N · generation gen-0003 · computed 12:04" and holds
  a graph selector and an "include inferences" toggle. The selector offers the default
  graph, the union and each named graph from `/$/stats`.
- **Class tree.** Each node shows its instance count. Classes that are used but not
  declared get an `undeclared` tag. Classes in `hierarchy.cycles` get a `cycle` icon,
  whose tooltip lists the other members.
- **Class detail.** It lists the asserted superclasses, the equivalent and disjoint
  classes, and the properties whose `declared.domains` or `declared.ranges` include the
  class.
- **Property list.** New columns show the triples, the distinct subjects and objects, and
  a stacked bar of object kinds and datatypes with language chips. When
  `maxPerSubject == 1`, a chip reads **"≤1 per subject (observed)"**. It is styled as a
  measurement and has the tooltip "No constraint enforces this; a future write may add a
  second value." A property declared `owl:FunctionalProperty` gets a separate
  **declared** chip. The UI never merges the two.

## 3. Standards basis

- RDF 1.1 Concepts and RDF 1.2 Concepts define the term kinds (IRI, blank node, literal,
  triple term), literal datatypes and language tags, `rdf:langString`, and
  `rdf:dirLangString` with its base direction.
- RDF Schema 1.1 defines `rdfs:Class`, `rdfs:subClassOf`, `rdfs:domain`, `rdfs:range`,
  `rdfs:subPropertyOf`, `rdfs:label`, `rdfs:comment` and `rdfs:Datatype`.
- OWL 2 Mapping to RDF Graphs defines `owl:Class`, the `owl:*Property` types,
  `owl:equivalentClass`, `owl:disjointWith`, `owl:inverseOf`, `owl:Ontology` and
  `owl:versionInfo`.
- The default graph, the union graph and the special IRIs `urn:x-arq:DefaultGraph` and
  `urn:x-arq:UnionGraph` come from the SPARQL 1.1 Protocol and Jena's conventions.
  `/{ds}/shacl` already accepts these IRIs.
- VoID, a W3C Interest Group Note, supplies the vocabulary for the Phase 2 Turtle
  export: `void:triples`, `void:classPartition`, `void:propertyPartition`, `void:class`,
  `void:property`, `void:entities`, `void:distinctSubjects` and `void:distinctObjects`.
- SHACL, a W3C Recommendation, supplies the vocabulary for the Phase 2 constraints layer:
  `sh:targetClass`, `sh:path`, `sh:minCount`, `sh:maxCount`, `sh:datatype`, `sh:class`
  and `sh:nodeKind`.

## 4. Semantics

### 4.1 Graph selection

The selection is a set `G` of graph ids. The schema counts **triples**, not quads, so a
triple that occurs in several graphs of `G` counts once. SPARQL treats the union graph
the same way.

| `graph` | `reasoning=true` | `reasoning=false` |
|---|---|---|
| `default` | {default graph, inferred graph}. With `--union-default-graph`, every graph. | {default graph}. With `--union-default-graph`, every graph except the inferred graph. |
| `union` | Every graph. | Every graph except the inferred graph. |
| IRI | {that graph}, or 404 if it has no quads. | Same. |

`declaredGraph` resolves the same way. `declared=asserted` always removes the inferred
graph from the declared selection, and `declared=all` keeps it. The default is
`asserted` because materialized RDFS/OWL adds the transitive closure of
`rdfs:subClassOf` and puts `rdfs:Resource` and `owl:Thing` everywhere. The explorer reads
declarations with `reasoning=false` today for the same reason.

### 4.2 Observed statistics (exact at the snapshot)

For predicate `p`, let `T_p` be the set of distinct triples `(s, p, o)` with a quad in
`G`:

- `triples = |T_p|`;
- `distinctSubjects = |{s}|`;
- `distinctObjects = |{o}|`;
- `maxPerSubject = max over s of |{o : (s,p,o) ∈ T_p}|`;
- `subjectsWithMultiple` = the number of subjects where that count is ≥ 2.

`objects` partitions `T_p` by the kind of `o`:

- `iri`, `blank` and `tripleTerm`;
- literals, grouped by datatype IRI. Simple literals have the datatype `xsd:string`.
  Language-tagged literals are `rdf:langString`, or `rdf:dirLangString` with a
  `direction`. Each group reports its `languages`, lowercased as stored.

Each group reports its `triples` and its `distinct` objects. Distinct objects are
**terms**, not values. `"1"^^xsd:integer` and `"01"^^xsd:integer` are two distinct
objects if both are stored. Inline ids are canonical, so the two can only coexist when
one of them is a vocabulary literal.

For class `C`, `instances` is the number of distinct `s` with `(s, rdf:type, C)` in `G`.
It counts asserted types, plus inferred types when `reasoning=true`. The library does not
roll subclass instances up into their superclasses. With `reasoning=true`, materialized
RDFS does that.

`maxPerSubject` measures one snapshot. The API documentation, the JSON field comments
and the UI must never call it "functional", "single-valued" or "scalar".

### 4.3 Declarations

Declarations are read from the declared selection, and only IRI objects are listed.
Blank-node class expressions such as `owl:Restriction` are counted in
`totals.anonymousClassExpressions` but not expanded. Phase 3 may render them. An entry's
labels and comments are all of its `rdfs:label` and `rdfs:comment` literals, with their
languages. The client picks which one to show.

### 4.4 Hierarchy

The hierarchy is computed over the declared `rdfs:subClassOf` edges between IRIs in the
classes list, with self-loops removed.

- `cycles` lists every strongly connected component with more than one member. Members
  are sorted by IRI, and components by their first member.
- `roots` lists every class with no superclass outside its own component. A component
  with more than one member contributes only its smallest IRI. Roots are ordered by
  descending subclass count, then by IRI.

`ex:D rdfs:subClassOf ex:D` is not a cycle, so `ex:D` can still be a root.

### 4.5 Snapshot, pagination, caching

- **One snapshot.** One `discover` call uses one `Arc<Snapshot>`. `snapshot.version` is
  `Snapshot::version`, which changes on every commit and compaction and resets at
  restart. It identifies a report only while the server runs. Once CI exists
  ([CI](CI-commit-identity.md)), the report adds `snapshot.commit` (`Snapshot::commit`)
  and `snapshot.datasetId`, and the response carries CI's `Sparkles-Commit` header. The
  cursor then binds to `commit` instead of `version`, so compaction no longer
  invalidates cursors.
- **Order.** Lists are sorted by IRI, in byte order of the UTF-8 string. Page `n+1`
  starts strictly after the last IRI of page `n`.
- **Cursor.** The cursor is base64url JSON:
  `{"v": version, "h": hash(selection params), "a": lastIri}`. A cursor whose `h` does
  not match the request gets 400. A cursor whose `v` differs from the current snapshot
  gets 409, unless the server still holds the cached report for `v`. In that case the
  page is served from the cached report. The server keeps the last report per dataset,
  as a single entry keyed by version and selection.
- **Summary.** `/$/schema/{ds}` always computes the report or reuses the cached one. The
  page endpoints do the same when called without a cursor.

### 4.6 Budgets and configuration

- **Time.** `timeout` defaults to the server's query timeout, 60 s. `discover` checks the
  deadline and the cancel flag after every scanned block or delta run. An exceeded
  deadline gives 408 and no partial report.
- **Entries.** `sparkles serve --schema-max-entries N` caps the classes and the
  predicates separately. The default is 1,000,000. An exceeded cap gives 413.
- **Read-only servers.** The endpoint only reads, so servers started with `--read-only`
  allow it.
- **Cache memory.** Each dataset caches one report. A newer snapshot's report replaces
  it.

## 5. Design sketch

The work lives in a new module, `crates/sparkles/src/schema.rs` (`pub mod schema` in
`lib.rs`). It uses only these `Snapshot` APIs: `scan`, `distinct_first`, `count`, `key`,
`term`, `lookup_iri` and `graph_ids`.

1. **Resolve the selection.** Build a
   `GraphFilter::{All, AllExcept(u64), Set(SmallVec<u64>)}` from `graph_ids()`. In PSO
   and POS the graph id is key column 3 (`Perm::order`: `[P,S,O,G]` / `[P,O,S,G]`).
2. **Predicate pass A (PSO).** For each `p` from `distinct_first(Perm::Pso)`, run
   `scan(Perm::Pso, &[p])` and drop rows whose `g` is not in the filter. `g` is the last
   column, so rows with equal `(s,o)` are adjacent. Duplicates across graphs collapse by
   comparing each row with the previous one. The pass counts `triples` and
   `distinctSubjects`, which are the changes of `s`. The run length of distinct `o` per
   `s` gives `maxPerSubject` and `subjectsWithMultiple`. Memory is O(1) per predicate.
3. **Predicate pass B (POS).** For the same `p`, count the distinct `o` runs and
   classify each distinct `o` once:
   - Inline tags map directly: `Int`→`xsd:integer`, `Decimal`→`xsd:decimal`,
     `Double`→`xsd:double`, `Bool`→`xsd:boolean`, `DateTime`→`xsd:dateTime`,
     `Date`→`xsd:date`, `BNode`→blank.
   - `Vocab` and `Delta` ids are classified from `snap.key(id)`. The first byte is `<`
     for an IRI, `"` for a literal and `(` for a triple term. For a literal, the suffix
     after the last `0xFF` (`KEY_SEP`) is empty for `xsd:string`, `@lang[--dir]` for a
     language-tagged literal, or `^datatype`.

   The triple count per kind is the length of each `o` run after graph dedupe.
4. **Class pass (POS, prefix `[rdf:type]`).** For each distinct class `o`, count the
   distinct `s` that pass the graph filter. The rows arrive sorted as `(o, s, g)`, so the
   pass streams. Blank-node objects count toward `anonymousTypeTargets`.
5. **Declarations.** Under the declared filter, run `scan(Perm::Pso, &[q])` for each
   structural predicate `q` (`rdfs:subClassOf`, `owl:equivalentClass`,
   `owl:disjointWith`, `rdfs:domain`, `rdfs:range`, `rdfs:subPropertyOf`,
   `owl:inverseOf`), and `scan(Perm::Pos, &[rdf:type, T])` for each meta-class `T`.
   Labels and comments are looked up per entry with `scan(Perm::Spo, &[s, rdfs:label])`.
   A scan of all `rdfs:label` triples would read every instance label in the dataset.
6. **Assemble.** Build `BTreeMap<String, ClassEntry>` and
   `BTreeMap<String, PredicateEntry>` in IRI order. Compute the hierarchy with Tarjan's
   SCC algorithm over the declared edges, and check `max_entries` while inserting.

**Cost.** The report takes two full passes over the selected predicates (PSO and POS),
one pass over `rdf:type`, and a few small declaration scans. On the 10.5M benchmark
dataset that means two sequential reads of about 10.5M rows each, well under the default
timeout. Phase 1 should measure it and record the time in the PR.

**Why planner statistics are not used for counts.** `builder::Stats` (`stats.json` per
generation) is exact only for the base generation. It counts quads over all graphs,
including the inferred graph, and it ignores the delta. `Snapshot::predicate_stat` adds
delta inserts, ignores deletes and sums distinct counts; its documentation calls them
"estimates only". The build-time `classes` are distinct subjects over all graphs. None
of these gives graph-scoped triple counts or stays exact after updates, so the schema
module scans. It uses planner statistics only to order the predicate loop by estimated
size, so that a timeout reports useful progress. Exact counts are affordable, and they
stay correct under deltas because `scan` already merges base ⊕ delta.

**Server.** A new set of `schema` handlers in `crates/sparkles-server/src/http.rs` does
the following:

- Parse the parameters.
- Resolve the graph IRI, and return 404 if `count(Perm::Gspo, &[g]) == 0`.
- Run `discover` in `blocking`, with the deadline from `timeout_param`.
- Keep the report cache in `Dataset` as
  `schema_cache: Mutex<Option<(SchemaKey, Arc<SchemaReport>)>>`.
- Paginate with the cursor rules of §4.5.

The report JSON carries `schemaFormat: 1`. Nothing is persisted, so crash safety is not
a concern.

**CLI.** `Cmd::Schema` in `main.rs` calls the same `discover`.

**UI.** `api.ts` gets `schema()` and its types. `explore.ts::loadSchema` becomes a thin
adapter from `SchemaSummary` pages to the existing `Schema` type, which gains `observed`
fields.

## 6. Phasing

**Phase 1** is the MVP, about one day of work:

- `sparkles::schema::discover` with passes 1–6. These cover classes, predicates, object
  kinds, datatypes, languages, `maxPerSubject`, declarations, labels, the ontology header
  and the hierarchy.
- The three HTTP endpoints, with cursor pagination, the 404/408/409/413 errors and the
  one-entry cache.
- `sparkles schema --format json|text`.
- The UI schema tab, switched to the endpoint, with the snapshot header and the
  observed and declared chips.
- Tests for §7 A–G.

**Phase 2:**

- The `constraints` layer. `shapes=<graph IRI>` reads shapes with
  `sparkles_shacl::Shapes::from_store`. For each target class, the layer lists the
  property shapes that have a predicate path, with their `sh:minCount`, `sh:maxCount`,
  `sh:datatype`, `sh:class` and `sh:nodeKind`. They are labelled
  `"enforcement": "validated-on-request"` until write-time validation
  ([C10](C10-write-time-validation.md)) exists.
- `detail=subjectClasses`. For each predicate it lists the classes of its subjects, with
  triple counts. It is a merge join of the `PSO[p]` subjects with `PSO[rdf:type]`, both
  sorted by `s`.
- A Turtle export (`Accept: text/turtle`). Observed statistics become VoID partitions in
  a `urn:x-sparkles:schema:<ds>:<version>` node, and declarations are emitted as the
  asserted triples.
- A GSPO-driven scan for small named graphs.
- `/$/stats` class counts computed with pass 4. This removes their mixed "distinct
  subjects vs quads" meaning.

**Phase 3** adds per-class property profiles, which list the predicates used by
instances of C with counts, and renders anonymous class expressions. It also adds a
schema diff between two snapshots, which needs point-in-time queries
([F06](F06-snapshots-and-point-in-time.md)), and incremental maintenance from the delta.

**Phase 4** drafts SHACL shapes and ShEx schemas from the data, with a support threshold
per constraint. It was designed after Phases 1 and 2 had shipped, and §11 describes it.

## 7. Acceptance examples

All requests go to dataset `t`, and `ex:` is `http://ex.org/`. The JSON shown is an
excerpt.

**A. Mixed datatypes and kinds** (default graph):

```turtle
ex:a ex:val 1, "1.5"^^xsd:decimal .
ex:b ex:val "x", "x"@en .
ex:c ex:val ex:d, _:n, <<( ex:a ex:val 1 )>> .
```

`GET /$/schema/t/predicates` returns this `ex:val` item:

```json
{"iri":"http://ex.org/val","observed":{"triples":7,"distinctSubjects":3,"distinctObjects":7,
 "maxPerSubject":3,"subjectsWithMultiple":3,
 "objects":{"iri":{"triples":1,"distinct":1},"blank":{"triples":1,"distinct":1},
  "tripleTerm":{"triples":1,"distinct":1},
  "literals":[
   {"datatype":"http://www.w3.org/1999/02/22-rdf-syntax-ns#langString","triples":1,"distinct":1,"languages":[{"lang":"en","triples":1}]},
   {"datatype":"http://www.w3.org/2001/XMLSchema#decimal","triples":1,"distinct":1},
   {"datatype":"http://www.w3.org/2001/XMLSchema#integer","triples":1,"distinct":1},
   {"datatype":"http://www.w3.org/2001/XMLSchema#string","triples":1,"distinct":1}]}}}
```

Literal groups are sorted by datatype IRI.

**B. Cycles:**

```turtle
ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:A . ex:C rdfs:subClassOf ex:A .
ex:D rdfs:subClassOf ex:D . ex:x a ex:C .
```

`GET /$/schema/t` → `hierarchy = {"roots":["http://ex.org/A","http://ex.org/D"],"cycles":[["http://ex.org/A","http://ex.org/B"]]}`.
`A` comes before `D` because `A` has a subclass, `C`, and `D` has none.
`ex:D.declared.superClasses = []`, `ex:C.observed.instances = 1`, and
`totals.classes = 4`.

**C. Undeclared classes:** `ex:x a ex:Person . ex:Employee rdfs:subClassOf ex:Person .`
gives `ex:Person` with `instances 1`, `declared.types []` and `superClasses []`, and
`ex:Employee` with `instances 0` and `superClasses ["http://ex.org/Person"]`. The UI tags
both `undeclared`.

**D. Delta changes:** Bulk-load `ex:x a ex:P . ex:y a ex:P .` into the base generation,
then run:

```sparql
DELETE DATA { ex:x a ex:P } ;
INSERT DATA { ex:z a ex:P ; ex:age "x" . GRAPH ex:g1 { ex:y a ex:P } }
```

- `graph=default`: `ex:P.instances = 2` (y, z), `ex:age.objects.literals = [xsd:string ×1]`,
  and `snapshot.version` is greater than before the update.
- `graph=union`: `ex:P.instances = 2`. `ex:y` is typed in two graphs but counted once.
- `graph=http://ex.org/g1`: `ex:P.instances = 1`.

Repeat the checks after `POST /$/compact/t`. The numbers must not change.

**E. Pagination:** 2,500 classes `ex:C0000…ex:C2499`, with one instance each.

- `GET /$/schema/t/classes?limit=1000` returns 1000 items, `total 2500`, and a string
  `next`.
- Following `next` gives 1000 items, then 500 items with `next: null`. The pages are
  disjoint, and together they hold all 2,500 classes in IRI order.
- Run `INSERT DATA { ex:q a ex:C9999 }` after the first page, then follow the old cursor.
  The server answers 200 while it still holds the cached report for the old version, and
  409 once the new version's report has replaced it.

**F. Errors:**

- `?graph=http://ex.org/missing` → 404.
- `?limit=0` → 400.
- `?timeout=0.000001` on a non-trivial dataset → 408 with `"exceeded"` in the error.
- `--schema-max-entries 10` with 11 classes → 413.

**G. Observation is not a contract:** with `ex:name a owl:FunctionalProperty .
ex:a ex:name "A" . ex:b ex:name "B1", "B2" .`, `declared.types` includes
`owl:FunctionalProperty` and `observed.maxPerSubject = 2`. After `ex:b`'s second value is
removed, `maxPerSubject = 1`, and no field anywhere changes its name or meaning.

**H. Reasoning toggle:** after `POST /$/reason/t` (rdfs) with
`ex:C rdfs:subClassOf ex:B . ex:x a ex:C .`:

- the default request gives `ex:B.observed.instances = 1`;
- `reasoning=false` gives `ex:B.instances = 0`;
- in both cases `ex:C.declared.superClasses = ["http://ex.org/B"]`, and `rdfs:Resource`
  is not a declared superclass (`declared=asserted`).

## 8. Rejected alternatives

- **Keep the logic in the UI with larger LIMITs.** It would still truncate, still read
  several snapshots, and still not be reusable.
- **Implement with internal SPARQL (`GROUP BY ?p (DATATYPE(?o))`).** This is simple, and
  `sparql::query(snap, …)` keeps it snapshot-consistent. But it needs several GROUP BY
  passes, each with its own hash grouping. `DATATYPE()` materializes every term, and the
  graph dedupe needs `DISTINCT` over triples. The key-level scan is one ordered pass and
  needs no grouping memory.
- **Report planner statistics.** They are not exact after updates and not graph-scoped
  (§5).
- **Count quads instead of triples.** Quad counts are easier to reconcile with
  `/$/stats`. But a triple asserted in two graphs would count twice in `union`, which
  disagrees with SPARQL over the same selection.
- **Offset pagination or top-N by count.** Offsets drift when the report is recomputed,
  and ordering by count makes the cursor depend on changing numbers. IRI order with a
  version-bound cursor is stable and detects change.
- **Partial reports on budget exhaustion.** A naive client would take them as complete.
  Failing explicitly serves the "no silent truncation" goal.
- **Approximate distinct counts (HyperLogLog).** They are unnecessary at the current
  scale. They could come back later as an explicit `approx=true` mode with error bounds.

## 9. Open questions

1. Should `graph` default to `union` for the explorer? The spec keeps `default`, which
   is consistent with SPARQL and `/{ds}/shacl`.
2. Should the server omit `builtin` entries by default, such as `owl:Class` as an
   `rdf:type` target? The spec includes them with a flag, and the UI filters them as it
   does today.
3. Should the dataset page's "Top predicates/classes" switch to this API? It would then
   need the scan, and `/$/stats` is cheaper. Phase 1 leaves `/$/stats` alone, and Phase 2
   fixes its class-count semantics.
4. Is 1,000,000 the right entry cap? Datasets that mint a class per entity would exceed
   it.
5. `version` is not durable. Cursors do not survive a restart, and 409 is the correct
   answer after one. With CI, cursors bind to `commit` and survive both restarts and
   compaction, as long as the cached report or an identical recomputation is available.

The constraints layer of Phase 2 raised four more questions. They were settled when it
was built, on 2026-10-02.

6. *Which shapes does the layer read when the request names none?* Phase 2 named only
   `shapes=<graph IRI>`. Write-time validation ([C10](C10-write-time-validation.md)) now
   gives each dataset a set of shapes it actually uses, read from the shapes graphs its
   configuration designates and from its shapes file. The summary includes those shapes
   by default, and `shapes=` names the sources explicitly: `guard`, `default`, graph
   IRIs, or `none`. Requests that name no shapes and target a dataset without SHACL
   validation get no layer, so their JSON is unchanged.
7. *What does `enforcement` say now that C10 exists?* Each property shape carries its own
   label. A shape of a guard in `reject` mode whose severity is at or above the guard's
   threshold is `reject-on-write`. A shape of a guard in `warn` mode, or one below the
   threshold, is `warn-on-write`. Shapes read from graphs named by the request are
   `validated-on-request`. The layer never derives a constraint from the counts, and the
   observed layer never reads the shapes.
8. *Which property shapes belong to a class?* The shapes that target it with
   `sh:targetClass` or an implicit class target, and the shapes reached from them through
   `sh:node` and `sh:and`, which apply to the same focus nodes. `sh:or`, `sh:xone` and
   `sh:not` impose nothing on their own, so their shapes are left out, and so are
   deactivated shapes. Paths other than a single predicate are counted, not listed, and
   so are shapes whose targets are not classes.
9. *Is the layer cached with the report?* No. It is built from the shapes for every
   summary request, which costs a parse of the shapes graphs or nothing for the guard,
   whose shapes are already parsed. The report's cache and its cursors are unchanged.
   `detail=subjectClasses` changes the counts, so it is part of the selection hash.

## 10. Sources

- The Sparkles repository, the only implementation source:
  - [CI-commit-identity.md](CI-commit-identity.md), a sibling clean-room spec:
    `Snapshot::commit`, the dataset id and the `Sparkles-Commit` header.
  - `ui/src/lib/explore.ts`: the current queries, LIMITs, `reduceSupers` and cycle
    promotion.
  - `ui/src/routes/explore/+page.svelte`: the schema tab.
  - `crates/sparkles/src/store.rs`: `Snapshot::{scan, count, estimate, distinct_first,
    predicate_stat}` and the delta semantics.
  - `crates/sparkles/src/builder.rs`: `Stats`, `PredicateStat` and the class collector.
  - `crates/sparkles/src/index.rs`: the `Perm` key orders.
  - `crates/sparkles/src/id.rs`: tags and the key encoding.
  - `crates/sparkles-server/src/http.rs`: the `stats` handler, parameter conventions and
    the error format.
  - `crates/sparkles-shacl/src/lib.rs` and `shapes.rs`: `Shapes::from_store` and
    `Constraint`.
  - `docs/API.md`, `docs/AUDIT.md`, `README.md`, and the project's clean-room feature
    order (internal planning notes).
- W3C RDF 1.1 and RDF 1.2 Concepts, RDF Schema 1.1, OWL 2 Mapping to RDF Graphs, SHACL
  and the SPARQL 1.1 Protocol, for the vocabulary and term kinds. These are cited from
  working knowledge and were not re-fetched for this spec.
- The W3C VoID Interest Group Note (https://www.w3.org/TR/void/), for the partition
  vocabulary of the Phase 2 Turtle export. It is cited from working knowledge.
- The Apache Jena convention of `urn:x-arq:DefaultGraph` and `urn:x-arq:UnionGraph`, as
  Sparkles' SHACL endpoint already implements it.
- Fluree was not consulted. No Fluree code, documentation or product pages were read.

## 11. Phase 4: shapes drafted from the data

This section was written on 2026-10-02, after Phases 1 and 2 had shipped. It turns part
of the §1 non-goal around: the report itself still states no constraints, but a separate
request can now draft them.

### 11.1 Summary

A dataset that already holds data needs shapes before write-time validation
([C10](C10-write-time-validation.md)) can guard it. Writing them by hand means reading the
schema report, guessing which observations are rules, and then finding out by trial
which guesses the data breaks. The report has the counts per predicate, but shapes are
per class, and the report does not say which predicates the instances of a class use.

Phase 4 computes per-class property profiles, the first item of Phase 3, and drafts one
SHACL node shape and one ShEx shape per class from them. Every constraint carries a
support threshold. A constraint is drafted only when the share of the instances it
applies to that satisfy it reaches the threshold, and the draft reports how many
instances each constraint would exclude.

**Goals**

1. A draft at support 1.0 validates the current data with no results, in SHACL and in
   ShEx. The tests check this on data with several types per node, subclasses, mixed
   datatypes, ill-formed literals, language tags and closed shapes.
2. Below 1.0, each drafted constraint states how many instances it applies to, how many
   satisfy it and how many it excludes. The best candidate that missed the threshold is
   listed too, with the same numbers.
3. The draft reads the same selection as the report (§4.1) and the caller's graph view
   ([C12](C12-graph-access-control.md) §5.4).
4. It is available as `GET /$/schema/{ds}/shapes`, `sparkles schema --draft-shapes`, an
   MCP tool, and an action of the UI's schema browser that opens the draft in the shapes
   editor or installs it as a write-time guard in `warn` mode.

**Non-goals.** The drafter does not sample, learn weights or guess beyond the counts.
It drafts no `sh:or` of datatypes, no inverse or sequence paths, no `sh:pattern`, length
or range constraints, and no `sh:node` references between class shapes. A draft is
text for a person to review. Nothing is enforced until someone installs it.

### 11.2 Basis

- **W3C SHACL** defines the drafted vocabulary: `sh:targetClass`, `sh:property`,
  `sh:path`, `sh:minCount`, `sh:maxCount`, `sh:nodeKind`, `sh:datatype`, `sh:class`,
  `sh:in`, `sh:languageIn`, `sh:uniqueLang`, `sh:closed` and `sh:ignoredProperties`. Two
  of its rules matter for exactness. The targets of `sh:targetClass C` are the SHACL
  instances of `C`, the nodes with an `rdf:type` whose class is `C` or reaches `C` over
  `rdfs:subClassOf` in the data graph. `sh:class` tests membership the same way. Value
  constraints such as `sh:datatype` fail a focus node when any one of its values fails,
  and `sh:datatype` also fails ill-formed literals.
- **ShEx 2.1** defines triple constraints with cardinalities, node constraints (node
  kinds, datatypes, value sets with language tags), `EXTRA`, `CLOSED` and query shape
  maps. ShEx applies no RDFS, so `rdf:type` arcs are matched as they are.
- **Shape extraction from knowledge graphs** gives the approach. QSE (Rabbani,
  Lissandrini and Hose, "Extraction of Validating Shapes from Very Large Knowledge
  Graphs", PVLDB 2023) builds an entity-to-types map, counts property and object-type
  pairs per class in one pass, and prunes constraints by support and confidence. sheXer
  (Fernández-Álvarez, Labra Gayo and Gayo-Avello, "Automatic extraction of shapes using
  sheXer", Knowledge-Based Systems 2022) keeps a constraint when the share of instances
  that comply with it reaches a trust threshold, and reports exact cardinalities or
  `+`/`*`. ABSTAT (Spahiu, Porrini, Palmonari, Rula and Maurino, 2016) summarizes data as
  minimal type patterns with cardinality statistics. These are cited from working
  knowledge. No code of theirs was used.

### 11.3 Semantics

**Instances.** For a class `C` in the selection, the instances are its SHACL instances
there: the subjects with an `rdf:type T` where `T` is `C` or reaches `C` over
`rdfs:subClassOf` in the same selection. `I(C)` is their number. This differs from the
report's `observed.instances`, which counts direct types only (§4.2). Classes in the
`rdf:`, `rdfs:`, `owl:`, `xsd:` and `sh:` namespaces get no shape unless the request
names them.

**Profiles.** For each class `C` and predicate `p` other than `rdf:type`, the profile
counts, over the instances of `C`:

- `W(C,p)`, the instances with at least one value of `p`;
- how many instances have `n` distinct values, for each `n`;
- per instance, whether all its values are IRIs, blank nodes or literals, whether they
  are all well-formed literals of one datatype, which classes all of them belong to, and
  their language tags and distinct values (capped, see §11.5).

**Applicability and support.** The support `s` is a number in (0, 1]. `sh:minCount`
applies to all `I(C)` instances. Every other constraint applies to the `W(C,p)`
instances that have a value, because an instance without one satisfies it whatever it
says. Counting those instances would let any constraint on a rare property pass. A
constraint is drafted when `satisfied ≥ s × applicable`, and `excluded` is
`applicable − satisfied`.

| Constraint | Candidate | Drafted when |
|---|---|---|
| `sh:minCount k` | the largest `k ≥ 1` such that enough instances have `k` or more values | always, if such a `k` exists |
| `sh:maxCount k` | the smallest `k` such that enough instances with values have at most `k` | `k` ≤ `maxCount` (default 1) |
| `sh:nodeKind` | the most specific of the six node kinds that covers all the values of enough instances | always, except `sh:Literal` next to a drafted `sh:datatype` |
| `sh:datatype D` | the datatype with the most instances whose values are all well-formed literals of `D` | always |
| `sh:class K` | the class, outside the built-in namespaces, with the most instances whose values are all instances of `K`; ties go to the class with fewer instances | always |
| `sh:in (v…)` | values in descending order of use; the shortest prefix that covers all the values of enough instances | at most `maxIn` values (default 10), each used by at least two instances; IRIs and literals only; never for `xsd:boolean` |
| `sh:languageIn` | language tags chosen the same way; an untagged value never satisfies it | some value has a tag |
| `sh:uniqueLang true` | instances with no two values in one language | some instance has two tagged values |
| `sh:closed true` | a property shape for every predicate an instance uses, with `sh:ignoredProperties (rdf:type)` | `closed=true` |

At support 1.0 every drafted constraint holds for every instance it applies to, so the
data conforms. The tests also check the converse below 1.0. For each drafted constraint,
`excluded` equals the number of distinct focus nodes that a validation of the draft
reports for that constraint's path and component.

**ShEx.** The ShEx draft carries the same decisions:

| SHACL | ShEx |
|---|---|
| node shape with `sh:targetClass C` | shape `<…CShape>`, and `{FOCUS rdf:type <C>}@<…CShape>` in the shape map |
| `sh:minCount`, `sh:maxCount` | the cardinality `{min,max}`, with `*` for no maximum |
| `sh:nodeKind` | `IRI`, `BNODE`, `LITERAL`, `NONLITERAL`, or `(IRI OR LITERAL)` and `(BNODE OR LITERAL)` |
| `sh:datatype` | the datatype |
| `sh:in`, `sh:languageIn` | a value set, `[v…]` or `[@en @de]` |
| `sh:class K` | `EXTRA rdf:type { rdf:type [K S…] + }`, where `S…` are the subclasses of `K` in the selection |
| `sh:closed` | `CLOSED`, with `rdf:type . *` |
| `sh:uniqueLang` | none (left out) |

A query shape map selects the direct instances of each class, which are a subset of the
SHACL targets. ShEx applies no RDFS, so the class test lists the subclasses explicitly.
The ShEx draft therefore conforms at support 1.0 as well.

**Graphs and reasoning.** The selection is chosen as in §4.1, except that `reasoning`
defaults to `false`. That matches the default `includeInferences: false` of write-time
validation, so a draft installed as a guard validates the graphs it was drafted from.

### 11.4 User-visible behavior

`GET /$/schema/{ds}/shapes` takes these parameters, all optional:

| Param | Default | Meaning |
|---|---|---|
| `graph` | `default` | As in §2.1. |
| `reasoning` | `false` | Whether `default` and `union` include the inferred graph. |
| `support` | `1` | The threshold, a number in (0, 1]. |
| `class` | every class with instances outside the built-in namespaces | Draft only these classes (repeatable, IRIs). |
| `minInstances` | `1` | Skip classes with fewer instances. |
| `maxIn` | `10` | The largest `sh:in` list. `0` drafts none. |
| `maxCount` | `1` | The largest `sh:maxCount` drafted. `0` drafts none. |
| `closed` | `false` | Draft closed shapes. |
| `base` | `urn:x-sparkles:shape:<ds>:` | The namespace of the shape IRIs. |
| `format` | `json` | `json`, `turtle` (SHACL) or `shexc` (ShEx). `Accept: text/turtle` and `text/shex` also select them. |
| `timeout` | the server's query timeout | As in §2.1. |

The JSON document holds the options, the snapshot, one entry per shape with its
instances and property shapes, each drafted and rejected constraint with its counts, and
the SHACL Turtle, the ShExC schema and the shape map as text. The Turtle and ShExC texts
carry the counts as comments. Errors are those of §2.1, and `400` also covers a support
outside (0, 1].

The CLI is `sparkles schema --draft-shapes [--support S] [--closed] [--max-in N]
[--max-count N] [--class IRI…] [--min-instances N] [--format turtle|shexc|json]`, with the
selection flags of `sparkles schema`. The MCP server gains a `draft_shapes` tool with the
same arguments. The UI's schema browser gains a **Draft shapes** action. It shows the
draft and its counts, opens it in the dataset page's shapes editor, or installs it with
`PUT /$/validation/{ds}` in `warn` mode, which needs `admin`.

### 11.5 Design sketch

The drafter is `sparkles::schema::draft`, next to `discover`, and uses the same snapshot
APIs and graph filters.

1. **Types.** One pass over `POS[rdf:type]` maps each subject to its direct classes. A
   pass over `PSO[rdfs:subClassOf]` gives the superclass closure, and each subject's
   classes are closed under it. Class sets are interned, so subjects with the same
   classes share one entry.
2. **Literal keys.** For each predicate, a pass over `POS[p]` classifies each distinct
   object once, reading base-vocabulary keys in sorted batches as §5 does.
3. **Profiles.** A pass over `PSO[p]` takes each subject's run of distinct objects,
   summarizes it, and adds the summary to the accumulator of every class of the subject.
   Distinct values are tracked up to 64 per class and predicate, and sets of values per
   instance up to 4,096. Past either cap, no `sh:in` is drafted.
4. **Decisions** follow §11.3 and are rendered as JSON, Turtle and ShExC.

The cost is one pass over `rdf:type` and the two passes per predicate of §5, plus memory
for the map of typed subjects. The deadline, cancel flag and entry cap of §4.6 apply.

### 11.6 Acceptance examples

- **D1.** On a fixture with several types per node, a subclass with its own instances,
  mixed datatypes, an ill-formed integer, language tags and a status enumeration, the
  draft at support 1.0 validates with no results in SHACL, open and closed, and every
  association of the ShEx draft's shape map conforms.
- **D2.** With one wrong status among 20 instances, support 0.9 drafts `sh:in` with
  `excluded: 1`, and SHACL validation of the draft reports `sh:InConstraintComponent`
  for exactly that node. Support 1.0 drafts no `sh:in` and lists it as rejected.
- **D3.** A caller limited to some graphs gets a draft of those graphs only.
- **D4.** `support=0`, `support=1.5` and `maxIn=x` answer `400`, and a graph with no
  quads answers `404`.

### 11.7 Rejected alternatives

- **SPARQL `GROUP BY` queries per class.** They need a grouping per constraint kind, and
  the per-instance tests ("all values are integers") need nested aggregates. The ordered
  index passes of §5 do it in one sweep.
- **Sampling, as approximate shape extractors do.** The drafts would no longer conform
  at support 1.0. Exact counts are affordable at the sizes Sparkles has been measured on.
- **Support over all instances for value constraints.** A constraint on a property that
  few instances use would always pass.
- **`sh:or` of datatypes for mixed values, and `sh:node` between class shapes.** They
  make drafts harder to read, and recursive references make the guard validate in full
  (C10 §6.2.3). A person can add them while editing the draft.
- **Writing drafts into a shapes graph of the dataset.** That would be a write to the
  data. Drafts go through review, and installing one uses the validation configuration.

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30 in three commits: the
`sparkles::schema::discover` library module, the server endpoints with `sparkles schema`,
and the UI schema browser. The same work added Vitest unit tests for the UI's pure
modules.

- `discover` computes the observed layer from one ordered PSO pass and one POS pass per
  predicate. It reads the declared RDFS/OWL layer from the declared graphs, and it finds
  the roots and cycles of the `rdfs:subClassOf` hierarchy from strongly connected
  components. A deadline, a cancel flag or the entry cap fails the whole report, so
  nothing is truncated.
- `GET /$/schema/{ds}` and `/$/schema/{ds}/classes|predicates` take the parameters of
  §2: `graph`, `declaredGraph`, `reasoning`, `declared`, `limit`, `cursor` and
  `timeout`. The dataset keeps its last report, so a listing can finish after a write. A
  cursor whose report is gone gets `409`. `--schema-max-entries` defaults to 1,000,000.
- `sparkles schema --format text|json` prints the complete report and exits with status
  2 when a budget is exceeded.
- The UI follows both cursors to the end on one snapshot and restarts from the top on
  `409`. It shows the snapshot, a graph selector, an inference toggle, undeclared and
  cycle markers, and the mix of object kinds and datatypes per predicate.

The open questions were settled as the spec proposed. `graph` defaults to `default`,
built-in entries are listed with a `builtin` flag, and `/$/stats` was left alone.

**Deviations.** Durable commit identity landed, but the report did not gain
`snapshot.commit` or `datasetId`. Cursors still bind to the in-memory
`snapshot.version`, so they do not survive a restart. §4.5 and open question 5 describe
the commit-bound alternative.

**Later use.** The MCP server's schema tools are built on `sparkles::schema`
([C11](C11-mcp-server.md)).

**VoID export.** The VoID/Turtle export of Phase 2 landed on 2026-10-02.
`GET /$/schema/{ds}` with an RDF `Accept` header or `format=` answers with one
`void:Dataset` node, `urn:x-sparkles:schema:<ds>:<version>`, as the spec proposed. The
node carries `void:triples`, `void:entities`, `void:classes`, `void:properties`,
`void:distinctSubjects` and `void:distinctObjects`, one blank `void:classPartition` per
class with instances, and one `void:propertyPartition` per used predicate. The
declarations follow as the asserted triples, and `declarations=false` leaves them out.
`sparkles schema --format void` prints the description in Turtle, and `--format turtle`
adds the declarations. Four choices were made during implementation, and the maintainer
may revisit them.

- The selection's distinct subjects and objects were not in the report. A new
  `SchemaOptions::term_totals` counts them with one more pass over the SPO and OSP
  indexes, and only RDF requests ask for it. The JSON document does not show them, so it
  does not depend on which request filled the cache.
- `void:entities` of the dataset counts the distinct IRI subjects, and that of a class
  partition counts the class's instances.
- `void:classes` counts the distinct `rdf:type` objects, blank nodes included, as the
  VoID note defines it.
- The description also carries `dcterms:title` (the dataset name) and `dcterms:created`
  (the time the report was computed). Labels and comments keep their language tags.

**Not built.** The GSPO-driven scan for small named graphs and the `/$/stats` class
counts from the same pass were not built, so open question 3 stays open for
`/$/stats`. Phase 3 was not built either: per-class property profiles as a listing,
anonymous class expressions, schema diffs and incremental maintenance. Phase 4 computes
per-class profiles for its drafts, but no endpoint lists them. The constraints layer and
subject classes landed later, as described at the end of this section.

**Phase 4 landed on 2026-10-02**, as §11 designed it.

- `sparkles::schema::draft::draft_shapes` computes the SHACL instances from one pass over
  `PSO[rdf:type]` and one over `PSO[rdfs:subClassOf]`, with each subject mapped to an
  interned set of classes closed under the superclasses. Per predicate, a pass over
  `POS[p]` classifies the literal objects in sorted vocabulary batches, and a pass over
  `PSO[p]` summarizes each subject's values and adds the summary to the profile of each
  drafted class of the subject. The decisions of §11.3 and the Turtle and ShExC renderers
  live in the same module.
- `GET /$/schema/{ds}/shapes`, `sparkles schema --draft-shapes`, the MCP tool
  `draft_shapes` and the schema browser's **Draft shapes** dialog are built on it. The
  dialog opens the draft in the dataset page's SHACL or ShEx editor, or installs it with
  `PUT /$/validation/{ds}` in `warn` mode after a confirmation.

**Choices made during implementation.**

- The Turtle lists each rejected candidate as a comment inside its property shape, such
  as `# not drafted: sh:maxCount 1  (32 of 33 instances, would exclude 1)`, so a reader
  sees why a constraint is missing.
- `sh:in` is drafted for IRIs and literals only. An instance with a blank node or triple
  term value can satisfy no `sh:in`, and neither can one with more than 64 values.
- The CLI's flag for inferences is `--with-inferences`, because `--no-inferences` already
  means the opposite default of the schema report.
- The MCP tool answers with one language per call and a compact summary of each shape:
  its counts and the constraints that exclude instances, with the draft's text.
- The dialog has no class filter, although the endpoint takes `class`.

**Tests at landing.**

- `sparkles-shacl/tests/draft.rs` validates drafts of a fixture with several types per
  node, subclasses, mixed kinds and datatypes, an ill-formed integer, language tags, a
  misspelt enumeration value, blank nodes and triple terms, open and closed. It also
  generates 60 random datasets with random subclass graphs. Each draft at support 1
  conforms, open and closed. For each of 4 classes at supports 0.95, 0.8 and 0.6, the
  distinct focus nodes that validation reports per path and component equal the
  `excluded` counts the draft gave.
- `sparkles-shex/tests/draft.rs` validates the ShEx drafts of the same fixture and of the
  60 random datasets with their shape maps. Every association conforms at support 1, and
  below 1 only excluded instances fail.
- `http::schema::tests::drafted_shapes` covers the parameters, the three formats, the
  errors, a draft that `/{ds}/shacl` reports as conforming, and its installation as a
  `warn` guard. `router_tests::auth::graphs::drafted_shapes_cover_the_view` checks that a
  reader of some graphs gets a draft of those graphs only, and
  `mcp::tests::draft_shapes_tool` covers the tool. The UI's pure helpers have Vitest
  tests, and the phone-width Playwright test opens the dialog.

**Cost.** On 1.18M triples of generated people, employees and organisations, with 8
predicates, measured with the release build on a machine with a load average of about
30 from other builds: `sparkles schema --format json` took a median of 0.09 s, and a
draft took 0.45 s (minimum 0.24 s). A closed ShEx draft at support 0.95 took 0.35 s.
A draft costs about five schema reports, mostly for the per-subject summaries and the
map of typed subjects.

**The constraints layer and subject classes of Phase 2 landed on 2026-10-02.** Open
questions 6 to 9 record the decisions taken for them.

- `sparkles::schema::constraints` holds the types of the layer, and
  `sparkles_shacl::constraints` builds it from parsed shapes. For each class that shapes
  target, it lists the property shapes with a predicate path and their `sh:minCount`,
  `sh:maxCount`, `sh:datatype`, `sh:class` and `sh:nodeKind`, the components of their
  other constraints, their severity and their enforcement. `ShaclGuard::shapes` exposes
  the shapes an installed guard validates against, and `configured_shapes` reads those
  of a database's `validation.json` without installing a guard, for the CLI.
- `GET /$/schema/{ds}` carries the layer in `constraints`, and the new
  `GET /$/schema/{ds}/constraints` answers with the layer alone, without counting
  anything. A caller limited to some graphs reads only shapes graphs it may read, and
  sees the guard's shapes only when it may read every shapes graph of the guard.
- `SchemaOptions::subject_classes` and `detail=subjectClasses` add the classes of each
  predicate's subjects, with triple and subject counts, and the triples whose subjects
  have no IRI class. As §6 proposed, they come from a merge join of the predicate's
  subjects in PSO order with the selection's `(subject, class)` pairs of `rdf:type`, both
  sorted by subject. The pairs are read once per report, so the detail costs one pass
  over `rdf:type` and 16 bytes of memory per typed pair.
- `sparkles schema` gained `--subject-classes` and `--shapes`. The text report prints a
  line of subject classes under each predicate and then the constraints, one line per
  property shape with its enforcement.
- The MCP tool `describe_schema` gained `section: "constraints"`, with a `shapes`
  argument, and `subjectClasses`, which adds the ten classes with the most triples to
  each predicate.
- The UI's schema browser shows a SHACL chip next to the observed and declared chips of
  each constrained property, and the class panel lists the class's constraints with
  their enforcement. The chips are styled apart from the observed ones and never merged
  with them.

**Choices made during implementation.**

- A ShEx guard is not summarized. The layer is defined in SHACL terms, and a ShEx
  schema's triple constraints would need a mapping of their own.
- With `at=`, shapes graphs are read at that state, but the guard's shapes are always
  those installed now, because the configuration has no history.
- Several shapes graphs named by one request form one source, merged as write-time
  validation merges its shapes graphs, so that shapes may refer to each other across
  them.
- When a property shape repeats `sh:minCount` or `sh:maxCount`, the layer reports the
  strictest value. A second `sh:datatype` or `sh:nodeKind` is reported only as its
  component in `other`.

**Tests at landing.** `sparkles-shacl/tests/constraints.rs` covers targets, implicit
class targets, `sh:node`, `sh:or`, deactivated shapes, inverse paths, other targets and
the enforcement of each guard mode and threshold. `schema::tests::subject_classes_of_predicates`
checks the counts per graph selection, after a delta and after compaction.
`http::schema::tests::constraints_layer` and `subject_classes_on_request` cover the
parameters, both sources, the errors and the cursor rule.
`router_tests::auth::graphs::constraints_cover_the_view` checks the rules for callers
limited to some graphs, `mcp::tests::describe_schema_constraints_and_subject_classes`
covers the tool, `tests/cli_schema.rs` runs the CLI, and Vitest covers the UI helpers.

**SHACLC drafts (2026-10-02, with [G03](G03-shaclc.md)).** The draft is also rendered in
the SHACL Compact Syntax, with the same counts as comments. The JSON gains a `shaclc`
field, `format=shaclc` and `Accept: text/shaclc` select it, `sparkles schema
--draft-shapes --format shaclc` prints it, and the UI's dialog has a SHACLC tab.
`tests/draft.rs` of `sparkles-shacl` checks that every drafted SHACLC reads to the same
graph as the drafted Turtle.
