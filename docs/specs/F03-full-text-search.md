# F03: Full-text search (Tantivy, `text:query`)

> **Status:** implemented in part
>
> **Phases:** Phase 1 shipped. It covers the `text` cargo feature, `text:query`, per-quad
> documents kept current in the commit path, catch-up or rebuild at open,
> `sparkles text-index` and `/$/text/{ds}`. Two Phase 2 items shipped with it:
> `PUT`/`DELETE /$/text/{ds}` and the UI (an index admin panel and ranked search in
> Explore). Snippets, highlighting, stemming, the Sparkles query grammar and the Phase 3
> items are not built.
>
> **User docs:** [API: Full-text search](../API.md#full-text-search) · [Features](../FEATURES.md#sparql-arq-equivalent) · [Benchmarks: Full-text index and observability](../BENCHMARKS.md#full-text-index-and-observability-105m-triples)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This spec depends on the commit-identity work ([CI](CI-commit-identity.md)). That work gives
the store and every `Snapshot` a durable commit sequence number, `seq`, that only increases.

## 1. Summary

Sparkles gets an embedded full-text index over string literals, ranked by BM25. SPARQL
queries it through Jena's `text:query` property function. A search returns subjects, the
matching literals, graphs, predicates and scores, and these join with ordinary graph
patterns. The index is derived data that lives next to the store. The commit path keeps it
current, so a query always sees text results that match its snapshot. It can be rebuilt from
the RDF data at any time.

The library is **Tantivy 0.26.2** (crates.io, released 2026-09-08), licensed **MIT**.
Sparkles uses it with `default-features = false, features = ["mmap", "stemmer", "lz4-compression"]`,
which keeps out the C dependency on zstd. Record this in [PROVENANCE.md](PROVENANCE.md)
when implementing.

### Goals

* Support `?s text:query "…"` and the list forms
  `(?s ?score ?literal ?g ?prop) text:query (prop* "q" limit "lang:xx")`, a Jena-compatible
  subset. The pattern works anywhere a triple pattern does: in a BGP, `GRAPH`, `OPTIONAL`,
  `EXISTS`, subqueries and the `WHERE` clause of an update.
* Read-after-write. Once an update returns 200, the next text query sees it.
* Stable document keys, derived from RDF term identity. Compaction never re-indexes anything.
* Crash safety. The RDF store stays authoritative. After a crash, a config change or a
  corrupt index, the text index is caught up or rebuilt without touching the RDF data.
* An opt-in cargo feature, `text`, on `sparkles`. `sparkles-server` enables it by default.
  Each dataset opts in separately, and datasets without text search pay nothing.

### Non-goals (all phases unless noted)

* No new literal datatype. Literals stay `xsd:string` or `rdf:langString`.
* No compatibility with Lucene or Jena indexes, and no Jena assembler configuration
  (`text:TextDataset`).
* Phase 1 has no highlighting, per-language stemming, facets or hybrid vector retrieval. It
  has no text search at historical snapshots either (see §6).
* No distributed or remote index.

## 2. User-visible behavior

### 2.1 SPARQL

The namespace is Jena's, `PREFIX text: <http://jena.apache.org/text#>`, so existing queries
run unchanged. A triple pattern with the predicate `text:query` is a property function. It
does not match data.

```
subject := term                       # ?s  or a constant IRI / blank-node label
         | ( term [?score [?literal [?g [?prop]]]] )
object  := "query"  |  "query"@lang
         | ( iri* "query" [limit] ["lang:xx"] )
```

| Slot | Meaning |
|---|---|
| `?s` / constant | The subject of the matching quad. A constant restricts retrieval to that subject. A literal subject gives no results, as in Jena. |
| `?score` | The BM25 score, as an `xsd:float` (Jena's type). A higher score is a better match. |
| `?literal` | The matching literal, as the exact stored RDF term (lexical form and language tag). |
| `?g` | The graph of the matching quad. Named graphs bind their IRI; the default graph binds `<urn:x-arq:DefaultGraph>`. |
| `?prop` | The predicate of the matching quad. |
| `iri*` | Restricts the search to these predicates. With none, every indexed predicate is searched. |
| `"query"` | The query string (§4.3). A language tag on it acts as `"lang:tag"`. |
| `limit` | An integer. With `n > 0` the search keeps the top `n` hits. With `≤ 0` or no limit it returns all hits, up to `maxHits`. |
| `"lang:xx"` | Keeps only literals whose language tag matches `xx`, by RFC 4647 basic filtering. `en` matches `en` and `en-GB`. |

Semantics:

* **Result multiplicity.** Each matching indexed quad `(s, p, o, g)` gives one solution. A
  subject with two matching literals appears twice. Jena behaves the same way, since each
  matching triple is a hit there. To collapse the rows, use `DISTINCT`, or `GROUP BY ?s` with
  `MAX(?score)`. The active graph can be a merge of several graphs (union default graph,
  `FROM a FROM b`). In that case, if `?g` is not requested, hits with the same `(s, p, o)`
  merge into one row with the highest score. Triple-pattern scans follow the same RDF merge
  semantics.
* **Evaluation order.** `text:query` is a table function. It is evaluated once, independently
  of the rest of the group, and then joined. So `limit` means the top `n` hits of the text
  search within its graph scope and its predicate, language and subject restrictions,
  *before* any join. The answer does not depend on where the pattern sits in the query. In
  Jena it does. Jena's documentation recommends putting `text:query` first, and in that
  position the two engines agree.
* **Graph scope inside retrieval.** The active graph becomes a filter inside the Tantivy
  query. This covers the default graph, `GRAPH <g>`, `GRAPH ?g`, `FROM`/`FROM NAMED`, the
  union default graph and `reasoning=false`. Top-`k` is taken within that scope and never
  filtered afterwards. `GRAPH ?g { … }` binds `?g` from the hit. It covers named graphs only
  and excludes default-graph documents, as scans do.
* **Ordering.** The operator emits rows by descending score and breaks ties by document key.
  SPARQL guarantees no order after joins, so use `ORDER BY DESC(?score)`. Scores use
  index-wide statistics and change with updates and merges. Compare rankings, not values.
* **OPTIONAL, MINUS, EXISTS and FILTER.** These need no special handling. The planner treats
  the text leaf like any other leaf and places `FILTER`s over `?score` or `?literal` after
  it. `OPTIONAL { (?s ?sc) text:query "x" }` left-joins the independent hit table.
* **Inside updates.** In `DELETE/INSERT … WHERE`, `text:query` sees the text index as it was
  when the transaction started. It does not see earlier operations of the same request. This
  is a documented limitation.
* **Arguments must be constants** in Phase 1: the query string, predicates, limit and
  language. A variable in one of these positions is a 400 error. Phase 3 adds bound-variable
  arguments.
* **No function form.** Sparkles adds no `FILTER` function. `CONTAINS` and `REGEX` already
  have key-level fast paths, and only the property function can produce scores and top-`k`.

### 2.2 HTTP (`/$/` extensions, documented in `docs/API.md`)

| Method | Path | Phase | Description |
|---|---|---|---|
| GET | `/$/text/{ds}` | 1 | Returns `TextStatus` (below), or `{ "enabled": false }` when text search is off. An unknown dataset gives `404`. |
| POST | `/$/text/{ds}/rebuild` | 1 | Rebuilds the index in full from the current snapshot and returns a `Task` (`kind: "text-rebuild"`). Gives `409` if a rebuild is running, `403` on `--read-only` and `400` if text search is not enabled. |
| PUT | `/$/text/{ds}` | 2 | Takes a `TextConfig` body, enables or reconfigures text search, then starts a rebuild task. An invalid config gives `400`. |
| DELETE | `/$/text/{ds}` | 2 | Disables text search and removes `text.json` and the index directory. |
| POST | `/$/datasets` | 2 | Accepts an optional `text: true \| TextConfig` in the JSON body. |
| GET/POST | `/{ds}/text?q=&predicate=&lang=&graph=&limit=&highlight=` | 2 | A convenience search for the UI. It returns JSON hits with snippets (§2.5). |

```ts
type TextConfig = {
  predicates: "all" | string[];          // default "all": every predicate
  graphs?: { include?: "all" | string[]; exclude?: string[] };  // default all graphs
  analyzer?: "standard";                 // Phase 2: languages?: { [tag]: "english" | … }
  maxTextBytes?: number;                 // default 262144; longer literals are indexed truncated
  maxHits?: number;                      // default 1_000_000 hits per text:query evaluation
};
type TextStatus = {
  enabled: boolean;
  state: "ready" | "rebuilding" | "stale" | "failed";
  docs: number; seq: number; storeSeq: number;   // ready ⇔ seq == storeSeq
  epoch: number;                                  // +1 per rebuild / reconfiguration
  diskBytes: number; segments: number;
  config: TextConfig; formatVersion: 1;
  lastRebuild?: { at: string; ms: number; docs: number };
  message?: string;                               // reason for stale / failed
};
```

`DatasetInfo` gets `text: null | { state, docs }`, and `DatasetStats` gets `text: TextStatus | null`.

### 2.3 CLI

```sh
sparkles text-index --loc db                      # enable with defaults (if needed), catch up or rebuild, print status
sparkles text-index --loc db --predicate http://www.w3.org/2000/01/rdf-schema#label \
                              --predicate http://www.w3.org/2004/02/skos/core#prefLabel
sparkles text-index --loc db --exclude-graph urn:x-sparkles:inferred
sparkles text-index --loc db --rebuild            # force a full rebuild
sparkles text-index --loc db --status             # print TextStatus as JSON; changes nothing
sparkles text-index --loc db --disable            # remove text.json and the index
sparkles serve --mem ds --text ds                 # enable (default config) for a --mem / --loc dataset
sparkles serve --loc ds=/db --text ds=text.json   # … with a config file
```

On a database that has a `text.json`, `load`, `update`, `query` and `infer` maintain and use
the index automatically. In a build without the feature, `text-index` exits with status 2
and prints `built without full-text search (cargo feature "text")`.

### 2.4 Rust API (`sparkles`, feature `text`)

```rust
pub struct TextConfig { pub predicates: PredicateSet, pub graphs: GraphScope,
                        pub max_text_bytes: usize, pub max_hits: usize }   // serde = text.json
impl Store {
    pub fn enable_text(&self, cfg: TextConfig) -> Result<()>;   // writes text.json, rebuilds
    pub fn disable_text(&self) -> Result<()>;
    pub fn rebuild_text(&self) -> Result<TextStatus>;
    pub fn text_status(&self) -> Option<TextStatus>;
}
impl Dataset { /* same four, delegating */ }
```

The configuration is stored in the database directory, so `Dataset::open` picks it up.
SPARQL is the query interface. Phase 2 adds
`Dataset::text_search(&TextRequest) -> Result<Vec<TextHit>>`, which serves `/{ds}/text`.

### 2.5 UI (SvelteKit, `ui/`, Phase 2)

* **Search page** at `/search`. It gets a new nav item, "Search", and uses the header's
  dataset switcher.
  * Controls. A query box comes with a syntax cheat sheet covering terms, `"phrase"`,
    `AND`/`OR`, `-term` and `prefix*`. Filters select predicates (from the configured or
    top predicates), a language and a graph. A field sets the limit.
  * Results. A ranked list in which each row shows a score bar, the subject (a `TermView`
    that links to the explorer), the predicate, the literal snippet with matches
    highlighted, the language tag and the graph.
  * Footer: "N hits · docs · index seq · ms".
  * "Open in query editor" writes the equivalent `text:query` SELECT into `/query`.
  * A dataset without an index shows an empty state with an "Enable full-text search"
    button. The button opens a config dialog that calls `PUT /$/text/{ds}`.
* **Dataset page.** A new "Full-text search" panel shows:
  * a state badge: ready, rebuilding with progress, stale, failed with a message, or off;
  * the indexed docs, the index size, the index `seq` against the store `seq`, the
    configured predicates and graphs, and the last rebuild;
  * the buttons Rebuild (a task in `TaskList`), Configure and Disable.
* **Create-dataset dialog.** An "Enable full-text search" checkbox and an optional predicate
  list.
* **Query editor.** `text:` joins the prefix completions, and `examples.ts` gets an example
  query.
* **`api.ts`** gets `textStatus`, `textRebuild`, `textConfigure`, `textDisable` and
  `textSearch`.

## 3. Standards basis

* SPARQL 1.1 Query §4.2.3 (RDF collections). In a pattern, `( … )` is syntax for an
  `rdf:first`/`rdf:rest` chain of blank nodes. `spargebra` expands it that way (verified in
  `spargebra-0.4.7/src/parser.rs`), and that is how the list arguments reach the planner.
  Property functions are an ARQ extension. A query that uses one is still a valid SPARQL 1.1
  query, evaluated with extension semantics.
* RDF 1.1 Concepts: simple literals (`xsd:string`) and `rdf:langString`. RDF 1.2 directional
  language strings are indexed like language strings.
* BCP 47 language tags, and RFC 4647 §3.3.1 basic filtering for `lang:`.
* BM25 (Robertson & Zaragoza), Tantivy's default scorer.
* Jena's text query documentation and the `jena-text` sources, for syntax compatibility.

## 4. Semantics

### 4.1 What is indexed

A quad `(s, p, o, g)` is a **document** when all of these hold:

* `o` is a literal with the datatype `xsd:string`, `rdf:langString` or `rdf:dirLangString`.
  In the vocabulary key, that means the key starts with `"` and its suffix after `0xFF` is
  empty or starts with `@`. Inline ids are never strings.
* `p` is in `predicates`. The value `"all"` accepts every predicate.
* `g` is in the graph scope. By default every graph is indexed, including
  `urn:x-sparkles:inferred`. Queries with `reasoning=true` include that graph, and the graph
  filter excludes it for `reasoning=false`.

The **default configuration** is `predicates: "all"`, so search works on any data without
setup. Narrow the predicates to control the index size (open question 1).

The "standard" analyzer works as follows:

* Tantivy's `SimpleTokenizer` splits on non-alphanumeric characters. `RemoveLongFilter(40)`,
  `LowerCaser` and `AsciiFoldingFilter` follow.
* It is registered as `sparkles_standard` and used both at index time and for query parsing.
* Text longer than `maxTextBytes` is truncated at a char boundary before tokenizing. The
  document and `?literal` keep the full term.
* CJK text is not segmented, so a run of CJK characters is one token. An n-gram analyzer
  could come later.

### 4.2 Consistency

* Every `Snapshot` carries `text: Option<Arc<TextView>>`. A `TextView` holds a Tantivy
  `Searcher`, the `seq` it corresponds to, and the epoch.
* **Invariant:** when `text.seq == snapshot.seq`, the snapshot searches exactly the documents
  that §4.1 derives from it. The commit path (§5.3) keeps this true for every published
  snapshot unless text maintenance fails.
* If `text` is missing because text search is not enabled, the query fails with
  `400 "dataset has no full-text index; enable it with sparkles text-index or PUT /$/text/{ds}"`.
* If `text.seq != snapshot.seq`, or the state is `rebuilding` or `failed`, the query fails
  with `503 "full-text index of 'ds' is <state> (index seq A, data seq B); retry later or POST
  /$/text/ds/rebuild"`. Stale reads never happen silently. An opt-in stale mode could come
  later.
* Text failures never fail RDF writes. If Tantivy errors after the WAL commit is durable, the
  update still returns 200. The dataset's text state becomes `stale`, the error is logged,
  and the state stays `stale` until a rebuild.

### 4.3 Query string (Phase 1 subset)

In Phase 1, Tantivy's `QueryParser` parses the query string over the `text` field. It runs
in strict mode (`parse_query`), and OR is the default conjunction, as in Lucene and Jena.

This syntax is supported and tested:

* terms: `fox`;
* phrases: `"brown bears"`, and with slop, `"brown bears"~2`;
* `AND` and `OR`, with `AND` binding tighter;
* `+term` and `-term`;
* parentheses;
* phrase prefixes: `"quick bro"*`.

Everything else is unsupported. That includes field-qualified clauses (`field:x`), ranges,
`IN`, boosts and a lone `*`. A parse error gives `400 "text:query: <parser message>"`. A
literal `:` must be escaped as `\:`.

Phase 2 replaces this with a small Sparkles grammar, a subset of classic Lucene syntax that
compiles directly to Tantivy queries. It adds bare `prefix*` (`RegexQuery`), fuzzy `term~`
and `NOT`. It treats `:` as plain text and puts the internal fields out of reach.

### 4.4 Errors (all `{ "error": … }` bodies per `docs/API.md`)

| Condition | Status | Message (prefix) |
|---|---|---|
| The query string is not a string literal (it is typed, an IRI or a number). | 400 | `text:query: query string must be a string literal` |
| The subject list is empty or longer than 5, or the object list has no query string or more than 3 trailing arguments. | 400 | `text:query: malformed argument list` |
| The score, literal, graph or prop slot is not a variable. | 400 | `text:query: <slot> must be a variable` |
| The object list contains a variable (Phase 1). | 400 | `text:query: arguments must be constants` |
| The predicate is not indexed. | 400 | `text:query: <p> is not text-indexed (indexed: …)` |
| A `highlight:` argument is given (Phase 1). | 400 | `text:query: highlight is not supported yet` |
| The query does not parse. | 400 | `text:query: …` |
| Hits exceed `maxHits` and no limit is given. | 507 | `text:query matched more than N documents; add a limit` (`Error::MemoryLimit`) |
| The index is not ready (§4.2). | 503 | See §4.2. A new `Error::TextUnavailable` maps to 503 with `Retry-After: 5`. |
| Sparkles was built without the `text` feature. | 501 | `built without full-text search (cargo feature "text")` (`Error::Unsupported`) |
| A rebuild is already running. | 409 | `text index rebuild already running` |

A malformed `rdf:first`/`rdf:rest` chain, such as a blank node with extra or missing links,
gives 400.

### 4.5 Limits and budgets

* `maxHits` (default 1,000,000) caps an evaluation without a limit. The row count also goes
  through `ctx.check_rows`.
* Tantivy search cannot be interrupted. So `ctx.check()` runs before and after the search,
  and every 4096 rows while hits are resolved.
* The incremental writer uses 1 thread and a 32 MiB heap. A rebuild uses `min(8, cores)`
  threads and 64 MiB per thread.

## 5. Design

### 5.1 Modules

| Where | Change |
|---|---|
| `crates/sparkles/Cargo.toml` | Add `tantivy = { version = "0.26", default-features = false, features = ["mmap","stemmer","lz4-compression"], optional = true }` and `[features] text = ["dep:tantivy"]`. |
| `crates/sparkles-server/Cargo.toml` | Add `text = ["sparkles/text"]` to `default`. |
| `sparkles/src/text/mod.rs` (new, `cfg(feature="text")`) | Holds `TextConfig`, the schema, `TextIndex` (writer, reader, state, paths), `TextView`, `apply_commit`, `rebuild`, `recover` and `search`. |
| `sparkles/src/sparql/textpf.rs` (new, always compiled) | Recognizes `text:query` in a BGP and decodes the collections into a `TextCall`. It is pure algebra, with no Tantivy. |
| `sparql/plan.rs` | In `collect()` for `GP::Bgp`, before `self.triple(tp, g)` is pushed, `textpf::extract(patterns)` removes each `text:query` triple and its `rdf:first`/`rdf:rest` chain triples. It then pushes `Item::Text(TextItem { call, graph: g.clone() })`. `plan_group()` turns each `Item::Text` into a leaf `Node` with `Kind::TextSearch(Box<TextSpec>)`, a graph filter from `self.graph_filter(&item.graph)` and `est = min(limit, Σ doc_freq(query terms), docs)`. It adds the leaf to `nodes` **before** the filter-equality pass, so its variables are never substituted. The DP then joins it like any leaf. `Node::operator()` returns `"TextSearch"`. |
| `sparql/exec.rs` | Dispatches `Kind::TextSearch` to `crate::text::search(ctx, spec, &n.vars)`. Without the feature it returns `Error::Unsupported`. |
| `sparql/cache.rs` | In `write_node`, `TextSearch` is cacheable. The key adds the query, predicates, lang, limit, subject, graph filter, output slots and `text.epoch`. It already has `snap.version` and the generation. |
| `store.rs` | `Store` gets `text: Option<text::TextIndex>`. `Snapshot` gets `text` and `wal_commits: u64`, the number of commits in the current generation's WAL that the snapshot sees. Hooks go into `open`, `publish_log` and `rebuild_locked` (§5.3–5.5). |
| `error.rs` | Adds `Error::TextUnavailable(String)`. |
| server `http.rs`, `main.rs`, `state.rs` | Add the routes and handlers of §2.2, the `text-index` command, `serve --text` and `DatasetInfo.text`. |

The spec type is `TextSpec { query, predicates: Vec<String>, lang: Option<String>, limit: Option<usize>,
subject: PathEnd, score/literal/graph_out/prop_out: Option<VarId>, graph: GraphFilter,
graph_var: Option<VarId>, dedup: bool }`. `dedup` is true when the active graph is a merge
(`GraphFilter::Named` or `Set` without a graph variable) and `graph_out` is `None`.

### 5.2 Persistence (format version 1)

```
<root>/text.json        TextConfig + "formatVersion": 1 — authoritative, written with write_atomic
<root>/text/            Tantivy index (derived; deleting it only forces a rebuild)
<root>/text.new/        rebuild in progress (deleted on open)
<root>/text.old/        previous index during the swap (deleted on open)
```

The index sits at the root, not in `gen-NNNN/`, because its keys do not depend on the
generation. In-memory stores use `Index::create_in_ram`.

Tantivy schema v1:

| Field | Type | Purpose |
|---|---|---|
| `key` | bytes, indexed | The document key (below), used for `delete_term`. |
| `s` | bytes, indexed + stored | The subject's vocabulary key: `<iri`, or `_` + u64-be for a blank node. Blank-node ids survive rebuilds (see `Slot::Id` in `rebuild_locked`). |
| `p` | string (raw), indexed + stored | The predicate IRI. |
| `o` | bytes, stored | The literal's vocabulary key (`"lex 0xFF @lang`). It supplies `?literal` and snippets. |
| `g` | string (raw), indexed + stored | The graph IRI, `_:b<hex>`, or `urn:x-arq:DefaultGraph`. |
| `lang` | string (raw), indexed, multi-valued | The full tag in lowercase (without `--dir`) and the primary subtag. Absent for `xsd:string`. |
| `text` | text, positions, tokenizer `sparkles_standard` | The lexical form, up to `maxTextBytes`. |

**Document key:** `SHA-256(varint(len) ‖ key(s) ‖ varint(len) ‖ key(p) ‖ varint(len) ‖ key(o) ‖ varint(len) ‖ key(g))[..16]`.
Here `key()` is `id::write_term_key` or the vocabulary key, and the default graph's key is
empty. The key depends only on RDF term identity, so it survives compaction, bulk rebuilds
and vocabulary renumbering. `sha2` is already a dependency.

**Commit payload.** It is set with `PreparedCommit::set_payload` and read back from
`IndexMeta.payload`:
`{"format":1,"seq":S,"generation":"gen-0007","walCommits":N,"epoch":E,"config":"<sha256 of canonical text.json>"}`.

* `seq` is the consistency watermark.
* `(generation, walCommits)` is the position for catching up after a crash. It does not
  depend on how the commit-identity spec numbers compactions.

### 5.3 Commit path (`WriteTxn::publish_log`)

A commit runs these steps in order:

1. `dvocab.sync()`, then the WAL write and fsync, as today.
2. **Text maintenance**, only when `store.text` is enabled and healthy. `touched` is the set
   of distinct quads in `self.log` that §4.1 selects. A per-generation
   `FxHashMap<Id, bool>` caches the predicate check, and the keys come from the transaction
   view.
   * If `touched` is empty, the new view is
     `TextView { searcher: prev.searcher.clone(), seq: new_seq, .. }`. There is no I/O, so an
     update that touches no text costs one hash lookup per logged quad.
   * Otherwise each quad gets `writer.delete_term(key)`, then `add_document` if the new
     delta view `contains` the quad. This upserts by final presence, so it is idempotent and
     independent of order. Then come `prepare_commit`, `set_payload`, `commit`,
     `reader.reload()` and a fresh `Searcher`.
   * On error, the writer calls `writer.rollback()`, the state becomes `stale`, and the
     previous view stays. Its `seq` is now behind, so text queries return 503.
3. Publish the `Snapshot` with `text` and `wal_commits + 1`, as today.

The writer mutex already serializes commits. The Tantivy `IndexWriter`, one per dataset and
kept open, needs no lock of its own.

**Latency trade-off.** Each update that touches indexed literals pays for a Tantivy commit:
a small segment, `meta.json` and fsyncs. Tantivy merges segments in the background. Measure
this with the update latency in `scripts/bench.sh` on a dataset with text search on. Updates
that touch no indexed literals, and datasets without text search, must show no regression.

### 5.4 Generation switches (`rebuild_locked`)

* **Compaction.** Here `extra` and `extra_quads` are both empty and the content is
  unchanged. Commit an empty Tantivy transaction with the payload
  `{seq, generation: new, walCommits: 0}` and publish with the same searcher. No documents
  are touched.
* **Bulk rebuilds that change content.** These are the bulk path of `Store::load` and
  `insert_bulk` at or above `bulk_threshold`. Once the new generation is built and `CURRENT`
  has switched, and while the writer lock is still held, run a full text rebuild (§5.5) from
  the new, unpublished snapshot. Then publish the snapshot with the new view.
  * Phase 3 makes this incremental, from `extra_quads` and the transaction log.
  * A crash between the `CURRENT` switch and the text commit is caught at open. The
    payload's generation does not match, so the index is rebuilt.

### 5.5 Full rebuild

1. Create `text.new/` and use a multi-threaded writer.
2. Enumerate the candidates.
   * With a predicate list, scan `PSO` once per predicate id (`lookup_iri`).
   * With `"all"`, run `for_each_quad` and keep the literal objects. Phase 3 narrows this to
     an `OPS` range scan over the literal id range from `Vocab::prefix_range(b"\"")`, plus
     the delta literals.
   * Resolve keys in batches (`Vocab::get_sorted`).
3. Commit with the payload, then drop the writer and the index.
4. Swap. Fsync `text.new`, rename `text` to `text.old`, rename `text.new` to `text`, run
   `sync_dir(root)` and remove `text.old`.
5. Reopen at `text/`, bump `epoch`, and republish the current snapshot with the new view.
   `Snapshot` is `Clone`, the `version` stays the same, and the cache key includes the
   epoch.

In Phase 1, an online rebuild (`POST …/rebuild`) holds the writer lock until it finishes.
Updates wait. Reads continue on the old view, or get 503 if that view is not ready. Phase 2
releases the lock during the build. The commit path appends the keys of touched quads to a
journal, and a short catch-up under the lock replays the journal before the swap.

### 5.6 Open and recovery (`Store::open`, before WAL replay)

1. Without `text.json`, text search is off. If `text.json` exists but Sparkles was built
   without the feature, it warns once and ignores the file. The text index then falls
   behind the WAL and generation, and a later open with the feature repairs it.
2. Delete `text.new/` and `text.old/`, then open `text/`. If it is missing or unreadable,
   has `format != 1`, or has a different config hash, run a **full rebuild** after the WAL
   replay.
3. Otherwise read the payload `P`. If `P.generation == CURRENT`, the WAL replay collects the
   quads of the commits numbered above `P.walCommits`. These are applied in one Tantivy
   commit with the §5.3 rule: upsert or delete by presence in the recovered snapshot. If
   the generations differ, a generation switch happened without a text commit, and the
   index gets a full rebuild.
4. If `P.walCommits` is greater than the WAL's commit count, run a full rebuild. A torn WAL
   tail cannot cause this, because the text commit comes after the WAL fsync. It means the
   files are foreign or restored.

In Phase 1 a full rebuild at open is synchronous and logs its progress. Phase 2 moves it to
the background, with the state `rebuilding` and 503s in the meantime. Text code never writes
the authoritative files: `CURRENT`, the generations, the WAL and `delta.vocab`. Backups stay
N-Quads only, and a restore rebuilds the index.

### 5.7 Search (`text::search`)

1. Check `snap.text` (§4.2).
2. Build `BooleanQuery[Must(parsed text query), Must(ConstScore(filters, 0.0))]`. The filters
   are `TermSetQuery(p ∈ predicates)`, `TermSetQuery(lang)`, `TermQuery(s = subject key)` and
   the graph filter:
   * `Default` becomes `g = urn:x-arq:DefaultGraph`;
   * `Named` becomes `MustNot` on that term;
   * `One` and `Set` become a `TermSetQuery` of `snap.term(id)`, rendered as `g` strings.
3. Collect with `TopDocs::with_limit(limit or maxHits + 1)`. With `dedup`, fetch more than
   needed and double the count until there are `limit` distinct `(s, p, o)` or the hits run
   out.
4. Resolve the stored fields to ids with `snap.lookup_key`, or with `Id::bnode` for `_` keys.
   Local terms, such as the default-graph IRI and scores, go through `ctx`. A key that does
   not resolve is skipped and counted as `staleHits` in the plan description. Debug builds
   assert on it.
5. Emit the columns in `n.vars` order.

A plan description looks like
`TextSearch ?s ?score ← "brown" [rdfs:label] lang=en limit 10 graph=default`.

## 6. Phasing

### Phase 1 (MVP, about one day)

1. The `text` features (`sparkles`, and the server default), `text.json`, schema v1,
   `TextIndex`, and `Snapshot.text` and `wal_commits`.
2. Maintenance: the §5.3 commit hook, compaction and bulk handling (§5.4), the synchronous
   full rebuild (§5.5), and catch-up or rebuild at open (§5.6).
3. Recognizing `text:query`, the `TextSearch` operator, graph scope, dedup, limit, `lang:`,
   constant subjects, scores, the cache key and the errors of §4.4.
4. The CLI command `sparkles text-index` (enable, `--predicate`, `--exclude-graph`,
   `--rebuild`, `--status`, `--disable`), `serve --text`, `GET /$/text/{ds}` and
   `POST /$/text/{ds}/rebuild`. The rebuild runs synchronously inside the task.
5. Tests. §7 A1–A12 become `crates/sparkles` unit and integration tests, and A13–A14 become
   server tests. Update `docs/API.md` and the "Full-text search" gap row in the README
   status table.

### Phase 2

* The UI of §2.5.
* `/{ds}/text` with snippets, using `SnippetGenerator` and the `Snippet::highlighted()`
  ranges. Sparkles does the HTML escaping.
* Jena's `highlight:` options in `text:query`. As in Jena, the highlighted string replaces
  `?literal`.
* `PUT`/`DELETE /$/text/{ds}`, and `text` on dataset creation.
* The Sparkles query grammar (§4.3).
* Per-language stemmed fields `text_<lang>` (`languages: {"en": "english"}`), used when the
  query has `lang:`.
* Online rebuilds with a journal, and a background rebuild at open.

### Phase 3 and later

* Variable arguments and bound-subject pushdown. When the left side is small and no limit is
  given, pass a `TermSetQuery` of subject keys.
* Tantivy fast fields instead of the doc store for `s` and `g`.
* Incremental bulk maintenance, and a rebuild over the `OPS` literal range.
* Facets by predicate, graph or language.
* Hybrid retrieval with [F04](F04-vector-search.md) vector search through reciprocal rank
  fusion (`score = Σ 1/(60 + rank)`). It would be a Sparkles property function over both
  indexes.
* Text search at [F06](F06-snapshots-and-point-in-time.md) historical snapshots, with an
  index kept per named snapshot or built on demand. Until then a historical snapshot gets
  503, because its `seq` differs.
* An opt-in query mode that allows stale results.

## 7. Acceptance examples

The data is in `ds`, with text search enabled and the default configuration. The prefixes
are `ex: <http://example.org/>`, `rdfs:` and `text:`.

```trig
ex:b1 a ex:Book ; rdfs:label "The Quick Brown Fox"@en .
ex:b2 a ex:Book ; rdfs:label "Brown Bears" ; rdfs:comment "A field guide to brown bears" .
ex:b3 a ex:Film ; rdfs:label "Le renard brun"@fr .
ex:p1 rdfs:label "Foxglove" ; ex:code "fox"^^ex:Code .
ex:g1 { ex:b4 rdfs:label "Fox in Socks" }
```

| # | Query / action | Expected |
|---|---|---|
| A1 | `SELECT ?s { ?s text:query "fox" }` | `{ex:b1}`. `Foxglove` is a different token, typed literals are not indexed, and `ex:b4` is in a named graph. |
| A2 | `SELECT ?s ?lit ?g { GRAPH ?g { (?s ?sc ?lit) text:query "fox" } }` | One row: `ex:b4, "Fox in Socks", ex:g1`. |
| A3 | `SELECT ?s { (?s ?sc) text:query (rdfs:label "brown") } ORDER BY DESC(?sc)` | `ex:b2, ex:b1`. The shorter field scores higher. `ex:b3` does not match, because its word is `brun`. |
| A4 | `SELECT ?s ?lit { (?s ?sc ?lit) text:query "brown" }` | 3 rows: `(b1, label)`, `(b2, "Brown Bears")` and `(b2, comment)`. This is the multiplicity rule. |
| A5 | `SELECT ?s ?lit { (?s ?sc ?lit) text:query "\"brown bears\"" }` | b2 twice, for the label and the comment. With `(rdfs:label "\"brown bears\"")`, only the label. |
| A6 | `{ ?s text:query (rdfs:label "renard" "lang:fr") }` / `{ ?s text:query "renard"@en }` | `{ex:b3}` for the first, empty for the second. |
| A7 | `SELECT DISTINCT ?s { ?s text:query "brown" . ?s a ex:Book }` | `{ex:b1, ex:b2}` |
| A8 | `SELECT ?s { (?s ?sc) text:query ("brown" 1) }` | Exactly 1 row, `ex:b2`. |
| A9 | `SELECT ?s ?sc { ?s a ex:Book OPTIONAL { (?s ?sc) text:query "fox" } }` | `b1` with a score, and `b2` with `?sc` unbound. |
| A10 | `INSERT DATA { ex:b5 rdfs:label "Brown Owl" }`, then A3's query right away. Then `DELETE DATA { ex:b1 rdfs:label "The Quick Brown Fox"@en }` and the query again. | First `{b2, b5, b1}`, with b5 among the first two. Then b1 is gone. `GET /$/text/ds` shows `seq == storeSeq`, with `docs` at 7 and then 6. |
| A11 | `sparkles compact --loc db`, then A1. | The same answer. `epoch` is unchanged and no rebuild is logged. |
| A12 | A crash simulation with the test hook `TextIndex::fail_next_commit`. The insert of A10 returns 200 and the state becomes `stale`. A1 then gives 503. Closing and reopening the store catches up from the WAL, the state is `ready`, and A10's results are correct. Deleting `text/` and reopening runs a full rebuild with the same answers. | As described. |
| A13 | `SELECT (COUNT(*) AS ?n) { ex:b2 text:query "bears" }` | `2`: a constant subject with two documents. |
| A14 | With `--union-default-graph` and `ex:g2 { ex:b2 rdfs:label "Brown Bears" }` added: `SELECT ?s ?lit { (?s ?sc ?lit) text:query (rdfs:label "bears") }` | One merged row. `(?s ?sc ?lit ?g)` gives two rows. |
| A15 | Errors: `?s text:query 42`; `(?s 1) text:query "x"`; `?s text:query (ex:nope "x")`; `?s text:query "(fox"`; `text:query` on a dataset without an index. | 400, 400, 400, 400 and 400, with the §4.4 messages. |
| A16 | `GET /$/text/ds` after the load. | `200 {"enabled":true,"state":"ready","docs":6,"seq":S,"storeSeq":S,"epoch":1,…,"config":{"predicates":"all"},"formatVersion":1}` |
| A17 | `POST /$/text/ds/rebuild` twice at the same time. | The first gives `200 Task{kind:"text-rebuild"}` and the second `409`. On `--read-only` it gives `403`. |
| A18 | `sparkles text-index --loc db --predicate http://www.w3.org/2000/01/rdf-schema#label` | stderr shows `text index: 5 docs (1 predicate), seq S, rebuilt in … ms`. A4 now returns 2 rows, because the comment is no longer indexed, and `(rdfs:comment "x")` gives 400 (not indexed). |
| A19 | A load larger than `bulk_threshold` into a store with text search on. | The status is `ready` with `seq == storeSeq`, and documents from the new data are searchable. |
| A20 | `cargo build -p sparkles` (no feature) and a query with `text:query`. | 501 / `Error::Unsupported`. `cargo tree -p sparkles` shows no `tantivy`. |

## 8. Rejected alternatives

* **A home-grown inverted index on the permutations**, like QLever's word and entity index.
  BM25, phrases, prefixes, snippets and segment merging would all be new code. Tantivy is
  mature, pure Rust, MIT-licensed and embedded.
* **One document per subject**, as Jena's entity style does with one Lucene document per
  subject and a field per predicate. An update would have to re-read all of the subject's
  literals, and the matching literal and graph would be lost. One document per quad maps
  1:1 onto RDF changes and gives an exact `?literal` and `?g`.
* **Keys from vocabulary ids.** Ids change with every generation, so every compaction would
  force a full re-index.
* **An index inside `gen-NNNN/`, or one built by the bulk `Builder`.** This has the same
  problem: compaction would rebuild the text index for nothing.
* **Asynchronous (eventual) indexing.** It breaks read-after-write. It may return later as
  an opt-in stale mode.
* **An in-memory overlay for recent updates instead of a Tantivy commit per commit.** Update
  latency would be lower, but there would be two scoring domains with inconsistent BM25
  statistics, and much more code. Revisit this if the §5.3 latency measurements demand it.
* **Filtering graphs or predicates after a global top-`k`.** This gives wrong results with
  `limit`.
* **A dedicated "searchable text" datatype.** It changes RDF term identity and breaks
  existing data and queries.
* **A `FILTER` function as the main interface.** It gives no scores and no top-`k`.
  `CONTAINS` and `REGEX` already cover substring filtering.
* **An external engine (Elasticsearch, Solr) or SQLite FTS5.** Each adds a process or a C
  dependency, and keeping it consistent with snapshots would be much harder.
* **Exposing Tantivy's field-qualified syntax.** It leaks internal fields. The Sparkles
  grammar replaces it in Phase 2.

## 9. Open questions (defaults chosen here)

1. The default `predicates: "all"` could instead be a set of label predicates: `rdfs:label`,
   `skos:prefLabel`, `skos:altLabel`, `dcterms:title` and `schema:name`.
2. Here an unindexed predicate in `text:query` is a 400 error. Jena warns and returns no rows.
3. Without an explicit limit, all hits are returned, up to `maxHits`, and then the query
   fails with 507. Jena silently truncates at 10,000.
4. `?score` is an `xsd:float`, as in Jena. An `xsd:double` could be inlined in the id.
5. ASCII folding is on in the standard analyzer, so `café` matches `cafe`.
6. The 4th slot of the subject list binds `<urn:x-arq:DefaultGraph>` for default-graph hits.
7. The inferred graph is indexed by default.
8. Dedup over a merged default graph keeps the highest score.
9. `text:query` in the `WHERE` clause of an update sees the text state at the start of the
   transaction.
10. In Phase 1 a rebuild blocks writers, and the full rebuild at open is synchronous.
11. The latency of a Tantivy commit per update needs measuring (§5.3). An asynchronous
    commit mode may be needed.
12. Sparkles could also accept a native namespace alias, such as `urn:x-sparkles:text#query`.
    This is not planned; Sparkles uses Jena's IRI.

## 10. Sources

* The Sparkles repository:
  * `README.md`, `docs/API.md` and `docs/AUDIT.md` for feature status and API conventions;
  * `crates/sparkles/src/store.rs` for generations, the WAL format and replay,
    `publish_log`, `rebuild_locked` and the preservation of blank-node ids;
  * `id.rs` for term keys and tags;
  * `vocab.rs` for `prefix_range` and `get_sorted`;
  * `sparql/plan.rs` for `collect`, `plan_group`, `graph_filter` and `term_pattern`;
  * `sparql/exec.rs` and `sparql/cache.rs` for dispatch and cache keys;
  * `sparql/ctx.rs` for `DatasetSpec` and the default and union graph IRIs;
  * `error.rs` and `sparkles-server/src/{main,http,state}.rs` for the CLI, routes, status
    codes and the registry;
  * `ui/src/routes` and `ui/src/lib/{api.ts,components}` for the UI structure.
* Apache Jena (Apache-2.0):
  * https://jena.apache.org/documentation/query/text-query.html, for the `text:query` forms,
    `lang:`, `highlight:`, graph behavior and multiplicity (one hit per matching triple);
  * `jena-text/src/main/java/org/apache/jena/query/text/TextQueryPF.java`, for argument
    parsing, the subject list of up to 5 elements, the errors for non-variables, and the
    score as `floatToNode` (`xsd:float`);
  * `TextIndexLucene.java`, for the default `MAX_N = 10000` when there is no limit.
* Tantivy (MIT):
  * https://crates.io/api/v1/crates/tantivy, for version 0.26.2, 2026-09-08, MIT;
  * the docs.rs pages for `tantivy::query::QueryParser` (syntax, strict and lenient
    parsing), `tantivy::indexer::PreparedCommit` (`set_payload`, `commit`),
    `tantivy::index::IndexMeta` (`payload`, `opstamp`), `tantivy::tokenizer` (built-in
    tokenizers and filters, the `stemmer` feature) and `tantivy::snippet::SnippetGenerator`,
    and the crate's feature list.
* `spargebra` 0.4.7 (Apache-2.0/MIT), `src/parser.rs`, which turns collections into
  `rdf:first`/`rdf:rest` blank-node triples.
* W3C SPARQL 1.1 Query Language §4.2.3 (RDF collections); RDF 1.1 Concepts (literals); IETF
  BCP 47 and RFC 4647 §3.3.1 (basic filtering).
* S. Robertson, H. Zaragoza, "The Probabilistic Relevance Framework: BM25 and Beyond" (2009);
  G. Cormack, C. Clarke, S. Büttcher, "Reciprocal Rank Fusion outperforms Condorcet and
  individual Rank Learning Methods" (SIGIR 2009).
* **Not consulted:** Fluree. No Fluree source, documentation, website or other material was
  opened or used for this spec.

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30 (`1120061`), right after durable commit
identity, and mostly as specified. Each quad is one Tantivy 0.26.2 document, keyed by a hash
of its terms' vocabulary keys. A planner leaf applies the graph, predicate, language and
subject filters inside the search. The rest is as designed: `text.json` and `text/`,
`sparkles text-index`, `serve --text`, `GET /$/text/{ds}` and `POST /$/text/{ds}/rebuild`.
Two Phase 2 items came with it or soon after: `PUT`/`DELETE /$/text/{ds}`, and the UI with
an admin panel and ranked search in Explore. Clones keep text search enabled (`89b455c`).

**Deviations, and why.**
- *Commit path.* A Tantivy commit with an fsync on every write cost too much (§5.3, open
  question 11). The first change (`8f3eb13`) stopped fsyncing index commits. The WAL is the
  durable record, and a checkpoint about once a second bounds the unsynced window. At open,
  a dirty index is verified by checksum and caught up from the WAL instead of rebuilt. The
  second change (`9bc3db2`, `2cc3361`) made the commit lazy. A write only stages its
  documents. They are committed by the next text query that needs them, by a tick about
  once a second, or once about 16,000 changes are staged. Each snapshot still searches
  exactly its own documents, so read-your-writes holds.
  [COMPARISON.md](../COMPARISON.md#divergences-from-jena--qlever-decisions) records this as
  a design decision.
- *States.* `TextStatus.state` is `ready` or `stale`. Rebuilds run as background tasks
  (`202` with a `text-rebuild` task) instead of using the spec's `rebuilding` and `failed`
  states.
- *Limits.* `maxHits` is reported through the shared budget error (`507`, row budget) that
  came with per-request budgets ([C01](C01-observability-and-budgets.md)).
- *Historical reads.* A `text:query` against an `?at=` snapshot
  ([F06](F06-snapshots-and-point-in-time.md)) answers `501` (`history-unsupported`), not
  `503`.
- *Doc store.* The compression work ([X01](X01-compression-codecs.md)) made zstd the default
  for the doc store (`docstoreCompression`). That brings in tantivy's `zstd-compression`
  when the `zstd` feature is on.

**Tests at landing.** Integration tests in `crates/sparkles/tests/text.rs` cover the
acceptance examples, with a test hook that fails an index commit. Server tests cover the
routes, and the Playwright smoke tests include text search.

**Performance** ([benchmarks](../BENCHMARKS.md#full-text-index-and-observability-105m-triples),
10.5M triples). Building the index over every string literal takes 8.0 s and 102 MB on
disk. With the lazy commit, a 1,000-triple insert takes 28.4 ms with text search on, against
24–26 ms with it off and 46.2 ms with a commit per write. A top-10 `text:query` takes about
15 ms.

**Query syntax, limits and search cost (2026-10-02).** The full-text benchmark
(`scripts/bench-text.sh`) compared Sparkles with Fuseki and jena-text and found three
problems, which this change fixed.

- *Query syntax.* Tantivy's query parser found nothing for a single word with a trailing
  `*`, so `al*` and `+ada +lov*` returned no rows and no error. Instead of the small grammar
  that Phase 2 planned, Sparkles now parses Lucene's classic query syntax itself
  (`text/lucene.rs`) and applies Lucene's rules for combining clauses. Words, phrases,
  slops, prefixes, wildcards, fuzzy words, regular expressions, ranges, boosts and the
  boolean operators all work. On the benchmark data, 67 query strings that cover these
  forms matched the same literals as in Jena, and both engines refused 5 malformed ones.
  A sloppy phrase uses Lucene's semantics,
  which accept the words in either order, so it has its own Tantivy query. A fuzzy word
  expands to at most 50 terms, as Lucene's `FuzzyQuery` does. Forms that would find
  nothing or that Sparkles cannot reproduce give `400`. These are field names, a query
  with only excluded words or with no word left after analysis, and a few Lucene regular
  expression operators. The phrase prefix `"quick bro"*` stays a Sparkles extension, which
  Jena reads as the phrase or any document.
- *Limits above `maxHits`.* A search with an explicit limit above `maxHits` fetched
  `maxHits + 1` hits, treated them as all hits and returned them without an error. Such a
  limit now counts as no limit, so more than `maxHits` hits give `507`.
- *Cost per hit.* Resolving each hit loaded its stored document, which cost a doc store
  block decompression per hit. Index format 2 keeps `s`, `p`, `o` and `g` in columns
  (fast fields) and stores nothing, so the Phase 3 item that planned fast fields for `s`
  and `g` is done. A search reads only the columns its outputs need, decodes each distinct
  term once in sorted order, and caches the term ids per segment for the store generation.
  The planner drops a score or literal output that the query uses nowhere else, so a
  `COUNT` or a subject join reads no literal. A search without a limit collects its hits
  without a top-k heap. On 1.05M triples, counting the 4,937 hits of a common word went from
  about 20 ms to about 2 ms per HTTP request, and joining them with a structural pattern
  went from about 20 ms to about 3 ms. These are medians of 40 requests on a busy machine,
  with the id cache warm. Without the cache, each distinct term costs a dictionary lookup
  of about half a microsecond. The `docstoreCompression` setting is still accepted, but it
  no longer changes the index size.

**Highlighting (2026-10-02).** Jena's `highlight:` argument works, with Jena's options
and defaults (`m:`, `z:`, `s:`, `e:`, `f:`, `jh:` and `jf:`). Sparkles does not use
Tantivy's `SnippetGenerator`, because it returns a single fragment and does not see the
terms of prefix, wildcard and fuzzy queries. `text/highlight.rs` follows Lucene's
`Highlighter` with a `SimpleFragmenter` instead. It cuts fragments at the same tokens,
scores them by their distinct query words, keeps the best, merges adjacent ones, and marks
a phrase only where it occurs. The query parser records what each word that is not
excluded matches, including the terms a fuzzy word expanded to. The literal's text comes
from the store's dictionary, so the index stores no text, and a search without
`highlight:` reads none. On the benchmark data, 14 highlighted queries returned the same
literals as Jena, fragments and marks included. One difference is deliberate. Jena drops
the language tag of a highlighted literal unless its index has a language field, while
Sparkles always keeps it. `GET /{ds}/text` from §2.2 returns ranked hits with HTML
snippets, escaped by Sparkles with the matches in `<mark>`. The full-text benchmark has a
sixth query for highlighting, on which Fuseki also runs.

**Not built.** Stemming, online rebuilds with a journal, `text` on dataset creation, and
the rest of Phase 3.
