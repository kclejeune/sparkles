# C02: Schema discovery and export API

> **Status:** implemented in part
>
> **Phases:** Phase 1 (the `sparkles::schema` library, `GET /$/schema/{ds}` with paginated
> class and predicate listings, `sparkles schema`, the UI schema browser) shipped. Phase 2
> (SHACL constraints layer, subject classes per predicate, VoID/Turtle export) and
> Phase 3 are not built.
>
> **User docs:** [API: Schema discovery](../API.md#schema-discovery) ·
> [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation; the [Outcome](#outcome) section at the end
> records how it landed.

This is a clean-room spec. It depends only on the engine and storage correctness fixes
that preceded it, and uses durable commit identity ([CI](CI-commit-identity.md)) once
that lands.

## 1. Summary

The explorer's schema tab (`ui/src/lib/explore.ts`, `loadSchema`) builds the class
hierarchy, property declarations and instance counts in the browser. It runs four
independent SPARQL SELECTs, which causes five problems:

- **Silent truncation.** Class and property rows use `LIMIT 20000`, class counts
  `LIMIT 5000`, and the ontology header `LIMIT 1`. Past those limits the tree is
  incomplete and nothing says so.
- **No single snapshot.** The four requests can see different snapshots when an update
  lands between them.
- **No observed statistics per predicate.** The UI gets no datatypes, languages, object
  kinds or distinct counts per predicate, and it cannot select a graph.
- **No reuse.** The CLI, the dataset page and later adapters (GraphQL, MCP:
  [C11](C11-mcp-server.md)) cannot call the logic.
- **Inconsistent `/$/stats` numbers.** `/$/stats` lists only the top 100 predicates and
  classes. Its `classes[].instances` means *distinct subjects* (build statistics) when
  the delta is empty and *`rdf:type` quad count* otherwise (`http.rs::stats`), and
  `distinctSubjects`/`distinctObjects` come from the base generation only.

**Goals**

1. A library module `sparkles::schema` that computes one schema report from one
   `Snapshot`, and a server API `GET /$/schema/{ds}` plus a CLI command
   `sparkles schema`.
2. Three separate layers:
   - **declared**: what the RDFS/OWL vocabulary in the data asserts;
   - **observed**: exact counts over the selected graph;
   - **constraints**: SHACL shapes (Phase 2), validated on request only, not enforced.
3. Graph selection (`default` / `union` / named graph), an inference toggle, and the
   snapshot the report was computed at.
4. Complete, cursor-based pagination. Every list reports its `total`. Nothing is
   truncated without saying so.
5. Budgets: a timeout and an entry cap. When one is exceeded the request fails
   explicitly and never returns a partial report.
6. The explorer's schema tab uses the endpoint.

**Non-goals.** Shape learning or SHACL generation. OWL reasoning inside the schema API
(inferences come only from the materialized graph). Approximate or sampled statistics.
Per-class property profiles (Phase 3). Any guarantee derived from observations.

## 2. User-visible behavior

### 2.1 HTTP

| Method | Path | Result |
|---|---|---|
| GET | `/$/schema/{ds}` | `SchemaSummary`: the snapshot, totals, the ontology header, the hierarchy, and the first page of classes and predicates |
| GET | `/$/schema/{ds}/classes` | `Page<ClassEntry>` |
| GET | `/$/schema/{ds}/predicates` | `Page<PredicateEntry>` |

Parameters (all optional):

| Param | Values | Default | Meaning |
|---|---|---|---|
| `graph` | `default`, `union`, graph IRI; also `urn:x-arq:DefaultGraph`, `urn:x-arq:UnionGraph` | `default` | Graph whose triples are counted (§4.1) |
| `declaredGraph` | same | same as `graph` | Graph read for declarations (ontologies often live in their own named graph) |
| `reasoning` | `true`/`false` | `true` if the dataset has materialized inferences | Include `urn:x-sparkles:inferred` in `default`/`union` for **observed** counts |
| `declared` | `asserted`, `all` | `asserted` | `all` also reads declarations from the inferred graph |
| `limit` | 1–10000 | 1000 | Page size (the summary uses it for both first pages) |
| `cursor` | opaque | none | Continuation token from `next` |
| `timeout` | seconds | server query timeout | Budget for computing the report |

Types:

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

Only classes and predicates that are declared or used appear. A predicate appears when it
occurs in the selection or is declared in the declared selection (such a predicate has
`observed.triples = 0`). A class appears when it is:

- an IRI object of `rdf:type` in the selection;
- a subject declared with `rdf:type rdfs:Class`, `owl:Class` or `rdfs:Datatype`;
- an IRI subject or object of `rdfs:subClassOf`, `owl:equivalentClass` or
  `owl:disjointWith` in the declared selection.

**Errors** (standard `{ "error", "detail"? }` body):

| Status | When |
|---|---|
| 400 | Bad parameter value, or a malformed cursor |
| 404 | Unknown dataset, or `graph`/`declaredGraph` names a graph with no quads |
| 408 | The report did not finish within `timeout`. The message names the phase, e.g. `"schema discovery exceeded 60s while scanning predicates (412/9031); narrow graph= or raise timeout="` |
| 409 | The cursor belongs to an older snapshot: `"snapshot changed since the first page (version 17 → 18); restart from the first page"` |
| 413 | More than `--schema-max-entries` classes or predicates: `"dataset has 1204331 classes (limit 1000000)"` |

### 2.2 CLI

```
sparkles schema --loc DB [--graph default|union|IRI] [--declared-graph G]
                [--no-inferences] [--declared asserted|all] [--format json|text]
```

Prints the complete report with no pagination (`json` is `SchemaSummary` with every
item and `next: null`). `text` prints one line per class (`instances`, supers) and one
per predicate (`triples`, kinds, `max/subject`). Exits 2 on a budget error.

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

The name of the inferred graph is passed in by the caller: the library does not know
`urn:x-sparkles:inferred`, and the server and CLI supply it.

### 2.4 UI

The explorer's **Schema** tab calls `api.schema(ds, {graph, reasoning})`. The helper
follows `next` until it is `null`, keeps the first page's `snapshot.version`, and
restarts if it gets a 409. `reduceSupers` and the root and cycle logic move to the
server. The UI still does the transitive reduction for drawing.

- **Header:** "as of version N · generation gen-0003 · computed 12:04". A graph selector
  (default / union / each named graph from `/$/stats`) and an "include inferences"
  toggle.
- **Class tree:** each node shows its instance count. Classes that are used but not
  declared get an `undeclared` tag. Classes in `hierarchy.cycles` get a `cycle` icon
  with a tooltip listing the other members.
- **Class detail:** the asserted supers, the equivalent and disjoint classes, and the
  properties whose `declared.domains` / `declared.ranges` include the class.
- **Property list** (new columns): triples, distinct subjects and objects, and a
  stacked bar of object kinds and datatypes with language chips. When
  `maxPerSubject == 1` the chip reads **"≤1 per subject (observed)"**, styled as a
  measurement, with a tooltip: "No constraint enforces this; a future write may add a
  second value." When a property is declared `owl:FunctionalProperty`, the UI shows
  that as a separate **declared** chip. The two are never merged.

## 3. Standards basis

- RDF 1.1 Concepts / RDF 1.2 Concepts: term kinds (IRI, blank node, literal, triple
  term), literal datatype and language tag, `rdf:langString`, and `rdf:dirLangString`
  with a base direction.
- RDF Schema 1.1: `rdfs:Class`, `rdfs:subClassOf`, `rdfs:domain`, `rdfs:range`,
  `rdfs:subPropertyOf`, `rdfs:label`, `rdfs:comment`, `rdfs:Datatype`.
- OWL 2 Mapping to RDF Graphs: `owl:Class`, the `owl:*Property` types,
  `owl:equivalentClass`, `owl:disjointWith`, `owl:inverseOf`, `owl:Ontology`,
  `owl:versionInfo`.
- SPARQL 1.1 Protocol and Jena conventions: default graph, union graph, and the special
  IRIs `urn:x-arq:DefaultGraph` / `urn:x-arq:UnionGraph` (already accepted by
  `/{ds}/shacl`).
- VoID (W3C Interest Group Note), for the Phase 2 Turtle export: `void:triples`,
  `void:classPartition`, `void:propertyPartition`, `void:class`, `void:property`,
  `void:entities`, `void:distinctSubjects`, `void:distinctObjects`.
- SHACL (W3C Rec), for the Phase 2 constraints layer: `sh:targetClass`, `sh:path`,
  `sh:minCount`, `sh:maxCount`, `sh:datatype`, `sh:class`, `sh:nodeKind`.

## 4. Semantics

### 4.1 Graph selection

The selection is a set `G` of graph ids. The counting unit is the **triple**: a triple
that occurs in several graphs of `G` counts once. This matches SPARQL's view of the
union graph.

| `graph` | `reasoning=true` | `reasoning=false` |
|---|---|---|
| `default` | {default graph, inferred graph}; with `--union-default-graph`, every graph | {default graph}; with `--union-default-graph`, every graph except the inferred graph |
| `union` | every graph | every graph except the inferred graph |
| IRI | {that graph} (404 if it has no quads) | same |

`declaredGraph` resolves the same way. `declared=asserted` always removes the inferred
graph from the declared selection. `declared=all` keeps it. The default is `asserted`,
because materialized RDFS/OWL adds the transitive closure of `rdfs:subClassOf` and
`rdfs:Resource` / `owl:Thing` everywhere, which is the same reason the explorer uses
`reasoning=false` for declarations today.

### 4.2 Observed statistics (exact at the snapshot)

For predicate `p`, let `T_p` be the set of distinct triples `(s, p, o)` with a quad in
`G`:

- `triples = |T_p|`;
- `distinctSubjects = |{s}|`;
- `distinctObjects = |{o}|`;
- `maxPerSubject = max over s of |{o : (s,p,o) ∈ T_p}|`;
- `subjectsWithMultiple` = the number of subjects where that count is ≥ 2.

`objects` partitions `T_p` by the kind of `o`:

- `iri`, `blank`, `tripleTerm`;
- literals, grouped by datatype IRI. `xsd:string` is the datatype of simple literals.
  Language-tagged literals are `rdf:langString`, or `rdf:dirLangString` with a
  `direction`. Each group reports its `languages`, lowercased as stored.

Each group gives `triples` and `distinct` objects. Distinct values are **terms**, not
values: `"1"^^xsd:integer` and `"01"^^xsd:integer` are two distinct objects if both are
stored. Inline ids are canonical, so the two can only be two terms when one of them is a
vocabulary literal.

For class `C`, `instances` is the number of distinct `s` with `(s, rdf:type, C)` in
`G`. It counts asserted types only, plus inferred types when `reasoning=true`. There is
no subclass roll-up in the library; with `reasoning=true`, materialized RDFS provides
it.

`maxPerSubject` is a measurement of one snapshot. The API documentation, the JSON field
comments and the UI must never call it "functional", "single-valued" or "scalar".

### 4.3 Declarations

Declarations are read from the declared selection. Only IRI objects are listed.
Blank-node class expressions such as `owl:Restriction` are counted in
`totals.anonymousClassExpressions` and not expanded (Phase 3 may render them). Labels
and comments are all `rdfs:label` / `rdfs:comment` literals of the entry, with their
language. Picking one is the client's job.

### 4.4 Hierarchy

The hierarchy is computed over the declared `rdfs:subClassOf` edges between IRIs in the
classes list, with self-loops removed:

- `cycles`: every strongly connected component with more than one member. Members are
  sorted by IRI, and components are sorted by their first member.
- `roots`: every class with no superclass outside its own component. For a component
  with more than one member, only its smallest IRI is listed. Classes are ordered by
  descending subclass count, then IRI.

`ex:D rdfs:subClassOf ex:D` is not a cycle, so `ex:D` can still be a root.

### 4.5 Snapshot, pagination, caching

- **One snapshot.** One `discover` call uses one `Arc<Snapshot>`. `snapshot.version` is
  `Snapshot::version`. It changes on every commit and compaction and resets at restart,
  so it identifies a report only while the server runs. When CI exists
  ([CI](CI-commit-identity.md)), the report adds `snapshot.commit` (`Snapshot::commit`)
  and `snapshot.datasetId`, the response carries CI's `Sparkles-Commit` header, and
  the cursor binds to `commit` instead of `version`. Compaction then no longer
  invalidates cursors.
- **Order.** Lists are ordered by IRI (byte order of the UTF-8 string). Page `n+1`
  starts strictly after the last IRI of page `n`.
- **Cursor.** The cursor is base64url JSON
  `{"v": version, "h": hash(selection params), "a": lastIri}`. A different `h` gives
  400. A different `v` from the current snapshot gives 409, unless the cached report
  for `v` is still held, in which case the page is served from it. The server keeps
  the last report per dataset (one entry, key = version + selection).
- **Summary.** `/$/schema/{ds}` always computes the report or reuses the cached one.
  The page endpoints without a cursor behave the same.

### 4.6 Budgets and configuration

- **Time.** `timeout` (default: the server's query timeout, 60 s). The deadline and the
  cancel flag are checked after every scanned block or delta run. An exceeded deadline
  gives 408, with no partial report.
- **Entries.** `sparkles serve --schema-max-entries N` (default 1,000,000) caps the
  classes and the predicates separately. An exceeded cap gives 413.
- **Read-only servers.** The endpoint is read-only, so it is allowed with `--read-only`.
- **Cache memory.** One cached report per dataset, dropped when a newer snapshot's
  report replaces it.

## 5. Design sketch

**New module `crates/sparkles/src/schema.rs`** (`pub mod schema` in `lib.rs`). It uses
only `Snapshot` APIs: `scan`, `distinct_first`, `count`, `key`, `term`, `lookup_iri`,
`graph_ids`.

1. **Resolve the selection.** Build a `GraphFilter::{All, AllExcept(u64), Set(SmallVec<u64>)}`
   using `graph_ids()`. The graph id is key column 3 in PSO and POS
   (`Perm::order`: `[P,S,O,G]` / `[P,O,S,G]`).
2. **Predicate pass A (PSO).** For each `p` from `distinct_first(Perm::Pso)`, run
   `scan(Perm::Pso, &[p])`. Drop rows whose `g` is not in the filter. Rows with equal
   `(s,o)` are adjacent (`g` is the last column), so duplicates across graphs collapse
   by comparing with the previous row. This pass counts `triples`, `distinctSubjects`
   (changes of `s`), `maxPerSubject` and `subjectsWithMultiple` (run length of distinct
   `o` per `s`). Memory is O(1) per predicate.
3. **Predicate pass B (POS).** For the same `p`, count distinct `o` runs and classify
   each distinct `o` once:
   - inline tags map directly: `Int`→`xsd:integer`, `Decimal`→`xsd:decimal`,
     `Double`→`xsd:double`, `Bool`→`xsd:boolean`, `DateTime`→`xsd:dateTime`,
     `Date`→`xsd:date`, `BNode`→blank;
   - `Vocab`/`Delta` ids are classified from `snap.key(id)`. The first byte is `<`
     (IRI), `"` (literal) or `(` (triple term). For a literal, the suffix after the last
     `0xFF` (`KEY_SEP`) is empty (`xsd:string`), `@lang[--dir]` or `^datatype`.

   Triples per kind come from the length of each `o` run (after graph dedupe).
4. **Class pass (POS, prefix `[rdf:type]`).** For each distinct class `o`, count distinct
   `s` after the graph filter. Rows are `(o, s, g)` sorted, so the pass streams.
   Blank-node objects go to `anonymousTypeTargets`.
5. **Declarations.** Run `scan(Perm::Pso, &[q])` for each structural predicate
   (`rdfs:subClassOf`, `owl:equivalentClass`, `owl:disjointWith`, `rdfs:domain`,
   `rdfs:range`, `rdfs:subPropertyOf`, `owl:inverseOf`) under the declared filter, and
   `scan(Perm::Pos, &[rdf:type, T])` for each meta-class `T`. Labels and comments are
   looked up per entry with `scan(Perm::Spo, &[s, rdfs:label])`. Scanning all of
   `rdfs:label` would read every instance label in the dataset.
6. **Assemble.** Build `BTreeMap<String, ClassEntry>` / `BTreeMap<String, PredicateEntry>`
   (IRI order), compute the hierarchy with Tarjan's SCC over the declared edges, and
   check `max_entries` while inserting.

**Cost.** Two full passes over the selected predicates (PSO and POS), one pass over
`rdf:type`, and small declaration scans. On the 10.5M benchmark dataset this is two
sequential reads of about 10.5M rows each, well under the default timeout. Measure it
in Phase 1 and record the time in the PR.

**Why planner statistics are not used for counts.** `builder::Stats`
(`stats.json` per generation) is exact only for the base generation, counts quads over
all graphs (including the inferred graph), and ignores the delta.
`Snapshot::predicate_stat` adds delta inserts, ignores deletes, and sums distinct counts
(documented as "estimates only"). The build-time `classes` are distinct subjects over
all graphs. None of them gives graph-scoped triple counts, and none is exact after
updates, so the schema module scans. Planner statistics are used only to order
the predicate loop by estimated size, so a timeout reports useful progress. Exact counts
are affordable and correct under deltas by construction, because `scan` already merges
base ⊕ delta.

**Server** (`crates/sparkles-server/src/http.rs`, a new `schema` handler set):

- Parse the parameters.
- Resolve the graph IRI, and return 404 if `count(Perm::Gspo, &[g]) == 0`.
- Run `discover` in `blocking`, with the deadline from `timeout_param`.
- Keep the report cache in `Dataset` as
  `schema_cache: Mutex<Option<(SchemaKey, Arc<SchemaReport>)>>`.
- Paginate with the cursor rules of §4.5.

The report JSON carries `schemaFormat: 1`. Nothing is persisted, so there are no
crash-safety concerns.

**CLI:** `Cmd::Schema` in `main.rs`, calling the same `discover`.

**UI:** `api.ts` gets `schema()` and types. `explore.ts::loadSchema` becomes a thin
adapter from `SchemaSummary` pages to the existing `Schema` type, extended with
`observed` fields.

## 6. Phasing

**Phase 1 (MVP, about one day):**

- `sparkles::schema::discover` with passes 1–6 (classes, predicates, object kinds,
  datatypes, languages, `maxPerSubject`, declarations, labels, ontology header,
  hierarchy);
- the three HTTP endpoints with cursor pagination, 404/408/409/413, and the one-entry
  cache;
- `sparkles schema --format json|text`;
- the UI schema tab switched to the endpoint, with the snapshot header and the
  observed/declared chips;
- tests for §7 A–G.

**Phase 2:**

- `constraints` layer: `shapes=<graph IRI>` reads shapes with
  `sparkles_shacl::Shapes::from_store` and lists, per target class, property shapes with
  a predicate path and their `sh:minCount`, `sh:maxCount`, `sh:datatype`, `sh:class`,
  `sh:nodeKind`. They are labelled `"enforcement": "validated-on-request"` until
  write-time validation ([C10](C10-write-time-validation.md)) exists.
- `detail=subjectClasses`: per predicate, the classes of its subjects with triple
  counts. This is a merge join of `PSO[p]` subjects with `PSO[rdf:type]`, both sorted by
  `s`.
- Turtle export (`Accept: text/turtle`): observed statistics as VoID partitions in a
  `urn:x-sparkles:schema:<ds>:<version>` node. Declarations are emitted as the asserted
  triples.
- A GSPO-driven scan for small named graphs.
- The `/$/stats` classes computed with pass 4, so the mixed "distinct subjects vs quads"
  meaning goes away.

**Phase 3:** per-class property profiles (the predicates used by instances of C, with
counts), rendering of anonymous class expressions, a schema diff between two snapshots
(needs point-in-time queries, [F06](F06-snapshots-and-point-in-time.md)), and incremental maintenance from the
delta.

## 7. Acceptance examples

All requests go to dataset `t`. `ex:` = `http://ex.org/`. The JSON shown is an excerpt.

**A. Mixed datatypes and kinds** (default graph):

```turtle
ex:a ex:val 1, "1.5"^^xsd:decimal .
ex:b ex:val "x", "x"@en .
ex:c ex:val ex:d, _:n, <<( ex:a ex:val 1 )>> .
```

`GET /$/schema/t/predicates` → the `ex:val` item:

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
The roots are in that order because `A` (with `C` below it) ranks before `D`.
`ex:D.declared.superClasses = []`, `ex:C.observed.instances = 1`, `totals.classes = 4`.

**C. Undeclared classes:** `ex:x a ex:Person . ex:Employee rdfs:subClassOf ex:Person .`
→ `ex:Person`: `instances 1`, `declared.types []`, `superClasses []`.
`ex:Employee`: `instances 0`, `superClasses ["http://ex.org/Person"]`. The UI tags both
`undeclared`.

**D. Delta changes:** bulk-load `ex:x a ex:P . ex:y a ex:P .` (base generation), then
run:

```sparql
DELETE DATA { ex:x a ex:P } ;
INSERT DATA { ex:z a ex:P ; ex:age "x" . GRAPH ex:g1 { ex:y a ex:P } }
```

- `graph=default`: `ex:P.instances = 2` (y, z), `ex:age.objects.literals = [xsd:string ×1]`,
  and `snapshot.version` is greater than before the update.
- `graph=union`: `ex:P.instances = 2`. `ex:y` is counted once although it is typed in
  two graphs.
- `graph=http://ex.org/g1`: `ex:P.instances = 1`.

Run the same checks again after `POST /$/compact/t`. The numbers must be identical.

**E. Pagination:** 2,500 classes `ex:C0000…ex:C2499`, one instance each.

- `GET /$/schema/t/classes?limit=1000` → 1000 items, `total 2500`, `next` is a string.
- Following `next` gives 1000 items, then 500 items with `next: null`. Pages are
  disjoint, and their union is all 2,500 classes in IRI order.
- Run `INSERT DATA { ex:q a ex:C9999 }` after the first page, then follow the old
  cursor → 200 while the cached report for the old version is held. After the new
  version's report replaces it → 409.

**F. Errors:**

- `?graph=http://ex.org/missing` → 404.
- `?limit=0` → 400.
- `?timeout=0.000001` on a non-trivial dataset → 408 with `"exceeded"` in the error.
- `--schema-max-entries 10` with 11 classes → 413.

**G. Observation is not a contract:** `ex:name a owl:FunctionalProperty .
ex:a ex:name "A" . ex:b ex:name "B1", "B2" .` → `declared.types` includes
`owl:FunctionalProperty`, and `observed.maxPerSubject = 2`. Remove `ex:b`'s second value:
`maxPerSubject = 1`, and no field anywhere changes its name or meaning.

**H. Reasoning toggle:** after `POST /$/reason/t` (rdfs) with
`ex:C rdfs:subClassOf ex:B . ex:x a ex:C .`:

- default request → `ex:B.observed.instances = 1`;
- `reasoning=false` → `ex:B.instances = 0`;
- in both cases `ex:C.declared.superClasses = ["http://ex.org/B"]`, and
  `rdfs:Resource` is not a declared super (`declared=asserted`).

## 8. Rejected alternatives

- **Keep the logic in the UI with larger LIMITs.** It still truncates, still uses
  several snapshots, and cannot be reused.
- **Implement with internal SPARQL (`GROUP BY ?p (DATATYPE(?o))`).** Simple, and
  snapshot-consistent through `sparql::query(snap, …)`. But it needs several GROUP BY
  passes with a hash grouping per query, `DATATYPE()` materializes each term, and the
  graph dedupe needs `DISTINCT` over triples. The key-level scan is one ordered pass
  and needs no grouping memory.
- **Report planner statistics.** Not exact after updates and not graph-scoped (§5).
- **Count quads instead of triples.** Cheaper to explain against `/$/stats`, but a
  triple asserted in two graphs would count twice in `union`. That disagrees with SPARQL
  over the same selection.
- **Offset pagination or top-N by count.** Offsets drift when the report is recomputed.
  Ordering by count makes the cursor depend on changing numbers. IRI order plus a
  version-bound cursor is stable and detects change.
- **Partial reports on budget exhaustion.** They look complete to naive clients.
  Failing explicitly matches the "no silent truncation" goal.
- **Approximate distinct counts (HyperLogLog).** Unnecessary at the current scale. They
  could return later as an explicit `approx=true` mode with error bounds.

## 9. Open questions

1. Should `graph` default to `union` for the explorer? The spec keeps `default` for
   consistency with SPARQL and `/{ds}/shacl`.
2. Should `builtin` entries (`owl:Class` as an `rdf:type` target, etc.) be omitted
   server-side by default? The spec includes them, flagged, and the UI filters them as
   today.
3. Should the dataset page's "Top predicates/classes" switch to this API? That would
   require the scan, while `/$/stats` is cheaper. The spec leaves `/$/stats` alone in
   Phase 1 and fixes its class-count semantics in Phase 2.
4. Is the entry cap of 1,000,000 right? Datasets that mint a class per entity would
   exceed it.
5. `version` is not durable. Cursors do not survive a restart, and 409 is the correct
   answer after one. With CI, cursors bind to `commit` and survive both restarts and
   compaction, as long as the cached report or an identical recomputation is
   available.

## 10. Sources

- Sparkles repository (the only implementation source):
  - [CI-commit-identity.md](CI-commit-identity.md) (a sibling clean-room spec):
    `Snapshot::commit`, the dataset id, the `Sparkles-Commit` header.
  - `ui/src/lib/explore.ts`: current queries, LIMITs, `reduceSupers`, cycle promotion.
  - `ui/src/routes/explore/+page.svelte`: the schema tab.
  - `crates/sparkles/src/store.rs`: `Snapshot::{scan, count, estimate, distinct_first,
    predicate_stat}` and delta semantics.
  - `crates/sparkles/src/builder.rs`: `Stats`, `PredicateStat`, the class collector.
  - `crates/sparkles/src/index.rs`: `Perm` key orders.
  - `crates/sparkles/src/id.rs`: tags and the key encoding.
  - `crates/sparkles-server/src/http.rs`: the `stats` handler, parameter conventions,
    error format.
  - `crates/sparkles-shacl/src/lib.rs` and `shapes.rs`: `Shapes::from_store`,
    `Constraint`.
  - `docs/API.md`, `docs/AUDIT.md`, `README.md`, and the project's clean-room feature
    order (internal planning notes).
- W3C RDF 1.1 / RDF 1.2 Concepts, RDF Schema 1.1, OWL 2 Mapping to RDF Graphs, SHACL,
  SPARQL 1.1 Protocol: vocabulary and term kinds. Cited from working knowledge; not
  re-fetched for this spec.
- W3C VoID Interest Group Note (https://www.w3.org/TR/void/): partition vocabulary for
  the Phase 2 Turtle export. Cited from working knowledge.
- Apache Jena convention of `urn:x-arq:DefaultGraph` / `urn:x-arq:UnionGraph`: as
  already implemented in Sparkles' SHACL endpoint.
- Fluree was **not** consulted: no code, documentation, or product pages.

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30 as three commits: the
`sparkles::schema::discover` library module, the server endpoints with `sparkles schema`,
and the UI schema browser. The same work added Vitest unit tests for the UI's pure
modules.

- `discover` computes the observed layer from one ordered PSO pass and one POS pass per
  predicate and the declared RDFS/OWL layer from the declared graphs, with roots and
  cycles of the `rdfs:subClassOf` hierarchy from strongly connected components. A
  deadline, a cancel flag and the entry cap fail the whole report; nothing is truncated.
- `GET /$/schema/{ds}` and `/$/schema/{ds}/classes|predicates` take the parameters of
  §2 (`graph`, `declaredGraph`, `reasoning`, `declared`, `limit`, `cursor`, `timeout`).
  The dataset keeps its last report, so a listing can finish after a write; a cursor
  whose report is gone answers `409`. `--schema-max-entries` defaults to 1,000,000.
- `sparkles schema --format text|json` prints the complete report and exits with
  status 2 when a budget is exceeded.
- The UI follows both cursors to the end on one snapshot and restarts from the top on
  `409`; it shows the snapshot, a graph selector, an inference toggle, undeclared and
  cycle markers, and the object-kind and datatype mix per predicate.

The open questions were settled as the spec proposed: `graph` defaults to `default`,
built-in entries are listed with a `builtin` flag, and `/$/stats` was left alone.

**Deviations.** After durable commit identity landed, the report did not gain
`snapshot.commit` or `datasetId`, and cursors still bind to the in-memory
`snapshot.version`, so they do not survive a restart (§4.5 and open question 5 describe
the commit-bound alternative).

**Later use.** The MCP server's schema tools are built on `sparkles::schema`
([C11](C11-mcp-server.md)).

**Not built.** All of Phase 2 (the SHACL constraints layer, `detail=subjectClasses`,
the VoID/Turtle export, the GSPO-driven scan for small named graphs, `/$/stats` class
counts from the same pass) and Phase 3 (per-class property profiles, anonymous class
expressions, schema diffs, incremental maintenance).
