# F03: Full-text search (Tantivy, `text:query`)

> **Status:** implemented in part
>
> **Phases:** Phase 1 (the `text` cargo feature, `text:query`, per-quad documents kept
> current in the commit path, open-time catch-up or rebuild, `sparkles text-index`,
> `/$/text/{ds}`) shipped, together with these Phase 2 items: `PUT`/`DELETE /$/text/{ds}`
> and the UI (index admin panel, ranked search in Explore). Snippets, highlighting,
> stemming, the Sparkles query grammar and the Phase 3 items are not built.
>
> **User docs:** [API: Full-text search](../API.md#full-text-search) · [Features](../FEATURES.md#sparql-arq-equivalent) · [Benchmarks: Full-text index and observability](../BENCHMARKS.md#full-text-index-and-observability-105m-triples)
>
> This is the design as written before implementation; the [Outcome](#outcome) section at
> the end records how it landed.

It depends on the commit-identity work ([CI](CI-commit-identity.md): a durable,
monotonically increasing commit sequence number `seq` on the store and on every
`Snapshot`).

## 1. Summary

Sparkles gets an embedded, ranked (BM25) full-text index over string literals, queried from
SPARQL through Jena's `text:query` property function. Results are subjects, the matching
literals, graphs, predicates and scores, and they join with ordinary graph patterns. The index
is derived data. It lives next to the store, is kept current inside the commit path so a query
always sees text results consistent with its snapshot, and can be rebuilt from RDF at any time.

Library: **Tantivy 0.26.2** (crates.io, released 2026-09-08), license **MIT**. It is used with
`default-features = false, features = ["mmap", "stemmer", "lz4-compression"]` so no C
dependency (zstd) comes in. Record this in [PROVENANCE.md](PROVENANCE.md) when implementing.

### Goals

* `?s text:query "…"` and the list forms `(?s ?score ?literal ?g ?prop) text:query (prop* "q" limit "lang:xx")`,
  a Jena-compatible subset, usable anywhere a triple pattern is (BGP, `GRAPH`, `OPTIONAL`,
  `EXISTS`, subqueries, updates' `WHERE`).
* Read-after-write: an update that returns 200 is visible to the next text query.
* Stable document keys taken from RDF term identity, so compaction never re-indexes anything.
* Crash safety: the RDF store stays authoritative. After a crash, a config change or a
  corrupt index, the text index is caught up or rebuilt without touching RDF data.
* An opt-in cargo feature `text` on `sparkles`. `sparkles-server` enables it by default.
  Each dataset opts in separately, so datasets without text search pay nothing.

### Non-goals (all phases unless noted)

* No new literal datatype. Literals stay `xsd:string` / `rdf:langString`.
* No Lucene/Jena index compatibility and no Jena assembler (`text:TextDataset`) configuration.
* Phase 1 has no highlighting, no per-language stemming, no facets, no hybrid vector retrieval
  and no text search at historical snapshots (see §6).
* No distributed or remote index.

## 2. User-visible behavior

### 2.1 SPARQL

Namespace: `PREFIX text: <http://jena.apache.org/text#>` (Jena's IRI, so existing queries run
unchanged). A triple pattern whose predicate is `text:query` is a property function, not a
data match.

```
subject := term                       # ?s  or a constant IRI / blank-node label
         | ( term [?score [?literal [?g [?prop]]]] )
object  := "query"  |  "query"@lang
         | ( iri* "query" [limit] ["lang:xx"] )
```

| Slot | Meaning |
|---|---|
| `?s` / constant | subject of the matching quad. A constant restricts retrieval to that subject. A literal subject gives no results (as in Jena). |
| `?score` | BM25 score as `xsd:float` (Jena's type). Higher means a better match. |
| `?literal` | the matching literal, the exact stored RDF term (lexical form + language tag) |
| `?g` | graph of the matching quad (named graph IRI; `<urn:x-arq:DefaultGraph>` for the default graph) |
| `?prop` | predicate of the matching quad |
| `iri*` | restrict to these predicates (zero = every indexed predicate) |
| `"query"` | query string (§4.3). A language tag on it acts as `"lang:tag"`. |
| `limit` | integer. `n > 0` keeps the top `n` hits. `≤ 0` or absent means all hits, up to `maxHits`. |
| `"lang:xx"` | only literals whose language tag matches `xx` (RFC 4647 basic filtering: `en` matches `en`, `en-GB`) |

Semantics:

* **Result multiplicity.** One solution per matching indexed quad `(s, p, o, g)`. A subject
  with two matching literals appears twice, as in Jena, where each matching triple is a hit.
  Use `DISTINCT` or `GROUP BY ?s` with `MAX(?score)` to collapse. When the active graph is a
  merge of several graphs (union default graph, `FROM a FROM b`) and `?g` is not requested,
  hits with the same `(s, p, o)` are merged into one, keeping the highest score. Triple-pattern
  scans follow the same RDF merge semantics.
* **Evaluation order.** `text:query` is a table function. It is evaluated once, independently
  of the rest of the group, and then joined. `limit` therefore means the top-`n` hits of the text
  search within its graph scope and its predicate, language and subject restrictions,
  *before* any join. Its answer does not depend on where the pattern appears in the query.
  (In Jena it does; Jena's documentation recommends putting `text:query` first, and in that
  position the two agree.)
* **Graph scope inside retrieval.** The active graph (default graph, `GRAPH <g>`,
  `GRAPH ?g`, `FROM`/`FROM NAMED`, union default graph, `reasoning=false`) becomes a filter
  inside the Tantivy query. Top-`k` is taken within that scope and never post-filtered.
  `GRAPH ?g { … }` binds `?g` from the hit. Named graphs only; default-graph documents are
  excluded, as for scans.
* **Ordering.** The operator emits rows by descending score (ties broken by document key),
  but SPARQL gives no order guarantee after joins, so use `ORDER BY DESC(?score)`. Scores use
  index-wide statistics and are not stable across updates or merges. Compare rankings, not
  values.
* **OPTIONAL / MINUS / EXISTS / FILTER.** No special handling. The text leaf is
  planned like any other leaf, `FILTER`s over `?score` / `?literal` are placed after it, and
  `OPTIONAL { (?s ?sc) text:query "x" }` left-joins the independent hit table.
* **Inside updates.** In `DELETE/INSERT … WHERE`, `text:query` sees the text index as of the
  transaction's start and does not see earlier operations of the same request. This is a
  documented limitation.
* **Arguments must be constants** in Phase 1 (query string, predicates, limit, lang). A
  variable there is a 400 error. Bound-variable arguments come in Phase 3.
* **No function form.** Sparkles adds no `FILTER` function. `CONTAINS` / `REGEX` already
  have key-level fast paths, and only the property function can produce scores and top-`k`.

### 2.2 HTTP (`/$/` extensions, documented in `docs/API.md`)

| Method | Path | Phase | Description |
|---|---|---|---|
| GET | `/$/text/{ds}` | 1 | `TextStatus` (below). `404` unknown dataset. When text search is off: `{ "enabled": false }`. |
| POST | `/$/text/{ds}/rebuild` | 1 | Full rebuild from the current snapshot. Returns `Task` (`kind: "text-rebuild"`). `409` if one is running, `403` on `--read-only`, `400` if text search is not enabled. |
| PUT | `/$/text/{ds}` | 2 | Body `TextConfig`: enable or reconfigure, then a rebuild task. `400` invalid config. |
| DELETE | `/$/text/{ds}` | 2 | Disable. Removes `text.json` and the index directory. |
| POST | `/$/datasets` | 2 | Optional `text: true \| TextConfig` in the JSON body. |
| GET/POST | `/{ds}/text?q=&predicate=&lang=&graph=&limit=&highlight=` | 2 | Convenience search for the UI. Returns JSON hits with snippets (§2.5). |

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

`load`, `update`, `query` and `infer` on a database that has `text.json` maintain and use the
index automatically. Built without the feature, `text-index` exits 2 with
`built without full-text search (cargo feature "text")`.

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

The configuration persists in the database directory, so `Dataset::open` picks it up. SPARQL
is the query interface; Phase 2 adds `Dataset::text_search(&TextRequest) -> Result<Vec<TextHit>>`,
which serves `/{ds}/text`.

### 2.5 UI (SvelteKit, `ui/`, Phase 2)

* **Search page** (`/search`, new nav item "Search", uses the header's dataset switcher).
  * Controls: a query box with a syntax cheat sheet (terms, `"phrase"`, `AND`/`OR`, `-term`,
    `prefix*`); filters for predicate (multi-select of configured or top predicates),
    language and graph; a limit.
  * Results: a ranked list. Each row shows a score bar, the subject (a `TermView` linking to
    the explorer), the predicate, the literal snippet with highlighted matches, the language
    tag and the graph.
  * Footer: "N hits · docs · index seq · ms".
  * "Open in query editor" writes the equivalent `text:query` SELECT into `/query`.
  * Datasets without an index show an empty state with an "Enable full-text search" button
    (a config dialog calling `PUT /$/text/{ds}`).
* **Dataset page:** a new "Full-text search" panel.
  * State badge (ready / rebuilding with progress / stale / failed with message / off).
  * Indexed docs, index size, `seq` vs store `seq`, configured predicates and graphs, last
    rebuild.
  * Buttons: Rebuild (a task in `TaskList`), Configure, Disable.
* **Create-dataset dialog:** an "Enable full-text search" checkbox plus an optional predicate
  list.
* **Query editor:** `text:` added to the prefix completions, plus an example query in
  `examples.ts`.
* **`api.ts`:** `textStatus`, `textRebuild`, `textConfigure`, `textDisable`, `textSearch`.

## 3. Standards basis

* SPARQL 1.1 Query §4.2.3 (RDF collections): `( … )` in a pattern is syntax for an
  `rdf:first`/`rdf:rest` chain of blank nodes. `spargebra` expands it that way (verified in
  `spargebra-0.4.7/src/parser.rs`), which is how the list arguments reach the planner.
  Property functions are an ARQ extension: a query that uses one is still a valid SPARQL 1.1
  query and is evaluated with extension semantics.
* RDF 1.1 Concepts: simple literals (`xsd:string`) and `rdf:langString`. RDF 1.2 directional
  language strings are indexed like language strings.
* BCP 47 language tags; RFC 4647 §3.3.1 basic filtering for `lang:`.
* BM25 (Robertson & Zaragoza), Tantivy's default scorer.
* Jena's text query documentation and `jena-text` sources, for syntax compatibility.

## 4. Semantics

### 4.1 What is indexed

A quad `(s, p, o, g)` is a **document** when all of these hold:

* `o` is a literal whose datatype is `xsd:string`, `rdf:langString` or `rdf:dirLangString`.
  From the vocabulary key: it starts with `"` and its suffix after `0xFF` is empty or starts
  with `@`. Inline ids are never strings.
* `p` is in `predicates` (`"all"` accepts every predicate).
* `g` is in the graph scope. By default every graph is indexed, including
  `urn:x-sparkles:inferred`: queries with `reasoning=true` include that graph and
  `reasoning=false` excludes it through the graph filter.

The **default configuration** is `predicates: "all"`, so search works on arbitrary data
without setup. Narrow it to control index size (open question 1).

Analyzer "standard":

* Tantivy `SimpleTokenizer` (splits on non-alphanumeric characters), then `RemoveLongFilter(40)`,
  `LowerCaser` and `AsciiFoldingFilter`.
* Registered as `sparkles_standard`. Used both at index time and for query parsing.
* Text past `maxTextBytes` is truncated at a char boundary before tokenizing; the document and
  `?literal` keep the full term.
* CJK text is not segmented (a run of CJK characters is a single token). An n-gram analyzer is
  a later option.

### 4.2 Consistency

* Every `Snapshot` carries `text: Option<Arc<TextView>>`. A `TextView` holds a Tantivy
  `Searcher`, the `seq` it corresponds to, and the epoch.
* **Invariant:** a snapshot whose `text.seq == snapshot.seq` gives exactly the documents that
  §4.1 derives from that snapshot. The commit path (§5.3) keeps this true for every published
  snapshot unless text maintenance fails.
* If `text` is missing (not enabled) → `400 "dataset has no full-text index; enable it with
  sparkles text-index or PUT /$/text/{ds}"`.
* If `text.seq != snapshot.seq` or the state is `rebuilding`/`failed` →
  `503 "full-text index of 'ds' is <state> (index seq A, data seq B); retry later or POST
  /$/text/ds/rebuild"`. There is no silent stale read. An opt-in stale mode is a later option.
* Text failures never fail RDF writes. If Tantivy errors after the WAL commit is durable, the
  update still returns 200. The dataset's text state becomes `stale` (logged with the error)
  and stays there until a rebuild.

### 4.3 Query string (Phase 1 subset)

In Phase 1 the query string is parsed by Tantivy's `QueryParser` over the `text` field, in
strict mode (`parse_query`), with OR as the default conjunction (as in Lucene/Jena).

Supported and tested syntax:

* terms: `fox`;
* phrases: `"brown bears"`, with slop: `"brown bears"~2`;
* `AND` / `OR` (AND binds tighter);
* `+term` / `-term`;
* parentheses;
* phrase prefix: `"quick bro"*`.

Everything else is unsupported, in particular field-qualified clauses (`field:x`), ranges,
`IN`, boosts and `*` alone. Parse errors → `400 "text:query: <parser message>"`; a literal
`:` must be escaped as `\:`.

Phase 2 replaces this with a small Sparkles grammar (Lucene-classic subset) that compiles
directly to Tantivy queries. It adds bare `prefix*` (`RegexQuery`), `term~` fuzzy and `NOT`,
treats `:` as plain text, and makes internal fields unreachable.

### 4.4 Errors (all `{ "error": … }` bodies per `docs/API.md`)

| Condition | Status | Message (prefix) |
|---|---|---|
| query string not a string literal (typed, IRI, number) | 400 | `text:query: query string must be a string literal` |
| subject list longer than 5 or empty, or object list without a query string / with more than 3 trailing arguments | 400 | `text:query: malformed argument list` |
| score/literal/graph/prop slot not a variable | 400 | `text:query: <slot> must be a variable` |
| variable in the object list (Phase 1) | 400 | `text:query: arguments must be constants` |
| predicate not indexed | 400 | `text:query: <p> is not text-indexed (indexed: …)` |
| `highlight:` argument (Phase 1) | 400 | `text:query: highlight is not supported yet` |
| query parse error | 400 | `text:query: …` |
| hits exceed `maxHits` without a limit | 507 | `text:query matched more than N documents; add a limit` (`Error::MemoryLimit`) |
| index not ready (§4.2) | 503 | see §4.2. New `Error::TextUnavailable` maps to 503 with `Retry-After: 5` |
| built without feature `text` | 501 | `built without full-text search (cargo feature "text")` (`Error::Unsupported`) |
| rebuild already running | 409 | `text index rebuild already running` |

A malformed `rdf:first`/`rdf:rest` chain (a blank node with extra or missing links) gives 400.

### 4.5 Limits and budgets

* `maxHits` (default 1,000,000) caps a limit-less evaluation. The row count also passes through
  `ctx.check_rows`.
* Tantivy search is not interruptible, so `ctx.check()` runs before the search and after it,
  and every 4096 rows while hits are resolved.
* The incremental writer uses 1 thread and a 32 MiB heap. A rebuild uses `min(8, cores)`
  threads and 64 MiB per thread.

## 5. Design

### 5.1 Modules

| Where | Change |
|---|---|
| `crates/sparkles/Cargo.toml` | `tantivy = { version = "0.26", default-features = false, features = ["mmap","stemmer","lz4-compression"], optional = true }`; `[features] text = ["dep:tantivy"]` |
| `crates/sparkles-server/Cargo.toml` | `text = ["sparkles/text"]`, added to `default` |
| `sparkles/src/text/mod.rs` (new, `cfg(feature="text")`) | `TextConfig`, schema, `TextIndex` (writer, reader, state, paths), `TextView`, `apply_commit`, `rebuild`, `recover`, `search` |
| `sparkles/src/sparql/textpf.rs` (new, always compiled) | recognizes `text:query` in a BGP and decodes the collections into a `TextCall`. Pure algebra, no Tantivy. |
| `sparql/plan.rs` | `collect()` / `GP::Bgp`: before `self.triple(tp, g)` is pushed, `textpf::extract(patterns)` removes each `text:query` triple and its `rdf:first`/`rdf:rest` chain triples, then pushes `Item::Text(TextItem { call, graph: g.clone() })`. `plan_group()` turns each `Item::Text` into a leaf `Node` (`Kind::TextSearch(Box<TextSpec>)`, graph filter from `self.graph_filter(&item.graph)`, `est = min(limit, Σ doc_freq(query terms), docs)`) and adds it to `nodes` **before** the filter-equality pass, so its variables are never substituted. It then joins in the DP like any leaf. `Node::operator()` → `"TextSearch"`. |
| `sparql/exec.rs` | `Kind::TextSearch` → `crate::text::search(ctx, spec, &n.vars)` (without the feature: `Error::Unsupported`) |
| `sparql/cache.rs` | `write_node`: `TextSearch` is cacheable. The key adds query, predicates, lang, limit, subject, graph filter, output slots and `text.epoch` (the key already has `snap.version` and the generation). |
| `store.rs` | `Store` gets `text: Option<text::TextIndex>`; `Snapshot` gets `text` and `wal_commits: u64` (commits in the current generation's WAL visible to the snapshot); hooks in `open`, `publish_log`, `rebuild_locked` (§5.3–5.5) |
| `error.rs` | `Error::TextUnavailable(String)` |
| server `http.rs`, `main.rs`, `state.rs` | routes and handlers of §2.2, `text-index` command, `serve --text`, `DatasetInfo.text` |

`TextSpec { query, predicates: Vec<String>, lang: Option<String>, limit: Option<usize>,
subject: PathEnd, score/literal/graph_out/prop_out: Option<VarId>, graph: GraphFilter,
graph_var: Option<VarId>, dedup: bool }`. `dedup` is true when the active graph is a merge
(`GraphFilter::Named` / `Set` without a graph variable) and `graph_out` is `None`.

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
| `key` | bytes, indexed | document key (below), used for `delete_term` |
| `s` | bytes, indexed + stored | subject vocabulary key (`<iri` or `_` + u64-be for blank nodes; blank-node ids survive rebuilds, see `rebuild_locked` `Slot::Id`) |
| `p` | string (raw), indexed + stored | predicate IRI |
| `o` | bytes, stored | literal vocabulary key (`"lex 0xFF @lang`); gives `?literal` and snippets |
| `g` | string (raw), indexed + stored | graph IRI, `_:b<hex>`, or `urn:x-arq:DefaultGraph` |
| `lang` | string (raw), indexed, multi-valued | lowercase full tag (without `--dir`) and primary subtag; absent for `xsd:string` |
| `text` | text, positions, tokenizer `sparkles_standard` | lexical form (≤ `maxTextBytes`) |

**Document key:** `SHA-256(varint(len) ‖ key(s) ‖ varint(len) ‖ key(p) ‖ varint(len) ‖ key(o) ‖ varint(len) ‖ key(g))[..16]`,
where `key()` is `id::write_term_key` / the vocabulary key and the default graph's key is
empty. It depends only on RDF term identity (`sha2` is already a dependency), so it survives
compaction, bulk rebuilds and vocabulary renumbering.

**Commit payload** (`PreparedCommit::set_payload`, read back from `IndexMeta.payload`):
`{"format":1,"seq":S,"generation":"gen-0007","walCommits":N,"epoch":E,"config":"<sha256 of canonical text.json>"}`.

* `seq` is the consistency watermark.
* `(generation, walCommits)` is the position used for crash catch-up. It does not depend on
  how the commit-identity spec numbers compactions.

### 5.3 Commit path (`WriteTxn::publish_log`)

The ordering is:

1. `dvocab.sync()`, then the WAL write and fsync, as today.
2. **Text maintenance** (only when `store.text` is enabled and healthy). `touched` = the
   distinct quads of `self.log` that §4.1 selects. The predicate check uses a per-generation
   `FxHashMap<Id, bool>` cache, and keys come from the transaction view.
   * `touched` empty: `TextView { searcher: prev.searcher.clone(), seq: new_seq, .. }`. No
     I/O, so non-text updates cost one hash lookup per logged quad.
   * `touched` non-empty: for each quad, `writer.delete_term(key)`, then `add_document` if the
     new delta view `contains` the quad. This upserts by final presence, so it is idempotent
     and order-free. Then `prepare_commit`, `set_payload`, `commit`, `reader.reload()`, and a
     fresh `Searcher`.
   * On error: `writer.rollback()`, state `stale`, keep the previous view (its `seq` is now
     behind, so text queries return 503).
3. Publish the `Snapshot` with `text` and `wal_commits + 1`, as today.

The writer mutex already serializes commits, so the Tantivy `IndexWriter` (one per dataset,
kept open) needs no lock of its own.

**Latency trade-off:** each update that touches indexed literals pays a Tantivy commit (a
small segment and `meta.json` plus fsyncs). Tantivy merges segments in the background.
Measure with `scripts/bench.sh` update latency on a text-enabled dataset. Updates that touch
no indexed literals, and datasets without text search, must show no regression.

### 5.4 Generation switches (`rebuild_locked`)

* **Compaction** (`extra` and `extra_quads` both empty; content unchanged): commit an empty
  Tantivy transaction with the payload `{seq, generation: new, walCommits: 0}` and publish
  with the same searcher. No documents are touched.
* **Content-changing bulk rebuilds** (`Store::load` bulk path, `insert_bulk` at or above
  `bulk_threshold`): once the new generation is built and `CURRENT` has switched, and while
  the writer lock is still held, run a full text rebuild (§5.5) from the new, unpublished
  snapshot, then publish the snapshot with the new view.
  * Phase 3 makes this incremental from `extra_quads` plus the transaction log.
  * A crash between the `CURRENT` switch and the text commit is caught at open: the payload's
    generation does not match, so the index is rebuilt.

### 5.5 Full rebuild

1. Create `text.new/` and use a multi-threaded writer.
2. Enumerate candidates:
   * with a predicate list: a `PSO` scan per predicate id (`lookup_iri`);
   * with `"all"`: `for_each_quad`, keeping literal objects. Phase 3 narrows this to an
     `OPS` range scan over the literal id range from `Vocab::prefix_range(b"\"")` plus delta
     literals.
   * Keys are resolved in batches (`Vocab::get_sorted`).
3. Commit with the payload, then drop the writer and index.
4. Swap: fsync `text.new`, rename `text` → `text.old`, rename `text.new` → `text`,
   `sync_dir(root)`, remove `text.old`.
5. Reopen at `text/`, bump `epoch`, and republish the current snapshot with the new view
   (`Snapshot` is `Clone`; same `version`, and the cache key includes the epoch).

In Phase 1 an online rebuild (`POST …/rebuild`) holds the writer lock for its whole duration.
Updates wait; reads continue on the old view, or get 503 if it is not ready. Phase 2 releases
the lock during the build: the commit path appends touched quad keys to a journal, and a
short locked catch-up replays the journal before the swap.

### 5.6 Open and recovery (`Store::open`, before WAL replay)

1. No `text.json` → text search is off. Built without the feature but `text.json` exists →
   warn once and ignore it. The WAL/generation position then diverges, and a later open with
   the feature repairs it.
2. Delete `text.new/` and `text.old/`. Open `text/`. If it is missing, unreadable, has
   `format != 1` or a different config hash → **full rebuild** after the WAL replay.
3. Otherwise read the payload `P`. If `P.generation == CURRENT`, the WAL replay collects the
   quads of commits numbered `> P.walCommits`, and these are applied with the §5.3 rule
   (upsert or delete by presence in the recovered snapshot) in one Tantivy commit. Otherwise
   (a generation switch without a text commit) → full rebuild.
4. `P.walCommits` greater than the WAL's commit count (a torn WAL tail cannot cause this,
   because the text commit comes after the WAL fsync; it means foreign or restored files) →
   full rebuild.

A full rebuild at open is synchronous in Phase 1 and logged with progress; Phase 2 moves it
to the background (state `rebuilding`, 503s meanwhile). Authoritative files (`CURRENT`,
generations, WAL, `delta.vocab`) are never written by text code. Backups stay N-Quads only,
and a restore rebuilds the index.

### 5.7 Search (`text::search`)

1. Check `snap.text` (§4.2).
2. Build `BooleanQuery[Must(parsed text query), Must(ConstScore(filters, 0.0))]`. The filters
   are `TermSetQuery(p ∈ predicates)`, `TermSetQuery(lang)`, `TermQuery(s = subject key)` and
   the graph filter:
   * `Default` → `g = urn:x-arq:DefaultGraph`;
   * `Named` → `MustNot` that term;
   * `One` / `Set` → `TermSetQuery` of `snap.term(id)` rendered as `g` strings.
3. Collect with `TopDocs::with_limit(limit or maxHits + 1)`. With `dedup`, over-fetch and
   double until `limit` distinct `(s, p, o)` are found or the hits run out.
4. Resolve stored fields to ids with `snap.lookup_key`, or `Id::bnode` for `_` keys. Local
   terms (the default-graph IRI, scores) go through `ctx`. A key that does not resolve counts
   as `staleHits` in the plan description and is skipped; in debug builds it asserts.
5. Emit columns in `n.vars` order.

The plan description looks like
`TextSearch ?s ?score ← "brown" [rdfs:label] lang=en limit 10 graph=default`.

## 6. Phasing

### Phase 1 (MVP, about one day)

1. The `text` features (`sparkles`, server default), `text.json`, schema v1, `TextIndex`, and
   `Snapshot.text` / `wal_commits`.
2. Maintenance: the §5.3 commit hook, compaction and bulk handling (§5.4), synchronous full
   rebuild (§5.5), and open-time catch-up or rebuild (§5.6).
3. `text:query` recognition, `TextSearch` operator, graph scope, dedup, limit, `lang:`,
   constant subject, score, cache key, and the errors of §4.4.
4. CLI `sparkles text-index` (enable/`--predicate`/`--exclude-graph`/`--rebuild`/`--status`/`--disable`),
   `serve --text`, and `GET /$/text/{ds}` plus `POST /$/text/{ds}/rebuild` (synchronous
   inside the task).
5. Tests: §7 A1–A12 as `crates/sparkles` unit/integration tests; A13–A14 as server tests.
   `docs/API.md` and the README status table updated (the "Full-text search" gap row).

### Phase 2

* The UI of §2.5.
* `/{ds}/text` with snippets (`SnippetGenerator`, `Snippet::highlighted()` ranges, with HTML
  escaping done by Sparkles).
* Jena `highlight:` options in `text:query` (as in Jena, the highlighted string replaces
  `?literal`).
* `PUT`/`DELETE /$/text/{ds}`, `text` on dataset creation.
* The Sparkles query grammar (§4.3).
* Per-language stemmed fields `text_<lang>` (`languages: {"en": "english"}`), used when the
  query has `lang:`.
* Online rebuild with a journal; background rebuild at open.

### Phase 3 and later

* Variable arguments and bound-subject pushdown: when the left side is small and no limit is
  given, pass a `TermSetQuery` of subject keys.
* Tantivy fast fields instead of the doc store for `s`/`g`.
* Incremental bulk maintenance; an `OPS` literal-range rebuild.
* Facets by predicate, graph or language.
* Hybrid retrieval with [F04](F04-vector-search.md) vector search via reciprocal rank fusion
  (`score = Σ 1/(60 + rank)`), exposed as a Sparkles property function over both indexes.
* Text search at [F06](F06-snapshots-and-point-in-time.md) historical snapshots: an index retained per named snapshot, or built on
  demand. Until then a historical snapshot gets 503 because its `seq` differs.
* An opt-in "allow stale" query mode.

## 7. Acceptance examples

Data (`ds`, text search enabled with the defaults), prefixes `ex: <http://example.org/>`,
`rdfs:`, `text:`:

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
| A2 | `SELECT ?s ?lit ?g { GRAPH ?g { (?s ?sc ?lit) text:query "fox" } }` | one row: `ex:b4, "Fox in Socks", ex:g1` |
| A3 | `SELECT ?s { (?s ?sc) text:query (rdfs:label "brown") } ORDER BY DESC(?sc)` | `ex:b2, ex:b1` (the shorter field scores higher); `ex:b3` does not match (`brun`) |
| A4 | `SELECT ?s ?lit { (?s ?sc ?lit) text:query "brown" }` | 3 rows: `(b1, label)`, `(b2, "Brown Bears")`, `(b2, comment)`. This is the multiplicity rule. |
| A5 | `SELECT ?s ?lit { (?s ?sc ?lit) text:query "\"brown bears\"" }` | b2 twice (label and comment); with `(rdfs:label "\"brown bears\"")` only the label |
| A6 | `{ ?s text:query (rdfs:label "renard" "lang:fr") }` / `{ ?s text:query "renard"@en }` | `{ex:b3}` / empty |
| A7 | `SELECT DISTINCT ?s { ?s text:query "brown" . ?s a ex:Book }` | `{ex:b1, ex:b2}` |
| A8 | `SELECT ?s { (?s ?sc) text:query ("brown" 1) }` | exactly 1 row, `ex:b2` |
| A9 | `SELECT ?s ?sc { ?s a ex:Book OPTIONAL { (?s ?sc) text:query "fox" } }` | `b1` with a score, `b2` with `?sc` unbound |
| A10 | `INSERT DATA { ex:b5 rdfs:label "Brown Owl" }`, then A3's query right away; then `DELETE DATA { ex:b1 rdfs:label "The Quick Brown Fox"@en }` and query again | first `{b2, b5, b1}` (b5 in the first two), then b1 gone. `GET /$/text/ds` has `seq == storeSeq`, `docs` 7 then 6. |
| A11 | `sparkles compact --loc db`, then A1 | same answer, `epoch` unchanged, no rebuild logged |
| A12 | crash simulation: test hook `TextIndex::fail_next_commit`. The insert of A10 succeeds with 200 and the state is `stale`; A1 → 503. Close and reopen the store → catch-up from the WAL, state `ready`, A10 results correct. Deleting `text/` and reopening → full rebuild, same answers. | as described |
| A13 | `SELECT (COUNT(*) AS ?n) { ex:b2 text:query "bears" }` | `2` (constant subject, two documents) |
| A14 | with `--union-default-graph` and `ex:g2 { ex:b2 rdfs:label "Brown Bears" }` added: `SELECT ?s ?lit { (?s ?sc ?lit) text:query (rdfs:label "bears") }` | one row (merged), while `(?s ?sc ?lit ?g)` gives two rows |
| A15 | errors: `?s text:query 42`; `(?s 1) text:query "x"`; `?s text:query (ex:nope "x")`; `?s text:query "(fox"`; `text:query` on a dataset without an index | 400, 400, 400, 400, 400 with the §4.4 messages |
| A16 | `GET /$/text/ds` after load | `200 {"enabled":true,"state":"ready","docs":6,"seq":S,"storeSeq":S,"epoch":1,…,"config":{"predicates":"all"},"formatVersion":1}` |
| A17 | `POST /$/text/ds/rebuild` twice concurrently | first `200 Task{kind:"text-rebuild"}`, second `409`; on `--read-only` → `403` |
| A18 | `sparkles text-index --loc db --predicate http://www.w3.org/2000/01/rdf-schema#label` | stderr `text index: 5 docs (1 predicate), seq S, rebuilt in … ms`; A4 now returns 2 rows (the comment is no longer indexed) and `(rdfs:comment "x")` → 400 not indexed |
| A19 | a load larger than `bulk_threshold` into a text-enabled store | status `ready`, `seq == storeSeq`, documents from the new data searchable |
| A20 | `cargo build -p sparkles` (no feature) and a query with `text:query` | 501 / `Error::Unsupported`; no `tantivy` in `cargo tree -p sparkles` |

## 8. Rejected alternatives

* **A home-grown inverted index on the permutations** (QLever-style word/entity index):
  BM25, phrases, prefixes, snippets and segment merging would all be new code. Tantivy is
  mature, pure Rust, MIT, and embedded.
* **One document per subject** (Jena entity style: one Lucene document per subject, with
  fields per predicate): an update would have to re-read all of the subject's literals, and
  the matching literal and graph would be lost. One document per quad maps 1:1 onto RDF
  changes and gives exact `?literal` / `?g`.
* **Keys from vocabulary ids:** ids change with every generation, so every compaction would
  force a full re-index.
* **An index inside `gen-NNNN/` or built by the bulk `Builder`:** same problem, because
  compaction would rebuild text for nothing.
* **Asynchronous (eventual) indexing:** breaks read-after-write. It may return later as an
  opt-in stale mode.
* **An in-memory overlay for recent updates instead of per-commit Tantivy commits:** lower
  update latency, but two scoring domains (inconsistent BM25 statistics) and much more code.
  Revisit if §5.3 latency measurements demand it.
* **Post-filtering graphs or predicates after a global top-`k`:** wrong results with `limit`.
* **A dedicated "searchable text" datatype:** changes RDF term identity and breaks existing
  data and queries.
* **A `FILTER` function as the primary interface:** no scores and no top-`k`. `CONTAINS` and
  `REGEX` already cover substring filtering.
* **An external engine (Elasticsearch/Solr) or SQLite FTS5:** an extra process or a C
  dependency, and consistency with snapshots would be much harder.
* **Exposing Tantivy's field-qualified syntax:** it leaks internal fields; replaced by the
  Sparkles grammar in Phase 2.

## 9. Open questions (defaults chosen here)

1. The default `predicates: "all"` could instead be a label set (`rdfs:label`,
   `skos:prefLabel`/`altLabel`, `dcterms:title`, `schema:name`).
2. An unindexed predicate in `text:query` is a 400 error here; Jena warns and returns no rows.
3. Without an explicit limit, all hits are returned (cap `maxHits`, then 507); Jena silently
   truncates at 10,000.
4. `?score` is `xsd:float` (Jena) rather than `xsd:double`, which could be inlined in the id.
5. ASCII folding is on in the standard analyzer (`café` matches `cafe`).
6. The 4th subject-list slot binds `<urn:x-arq:DefaultGraph>` for default-graph hits.
7. The inferred graph is indexed by default.
8. Merged-default-graph dedup keeps the highest score.
9. `text:query` inside update `WHERE` clauses sees the text state at transaction start.
10. Phase 1 rebuilds block writers; the full rebuild at open is synchronous.
11. The per-update Tantivy commit latency needs measuring (§5.3); an async commit mode may be
    needed.
12. Should Sparkles also accept a native namespace alias (e.g. `urn:x-sparkles:text#query`)?
    Not planned; Jena's IRI is used.

## 10. Sources

* Sparkles repository:
  * `README.md`, `docs/API.md`, `docs/AUDIT.md`: feature status, API conventions;
  * `crates/sparkles/src/store.rs`: generations, WAL format and replay, `publish_log`,
    `rebuild_locked`, bnode id preservation;
  * `id.rs`: term keys, tags;
  * `vocab.rs`: `prefix_range`, `get_sorted`;
  * `sparql/plan.rs`: `collect`, `plan_group`, `graph_filter`, `term_pattern`;
  * `sparql/exec.rs` and `sparql/cache.rs`: dispatch, cache keys;
  * `sparql/ctx.rs`: `DatasetSpec`, default/union graph IRIs;
  * `error.rs`, `sparkles-server/src/{main,http,state}.rs`: CLI, routes, status codes,
    registry;
  * `ui/src/routes`, `ui/src/lib/{api.ts,components}`: UI structure.
* Apache Jena (Apache-2.0):
  * https://jena.apache.org/documentation/query/text-query.html: `text:query` forms, `lang:`,
    `highlight:`, graph behavior, and multiplicity (a hit per matching triple);
  * Jena's `jena-text/src/main/java/org/apache/jena/query/text/TextQueryPF.java`:
    argument parsing, subject list of up to 5 elements, non-variable errors, and the score
    as `floatToNode` (`xsd:float`);
  * `TextIndexLucene.java`: default `MAX_N = 10000` when no limit.
* Tantivy (MIT):
  * https://crates.io/api/v1/crates/tantivy: version 0.26.2, 2026-09-08, MIT;
  * docs.rs pages for `tantivy::query::QueryParser` (syntax, strict and lenient parsing),
    `tantivy::indexer::PreparedCommit` (`set_payload`, `commit`),
    `tantivy::index::IndexMeta` (`payload`, `opstamp`), `tantivy::tokenizer` (built-in
    tokenizers and filters, `stemmer` feature), `tantivy::snippet::SnippetGenerator`, and the
    crate's feature list.
* `spargebra` 0.4.7 (Apache-2.0/MIT), `src/parser.rs`: collections become `rdf:first`/`rdf:rest`
  blank-node triples.
* W3C SPARQL 1.1 Query Language §4.2.3 (RDF collections); RDF 1.1 Concepts (literals); IETF
  BCP 47 and RFC 4647 §3.3.1 (basic filtering).
* S. Robertson, H. Zaragoza, "The Probabilistic Relevance Framework: BM25 and Beyond" (2009);
  G. Cormack, C. Clarke, S. Büttcher, "Reciprocal Rank Fusion outperforms Condorcet and
  individual Rank Learning Methods" (SIGIR 2009).
* **Not consulted:** Fluree. No Fluree source, documentation, website or other material was
  opened or used for this spec.

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30 (`1120061`), right after durable commit
identity, largely as specified: one Tantivy 0.26.2 document per quad keyed by a hash of
the terms' vocabulary keys, a planner leaf with graph, predicate, language and subject
filters inside the search, `text.json` plus `text/`, `sparkles text-index`,
`serve --text`, `GET /$/text/{ds}` and `POST /$/text/{ds}/rebuild`. Two Phase 2 items
came with it or soon after: `PUT`/`DELETE /$/text/{ds}`, and the UI (an admin panel and
ranked search in Explore). Clones keep text search enabled (`89b455c`).

**Deviations, and why.**
- *Commit path.* A Tantivy commit with an fsync per write cost too much (§5.3, open
  question 11). First (`8f3eb13`), index commits stopped fsyncing: the WAL is the durable
  record, a checkpoint about once a second bounds the unsynced window, and at open a
  dirty index is checksum-verified and caught up from the WAL instead of rebuilt. Then
  (`9bc3db2`, `2cc3361`) the commit became lazy: a write only stages its documents, and
  the next text query that needs them, a tick about once a second, or about 16,000
  staged changes commit them. Each snapshot still searches exactly its own documents
  (read-your-writes holds);
  [COMPARISON.md](../COMPARISON.md#divergences-from-jena--qlever-decisions) records this as
  a design decision.
- *States.* `TextStatus.state` is `ready` or `stale`; rebuilds run as background tasks
  (`202` with a `text-rebuild` task) rather than the spec's `rebuilding`/`failed` states.
- *Limits.* `maxHits` is reported through the shared budget error (`507`, row budget)
  introduced with per-request budgets ([C01](C01-observability-and-budgets.md)).
- *Historical reads.* A `text:query` against an `?at=` snapshot
  ([F06](F06-snapshots-and-point-in-time.md)) answers `501` (`history-unsupported`)
  rather than `503`.
- *Doc store.* With the compression work ([X01](X01-compression-codecs.md)) the doc store
  became zstd by default (`docstoreCompression`), which brings in tantivy's
  `zstd-compression` when the `zstd` feature is on.

**Tests at landing.** Integration tests in `crates/sparkles/tests/text.rs` cover the
acceptance examples, with a test hook that fails an index commit; server tests cover the
routes; the Playwright smoke tests include text search.

**Performance** ([benchmarks](../BENCHMARKS.md#full-text-index-and-observability-105m-triples),
10.5M triples): building the index over every string literal takes 8.0 s and 102 MB on
disk; with the lazy commit a 1,000-triple insert costs 28.4 ms with text search on against
24–26 ms off (46.2 ms with a commit per write); a top-10 `text:query` takes about 15 ms.

**Not built.** `/{ds}/text` with snippets, `highlight:`, stemming, the Sparkles query
grammar, online rebuilds with a journal, `text` on dataset creation, and all of Phase 3.
