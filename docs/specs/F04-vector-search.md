# F04: Vector similarity search

> **Status:** implemented in part
>
> **Phases:** Phase 1 (`spk:vector` literals, the similarity functions, exact top-k
> `spk:vectorSearch` scoped to the active graph, with the delta overlay and a memory
> budget) shipped, plus a minimal part of Phase 1b: the read-only status route
> `GET /$/vector/{ds}`, the budget flag and "Similar" in the UI's explorer. Configured
> indexes, persisted segments, `candidates:join`, variable queries, the `sparkles vector`
> CLI, Phase 2 (HNSW) and Phase 3 are not built.
>
> **User docs:** [API: Vector similarity](../API.md#vector-similarity) · [Features](../FEATURES.md#sparql-arq-equivalent)
>
> This is the design as written before implementation; the [Outcome](#outcome) section at
> the end records how it landed.

It depends on the commit-identity work ([CI](CI-commit-identity.md): durable `seq`), the
budgets of [C01](C01-observability-and-budgets.md), and optionally
[F03](F03-full-text-search.md) for hybrid text + vector queries.

## 1. Summary, goals, non-goals

Embeddings are stored as ordinary RDF literals of a Sparkles datatype,
`"[0.1, 0.2, 0.3]"^^spk:vector`. SPARQL can then (a) compute similarities in
expressions (`spk:cosine`, `spk:dot`, `spk:euclidean`) and (b) run a top-k nearest
neighbour search as a property function, `(?s ?score) spk:vectorSearch (ex:emb "[…]"^^spk:vector 10)`.
The results join with the rest of the graph pattern like any other solution sequence.
The literal stays authoritative. Packed `f32` matrices and HNSW graphs are derived
data: rebuildable, watermarked, and never on the commit path.

**Goals**
- Exact top-k search that is correct under MVCC (every snapshot sees exactly its own
  vectors) and scoped to the query's active graph. It is also the recall oracle for ANN.
- Approximate search (HNSW) for the head generation behind an optional cargo feature.
  Its scores are identical to the exact path because candidates are re-scored with the
  same kernel.
- Explicit configuration: an index is bound to a predicate, a dimension, a metric and an
  optional model label. Vectors of another dimension never enter it.
- Memory accounting: a budget, per-index byte reporting, and admission errors instead of
  OOM kills.
- Admin HTTP, CLI and UI surfaces: status, create, rebuild, and a similarity panel.

**Non-goals**
- Computing embeddings (no model runtime). Clients bring vectors.
- Sparse vectors, binary/Hamming metrics, multi-vector (late-interaction) scoring.
- Value-level equality or ordering of `spk:vector` literals (`=` stays RDF-term equality).
- Distributed or GPU search.
- Hybrid ranking fusion with F03. Sections 6 and 9 only reserve room for it.

## 2. User-visible behavior

Namespace: `PREFIX spk: <urn:x-sparkles:>` (justified in §4.1). Terms:

| IRI | Kind |
|---|---|
| `spk:vector` | datatype |
| `spk:cosine`, `spk:dot`, `spk:euclidean`, `spk:dimension` | functions |
| `spk:vectorSearch` | property function (reserved predicate) |

### 2.1 Data

```turtle
@prefix ex:  <http://example.org/> .
@prefix spk: <urn:x-sparkles:> .
ex:doc1 ex:embMiniLM "[0.0132, -0.2210, 0.0871]"^^spk:vector .
ex:doc1 ex:embMiniLM "[0.0101, -0.2000, 0.0900]"^^spk:vector .   # a second embedding (e.g. a second chunk)
```

Vectors load through every existing path (bulk load, GSP, INSERT DATA, upload). A
literal whose lexical form is not a valid vector is still stored (RDF 1.2 §3.4.2:
implementations SHOULD accept ill-typed literals). Search skips it, and index status
counts it under `skipped.malformed`.

### 2.2 SPARQL functions (usable in FILTER, BIND, SELECT expressions, ORDER BY, aggregates)

| Function | Result |
|---|---|
| `spk:cosine(?a, ?b)` | `xsd:double` in [-1, 1] |
| `spk:dot(?a, ?b)` | `xsd:double` |
| `spk:euclidean(?a, ?b)` | `xsd:double` ≥ 0 (L2 distance, not squared) |
| `spk:dimension(?a)` | `xsd:integer` |

Arguments must be well-typed `spk:vector` literals. These cases are SPARQL type errors
(unbound in BIND, false in FILTER, per SPARQL 1.1 §17.2 / §17.6):
- the argument is not an `spk:vector` literal, or it is ill-typed;
- the two vectors differ in dimension;
- cosine where either vector has zero norm;
- any non-finite intermediate result.

`STRDT("[1,2]", spk:vector)` builds a vector from a string.

### 2.3 Top-k search: `spk:vectorSearch`

```sparql
PREFIX spk: <urn:x-sparkles:>  PREFIX ex: <http://example.org/>
SELECT ?s ?score WHERE {
  (?s ?score) spk:vectorSearch (ex:embMiniLM "[0.01, -0.2, 0.09]"^^spk:vector 10) .
  ?s ex:title ?title .
} ORDER BY DESC(?score)
```

- **Subject**: `?s`, `(?s)`, `(?s ?score)` or `(?s ?score ?vector)`. `?vector` binds the
  matched stored literal, which makes the result unambiguous when an entity has several
  embeddings. A constant in any position acts as an equality constraint on the output.
- **Object list**: `(predicate query [k] ["option:value" …])`.
  - `predicate`: an IRI naming the embedding predicate.
  - `query`: one of:
    - an `spk:vector` literal;
    - an IRI or blank-node label naming an entity. The query vector is then that
      entity's single vector under `predicate` within the active graph. It is an error
      if the entity has more than one; with none the result is empty;
    - a variable (Phase 1b; see §4.5).
  - `k`: a positive `xsd:integer`, default 10, maximum `max_k` (default 10 000).
  - options: string literals of the form `"key:value"`. The keys are:

    | Option | Values | Phase |
    |---|---|---|
    | `metric:` | `cosine`, `dot`, `euclidean` | 1 |
    | `exact:true` | force the exact path | 2 |
    | `ef:N` | HNSW expansion for this query | 2 |
    | `candidates:join` | restrict the search to join candidates | 1b |
    | `distinct:subject` | at most one row per entity | 1b |

- **Output**: one solution per selected stored quad `(s, predicate, o, g)`:
  - `?s` = s;
  - `?score` = the score as an `xsd:double`;
  - `?vector` = o;
  - under `GRAPH ?g { … }`, `?g` = g.
- **Score direction**: cosine and dot are similarities (higher is better). Euclidean is
  a distance (lower is better). "Top-k" means the k best in the metric's direction.
- **Order**: the operator emits rows best-first, but SPARQL result order is only defined
  by ORDER BY. Queries (and the UI) use `ORDER BY DESC(?score)`, or `ORDER BY ?score` for
  euclidean.
- **Scope**: top-k is computed over the rows in the pattern's active graph. The
  candidates are the default graph (after `FROM`, `default-graph-uri`,
  `--union-default-graph`, `reasoning=`), `GRAPH <g>` or `GRAPH ?g`. Graph scoping is
  never applied by post-filtering a global top-k. Other patterns in the group join with
  the k rows afterwards, so `?s a ex:Doc` can reduce the result below k. `candidates:join`
  (Phase 1b) inverts this, see §4.5.
- **Placement**: the pattern works in any group, including OPTIONAL, MINUS, EXISTS,
  subqueries and GRAPH. It is not rewritten inside SERVICE, whose text is sent to the
  remote endpoint verbatim.

### 2.4 HTTP (Phase 1b unless noted)

| Method | Path | Description |
|---|---|---|
| GET | `/$/vector/{ds}` | `VectorStatus`: budget, usage, all configured indexes and implicit partitions |
| GET | `/$/vector/{ds}/{name}` | one `VectorIndexStatus`; `404` if unknown |
| PUT | `/$/vector/{ds}/{name}` | create or replace the configuration (JSON below). `201` when created, `200` when replaced. The body is `{ index: VectorIndexStatus, task: Task }`, and the build runs as a task of kind `vector` |
| DELETE | `/$/vector/{ds}/{name}` | drop the configuration and its derived files; `204` |
| POST | `/$/vector/{ds}/{name}/rebuild` | discard the derived files and rebuild; returns `Task` |
| POST | `/$/vector/{ds}/{name}/recall?samples=100&k=10` | Phase 2: measures ANN recall@k against the exact oracle, using stored vectors as queries; returns a `Task` whose message holds the result |

```ts
type VectorIndexConfig = {
  predicate: string;                  // IRI
  dimension: number;                  // 1..16384
  metric: "cosine" | "dot" | "euclidean";
  model?: string;                     // label only (see §4.2)
  ann?: null | { quantization?: "f32" | "f16" | "i8"; connectivity?: number;
                 expansionAdd?: number; expansionSearch?: number };  // Phase 2
};
type VectorIndexStatus = {
  name: string | null;                // null = implicit partition (no configuration)
  predicate: string; dimension: number; metric: string; model?: string;
  state: "ready" | "queued" | "building" | "over-budget" | "failed";
  progress?: number; message?: string;
  rows: number;                       // vectors in the base segment
  overlay: { inserts: number; deletes: number };           // served from the snapshot delta
  skipped: { malformed: number; wrongDimension: number; zeroNorm: number };
  watermark: { generation: string; baseSeq: number | null }; headSeq: number | null;
  memory: { segmentBytes: number; annBytes: number; overlayBytes: number;
            residency: "heap" | "mmap" };
  ann: null | { backend: string; state: "ready" | "building" | "failed" | "unavailable";
                quantization: string; connectivity: number; expansionSearch: number;
                recall?: { k: number; value: number; samples: number; at: string } };
  builtAt?: string; buildMs?: number;
};
type VectorStatus = { budgetBytes: number; usedBytes: number; indexes: VectorIndexStatus[] };
```

Errors use the existing `{ error, detail? }` body:
- `400`: invalid configuration (dimension out of range, unknown metric, invalid ANN
  parameters).
- `403`: mutating endpoints on a `--read-only` server.
- `404`: unknown dataset or index.
- `409`: the predicate is already indexed under another name.
- `501`: `ann` requested in a build without the `vector-hnsw` feature.

`DatasetStats` gains `vector: { indexes: number; usedBytes: number; budgetBytes: number }`.

### 2.5 CLI (Phase 1b)

```sh
sparkles vector status  --loc db                          # table of VectorIndexStatus
sparkles vector create  --loc db --name minilm --predicate http://example.org/embMiniLM \
                        --dim 384 --metric cosine [--model all-MiniLM-L6-v2] [--ann f16]
sparkles vector drop    --loc db --name minilm
sparkles vector rebuild --loc db --name minilm
sparkles vector search  --loc db --predicate IRI (--vector '[…]' | --entity IRI) [-k 10]
                        [--metric cosine] [--graph default|union|IRI] [--exact] [--results text|json|tsv]
```

`search` is a thin wrapper that generates the §2.3 SPARQL, so it runs the same code path.
The global flag `--vector-mb N` (default 4096) sets the memory budget for `serve` and all
commands.

### 2.6 Rust library API

```rust
pub mod vector {                                   // crates/sparkles/src/vector/
    pub const DATATYPE: &str = "urn:x-sparkles:vector";
    pub fn parse(lex: &str) -> Result<Vec<f32>, VectorError>;   // §4.1 grammar
    pub fn canonical(v: &[f32]) -> String;                      // §4.1 canonical form
    pub fn literal(v: &[f32]) -> oxrdf::Literal;
    #[derive(Clone, Copy)] pub enum Metric { Cosine, Dot, Euclidean }
    pub fn score(m: Metric, a: &[f32], b: &[f32]) -> Option<f32>; // §4.3 kernel
}
impl Dataset {                                    // Phase 1b
    pub fn vector_indexes(&self) -> Vec<VectorIndexStatus>;
    pub fn create_vector_index(&self, name: &str, cfg: VectorIndexConfig) -> Result<()>;
    pub fn drop_vector_index(&self, name: &str) -> Result<()>;
    pub fn rebuild_vector_index(&self, name: &str) -> Result<()>;   // blocking
}
```

The query builder needs nothing new: `SelectBuilder::where_("(?s ?score)", "spk:vectorSearch", "(ex:emb \"[…]\"^^spk:vector 10)")`
already emits the text. A typed helper is optional.

### 2.7 UI (SvelteKit, Phase 1b)

- **New route `/similar`**, nav item "Similar" between Explore and Datasets, dataset from
  `app.current`.
  - **Left panel, query input.**
    - Index picker, filled from `GET /$/vector/{ds}`, with predicate, dimension, metric
      and state badge.
    - Query mode toggle:
      - *Entity*: IRI input with prefix-aware completion. It uses a label lookup through
        SPARQL, like the explorer does.
      - *Vector*: textarea with live client-side validation using the §4.1 grammar. It
        shows the parsed dimension, or the first error with its offset, and flags a
        dimension mismatch with the index.
    - k (1–100, default 10), metric (defaults to the index metric), graph (default /
      union / each named graph from `DatasetStats.graphs`), and an "exact" checkbox when
      the index has ANN.
  - **Results.** A table with:
    - rank;
    - entity (`TermView`, with `rdfs:label` when present, via `OPTIONAL` in the same query);
    - a score bar oriented by the metric direction;
    - graph;
    - "matched vector", collapsed to the first 8 components and the dimension.

    Row actions: "Explore" (opens `/explore?ds=…` focused on the entity) and "Search
    from here" (switches to entity mode with that entity). In entity mode the UI
    requests k+1 and hides the query entity itself.
  - **Footer.** Elapsed time, plan mode (`exact` / `hnsw ef=64` / `exact (hnsw building)`),
    and "Open in query editor", which carries the generated SPARQL to `/query`.
- **Index status panel** (on `/similar`, and as a "Vector indexes" section on
  `/datasets/[name]`), one card per index:
  - rows, dimension, metric, model;
  - state with a progress bar while building;
  - memory (segment / ANN / overlay) against the budget bar;
  - overlay inserts and deletes, with a "compact to fold in" hint when the overlay
    exceeds 10 % of rows;
  - skipped counts;
  - watermark;
  - ANN recall if measured.

  Actions: Create index (dialog posting `VectorIndexConfig`), Rebuild, Drop. The actions
  are disabled on read-only servers, and their tasks appear in `TaskList`.
- `ui/mock/server.mjs` gains the `/$/vector/*` endpoints and a tiny `spk:vectorSearch`
  emulation, so the UI can be built against the mock.

## 3. Standards basis

- **RDF 1.2 Concepts.**
  - §5 defines a datatype as lexical space, value space and lexical-to-value mapping;
    §4.1 defines `spk:vector` in those terms.
  - §3.4.2: ill-typed literals are accepted, not rejected.
  - §5.2: implementations need not recognise every datatype.
  - Appendix A.3 defines `rdf:JSON` (§8 explains why it is not used).
- **SPARQL 1.1 Query.**
  - §17.6 extensible value testing: IRI-named functions whose errors are type errors.
  - §17.3 RDFterm-equal for unknown datatypes.
  - §18 BGP and join semantics, which the property-function result joins under.
  - §13 RDF datasets and the active graph, which define the search scope.
- **RFC 8259** §6 number grammar, which the lexical grammar reuses.
- **IEEE 754-2019** binary32, round-to-nearest-ties-to-even conversion.
- **RFC 8141** §5.1: formal URN namespace IDs must not start with `X-`, so a `urn:x-…`
  IRI cannot collide with a registered URN namespace.
- **Apache Jena ARQ property functions**: list arguments in subject and object position.
  The `jena-text` form `(?s ?score) text:query (…)` is the syntactic precedent, and F03
  uses the same shape.
- **Malkov & Yashunin, HNSW** (arXiv:1603.09320, IEEE TPAMI 2020): the ANN algorithm,
  with parameters M (connectivity) and ef (expansion).

## 4. Semantics

### 4.1 Datatype `spk:vector` (IRI `urn:x-sparkles:vector`)

**Why `urn:x-sparkles:`.** Sparkles already mints `urn:x-sparkles:inferred`. Jena uses
`urn:x-arq:` the same way. The IRI needs no domain ownership, and by RFC 8141 it cannot
collide with a registered URN namespace. An `https://sparkles.dev/…` IRI would require
owning that domain, which has not been verified. The datatype IRI is persisted in user
data, so the choice must be final before the first release (§9).

**Lexical space** (JSON array of RFC 8259 numbers):

```
vector = ws "[" ws number *( ws "," ws number ) ws "]" ws
number = [ "-" ] ( "0" / %x31-39 *DIGIT ) [ "." 1*DIGIT ] [ ( "e" / "E" ) [ "+" / "-" ] 1*DIGIT ]
ws     = *( %x20 / %x09 / %x0A / %x0D )
```

The following are outside the lexical space, so the literal is ill-typed:
- `NaN`, `Infinity`, `+1`, `.5`, `1.`, hex, an empty array `[]`, or nested arrays;
- more than `MAX_DIM = 16384` elements, or a lexical form over 1 MiB;
- any element whose nearest binary32 is ±∞, i.e. its magnitude is ≥ 2^128 after rounding.

**Value space**: finite sequences of 1..16384 finite binary32 values. The mapping takes
each number to the nearest binary32, ties to even: validate with the grammar above, then
use Rust `str::parse::<f32>`, which is correctly rounded. `-0` maps to −0.0, and
subnormals are kept.

**Term identity**: Sparkles never rewrites a literal. `"[1, 0]"` and `"[1.0,0.0]"` are
different RDF terms. Both are stored, returned byte-for-byte, and indexed as two rows
with equal values.

**Canonical form**, used only when Sparkles creates a literal (`vector::literal`, CLI
output): `[` + elements joined by `,` with no spaces + `]`. Each element is Rust's
shortest round-trip `{:?}` form, e.g. `1.0`, `0.1`, `1e-7`, `-0.0`, which is always a
valid JSON number.

**Precision**: storage and arithmetic are f32. Scores are widened exactly to f64 for the
`xsd:double` result. The widened value's low 29 mantissa bits are zero, so it always
fits Sparkles' inline `Tag::Double` id and scores never touch a vocabulary.

**Normalization**: stored values are never normalized. Cosine uses precomputed norms. A
zero-norm vector has no cosine: the function gives a type error, search skips the row
(`skipped.zeroNorm`), and a zero query vector with cosine returns `400`. Dot and
euclidean accept zero vectors.

### 4.2 Index identity: dimension and model

- **Partitions.** A search runs over one partition, keyed by *(predicate, dimension)*. The
  dimension is the query vector's dimension. Rows of any other dimension are never scored
  together with it.
- **Implicit partitions** (no configuration) let small datasets work with no setup.
  `implicit_partitions = true` is the default.
- **Configured indexes.** A configured index fixes predicate, dimension, metric (for ANN
  and as the default metric) and the optional model label. It skips rows of any other
  dimension and counts them (`skipped.wrongDimension`). A query of another dimension
  against a configured predicate gets `400`.
- **Uniqueness.** A predicate has at most one configured index (`409` otherwise).
- **Models.** The model label is declarative only. Sparkles cannot tell two models of
  equal dimension apart, so the documented practice is one predicate per model, e.g.
  `ex:embMiniLM` and `ex:embE5`.
- **Multiple embeddings per entity** are multiple rows (distinct quads). Top-k counts
  rows. With `distinct:subject` it counts entities, each represented by its best row.

### 4.3 Kernel (shared by functions, exact search and ANN re-scoring)

For vectors of length n, with eight f32 accumulators: `acc[i mod 8] += x_i` for
i = 0..n-1. Sum the accumulators as `((acc0+acc4)+(acc1+acc5))+((acc2+acc6)+(acc3+acc7))`.
Rust never contracts a `mul` and an `add` into FMA implicitly, and the lanes are
independent, so the result is bit-identical whether or not LLVM auto-vectorizes (SSE,
AVX2 or NEON).

| Metric | Formula |
|---|---|
| `dot(a,b)` | Σ aᵢbᵢ |
| `euclidean(a,b)` | √(Σ (aᵢ−bᵢ)²) |
| `cosine(a,b)` | `dot(a,b) / (√dot(a,a) · √dot(b,b))`, clamped to [-1, 1] |

Norms are computed with the same kernel, so a stored norm equals a recomputed one bit
for bit. A non-finite result (overflow) yields `None`. The function then raises a type
error, and search skips the row (it is not counted; a debug log is written).

### 4.4 Search semantics

Let **R** be the visible quads `(s, p, o, g)` of the snapshot with `g` accepted by the
active-graph filter, and `o` a well-typed `spk:vector` of the query's dimension (and
non-zero norm for cosine).

- **Selection.** The result is the k rows of R with the best score. Ties are broken by
  ascending `(s, o, g)` raw id. That order is deterministic within a snapshot but may
  change after compaction (§9).
- **Duplicates across graphs.** If the scope spans several graphs and the pattern has no
  graph variable, rows with equal `(s, o)` are one solution. RDF merge semantics apply:
  the default graph is a merge, not a bag.
- **Entity queries.** For an entity query, the entity's own row is part of R.

Errors are raised at plan time, or at execution for entity lookups. All map to `400`
via `Error::Invalid`:

| Condition | Message (prefix) |
|---|---|
| object not a list, list not well formed, predicate argument not an IRI | `spk:vectorSearch: expected (predicate query [k] [options])` |
| malformed query literal | `malformed spk:vector literal at offset N: …` |
| no partition of the query dimension exists but others do, or a configured-dimension mismatch | `dimension mismatch: <p> has vectors of dimension 384; query has 768` |
| `k` not a positive integer or > `max_k` | `spk:vectorSearch: k must be 1..=10000` |
| unknown option or metric | `spk:vectorSearch: unknown option "…"` |
| entity with more than one vector in scope | `entity <x> has 2 vectors for <p>; pass a vector literal` |
| zero query vector with cosine | `cosine is undefined for a zero query vector` |

If the predicate has no vectors at all, the result is empty (not an error), as with any
BGP.

The following map to the existing status codes:
- a timeout or cancellation during a scan or build wait: `408` / `503`;
- a partition that would exceed the memory budget: `507` (`Error::MemoryLimit`,
  "vector partition <p>/768 needs 2.9 GiB; 1.1 GiB of 4.0 GiB free").

### 4.5 Consistency, freshness and candidates

- **MVCC.** A search on snapshot S sees exactly S's quads. The derived base segment
  covers the generation's base. The snapshot's delta is overlaid on each query:
  `del[PSO]` keys with prefix `[p]` remove base rows, and `ins[PSO]` keys add rows, whose
  literals are parsed through a per-generation id→vector cache. There is no staleness
  window and no commit-path work.
- **Variable query** (Phase 1b). A variable as the `query` argument is bound by the rest
  of the group, like a bound-from-left path. The operator runs once per distinct bound
  vector, at most `max_query_vectors = 1000`; beyond that it returns `507`. Solutions
  carry the input binding.
- **`candidates:join`** (Phase 1b). The result is `Join(Rest, TopK(σ_{s ∈ π_s(Rest)} R))`,
  where Rest is the group without this pattern: the k best rows among entities that also
  satisfy the rest of the group. This definition is declarative. The planner evaluates
  Rest first and passes its distinct `?s` ids as a sorted filter.
- **ANN** (Phase 2) serves only snapshots whose generation is the store's current one
  (the head). The ANN graph covers the base segment; overlay inserts are scored exactly
  and merged, and overlay deletes and graph and candidate filters become the ANN filter
  predicate. The exact path is used instead when any of these holds:
  - `exact:true`;
  - the ANN build is not ready;
  - the metric differs from the index metric;
  - the accepted rows number at most `exact_threshold` (default 20 000);
  - the filtered ANN search returned fewer than k rows while at least k rows are
    accepted.

  Historical snapshots from a later `?at=` spec use exact search, or get `501` if their
  generation's segment is no longer available.

### 4.6 Limits, budgets and defaults

| Setting | Default | Where |
|---|---|---|
| `vector_budget_bytes` | 4 GiB | `StoreOptions`, `--vector-mb` |
| `MAX_DIM` | 16384 | constant |
| `max_k` | 10 000 | `StoreOptions` |
| `implicit_partitions` | true | `StoreOptions` |
| `exact_threshold` | 20 000 rows | index config / `StoreOptions` |
| overlay parse cache | ≤ 5 % of the budget, LRU | per generation |
| ANN connectivity / expansionAdd / expansionSearch / quantization | 16 / 128 / 64 / f16 | index config |

**Memory model.** The budget covers these per-row costs:

| Structure | Bytes per row |
|---|---|
| segment | `4·dim + 4` (norm) + 24 (`s, o, g` ids) |
| ANN vectors | `dim · {4, 2, 1}` for f32 / f16 / i8 |
| ANN links (M = 16) | ≈ 2M·4 + 16 |

The overlay cache is counted too. mmapped files count as fully resident, which is
conservative and deterministic.

**Worked example: 1M × 768.**

| Component | Size |
|---|---|
| segment | 3.10 GB (2.86 GiB raw f32 + 28 MB ids and norms) |
| ANN f16 | 1.54 GB (1.43 GiB) |
| ANN links | ≈ 0.14 GB |
| **Total** | **≈ 4.8 GB (4.46 GiB)** |

With i8 quantization the total is ≈ 3.7 GiB.

The lexical forms also live in the vocabulary, on disk and mmapped (outside the
budget). At about 12 bytes per element that is ≈ 9 GB. The status reports this separately
as `lexicalBytes` (estimated), and §9 raises a compact alternative datatype.

**Admission.** A partition or ANN build whose estimate exceeds the free budget is
refused:
- the index goes to `over-budget`;
- ANN absence degrades to exact search;
- segment absence yields `507` for searches on that partition.

Implicit partitions are LRU-evicted before configured ones are refused. Configured
segments are pinned.

## 5. Design sketch

**Modules.**
- `crates/sparkles/src/vector/mod.rs`: lexical parsing and formatting, `Metric`, kernel.
  Always compiled (no dependencies).
- `vector/segment.rs`: packed partitions.
- `vector/search.rs`: exact top-k and overlay.
- `vector/registry.rs`: configuration, status, budget.
- `vector/ann.rs`: behind `feature = "vector-hnsw"`. A backend trait:
  `build(&Segment) -> Ann`, `search(q, k, ef, filter: &dyn Fn(u64) -> bool) -> Vec<u64 /*row*/>`,
  `save/view(path)`, `memory_bytes()`.

**Cargo features (crate `sparkles`).**
- `vector`, on by default: no new dependencies. It gates the property function, the
  functions and the registry.
- `vector-hnsw` = `["vector", "dep:usearch"]`, off by default. Embedded users therefore
  pay nothing for ANN.

`sparkles-server` enables `vector-hnsw` in its default features (it needs a C++ compiler,
which the Nix stdenv already provides). Built without `vector`, the planner still
recognises `spk:vectorSearch` and returns `501 "built without vector support"`, never a
silent empty match.

**Store integration (`store.rs`).**
- `Generation` gains `vectors: vector::GenerationVectors`. It holds a map from
  *(predicate id, dim)* to `Arc<OnceCell<Result<Segment>>>`, plus the overlay parse cache.
  Because it lives on the generation, it is dropped with the generation, and ids inside
  it are always that generation's ids.
- `Store` gains `vector: Arc<vector::Registry>`, which holds the configuration, the budget
  counter and build tasks. `Snapshot` reaches it through the store handle that `Ctx`
  already carries via `snap`. That is one new `Arc` field on `Snapshot`, like `results`.
- **Segment** = the rows of `Perm::Pso` with prefix `[p]` of the base generation only,
  read through `PermIndex::for_each_range_until`, not the delta. It holds:
  - `ids: Vec<[u64;3]>` (s, o, g in PSO order, so duplicates across graphs are adjacent);
  - `norms: Vec<f32>`;
  - `data: Vec<f32>` (row-major, 64-byte aligned).

  Literals are parsed in parallel per block with rayon (`Vocab::get_sorted` batches the
  key reads).
- **Overlay.** Per query, `snap.delta.del[Pso]` in range `[p]` goes into an
  `FxHashSet<[u64;3]>`, and `snap.delta.ins[Pso]` in range `[p]` goes through the
  per-generation cache (`FxHashMap<u64 obj id, Option<Arc<[f32]>>>`). Delta and vocab ids
  are stable within a generation. `ins` is disjoint from base and `del ⊆ base`
  (`store::apply`), so R = (segment − del) ∪ ins exactly.

**Planner (`sparql/plan.rs`).**
- In `plan_group`, before filter-equality substitution, find triples whose predicate is
  the constant `spk:vectorSearch`. Blank nodes are already hidden variables named
  `" bn…"`. The rewrite follows `rdf:first` / `rdf:rest` chains among the group's
  triples: spargebra's `Collection` rule emits two triples per element, ending in
  `rdf:nil`. Each list node must have exactly one `first` and one `rest` and no other use;
  the rewrite then removes them.
- It emits `Item::Node(Node::leaf(Kind::VectorSearch(Box<VectorSearchSpec>), vars, est, desc))`.
  - `est` = min(k, partition rows).
  - Graph handling reuses `graph_filter(&t.graph)` and binds the graph var when present.
  - `desc` looks like `VectorSearch <emb> cosine k=10 dim=768 exact rows=1.0M (+12 −3)`,
    with the query vector abbreviated.
- `VectorSearchSpec { pred: Id, query: QueryArg /* Vector(Arc<[f32]>) | Entity(Id) | Var(VarId) */, k, metric, opts, graph: GraphFilter, graph_var, out: [Option<VarId>; 3], dedup }`.
- `candidates:join` makes the node dependent. It becomes a child-taking kind, like
  `Path { bound_from_left }`, with Rest as child 0.

**Executor (`sparql/exec.rs`).**
- `Kind::VectorSearch` splits the segment into row chunks of 4096 and runs them with rayon.
  - Each chunk calls `ctx.check()`.
  - Each chunk skips rows by `graph.accepts(g)`, by the del set, by adjacent-duplicate
    `(s, o)` when `dedup` is set, and by the candidate set.
  - Each chunk keeps a bounded binary heap of size k keyed by (goodness, Reverse(ids)).
- The chunk heaps are merged, then overlay rows are pushed. The heap is dedup-guarded by
  an `(s, o)` set when `dedup` is set.
- Output is a `Table` in best-first order. `score` is `Id::from_f64(f as f64)`, which
  always succeeds (§4.1).
- `describe` reports the operator as "VectorSearch".
- `cache.rs`: Phase 1 marks the kind non-cacheable. Phase 2 keys on (spec, query-vector
  hash, engine mode).

**Functions (`sparql/expr.rs`).**
- `extension()` gains an `SPK = "urn:x-sparkles:"` branch, and `is_extension` accepts it.
- Parsing memoises by argument id in a small `Ctx` map (≤ 64 MiB), so BIND over many
  rows parses each distinct literal once.

**Persistence (Phase 1b).**
- **Configuration**: `<root>/vector.json`
  (`{ "version": 1, "indexes": [{ "name", …VectorIndexConfig }] }`), written with
  `write_atomic`. In-memory stores keep it in memory.
- **Segment file**: `<root>/gen-NNNN/vectors/<name>.spkv`, little-endian.
  - Header (64 bytes):
    - magic `SPKVSEG\x01`;
    - `u32 format_version = 1`, `u32 dim`;
    - `u64 rows`;
    - `u64 base_seq` (the watermark);
    - `u64 config_hash` (FNV-1a over the canonical config JSON);
    - `u64 predicate_id`;
    - `u64 malformed`, `u32 wrong_dim`, `u32 zero_norm`.
  - Body: `ids[rows][3]u64`, `norms[rows]f32`, padding to 64, `data[rows·dim]f32`.
  - Footer: `u64 rows`, `u64 fnv(header ‖ ids)`, magic.

  The segment is opened with `memmap2`, which is already a dependency.
- **ANN file** (Phase 2): `<name>.ann` (the backend's own file, opened with the backend's
  mmap view) plus `<name>.ann.json`
  `{ format_version: 1, backend, backend_version, base_seq, config_hash, rows, quantization, connectivity, expansionAdd }`.
- **Watermark.** `base_seq` is the commit `seq` folded into the generation (from the
  commit-identity spec: `IndexMeta.base_seq`). Until that lands, use the tuple
  `(generation name, meta.created, meta.quads)`.
- **Validity on open.** A derived file is valid only if magic, version, `config_hash`,
  `base_seq`, row count and footer checksum all match. Anything else means delete and
  rebuild. The data section is not checksummed; the atomic rename guarantees
  completeness.

**Crash safety.**
- Builds write `*.tmp`, `fsync`, `rename`, then `fsync` the directory. `*.tmp` files are
  deleted when a store opens.
- Derived files are never part of a commit: a crash at any point loses at most work, and
  queries rebuild lazily.
- Compaction and bulk rebuilds create a new generation directory with no `vectors/`.
  After the `CURRENT` switch in `rebuild_locked`, the registry enqueues background builds
  for every configured index, segments first, then ANN.
- Queries that arrive during a build wait on the same `OnceCell`, bounded by their
  deadline (`408` with `"vector index <name> is building (37%)"`).
- Removing the old generation directory removes its derived files.
- On a read-only server, segments are built in memory only and never written.

## 6. Phasing

**Phase 1 (MVP, about 1 day).**
- `vector/mod.rs`: grammar, parse, canonical, kernel, unit tests (grammar edge cases,
  kernel determinism, the §7 function examples).
- `spk:cosine`, `spk:dot`, `spk:euclidean`, `spk:dimension` in `expr.rs`.
- The `spk:vectorSearch` rewrite. Supported: a constant query (literal or entity), a
  constant k, and the `metric:` option. A variable query returns `501`.
- Exact search over lazily built in-memory segments keyed by (p, dim), with the delta
  overlay, graph scoping and dedup, a bounded heap, `ctx.check()` cancellation, and the
  budget check (`507`). Rayon is optional in P1.
- Tests: the §7 SPARQL examples in `sparql/tests.rs`.

**Phase 1b (exact search, complete).**
- `candidates:join`, variable query and `distinct:subject`.
- Configured indexes (`vector.json`) and persisted segments with watermark.
- Background builds after generation switches, the `/$/vector` endpoints, `sparkles vector`,
  `DatasetStats.vector`, and result caching.
- UI route `/similar` and the dataset-page section; mock endpoints.

**Phase 2 (HNSW).**
- `vector-hnsw` with the USearch backend (§8 compares the libraries): filtered search,
  f16 default quantization, and re-scoring with the §4.3 kernel.
- Head-only ANN, the exact fallback rules, the recall endpoint, and UI recall display.
- Acceptance: recall@10 ≥ 0.95 against the exact oracle on a clustered synthetic set
  (100k × 128), with p50 latency at least 10× below exact.

**Phase 3 (later).**
- Incremental ANN catch-up of overlay inserts in the background: USearch `add`/`remove`,
  with the ANN file's watermark advancing to the last applied `seq`.
- Carrying ANN across compaction by remapping row keys through term keys.
- `ORDER BY DESC(spk:cosine(?v, C)) LIMIT k` rewritten to `VectorSearch`.
- Hybrid ranking with F03 `text:query` (for example reciprocal-rank fusion as a function
  over two scored patterns).
- A compact datatype (§9).

## 7. Acceptance examples

Fixture (`PREFIX ex: <http://example.org/> PREFIX spk: <urn:x-sparkles:>`). Scores
compare with |Δ| ≤ 1e-6 unless exact.

```turtle
ex:a ex:emb "[1, 0, 0]"^^spk:vector ; a ex:Doc .
ex:b ex:emb "[0.8, 0.6, 0]"^^spk:vector ; a ex:Doc .
ex:c ex:emb "[0, 1, 0]"^^spk:vector ; a ex:Img .
ex:d ex:emb "[-1, 0, 0]"^^spk:vector .
ex:e ex:emb "[0, 0, 0]"^^spk:vector .
ex:f ex:emb "[1, 2]"^^spk:vector .
ex:g ex:emb "[1, NaN, 0]"^^spk:vector .
GRAPH ex:g1 { ex:h ex:emb "[0.6, 0.8, 0]"^^spk:vector . ex:a ex:emb "[1, 0, 0]"^^spk:vector . }
GRAPH ex:g2 { ex:a ex:emb "[1, 0, 0]"^^spk:vector . }
```

1. **Default-graph scope.** `SELECT ?s ?score { (?s ?score) spk:vectorSearch (ex:emb "[1,0,0]"^^spk:vector 3) } ORDER BY DESC(?score)`
   returns `a 1.0`, `b 0.8`, `c 0.0`. Excluded: e (zero norm), f (other dimension), g
   (malformed), and h (named graph, out of scope).
2. **Named graphs.** `SELECT ?s ?g ?score { GRAPH ?g { (?s ?score) spk:vectorSearch (ex:emb "[1,0,0]"^^spk:vector 5) } } ORDER BY DESC(?score)`
   returns `a ex:g1 1.0`, `a ex:g2 1.0`, `h ex:g1 0.6` (one row per graph).
3. **Union default.** With `--union-default-graph` (default graph = union of the named
   graphs), query 1 with k=2 returns `a 1.0`, `h 0.6`. The `(a, o)` pair in ex:g1 and
   ex:g2 is one solution, so it does not crowd out h.
4. **Join after top-k.** `{ ?s a ex:Doc . (?s ?score) spk:vectorSearch (ex:emb "[0,1,0]"^^spk:vector 2) }`:
   the top-2 is c (1.0), b (0.6), and the join keeps `b 0.6` only.
   With `"candidates:join"` (Phase 1b): `b 0.6`, `a 0.0`.
5. **Euclidean.** `(?s ?score) spk:vectorSearch (ex:emb "[1,0,0]"^^spk:vector 3 "metric:euclidean")`
   returns `a 0.0`, `b 0.6324555`, `e 1.0`.
6. **Other dimensions.** Query `"[1,0]"` returns `f 0.4472136` (cosine). Query
   `"[1,0,0,0]"` returns `400 {"error":"dimension mismatch: <http://example.org/emb> has vectors of dimension 2, 3; query has 4"}`.
7. **Malformed query and bad k.** `"[1, x]"^^spk:vector` returns `400 malformed spk:vector literal at offset 4`.
   `k = 0` returns `400`. `(ex:emb "[1,0,0]"^^spk:vector 10 "foo:bar")` returns `400 unknown option`.
8. **Entity query.** `(?s ?score) spk:vectorSearch (ex:emb ex:b 2)` returns `b 1.0`, `a 0.8`.
   `ex:zzz` (no vector) returns 0 rows. Adding `ex:b ex:emb "[0,0,1]"^^spk:vector`, then
   querying ex:b again, returns `400 entity … has 2 vectors`.
9. **Functions.**

   | Expression | Result |
   |---|---|
   | `spk:dot("[1,2,3]"^^spk:vector, "[4,5,6]"^^spk:vector)` | `32.0e0` (exact) |
   | `spk:euclidean("[0,0]"^^spk:vector, "[3,4]"^^spk:vector)` | `5.0e0` (exact) |
   | `spk:dimension("[1,2,3]"^^spk:vector)` | `3` |
   | `BIND(spk:cosine(?v, "[0,0,0]"^^spk:vector) AS ?x)` | unbound |
   | `spk:cosine("[1,0]"^^spk:vector, "[1,0,0]"^^spk:vector)` | type error |
   | `spk:cosine("[1,0]", …)` (plain string) | type error |

10. **MVCC and round-trip.** `INSERT DATA { ex:z ex:emb "[1.0, 0.0,0]"^^spk:vector }`;
    query 1 with k=2 returns `a 1.0`, `z 1.0` (tie: base id < delta id).
    `SELECT ?v { ex:z ex:emb ?v }` returns the exact lexical form `"[1.0, 0.0,0]"`.
    `DELETE DATA { ex:a ex:emb "[1, 0, 0]"^^spk:vector }`, then query 1 with k=2 returns
    `z 1.0`, `b 0.8`. A reader holding the pre-delete snapshot still sees a.
11. **Compaction invariance.** After `POST /$/compact/ds`, queries 1–10 return the same
    rows and scores (tie order may change only among equal scores).
12. **Cancellation.** A 200k × 256 exact search with `timeout=0.001` returns `408`.
13. **Budget.** With `--vector-mb 1`, a 10k × 128 search returns `507` with the "needs …
    free" message.
14. **HTTP (1b).** On the fresh fixture (all graphs are in the segment),
    `PUT /$/vector/ds/emb3 {"predicate":"http://example.org/emb","dimension":3,"metric":"cosine"}`
    returns `201` and a task. `GET` then shows `state:"ready"`, `rows:8` (e is stored
    because dot and euclidean can use it), and `skipped:{malformed:1, wrongDimension:1, zeroNorm:1}`.
    Then:
    - **after restart:** the index is still `ready` with the same `watermark`, and the
      `.spkv` file was not rewritten (same mtime);
    - **after corrupting the footer:** it rebuilds;
    - **with `vector-hnsw` absent, `PUT … "ann":{}`:** `501`.
15. **CLI (1b).** `sparkles vector search --loc db --predicate http://example.org/emb --vector '[1,0,0]' -k 2`
    prints two rows, `<http://example.org/a> 1.0` and `<…/b> 0.8`, and exits 0.
    `--vector '[1,0,0,0]'` exits 1 with the dimension message.
16. **Recall (2).** On 100k clustered vectors, the recall endpoint reports ≥ 0.95 at k=10,
    ef=64. With `exact:true`, results equal the Phase 1 output bit for bit.

## 8. Rejected alternatives

- **Canonicalizing vector literals on load.** It breaks Sparkles' exact term identity
  (README decision on canonical inlining); the lexical form must round-trip.
- **A new id `Tag` or inline ids for vectors.** Vectors cannot fit 60 bits, and tags are
  scarce (4 bits, 12 used). The segment already is out-of-line storage keyed by the
  vocab id.
- **`rdf:JSON` as the datatype.** Its value space is any JSON value, and its numbers are
  `xsd:double`. It cannot express "finite f32 vector of fixed dimension", and reusing a
  W3C datatype with narrower semantics would mislead other tools.
- **`rdf:List` of `xsd:float`.** 768 triples per vector: 768M triples for 1M vectors.
- **`xsd:base64Binary`.** Opaque and not human-readable. It is kept as a possible
  *second* compact datatype (§9).
- **Magic `SERVICE spk:vector { … }`.** It overloads federation semantics (endpoint
  evaluation, SILENT, variable scoping) in the existing `Kind::Service` path. The
  property-function list form parses with spargebra unchanged and matches Jena's
  `text:query` and F03.
- **Only functions plus `ORDER BY … LIMIT`.** Every query would be a full scan and sort,
  with no ANN and no graph-scoped top-k. It is kept as a later rewrite.
- **Normalizing at ingest for cosine.** It changes values seen by dot and euclidean on the
  same data. Precomputed norms cost 4 bytes per row.
- **f64 storage.** Twice the memory, and embeddings are produced as f32 anyway.
- **Maintaining HNSW in the commit path.** It adds latency and couples crash recovery to
  a C++ structure. The delta overlay already gives exact freshness; background catch-up
  is Phase 3.
- **ANN libraries** (versions checked on crates.io on 2026-09-30):

  | Crate | License | Version | Assessment |
  |---|---|---|---|
  | **USearch** | Apache-2.0 | 2.26.2, 2026-08-31 | Recommended. `filtered_search` with an arbitrary key predicate, `remove`, `save`/`view` (mmap), `memory_usage`, f16/bf16/i8 quantization, `exact_search`, `Send + Sync`. Cons: C++ via `cxx` (a C++ toolchain; check static/musl builds), not unwind-safe (wrap calls, never panic in the filter), and transitive licenses (optional SIMD dependencies) to verify with `cargo deny` at adoption |
  | **hnsw_rs** | MIT/Apache-2.0 | 0.3.4, 2026-02 | Pure Rust, rayon `parallel_insert`, `search_filter`, dump/reload with mmapped data. No deletion, no quantization, larger transitive tree (bincode, mmap-rs, anndists). **Fallback** if the maintainer prefers pure Rust (§9); the backend trait makes swapping cheap |
  | instant-distance | MIT OR Apache-2.0 | 0.6.1, 2023-06 | No filtered search, no deletion, stale |
  | hnsw (rust-cv) | MIT | 0.11.0, 2021 | Unmaintained |
  | arroy / hannoy | MIT | 0.8.0 / 0.2.0 | LMDB-backed: a second storage engine inside Sparkles |
  | FAISS bindings, LanceDB | — | — | Heavy native or dependency footprint for one index type |

## 9. Open questions (defaults chosen; maintainer may revisit)

1. **Namespace.** `urn:x-sparkles:` versus an owned `https://` vocabulary IRI. This must
   be settled before release, since it is persisted in data.
2. **Score datatype.** `xsd:double` (inline ids, cheap) versus `xsd:float` (the true
   precision, but a local-vocab entry per row).
3. **Tie-break.** By raw id (cheap, snapshot-deterministic) or by term order (stable
   across compaction, but key reads at the boundary).
4. **k.** Default k=10 versus making k mandatory.
5. **Query entity.** Whether it appears in its own results (it does now; the UI hides
   it).
6. **Implicit partitions.** Whether they should require a configured index on large
   stores (for example refuse implicit partitions above 1M rows).
7. **USearch versus hnsw_rs.** USearch has more features; hnsw_rs matches the README's
   pure-Rust preference (LZ4 over zstd).
8. **Strict writes.** An optional mode rejecting ill-typed `spk:vector` literals on write
   (default: accept and count).
9. **Compact datatype.** A second datatype `spk:vectorB64` (base64 of little-endian
   binary32, ≈ 5.3 bytes per element instead of ≈ 12) for large corpora, where lexical text
   dominates disk use.
10. **Mapped memory.** Whether mmapped segments should count fully against the budget (they
    do now) or only as page cache.
11. **Read-only servers.** Whether persisted rebuilds should be allowed there (they are
    now in memory only).

## 10. Sources

- **Sparkles repository** (read only):
  - `README.md` and `docs/API.md`: endpoints, error mapping, and decisions (exact term
    identity, pure-Rust preference).
  - `crates/sparkles/src/`:
    - `id.rs`: tags and inline doubles (the f32 widening fact);
    - `vocab.rs`: base and delta vocabularies, key layout;
    - `store.rs`: generations, delta and the `apply` invariants, WAL, `rebuild_locked`,
      snapshots;
    - `builder.rs`: `IndexMeta`;
    - `error.rs`;
    - `sparql/expr.rs`: extension function dispatch;
    - `sparql/plan.rs`: `plan_group`, `graph_filter`, blank-node variables, `Kind`;
    - `sparql/exec.rs`, `sparql/ctx.rs` (cancellation, dataset), `sparql/cache.rs`,
      `sparql/table.rs`.
  - `crates/sparkles/src/dataset.rs`.
  - `crates/sparkles-server/src/{http.rs,state.rs,main.rs}` and both `Cargo.toml` files.
  - `ui/src/routes/*`, `ui/src/lib/api.ts`, `ui/mock/`.
- **spargebra 0.4.7 source** (MIT OR Apache-2.0), `spargebra-0.4.7/src/parser.rs` (the published crate):
  the `Collection` rule (rdf:first/rest expansion with fresh blank nodes) and `build_bgp`.
- **W3C RDF 1.2 Concepts**, https://www.w3.org/TR/rdf12-concepts/: §3.4.2 ill-typed
  literals, §5 datatypes, §5.2 unknown datatypes, A.3 `rdf:JSON`.
- **RFC 8141**, https://www.rfc-editor.org/rfc/rfc8141.html, §5.1: the `X-` NID
  restriction.
- **Apache Jena documentation (Apache-2.0)**:
  - https://jena.apache.org/documentation/query/extension.html: property functions and
    list arguments;
  - https://jena.apache.org/documentation/query/text-query.html: `text:query` subject and
    object list forms, score output, and GRAPH scoping.
- **crates.io API**, consulted 2026-09-30, for versions, licenses and dates:
  `usearch`, `hnsw_rs`, `instant-distance`, `hannoy`, `arroy`, `hnsw`.
- **docs.rs**:
  - `usearch` `Index` methods, `IndexOptions`, `MetricKind` (L2sq is squared), `ScalarKind`;
    crate page for the `cxx` build dependency and features;
  - `hnsw_rs` `Hnsw` methods and the crate README (distances, dump/reload, mmap,
    filtering, no deletion).
- **Cited from the standards and literature, not re-fetched**:
  - SPARQL 1.1 Query (§13, §17.2, §17.3, §17.6, §18);
  - RFC 8259 §6;
  - IEEE 754-2019 binary32;
  - Malkov & Yashunin, arXiv:1603.09320.
- **Not consulted**: anything from Fluree (source, docs, website, blog or talks).

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30 (`9b06c89`): the `spk:vector` datatype
(`urn:x-sparkles:vector`) with the §4.1 grammar, `spk:cosine`, `spk:dot`,
`spk:euclidean` and `spk:dimension`, and an exact top-k `spk:vectorSearch` leaf. The
query is a vector literal or an entity; `k` defaults to 10 (at most 10,000) and the only
option is `metric:`. Base vectors are packed per (predicate, dimension) on first search and
cached per generation. Every query overlays its snapshot's not-yet-compacted inserts and
deletes, so results match its data exactly. The search is scoped to the active graph, and
rows of the same subject and vector in a merged default graph count once. Scores are
`xsd:double` and ties break by term id, the defaults of §9. Literals are stored and
returned exactly as written; a malformed one is stored but never matched.

From Phase 1b only a minimal introspection piece was built (`cb598c8`): the read-only
`GET /$/vector/{ds}` (budget, used bytes and the packed predicates and dimensions of the
current generation) and `sparkles serve --vector-memory-mb` (default 4096) in place of
the spec's `--vector-mb`. The budget is process-wide; past it a search answers `507`
through the shared budget error of [C01](C01-observability-and-budgets.md), and search
outputs are also charged to the per-query memory budget. The UI shows "Similar" in the
explorer rather than a separate `/similar` page, and the MCP server
([C11](C11-mcp-server.md)) exposes the search as its `similar_entities` tool.

**Deviations.** The vector code is always compiled; there is no `vector` cargo feature
(it has no dependencies). A variable query vector answers `501` as Phase 1 specified.
Vector search results are not cached.

**Tests at landing.** Integration tests in `crates/sparkles/tests/vectors.rs` and unit
tests of the grammar and the kernel in `crates/sparkles/src/vector.rs`.

**Performance.** No vector benchmark has been published.

**Not built.** Configured indexes (`vector.json`, `PUT`/`DELETE /$/vector/{ds}/{name}`),
persisted segment files, background builds, `candidates:join`, `distinct:subject`,
variable query vectors, the `sparkles vector` CLI, the approximate HNSW index (Phase 2),
and everything in Phase 3, including hybrid ranking with
[F03](F03-full-text-search.md). The README lists the missing approximate index as a known
gap.
