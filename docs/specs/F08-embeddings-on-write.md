# F08: Embeddings computed on write

> **Status:** implemented in part
>
> **Phases:** Phase 1 shipped: the configuration, the worker, the input record, full
> passes, `reembed`, status, text queries, the HTTP, CLI, Python and UI surfaces, and tests
> with a mock provider. Of Phase 2, the metrics shipped. Chunking and a token-based rate
> limit are not built.
>
> **User docs:** [API: Embeddings on write](../API.md#embeddings-on-write) · [Usage: Embeddings](../USAGE.md#embeddings-computed-on-write) · [Features](../FEATURES.md#sparql-arq-equivalent)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

The design builds on the configured vector indexes of [F04](F04-vector-search.md), the
outbound policy of SERVICE and `LOAD`, the commit sequence of
[CI](CI-commit-identity.md) and the write path of
[C10](C10-write-time-validation.md).

## 1. Summary, goals, non-goals

A configured vector index can name an embedding source and a provider. Sparkles then
computes the index's vectors itself. After each commit, a background worker finds the
subjects whose selected text changed, sends the text to an embeddings endpoint that
speaks the OpenAI embeddings API, and writes the returned vectors as ordinary
`spk:vector` literals under the index's predicate. Those writes are commits of their
own. Every search, function and build of F04 then works on them unchanged, because the
literal stays the source of truth. A search may also pass a text instead of a vector,
and Sparkles embeds it with the same model.

**Goals**
- Writes stay as fast as they are today. The commit path only notes which subjects a
  commit touched, and a provider outage never blocks or fails a write.
- The vectors converge on the text: inserted text gets a vector, changed text gets a new
  one, and deleted text loses its vector. Restarts, crashes, bulk loads and edits made
  while no server ran are caught up without re-embedding text that did not change.
- One provider protocol covers hosted and local models: OpenAI's `POST /v1/embeddings`,
  which Ollama, vLLM, LM Studio, llama.cpp's server, LocalAI, Text Embeddings Inference
  and most gateways also serve.
- Nothing leaves the machine unless an index's configuration names a provider. Every
  request goes through the server's outbound policy, and API keys never enter the
  dataset, its configuration files or the API's responses.
- Status that answers "is it done yet": the backlog, progress, errors and the last
  commit whose changes are embedded.

**Non-goals**
- A model runtime inside Sparkles (§8 explains why). Local models run behind Ollama or
  another OpenAI-compatible server.
- Chunking long text into several vectors. Text past a configured length is truncated.
- Other provider protocols (Cohere, Vertex AI, Bedrock). Gateways such as LiteLLM
  translate them to the OpenAI protocol.
- Image and other non-text inputs.
- Strong consistency between a text write and its vector. The vector follows the text
  eventually, and §4.6 says how a client waits for it.

## 2. User-visible behavior

### 2.1 Configuration

The embedding settings are an optional `embedding` object in the configuration of a
vector index, the body of `PUT /$/vector/{ds}/{name}`. They are stored with the index in
`vector.json`.

```json
{
  "predicate": "http://example.org/embedding",
  "dimension": 768,
  "metric": "cosine",
  "embedding": {
    "url": "http://127.0.0.1:11434/v1/embeddings",
    "model": "nomic-embed-text",
    "predicates": ["http://www.w3.org/2000/01/rdf-schema#label"],
    "languages": ["en", ""],
    "classes": ["http://example.org/Article"],
    "inputPrefix": "search_document: ",
    "queryPrefix": "search_query: "
  }
}
```

| Field | Default | Meaning |
|---|---|---|
| `url` | required | The embeddings endpoint, an `http` or `https` URL such as `https://api.openai.com/v1/embeddings`. |
| `model` | required | The `model` sent with each request. |
| `apiKey` | none | Where the bearer token comes from: `{"secret": NAME}`, `{"env": VAR}` or `{"file": PATH}` (§2.3). Without it, requests carry no `Authorization` header. |
| `sendDimensions` | `false` | Send the index's dimension as the request's `dimensions` parameter, for models that shorten their output on request. |
| `predicates` | — | The predicates whose literals are embedded. One of `predicates` and `query` is required. |
| `languages` | all | Language ranges a literal's tag must match (RFC 4647 basic filtering). `""` matches literals without a tag. |
| `classes` | all | The subject must have one of these `rdf:type`s in the literal's graph. |
| `query` | — | A SPARQL SELECT whose rows give `?s` and `?text`, and optionally `?g` (§4.2). |
| `combine` | `false` | Embed a subject's selected texts in one graph as one input, joined by newlines, instead of one vector per literal. |
| `inputPrefix` | `""` | Text put before every stored input, for models trained with instructions such as `passage: `. |
| `queryPrefix` | `""` | Text put before every query text (§2.5). |
| `queryText` | `true` | Whether searches may pass text for this index. |
| `batchSize` | 64 | Inputs per request, 1 to 2048. |
| `maxInputChars` | 8000 | Inputs are cut at this many characters. |
| `requestsPerMinute` | 0 | A ceiling on requests per minute. 0 sets none. |
| `maxRetries` | 5 | Retries of a request that failed with a network error, a timeout, `429` or `5xx`. |
| `timeoutSecs` | 60 | The time one request may take, within the outbound policy's own timeout. |

Only literals whose datatype is `xsd:string` or `rdf:langString` are embedded. The
index's predicate may not be one of the source predicates. The `embedding` object can
change without a rebuild of the index, because the index's files depend only on the
fields F04 lists.

### 2.2 Writes and vectors

For a predicate source, each selected literal `(s, p, "text", g)` yields one input, and
the vector is written as `(s, index-predicate, vector, g)`, in the literal's graph.
With `combine`, all of a subject's selected texts in a graph yield one input and one
vector. Identical inputs of one subject and graph yield one vector.

The worker owns the index's predicate for every subject it reconciles. After a
reconciliation, the subject's vectors in that graph are exactly the vectors of its
current inputs. A vector written by hand under that predicate is replaced the next time
the subject's text changes, and is removed if the subject has no selected text.

Embedding commits have the commit kind `embed` and the message
`embeddings of vector index NAME`. They go through the write-time validation, the
storage quota and the disk reserve like any other commit.

### 2.3 API keys and secrets

`apiKey` names a source, never the key itself.

- `{"secret": "openai"}` names a secret the server's operator defined with
  `serve --embedding-secret openai=env:OPENAI_API_KEY` or
  `--embedding-secret openai=file:/run/secrets/openai`. This is the only form the HTTP
  API accepts.
- `{"env": "OPENAI_API_KEY"}` and `{"file": "/run/secrets/openai"}` read an environment
  variable or a file directly. They are accepted from the local command line and the
  library, where the caller is the operator. The HTTP API refuses them with `400`,
  because a dataset administrator could otherwise send any of the server's environment
  variables or readable files to an endpoint of their choice.

The key is read when a request is made, so a rotated file takes effect without a
restart. A secret that is missing or empty fails the requests and shows in the status.

### 2.4 HTTP

| Method | Path | Description |
|---|---|---|
| PUT | `/$/vector/{ds}/{name}` | As in F04. The body may carry `embedding`. |
| GET | `/$/vector/{ds}/{name}` | As in F04. The status gains `embedding` (below). |
| POST | `/$/vector/{ds}/{name}/reembed` | Embeds every selected text again, for example after the model behind a name changed. Answers `202` with the index status. |

```ts
type EmbeddingStatus = {
  state: "idle" | "scanning" | "embedding" | "backoff" | "paused" | "disabled";
  model: string; endpoint: string;           // the URL without credentials or query
  backlog: number;                           // subjects (per graph) waiting
  scan?: { done: number; total: number };    // a full pass in progress
  appliedSeq: number;                        // every commit up to this one is embedded
  headSeq: number;
  embedded: number; requests: number; failed: number;   // since the process started
  lastError?: { at: string; message: string; subject?: string };
  retryAt?: string;                          // while in backoff
  lastBatch?: { at: string; inputs: number; ms: number };
};
```

`state` is `paused` on a read-only server and on a store opened without a worker, and
`disabled` when the server runs with `--no-embedding`.

Errors:
- `400`: an invalid `embedding` object, an `apiKey` of the `env` or `file` form through
  the API, an unknown secret name, or a URL the outbound policy refuses outright.
- `403`: a mutating request to a read-only server.
- `404`: an unknown dataset or index.

Authorization follows F04. `reembed` needs `admin` on the dataset.

### 2.5 Searching with text

```sparql
PREFIX spk: <urn:x-sparkles:>  PREFIX ex: <http://example.org/>
SELECT ?s ?score WHERE {
  (?s ?score) spk:vectorSearch (ex:embedding "rivers of southern France" 10) .
}
```

When the query argument of `spk:vectorSearch` is a string literal, Sparkles embeds
`queryPrefix + text` with the index's provider and searches with the result. A variable
query bound to a string literal works the same way, and so does the vector list of
`spk:hybridSearch`. Query texts are cached per index (up to 1024 texts, least recently
used first out). The cases that fail:

| Condition | Status |
|---|---|
| The predicate has no configured index, or its index has no `embedding`. | `400` "… has no embedding provider; pass an spk:vector literal" |
| The index has `queryText: false`. | `400` |
| The provider fails, refuses, or returns a vector of another dimension. | `502` (as a failed SERVICE call) |
| The outbound policy refuses the endpoint. | `403` |
| The server runs with `--no-embedding`. | `400` |

### 2.6 Command line

```sh
sparkles vector create --loc db --name docs --predicate http://example.org/embedding --dim 768 \
    --embed-url http://127.0.0.1:11434/v1/embeddings --embed-model nomic-embed-text \
    --embed-from http://www.w3.org/2000/01/rdf-schema#label [--embed-api-key-env VAR] \
    [--embed-config FILE.json]
sparkles vector embed   --loc db [--name docs]     # embed the backlog now and exit
sparkles vector reembed --loc db --name docs       # embed everything again
sparkles serve --embedding-secret openai=env:OPENAI_API_KEY [--no-embedding]
```

`vector create`, `reembed` and `status` also work against a server with `--server`.
`--embed-config` reads the whole `embedding` object from a JSON file, and the other
`--embed-*` flags override its fields. A local command such as `sparkles update --loc`
does not embed. The next server start or `vector embed` catches up (§4.5).

### 2.7 UI

The vector index card on the dataset page shows the provider's model and endpoint, the
state, the backlog, the scan's progress, the applied commit against the head, the counts
and the last error. Administrators get a "Re-embed" action. The create and edit dialog
takes the embedding settings as JSON.

### 2.8 Library

```rust
let mut cfg = VectorIndexConfig::new("http://example.org/embedding", 768);
cfg.embedding = Some(EmbeddingConfig::new(url, model).from_predicates(&[RDFS_LABEL]));
store.create_vector_index("docs", cfg)?;
store.embed_until_idle(deadline)?;              // embed on this thread
let st = store.vector_index("docs").unwrap().embedding;
```

A server runs one worker thread per dataset with an embedding index. An embedder of the
library runs `vector::embed::run_worker` on a thread of its own, or calls
`embed_until_idle` after its writes. `vector::embed::set_environment` sets the outbound
policy and the named secrets the process uses.

## 3. Standards and sources

- **OpenAI embeddings API.** `POST /v1/embeddings` takes `{model, input, dimensions?,
  encoding_format?}`, where `input` is a string or an array of up to 2048 strings, and
  answers `{data: [{index, embedding}], model, usage}`. The `dimensions` parameter
  shortens the output of models that support it. Ollama, vLLM, LM Studio, llama.cpp's
  server and Hugging Face's Text Embeddings Inference serve the same path.
- **RFC 9110** §15.5.20 and §10.2.3 define `429 Too Many Requests` (with RFC 6585 §4)
  and `Retry-After`, as seconds or an HTTP date. RFC 6750 §2.1 defines the bearer token
  in `Authorization`.
- **RFC 4647** §3.3.1 defines basic filtering of language tags, the rule `languages`
  uses.
- **SPARQL 1.1 Query** §15 (projection) and §10.2.2 (VALUES) for the query source.
- **Other engines**, read as documentation for behaviour:
  - *pgai Vectorizer* (Timescale) keeps a queue of changed rows filled by triggers, embeds
    them asynchronously in a worker, writes the embeddings to a separate table, retries
    failures and reports a pending count. Sparkles follows the same queue-and-worker
    shape.
  - *Weaviate vectorizer modules* (`text2vec-openai`, `text2vec-ollama`) vectorize an
    object from its text properties at import, synchronously, and vectorize `nearText`
    queries with the same module. Sparkles adopts the query side and makes the import
    side asynchronous.
  - *Elasticsearch inference endpoints and `semantic_text`* embed at ingest through a
    configured endpoint and embed queries with the same endpoint.
  - *Neo4j GenAI* offers `genai.vector.encode` and `encodeBatch` procedures that the user
    calls in Cypher to write embedding properties, with provider credentials passed per
    call.
  - *Qdrant* stores client-supplied vectors and offers FastEmbed-based inference in its
    clients and cloud.
  - *pgvector*, *Oxigraph* and *Apache Jena* compute no embeddings.

## 4. Semantics

### 4.1 Inputs of a subject

For a predicate source, the inputs of `(s, g)` are computed from the snapshot:

1. Take each quad `(s, p, l, g)` with `p` in `predicates` and `l` a string or
   language-tagged literal.
2. Keep `l` if `languages` is unset, or if its tag (lowercase, `""` for none) matches one
   of the ranges by basic filtering.
3. If `classes` is set, keep the literals only when `(s, rdf:type, c, g)` exists for some
   listed `c`.
4. An input is `inputPrefix + text`, cut to `maxInputChars` characters. With `combine`,
   the texts are sorted by predicate order in the list and then by literal, and joined
   with `\n` before the prefix is added.

The desired vectors of `(s, g)` are the embeddings of its distinct inputs.

### 4.2 Query sources

A `query` source is a SELECT query run over the dataset with the store's default graph
settings. Each row with `?s` bound to an IRI or blank node and `?text` bound to a
literal gives an input of `(s, g)`, where `g` is the row's `?g` if the query binds it to
an IRI or blank node, and the default graph otherwise. Unbound or other rows are
skipped. Because a change to any quad can change the query's result, a commit that
touches any predicate the query mentions schedules a full pass (§4.5), and the passes
of one index start at most once a second.

### 4.3 What a commit schedules

When a commit publishes through the write-ahead log, the store looks at its changes. For
each index with an embedding, a quad schedules `(s, g)` when its predicate is a source
predicate, or when it is `rdf:type` with a listed class as object. Commits of kind
`embed` schedule nothing. The scheduled pairs are kept as vocabulary keys, so they
survive compactions, which renumber terms. A commit that schedules more than 10,000
pairs schedules a full pass instead, which is cheaper than keeping that many keys. A
bulk commit, which rebuilds the generation without log records, also schedules a full
pass.

### 4.4 Reconciliation

The worker takes up to `batchSize` inputs' worth of scheduled pairs. For each pair it
computes the inputs on the current snapshot and looks up the input hash it recorded
the last time it reconciled that pair (§4.5). A pair whose hash is unchanged and whose
vector count matches its distinct inputs needs nothing. The other inputs are sent to the
provider in one request. Then, in one write transaction of kind `embed`, the worker
computes each pair's inputs again on the transaction's view. A pair whose inputs changed
since the request is skipped, since the change scheduled it again. For the others it
deletes the pair's vectors that are not desired and inserts the desired ones that are
missing. Vectors are written in the canonical form of F04 §4.1, so an unchanged text
keeps a byte-identical literal and causes no change.

### 4.5 The record of reconciled pairs, restarts and full passes

Each index keeps a map from a 64-bit hash of `(g, s)` to a 64-bit hash of the pair's
sorted inputs, recorded after each successful reconciliation. A persistent store appends
the records to `<root>/embed/<name>.log` and rewrites the file when it holds more than
twice as many records as the map. The file's header holds the provider identity: the
model, the dimension, `sendDimensions` and `combine`. A different identity discards the
map, so every input is embedded again. A new URL or API key keeps the vectors.

A **full pass** lists every pair that has a selected literal or a vector under the
index's predicate, and schedules those whose input hash differs from the record or whose
vector count differs from their distinct inputs. A pass needs no provider calls for
pairs that are up to date, so it costs one read of the source text. It runs:
- when a worker starts, which catches up commits made while no worker ran, including
  the local command line and crashes before the worker caught up;
- after a bulk commit, and after a commit that schedules too many pairs;
- after `reembed`, which first clears the map, so every pair is scheduled;
- for query sources, after commits that may change the query's result (§4.2).

A crash may lose the tail of the record file. Those pairs are embedded again at the next
pass, which costs provider calls but changes nothing when the model is deterministic.

### 4.6 Consistency and what queries see

Embeddings are eventually consistent. A query sees the vectors committed in its
snapshot, like any other data. Until the worker reconciles a pair, a new subject has no
vector and is not found by vector search, and a subject whose text changed still has its
old vector. Deleted text keeps its vector until the worker removes it, so a search may
still return such a subject for a short while.

`appliedSeq` in the status is the newest commit whose scheduled pairs have all been
reconciled or have failed for good. A client that needs read-your-writes waits until
`appliedSeq` reaches the seq of its own commit (from its commit receipt). During a full
pass, `appliedSeq` stays below the commit that started it.

### 4.7 Failures, retries and rate limits

- A network error, a timeout, `429` or a `5xx` answer is retried up to `maxRetries`
  times, with exponential backoff from 1 s to 60 s with jitter, or after `Retry-After`
  when the provider sends it. After the last retry the worker keeps the batch, enters
  `backoff` for up to five minutes, and tries again. Writes go on meanwhile, and the
  backlog grows.
- `401` and `403` put the worker in `backoff` for five minutes with the provider's
  message, since retrying sooner cannot help.
- Another `4xx` on a batch of several inputs is retried with each half of the batch, so
  one bad input cannot hold up the others. An input that fails alone, a response whose
  vector has the wrong dimension or is not finite, and a response with the wrong number
  of vectors count as `failed`. The pair is not tried again until its inputs change or a
  full pass runs.
- A commit refused by write-time validation, the quota or the disk reserve fails its
  pairs the same way, and the status shows the reason.
- `requestsPerMinute` spaces requests evenly. Every request uses the outbound policy:
  the allowed destinations, the connect and total timeouts, redirects checked hop by hop
  and the response ceiling.

### 4.8 Limits

| Setting | Value |
|---|---|
| Indexes with an embedding per dataset | as for indexes (64) |
| Source predicates, classes, languages | 16 each |
| `batchSize` | 1 to 2048 |
| `maxInputChars` | 1 to 1,000,000 |
| Pairs scheduled by one commit before a full pass replaces them | 10,000 |
| Query-text cache | 1024 texts per index |

## 5. Design sketch

- `crates/sparkles/src/vector/embed/` holds the configuration and its validation
  (`config.rs`), the OpenAI-compatible client (`client.rs`), the per-store worker state
  with its queue, record and status (`worker.rs`), and the process-wide environment: the
  outbound policy, the named secrets and the on-off switch.
- The client posts JSON through a new `outbound::post_json`, which reuses the policy's
  checked resolver, redirect checks, timeouts and response ceiling.
- The store's commit path (`publish_log`, next to `maintain_text`) calls a hook that
  matches the log against the watched predicate ids of the head generation and pushes
  the keys of the scheduled pairs. Generation switches schedule a full pass.
- The worker is split into three steps so that no store reference is held during a
  provider call: *prepare* (pick a batch and compute inputs), *request* (no store), and
  *apply* (one write transaction). The server's worker thread holds only a weak
  reference to its dataset between steps.
- `CommitKind::Embed` is a new commit kind (code 11, name `embed`).
- The planner's `VectorQuery` gains a `Text` variant, resolved when the search runs,
  so explaining a query sends nothing.

## 6. Phasing

**Phase 1.** Configuration, the worker, the record file, full passes, `reembed`, status,
text queries, the HTTP and CLI surfaces, the UI card, and tests with a mock provider.

**Phase 2.** Chunking of long texts into several vectors with a chunk predicate,
metrics in `/$/metrics`, and a token-based rate limit.

## 7. Acceptance examples

The examples use a mock provider whose vector for a text is a fixed function of the
text's bytes, with an index `docs` on `ex:emb`, dimension 8, embedding `rdfs:label`.

1. **Insert.** `INSERT DATA { ex:a rdfs:label "alpha" }`, then waiting until
   `appliedSeq` reaches the insert's seq: `ex:a ex:emb ?v` holds the mock's vector of
   `"alpha"`, and the commit after the insert has kind `embed`.
2. **Change.** Replacing the label with `"beta"` replaces the vector. Adding a second
   label adds a second vector. Deleting a label removes its vector, and deleting all of
   them removes all.
3. **Filters.** With `languages: ["en"]`, `"chat"@fr` gets no vector and `"cat"@en`
   does. With `classes: [ex:Doc]`, a subject gets a vector once it has the type, and
   loses it when the type goes.
4. **Graphs.** A label in `GRAPH ex:g` gets its vector in `ex:g`.
5. **Outage.** With the provider answering `503`, writes succeed, the backlog grows and
   the state is `backoff`. When the provider answers again, the backlog drains.
6. **Restart.** Labels written while the store was open without a worker are embedded
   when a worker starts, and labels already embedded cause no request.
7. **Re-embed.** `reembed` sends every label again. A changed `model` does the same.
8. **Bad input.** One input the provider rejects with `400` fails alone, and the rest
   of its batch is written.
9. **Text query.** `(?s ?score) spk:vectorSearch (ex:emb "alpha" 1)` returns `ex:a`.
   An index without `embedding` answers `400`.
10. **Secrets.** `PUT` with `"apiKey": {"env": "HOME"}` answers `400`. With
    `--embedding-secret k=env:VAR`, the mock receives `Authorization: Bearer` and the
    variable's value.
11. **Outbound policy.** On a server without `--outbound-allow-private`, an index whose
    URL is `http://127.0.0.1:…` is refused at `PUT`.

## 8. Rejected alternatives

- **Embedding on the commit path.** It would make every write wait for a network call
  and fail writes during a provider outage.
- **Vectors outside the RDF data,** in a side table keyed by subject. F04 already makes
  the literal the source of truth with an exact overlay of each snapshot. A side store
  would need its own MVCC, backups and history.
- **Replaying the change feed** to find what changed since a watermark. The feed reaches
  back only as far as history is retained, so after a compaction the worker could lose
  track. The input-hash record and full passes do not depend on retention.
- **API keys in `vector.json`** or in the API's bodies. Keys would end up in backups,
  clones and logs.
- **A model runtime in the binary.** fastembed-rs (Apache-2.0) runs models through ONNX
  Runtime, a native library its default features download at build time, which the
  sandboxed Nix build cannot do, and it fetches models from Hugging Face at run time.
  Candle (MIT OR Apache-2.0) is pure Rust, but a runtime brings model files, tokenizers
  and CPU-heavy inference into the server process, next to its query threads. Ollama,
  llama.cpp's server and Text Embeddings Inference run the same models locally behind
  the OpenAI protocol, so the provider route covers local models without them. The
  binary-size cost of either crate was not measured.
- **One vector per subject always** (Weaviate's default). Labels in several languages
  and several labels per subject are common in RDF, so the default is one vector per
  literal, and `combine` gives the document-style alternative.

## 9. Open questions (defaults chosen)

1. Whether the worker should own the index's predicate (the default) or only the
   vectors it wrote. Owning it keeps the reconciliation exact without per-vector
   provenance.
2. Whether an embedding commit should mark materialized inferences stale. It does now,
   like any commit to a watched graph.
3. Whether query texts should count against a per-caller quota.

## 10. Sources

- The Sparkles code: `vector/`, `store/vector.rs`, `store.rs` (`publish_log`,
  `rebuild_locked`), `outbound.rs`, `commit.rs`, `sparql/plan.rs` and `exec.rs`, and the
  server's `vector.rs`, `outbound.rs`, `state.rs`, `compaction.rs` and backup credential
  sources.
- The F04, CI, C10 and C13 specs.
- OpenAI API reference, "Embeddings": the request and response of `POST /v1/embeddings`,
  input limits and the `dimensions` parameter.
- Ollama documentation, "OpenAI compatibility" (`/v1/embeddings`); vLLM's
  OpenAI-compatible server; LM Studio's OpenAI-compatible endpoints; Hugging Face Text
  Embeddings Inference.
- Timescale pgai documentation: Vectorizer overview, the vectorizer worker and its queue.
- Weaviate documentation: vectorizer modules `text2vec-openai` and `text2vec-ollama`,
  `nearText`.
- Elastic documentation: inference API, `semantic_text`, `query_vector_builder`.
- Neo4j documentation: GenAI integrations, `genai.vector.encode` and `encodeBatch`.
- Qdrant documentation: FastEmbed integration.
- crates.io pages of `fastembed` (7.1.0, Apache-2.0, default features `ort` binary
  download and `hf-hub`) and `candle-core`.
- RFC 9110, RFC 6585, RFC 6750, RFC 4647, SPARQL 1.1 Query.

## Outcome

**Delivered** on 2026-10-02, as Phase 1.

* **Configuration.** `VectorIndexConfig` has an optional `embedding` object with the
  fields of §2.1, validated with the index and stored in `vector.json`. The index's
  predicate cannot be a source, a query source cannot also set `classes` or `languages`,
  and changing the object keeps the index's build. The engine code is in
  `crates/sparkles/src/vector/embed/` (configuration, client, worker state and a mock
  endpoint) and `crates/sparkles/src/store/embed.rs` (scheduling, passes, the prepare and
  apply steps, status and text queries).
* **Scheduling.** `publish_log` calls a hook after it publishes a commit. The hook
  matches the commit's log against the source predicates and `rdf:type` with a listed
  class, and queues the (subject, graph) pairs as vocabulary keys, or schedules a full
  pass past 10,000 pairs. Commits of the new kind `embed` (code 11) schedule nothing. A
  bulk commit, a store's opening, a new embedding configuration and `reembed` schedule a
  full pass.
* **Record.** Each index keeps a map from a 64-bit FNV-1a hash of the pair's keys to a
  hash of its sorted inputs in `embed/<name>.log`. The file has a header with the provider
  identity and 16-byte records appended after each batch, and it is rewritten when it
  holds more than twice as many records as pairs. A pair needs work when its inputs hash
  differs from the record, or when its vector count differs from its distinct inputs.
* **Worker.** `Store::embed_prepare`, `Batch::run` and `Store::embed_apply` are the three
  steps, and `vector::embed::run_worker` loops over them with a closure that reaches the
  store. The server starts one worker thread per dataset with an embedding index, from a
  supervisor that looks every second and from the `PUT` and `reembed` handlers. The
  thread holds only a weak reference to its dataset between steps. Indexes of one
  dataset take turns, one batch at a time. The apply step skips the check of a batch's
  inputs when no commit came between the prepare step and the write.
* **Client.** Requests go through a new `outbound::post_json`, which uses the policy's
  checked resolver, redirect checks, timeouts and response ceiling. Retries follow
  §4.7. `Retry-After` is read as seconds. A batch rejected with another `4xx` is split
  in halves. The last 4096 inputs and query texts are cached per dataset, shared by the
  worker and the searches, and `reembed` clears the cache.
* **Server and CLI.** The server gained `serve --embedding-secret NAME=env:VAR|file:PATH`
  and `--no-embedding`, the route `POST /$/vector/{ds}/{name}/reembed`, and the
  `embedding` status in `GET /$/vector/{ds}/{name}`. `sparkles vector create` gained the
  `--embed-*` flags, and `vector embed` and `vector reembed` run the worker on a local
  database until nothing is left. A read-only server starts no worker.
* **Python.** The package gained `Dataset.embed(timeout=, allow_private=, secrets=)` and
  `Dataset.reembed_vector_index(name)`, and `create_vector_index` takes the `embedding`
  object in its options.
* **UI.** The index card shows the worker's state, model, endpoint, progress, counts and
  last error, and has a "Re-embed" action for administrators. The create and edit
  dialog takes the `embedding` object as JSON and keeps it when an index is edited. The
  mock server reports a worker that drains a backlog.

**Deviations.**
* A query source schedules a full pass after any commit that is not an `embed` commit,
  not only after commits that touch a predicate the query names (§4.2). The passes are
  still at least a second apart.
* The status also returns the `embedding` object as `config`, so that the UI can edit
  it. URLs with credentials are refused, so the object never holds a secret.
* The status is `paused` whenever work waits and no worker runs, which includes the
  pass every store owes when it is opened by a local command.
* The cache holds 4096 entries per dataset, not 1024 query texts per index, and it
  holds stored inputs as well.
* A batch takes up to `batchSize` pairs. Their inputs go out in requests of at most
  `batchSize` inputs each.
* `vector embed` and the local `vector reembed` take `--embed-timeout` and the local
  `--outbound-*` flags.
* Backups and clones do not hold the record, because it could be newer than the
  backup's commit. A restored or cloned dataset embeds its text again when its worker
  first runs.

**Decisions on the open questions (§9).** The worker owns the index's predicate.
Embedding commits make materialized inferences stale like any commit to a watched graph.
Query texts have no quota of their own beyond the rate limits.

**Tests at landing.**
* `crates/sparkles/tests/embeddings.rs`: inserts, changes and deletions, languages,
  classes, graphs and combined inputs, query sources, outages and backoff, rejected
  inputs, restarts with the record, `reembed` and a new model, bulk loads, text
  searches, secrets, the outbound policy, `--no-embedding`, configuration errors, and
  text that changes while its request is out. They run against the mock endpoint in
  `vector::embed::mock`.
* Unit tests of the configuration, the language ranges and the response parsing in
  `vector/embed/`.
* The server's router tests (`http/router_tests/embeddings.rs`) cover the API's key and
  policy checks, the worker the server starts, text searches and `reembed`.
  `crates/sparkles-server/tests/cli_vector.rs` covers the CLI. The Python suite runs a
  small embeddings server in Python. The UI has Vitest tests for the dialog's JSON and
  the status text.

**Performance.** Not measured against a real provider. The commit hook adds a few
lookups per commit and one hash check per changed quad, and a full pass reads every
selected literal once.

**Metrics** (Phase 2, 2026-10-02). `/$/metrics` has the `sparkles_embedding_*` series
per dataset label and index. Three counters give requests (retries included), inputs sent
and vectors written. `sparkles_embedding_failures_total` counts failures by kind: failed
batches (`transient`, `auth`, `refused`, `fatal`, `write`), inputs the provider refused
(`rejected`), and subjects whose text could not be read (`read`). Two gauges give the
backlog and the lag in commits behind the head. The datasets past
`--metrics-max-datasets` share `$other`, as in the other series. Their counters and
backlogs add up per index name, and the largest lag stands for them. The counters start
at zero when the dataset is opened, like those of the status. The store's
`embedding_metrics` returns them, and the library and router tests check them after an
outage, a rejected input and a caught-up worker.

**Not built.** Chunking and a token-based rate limit, the rest of Phase 2, were not
built. The NixOS module has no vector settings, so it gained no embedding options, and
`--embedding-secret` goes through its `extraArgs`. The Similar page and the MCP tool
`similar_entities` do not search with text.
