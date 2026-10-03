# G02: ShEx 2.1 validation

> **Status:** implemented in part (Phases 1 and 2; Phase 3 except ShEx 2.2)
>
> **Phases:** Phase 1 is the `sparkles-shex` crate with ShExC and ShExJ, imports, shape
> maps and stratified typing, plus `POST /{ds}/shex`, `sparkles shex validate|parse`, the
> shexTest harness and `bench:shex`. Phase 2 is write-time ShEx validation with
> `validation.json` format 2, ShExR, `SPARQL """…"""` selectors, the UI's SHACL | ShEx
> switch and `bench:shex-write`. Four Phase 3 items are built: the MCP tools
> `validate_shacl` and `validate_shex`, the interval matcher, which became the Phase 1
> matcher, incremental guard validation, which propagates typing changes, and ShExR
> schemas stored in named graphs for the guard. The guard also has a grandfather mode.
> ShEx 2.2 (`EXTENDS`, `ABSTRACT`) is not built.
>
> **User docs:** [API: ShEx validation](../API.md#shex-validation) ·
> [API: Write-time validation](../API.md#write-time-validation) ·
> [API: MCP server](../API.md#mcp-server) · [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This is a clean-room spec. It depends on three parts of Sparkles:

* the store snapshot API (`Snapshot::scan`, `count`, `lookup_term`, `term`);
* the data-graph selection of the SHACL validator (`crates/sparkles-shacl/src/data.rs`);
* for Phase 2, the write guard of [C10](C10-write-time-validation.md)
  (`crates/sparkles/src/guard.rs`, `CommitGuard`).

It closes the README gap "Shape languages | ShEx (jena-shex) | ✗ SHACL only" and removes
ShEx from the non-goals in [AUDIT §5](../AUDIT.md#5-explicit-non-goals-for-v1).

## 1. Summary, goals, non-goals

Sparkles validates RDF with SHACL only. Apache Jena also ships `jena-shex`, a ShEx 2.1
validator with a `shex` command. This feature adds **Shape Expressions (ShEx) 2.1** as a
second shape language, implemented in a new crate `sparkles-shex`. The validator reads
the data graph directly from a store `Snapshot` through index scans, as SHACL does. It
validates focus nodes in parallel and handles recursive schemas without deep call
stacks. The feature provides:

* ShExC parsing and writing, and ShExJ import and export.
* Shape maps: query, fixed and result shape maps, in compact and JSON syntax.
* A `POST /{ds}/shex` endpoint and a `sparkles shex validate|parse` command whose names
  and flags follow Jena's `shex` command.
* In Phase 2, ShEx as an alternative language for the C10 write guard, and a UI panel.

**Goals**

* **Conformance to ShEx 2.1.** Pass the shexTest suite: 100 % of the negative syntax and
  structure tests, and every approved validation test except a short, justified
  known-failures list (§8).
* **Index-backed neighbourhoods.** For each (node, shape) pair, fetch only the arcs whose
  predicate the shape mentions, with prefix scans. The one exception is `CLOSED`, which
  needs a scan of all outgoing arcs. Nothing is copied into an in-memory graph.
* **Recursion.** Handle recursive schemas exactly, under the specification's stratified
  greatest-fixpoint semantics (§4.5). Data-dependent recursion depth must not use the
  Rust call stack: a 100k-node `foaf:knows` chain must validate.
* **Bounded worst cases.** Matching a neighbourhood is NP-hard in general (§3.1). The
  matcher is polynomial for the common deterministic shapes, and the ambiguous cases run
  under an explicit work budget.
* **Performance.** Within 2× of the SHACL benchmark on the same data (§9).
* **Jena compatibility.** Where Jena has an equivalent, Sparkles matches it. That covers
  the CLI command names and flags, the compact shape-map syntax and a Jena-like text
  report.

**Non-goals**

* **ShEx 2.2 / 2.next** (`EXTENDS`, `ABSTRACT`, `ShapeDecl`). These are an unfinished
  Community Group draft, and Jena does not implement them. A schema using them is a parse
  error that names the feature. Revisit after Phase 3 (Open question 11).
* **Executing semantic actions** beyond the specification's Test extension (§4.6). No
  JavaScript, SPARQL-generating or "Map" actions.
* **Repair and suggestion features**, such as shexTest's `validation-contrib`.
* **ShEx-to-SHACL translation** in either direction.
* **ShExR in Phase 1.** Schemas as RDF wait for Phase 2. A ShEx result vocabulary in RDF
  is out of scope, because none is standardized.
* **Incremental write-time validation.** This waits for Phase 3. Until then the guard
  validates in full, like C10 Phase 1.

## 2. User-visible behavior

### 2.1 Schemas

| Syntax | Media type (in / out) | Extension | Phase |
|---|---|---|---|
| ShExC (compact) | `text/shex` | `.shex` | 1 |
| ShExJ (JSON-LD, context `http://www.w3.org/ns/shex.jsonld`) | `application/shex+json`, which is unregistered (Open question 9). Also `application/json` or `application/ld+json` when the object has `"type": "Schema"`. | `.json`, `.shexj` | 1 |
| ShExR (RDF, ShEx vocabulary `http://www.w3.org/ns/shex#`) | any RDF syntax Sparkles reads | `.ttl` … | 2 |

The ShEx 2.1 specification registers only `text/shex` (IANA section). Jena registers no
`Lang` for ShEx, so Sparkles has nothing to match and chooses the media types above.

Every syntax parses into one AST that mirrors ShExJ (§6.1). ShExC → ShExJ → ShExC round
trips therefore keep everything, annotations and semantic actions included, except
comments. Schema text is UTF-8. Relative IRIs resolve against `BASE` first, then against
the schema's location: a file path, or the request's `base` parameter.

### 2.2 Shape maps

A **shape association** pairs a node selector with a shape label. A **shape map** is a
list of associations. Sparkles uses the forms of the 2017 ShapeMap draft
(shex.io/shape-map):

| Kind | Node selector | Use |
|---|---|---|
| query map (input) | An RDF term, or one of the patterns `{FOCUS p o}`, `{FOCUS p _}`, `{s p FOCUS}` and `{_ p FOCUS}`. Phase 2 adds `SPARQL """SELECT ?focus …"""`, an extension (§4.7). | what to validate |
| fixed map | an RDF term | the query map after expansion |
| result map | an RDF term, plus `status` (`conformant` \| `nonconformant`), `reason`, `appinfo` | the answer |

The shape label is an IRI, a prefixed name, or `START` (`@START` or `@start`).

**Compact syntax.** This is the ShapeMap grammar plus Jena's extensions:

* `BASE` and `PREFIX` directives at the start;
* optional commas between associations;
* an optional trailing `.`;
* `a` for `rdf:type`.

Associations use the prefixes and base that the directives declare. Without directives,
prefixed names use the schema's prefixes.

```
PREFIX ex: <http://example.org/>
{FOCUS a ex:Person}@ex:PersonShape,
ex:alice@ex:PersonShape,
"42"^^xsd:integer@ex:Answer,
_:b1f@START
```

A blank node such as `_:b1f` names a store blank node by the label Sparkles prints in
query results (`_:b<hex>`). Any other blank-node label selects nothing and produces a
warning.

**JSON syntax.** An array of objects
`{"node": string, "shape": string, "status"?: …, "reason"?: …, "appinfo"?: …}`:

* `node` is in compact term syntax. A bare IRI is accepted, as in shexTest's map files.
  A string starting with `{` (or `SPARQL`) is a selector.
* `shape` is an IRI or `"START"`.
* The 2017 draft's names `nodeSelector` and `shapeLabel` are accepted on input.
* A duplicate (node, shape) pair is an error, as the ShapeMap draft says.

### 2.3 HTTP: `POST /{ds}/shex`

Fuseki has no ShEx operation. `jena-fuseki2` neither mentions ShEx nor depends on
`jena-shex`, so this endpoint is a Sparkles extension. It is modeled on Fuseki's
`/{ds}/shacl`, as Sparkles implements it in `shacl()` in `http.rs`.

**Request.** Two equivalent forms:

1. **Schema as the body.** `Content-Type: text/shex`, or a ShExJ media type from §2.1.
   The shape map comes from the query string:
   * `map=<compact shape map>` (URL-encoded); or
   * `node=<term>` with `shape=<label>`. `shape` defaults to `START`.
2. **A JSON envelope.** `Content-Type: application/json`, with a body that is not itself
   a ShExJ `Schema`:
   ```json
   { "schema": "PREFIX ex: <…> ex:S { … }",
     "schemaFormat": "shexc",            // or "shexj"; default: sniffed (leading '{' → shexj)
     "map": "{FOCUS a ex:Person}@ex:S",  // compact string, or a JSON shape map array
     "externs": "ex:Ext { … }",          // optional: definitions of EXTERNAL shapes (§4.6)
     "imports": { "http://ex.org/common": "<ShExC text>" },  // optional: inline import bodies (§4.2)
     "base": "http://ex.org/schema" }    // optional
   ```

Query parameters, the same as `/{ds}/shacl` where they overlap:

| param | values | default |
|---|---|---|
| `graph` | `default` \| `union` \| graph IRI (Jena's `urn:x-arq:*` IRIs accepted) | `default` |
| `reasoning` | `true` \| `false`: merge `urn:x-sparkles:inferred` into the data graph | `true` when the dataset has inferences |
| `results` | `all` \| `nonconformant` | `all` |
| `format` | `json` \| `shapemap` \| `smap` \| `text` (or use `Accept`) | `json` |
| `timeout` | seconds | the server's query timeout |
| `semact-trace` | `true`: include Test-extension `print` output in `appinfo` | `false` |

**Responses.**

* `200` whether or not the data conforms. The body is the report of §2.4, and the
  response carries `Sparkles-Commit: <head>`. When the data graph includes inferences,
  the response also carries the inference headers of `/{ds}/shacl` (`with_inferences`).
* `400`:
  * a syntax error in the schema, the shape map or the externs, with `line` and
    `column`;
  * a structure error: an unresolved reference, a negated reference cycle, an invalid
    `&include`, an import that cannot be resolved, or 2.2 syntax;
  * a shape label in the map that the schema does not define;
  * an invalid parameter.
* `404`: a `graph` that does not exist.
* `408`: timeout.
* `413`: the body exceeds `--max-query-body-mb`.
* `507` with `budget: "result-bytes"`: the report is larger than `--max-result-mb`. The
  rule is the same as in `shacl::max_results`.
* `507` with `budget: "validation-work"`: the partition or typing budget of §5.9 ran
  out. This is a new `BudgetKind`.
* `501`: the server was built without the `shex` feature.

Validations run in the existing pool that uses half the cores. Its accessor
`crate::shacl::pool()` is renamed `validation_pool()`. A validation stops at its next
check when the client disconnects (`cancel_on_drop`).

### 2.4 Result format

**`application/json`** (default) is a Sparkles report. Nodes and shapes are SPARQL JSON
terms, as in `ShaclReport`, so the UI's `TermView` renders them unchanged:

```ts
type ShexReport = {
  conforms: boolean;                        // every association conformant
  counts: { conformant: number; nonconformant: number };
  results: {                                // all, or only nonconformant (results=nonconformant)
    node: Term;
    shape: Term | { type: "start" };
    status: "conformant" | "nonconformant";
    reason?: string;                        // first failure, one line (§5.8)
    appinfo?: {
      failures: ShexFailure[];              // structured; up to 8 per result
      prints?: string[];                    // Test semantic-action output (semact-trace)
    };
  }[];
  warnings: string[];                       // e.g. "2 semantic actions with unknown extension <…> were not run"
  millis: number;
};
type ShexFailure =
  | { kind: "nodeKind" | "datatype" | "facet" | "valueSet"; value: Term; constraint: string }
  | { kind: "cardinality"; predicate: string; inverse: boolean; min: number; max: number | null; count: number }
  | { kind: "closed"; predicate: string; value: Term }
  | { kind: "extra"; predicate: string; value: Term }   // a non-EXTRA arc left unmatched
  | { kind: "noMatch"; detail: string }                 // OneOf / group cardinality: no partition matches
  | { kind: "reference"; shape: string; value: Term }   // a value fails a referenced shape
  | { kind: "not"; shape: string }
  | { kind: "semAct"; extension: string; message: string }
  | { kind: "external"; shape: string };
```

Other formats:

* **`format=shapemap`.** The ShapeMap-draft JSON result map: an array of
  `{node, shape, status, reason?, appinfo?}`, with compact-syntax strings for `node` and
  `shape`. This is the interoperable form.
* **`format=smap`** (`text/plain`). The compact result map, one association per line:
  `<n>@<S>` for conformant and `<n>@!<S>` for nonconformant. Other ShEx tools use the
  `@!` form. The published draft grammar has no syntax for the status.
* **`format=text`.** Jena's text report:
  * `OK` when everything conforms;
  * otherwise one line per association,
    `<n> @ <S> :: Focus = <n>, Status = nonconformant, Reason = <reason>`.

  `sparkles shex validate` prints this by default, as Jena's `shex validate` does.

**Order.** Results follow the query-map association order. Within one selector they
follow the store's id order, which is deterministic for a snapshot.

**Mapping onto the SHACL report UI.** The SHACL results table has the columns Focus node,
Path, Value, Constraint, Severity and Message. The ShEx table has Node, Shape, Status and
Reason. Status is a badge like the severity badge, with `nonconformant` styled as a
violation. Expanding a row shows its `appinfo.failures`. The guard (§2.7) carries the same
result objects in `ValidationSummary.results`. ShEx has no severities, so each
nonconformant result counts as one `violation` in `by_severity`.

### 2.5 CLI

Jena's launcher is `shex <cmd>`, with these subcommands:

* `validate`, `val`, `v`;
* `parse`, `p`, `print`.

Its flags (from `jena-cmds/src/main/java/shex/shex_validate.java`) are:

* `--schema`, `--shapes`, `-s`;
* `--data`, `-d`;
* `--map`, `--shapesMap`, `-m`;
* `--target`, `--node`, `-n`, which validates against the schema's `START`.

Sparkles keeps those names as aliases:

```
sparkles shex validate (--loc DB | --data FILE…) --schema FILE
    (--map FILE | --shape-map 'STR' | --node TERM [--shape LABEL])
    [--graph default|union|IRI] [--no-inferences] [--externs FILE]
    [--format text|json|shapemap|smap] [--only-nonconformant] [--timeout S]
    [--semact-trace]
sparkles shex parse FILE… [--out shexc|shexj|text] [--base IRI]
```

* `validate` aliases: `val`, `v`. Flag aliases: `--shapes`/`-s`, `--datafile`/`-d`,
  `--shapesMap`/`-m`, `--target`/`-n`. A map file ending in `.json` is read as a JSON
  map, and any other file as compact syntax.
* `--node` without `--shape` validates against `START`. If the schema has no start
  shape, the error is `"the schema has no start shape; give --shape"`. Jena says
  "Start node required for URI-validation".
* **Exit status.** 0 when everything conforms and 1 when any association is
  nonconformant, as in `sparkles shacl` and Jena. Usage, parse and structure errors exit
  2.
* `parse`:
  * `--out shexc` (the default) pretty-prints ShExC;
  * `--out shexj` prints ShExJ;
  * `--out text` prints a structural dump, like Jena's default `text`.

  Jena's `parse` cannot print JSON, because `shex_parse` has no ShExJ writer. This
  command can. Several files are printed one after another under `# file` headers, as in
  Jena.
* Imports in local files resolve against the schema file's directory (§4.2).
  `--data FILE…` loads the files into an in-memory store, like `sparkles shacl`.

`sparkles shacl` stays a flat command. Open question 5 asks about aligning it.

### 2.6 Rust library API

```rust
// sparkles_shex
pub struct Schema { /* ShExJ-shaped AST: prefixes, base, imports, start, start_acts, shapes */ }
impl Schema {
    pub fn parse_shexc(text: &str, base: Option<&str>) -> Result<Schema, ParseError>; // line/col
    pub fn from_shexj(json: &str) -> Result<Schema, ParseError>;
    pub fn to_shexj(&self) -> serde_json::Value;
    pub fn to_shexc(&self) -> String;
}
pub trait Resolver: Send + Sync {          // imports and EXTERNAL shapes (§4.2, §4.6)
    fn import(&self, iri: &str) -> anyhow::Result<Option<Schema>>;
    fn external(&self, label: &str) -> anyhow::Result<Option<ShapeExpr>> { Ok(None) }
}
pub struct FileResolver { pub dirs: Vec<PathBuf>, pub inline: HashMap<String, Schema>, /* … */ }
pub struct CompiledSchema { /* IR of §5.2; Send + Sync, reusable across snapshots */ }
pub fn compile(schema: &Schema, resolver: &dyn Resolver) -> Result<CompiledSchema, SchemaError>;

pub struct ShapeMap(pub Vec<Association>);          // query or fixed
impl ShapeMap {
    pub fn parse(text: &str, prefixes: &PrefixMap, base: Option<&str>) -> Result<ShapeMap, ParseError>;
    pub fn from_json(json: &str) -> Result<ShapeMap, ParseError>;
}
#[derive(Clone, Debug, Default)]
pub struct ValidateOptions {        // the data-graph fields and limits of sparkles_shacl::ValidateOptions
    pub data_graph: Option<String>, pub extra_graphs: Vec<String>, pub exclude_graphs: Vec<String>,
    pub parallel: bool, pub pool: Option<Arc<rayon::ThreadPool>>,
    pub timeout: Option<Duration>, pub cancel: Option<Arc<AtomicBool>>,
    pub max_results: Option<usize>,
    pub max_pairs: Option<usize>,            // typing graph size (§5.9), default 10M
    pub max_partitions: Option<u64>,         // per (node, shape) match (§5.5), default 100k
    pub only_nonconformant: bool,
    pub semact_trace: bool,
}
pub struct ResultMap { pub conforms: bool, pub results: Vec<ShapeResult>, pub warnings: Vec<String>, /* counts */ }
pub fn validate(snap: &Arc<Snapshot>, schema: &CompiledSchema, map: &ShapeMap, opts: &ValidateOptions)
    -> anyhow::Result<ResultMap>;
pub fn validate_node(snap: &Arc<Snapshot>, schema: &CompiledSchema, node: &Term, shape: &ShapeLabel,
    opts: &ValidateOptions) -> anyhow::Result<ShapeResult>;
// ResultMap: to_json(), to_shapemap_json(), to_smap(), to_text()
```

The API mirrors `sparkles_shacl::{Shapes::parse, validate, validate_node}`. Jena's
equivalents are `Shex.readSchema` / `ShexValidator.get().validate(graph, schema, map)`,
listed in the README's Jena→Sparkles table.

### 2.7 Write-time validation with ShEx (Phase 2)

Write-time validation reuses the [C10](C10-write-time-validation.md) guard. A dataset
uses one language at a time (Open question 7). `validation.json` moves to format 2, which
adds a `language` field. A format 1 file reads as `"language": "shacl"` and keeps working
unchanged:

```json
{ "format": 2, "language": "shex", "mode": "reject",
  "schema": { "file": "validation-schema.shex", "format": "shexc", "source": "/abs/schema.shex", "sha256": "…" },
  "shapeMap": "PREFIX ex: <http://ex.org/> {FOCUS a ex:Person}@ex:PersonShape, {FOCUS a ex:Org}@ex:OrgShape",
  "dataGraph": "default", "includeInferences": false,
  "timeoutSeconds": 10, "reportLimit": 100, "updated": "…" }
```

| field | meaning |
|---|---|
| `schema` | Copied into `<db>/validation-schema.shex` (or `.json`) when the config is set. The copy exists for the reason in C10 §9: a file at a referenced path could change without the data being revalidated. Imports are resolved at set time and inlined into the copied ShExJ, so a later write never fetches anything. Phase 3 adds ShExR schemas in named graphs (`{"graphs": [iri…]}`), read from the post-state like SHACL shapes graphs. |
| `shapeMap` | A query map, re-expanded on every validated post-state, so new focus nodes are picked up. A fixed map is allowed but rarely useful. |
| `threshold` | Not accepted: every nonconformant association blocks. |
| others | As C10 §2.1. |

Behavior:

* **Semantics.** The guard behaves as in C10 §§4.1–4.6. That covers validation of the
  post-state, the `reject`/`warn` decision, the enable precondition (`409` when the head
  does not conform), bypass, timeouts, and skipping writes that touch no validated graph.
* **Rejection.** A rejected write gets `422`. `validation.results` holds the
  nonconformant ShEx result objects (§2.4). `blocking` is the nonconformant count, and
  `total` is the number of associations. There is no Turtle report, so a request whose
  `Accept` lists `text/turtle` still gets JSON. The header gains `lang=shex`:
  `Sparkles-Validation: status=rejected, mode=reject, strategy=full, lang=shex, blocking=3, …`.
* **Fail closed.** The `GuardMissing` rule covers a binary built without `shex` that opens
  a ShEx-validated database.
* **HTTP.** `/$/validation/{ds}` takes the format 2 body. On a PUT, the inline schema is
  `{"schema": {"inline": "<ShExC>", "format": "shexc"}}`.
* **CLI.** `sparkles validation --loc DB --lang shex --schema FILE --shape-map STR
  --mode reject|warn …`.

### 2.8 UI

In Phase 2 the dataset page's *Validate (SHACL)* panel becomes *Validate*, with a
SHACL | ShEx switch. The ShEx side has:

* a schema text area, with a default example schema built from the dataset's top classes
  (`/$/schema/{ds}`);
* a shape-map input with examples such as `{FOCUS a <C>}@<S>`;
* the same graph and inference controls;
* *Validate* and *Download* buttons. Download fetches `format=shapemap` JSON.

The results table follows §2.4. Its Node column links to Explore, as the SHACL focus
column does. The schema and map persist in `localStorage`, like `sparkles.shacl.{name}`.
`api.ts` gains `shex()`, `shexRaw()` and the `ShexReport` type. Where the UI has a
write-time validation panel, it shows the language.

### 2.9 Routing, auth, limits, metrics

* `routes.rs`:
  * the route table gains `("/{ds}/shex", &["POST"])`;
  * `need()` maps it to `Dataset(Read)`, like `/{ds}/shacl`;
  * `/$/validation/{ds}` is unchanged.
* `ratelimit.rs` puts the route in the query class, with `/{ds}/shacl`.
* `obs.rs` adds `Op::Shex` (`"shex"`), so requests are counted in
  `sparkles_requests_total`. The `sparkles_validation_*` series gain a `language` label
  (`shacl` \| `shex`).
* `DatasetInfo.endpoints` gains `shex` when the feature is built.
* The access log, timeouts, `--max-query-body-mb` and `--max-result-mb` apply as for
  `/{ds}/shacl`.

## 3. Standards basis

### 3.1 ShEx 2.1 (Final Community Group Report, 2019-10-08, shex.io/shex-semantics)

* **§5.2 Validation Definition.**
  * `isValid(G, Sch, ism)` holds iff every pair (n, sl) of the fixed map satisfies
    `satisfies(n, s, G, Sch, completeTyping(G, Sch))`.
  * **completeTyping** is built per stratum of the maximal strongly connected components
    (SCCs) of the dependency graph.
  * Each stratum's typing is the union of all correct typings that agree with the lower
    strata. That is a greatest fixed point per stratum, and §4.5 implements it.
* **§5.3.2** defines `satisfies` for NodeConstraint, Shape (membership in the typing),
  ShapeOr/And/Not, ShapeExternal ("implementation-specific mechanisms") and references.
* **§5.4 Node constraints.**
  * Node kinds, and datatypes with lexical validity.
  * String facets on the lexical form, the IRI string or the blank-node label. Lengths
    are in code points, and `pattern` is XPath `fn:matches` with flags.
  * Numeric facets on SPARQL numeric types, with type promotion.
  * Value sets (§5.4.6): objectValue, Language, IriStem, IriStemRange, LiteralStem,
    LiteralStemRange, LanguageStem, LanguageStemRange, and wildcard ranges with
    exclusions. Stems compare with `fn:starts-with`, and language stems with RFC 4647
    basic filtering.
* **§5.5.2** defines `matchesShape` through a partition of `neigh(G, n)` into *matched*
  and *remainder*. It also defines `matches` for EachOf, OneOf, TripleConstraint and
  cardinalities ("a max of -1 is treated as unbounded"), and the meaning of EXTRA and
  CLOSED.
* **§5.6 Import.** Labels of imported schemas are in scope transitively. Circular and
  redundant imports collapse, and an import's `start` is ignored.
* **§5.7 Requirements.**
  * §5.7.2 and §5.7.3: references resolve, and no label reaches itself through references
    alone.
  * §5.7.4: "the dependency graph of a schema MUST NOT have a cycle that traverses some
    negated reference". A reference is negated under an odd number of `ShapeNot`, **or
    when the triple constraint's predicate is in the shape's `extra`**.
* **§5.8.1.** "The evaluation of an individual SemAct is implementation-dependent."
  §5.8.2 describes the `http://shex.io/extensions/Test/` extension.
* **§5.9.** Annotations do not affect validation.
* **Complexity.** Staworko et al. (ICDT 2015):
  * bag membership in a regular bag expression (RBE) is NP-complete, and so is
    single-type validation for ShEx(RBE) (Thm 1);
  * membership for **single-occurrence** RBEs (SORBE) is in PTIME (Thm 5);
  * deterministic ShEx(SORBE) multi-type validation is in PTIME (Cor. 6).

  §5.5 uses these results. Boneva, Labra Gayo and Prud'hommeaux (ISWC 2017) prove the
  stratified semantics well-defined and give validation algorithms. Labra Gayo et al.
  (2015) introduce RBE derivatives.

### 3.2 Readings where the 2.1 text and the test suite differ

| # | 2.1 text | Sparkles follows | why |
|---|---|---|---|
| R1 | `matchables` = remainder ∩ **arcsOut** with a mentioned predicate. An unmatched incoming arc is then always allowed. | **Direction-symmetric.** An incoming arc in the remainder whose predicate appears in an *inverse* triple constraint is a matchable, so it must not match, and its predicate must be in `extra`. | shexTest `1inversedotRef1_fail` ("two incoming `<p1>` for a {1,1} inverse") and `1inversedotCard2_fail` require it. Open question 1. |
| R2 | "minlength: v >= len", "maxlength: v <= len", "totaldigits: v is less than or equals the number of digits" | the intended direction: `len ≥ minlength`, `len ≤ maxlength`, `digits ≤ totaldigits` | The spec's own examples and the suite use this direction. It is a known erratum. |
| R3 | ShapeExternal: implementation-defined | an error when no definition is supplied (§4.6) | Jena treats EXTERNAL as always true, so it accepts data without saying so. Open question 2. |

### 3.3 Apache Jena `jena-shex` (Apache-2.0), and where Sparkles differs

Jena's module (checkout `b1dcba53b5`) is the compatibility reference for what users see:

* the CLI (§2.5);
* the compact shape-map syntax with BASE/PREFIX, optional commas and a trailing `.`;
* triple-pattern selectors with exactly one FOCUS;
* the text report;
* the semantic-action plug-in model, in which unknown action IRIs are ignored.

Sparkles differs from Jena on purpose where Jena does not follow the spec:

| Jena behaviour (file) | Sparkles |
|---|---|
| ShExJ and ShExR parsers are empty stubs (`ParserShExJ.java`, `ParserShExR.java`), and there is no ShExJ writer | ShExJ in and out (Phase 1); ShExR (Phase 2) |
| Validation enumerates the full cartesian product of triple→sub-expression assignments (`ShapeEvalEachOf`), and set partitions for group cardinality | derivatives, equivalence classes and a budget (§5.5) |
| Group cardinality with `min == 0` always matches (`ShapeEvalCardinality` L45) | the spec's partition definition |
| Inverse constraints re-query the graph instead of using the neighbourhood (`ShapeEvalTripleConstraint` L42) | inverse arcs are part of the neighbourhood (§5.3) |
| Recursion assumes `true` on a cycle, with no typing or invalidation (`ValidationContext.cycle`) | the stratified greatest fixed point (§4.5) |
| `EXTERNAL` becomes `ShapeExprNone`, which is always true | an error unless definitions are supplied (R3) |
| An empty `{}` shape (even `CLOSED {}`) becomes `ShapeExprNone` | `CLOSED {}` rejects every outgoing arc, per §5.5.2 |
| Start semantic actions run only on the single-node path | they run once per validation, on every path |
| Annotations are dropped | kept and round-tripped |
| No JSON results | JSON and the ShapeMap JSON forms (§2.4) |

## 4. Semantics

### 4.1 Schema checks (at compile time)

These checks produce `SchemaError`, mapped to HTTP `400` and CLI exit 2:

* **S1.** Every shape-expression reference and triple-expression inclusion (`&label`)
  resolves within the schema, its imports or its externs.
* **S2.** No shape label reaches itself through `ShapeAnd`/`ShapeOr`/`ShapeNot`/reference
  edges alone, that is, without passing through a `Shape` (§5.7.2). Likewise no triple
  expression includes itself (§5.7.3).
* **S3.** An `&include` names a triple expression. A node constraint does not qualify
  (negativeStructure `includeNonSimpleShape`).
* **S4. Negation requirement** (§5.7.4). The label dependency graph has an edge s1 → s2
  for every reference from s1's expression to s2. An edge is negative when it is under an
  odd number of `NOT`, or when it is a triple constraint's value reference whose
  predicate is in the enclosing shape's `extra`. Tarjan's algorithm computes the SCCs,
  and an SCC that contains a negative edge is an error. The error names the cycle, for
  example `negated reference cycle: :S -[EXTRA :a]-> :S` (negativeStructure
  `Cycle2Extra`).
* **S5.** Labels are unique across the schema and its imports. Overlapping labels are an
  error (§5.6).
* **S6.** The 2.1 `STRICT` rules Jena applies are kept, because negativeSyntax tests
  require them:
  * two string-length facets of the same kind;
  * a numeric facet on a non-numeric datatype;
  * an unknown datatype with a numeric facet;
  * `MININCLUSIVE` and similar with a literal that is not numeric.

The SCCs from S4 also give the **strata** that §4.5 uses. Strata are numbered bottom up:
an edge s1 → s2 across SCCs has stratum(s2) < stratum(s1).

### 4.2 Imports

* `IMPORT <iri>` is resolved by a `Resolver`, in this order:
  1. inline bodies supplied with the request (`imports` in the JSON envelope), which the
     test harness uses;
  2. **files**:
     * the CLI resolves the IRI as a path relative to the importing schema;
     * the server allows `file:` IRIs only under `--load-dir`, reusing `FileLoads`, the
       same rule as SPARQL `LOAD <file:…>`;
  3. **http(s)**, through the server's `OutboundPolicy` (the `--outbound-*` flags, timeout
     and size limits of SPARQL `LOAD`). There is no network access in tests.

  If the exact IRI does not resolve, the resolver appends `.shex` and then `.json`,
  because shexTest imports name schemas without an extension.
* The import closure is followed transitively. Each IRI is fetched once, and cycles
  terminate. The `start` of imported schemas is ignored.
* Overlapping labels are an error (S5). So is an imported schema with `startActs`. The
  ShEx 2.1 Import section requires both.
* The guard (§2.7) resolves imports when the config is set and stores the closed
  schema.

### 4.3 Node constraints

Node constraints are evaluated on store ids (§5.4):

* **Node kind.** `IRI`, `BNODE`, `NONLITERAL`, `LITERAL`. RDF 1.2 triple terms are none of
  these, and fail every node kind.
* **Datatype.** The literal's datatype IRI is equal to the constraint's, and the
  lexical form is valid for XSD datatypes. Validity uses the same checks as `sh:datatype`
  (`sparkles_shacl::is_valid_literal`, moved to `sparkles::xsd`, §6.2). An
  `rdf:langString` constraint matches any language-tagged literal.
* **String facets.**
  * `LENGTH`, `MINLENGTH`, `MAXLENGTH` count Unicode code points of the literal's
    lexical form, an IRI's string, or a blank node's label.
  * `PATTERN /re/flags` uses XPath `fn:matches` semantics. Patterns are compiled with
    `sparkles::sparql::expr::compile_regex(pattern, flags)`, the SPARQL `REGEX`
    implementation, which accepts the flags `s m i x q`. ShExC `\/` and `\uXXXX` escapes
    are undone first.
  * The facets apply to every node kind (§5.4.4).
* **Numeric facets.**
  * `MININCLUSIVE`, `MINEXCLUSIVE`, `MAXINCLUSIVE`, `MAXEXCLUSIVE`, `TOTALDIGITS`,
    `FRACTIONDIGITS`.
  * The value must be a SPARQL numeric (`xsd:integer` and its derived types,
    `xsd:decimal`, `xsd:float`, `xsd:double`) with a valid lexical form. Anything else
    fails.
  * Comparisons use XPath numeric promotion: `sparkles::sparql::value`, the comparison
    behind SPARQL `<`.
  * `TOTALDIGITS` and `FRACTIONDIGITS` fail on float and double. They count digits of
    the canonical decimal form, ignoring leading and trailing zeros (R2).
* **Value sets.**
  * An IRI or literal `objectValue` matches by RDF term equality. A literal's datatype
    and language are part of the term, so `1` does not match `"1"`, and `"01"^^xsd:integer`
    does not match `1` (`sht:NumericEquivalence`).
  * `@en` matches the literals tagged exactly `en`, case-insensitively.
  * `<iri>~`, `"lit"~` and `@en~` are stems: the IRI string or the literal's lexical form
    starts with the stem. A language stem uses RFC 4647 basic filtering
    (`sparkles::sparql::expr::lang_matches`, made `pub`). The empty language stem `@~`
    matches every language-tagged literal.
  * A stem with exclusions (`<a>~ - <a/b> - <a/c>~`) matches when the stem matches and no
    exclusion does.
  * A wildcard with exclusions (`. - <a> - "x"~`) matches anything not excluded.
* **Combination.** A node constraint with several parts (kind, datatype, facets, values)
  is satisfied iff all parts are.

### 4.4 Shapes and triple expressions

`matchesShape(n, S, G, m)` holds iff the arcs of `neigh(G, n)` can be split into
*matched* and *remainder* such that:

1. *matched* matches `S.expression` (`matches` below). With no expression, *matched*
   is ∅.
2. **Matchables** (reading R1) are the remainder arcs (s, p, o) whose (p, direction)
   appears in some triple constraint of the expression. Every matchable has
   `p ∈ S.extra`, and no matchable *matches* a triple constraint of the expression. An
   arc matches a triple constraint when the predicate and direction are the same and the
   arc's value satisfies the constraint's `valueExpr` under the typing m.
3. If `S.closed`, every remainder arc that is outgoing and not a matchable is forbidden.
   Incoming arcs are never constrained by `CLOSED`.

`matches(T, e, m)`:

* **TripleConstraint** `p@V {min,max}`: T can be split into k singletons with min ≤ k ≤
  max. Each singleton holds an arc with predicate p in the constraint's direction, and
  its value (the object, or the subject for an inverse constraint) satisfies V, or V is
  absent.
* **EachOf** `(e1 ; … ; ek){min,max}`, **OneOf** `(e1 | … | ek){min,max}`: T can be split
  into j parts, min ≤ j ≤ max, and each part matches the group once.
  * For EachOf, matching once means the part can be split into k subparts with
    `matches(subpart_i, e_i)`.
  * For OneOf, it means some `e_i` matches the whole part.
* **Semantic actions** on an expression must succeed whenever it matches (§4.6).
* **`&label`** stands for the labelled triple expression. It is expanded in place at
  compile time, giving each occurrence its own constraint instances (§5.2).

### 4.5 Typing, recursion and negation

* **Pairs.** A typing assigns `true`/`false` to *pairs* (node, shape-expression id).
  Pairs exist for declared labels and for every shape expression that is a triple
  constraint's `valueExpr`, since anonymous nested shapes also need a typing entry
  (§5.2).
* **`satisfies`** is compositional:
  * And, Or and Not combine their operands' results at the same node;
  * a reference reads the referenced label's pair at the same node;
  * a Shape evaluates `matchesShape`;
  * a node constraint evaluates §4.3.
* **Result.** Sparkles computes the specification's `completeTyping`, restricted to the
  pairs reachable from the fixed map:
  * Strata are processed in increasing order (§4.1).
  * Within one stratum every reference is positive, so `satisfies` is monotone in the
    stratum's own pairs. The stratum's typing is the **greatest fixed point**: every
    pair starts `true`, and pairs that fail are removed until nothing changes (§5.6).
  * Pairs of lower strata are final before any pair that reads them is evaluated, so
    `NOT` and EXTRA references always read final values.
* **Restriction.** Restricting to reachable pairs is exact: a pair's value depends only
  on the pairs it can reach through references.

### 4.6 Semantic actions, annotations, EXTERNAL, START

* **Semantic actions** are parsed, kept in the AST and round-tripped. They are executed
  through a `SemActHandler` registry keyed by extension IRI:
  * **`http://shex.io/extensions/Test/`** is built in and enabled by default (Open
    question 3). It has no side effects. `fail("msg")` fails the expression it is
    attached to, or the whole validation for a start action. `print(s|p|o|"lit")`
    appends to `appinfo.prints` when `semact-trace` is set.
  * **Any other extension IRI is ignored**, and its actions count as successes. This
    matches Jena's plug-in behaviour and the "implementation-dependent" wording of
    §5.8.1. The report carries one warning per unknown IRI:
    `"N semantic actions with extension <iri> were not run"`.
  * Placement:
    * start actions (`startActs`) run once per validation;
    * actions on a Shape run on a node that matches it;
    * actions on a triple constraint run once per triple matched to it;
    * actions on EachOf/OneOf run on the arcs matched by the group.

    The handler sees `(s, p, o)` for triple constraints and the focus node for shapes.
* **Annotations** never affect validation.
* **EXTERNAL.** A label declared `EXTERNAL` gets its definition from the `Resolver`:
  * `--externs FILE` or the envelope's `externs` (a ShExC/ShExJ document whose shapes
    define the external labels), the form of shexTest's `.shextern` files;
  * otherwise, a reference to the label is a `SchemaError` raised at compile time:
    `"external shape <L> has no definition"` (R3).
* **START.** `start = <S>` or `start = { … }` names the shape behind `START` in maps and
  in `--node` without `--shape`. A map that references `START` when the schema has none
  is a `400`.

### 4.7 Shape-map expansion

* A node term selects itself, whether or not it occurs in the data graph. A node that is
  absent has an empty neighbourhood.
* `{FOCUS p o}` selects the subjects of (?, p, o) with a `POS` prefix scan. `{FOCUS p _}`
  selects the distinct subjects of p (`PSO`). `{s p FOCUS}` and `{_ p FOCUS}` are
  symmetric (`SPO` / `POS`). All of these scans are restricted to the data graph.
* `a` means `rdf:type`. There is no RDFS reasoning. With `reasoning=true`, materialized
  `rdf:type` inferences are part of the data graph.
* **Phase 2:** `SPARQL """query"""` runs a SELECT on the same snapshot and data graph,
  with the request's query budgets. The focus nodes are the bindings of `?focus`, or of
  the first projected variable. This selector comes from other ShEx tools and is not part
  of the ShapeMap draft.
* The fixed map is the union of the expansions, deduplicated per (node, label) and kept
  in first-seen order. Its size counts against `max_results`.

### 4.8 Errors

| condition | HTTP | CLI | library |
|---|---|---|---|
| schema, map or externs syntax error (line and column), or 2.2 syntax | 400 | 2 | `ParseError` |
| structure error S1–S6, an import that cannot be resolved, a missing external | 400 | 2 | `SchemaError` |
| map references an undefined label, or `START` without a start shape | 400 | 2 | `SchemaError` |
| `graph` does not exist | 404 | 2 | `Err` |
| timeout | 408 | 1 (error text) | `Err` ("validation timed out") |
| client disconnected | 503 (existing `Cancelled`) | – | `Err` |
| partition or typing budget exceeded (§5.9) | 507 `validation-work` | 1 | `Err(BudgetExceeded)` |
| report larger than the response budget | 507 `result-bytes` | – | `TooManyResults` |
| built without `shex` | 501 | "built without the `shex` feature" | – |

A budget error never becomes a `nonconformant` result: an answer the engine could not
compute is an error, not a verdict.

## 5. Validation algorithm

### 5.1 Pipeline

```text
schema text ──parse──▶ AST ──resolve imports/externs, checks S1–S6──▶ compile ──▶ CompiledSchema (IR)
shape map ──parse──▶ query map ──expand on snapshot (§4.7)──▶ fixed map F
F ──discover (§5.6.1)──▶ typing graph: pairs + dependency edges, bucketed by stratum
for stratum 1..k: refine to the greatest fixed point (§5.6.2), in parallel waves
results for F's pairs; reasons for nonconformant ones (explain mode, §5.8)
```

### 5.2 Compilation (IR)

* **Shape expressions** live in an arena, as `SeId → enum { And(Vec<SeId>),
  Or(Vec<SeId>), Not(SeId), Ref(SeId), Nc(NcId), Shape(ShapeId), External(SeId) }`.
  * References are resolved to `SeId`s, and chains of references are collapsed.
  * Each label and each triple-constraint `valueExpr` is a **pair kind**: it gets an
    index `0..P` used in pair keys.
  * Any other And/Or/Not node is evaluated inline at its node, without a pair.
* **Shapes.**
  * `Shape { closed, extra: SmallVec<PredId>, expr: Option<TeId>, sem_acts }`.
  * Triple expressions are flattened. Includes are expanded per occurrence, and nested
    groups keep their cardinalities.
  * Each shape stores `preds: Vec<(PredId, Dir, SmallVec<TcIdx>)>`, the triple
    constraints per (predicate, direction), and `max_occ[tc]`: the product of the
    maximum cardinalities from the constraint up to the root, or ∞.
  * A shape is **flat** when its expression is a single triple constraint, or an EachOf
    of triple constraints without group cardinality, and every (predicate, direction)
    has exactly one triple constraint. Matching a flat shape is per-constraint counting
    (§5.5.1).
  * A shape is **deterministic** when every (predicate, direction) has one triple
    constraint, even if there are OneOf or nested groups. The bag of constraint symbols
    is then determined by the data, and the expression is single-occurrence by
    construction. Every triple-constraint instance is a distinct symbol after expansion,
    so the expression is always a SORBE over those symbols. The NP-hard part is only the
    assignment of triples to constraint instances when several instances share a
    predicate.
* **Predicates** are resolved once per snapshot with `lookup_iri`. A predicate that is
  absent from the store matches no arcs. Its constraints are still counted with zero
  arcs.
* **Node constraints** are compiled per snapshot (`NcPlan`):
  * kind and datatype become tag tests (§5.4);
  * IRI and literal value sets become `FxHashSet<Id>` of resolved ids. Values absent
    from the store can never match, so they are dropped;
  * IRI stems become a base-vocabulary id range plus a string test for `Delta` ids
    (§5.4);
  * facets keep their parsed bounds.

### 5.3 Neighbourhoods from snapshots

For a pair (n, S), the engine fetches only what `matchesShape` can observe:

| arcs | scan | when |
|---|---|---|
| outgoing with predicate p | `SPO` prefix `[n, p]`, which yields the objects | every outgoing p in `S.preds` |
| incoming with predicate p | `POS` prefix `[p, n]`, which yields the subjects | every inverse p in `S.preds` |
| any other outgoing arc | `SPO` prefix `[n]`, stopping at the first predicate not in `S.preds` | only when `S.closed` |

* All scans go through the data-graph selection of `sparkles-shacl/src/data.rs`
  (`GraphSel`: default, union, a set, all-except). It moves to a shared module (§6.2).
  Arcs are deduplicated across graphs: a triple present in two graphs of the data graph
  is one arc, as in the RDF merge. `data.rs` already deduplicates adjacent keys, because
  `G` is the last key column.
* Unmentioned incoming arcs are never fetched, because no rule observes them. This keeps
  hubs cheap: a class node with 10M incoming `rdf:type` arcs costs nothing unless a shape
  has `^rdf:type`.
* **Count-first fast fail.** For a (p, dir) with no EXTRA, every arc must be matched.
  If `Snapshot::count(prefix)` exceeds the sum of `max_occ` over its triple
  constraints, the pair is `false` without decoding any value. Index counts include
  duplicates across graphs, so this test is used only when the data graph is a single
  graph.
* **Streaming.** Arcs are consumed in chunks of up to 4096 values. Flat shapes count
  while scanning (§5.5.1) and stop at the first value that settles the answer, such as
  `count > max` or a failing value without EXTRA.

### 5.4 Node constraints on ids

Most checks never decode a term:

* **Tag tests.**
  * `Int`, `Decimal`, `Double`, `Bool`, `DateTime` and `Date` ids are literals of known
    datatype. Inline ids are canonical, so they are lexically valid by construction.
  * `BNode` ids are blank nodes.
  * `Vocab` and `Delta` ids need the term's kind. It comes from the first byte of the
    vocabulary key (`<` for IRIs), read through a new
    `Snapshot::term_kind(id) -> TermKind` that avoids building an `oxrdf::Term`.
* **Numeric facets** on inline ids compare the decoded number directly. Otherwise they go
  through `Value::from_term`.
* **IRI stems.** The base vocabulary is sorted with IRIs keyed as `<iri`, so the IRIs
  with a given prefix form one contiguous id range. A new
  `Snapshot::iri_prefix_range(prefix) -> (Id, Id)` wraps `Vocab::prefix_range`. A
  `Vocab` id is tested by range comparison, and a `Delta` id by string comparison.
* **Per-snapshot cache.** Results of node-constraint checks on `Vocab`/`Delta` ids are
  cached per `NcId` in a small sharded map, because popular values such as class IRIs
  recur.

### 5.5 Matching a neighbourhood

**Inputs.** The arcs A of n restricted to `S.preds`, and the current typing.

**Step 1: candidates.** For each arc a with (p, dir), compute
`cand(a) = { tc ∈ S.preds[p, dir] : value(a) satisfies tc.valueExpr }`. Reading a
reference uses the typing (§5.6) and never recurses. An arc with an empty `cand(a)` is a
matchable remainder arc. If p ∉ `S.extra`, the pair fails (reason `extra`). Otherwise the
arc is allowed and dropped.

**Step 2: the fast path for flat shapes** (§5.5.1). Every non-empty `cand(a)` is a
singleton, so each constraint's count is the number of arcs assigned to it. The pair
holds iff `min_tc ≤ count_tc ≤ max_tc` for every tc. This is O(|A|), streaming, and
covers almost every real schema.

**Step 3: deterministic shapes.** `cand(a)` is still a singleton, so the bag
`c: TcIdx → count` is fixed. Membership of c in the expression is decided by
**bag derivatives** (Labra Gayo et al. 2015). The implementation decides it with
iteration-count intervals instead; see the
[implementation note](#implementation-note-2026-10-01-matcher-algorithm) and the Outcome.

* `∂_{tc}^k(e)` derives by symbol tc taken k times at once, so the cost does not grow
  with arc counts. For example `∂_a^k(a{m,n}) = a{max(m−k,0), n−k}`, and the derivative
  is ∅ when k > n.
* The derivative of an EachOf distributes over the one child that contains the symbol.
  Every symbol occurs once, so no OneOf is introduced.
* The derivative of a OneOf keeps the branches that contain the symbol, and requires the
  other symbols of the bag to stay in the same branch.
* The derivative of a group with cardinality `(g){m,n}` unrolls one iteration:
  `∂(g)·(g){m−1,n−1}`.
* The bag matches iff the final expression is nullable.

Because the expression is single-occurrence and simplification drops ∅ branches, the
derivative stays O(|e|) in size. If measurements call for it, Phase 3 may add the
interval algorithm of Staworko et al. (2015, Thm 5) for deterministic shapes. The
derivative matcher stays the reference, and property tests compare the two.

**Step 4: ambiguous shapes.** Some arcs have |cand(a)| ≥ 2, for example
`ex:p [1] ; ex:p @<A> ; ex:p .`.

* Group the ambiguous arcs into **classes** by their candidate set. Only the number of
  arcs in a class assigned to each constraint matters, not which arcs. A class of n arcs
  over e candidates has C(n+e−1, e−1) distributions instead of eⁿ assignments.
* Enumerate the distributions class by class, depth first, adding to the fixed counts
  of the singleton arcs. Prune:
  * any count above `max_occ[tc]`;
  * any constraint whose minimum can no longer be reached by the arcs left.
* Each complete count vector is tested as in Step 3. Vectors already tested are memoized
  in a hash set.
* The first vector that matches makes the pair `true`. Exhausting the vectors makes it
  `false`.
* Each tested vector costs one unit of `max_partitions` (default 100k per pair).
  Exceeding it is `Err(BudgetExceeded("validation-work"))` (§4.8).

**Step 5: semantic actions.** A match is accepted only if the semantic actions of the
constraints and groups involved succeed on the assignment. When actions exist, Step 4
enumerates concrete assignments within a matching count vector until one also satisfies
the actions. Only the Test extension can fail an action, so this matters for conformance
tests only.

**Group cardinality with OneOf.** `(a | b){2}` matches `{a, b}`, because its two
iterations take different branches. The derivative handles this exactly. Jena's min-0
shortcut does not (§3.3).

### 5.6 Typing: stratified worklist refinement

Jena and rudof use goal-directed recursive validation. A call `check(n, S)` evaluates S
at n and recurses into `check(v, S')` for each referenced value. A pair already in
progress is assumed `true`. Sparkles does not do this, for two reasons:

* The call depth equals the length of reference chains in the **data**. A 100k-person
  `foaf:knows` chain against `{ foaf:knows @<Person>* }` needs 100k frames. In this
  spec's research, rudof's debug build overflowed the default 8 MB stack on shexTest
  itself.
* Results computed under an assumption that later fails must be invalidated. Jena does
  not invalidate them, which is unsound for some recursive schemas.

Sparkles instead computes the greatest fixed point over an explicit graph of pairs.

#### 5.6.1 Discovery

1. **Seed.** The pairs (n, label) of the fixed map.
2. **Expand.** For a pair (n, k), the pairs its evaluation can read are:
   * (n, k′) for each reference reached through And/Or/Not at the same node;
   * (v, tcv) for each arc a of n in the shape's `preds` whose triple constraint has a
     `valueExpr` that is a pair kind tcv, with v = value(a).

   These come from the scans of §5.3, which do not depend on the typing.
3. **Local pre-check.** Before a pair is expanded, it is evaluated with every reference
   read as *unknown*, in Kleene three-valued logic: `NOT unknown = unknown`, and an
   unknown candidate is "maybe". If the result is definitely `false`, for example a
   failing node constraint or a cardinality that no assignment can satisfy, the pair is
   final `false` and is **not expanded**. When data are locally invalid, this prunes most
   of the graph and recovers most of the early exit of goal-directed search.
4. **Storage.**
   * Pairs are interned in a sharded `FxHashMap<(Id, PairKind), PairIdx>`.
   * Edges are stored as forward adjacency in `Vec<u32>` slices, plus reverse edges for
     re-evaluation.
   * Each pair records its stratum, which is the stratum of its label.
5. **Parallel BFS.** Discovery is breadth first by frontier. Each frontier is processed
   with `par_chunks(256)` on the validation pool.

#### 5.6.2 Refinement per stratum

For stratum i = 1 … k:

```text
alive ← all undecided pairs of stratum i            // assumed true (greatest fixed point)
dirty ← alive
while dirty ≠ ∅:                                    // one parallel wave per iteration
    failed ← { p ∈ dirty : ¬eval(p, typing) }       // par_chunks(256); reads lower strata (final)
                                                    //   and stratum-i pairs (alive = true)
    alive ← alive \ failed;  mark failed false (final)
    dirty ← { q ∈ alive : q has an edge to some p ∈ failed }   // reverse edges
mark alive true (final)
```

* `eval` is §5.5 with references read from the typing.
* **Soundness.** At the end, `alive` is a correct typing for stratum i: every pair in it
  re-evaluated `true` against `alive` after its last dependency changed. So `alive` is
  contained in the greatest correct typing.
* **Completeness.** A pair is removed only when it is false with every remaining
  stratum-i pair assumed true. That assumption over-approximates the greatest fixed
  point, and evaluation is monotone within the stratum (all references are positive
  there, §4.1 S4). So no pair of the greatest fixed point is ever removed, and `alive`
  equals the greatest fixed point restricted to the discovered pairs. Lower strata are
  final before stratum i starts, which is why `NOT` and EXTRA are evaluated against
  final values.
* **Termination and cost.** `alive` only shrinks. A pair is re-evaluated at most once
  per removal of one of its dependencies, so the total work is O(Σ over pairs of
  (1 + in-stratum dependency removals)) evaluations. Waves stop when nothing fails.
  Non-recursive schemas finish in one wave per stratum.

**Determinism.** The fixed point is unique, so results do not depend on thread
scheduling. Reasons are computed afterwards from the final typing (§5.8), so they are
deterministic too.

### 5.7 Parallelism

* Expansion of the shape map, discovery frontiers and refinement waves run on the
  validation pool in chunks of 256 pairs. Small inputs (< 512 pairs per wave) run on the
  calling thread, as SHACL does.
* The typing is a `Vec<AtomicU8>` indexed by `PairIdx`, with the states `unknown`,
  `alive`, `true` and `false`. Waves write disjoint pairs and read the others, and a
  barrier separates waves.
* The node-constraint cache (§5.4) is sharded. Per-thread scratch (arc buffers, count
  vectors, derivative arenas) is reused within a chunk.
* A single huge neighbourhood, such as one node with 1M `foaf:knows` arcs, is evaluated
  by one thread. Its candidate computation reads only the typing, so Phase 3 may split
  Step 1 of §5.5 over chunks of arcs when |A| > 64k, if measurements call for it.

### 5.8 Reasons (explain mode)

* The evaluation in §5.6 is boolean and builds no messages. For each **nonconformant**
  result of the fixed map, an explain pass re-evaluates the pair once against the final
  typing and records the first failures as `ShexFailure` values (up to 8).
  * A failing reference is reported as `reference`. The referenced pair's own reason is
    not expanded, which keeps reasons bounded and acyclic.
  * `reason` is a one-line rendering, for example
    `ex:alice: 0 foaf:name arcs, foaf:name xsd:string {1,1} needs 1`, or
    `ex:o3: CLOSED shape forbids ex:mayor ex:joe`.
* Explain runs in parallel over the nonconformant results. It is skipped for results
  beyond `max_results` or the report limit of the guard.

### 5.9 Budgets, timeouts, cancellation

* `check_limits()` (timeout and cancellation) runs every 256 pair evaluations and
  between waves, as in the SHACL `Engine`.
* `max_pairs` (default 10M, or `--query-memory-mb` ÷ 64 bytes per pair, whichever is
  smaller) bounds the typing graph. Exceeding it is the `validation-work` budget error.
* `max_partitions` bounds the distributions tested per pair evaluation (default 100k,
  §5.5 Step 4).
* `max_results` works as `shacl::max_results(&limits)` and counts reported results. With
  `results=nonconformant`, only the nonconformant ones count.

## 6. Design sketch

### 6.1 Crate `sparkles-shex`

```
crates/sparkles-shex/
  src/lib.rs          public API (§2.6)
  src/ast.rs          ShExJ-shaped AST (serde: ShExJ in/out; 2.1 `shapes: [shapeExpr with id]`)
  src/shexc/lexer.rs  hand-written lexer (IRIREF, PNAME_*, BLANK_NODE_LABEL, strings with \u escapes,
                      numbers, LANGTAG, REGEXP, CODE `%iri{…%}`, comments)
  src/shexc/parser.rs recursive descent (ShExC 2.1 grammar; errors with line/column and expected tokens)
  src/shexc/writer.rs ShExC pretty printer (round-trip tests)
  src/shapemap.rs     compact + JSON shape maps; result map writers
  src/resolve.rs      Resolver, FileResolver, import closure, externs
  src/check.rs        S1–S6, dependency graph, Tarjan SCCs, strata
  src/compile.rs      IR (§5.2), per-snapshot NcPlan
  src/nc.rs           node constraints on ids (§5.4)
  src/matcher.rs      §5.5: flat fast path, derivatives, class distributions, budget
  src/typing.rs       §5.6 discovery and refinement
  src/explain.rs      §5.8
  src/semact.rs       SemActHandler registry, Test extension
  src/report.rs       JSON / ShapeMap JSON / smap / text
  src/guard.rs        Phase 2: ShexGuard (CommitGuard)
  examples/bench.rs   §9
  tests/shextest.rs   §8
  tests/known-failures.txt
```

* **Dependencies:** `sparkles`, `oxrdf`, `oxiri`, `oxilangtag`, `oxsdatatypes`, `rayon`,
  `rustc-hash`, `regex`, `serde`, `serde_json`, `anyhow`, `thiserror`, `tracing`,
  `smallvec`. All are already in the workspace lock, so no third-party crates are added.
* **Parser.** A hand-written recursive-descent parser was chosen over `peg`, which is
  already in the tree through spargebra. It gives precise error positions and stays
  reusable by a future ShExC formatter ([X02](X02-formatter.md)). The ShExC grammar is LL(1) apart from a
  few one-token lookaheads.
* **Server feature.** `sparkles-server` gets the feature `shex = ["dep:sparkles-shex"]`,
  on by default. The SHACL feature is independent.

### 6.2 Shared code moved out of `sparkles-shacl`

ShEx needs the same data-graph access and XSD checks, so they move to `sparkles`.
`sparkles-shacl` re-exports them, so the SHACL code does not change:

* The `GraphSel` part of `sparkles-shacl/src/data.rs` moves to
  `sparkles::validation::DataGraph`, together with `objects`, `subjects`, `out_edges`,
  `subjects_of` and `objects_of`. The SHACL-specific subclass cache stays in
  `sparkles-shacl`.
* `sparkles-shacl/src/xsd.rs` moves to `sparkles::xsd`.
* New snapshot helpers:
  * `Snapshot::term_kind(id)` and `Snapshot::iri_prefix_range(prefix)` (§5.4);
  * `sparkles::sparql::expr::lang_matches` becomes `pub`.
* `sparkles-server/src/shacl.rs` keeps `DataGraph::parse`, `validate_options`,
  `max_results` and `pool()`. The ShEx server code reuses them through a small
  `ValidationInputs` struct.

### 6.3 Server plumbing

* `http.rs` gains `async fn shex(...)`, structured like `shacl()`:
  1. parse the schema;
  2. compile it with a `FileResolver` built from `--load-dir`, the request's inline
     imports and `OutboundPolicy`;
  3. expand the map on `ds.store.snapshot()`;
  4. validate;
  5. negotiate the format.

  The route goes next to `/{ds}/shacl`.
* `main.rs` gains `Cmd::Shex { cmd: ShexCmd::Validate{…} | ShexCmd::Parse{…} }` with the
  aliases of §2.5. The module doc line becomes "`shex` ≈ jena `shex validate|parse`".
* `routes.rs`, `ratelimit.rs`, `obs.rs` and the `DatasetInfo` endpoints change as §2.9
  describes.
* Docs:
  * `docs/API.md` gains a "ShEx validation" section next to "SHACL validation";
  * the README feature table gains a ShEx row, its "Shape languages" gap row changes, and
    the Jena→Sparkles table gets `ShexValidator` → `sparkles_shex::validate`;
  * AUDIT §5 drops ShEx.

### 6.4 Guard (Phase 2)

* `sparkles_shex::guard::ShexGuard` implements `CommitGuard`. It reuses [C10](C10-write-time-validation.md)'s
  `Candidate`, `ValidationSummary`, baseline, counters and relevance skip. The C10
  config helpers (atomic write, baseline handling) are factored into a shared
  `sparkles::guard::config` module.
* `sparkles::store` reads only `mode` for `guard_required`, as before. A new
  `sparkles::guard::config_language(root) -> Language` lets the server and CLI call
  either `sparkles_shacl::guard::install` or `sparkles_shex::guard::install`.
* **Cached state.** The guard keeps the `CompiledSchema` and the parsed query map. The
  per-snapshot `NcPlan` is rebuilt on every write. That takes microseconds, because it
  only resolves predicates and value-set ids.

## 7. rudof: depend, reuse, or neither

rudof (github.com/rudof-project/rudof, Jose Emilio Labra Gayo et al.) is the main Rust
ShEx implementation. It is licensed MIT OR Apache-2.0, as its workspace `Cargo.toml` and
its `LICENSE-MIT` and `LICENSE-APACHE` files state. Its latest version is 0.3.24
(2026-09-27). It is the most conformant implementation found. Measured against shexTest
main `fc784a9` during this spec's research:

* 1305/1309 validation tests pass with ShExC schemas, and 1183/1195 with ShExJ;
* 421/444 representation tests pass;
* negative syntax and structure tests are `todo!()` in its harness.

| Criterion | Finding |
|---|---|
| License | MIT OR Apache-2.0, compatible with Sparkles (Apache-2.0). Reading its code and using it in tests is allowed. |
| Dependency weight | `shex_validation` and `shex_ast` resolve to 234 crates. On native targets, `rudof_rdf` always pulls in `oxigraph`, `tokio` "full" and `reqwest`/rustls (through `rudof_iri`), even with default features off. The Sparkles server already uses tokio, but the library crates do not, and none of them needs a second RDF store. |
| Fit with snapshots | The validator needs `S: NeighsRDF + QueryRDF` (`validator.rs:129`, `engine.rs:81`). It works on term types, so every arc would be decoded from ids into `oxrdf` terms, and the value-set and id-range fast paths of §5.4 would be lost. `QueryRDF` is needed because rudof resolves triple-pattern selectors through SPARQL. |
| Robustness | `todo!()` panics remain in the engine (negative atoms, `engine.rs:121,136`) and in shape-map selector paths. A panic in a server request is unacceptable. The debug build's recursive descent overflowed an 8 MB stack on the suite. Semantic actions on EachOf/OneOf are dropped (`ast2ir.rs:532,562`). |
| API stability | Still 0.x. Crates and functions are renamed or merged every few months (`srdf`→`rudof_rdf`, `iri_s`→`rudof_iri`, `shex_compact` and `shapemap`→`shex_ast`, `validate_shapemap2`→`validate_shapemap`), and there are several releases a week. |
| Scope | It includes ShEx 2.2 `EXTENDS`/`ABSTRACT`, which Sparkles does not need yet. |

**Recommendation:** depend on neither rudof's validator nor its parser, since `shex_ast`
alone still pulls in `rudof_rdf`. Implement `sparkles-shex` natively over store ids. Use
rudof in two permitted ways, neither of which links it:

1. **Differential oracle (development only).** `scripts/shex-diff.sh` runs
   `rudof shex-validate` (a binary installed by the developer, never a Cargo dependency)
   and `sparkles shex validate --format shapemap` on generated data and schemas, and
   compares the result maps. It runs in `bench:shex --compare-rudof`, not in CI.
2. **Design reference.** Its published algorithm notes, such as the derivative-based RBE
   matcher and the grouping of values by eligible buckets, agree with the papers cited
   in §3.1. This spec's §5.5 is derived from those papers. If an implementer reads rudof
   source while building the matcher, the provenance entry must say so. No code is
   copied.

Revisit if rudof reaches 1.0 with an optional, store-agnostic core: no `oxigraph` or
`tokio`, an id-level neighbourhood trait, and no panics.

## 8. Conformance testing

**Suite location.** As with the SHACL suites, the suite comes from the Jena checkout's
vendored copy (`jena-shex/src/test/files/spec`) or from `SPARKLES_SHEX_TESTS`. The copy
is shexTest, under the W3C Software and Document License according to its README. It
has:

* `syntax/` (425 `.shex`);
* `negativeSyntax/` (99);
* `negativeStructure/` (14 plus a manifest);
* `schemas/` (425 `.shex`, 422 `.json`, 423 `.ttl`, `manifest.jsonld`);
* `validation/manifest.ttl` (1115 entries: 595 `sht:ValidationTest`, 520
  `sht:ValidationFailure`).

`SPARKLES_SHEX_TESTS` may instead point at an upstream shexTest checkout (main: 1309
validation, 484 representation, 105 negative syntax, 19 negative structure). The harness
handles both layouts. Nothing is vendored into Sparkles, and the tests are skipped
without a suite, as the SHACL suites are.

**Harness** (`crates/sparkles-shex/tests/shextest.rs`, `mise run test:shex`):

| Group | Check |
|---|---|
| syntax | Every `syntax/*.shex` (Jena layout) or `schemas/*.shex` parses. Jena's two exclusions for ill-formed surrogates are listed with reasons. |
| negativeSyntax | Every file fails to parse. |
| negativeStructure | Every file parses, and `compile` fails with a `SchemaError` (S1–S6). |
| representation (`schemas/manifest.jsonld`) | ShExC → AST → ShExJ equals the `.json` file after normalizing key order, number forms and defaulted `min`/`max`. Upstream 2.next `ShapeDecl` wrappers without `abstract` are unwrapped. ShExJ → ShExC → ShExJ is stable. |
| validation (`validation/manifest.ttl`) | For each entry, the data is loaded into an in-memory store (`Store::in_memory`). The schema is run twice, once from ShExC and once from `.json`. Supported forms are `shape` + `focus`, `map` (JSON), `semActs`, `shapeExterns` and `extensionResults`. The result must match the test type, and the Test extension's prints must equal `mf:extensionResults`. |

**Told blank nodes.** These tests carry the traits `sht:ToldBNode` and `LexicalBNode`.
Jena skips about 51 of them.

* The harness parses the data with `oxttl` itself and inserts it through a `WriteTxn`.
  It keeps a map from each parsed label to the store blank node's id, and resolves
  `sht:focus _:label` through that map.
* This needs the store to keep a stable label → id mapping for the terms a transaction
  inserts, for the duration of the test. If that is impossible, these tests go to the
  known-failures list with this reason.

**Exclusions by trait**, not counted as failures:

* `sht:Extends`, `sht:Abstract`, `sht:ExtendsDiamond` (2.2);
* `validation-contrib`;
* `mf:proposed` / `mf:Proposed` entries, which are reported but non-blocking.

`tests/known-failures.txt` uses the SHACL harness's format: one test id per line with a
reason. The run fails on unlisted failures and reports listed tests that pass.

**Targets:**

| Phase | negativeSyntax | negativeStructure | validation (approved, ShExC) | validation (ShExJ) | representation | ShExR |
|---|---|---|---|---|---|---|
| 1 | 100 % | 100 % | ≥ 99 % (known failures listed with reasons; aim 100 %) | ≥ 99 % | ≥ 95 % | – |
| 2 | 100 % | 100 % | 100 % | 100 % | ≥ 98 % | ≥ 95 % (`.ttl` schemas through ShExR import) |

**Other tests:**

* **Unit tests** cover every node-constraint facet, using shexTest's facet tables. The
  `facets.ods` and `facet-tests.ods` sheets are the reference, but they are not parsed.
* **Matcher property tests** use proptest, which is already in the workspace. They run
  the derivative matcher on random single-occurrence expressions and bags, and compare it
  with a brute-force partition enumerator that takes ≤ 8 arcs. Random ambiguous shapes
  are checked against the same oracle.
* **Typing property tests** run random recursive schemas, with positive and stratified
  negative references, on random graphs of ≤ 30 nodes. They check refinement against a
  naive greatest-fixed-point computation over all pairs.
* **Server tests** in `http/shex_tests.rs` follow `validation_tests.rs` and cover
  formats, errors, budgets and auth.

## 9. Performance targets and benchmark

**Benchmark.** `mise run bench:shex [people]` runs
`crates/sparkles-shex/examples/bench.rs` on `scripts/gen-data.py` data. It mirrors
`bench:shacl` and prints:

* load time;
* parse and compile time;
* map expansion;
* validation, sequential and parallel (median of 5);
* result counts by reason kind.

`--compare-rudof` (§7) and `--compare-jena FILE` (Jena's text report) are optional
equivalence checks. These are the SHACL bench shapes written in ShEx:

```shex
PREFIX ex: <http://example.org/>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>

# ShEx has no rdfs:subClassOf inference: class membership is spelled out as value sets.
ex:Person {
  a [ex:Employee ex:Researcher ex:Manager ex:Student] ;
  foaf:name xsd:string ;
  foaf:age xsd:integer MININCLUSIVE 18 MAXINCLUSIVE 65 ? ;
  ex:salary xsd:decimal MINEXCLUSIVE 0 ? ;
  foaf:knows IRI @ex:Person * ;            # recursive: one SCC over the knows graph
  ex:colleagueOf IRI * ;
  ex:authorOf @ex:Document ? ;
  ex:worksFor @ex:Org ? ;
  ex:advisor { a [ex:Researcher] } ?
}
ex:Document {
  a [ex:Article ex:Book] ;
  ex:title [@en] PATTERN "^On the theory" ;
  ex:year xsd:integer MAXINCLUSIVE 2025 ;
  ex:cites @ex:Document *                  # recursive
}
ex:Org CLOSED {
  a [ex:University ex:Company] ;
  foaf:name . ;
  ex:city ["Kyoto" "Paris" "Berlin" "Boston" "Zurich" "Toronto" "Freiburg" "London"] ;
  ex:founded xsd:date
}
```

The file is `crates/sparkles-shex/examples/bench.shex`. It keeps the constraint mix of
the SHACL shapes where ShEx can express it and drops what it cannot:

* the sequence path `(foaf:knows ex:worksFor)` with maxCount 4;
* `sh:uniqueLang`;
* `sh:class` with subclass inference, which is replaced by value sets.

The shape map:

```
{FOCUS a ex:Researcher}@ex:Person, {FOCUS a ex:Manager}@ex:Person,
{FOCUS a ex:Employee}@ex:Person, {FOCUS a ex:Student}@ex:Person,
{FOCUS a ex:Article}@ex:Document, {FOCUS a ex:Book}@ex:Document,
{FOCUS a ex:University}@ex:Org, {FOCUS a ex:Company}@ex:Org
```

The data contains deliberate failures, as in the SHACL bench: ages up to 80, the
cities Austin and Oslo, years up to 2026, and advisors who are not researchers. The
recursive `foaf:knows @ex:Person` makes almost all persons one strongly connected typing
component. Failing ages then propagate through `knows` in refinement waves, which
exercises §5.6.2 at scale.

**Targets** (release build, the machine of `docs/BENCHMARKS.md`, `bench:shex 100000` ≈
1.05M triples):

| Measure | Target | Basis |
|---|---|---|
| full validation, parallel | ≤ 350 ms, i.e. ≤ 2× SHACL's 164 ms on the same data | SHACL bench |
| full validation, sequential | ≤ 1.5 s (SHACL 741 ms) | |
| refinement waves for the knows SCC | ≤ 50, reported by the bench | |
| single node, non-recursive shape (`validate_node`) | p50 < 0.2 ms | flat path: a few prefix scans |
| single node with the recursive Person shape | ≤ full validation time (the reachable component is the graph) | inherent |
| pathological ambiguity: `{ ex:p [1 2] ; ex:p [2 3] ; ex:p . * }` on a node with 1,000 `ex:p` arcs | completes, or fails with `validation-work`, in < 1 s | the budget |
| memory | ≤ 64 bytes per discovered pair, plus the edge vectors | §5.9 |

The results go into `docs/BENCHMARKS.md` next to the SHACL row. Phase 2 adds
`bench:shex-write`, which reuses `bench:shacl-write` with `--lang shex`. Its target is the
one in [C10 §6.1](C10-write-time-validation.md): `reject` latency ≤ standalone
validation + 10 ms.

## 10. Phasing

Estimates are one engineer's working days.

**Phase 1: ShEx 2.1 on demand (about 14 days)**

| Work | Days |
|---|---|
| AST, ShExJ serde in and out, ShExC writer | 2 |
| ShExC lexer and parser with positioned errors; 2.2 detection | 3 |
| Resolver (files, inline, outbound), imports, externs; checks S1–S6, SCCs, strata | 1.5 |
| IR and per-snapshot compilation; node constraints on ids (§5.4); moving shared code (§6.2) | 1.5 |
| Matcher: flat path, derivatives, class distributions, budget, semantic actions (Test extension) | 2 |
| Typing: discovery, pre-check, parallel refinement; explain | 1.5 |
| Shape maps (compact and JSON, selectors), report writers | 1 |
| `/{ds}/shex`, `sparkles shex validate\|parse`, routes, auth, rate limit, metrics; API.md, README, AUDIT | 1.5 |
| shexTest harness and fixes to the targets in §8; property tests; `bench:shex` | 2.5 (overlaps the above) |

**Phase 2: guard, UI, ShExR (about 7 days)**

* Write-time validation with ShEx (§2.7):
  * `validation.json` format 2 with `language`;
  * `ShexGuard`, install dispatch, `/$/validation` and `sparkles validation --lang shex`;
  * `bench:shex-write`.

  2.5 days.
* UI: the SHACL | ShEx switch, the ShEx results table, `api.ts`, Playwright tests.
  2 days.
* ShExR import and export (the ShEx vocabulary, the `schemas/*.ttl` representation
  tests). 2 days.
* The `SPARQL """…"""` selector. 0.5 days.

**Phase 3 (about 6–8 days; each item independent)**

* **Incremental guard validation.** This is [C10 §6.2](C10-write-time-validation.md)
  specialized to ShEx, where each triple constraint is one hop. For a change (s, p, o),
  the affected pairs are s for outgoing p and o for
  inverse p in the shapes that mention p. These propagate up the reverse pair-dependency
  edges and are unioned with the selector delta. `CLOSED` reads `*`. The F1–F8 fallbacks
  carry over. 3 days.
* **ShExR schemas stored in named graphs** for the guard (`schema.graphs`), validated
  when they change, like C10 §4.4. 1 day.
* **MCP tools** `validate_shacl` and `validate_shex` ([C11](C11-mcp-server.md)), returning the compact JSON
  reports with result limits. 1 day.
* **The interval matcher** for deterministic shapes, and splitting huge neighbourhoods
  across threads (§5.5, §5.7), if the benchmarks call for them. 1–2 days.
* **ShEx 2.2** `EXTENDS`/`ABSTRACT`, if the maintainer wants them (Open question 11). About 3
  days.

## 11. Acceptance examples

Setup: `sparkles serve --data data` with dataset `ds`. The prefixes are
`ex: <http://ex.org/>`, `foaf:` and `xsd:`. The data is:

```turtle
ex:alice a ex:Person ; foaf:name "Alice" ; foaf:age 30 ; foaf:knows ex:bob .
ex:bob   a ex:Person ; foaf:name "Bob" ; foaf:knows ex:alice .
ex:carol a ex:Person ; foaf:age 200 .
ex:acme  a ex:Org ; foaf:name "ACME" ; ex:city "Paris" ; ex:mayor ex:bob .
```

and the schema `s.shex`:

```shex
PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
start = @ex:Person
ex:Person EXTRA a { a [ex:Person] ; foaf:name xsd:string ; foaf:age xsd:integer MAXINCLUSIVE 150 ? ; foaf:knows @ex:Person * }
ex:Org CLOSED { a [ex:Org] ; foaf:name . ; ex:city ["Paris" "Kyoto"] }
```

**B1: endpoint, default JSON.**
`curl --data-binary @s.shex -H 'Content-Type: text/shex' 'localhost:3030/ds/shex?map={FOCUS a ex:Person}@ex:Person,ex:acme@ex:Org'`,
with the map URL-encoded, returns `200`:

* `conforms: false`;
* `counts: {conformant: 2, nonconformant: 2}`;
* results in map order: alice ✓, bob ✓, carol ✗, acme ✗.

carol's `appinfo.failures` contains
`{kind:"cardinality", predicate:"…foaf/0.1/name", min:1, max:1, count:0}` and
`{kind:"facet", constraint:"MAXINCLUSIVE 150", value:{…200}}`. acme's failure is
`{kind:"closed", predicate:"http://ex.org/mayor"}`. The response carries
`Sparkles-Commit`.

**B2: recursion is a greatest fixed point.** alice and bob know each other, so (alice,
Person) and (bob, Person) depend on each other. Both are conformant. After
`INSERT DATA { ex:bob foaf:age "old" }`:

* bob is nonconformant (`datatype`);
* alice is nonconformant with `{kind:"reference", shape:"ex:Person", value: ex:bob}`.

A unit test checks that `foaf:knows` chains of 200k nodes validate without stack growth.
It runs in a thread with a 256 KB stack.

**B3: negation stratification.** A schema `ex:S { ex:a NOT @ex:S }` gets `400` with
`"negated reference cycle: ex:S -[NOT]-> ex:S"`. `ex:S EXTRA ex:a { ex:a @ex:S }` gets
`400` with `-[EXTRA ex:a]->`.

**B4: EXTRA.** `ex:T EXTRA foaf:knows { foaf:knows @ex:Person }` on a node that knows one
conforming and one nonconforming person is conformant: the nonconforming arc is an
allowed EXTRA arc. Without `EXTRA` it is nonconformant
(`{kind:"extra", predicate:"…knows"}`). With two conforming arcs it is nonconformant in
both cases. The cause is cardinality: a matching arc may not be left in the remainder.

**B5: inverse (R1).** `ex:Child { ^ex:parentOf @ex:Person {1,2} }`. A node with three
incoming `ex:parentOf` arcs from conforming persons is nonconformant (`cardinality`,
`inverse: true`, `count: 3`).

**B6: formats.**

* `format=smap` gives `ex:alice` as `<http://ex.org/alice>@<http://ex.org/Person>`, and
  carol as `…@!<http://ex.org/Person>`.
* `format=shapemap` gives a JSON array of `{node, shape, status, reason, appinfo}` with
  strings.
* `format=text` gives Jena's line format.
* `results=nonconformant` returns 2 results with unchanged `counts`.

**B7: ShExJ.** `sparkles shex parse s.shex --out shexj | sparkles shex parse - --out shexc`
round-trips. POSTing the ShExJ with `Content-Type: application/json` gives the same
report as B1.

**B8: CLI.**

* `sparkles shex validate --loc DB -s s.shex -m map.smap` prints the text report and
  exits 1.
* `sparkles shex v --loc DB --schema s.shex --node ex:alice` validates against START,
  prints `OK` and exits 0.
* With a schema without `start`, the same command exits 2 with "the schema has no start
  shape; give --shape".

**B9: imports and externs.**

* `IMPORT <common>` next to `s.shex` resolves `common.shex` from the CLI.
* On the server, an import of `file:/etc/x.shex` outside `--load-dir` gets `400`
  ("import not allowed"). An import of `http://10.0.0.1/x` gets `400` under the default
  outbound policy.
* `EXTERNAL` without `externs` gets `400`. With `externs`, the result is computed
  normally.

**B10: semantic actions.**

* `%<http://shex.io/extensions/Test/>{ fail("no") %}` on a triple constraint makes the
  match fail.
* An action with extension `<http://ex.org/js>` is ignored, and `warnings` holds one
  entry.
* `semact-trace=true` returns `print` output in `appinfo.prints`.

**B11: budgets.** The ambiguity schema of §9 on 1,000 arcs, with
`max_partitions = 1000`, gets `507` `{"budget":"validation-work"}`, never a
`nonconformant` result. A `timeout=0.001` request on 1M triples gets `408`.

**B12: auth and routing.**

* With `--auth-config` and a token holding only `read` on `ds`, `POST /ds/shex` gets
  `200`. Without access it gets `404`, because the dataset is hidden.
* `/$/metrics` counts `sparkles_requests_total{op="shex"}`.
* `GET /$/datasets/ds` lists `endpoints.shex`.

**B13: guard (Phase 2).** `PUT /$/validation/ds` with
`{"language":"shex","mode":"reject","schema":{"inline":"<s.shex>","format":"shexc"},"shapeMap":"{FOCUS a ex:Person}@ex:Person"}`
gets `409`, because carol does not conform. After carol is fixed, it gets `200`. Then
`INSERT DATA { ex:dave a ex:Person }` gets `422` with:

* `validation.results[0].status: "nonconformant"`;
* `Sparkles-Validation: status=rejected, …, lang=shex, blocking=1, …`.

## 12. Rejected alternatives

* **Depend on rudof.** §7 has the details. It pulls in 234 crates, including a second RDF
  store and network stacks. Its 0.x API changes often, it reads the store by term rather
  than by id, it has `todo!()` panics, and it overflows the stack on deep recursion.
* **Vendor rudof's parser crate only.** `shex_ast` is entangled with `rudof_rdf`,
  `prefixmap` and `rudof_iri`, so it pulls in most of the same dependency tree.
* **Port Jena's `jena-shex` structure.** Its full cartesian product over assignments and
  its set-partition enumeration are exponential without pruning. It has no ShExJ, and its
  recursion has no invalidation (§3.3).
* **Goal-directed recursive validation with hypothesis stacks** (Jena, rudof). The call
  depth equals data chain length, and invalidation of results that depend on a failed
  hypothesis is subtle. The explicit stratified worklist (§5.6) is exact, iterative and
  parallel. The local pre-check recovers most of the early exit.
* **Translate ShEx to SHACL** and reuse `sparkles-shacl`. Partition semantics, EXTRA,
  OneOf and the greatest-fixed-point recursion have no SHACL Core equivalent, and SHACL
  leaves recursion undefined.
* **Translate ShEx to SPARQL.** Recursion and partition matching are not expressible.
* **The interval algorithm as the only matcher.** It is limited to single-occurrence
  expressions with fixed bags, so ambiguous shapes would still need a second matcher.
  Derivatives cover both and are the specification community's reference technique.
  Intervals stay an optional optimization.
* **ShEx 2.2 (`EXTENDS`/`ABSTRACT`) in Phase 1.** It is not a final report, and Jena lacks
  it. It is deferred (Open question 11).
* **Executing other semantic actions** (JavaScript, SPARQL generation, the Map
  extension). They are a security risk on a server, and transformation is out of scope.
* **An RDF report** (an `sh:ValidationReport` lookalike or ShExV). No standard
  vocabulary exists, and a SHACL-shaped report would misstate ShEx semantics. Reports are
  JSON only.
* **ShExC text in a named graph as a literal** for the guard. The guard uses a copied
  file (Phase 2) or a ShExR graph (Phase 3) instead.
* **Running SHACL and ShEx guards together in Phase 2.** It needs a guard chain and a
  combined `validation.json`. It is deferred (Open question 7).
* **`peg` grammar for ShExC.** It gives weaker error positions and is not reusable by a
  formatter.
* **Treating budget overruns as `nonconformant`.** That would turn resource limits into
  false verdicts, and in `reject` mode into false rejections.

## 13. Open questions

1. **Direction-symmetric matchables (R1).** The 2.1 text constrains only outgoing
   remainder arcs, while shexTest requires incoming arcs with inverse-constraint
   predicates to be matched. Default: follow the suite, and apply `EXTRA` to such
   incoming arcs as well.
2. **`EXTERNAL` without definitions.** Should it be an error, or always true as in Jena?
   Default: error. A `--externs-default true|false` flag can be added later if users need
   it.
3. **Test semantic actions on by default?** They are pure, and `fail` changes results.
   Default: on. Unknown extensions are ignored with a warning, as in Jena.
4. **Facet erratum (R2).** Follow the intent and the suite. Default: yes.
5. **CLI shape.** `sparkles shex validate|parse` uses subcommands, as Jena's `shex` does,
   while `sparkles shacl` is flat. Should `sparkles shacl` also gain `validate`/`parse`
   subcommands with the flat form kept? Default: leave SHACL unchanged.
6. **Imports on a server.** Allow `http(s)` imports under the outbound policy, as SPARQL
   `LOAD` does, or refuse them unless `--shex-remote-imports`? Default: the outbound
   policy, as for LOAD (private addresses already blocked).
7. **One guard language per dataset.** A guard chain (SHACL and ShEx on one dataset) is
   deferred. Default: one language.
8. **Budget defaults.** `max_partitions` is 100k per pair evaluation. `max_pairs` is 10M
   or the memory budget ÷ 64. Default: as stated, revisited after `bench:shex`.
9. **ShExJ media type.** Accept the unregistered `application/shex+json` plus sniffed
   `application/json` / `application/ld+json`? Default: yes. Responses that return ShExJ
   (`parse --out shexj`) use `application/json`.
10. **`shex` feature on by default.** It adds no new dependencies. Default: on.
11. **ShEx 2.2 (`EXTENDS`, `ABSTRACT`).** rudof has it and Jena does not. The spec is a
    CG draft. Default: not before Phase 3, and only on request.
12. **`SPARQL """…"""` selectors.** A non-standard extension. Default: Phase 2, run with
    the request's query budgets.
13. **Results for absent nodes.** A fixed-map node that is not in the data has an empty
    neighbourhood, and is conformant when the shape allows nothing. Default: validate as
    the spec says, with no warning. Should the report warn when a selector expands to
    nothing?

## 14. Sources

* **Sparkles repository (read):**
  * `crates/sparkles-shacl/src/{lib.rs,data.rs,validate.rs,guard.rs}` (snapshot access,
    `GraphSel`, parallel chunking, limits, the guard and `validation.json`),
    `crates/sparkles-shacl/examples/bench.rs`, `crates/sparkles-shacl/tests/w3c_shacl.rs`
    and `known-failures.txt`;
  * `crates/sparkles/src/guard.rs` (`CommitGuard`, `Candidate`, `ValidationSummary`),
    `store.rs` (guard slots, `scan`, `count`, `term`, blank-node labels), `index.rs`
    (`Perm`), `id.rs` (tags), `vocab.rs` (`prefix_range`), `error.rs` (`BudgetKind`),
    `sparql/expr.rs` (`compile_regex`, `lang_matches`);
  * `crates/sparkles-server/src/{http.rs,shacl.rs,main.rs,auth/routes.rs,ratelimit.rs,obs.rs,outbound.rs}`;
  * `ui/src/lib/api.ts`, `ui/src/routes/datasets/[name]/+page.svelte` (the SHACL panel);
  * `scripts/gen-data.py`, `mise.toml` (`bench:shacl`, `bench:shacl-write`);
  * `docs/API.md` (SHACL validation, write-time validation), `docs/BENCHMARKS.md` (SHACL
    164 ms / 741 ms), `docs/AUDIT.md`, `README.md`.
* **Existing specs:** [C10](C10-write-time-validation.md) (structure, guard semantics)
  and [PROVENANCE.md](PROVENANCE.md). No other design documents were read.
* **Shape Expressions Language 2.1**, Final Community Group Report, 8 October 2019,
  http://shex.io/shex-semantics/ (fetched): §§2.5, 5.2–5.9, the IANA section. It is
  under the W3C Community Final Specification Agreement, and the reports are under the
  W3C Software and Document License.
* **ShEx editor's draft "2.next"**, https://shexspec.github.io/spec/ (consulted for 2.2
  scope only).
* **ShapeMap Structure and Language**, Draft Community Group Report, 13 July 2017,
  http://shex.io/shape-map/, and the editor's draft at
  https://shexspec.github.io/shape-map/ (fetched): §3 structure, §4 triple-pattern
  semantics, §6 grammar, the JSON form. The repository carries an MIT `LICENSE`.
* **shexTest**, https://github.com/shexSpec/shexTest (main `fc784a9`, tags up to v2.1.0;
  W3C Software and Document License, `W3C-20150513`). It is also used through the copy
  vendored in Apache Jena (`jena-shex/src/test/files/spec`, same license per its README).
  Manifests, traits and counts were inspected. The `doc/ShExJ.jsg` and
  `ShExJ-context.jsonld` files were consulted for ShExJ.
* **Apache Jena `jena-shex` and `jena-cmds`** (Apache-2.0), checkout `b1dcba53b5`
  (2026-09-28). Read for behaviour and CLI surface:
  * `Shex`, `ShexValidator`, `ShexReport`, `ShexStatus`;
  * `parser/ShExC`, `parser/ParserShExC`, `parser/ShExJ`;
  * `eval/*`, `sys/ValidationContext`, `sys/ShexValidatorImpl`, `sys/SysShex`;
  * `semact/*`, `writer/WriterShExC`;
  * `jena-cmds/src/main/java/shex/{shex,shex_validate,shex_parse}.java`;
  * the test runners and exclusions;
  * `jena-fuseki2` (searched; it has no ShEx operation).

  No code was copied.
* **rudof**, https://github.com/rudof-project/rudof (commit `79a9e69`, 2026-09-30;
  crates 0.3.24; **MIT OR Apache-2.0**). Licenses, crate structure, dependency tree
  (built in a scratch consumer crate), trait signatures, documented algorithm and the
  shexTest pass rates were inspected and measured. It is not a dependency. Its
  `feasibility-model.md` credits a port of an Apache Jena fork (fhircat/jena). That
  fork was not consulted.
* **Papers** (citations verified; content per the published PDFs and abstracts):
  * S. Staworko, I. Boneva, J. E. Labra Gayo, S. Hym, E. G. Prud'hommeaux, H. Solbrig,
    "Complexity and Expressiveness of ShEx for RDF", ICDT 2015, LIPIcs 31, pp. 195–211,
    doi:10.4230/LIPIcs.ICDT.2015.195 (CC-BY): Theorems 1, 3–5, 7, 9 and Corollary 6;
  * I. Boneva, J. E. Labra Gayo, E. G. Prud'hommeaux, "Semantics and Validation of Shapes
    Schemas for RDF", ISWC 2017, LNCS 10587, pp. 104–120,
    doi:10.1007/978-3-319-68288-4_7 (extended: arXiv:1404.1270);
  * J. E. Labra Gayo, E. Prud'hommeaux, I. Boneva, S. Staworko, H. Solbrig, S. Hym,
    "Towards an RDF Validation Language Based on Regular Expression Derivatives",
    EDBT/ICDT 2015 Workshops, CEUR-WS Vol-1330, pp. 197–204;
  * E. Prud'hommeaux, J. E. Labra Gayo, H. Solbrig, "Shape Expressions: An RDF Validation
    and Transformation Language", SEMANTiCS 2014, doi:10.1145/2660517.2660523;
  * J. E. Labra Gayo, E. Prud'hommeaux, I. Boneva, D. Kontokostas, *Validating RDF Data*,
    Morgan & Claypool, 2017, doi:10.2200/S00786ED1V01Y201707WBE016 (free HTML at
    http://book.validatingrdf.com; cited, not copied).
* **Cited from general knowledge, not fetched:**
  * RFC 4647 (language ranges), BCP 47;
  * XPath and XQuery Functions 3.1 (`fn:matches`, `fn:starts-with`);
  * RFC 9110;
  * Tarjan's SCC algorithm (1972);
  * Brzozowski derivatives (1964).
* **Not read:** the shex.js and PyShEx sources.

## Appendix: provenance entry (for [PROVENANCE.md](PROVENANCE.md))

This is the entry as drafted with the spec, before implementation. The published entry in
[PROVENANCE.md](PROVENANCE.md) records the implementation.

```markdown
## ShEx validation

- **Spec:** `specs/G02-shex.md`, written on 2026-09-30 from the Sparkles
  code and spec C10; the ShEx 2.1 Final Community Group Report and the ShapeMap draft
  (shex.io; W3C Software and Document License; the shape-map repository is MIT), both
  fetched; the shexTest suite (W3C Software and Document License), upstream and as
  vendored in Apache Jena; Apache Jena's `jena-shex`, `jena-cmds` and `jena-fuseki2`
  sources (Apache-2.0), read for behaviour and CLI surface, no code copied; rudof 0.3.24
  (MIT OR Apache-2.0), inspected for licensing, dependency weight, API and conformance,
  not used as a dependency; and papers by Staworko et al. (ICDT 2015), Boneva, Labra Gayo
  and Prud'hommeaux (ISWC 2017) and Labra Gayo et al. (2015, RBE derivatives).
- **Implementation:** not started.
  - **Planned dependencies:** none new; `sparkles-shex` uses workspace crates only.
  - **Test data:** shexTest is read from the Jena checkout or `SPARKLES_SHEX_TESTS`,
    not vendored.
  - **Development-only:** the rudof CLI as a differential oracle (`bench:shex
    --compare-rudof`), never linked.
- **Rejected** (spec §12):
  - depending on rudof, or vendoring its parser crate;
  - porting Jena's partition-enumeration validator;
  - goal-directed recursive validation with hypothesis stacks;
  - translating ShEx to SHACL or SPARQL;
  - the interval algorithm as the only matcher;
  - ShEx 2.2 in Phase 1;
  - executing semantic actions other than the Test extension;
  - an RDF report format;
  - ShExC literals in named graphs;
  - SHACL and ShEx guards together in Phase 2;
  - a `peg` grammar;
  - treating budget overruns as nonconformant.
```

## Implementation note (2026-10-01): matcher algorithm

The Phase 1 matcher does not use regular-bag-expression derivatives (∂^k). For each
sub-expression it computes the interval of iteration counts that a count vector can be
split into. An EachOf takes the intersection over its parts and a OneOf the sum over its
branches, and then the group's own `{m,n}` applies. The vector matches when the root's
interval contains 1. This is the interval method that §5 had deferred to Phase 3. It is
exact for any expression, and linear in the expression's size whatever the counts.

Property tests check it against a brute-force enumerator that follows the partition
definition of §4.4 literally. The tests cover up to 8 arcs, nested groups, random
cardinalities, EXTRA and ambiguous predicates. Class distributions, the `max_partitions`
budget and semantic-action enumeration are as specified.

## Outcome

**Delivered.** The spec was written on 2026-09-30. Phases 1 and 2 landed on 2026-10-01 as
designed in §2–§6. Phase 1 brought the crate, the endpoint, the CLI, the harness,
`bench:shex` and the rudof differential script. Phase 2 brought the format 2 guard with the
predicate relevance skip, ShExR, SPARQL selectors, the UI switch and `bench:shex-write`.
The MCP tools `validate_shacl` and `validate_shex` were taken early from Phase 3. No new
third-party crates were added.

**Deviations from the spec.**
* **Matcher.** The interval method replaced bag derivatives as the only matcher (see the
  implementation note above). It is exact for every expression, because each triple
  constraint occurs once after `&include` expansion. Its cost is linear in the
  expression's size whatever the counts, while derivatives copy group bodies. The change
  was approved during implementation. The brute-force partition enumerator stays the
  property-test oracle.
* **Told blank nodes.** The store does not keep blank-node labels. The harness therefore
  skips the `LexicalBNode` validation tests, counting them, and lists one more test in
  `known-failures.txt`.
* **`validation.json` is always written as format 2**, with `"language"`, for SHACL
  configurations too. Format 1 files are still read. The maintainer decided this because
  the file is internal to Sparkles and only older Sparkles binaries are affected.
* Node-constraint annotations and semantic actions stay rejected, because ShExJ 2.1 has
  no slot for them. An exhausted outbound budget while fetching imports answers `507`,
  not `400`. Imports are capped at 64 schemas and 16 MiB per validation.

**Decided by the maintainer.**
* The `sparkles_validation_*` series gain a `language` label. The SHACL series become
  `language="shacl"`.
* SPARQL selectors are refused in the write-time guard (`400` when the configuration is
  set) and allowed on demand.
* Server flags for the import limits, and CLI `--outbound-*` flags for imports, are
  deferred.

**Chosen during implementation** (open to revision).
* The SHACL `Sparkles-Validation` header is unchanged, and ShEx adds `lang=shex`.
* Summaries and `GET /$/validation/{ds}` gain an additive `language` field.
* The stored schema is the text as given when it has no imports. Otherwise it is the
  closed schema as ShExJ, with its base and prefixes.
* The PUT body takes no inline imports or externs, so `EXTERNAL` is a `400`.
* An empty shape map is refused.
* SPARQL selectors do not inherit the prefixes of the map or the schema.
* `--schema` and `--shape-map` imply `--lang shex`.
* The MCP tools are on by default. They are read-only, use `maxResults` 20 and refuse
  imports.
* The UI gets no write-time validation panel in this phase. The dataset page's
  Write-time validation panel came later, and it shows ShEx guards as well as SHACL ones.

**Conformance at landing.** Sparkles passes 100 % of the syntax, negative-syntax,
negative-structure, representation and ShExR tests. It passes 99.9 % of the validation
tests from ShExC, ShExJ and ShExR: 42 blank-node-label tests are skipped, and one more is
listed in `known-failures.txt`.
[FEATURES.md](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui) has the
current numbers. SHACL stayed at 98/98 + 20/20 through the shared-code moves.

**Performance.** `mise run bench:shex` and `bench:shex-write` exist, but
[BENCHMARKS.md](../BENCHMARKS.md) publishes no ShEx numbers, so the §9 targets are
unverified.

**Incremental guard validation** landed on 2026-10-02 with C10 Phase 2. The guard keeps
the counts of the head's result map, in memory and in `validation-status.json`. A write
validates the associations of the nodes it can affect, in the states before and after
it, and moves the counts by the difference.
* The affected nodes are found per node rather than per pair. They are the endpoints of
  changed triples whose predicate a triple constraint reads, plus the nodes that reach
  them backwards over triple constraints whose value expression refers to a shape. This
  over-approximates the pair dependencies of the design, and needs no typing of the
  states.
* The map's associations at those nodes are recomputed in each state. Term selectors
  select their node in both, and `{FOCUS p o}` and `{s p FOCUS}` selectors are checked
  against the state's arcs.
* The fallbacks are those of C10 that apply: an unknown state, `reject` on a head that
  does not conform, bulk writes, more than 50,000 affected nodes, and SPARQL selectors,
  which the guard refuses anyway.
* A recursive reference over a large connected graph makes every node that reaches the
  changed one affected. With `foaf:knows @ex:Person *` in `bench:shex-write`, a new
  `foaf:knows` arc between two people reaches more than 50,000 of 100,000 people, so that
  write is validated in full. Propagating only typings that change avoids this. It was
  not built with this step and landed later, as described below.
* At 1.05M triples, the benchmark's other write took 8 to 12 ms with `warn` and
  `reject`, against 2.1 to 2.7 s before.
* A property test checks the counts and the listed associations against full
  validations, over recursive, inverse and negated references with and without a
  `CLOSED` shape.

**Grandfather mode** landed on 2026-10-02 with C10 Phase 3. It has the semantics of
SHACL's: `"baseline": "grandfather"` in the configuration, `--grandfather` in the CLI,
`reject` allowed on data that does not conform, and a write blocked only by the
nonconformant associations it introduces. Associations are matched by node and shape as a
multiset, in the state before the write and in the state after it. Summaries gain
`introduced`, and the new associations are listed first. A full validation in this mode
also validates the state before the write. A schema changes only through a new
configuration, and setting one never compares two schemas, so the rule that a shapes
change compares with the old shapes' results has no ShEx case. The property test runs in
`warn` and in grandfather `reject`, and checks every decision, count and listing.

**Typing-change propagation** landed on 2026-10-02 too. For schemas whose references
follow arcs, the guard keeps the typing of the head in memory: every pair the validations
of the map discovered, with its value. A full validation sets it, and every write moves
it. It is dropped by a restart, a compaction (the generation's terms are numbered anew),
a bypassed write and any commit the guard did not judge. With it, a write is validated
as follows.

1. The region starts with the nodes whose neighbourhood the write changed and the nodes
   a changed selector arc may select.
2. The region's pairs, and the map's associations at its nodes, are typed in the state
   after the write. Every other pair the head's typing has as `true` is read as `true`
   and not discovered. A pair it has as `false`, or does not have, is discovered and
   typed.
3. If a typed pair's value differs from the head's, its node and every node that reaches
   it backwards over referring constraints join the region, and step 2 runs again.
4. When no value outside the region changes, the associations of the region's nodes are
   the only ones whose results the write can change. Their results before the write come
   from the head's typing, and those after it from the last typing.

The result is exact. Every pair outside the region keeps the head's value, which is a
fixed point of the state after the write. It is also the greatest one. A pair outside
the region that the head had as `false` and could turn `true` is read by no pair of the
region, since those are typed, and it reads only unchanged pairs. Setting it `true`
would therefore give a larger fixed point of the state before the write, which
contradicts the head's typing. Negation reads lower strata, which are exact first.
Joining the whole backward reach of a changed pair at once, rather than one reader per
round, keeps a change that runs along a chain to one more typing.

On data that mostly does not conform, typing every `false` pair the region reads soon
reaches most of the graph. The first build did so in `bench:shex-write`'s `warn` mode,
where the person write took 2.6 s. The typing therefore also records which `false` pairs
failed in discovery with every reference unknown, such as a person whose age breaks a
node constraint. Only a change to such a pair's own neighbourhood can make it `true`, so
outside the region it is read as `false` and not typed again. A person who knows one of
them is then decided `false` without going further. The design's plan of a
typing of both states was not needed, because the head's typing stands in for the state
before. The old walk over every node that reaches a changed one remains for writes when
the typing is unknown.

With `foaf:knows @ex:Person *`, a new `foaf:knows` arc between two people who conform
validates one association, where it validated everyone who reaches them, or fell back to
a full validation beyond 50,000 people. A unit test runs the four cases on a ring of
people. The property test now runs mostly through this path and passed 20,000 cases with
the final code, and 50,000 before the `false` pairs were split. shexTest stayed at 99.9 %
of the validation tests.

At 1.05M triples (152,000 associations), on a machine with a load average near 30 from
other builds, `bench:shex-write`'s person write took 16 ms in `reject` over the schema
the data conforms to, 18 ms in `warn` and 28 ms in grandfather `reject` over the schema
it mostly does not conform to. That write was validated in full before, in 2.1 to 2.7 s.
With validation off it took 20 ms. The first such write after a restart is still
validated in full, because the typing is not persisted, unless the server was started
with `--validate`.

**ShExR schemas in named graphs** landed on 2026-10-02, as §10's Phase 3 item described
them. The configuration's `schema.graphs` names graphs of the dataset that hold the
schema in ShExR, instead of a copied file.

* The schema is read from the state being validated. The union of the named graphs'
  triples is parsed as ShExR, checked and compiled, and the shape map is parsed against
  it. A configuration's `schema.prefixes` stand in for the prefixes a graph cannot hold,
  so the map may use them. Nothing is copied into the database, and imports are refused
  because a write never fetches anything.
* The schema graphs are never part of the data graph, and a `dataGraph` list that names
  one is refused.
* A write that changes a schema graph reads the schema it leaves and validates the state
  after it in full, with the fallback reason `schema`. In grandfather mode the state
  before is judged under the old schema, as SHACL compares a shapes change with the old
  shapes' results. The new schema, its shape map and what it reads replace the old ones
  when the write commits, together with the typing of the full validation. A write whose
  schema does not parse, check or define the map's labels is rejected with the reason
  in `shapesError`, in `warn` mode too.
* `sparkles validation` gained `--schema-graph IRI` and `--schema-prefix PREFIX=IRI`, and
  a restart reads the schema from the graphs again.

To swap a schema at commit time, the guard keeps its compiled schema, map, read
predicates and incremental plan together behind a lock, and a pending write carries the
new set. The property test of incremental validation now also runs with the schema in a
graph, and passed 1,500 cases with the final code. `guard::tests::schemas_in_graphs`
covers a schema change, a rejected change and a restart,
`http::validation::shex_tests::schema_in_a_graph` the HTTP side, and
`tests/cli_validation.rs` the CLI.

**Deferred or rejected.** SHACL and ShEx guards on one dataset, a persisted typing, a
schema in graphs merged with a file, and ShEx 2.2 (`EXTENDS`, `ABSTRACT`) are not
built.
