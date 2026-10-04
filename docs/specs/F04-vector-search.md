# F04: Vector similarity search

> **Status:** implemented in part (Phases 1, 1b and 2, part of Phase 3)
>
> **Phases:** Phases 1, 1b and 2 shipped. They cover `spk:vector` literals, the
> similarity functions, `spk:vectorSearch` with variable queries, `candidates:join` and
> `distinct:subject`, configured indexes with persisted files and background builds,
> `/$/vector/{ds}/{name}`, `sparkles vector`, an HNSW graph with an exact overlay of each
> snapshot's changes, the `/similar` page and the dataset page's index cards. Of Phase 3,
> hybrid ranking with full-text search shipped as `spk:hybridSearch`. The rest of Phase 3
> is not built.
>
> **User docs:** [API: Vector similarity](../API.md#vector-similarity) · [API: Vector indexes](../API.md#vector-indexes) · [Features](../FEATURES.md#sparql-arq-equivalent)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

The design depends on the durable commit `seq` from the commit-identity work
([CI](CI-commit-identity.md)) and on the budgets of
[C01](C01-observability-and-budgets.md). Hybrid text and vector queries would also use
[F03](F03-full-text-search.md), but nothing else requires it.

## 1. Summary, goals, non-goals

Embeddings are stored as ordinary RDF literals of a Sparkles datatype,
`"[0.1, 0.2, 0.3]"^^spk:vector`. SPARQL expressions can compute similarities with
`spk:cosine`, `spk:dot` and `spk:euclidean`. A property function runs a top-k
nearest-neighbour search: `(?s ?score) spk:vectorSearch (ex:emb "[…]"^^spk:vector 10)`.
Its results join with the rest of the graph pattern like any other solution sequence.
The literal is the source of truth. Packed `f32` matrices and HNSW graphs are derived
data. They can be rebuilt, they carry a watermark, and they are never built on the
commit path.

**Goals**
- Exact top-k search that is correct under MVCC, so every snapshot sees exactly its own
  vectors. It is scoped to the query's active graph and serves as the recall oracle for
  approximate search.
- Approximate search (HNSW) for the head generation, behind an optional cargo feature.
  Candidates are re-scored with the same kernel, so its scores equal the exact path's.
- Explicit configuration. An index is bound to a predicate, a dimension, a metric and an
  optional model label. Vectors of another dimension never enter it.
- Memory accounting: a budget, per-index byte counts, and admission errors instead of
  OOM kills.
- Admin surfaces in HTTP, the CLI and the UI: status, create, rebuild, and a similarity
  panel.

**Non-goals**
- Computing embeddings. Sparkles has no model runtime, so clients bring their vectors.
- Sparse vectors, binary or Hamming metrics, and multi-vector (late-interaction) scoring.
- Equality or ordering of `spk:vector` literals by value. `=` stays RDF-term equality.
- Distributed or GPU search.
- Hybrid ranking fusion with F03. Sections 6 and 9 only reserve room for it.

## 2. User-visible behavior

All terms live in the namespace `PREFIX spk: <urn:x-sparkles:>`. §4.1 explains the
choice.

| IRI | Kind |
|---|---|
| `spk:vector` | Datatype |
| `spk:cosine`, `spk:dot`, `spk:euclidean`, `spk:dimension` | Functions |
| `spk:vectorSearch` | Property function (a reserved predicate) |

### 2.1 Data

```turtle
@prefix ex:  <http://example.org/> .
@prefix spk: <urn:x-sparkles:> .
ex:doc1 ex:embMiniLM "[0.0132, -0.2210, 0.0871]"^^spk:vector .
ex:doc1 ex:embMiniLM "[0.0101, -0.2000, 0.0900]"^^spk:vector .   # a second embedding (e.g. a second chunk)
```

Vectors load through every existing path: bulk load, GSP, INSERT DATA and upload. A
literal whose lexical form is not a valid vector is still stored, because RDF 1.2 §3.4.2
says implementations SHOULD accept ill-typed literals. Search skips it, and the index
status counts it under `skipped.malformed`.

### 2.2 SPARQL functions (usable in FILTER, BIND, SELECT expressions, ORDER BY, aggregates)

| Function | Result |
|---|---|
| `spk:cosine(?a, ?b)` | `xsd:double` in [-1, 1] |
| `spk:dot(?a, ?b)` | `xsd:double` |
| `spk:euclidean(?a, ?b)` | `xsd:double` ≥ 0. The L2 distance, not its square. |
| `spk:dimension(?a)` | `xsd:integer` |

Arguments must be well-typed `spk:vector` literals. The following cases are SPARQL type
errors, so the value is unbound in BIND and false in FILTER (SPARQL 1.1 §17.2, §17.6):
- an argument is not an `spk:vector` literal, or is ill-typed;
- the two vectors differ in dimension;
- cosine of a vector with zero norm;
- a non-finite intermediate result.

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
  stored literal that matched, which keeps a result unambiguous when an entity has
  several embeddings. A constant in any position requires that output to equal it.
- **Object list**: `(predicate query [k] ["option:value" …])`.
  - `predicate` is an IRI naming the embedding predicate.
  - `query` is one of:
    - an `spk:vector` literal;
    - an IRI or blank-node label naming an entity. The query vector is then that
      entity's vector under `predicate` in the active graph. If the entity has more than
      one, the query fails. If it has none, the result is empty;
    - a variable (Phase 1b, see §4.5).
  - `k` is a positive `xsd:integer`. The default is 10 and the maximum is `max_k`
    (default 10 000).
  - Options are string literals of the form `"key:value"`, with these keys:

    | Option | Values | Phase |
    |---|---|---|
    | `metric:` | `cosine`, `dot`, `euclidean` | 1 |
    | `exact:true` | Forces the exact path. | 2 |
    | `ef:N` | Sets the HNSW expansion for this query. | 2 |
    | `candidates:join` | Searches only the join candidates. | 1b |
    | `distinct:subject` | Returns at most one row per entity. | 1b |

- **Output**: one solution per selected stored quad `(s, predicate, o, g)`. `?s` is s,
  `?score` is the score as an `xsd:double`, and `?vector` is o. Under
  `GRAPH ?g { … }`, `?g` is g.
- **Score direction**: cosine and dot are similarities, where higher is better. Euclidean
  is a distance, where lower is better. "Top-k" means the k best rows in the metric's
  direction.
- **Order**: the operator emits rows best first, but only ORDER BY defines the order of
  SPARQL results. Queries and the UI use `ORDER BY DESC(?score)`, or `ORDER BY ?score` for
  euclidean.
- **Scope**: top-k is computed over the rows in the pattern's active graph. That is the
  default graph (after `FROM`, `default-graph-uri`, `--union-default-graph` and
  `reasoning=` apply), `GRAPH <g>` or `GRAPH ?g`. Sparkles never scopes a search by
  filtering a global top-k afterwards. The other patterns in the group join with the k
  rows after the search, so `?s a ex:Doc` can leave fewer than k rows. `candidates:join`
  (Phase 1b) reverses this order, as §4.5 describes.
- **Placement**: the pattern works in any group, including OPTIONAL, MINUS, EXISTS,
  subqueries and GRAPH. Inside SERVICE it is not rewritten, because SERVICE sends its
  text to the remote endpoint verbatim.

### 2.4 HTTP (Phase 1b unless noted)

| Method | Path | Description |
|---|---|---|
| GET | `/$/vector/{ds}` | Returns `VectorStatus`: the budget, the usage, and every configured index and implicit partition. |
| GET | `/$/vector/{ds}/{name}` | Returns one `VectorIndexStatus`, or `404` if the index is unknown. |
| PUT | `/$/vector/{ds}/{name}` | Creates or replaces the configuration (JSON below), with `201` when created and `200` when replaced. The body is `{ index: VectorIndexStatus, task: Task }`. The build runs as a task of kind `vector`. |
| DELETE | `/$/vector/{ds}/{name}` | Drops the configuration and its derived files. Answers `204`. |
| POST | `/$/vector/{ds}/{name}/rebuild` | Discards the derived files and rebuilds them. Returns a `Task`. |
| POST | `/$/vector/{ds}/{name}/recall?samples=100&k=10` | Phase 2. Measures ANN recall@k against the exact oracle, with stored vectors as the queries. Returns a `Task` whose message holds the result. |

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
- `400`: an invalid configuration, such as a dimension out of range, an unknown metric or
  invalid ANN parameters.
- `403`: a mutating endpoint on a `--read-only` server.
- `404`: an unknown dataset or index.
- `409`: the predicate is already indexed under another name.
- `501`: `ann` was requested in a build without the `vector-hnsw` feature.

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

`search` is a thin wrapper that generates the SPARQL of §2.3, so it runs the same code
path as a query. The global flag `--vector-mb N` (default 4096) sets the memory budget for
`serve` and every other command.

### 2.6 Rust library API

```rust
pub mod vector {                                   // crates/sparkles-core/src/vector/
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

The query builder needs nothing new. `SelectBuilder::where_("(?s ?score)", "spk:vectorSearch", "(ex:emb \"[…]\"^^spk:vector 10)")`
already emits the text. A typed helper is optional.

### 2.7 UI (SvelteKit, Phase 1b)

- **A new route, `/similar`.** Its nav item, "Similar", sits between Explore and
  Datasets, and the dataset comes from `app.current`.
  - **Left panel: the query input.**
    - An index picker, filled from `GET /$/vector/{ds}`. It shows each index's
      predicate, dimension, metric and state badge.
    - A query mode toggle:
      - *Entity*: an IRI input with prefix-aware completion. It looks up labels through
        SPARQL, as the explorer does.
      - *Vector*: a textarea that the browser validates live against the §4.1 grammar.
        It shows the parsed dimension or the first error and its offset, and it flags a
        dimension that differs from the index's.
    - k (1–100, default 10), the metric (the index's metric by default), the graph
      (default, union, or any named graph from `DatasetStats.graphs`), and an "exact"
      checkbox when the index has ANN.
  - **Results.** A table with these columns:
    - rank;
    - entity (a `TermView` with its `rdfs:label` when present, fetched by an `OPTIONAL`
      in the same query);
    - a score bar oriented by the metric's direction;
    - graph;
    - "matched vector", collapsed to the first 8 components and the dimension.

    Each row has two actions. "Explore" opens `/explore?ds=…` focused on the entity.
    "Search from here" switches to entity mode with that entity. In entity mode the UI
    requests k+1 rows and hides the query entity itself.
  - **Footer.** The elapsed time, the plan mode (`exact`, `hnsw ef=64` or
    `exact (hnsw building)`), and "Open in query editor", which carries the generated
    SPARQL to `/query`.
- **Index status panel.** It appears on `/similar` and as a "Vector indexes" section on
  `/datasets/[name]`, with one card per index. A card shows:
  - rows, dimension, metric and model;
  - the state, with a progress bar during a build;
  - memory (segment, ANN and overlay) against the budget bar;
  - overlay inserts and deletes, with a "compact to fold in" hint once the overlay
    exceeds 10 % of the rows;
  - the skipped counts;
  - the watermark;
  - the ANN recall, if it was measured.

  The actions are Create index (a dialog that posts a `VectorIndexConfig`), Rebuild and
  Drop. They are disabled on read-only servers, and their tasks appear in `TaskList`.
- `ui/mock/server.mjs` gains the `/$/vector/*` endpoints and a small `spk:vectorSearch`
  emulation, so the UI can be built against the mock.

## 3. Standards basis

- **RDF 1.2 Concepts.**
  - §5 defines a datatype by its lexical space, value space and lexical-to-value mapping.
    §4.1 defines `spk:vector` in those terms.
  - §3.4.2 says ill-typed literals are accepted, not rejected.
  - §5.2 says implementations need not recognise every datatype.
  - Appendix A.3 defines `rdf:JSON`. §8 explains why this design does not use it.
- **SPARQL 1.1 Query.**
  - §17.6 covers extensible value testing: functions named by IRI, whose errors are type
    errors.
  - §17.3 defines RDFterm-equal for unknown datatypes.
  - §18 defines BGP and join semantics. The property function's result joins under them.
  - §13 defines RDF datasets and the active graph, which set the search scope.
- **RFC 8259** §6 gives the number grammar that the lexical grammar reuses.
- **IEEE 754-2019** defines binary32 and conversion by round-to-nearest, ties to even.
- **RFC 8141** §5.1 says formal URN namespace IDs must not start with `X-`, so a
  `urn:x-…` IRI cannot collide with a registered URN namespace.
- **Apache Jena ARQ property functions** take list arguments in subject and object
  position. The `jena-text` form `(?s ?score) text:query (…)` is the syntactic precedent,
  and F03 uses the same shape.
- **Malkov & Yashunin, HNSW** (arXiv:1603.09320, IEEE TPAMI 2020) is the ANN algorithm.
  Its parameters are M (connectivity) and ef (expansion).

## 4. Semantics

### 4.1 Datatype `spk:vector` (IRI `urn:x-sparkles:vector`)

**Why `urn:x-sparkles:`.** Sparkles already mints `urn:x-sparkles:inferred`, and Jena uses
`urn:x-arq:` the same way. The IRI needs no domain, and by RFC 8141 it cannot collide with
a registered URN namespace. An `https://sparkles.dev/…` IRI would require owning that
domain, and that ownership has not been verified. The datatype IRI is stored in user
data, so the choice must be final before the first release (§9).

**Lexical space.** A vector is written as a JSON array of RFC 8259 numbers:

```
vector = ws "[" ws number *( ws "," ws number ) ws "]" ws
number = [ "-" ] ( "0" / %x31-39 *DIGIT ) [ "." 1*DIGIT ] [ ( "e" / "E" ) [ "+" / "-" ] 1*DIGIT ]
ws     = *( %x20 / %x09 / %x0A / %x0D )
```

These forms are outside the lexical space, so their literals are ill-typed:
- forms with `NaN`, `Infinity`, `+1`, `.5`, `1.` or hex numbers, an empty array `[]`, or
  nested arrays;
- forms with more than `MAX_DIM = 16384` elements, or longer than 1 MiB;
- forms with an element whose nearest binary32 is ±∞, meaning its magnitude is 2^128 or
  more after rounding.

**Value space.** The values are finite sequences of 1 to 16384 finite binary32 numbers.
Each number maps to the nearest binary32, with ties to even. The parser checks the
grammar above and then calls Rust's `str::parse::<f32>`, which rounds correctly. `-0` maps
to −0.0, and subnormals are kept.

**Term identity.** Sparkles never rewrites a literal. `"[1, 0]"` and `"[1.0,0.0]"` are
different RDF terms. Both are stored and returned byte for byte, and they are indexed as
two rows with equal values.

**Canonical form.** Sparkles uses this form only for literals it creates itself
(`vector::literal`, CLI output). It is `[`, then the elements joined by `,` with no
spaces, then `]`. Each element is written in Rust's shortest round-trip `{:?}` form, such
as `1.0`, `0.1`, `1e-7` or `-0.0`, which is always a valid JSON number.

**Precision.** Storage and arithmetic use f32. A score is widened exactly to f64 for the
`xsd:double` result. The low 29 mantissa bits of the widened value are zero, so it always
fits Sparkles' inline `Tag::Double` id, and scores never touch a vocabulary.

**Normalization.** Stored values are never normalized. Cosine uses precomputed norms. A
zero-norm vector has no cosine: the function returns a type error, search skips the row
(`skipped.zeroNorm`), and a zero query vector with cosine gets `400`. Dot and euclidean
accept zero vectors.

### 4.2 Index identity: dimension and model

- **Partitions.** A search runs over one partition, keyed by *(predicate, dimension)*,
  where the dimension is the query vector's. Rows of any other dimension are never scored
  with it.
- **Implicit partitions** need no configuration, so small datasets work without setup.
  `implicit_partitions = true` is the default.
- **Configured indexes.** A configured index fixes the predicate, the dimension, the
  metric (used for ANN and as the default metric) and an optional model label. It skips
  rows of any other dimension and counts them in `skipped.wrongDimension`. A query of
  another dimension against a configured predicate gets `400`.
- **Uniqueness.** A predicate has at most one configured index. A second one gets `409`.
- **Models.** The model label is only a label. Sparkles cannot tell apart two models with
  the same dimension, so the documented practice is one predicate per model, such as
  `ex:embMiniLM` and `ex:embE5`.
- **Multiple embeddings per entity** are multiple rows (distinct quads), and top-k counts
  rows. With `distinct:subject` it counts entities instead, each represented by its best
  row.

### 4.3 Kernel (shared by functions, exact search and ANN re-scoring)

The kernel keeps eight f32 accumulators. For vectors of length n it computes
`acc[i mod 8] += x_i` for i = 0..n-1, then sums the accumulators as
`((acc0+acc4)+(acc1+acc5))+((acc2+acc6)+(acc3+acc7))`. Rust never fuses a `mul` and an
`add` into an FMA on its own, and the lanes are independent. The result is therefore
bit-identical whether or not LLVM auto-vectorizes the loop (SSE, AVX2 or NEON).

| Metric | Formula |
|---|---|
| `dot(a,b)` | Σ aᵢbᵢ |
| `euclidean(a,b)` | √(Σ (aᵢ−bᵢ)²) |
| `cosine(a,b)` | `dot(a,b) / (√dot(a,a) · √dot(b,b))`, clamped to [-1, 1] |

Norms use the same kernel, so a stored norm equals a recomputed one bit for bit. A
non-finite result (overflow) yields `None`. The function then raises a type error. Search
skips the row and writes a debug log, but does not count the row as skipped.

### 4.4 Search semantics

Let **R** be the snapshot's visible quads `(s, p, o, g)` where the active-graph filter
accepts `g` and `o` is a well-typed `spk:vector` of the query's dimension. For cosine,
`o` must also have a non-zero norm.

- **Selection.** The result is the k rows of R with the best scores. Ties break by
  ascending raw id of `(s, o, g)`. That order is deterministic within a snapshot but can
  change after compaction (§9).
- **Duplicates across graphs.** When the scope spans several graphs and the pattern has
  no graph variable, rows with equal `(s, o)` are one solution. RDF merge semantics
  apply: the default graph is a merge, not a bag.
- **Entity queries.** The entity's own row is part of R.

Errors are raised when the query is planned, or during execution for entity lookups. All
of them map to `400` through `Error::Invalid`:

| Condition | Message (prefix) |
|---|---|
| The object is not a list, the list is malformed, or the predicate argument is not an IRI. | `spk:vectorSearch: expected (predicate query [k] [options])` |
| The query literal is malformed. | `malformed spk:vector literal at offset N: …` |
| No partition has the query's dimension but others exist, or the dimension differs from a configured index's. | `dimension mismatch: <p> has vectors of dimension 384; query has 768` |
| `k` is not a positive integer, or exceeds `max_k`. | `spk:vectorSearch: k must be 1..=10000` |
| An option or metric is unknown. | `spk:vectorSearch: unknown option "…"` |
| The entity has more than one vector in scope. | `entity <x> has 2 vectors for <p>; pass a vector literal` |
| The query vector is zero and the metric is cosine. | `cosine is undefined for a zero query vector` |

If the predicate has no vectors at all, the result is empty, as with any BGP. This is not
an error.

Two more conditions map to existing status codes:
- a timeout or cancellation during a scan, or while waiting for a build: `408` / `503`;
- a partition that would exceed the memory budget: `507` (`Error::MemoryLimit`,
  "vector partition <p>/768 needs 2.9 GiB; 1.1 GiB of 4.0 GiB free").

### 4.5 Consistency, freshness and candidates

- **MVCC.** A search on snapshot S sees exactly S's quads. The derived base segment
  covers the generation's base, and each query overlays the snapshot's delta on it.
  `del[PSO]` keys with prefix `[p]` remove base rows. `ins[PSO]` keys add rows, and their
  literals are parsed through a per-generation cache from id to vector. There is no
  staleness window, and the commit path does no extra work.
- **Variable query** (Phase 1b). When the `query` argument is a variable, the rest of the
  group binds it, as with a path bound from the left. The operator runs once per distinct
  bound vector, up to `max_query_vectors = 1000`, and returns `507` beyond that.
  Solutions carry the input binding.
- **`candidates:join`** (Phase 1b). The result is `Join(Rest, TopK(σ_{s ∈ π_s(Rest)} R))`,
  where Rest is the group without this pattern. It is the k best rows among entities that
  also satisfy the rest of the group. This definition is declarative. The planner
  evaluates Rest first and passes its distinct `?s` ids to the search as a sorted filter.
- **ANN** (Phase 2) serves only snapshots of the store's current generation (the head).
  The ANN graph covers the base segment. Overlay inserts are scored exactly and merged
  in. Overlay deletes and the graph and candidate filters become the ANN filter
  predicate. Search uses the exact path instead when any of these holds:
  - `exact:true` is set;
  - the ANN build is not ready;
  - the metric differs from the index metric;
  - at most `exact_threshold` rows (default 20 000) are accepted;
  - the filtered ANN search returned fewer than k rows although at least k rows are
    accepted.

  Historical snapshots, reached through the `?at=` parameter of a later spec, use exact
  search. If their generation's segment is gone, they get `501`.

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

The budget also counts the overlay cache. Memory-mapped files count as fully resident,
which is conservative and deterministic.

**Worked example: 1M × 768.**

| Component | Size |
|---|---|
| segment | 3.10 GB (2.86 GiB raw f32 + 28 MB ids and norms) |
| ANN f16 | 1.54 GB (1.43 GiB) |
| ANN links | ≈ 0.14 GB |
| **Total** | **≈ 4.8 GB (4.46 GiB)** |

With i8 quantization the total is about 3.7 GiB.

The lexical forms also live in the vocabulary, on disk and memory-mapped, outside the
budget. At about 12 bytes per element they take about 9 GB. The status reports them
separately as an estimated `lexicalBytes`, and §9 proposes a compact alternative
datatype.

**Admission.** Sparkles refuses a partition or ANN build whose estimate exceeds the free
budget. The index state becomes `over-budget`. An index without its ANN graph falls back
to exact search. Without its segment, searches on that partition get `507`.

Implicit partitions are evicted, least recently used first, before a configured one is
refused. Configured segments are pinned.

## 5. Design sketch

**Modules.**
- `crates/sparkles-core/src/vector/mod.rs` holds lexical parsing and formatting, `Metric` and
  the kernel. It has no dependencies and is always compiled.
- `vector/segment.rs` holds packed partitions.
- `vector/search.rs` holds exact top-k and the overlay.
- `vector/registry.rs` holds configuration, status and the budget.
- `vector/ann.rs` sits behind `feature = "vector-hnsw"` and defines a backend trait:
  `build(&Segment) -> Ann`, `search(q, k, ef, filter: &dyn Fn(u64) -> bool) -> Vec<u64 /*row*/>`,
  `save/view(path)`, `memory_bytes()`.

**Cargo features (crate `sparkles`).**
- `vector` is on by default and adds no dependencies. It gates the property function,
  the functions and the registry.
- `vector-hnsw` = `["vector", "dep:usearch"]` is off by default, so embedded users pay
  nothing for ANN.

`sparkles-server` enables `vector-hnsw` in its default features. That needs a C++
compiler, which the Nix stdenv already provides. In a build without `vector`, the planner
still recognises `spk:vectorSearch` and returns `501 "built without vector support"`
rather than silently matching nothing.

**Store integration (`store.rs`).**
- `Generation` gains `vectors: vector::GenerationVectors`. It maps
  *(predicate id, dim)* to `Arc<OnceCell<Result<Segment>>>` and holds the overlay parse
  cache. It lives on the generation, so it is dropped with the generation, and the ids
  inside it always belong to that generation.
- `Store` gains `vector: Arc<vector::Registry>`, which holds the configuration, the budget
  counter and the build tasks. `Snapshot` reaches it through the store handle that `Ctx`
  already carries via `snap`. This adds one `Arc` field to `Snapshot`, like `results`.
- **Segment.** A segment holds the rows of `Perm::Pso` with prefix `[p]` from the base
  generation only, not the delta. It reads them with `PermIndex::for_each_range_until`
  and stores:
  - `ids: Vec<[u64;3]>`: s, o and g in PSO order, so duplicates across graphs sit next to
    each other;
  - `norms: Vec<f32>`;
  - `data: Vec<f32>`, row-major and 64-byte aligned.

  Literals are parsed in parallel per block with rayon, and `Vocab::get_sorted` batches
  the key reads.
- **Overlay.** For each query, the `[p]` range of `snap.delta.del[Pso]` goes into an
  `FxHashSet<[u64;3]>`. The `[p]` range of `snap.delta.ins[Pso]` is parsed through the
  per-generation cache (`FxHashMap<u64 obj id, Option<Arc<[f32]>>>`). Delta and vocabulary
  ids are stable within a generation. `store::apply` keeps `ins` disjoint from the base
  and `del ⊆ base`, so R = (segment − del) ∪ ins exactly.

**Planner (`sparql/plan.rs`).**
- In `plan_group`, before filter-equality substitution, the planner finds triples whose
  predicate is the constant `spk:vectorSearch`. Blank nodes are already hidden variables
  named `" bn…"`. spargebra's `Collection` rule expands a list into two triples per
  element, ending in `rdf:nil`, so the rewrite follows the `rdf:first` / `rdf:rest`
  chains among the group's triples. Each list node must have exactly one `first`, one
  `rest` and no other use. The rewrite then removes those triples.
- It emits `Item::Node(Node::leaf(Kind::VectorSearch(Box<VectorSearchSpec>), vars, est, desc))`.
  - `est` is min(k, partition rows).
  - Graph handling reuses `graph_filter(&t.graph)` and binds the graph variable when
    there is one.
  - `desc` reads like `VectorSearch <emb> cosine k=10 dim=768 exact rows=1.0M (+12 −3)`,
    with the query vector abbreviated.
- `VectorSearchSpec { pred: Id, query: QueryArg /* Vector(Arc<[f32]>) | Entity(Id) | Var(VarId) */, k, metric, opts, graph: GraphFilter, graph_var, out: [Option<VarId>; 3], dedup }`.
- `candidates:join` makes the node depend on the rest of the group. It becomes a kind
  that takes a child, like `Path { bound_from_left }`, with Rest as child 0.

**Executor (`sparql/exec.rs`).**
- `Kind::VectorSearch` splits the segment into chunks of 4096 rows and runs them on
  rayon. Each chunk:
  - calls `ctx.check()`;
  - skips rows that `graph.accepts(g)` rejects, rows in the del set, adjacent duplicate
    `(s, o)` rows when `dedup` is set, and rows outside the candidate set;
  - keeps a bounded binary heap of size k, keyed by (goodness, Reverse(ids)).
- The chunk heaps are merged, and then the overlay rows are pushed. When `dedup` is set,
  an `(s, o)` set guards the heap against duplicates.
- The output is a `Table` in best-first order. `score` is `Id::from_f64(f as f64)`, which
  always succeeds (§4.1).
- `describe` reports the operator as "VectorSearch".
- In `cache.rs`, Phase 1 marks the kind as not cacheable. Phase 2 keys it on (spec,
  query-vector hash, engine mode).

**Functions (`sparql/expr.rs`).**
- `extension()` gains a branch for `SPK = "urn:x-sparkles:"`, and `is_extension` accepts
  it.
- Parsed arguments are memoised by id in a small `Ctx` map (≤ 64 MiB), so a BIND over
  many rows parses each distinct literal once.

**Persistence (Phase 1b).**
- **Configuration** lives in `<root>/vector.json`
  (`{ "version": 1, "indexes": [{ "name", …VectorIndexConfig }] }`) and is written with
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
- **ANN file** (Phase 2): `<name>.ann`, the backend's own file, opened with the backend's
  mmap view. Next to it, `<name>.ann.json` holds
  `{ format_version: 1, backend, backend_version, base_seq, config_hash, rows, quantization, connectivity, expansionAdd }`.
- **Watermark.** `base_seq` is the commit `seq` folded into the generation
  (`IndexMeta.base_seq` in the commit-identity spec). Until that lands, the watermark is
  the tuple `(generation name, meta.created, meta.quads)`.
- **Validity on open.** A derived file is valid only if its magic, version,
  `config_hash`, `base_seq`, row count and footer checksum all match. Otherwise Sparkles
  deletes and rebuilds it. The data section has no checksum, because the atomic rename
  guarantees that the file is complete.

**Crash safety.**
- A build writes `*.tmp`, fsyncs it, renames it, and then fsyncs the directory. Opening a
  store deletes leftover `*.tmp` files.
- Derived files are never part of a commit. A crash at any point loses at most the work
  in progress, and queries rebuild lazily.
- Compaction and bulk rebuilds create a new generation directory with no `vectors/`.
  After `rebuild_locked` switches `CURRENT`, the registry queues background builds for
  every configured index, segments first and then ANN.
- Queries that arrive during a build wait on the same `OnceCell` until their deadline.
  At the deadline they get `408` with `"vector index <name> is building (37%)"`.
- Removing an old generation directory removes its derived files with it.
- A read-only server builds segments in memory only and never writes them.

## 6. Phasing

**Phase 1 (MVP, about 1 day).**
- `vector/mod.rs`: the grammar, parsing, the canonical form and the kernel. Unit tests
  cover grammar edge cases, kernel determinism and the function examples of §7.
- `spk:cosine`, `spk:dot`, `spk:euclidean` and `spk:dimension` in `expr.rs`.
- The `spk:vectorSearch` rewrite, supporting a constant query (literal or entity), a
  constant k and the `metric:` option. A variable query returns `501`.
- Exact search over in-memory segments built lazily per (p, dim). It includes the delta
  overlay, graph scoping, dedup, a bounded heap, cancellation through `ctx.check()`, and
  the budget check (`507`). Rayon is optional in Phase 1.
- Tests: the SPARQL examples of §7 in `sparql/tests.rs`.

**Phase 1b (exact search, complete).**
- `candidates:join`, variable queries and `distinct:subject`.
- Configured indexes (`vector.json`) and persisted segments with a watermark.
- Background builds after a generation switch, the `/$/vector` endpoints,
  `sparkles vector`, `DatasetStats.vector`, and result caching.
- The `/similar` UI route, the dataset-page section and the mock endpoints.

**Phase 2 (HNSW).**
- `vector-hnsw` with the USearch backend (§8 compares the libraries). It provides
  filtered search, f16 quantization by default, and re-scoring with the §4.3 kernel.
- ANN for the head generation only, the exact fallback rules, the recall endpoint and
  the recall display in the UI.
- Acceptance: recall@10 ≥ 0.95 against the exact oracle on a clustered synthetic set
  (100k × 128), with a p50 latency at least 10× lower than exact search.

**Phase 3 (later).**
- Background ANN catch-up of overlay inserts with USearch `add`/`remove`. The ANN file's
  watermark advances to the last applied `seq`.
- Keeping the ANN graph across compaction by remapping row keys through term keys.
- Rewriting `ORDER BY DESC(spk:cosine(?v, C)) LIMIT k` to `VectorSearch`.
- Hybrid ranking with F03 `text:query`, for example reciprocal-rank fusion as a function
  over two scored patterns.
- A compact datatype (§9).

## 7. Acceptance examples

The examples use this fixture, with
`PREFIX ex: <http://example.org/> PREFIX spk: <urn:x-sparkles:>`. Scores match within
|Δ| ≤ 1e-6 unless marked exact.

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
   returns `a 1.0`, `b 0.8`, `c 0.0`. It leaves out e (zero norm), f (another
   dimension), g (malformed) and h (in a named graph, outside the scope).
2. **Named graphs.** `SELECT ?s ?g ?score { GRAPH ?g { (?s ?score) spk:vectorSearch (ex:emb "[1,0,0]"^^spk:vector 5) } } ORDER BY DESC(?score)`
   returns `a ex:g1 1.0`, `a ex:g2 1.0` and `h ex:g1 0.6`, one row per graph.
3. **Union default.** With `--union-default-graph`, the default graph is the union of the
   named graphs, and query 1 with k=2 returns `a 1.0`, `h 0.6`. The `(a, o)` pair in
   ex:g1 and ex:g2 is one solution, so it does not crowd out h.
4. **Join after top-k.** `{ ?s a ex:Doc . (?s ?score) spk:vectorSearch (ex:emb "[0,1,0]"^^spk:vector 2) }`:
   the top 2 are c (1.0) and b (0.6), and the join keeps only `b 0.6`. With
   `"candidates:join"` (Phase 1b) the result is `b 0.6`, `a 0.0`.
5. **Euclidean.** `(?s ?score) spk:vectorSearch (ex:emb "[1,0,0]"^^spk:vector 3 "metric:euclidean")`
   returns `a 0.0`, `b 0.6324555`, `e 1.0`.
6. **Other dimensions.** Query `"[1,0]"` returns `f 0.4472136` (cosine). Query
   `"[1,0,0,0]"` returns `400 {"error":"dimension mismatch: <http://example.org/emb> has vectors of dimension 2, 3; query has 4"}`.
7. **Malformed query and bad k.** `"[1, x]"^^spk:vector` returns `400 malformed spk:vector literal at offset 4`.
   `k = 0` returns `400`. `(ex:emb "[1,0,0]"^^spk:vector 10 "foo:bar")` returns `400 unknown option`.
8. **Entity query.** `(?s ?score) spk:vectorSearch (ex:emb ex:b 2)` returns `b 1.0`, `a 0.8`.
   Querying `ex:zzz`, which has no vector, returns 0 rows. After adding
   `ex:b ex:emb "[0,0,1]"^^spk:vector`, querying ex:b again returns
   `400 entity … has 2 vectors`.
9. **Functions.**

   | Expression | Result |
   |---|---|
   | `spk:dot("[1,2,3]"^^spk:vector, "[4,5,6]"^^spk:vector)` | `32.0e0` (exact) |
   | `spk:euclidean("[0,0]"^^spk:vector, "[3,4]"^^spk:vector)` | `5.0e0` (exact) |
   | `spk:dimension("[1,2,3]"^^spk:vector)` | `3` |
   | `BIND(spk:cosine(?v, "[0,0,0]"^^spk:vector) AS ?x)` | unbound |
   | `spk:cosine("[1,0]"^^spk:vector, "[1,0,0]"^^spk:vector)` | type error |
   | `spk:cosine("[1,0]", …)` (plain string) | type error |

10. **MVCC and round-trip.** After `INSERT DATA { ex:z ex:emb "[1.0, 0.0,0]"^^spk:vector }`,
    query 1 with k=2 returns `a 1.0`, `z 1.0`. The tie breaks this way because the base
    id is lower than the delta id. `SELECT ?v { ex:z ex:emb ?v }` returns the exact
    lexical form `"[1.0, 0.0,0]"`. After `DELETE DATA { ex:a ex:emb "[1, 0, 0]"^^spk:vector }`,
    query 1 with k=2 returns `z 1.0`, `b 0.8`. A reader holding the snapshot from before
    the delete still sees a.
11. **Compaction invariance.** After `POST /$/compact/ds`, queries 1–10 return the same
    rows and scores. Only the order among equal scores may change.
12. **Cancellation.** A 200k × 256 exact search with `timeout=0.001` returns `408`.
13. **Budget.** With `--vector-mb 1`, a 10k × 128 search returns `507` with the "needs …
    free" message.
14. **HTTP (1b).** On the fresh fixture, where every graph is in the segment,
    `PUT /$/vector/ds/emb3 {"predicate":"http://example.org/emb","dimension":3,"metric":"cosine"}`
    returns `201` and a task. A `GET` then shows `state:"ready"`, `rows:8` and
    `skipped:{malformed:1, wrongDimension:1, zeroNorm:1}`. The 8 rows include e, because
    dot and euclidean can use it. Then:
    - **After a restart:** the index is still `ready` with the same `watermark`, and the
      `.spkv` file was not rewritten (its mtime is unchanged).
    - **After the footer is corrupted:** the index rebuilds.
    - **Without `vector-hnsw`, `PUT … "ann":{}`:** returns `501`.
15. **CLI (1b).** `sparkles vector search --loc db --predicate http://example.org/emb --vector '[1,0,0]' -k 2`
    prints two rows, `<http://example.org/a> 1.0` and `<…/b> 0.8`, and exits 0.
    `--vector '[1,0,0,0]'` exits 1 with the dimension message.
16. **Recall (2).** On 100k clustered vectors, the recall endpoint reports ≥ 0.95 at k=10,
    ef=64. With `exact:true`, the results equal the Phase 1 output bit for bit.

## 8. Rejected alternatives

- **Canonicalizing vector literals on load.** It would break Sparkles' exact term
  identity, which the README's decision on canonical inlining sets. The lexical form must
  round-trip.
- **A new id `Tag` or inline ids for vectors.** Vectors do not fit in 60 bits, and tags
  are scarce: there are 4 bits and 12 values are used. The segment already stores vectors
  out of line, keyed by vocabulary id.
- **`rdf:JSON` as the datatype.** Its value space is any JSON value, and its numbers are
  `xsd:double`. It cannot express a finite f32 vector of fixed dimension, and reusing a
  W3C datatype with narrower semantics would mislead other tools.
- **An `rdf:List` of `xsd:float`.** That takes 768 triples per vector, or 768M triples for
  1M vectors.
- **`xsd:base64Binary`.** It is opaque and not human-readable. It remains a candidate for
  a *second*, compact datatype (§9).
- **A magic `SERVICE spk:vector { … }`.** It would overload federation semantics
  (endpoint evaluation, SILENT, variable scoping) in the existing `Kind::Service` path.
  The property-function list form parses with spargebra unchanged and matches Jena's
  `text:query` and F03.
- **Only functions plus `ORDER BY … LIMIT`.** Every query would be a full scan and sort,
  with no ANN and no graph-scoped top-k. It stays as a possible later rewrite.
- **Normalizing at ingest for cosine.** It would change the values that dot and euclidean
  see on the same data. Precomputed norms cost only 4 bytes per row.
- **f64 storage.** It takes twice the memory, and embeddings are produced as f32 anyway.
- **Maintaining HNSW on the commit path.** It adds latency and ties crash recovery to a
  C++ structure. The delta overlay already gives exact freshness, and background
  catch-up is Phase 3.
- **ANN libraries.** Versions were checked on crates.io on 2026-09-30.

  | Crate | License | Version | Assessment |
  |---|---|---|---|
  | **USearch** | Apache-2.0 | 2.26.2, 2026-08-31 | Recommended. It has `filtered_search` with any key predicate, `remove`, `save`/`view` (mmap), `memory_usage`, f16/bf16/i8 quantization and `exact_search`, and it is `Send + Sync`. The downsides: it is C++ through `cxx`, so it needs a C++ toolchain, and static and musl builds need checking. It is not unwind-safe, so calls must be wrapped and the filter must never panic. The licenses of its optional SIMD dependencies need a `cargo deny` check before adoption. |
  | **hnsw_rs** | MIT/Apache-2.0 | 0.3.4, 2026-02 | Pure Rust, with rayon `parallel_insert`, `search_filter`, and dump and reload with memory-mapped data. No deletion or quantization, and a larger dependency tree (bincode, mmap-rs, anndists). **The fallback** if the maintainer prefers pure Rust (§9). The backend trait makes swapping cheap. |
  | instant-distance | MIT OR Apache-2.0 | 0.6.1, 2023-06 | No filtered search or deletion. Stale. |
  | hnsw (rust-cv) | MIT | 0.11.0, 2021 | Unmaintained. |
  | arroy / hannoy | MIT | 0.8.0 / 0.2.0 | Backed by LMDB, which would put a second storage engine inside Sparkles. |
  | FAISS bindings, LanceDB | — | — | A heavy native or dependency footprint for one index type. |

## 9. Open questions (defaults chosen; maintainer may revisit)

1. **Namespace.** `urn:x-sparkles:` or an owned `https://` vocabulary IRI. The IRI is
   persisted in data, so this must be settled before release.
2. **Score datatype.** `xsd:double` fits inline ids and is cheap. `xsd:float` matches the
   true precision but needs a local-vocabulary entry per row.
3. **Tie-break.** Raw ids are cheap and deterministic within a snapshot. Term order is
   stable across compaction but needs key reads at the boundary.
4. **k.** Keep the default of 10, or make k mandatory.
5. **Query entity.** Whether it appears in its own results. It does now, and the UI
   hides it.
6. **Implicit partitions.** Whether large stores should require a configured index, for
   example by refusing implicit partitions above 1M rows.
7. **USearch or hnsw_rs.** USearch has more features. hnsw_rs matches the README's
   preference for pure Rust (LZ4 over zstd).
8. **Strict writes.** An optional mode that rejects ill-typed `spk:vector` literals on
   write. The default accepts and counts them.
9. **Compact datatype.** A second datatype, `spk:vectorB64`, holding base64 of
   little-endian binary32, at about 5.3 bytes per element instead of about 12. It would
   serve large corpora, where lexical text dominates disk use.
10. **Mapped memory.** Whether memory-mapped segments count fully against the budget, as
    they do now, or only as page cache.
11. **Read-only servers.** Whether they may persist rebuilt segments. Today they build
    them in memory only.

## 10. Sources

- **Sparkles repository** (read only):
  - `README.md` and `docs/API.md`: endpoints, error mapping, and the decisions on exact
    term identity and the pure-Rust preference.
  - `crates/sparkles-core/src/`:
    - `id.rs`: tags and inline doubles (the basis of the f32 widening fact);
    - `vocab.rs`: base and delta vocabularies, key layout;
    - `store.rs`: generations, the delta and the `apply` invariants, the WAL,
      `rebuild_locked`, snapshots;
    - `builder.rs`: `IndexMeta`;
    - `error.rs`;
    - `sparql/expr.rs`: extension function dispatch;
    - `sparql/plan.rs`: `plan_group`, `graph_filter`, blank-node variables, `Kind`;
    - `sparql/exec.rs`, `sparql/ctx.rs` (cancellation, dataset), `sparql/cache.rs`,
      `sparql/table.rs`.
  - `crates/sparkles/src/dataset.rs`.
  - `crates/sparkles-server/src/{http.rs,state.rs,main.rs}` and both `Cargo.toml` files.
  - `ui/src/routes/*`, `ui/src/lib/api.ts`, `ui/mock/`.
- **spargebra 0.4.7 source** (MIT OR Apache-2.0), `spargebra-0.4.7/src/parser.rs` in the
  published crate: the `Collection` rule, which expands `rdf:first`/`rdf:rest` with fresh
  blank nodes, and `build_bgp`.
- **W3C RDF 1.2 Concepts**, https://www.w3.org/TR/rdf12-concepts/: §3.4.2 ill-typed
  literals, §5 datatypes, §5.2 unknown datatypes, A.3 `rdf:JSON`.
- **RFC 8141**, https://www.rfc-editor.org/rfc/rfc8141.html, §5.1: the `X-` NID
  restriction.
- **Apache Jena documentation (Apache-2.0)**:
  - https://jena.apache.org/documentation/query/extension.html: property functions and
    list arguments;
  - https://jena.apache.org/documentation/query/text-query.html: the subject and object
    list forms of `text:query`, score output, and GRAPH scoping.
- **crates.io API**, consulted 2026-09-30, for versions, licenses and dates:
  `usearch`, `hnsw_rs`, `instant-distance`, `hannoy`, `arroy`, `hnsw`.
- **docs.rs**:
  - `usearch`: the `Index` methods, `IndexOptions`, `MetricKind` (L2sq is squared) and
    `ScalarKind`, and the crate page for the `cxx` build dependency and features;
  - `hnsw_rs`: the `Hnsw` methods and the crate README (distances, dump and reload, mmap,
    filtering, no deletion).
- **Cited from the standards and literature, not re-fetched**:
  - SPARQL 1.1 Query (§13, §17.2, §17.3, §17.6, §18);
  - RFC 8259 §6;
  - IEEE 754-2019 binary32;
  - Malkov & Yashunin, arXiv:1603.09320.

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30 (`9b06c89`). It added the `spk:vector`
datatype (`urn:x-sparkles:vector`) with the §4.1 grammar, the functions `spk:cosine`,
`spk:dot`, `spk:euclidean` and `spk:dimension`, and an exact top-k `spk:vectorSearch`
leaf. The query is a vector literal or an entity. `k` defaults to 10, with a maximum of
10,000, and the only option is `metric:`. The first search of a (predicate, dimension)
pair packs its base vectors, and the packed vectors are cached per generation. Every
query overlays the inserts and deletes in its snapshot that compaction has not yet
merged, so results match the snapshot's data exactly. The search is scoped to the active
graph. In a merged default graph, rows with the same subject and vector count once.
Scores are `xsd:double` and ties break by term id, which are the §9 defaults. Literals
are stored and returned exactly as written. A malformed literal is stored but never
matched.

Only a small introspection piece of Phase 1b was built (`cb598c8`). The read-only
`GET /$/vector/{ds}` reports the budget, the bytes used, and the packed predicates and
dimensions of the current generation. `sparkles serve --vector-memory-mb` (default 4096)
replaces the spec's `--vector-mb`. The budget is process-wide. Past it, a search answers
`507` through the shared budget error of [C01](C01-observability-and-budgets.md). Search
outputs also count against the per-query memory budget. The UI shows "Similar" in the
explorer instead of a separate `/similar` page, and the MCP server
([C11](C11-mcp-server.md)) exposes the search as its `similar_entities` tool.

Phases 1b and 2 landed on 2026-10-02.
* **Configured indexes.** `vector.json` holds the indexes of a dataset by name, each with
  its predicate, dimension, metric, model label, HNSW settings (`m`, `efConstruction`,
  `efSearch`) and `exactThreshold`. `PUT`, `GET` and `DELETE /$/vector/{ds}/{name}`,
  `POST …/rebuild` and `sparkles vector create|drop|rebuild|list|status` manage them,
  locally or with `--server`. A predicate has at most one index (`409`), and a query of
  another dimension against an indexed predicate gets `400`.
* **Builds.** A build runs on a background thread from a snapshot, without the writer
  lock, while writes go on. It packs the predicate's base vectors, publishes them (searches
  then scan them exactly), builds the graph, writes `gen-NNNN/vectors/<name>.spkv`, and
  maps it. Opening a store maps the file when its configuration hash, base commit, quad
  and term counts and the checksum of its header and ids match. Otherwise the index is
  built again. A compaction or bulk load starts a build for the new generation, and
  writes to the replaced generation's directory stop. A new `efSearch`, threshold or
  model keeps the build. `sparkles check` validates `vector.json` and the files, and
  clones and backups keep `vector.json`.
* **HNSW.** The graph follows the paper: levels from `⌊−ln(U)·mL⌋`, insertion by
  Algorithm 1 with the neighbour heuristic of Algorithm 4 (keeping pruned candidates in
  free places), and search by Algorithms 2 and 5, with rayon-parallel insertion and a
  lock per node. It holds only links between node numbers and reads the vectors from the
  packed segment, so it adds about 136 bytes per vector at M = 16. The rows of one
  (subject, vector) pair in several graphs are one node.
* **Searches through the graph.** The graph returns `ef` candidates (at least k) among
  base rows the active graph accepts and the snapshot has not deleted. They are scored
  with the §4.3 kernel, merged with the snapshot's inserted rows (scored exactly, parsed
  once per generation), and cut to k. A row's score is the same on every path. The exact
  path runs instead when `exact:true` is set, the snapshot is a past state, the metric
  differs from the index's, the graph is still being built, at most `exactThreshold`
  rows are in scope, or the active graph holds less than 5 % of the rows. A graph search
  that finds fewer than k rows while more exist widens `ef` four times over, up to 4096,
  and then falls back. Plan counters report `method`, `exactBecause`, `ef`, `rows`,
  `scored` and the overlay counts.
* **Bound queries.** A variable query, or `candidates:join`, makes the search a node over
  the rest of its group, attached after the group's joins, paths and triple-term
  unpacking. It runs once per distinct query value (literal or entity, at most 1000,
  else `507`), and `candidates:join` scores only the rows of the bound subjects, found by
  binary search in the PSO-ordered segment, always exactly. `distinct:subject` keeps the
  best row per subject.
* **Recall endpoint.** `POST /$/vector/{ds}/{name}/recall` answers synchronously with
  `{ k, samples, ef, recall, hnswMs, exactMs }`, using stored vectors as queries.

**Deviations.**
* The vector code is always compiled, and so is the HNSW graph. There is no `vector` or
  `vector-hnsw` cargo feature, because neither adds a dependency. USearch was measured
  against hnsw_rs, instant-distance and a graph written for Sparkles
  ([PROVENANCE](PROVENANCE.md#vector-similarity-and-vector-indexes)). The Sparkles graph
  matched USearch's recall and latency, reads the packed vectors instead of copying
  them, takes `ef` per query, and needs no C++ toolchain, so no ANN crate was added.
  There is no quantization (§2.4 `quantization`).
* Past states are always searched exactly, also in the current generation where the
  graph would answer them correctly. `?at=` reads of an older generation pack that
  generation's vectors on demand, as Phase 1 did.
* Packed vectors and graphs share the process-wide `--vector-memory-mb` budget, now a
  global flag. Mapped files count in full. An index over budget is `over-budget`, and
  searches of its predicate scan exactly, which needs the same budget.
* The default `exactThreshold` is 10,000 rows, not 20,000.
* `PUT` answers `{ index, task }` once the configuration is written, and the task
  follows the build. The recall endpoint is synchronous, not a task.
* `DatasetStats.vector` is not built. The UI reads `GET /$/vector/{ds}` instead.
* Vector search results are not cached.

**Tests at landing.**
* `crates/sparkles-core/tests/vectors.rs`: the §7 examples, bound queries, `candidates:join`
  and `distinct:subject`.
* `crates/sparkles-core/tests/vector_index.rs`: recall@10 against the exact search on fixed
  seeds (at least 0.95 at ef = 64), equal scores on both paths, random inserts, deletes
  and re-inserts after a build, past states, files across reopens and damage, builds
  while writes go on, and configuration errors.
* Unit tests of the grammar, kernel, graph, file format and configuration in
  `crates/sparkles-core/src/vector/`.
* The server's router tests and `crates/sparkles-server/tests/cli_vector.rs` cover the
  HTTP API and the CLI, locally and against a server.

**Performance.** `scripts/bench-vector.sh` (`mise run bench:vector`) measures build time,
memory, recall@10 and latency over HTTP. The first numbers were taken on a 16-core
machine busy with other compiles (load about 30), so they are rough. The vectors are
clustered (1000 centres, Gaussian noise), cosine, with M = 16, efConstruction = 128 and
1000 queries. Latency is per query through a keep-alive HTTP client, median then p99.

| Vectors | Build | Packed vectors + graph | Exact p50 | ef=32 | ef=64 | ef=128 | ef=256 |
|---|---|---|---|---|---|---|---|
| 100k × 384 | 16 s | 149 + 13 MB | 2.5 ms | 0.999 at 0.55 ms | 1.000 at 0.57 ms | 1.000 at 0.92 ms | 1.000 at 1.52 ms |
| 100k × 768 | 35 s | 296 + 13 MB | 4.4 ms | 0.996 at 0.69 ms | 0.999 at 0.93 ms | 0.999 at 1.49 ms | 0.999 at 2.91 ms |
| 1M × 384 | 175 s | 1,492 + 134 MB | 54.0 ms | 0.828 at 0.96 ms | 0.939 at 1.02 ms | 0.975 at 1.20 ms | 0.993 at 1.45 ms |
| 1M × 768 | 332 s | 2,956 + 134 MB | 90.8 ms | 0.774 at 2.01 ms | 0.895 at 2.23 ms | 0.963 at 2.87 ms | 0.993 at 3.98 ms |

Each cell after the exact search gives recall@10 and the median latency. The exact search
runs in parallel on every core. At 1M vectors an `efSearch` of 64 gives a recall of 0.89
to 0.94 on this data, so the default was raised to 128 after this measurement. That gives
0.96 to 0.975 at 1.2 to 2.9 ms, and `ef:256` gives 0.993 at 1.5 to 4 ms, still 25 to 40
times faster than the exact search. USearch, measured on the same kind of data at 1M ×
384 outside Sparkles, reached a recall of 0.908 at ef = 64 and 0.992 at ef = 256, so the
lower recall at 1M comes from the data, not the graph. USearch built its index 2.5 times
faster (70 s against 176 s) and held 1.3 GB for it, against 130 MB for the Sparkles
graph. The peak memory of a build is 0.7 GB at 100k × 384 and 4.7 GB at 1M × 384. At
1M × 768 it reaches 9.3 GB, most of which is mapped vocabulary pages holding the literals'
text. The server maps the index file and used 3.2 GB at 1M × 768 after the measurements.

**UI.** The §2.7 pieces landed on 2026-10-02, after the server work.
* The dataset page has a "Vector indexes" panel with a card for each index. A card shows
  the predicate, dimension, metric and model, the state with a progress bar during a
  build, rows, memory, the exact threshold, the HNSW settings with node and layer
  counts, the segment, graph and file sizes, the overlay, the skipped counts and the last
  build. An overlay over 10 % of the rows gets a hint that compacting folds it in. The
  panel also shows the budget in use and the predicates packed without an index, each
  with a shortcut to index it.
* Create, Edit, Rebuild and Drop are shown only to callers with `admin` on the dataset,
  and are disabled on a read-only server. The create dialog reads the dimension from one
  of the predicate's vectors, and the edit dialog says whether the change keeps the build.
* "Measure recall" calls the recall endpoint with a chosen k and `ef`. The server keeps
  no measurement, so the card shows the last one taken in this browser.
* `/similar` sits between Explore and Datasets in the navigation. It picks an index, a
  packed predicate or any other predicate, and searches from an entity or from a pasted
  vector. The vector is checked against the §4.1 grammar as it is typed, with the first
  error's offset and a warning when its dimension differs from the index's. An entity
  with several vectors under the predicate is searched by the one picked. Results show
  the rank, the label, a score bar, the matched vector and actions to open the entity in
  Explore or to search from it. The footer gives the time and the plan's `method` and
  `exactBecause`, and the generated SPARQL opens in the query editor. The URL keeps the
  dataset, index, entity and controls, and the explorer's Similar section links there.
* The page has no graph picker and no graph column, and labels come from a second query
  instead of an `OPTIONAL` in the search. k goes up to 100 through a fixed list.
* The mock (`ui/mock/vector.mjs`) serves every `/$/vector` endpoint with `vector-index`
  tasks, and its `spk:vectorSearch` follows the index's metric, dimension, `ef:` and
  `exact:true` and reports the plan counters. Vitest covers the query builder, the
  vector grammar, the plan counters and the index form, and `ui/tests/mock/vector.spec.ts`
  and the phone overflow tests drive both pages against the mock.

**Hybrid ranking (2026-10-02).** Phase 3's hybrid ranking with
[F03](F03-full-text-search.md) is a property function, `spk:hybridSearch`
(`urn:x-sparkles:hybridSearch`). It runs a `text:query` search and a `spk:vectorSearch`
search and fuses their rankings by reciprocal rank fusion (Cormack, Clarke and Büttcher,
SIGIR 2009).

```sparql
PREFIX spk: <urn:x-sparkles:>  PREFIX ex: <http://example.org/>
SELECT ?s ?score ?textRank ?vectorRank WHERE {
  (?s ?score ?textRank ?vectorRank) spk:hybridSearch (
      (rdfs:label "brown fox" 100 "lang:en")
      (ex:embMiniLM "[0.01, -0.2, 0.09]"^^spk:vector 100)
      10 "rrf:60" "weights:1,0.5") .
} ORDER BY DESC(?score)
```

```
subject := term | ( term [?score [?textRank [?vectorRank]]] )
object  := ( text vector [limit] ["rrf:k"] ["weights:wt,wv"] )
text    := "query" | "query"@lang | ( iri* "query" [depth] ["lang:xx"] )
vector  := ( predicate query [depth] ["option:value" …] )
```

| Slot | Meaning |
|---|---|
| `?s` / constant | The subject. A constant keeps that subject's row of the fused ranking, if either list holds it. |
| `?score` | The fused score as an `xsd:double`. Higher is better. |
| `?textRank` | The subject's rank in the text ranking. It is unbound when the text list does not hold the subject. |
| `?vectorRank` | The subject's rank in the vector ranking. It is unbound when the vector list does not hold the subject. |
| `text` | The object list of `text:query`, or a bare query string. Its limit is the depth of the text ranking. |
| `vector` | The object list of `spk:vectorSearch`. Its `k` is the depth of the vector ranking, and its options apply. |
| `limit` | The number of subjects returned, 1 to 10,000. The default is 10. |
| `"rrf:k"` | The constant k of the fusion, a number of at least 0. The default is 60, the paper's value. |
| `"weights:wt,wv"` | The weights of the text and vector rankings, numbers of at least 0 that are not both 0. The default is `1,1`. |

Semantics:

* **The two searches.** Each list runs as its own property function would, once and
  within the active graph, with the same scope rules, budgets and errors. A list without
  a limit or `k` has a depth of 100, where `text:query` would return every hit and
  `spk:vectorSearch` would return 10.
* **Ranks.** Each ranking is reduced to one entry per subject, its best hit. Under
  `GRAPH ?g` the entries are per subject and graph instead, and `?g` is bound. A subject's
  rank is one more than the number of subjects in that list with a better score, so tied
  subjects share a rank. For the euclidean metric a lower distance is better.
* **Fusion.** A subject's fused score is the sum of `w / (k + rank)` over the lists that
  hold it. A subject that only one list holds gets only that list's term, and a list with
  weight 0 adds nothing but still contributes its subjects. The result is the `limit`
  subjects with the highest fused scores. Ties break by term id, as in `spk:vectorSearch`.
* **Joins.** The call is a leaf like the two searches, so `limit` is the top n before any
  join. Rows are emitted best first, but only `ORDER BY DESC(?score)` orders a result.
* **Restrictions.** The text list takes no `highlight:` option. The vector query must be
  a vector literal or an entity, and `candidates:join` is refused, because the call does
  not read the rest of its group. Arguments must be constants.

| Condition | Status |
|---|---|
| A malformed call, a slot after `?s` that is not a variable, a limit outside 1 to 10,000, a negative or non-numeric `rrf:`, or weights that are not two numbers of at least 0 with a positive sum. | 400 |
| An error of either search, such as a predicate that is not text-indexed, a dataset without a full-text index, or a `k` above 10,000. | As for that search |
| A text depth above `maxHits` with more hits than `maxHits`. | 507 |
| A build without the `text` feature. | 501 |

The implementation is `sparql/hybrid.rs`. It plans both searches with the planner code of
their own property functions, under hidden output variables, so the scope, dedup and
option handling are shared. It then reads both result tables, keeps the best score per
subject, ranks, and fuses. `text:query` gained a rank output for the same purpose, the
sixth slot of its subject list (F03). The plan shows one `HybridSearch` node whose
counters are `textHits`, `vectorHits`, `vectorMethod` and `fused`. Results are not
cached, as for vector searches. Tests in `crates/sparkles-core/tests/hybrid.rs` cover the
fusion against hand-computed scores, subjects missing from one list, ties in each
ranking and in the fused score, depths, limits and options, euclidean ranking, constant
subjects, `GRAPH ?g`, subjects with several hits, the `maxHits` budget and every error.

**Embeddings.** Computing embeddings, a non-goal of this spec, became
[F08](F08-embeddings-on-write.md): an index can name an OpenAI-compatible endpoint that
computes its vectors after each commit, and `spk:vectorSearch` and `spk:hybridSearch`
then accept a text as their query.

**Not built.** Quantization and the rest of Phase 3 were not built. That rest is
background catch-up of the graph with overlay inserts, keeping the graph across
compactions, rewriting `ORDER BY spk:cosine(…) LIMIT k`, and a compact datatype. A
hybrid call with a variable vector query or with `candidates:join` is not built either.
