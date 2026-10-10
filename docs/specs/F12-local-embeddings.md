# F12: Local embedding models

> **Status:** implemented
>
> **Phases:** Phase 1 shipped: the model store crate, the embedding runtime crate with
> four architectures, tests against the published outputs of real models and a
> benchmark. Phase 2 shipped: `sparkles models`, the server's model store flags, the
> `local` provider kind, vector indexes that name a provider of the model configuration,
> the model state in `GET /$/models` and on the index card, and the NixOS helper and
> options. The build feature `embed-local` is off in cargo's default features and on in
> the flake's packages.
>
> **User docs:** [API: Embeddings on write](../API.md#embeddings-on-write) · [API: Model providers](../API.md#model-providers) · [Usage: Local embedding models](../USAGE.md#local-embedding-models) · [Usage: NixOS](../USAGE.md#local-embedding-models-on-nixos)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

The design builds on the embeddings of [F08](F08-embeddings-on-write.md), the model
configuration and secrets of [C18](C18-natural-language-questions-and-ingest.md) and
[C19](C19-layered-settings.md), and the outbound policy of SERVICE and `LOAD`.

## 1. Summary, goals, non-goals

F08 computes a vector index's vectors through an endpoint that speaks OpenAI's
embeddings protocol. A local model then needs a second service next to Sparkles, such
as Ollama or Text Embeddings Inference. This spec adds an embedding runtime inside the
server, behind a build feature, and a store of model files that the server, the CLI and
a Nix build share. A vector index can then name a provider of the model configuration
instead of a URL, and that provider can be a local model, an OpenAI-compatible endpoint
or Ollama.

**Goals**
- A small sentence-embedding model runs in the server process with no other service.
  The common open models of 2024 and 2025 are covered: BERT-style models such as
  all-MiniLM-L6-v2 and the BGE family, XLM-RoBERTa models such as multilingual-e5,
  nomic-embed-text, and Qwen3-Embedding.
- Model files are pinned and verified. A snapshot is named by repository and full
  commit id, each file is checked against a digest, and a snapshot directory is either
  complete or absent.
- The runtime does not compete with queries. It runs on its own small thread pool at a
  lower scheduling priority, with a bounded queue, and drops its weights when idle.
- The server can run with a read-only model store, such as a Nix store path, and makes
  no network request unless the operator turns downloads on.
- The default build does not change. The runtime is a cargo feature, and a build
  without it still reads the configuration and reports what is missing.
- The store is not specific to embeddings. The OCR models of PDF ingestion use it later.

**Non-goals**
- GPUs and accelerator back ends. Candle's CUDA and Metal features stay off.
- Generative models in the server. The model roles of C18 keep using providers.
- ONNX, GGUF and PyTorch pickle weights. Only safetensors are read.
- Training, fine-tuning or quantization in Sparkles.
- Rerankers and cross-encoders.

## 2. User-visible behavior

### 2.1 The model store and `sparkles models`

The store is a directory with one subdirectory per snapshot,
`DIR/<owner>/<name>/<revision>/`. Each snapshot holds its files and a
`sparkles-manifest.json` with the repository, the revision and each file's path, size
and SHA-256.

```text
sparkles models pull REPO[@REVISION]... [--manifest FILE] [--dir DIR | --data DIR]
                     [--allow-unpinned] [--hub-endpoint URL] [--hub-token-file FILE] [--json]
sparkles models list   [--dir DIR | --data DIR] [--json]
sparkles models verify [--dir DIR | --data DIR] [--json]
sparkles models rm REPO@REVISION [--dir DIR | --data DIR]
```

- The store is `--dir`, or `DIR/models` for `--data DIR`, or `$SPARKLES_MODELS_DIR`,
  or `./data/models`, which is the store of a server started with the default `--data`.
- `pull` takes references and a manifest file of the form
  `{"models": [{"repo": ..., "revision": ..., "files": [...]}]}`. `files` is optional and
  names a subset of the repository's files. Without it, the files a sentence-transformers
  model needs are pulled (§4.2).
- A revision must be a full 40-character commit id. A branch, a tag or a missing
  revision is refused unless `--allow-unpinned` is given, in which case the commit the
  Hub names for it is recorded as the snapshot's revision.
- A pinned snapshot already in the store is not downloaded again, and `pull` makes no
  network request for it.
- `pull` exits non-zero when any reference fails, after trying all of them.
- `verify` reads every file of every snapshot again and compares its size and SHA-256
  with the manifest. It exits non-zero when any snapshot fails.
- Downloads go through the outbound policy. The local CLI allows private addresses, as
  `sparkles query` does, so a Hub mirror on the local network works.

### 2.2 Server flags

| Flag | Environment | Default | Meaning |
|---|---|---|---|
| `--models-dir DIR` | `SPARKLES_MODELS_DIR` | `<data>/models` | The model store. It may be read-only. |
| `--models-download off\|on` | `SPARKLES_MODELS_DOWNLOAD` | `off` | Whether the server may download a pinned snapshot that the store lacks. |

With downloads off, a local model whose snapshot is missing fails with a message that
gives the `sparkles models pull` command. With downloads on, the first request for the
model starts a background download and fails as a transient error, so the F08 worker
waits and tries again. The server never downloads an unpinned revision. The server and
the CLI read a mirror's address from `SPARKLES_HUB_ENDPOINT` and a token from
`HF_TOKEN`.

### 2.3 The `local` provider kind

The model configuration of C18 gains the kind `local`. A local provider has no endpoint
and no key. Each of its models names a snapshot in the store by `repo` and `revision`,
or a directory by an absolute `path`.

```json
{
  "providers": {
    "embed": {
      "kind": "local",
      "threads": 2,
      "models": {
        "minilm": {
          "repo": "sentence-transformers/all-MiniLM-L6-v2",
          "revision": "1110a243fdf4706b3f48f1d95db1a4f5529b4d41"
        },
        "qwen3": {
          "repo": "Qwen/Qwen3-Embedding-0.6B",
          "revision": "97b0c614be4d77ee51c0cef4e5f07c00f9eb65b3",
          "dtype": "bf16",
          "dimensions": 512
        }
      }
    }
  }
}
```

| Member | Default | Meaning |
|---|---|---|
| `repo`, `revision` | — | A snapshot of the store. The revision must be a full commit id. |
| `path` | — | An absolute directory with the files, instead of `repo` and `revision`. |
| `dtype` | `f32` | `f32` or `bf16`, the type the weights are kept at in memory. |
| `threads` | 2 | Threads of the model's inference pool, 1 to 256. |
| `idleUnloadSecs` | 600 | Seconds without work before the weights are dropped. 0 keeps them. |
| `dimensions` | the model's | Truncate vectors to this many components (Matryoshka models). |
| `maxTokens` | the model's | Tokens per text, at most the model's own limit. |
| `queryPrefix`, `documentPrefix` | the snapshot's prompts | Text put before queries and before stored texts. |

The provider's own members are the defaults of its models. A role of C18 cannot name a
local provider, because a local model answers no prompts. These members are refused on
the other kinds.

### 2.4 Vector indexes that name a provider

F08's `embedding` object gains `provider`. With it, `url` and `apiKey` are absent, and
`model` names a model of that provider.

```json
"embedding": { "provider": "embed", "model": "minilm",
               "predicates": ["http://www.w3.org/2000/01/rdf-schema#label"] }
```

- A `local` provider embeds in the process. Stored texts are documents and search texts
  are queries, so each gets its prompt.
- An `openai` provider is called at `{endpoint}/embeddings` and an `ollama` provider at
  `{endpoint}/v1/embeddings`, with the provider's key from the model secrets. This
  lets an index reuse a provider's endpoint and key instead of repeating them.
- An `anthropic` provider is refused, since Anthropic's API has no embeddings.
- The configuration is resolved at each batch, so a change of the model configuration
  applies without editing the index. The index's identity, which decides when
  everything is embedded again, is the provider, the model, the dimension and
  `combine`. A change of a local model's snapshot under the same name does not embed
  again by itself, and `reembed` does that.
- The old form with `url`, `model` and `apiKey` keeps working unchanged.

### 2.5 Status

`GET /$/models` gains `local`, a list with one entry per model of a local provider, and
`modelsDir`. Each entry has the provider, the model, the source, the dtype, the threads,
whether the build has the runtime, and a `state`.

| State | Meaning |
|---|---|
| `absent` | The snapshot is not in the store. |
| `downloading` | The server is downloading it. |
| `present` | The snapshot is there and no index has used the model yet. |
| `loading` | The weights are being read. |
| `loaded` | The weights are in memory. `weightBytes` gives their size. |
| `unloaded` | The weights were dropped after the idle time, and the next text loads them. |

`GET /$/vector/{ds}/{name}` adds the same entry as `embedding.local` for an index whose
provider is local, and the UI's index card shows the state with a tooltip that gives the
source, the memory and any error.

### 2.6 NixOS

The flake exposes `legacyPackages.<system>.modelSnapshots` and the overlay's
`sparkles-model-snapshots`. Given manifests of the form `sparkles models pull` writes,
it builds a store in the Nix store with one fixed-output `fetchurl` per file, checked
against the manifest's SHA-256. `services.sparkles.models.dir` and
`services.sparkles.models.download` map to the two flags, and a store outside the Nix
store and the data directory is added to the service's read-only or writable paths.

## 3. Standards and sources

- Candle's documentation and source (`candle-core`, `candle-nn`,
  `candle-transformers`): tensors on the CPU, safetensors loading through memory maps,
  the BERT, XLM-RoBERTa and Qwen3 models, and the rayon-based CPU kernels.
- The sentence-transformers documentation of a saved model: `modules.json` lists the
  modules in order (`Transformer`, `Pooling`, `Normalize`), `1_Pooling/config.json`
  gives the pooling mode (`pooling_mode_cls_token`, `pooling_mode_mean_tokens`,
  `pooling_mode_max_tokens`, `pooling_mode_mean_sqrt_len_tokens`,
  `pooling_mode_lasttoken`), `sentence_bert_config.json` gives `max_seq_length` and
  `do_lower_case`, and `config_sentence_transformers.json` gives named `prompts`.
- The Hugging Face Hub API: `GET /api/models/{repo}/revision/{revision}?blobs=true`
  lists a revision's commit (`sha`) and its files (`siblings`) with `rfilename`, `size`,
  the Git `blobId` and, for files in Git LFS, `lfs.sha256` and `lfs.size`.
  `GET /{repo}/resolve/{revision}/{path}` serves a file, usually through a redirect.
- The Hugging Face tokenizers documentation of `tokenizer.json` and the crate's
  features (`onig`, `esaxx_fast`, `http`, `progressbar`, `fancy-regex`).
- The safetensors format: an 8-byte little-endian header length, a JSON header with
  each tensor's dtype, shape and byte offsets, then the data, which can be memory-mapped.
- The Qwen3-Embedding model card: last-token pooling, left padding, the end-of-text
  token appended to each input, the instruction format for queries, Matryoshka
  dimensions from 32 to the model's size, and the similarity matrix of its usage
  example, which the tests reproduce.
- The nomic-embed-text-v1.5 model card: the task prefixes `search_query: ` and
  `search_document: `, and Matryoshka dimensions.
- Git's object format, for the blob id of files outside LFS: the SHA-1 of
  `blob <size>\0` followed by the content.
- Linux `setpriority(2)` with `PRIO_PROCESS` and a thread id, which sets the nice value
  of one thread.

## 4. Semantics

### 4.1 Snapshots and manifests

A snapshot is identified by `(repo, revision)`. `repo` is `owner/name`, each part made
of ASCII letters, digits, `.`, `_` and `-`, not starting with `.`. `revision` is a
lowercase 40-character hex commit id. Paths inside a snapshot are relative, use `/`,
and may not contain `..`, empty segments or a leading `.`.

The manifest is written last. A directory without a manifest is not a snapshot, and
`list` skips it. The manifest always records SHA-256, whatever digest the source gave,
so `verify` and the Nix helper need only one hash.

### 4.2 Downloads and verification

1. The plan lists the revision's files and resolves the revision to its commit. A
   pinned revision whose listing names another commit is refused.
2. The selection keeps the files a model needs to run from safetensors: the JSON files
   at the root, `model.safetensors` or its shards `model-*.safetensors`,
   `tokenizer.model`, and `config.json` in each numbered module directory such as
   `1_Pooling`. ONNX, OpenVINO, PyTorch pickles and other formats are skipped. A
   manifest's `files` replaces this selection.
3. Each file has an expected digest. LFS files have the SHA-256 the listing gives.
   Other files have their Git blob id, a SHA-1, which the download checks. The SHA-256
   of every file is computed while it is written.
4. A snapshot is downloaded under a lock file `.REVISION.lock` next to its directory,
   into a staging directory `.REVISION.partial`. A file is written to `NAME.part`. A
   file that ends early is kept, and the next attempt resumes it with an HTTP range
   request. A file that is too long or whose digest differs is deleted, and the
   download fails.
5. When every file is in, the manifest is written and the staging directory is renamed
   to the snapshot's name, so the snapshot appears complete or not at all. A second
   process that waited for the lock finds the snapshot and does nothing.

### 4.3 Reading a snapshot

The runtime reads the snapshot's configuration when the model is created, before any
weight is loaded, so a wrong snapshot fails at once.

- `config.json` gives `model_type`, which picks the architecture (§4.4).
- `modules.json` must list a `Transformer` module first, then optionally `Pooling` and
  `Normalize`. Other modules, such as `Dense`, are refused. A snapshot without
  `modules.json` needs its pooling set by the caller, and its vectors are normalized.
- The pooling mode comes from the `Pooling` module's configuration. `Normalize` turns on
  L2 normalization. A pooling that leaves out the prompt's tokens
  (`include_prompt: false`) is refused together with a prompt, since the runtime does
  not track where the prompt ends.
- The token limit is `max_seq_length` of `sentence_bert_config.json`, or the model's
  position limit, lowered by `maxTokens`.
- The query prompt is the `query` or `search_query` prompt of
  `config_sentence_transformers.json`, and the document prompt is `document`,
  `passage` or `search_document`. `queryPrefix` and `documentPrefix` replace them.
- `tokenizer_config.json` gives the padding side. Left padding with last-token pooling
  appends the end-of-sequence token when the tokenizer does not.
- Weights are `model.safetensors` or the shards its index lists. A snapshot with only
  PyTorch weights is refused.

### 4.4 Architectures and types

| `model_type` | Encoder | `f32` | `bf16` |
|---|---|---|---|
| `bert` | Candle's BERT | yes | no |
| `xlm-roberta` | Candle's XLM-RoBERTa | yes | no |
| `nomic_bert` | this crate's | yes | yes |
| `qwen3` | this crate's | yes | yes |

Candle's CPU matrix product has no bf16 kernel. A bf16 model keeps its linear weights at
bf16 in memory and widens each to f32 for its product, and norms and attention run in
f32. That halves the resident weights at a cost per product. BERT and XLM-RoBERTa build
their attention mask in a way that overflows in bf16, so they run in f32 only. They are
small, so the saving would be small too.

Qwen3 is a decoder. Candle's model keeps a key-value cache across calls and has no
padding mask, so this spec uses its own stateless encoder with a causal mask that also
hides padding. A padding position attends to itself only, so no row of the softmax is
empty.

Matryoshka truncation keeps the first `dimensions` components and normalizes again when
the model normalizes.

### 4.5 Execution

Each model has one dispatcher thread and a rayon pool of `threads` threads. The pool's
threads lower their own priority by 10 nice steps on Linux, so a busy model yields to
query threads. Candle's kernels run inside the pool and use its threads only.

- Requests are queued per kind. Queries go before documents. A document request is cut
  into micro-batches of 16 texts, longest first so that a batch pads little, and a
  query waits at most for the micro-batch in progress.
- At most 4096 texts may wait per kind. More fail with a busy error, which the F08
  worker treats as transient.
- A batch's texts are padded to the longest of them.

### 4.6 Loading, unloading and memory

The weights are loaded at the first text, not at startup, from memory-mapped
safetensors. After `idleUnloadSecs` without work the dispatcher drops them, and the next
text loads them again. The resident memory of a loaded model is about the size of its
weights at the chosen type, plus activations that grow with the batch and the text
length.

### 4.7 Failures

- A snapshot that is missing, incomplete or of an unsupported kind fails each request
  with a message, and the F08 worker records it as the batch's error and waits.
- A model that fails to load reports `lastError` in its status and is tried again at the
  next text.
- A vector of the wrong dimension or with a non-finite component fails its batch.
- A build without the feature accepts the configuration, shows the models with
  `runtime: false`, and fails each request with a message that names the feature.

## 5. The build feature

`embed-local` on `sparkles-server` adds `sparkles-embed` and its dependencies. The
choices that keep it small are these.

- Candle without accelerator features, so the build needs no CUDA, Metal or BLAS.
- Hugging Face tokenizers without default features and with `fancy-regex`. That leaves
  out Oniguruma, which is C, the C++ suffix array of `esaxx_fast`, the HTTP client and
  the progress bars.
- No download code in the runtime. The store crate takes an HTTP client from its caller,
  so the server's outbound policy applies.

The store crate `sparkles-modelstore` is always built, since `sparkles models` and the
status in `GET /$/models` use it. It depends on serde, sha1, sha2 and percent-encoding,
which the server already has.

## 6. Design sketch

- `crates/sparkles-modelstore`: `ModelStore` with `get`, `list`, `verify`, `remove` and
  `fetch`, `HubSource::plan`, the `HttpClient` trait, `LocalSnapshot::open` for an
  operator's directory, and the default file selection.
- `crates/sparkles-embed`: `Embedder::new(ModelSpec, Options)` reads the snapshot's
  configuration. `embed(&[&str], Kind)` queues the texts and waits. `status()` and
  `info()` report state and settings, and `unload()` drops the weights.
- `sparkles-core`: the `provider` member of the embedding configuration, and a
  `Providers` trait in the embedding environment that resolves a provider and model to
  a remote URL and key or to a local model.
- `sparkles-server`: `sparkles models`, the store flags, the `local` kind in the model
  configuration, `ServerProviders`, which implements `Providers` from the server's
  state, and `LocalModels`, which keeps one `Embedder` per provider and model and
  replaces it when the model's settings change.

## 7. OCR models

The OCR of PDF ingestion (C18 Phase 4, the `pdf-ocr` feature) needs model files too. It
will use the same store, the same `sparkles models pull` with a manifest that lists the
files, the same `--models-dir` and `--models-download`, and the same Nix helper. The
store crate has no knowledge of embeddings for that reason. This spec makes no change
to OCR.

## 8. Phasing

1. The store crate, the runtime crate with BERT, XLM-RoBERTa, NomicBERT and Qwen3,
   tests with tiny random models and ignored tests with real models, and a benchmark.
2. `sparkles models`, the server flags, the `local` kind, `provider` in vector indexes,
   the status, the UI, the NixOS helper and options, and the decision on the default
   build.

## 9. Acceptance examples

- **A1.** `sparkles models pull sentence-transformers/all-MiniLM-L6-v2@<commit>` writes
  the snapshot with its manifest. A second run prints `present` and makes no request.
- **A2.** `pull org/model` and `pull org/model@main` fail without `--allow-unpinned`,
  and with it the snapshot is stored under the commit the Hub names.
- **A3.** After a byte of a snapshot's file changes, `verify` names the file and exits
  non-zero.
- **A4.** A file whose listed SHA-256 differs from its content is not stored, and the
  snapshot does not appear.
- **A5.** An interrupted download resumes with a range request.
- **A6.** Qwen3-Embedding-0.6B in f32 and in bf16 reproduces the similarity matrix of
  its model card to two decimals.
- **A7.** A left-padded batch gives the same vectors as the texts embedded one by one.
- **A8.** A query that arrives while 64 documents wait is answered before them.
- **A9.** After the idle time the weights are dropped, and the next text loads them.
- **A10.** A vector index with `"provider": "embed", "model": "minilm"` writes vectors
  without any URL in its configuration, and a provider of the `openai` kind sends the
  provider's key.
- **A11.** A server built without `embed-local` lists the local models with
  `runtime: false`, and the index's error names the feature.

## 10. Rejected alternatives

- **ONNX Runtime through `ort` or fastembed.** It is a native library that the default
  features download at build time, which the sandboxed Nix build cannot do.
- **The `hf-hub` crate for downloads.** It brings its own HTTP client and cache layout,
  bypasses the outbound policy and accepts unpinned revisions.
- **Loading at startup.** Most servers would hold weights for indexes that rarely embed.
  Lazy loading with an idle unload costs one load time after a quiet period.
- **Rayon's global pool.** The engine's own parallel work shares it, so a long batch
  would delay queries.
- **Candle's Qwen3 model.** Its key-value cache and missing padding mask give wrong
  vectors for a padded batch.
- **A process per model.** Isolation would be stronger, but it brings a protocol and a
  supervisor for what a thread pool at a lower priority already achieves.
- **The runtime in cargo's default features.** Every debug build and test run of the
  server would compile Candle. The Outcome records which packages turn it on.

## 11. Open questions (defaults chosen)

1. Whether a local model should be shared between servers on one machine. It is not.
   Each process loads its own copy.
2. Whether `reembed` should run when a local model's snapshot changes under the same
   name. It does not, and the operator runs it.
3. Whether the server should verify a snapshot at load. It does not, and `sparkles
   models verify` does that on demand.

## 12. Sources

- The Sparkles code: `vector/embed/` and `store/embed.rs` in the engine, `outbound.rs`,
  and the server's `models/`, `vector.rs` and `main.rs`.
- The F08, C18 and C19 specs.
- Candle 0.9 and 0.11 documentation and source on docs.rs and crates.io.
- sentence-transformers documentation: saving and loading models, `models.Pooling`,
  `models.Normalize`, prompts.
- Hugging Face Hub documentation: the Hub API endpoints for model info and revisions,
  and file downloads with `resolve`.
- Hugging Face tokenizers crate documentation and its Cargo features.
- safetensors format documentation.
- The model cards of all-MiniLM-L6-v2, BGE-small-en-v1.5, multilingual-e5,
  nomic-embed-text-v1.5 and Qwen3-Embedding-0.6B.
- Git documentation of object hashing.
- The Linux man page `setpriority(2)`.

## Outcome

**Delivered** on 2026-10-10, as Phases 1 and 2.

* **Crates.** `crates/sparkles-modelstore` holds the store, the Hub source, the digests
  and the default file selection. `crates/sparkles-embed` holds the snapshot reader, the
  pooling, the BERT and XLM-RoBERTa wrappers, its own NomicBERT and Qwen3 encoders and
  the dispatcher. The server's `model_store.rs` holds the flags, `LocalModels` and
  `ServerProviders`, and `models_cmd.rs` holds `sparkles models`. The engine's
  embedding configuration gained `provider` and the `Providers` trait.
* **Surfaces.** `sparkles models pull|list|verify|rm`, `serve --models-dir` and
  `--models-download` with their environment variables, the `local` kind with the
  members of §2.3, `provider` in a vector index's `embedding`, `local` and `modelsDir`
  in `GET /$/models`, `embedding.local` in the index status, a state badge with a
  tooltip on the UI's index card, the OpenAPI description of all of them, the flake's
  `modelSnapshots` helper and the module's `models.dir` and `models.download`.

**Departures from the design.**

* **Candle 0.9.2 instead of 0.11.** Candle 0.10 and 0.11 depend on tokenizers 0.22 with
  the `onig` feature for their GGUF tokenizer, which would have brought Oniguruma, a C
  library, and a second tokenizers version into the build. 0.9.2 has no such
  dependency. It lacks `nomic_bert`, so the NomicBERT encoder is this crate's own,
  checked against the vectors candle-transformers 0.11 produced for the same snapshot
  before the switch: the first components agree to the printed precision and the
  cosine to four decimals. The own encoder also runs in bf16, which 0.11's did not
  allow.
* **Linear layers.** The first version multiplied a batch by a broadcast weight, which
  made Candle copy the weight for every product. The layers now keep their weight
  transposed and fold the batch into one 2-D product. With the same allocator, that cut
  the query latency of nomic-embed-text from 335 ms to 36 ms. Qwen3-Embedding-0.6B had
  taken 3.4 s per query, measured under heavier load, and now takes 175 ms. The cost is
  one transposition per weight at load, with no change in resident memory.
* **Memory after an unload.** mimalloc keeps freed pages in per-thread heaps, so
  dropping the weights left the memory resident. `Options::release` is a hook that
  runs on the dispatcher and on each pool thread after an unload, and the server sets
  it to `mi_collect` (or `malloc_trim` without mimalloc). An unloaded nomic-embed-text
  went from 714 MB resident back to 17 MB above the process's baseline.
* **Hub settings in the server.** A server download reads `SPARKLES_HUB_ENDPOINT` and
  `HF_TOKEN` as `sparkles models pull` does.
* **Licenses.** `scripts/third-party-licenses.py` resolves sparkles-server with
  `embed-local` as well, so THIRD_PARTY_LICENSES.md lists the 49 crates of the feature
  and a build with it ships their notices.

**The default build.** The maintainer asked for a decision on the cargo default, the
Nix package and the container image.

* Cargo's default features leave `embed-local` out. Every `cargo test` and clippy run
  of the server would otherwise compile Candle in debug mode, and the library crates
  and the feature matrix stay as they were.
* The flake's `sparkles` and `sparkles-cli` packages include it (`nix/package.nix`
  takes `features`, `[ "embed-local" ]` by default). The cost is about 5 MB of binary,
  nothing runs until a local provider is configured, and a NixOS deployment would
  otherwise rebuild the whole package from source to turn it on.
* The container image should include it for the same reason, so that a Helm
  deployment with a model store can use it. The Dockerfile belongs to another change.
  The recommended form is an `ARG FEATURES=embed-local` passed as
  `cargo build --release --locked -p sparkles-server --features "${FEATURES}"`, so an
  image without the runtime stays one build argument away.

**Measurements.** All on one machine, an Intel Core Ultra X7 358H with 16 cores and
30 GB of memory, while other work kept the load average at 5 to 12, so the absolute
numbers are approximate.

| Build | Without `embed-local` | With it |
|---|---|---|
| Release binary, stripped | 99.6 MB | 105.0 MB (+5.4 MB, +5.4%) |
| Release binary with line tables | 634 MB | 675 MB |

The 49 added crates took 99 s of compile time summed over their units, and 47 s of
wall time when built alone. Candle's two main crates account for 41 s of that. A
release build of the server with the feature after a default release build took
480 s, almost all of it compiling and linking sparkles-server itself, which any change
to the server repeats.

The runtime was measured with `embed-bench` (mimalloc, the pool at normal priority).
The query is one short sentence with the model's prompt. The documents are 256
paragraphs of about 60 words, embedded in one call.

| Model | dtype | Threads | Load | Weights | Resident after load | Query p50 / p95 | Documents | Resident after unload |
|---|---|---|---|---|---|---|---|---|
| all-MiniLM-L6-v2 (384) | f32 | 2 | 28 ms | 91 MB | +117 MB | 5.7 / 7.5 ms | 47 per s | +13 MB |
| all-MiniLM-L6-v2 | f32 | 4 | 28 ms | 91 MB | +124 MB | 4.0 / 4.6 ms | 60 per s | +19 MB |
| nomic-embed-text-v1.5 (768) | f32 | 2 | 341 ms | 547 MB | +652 MB | 28 / 37 ms | 12 per s | +17 MB |
| nomic-embed-text-v1.5 | bf16 | 2 | 324 ms | 273 MB | +327 MB | 50 / 52 ms | 12 per s | +16 MB |
| Qwen3-Embedding-0.6B (1024) | f32 | 4 | 4.4 s | 2383 MB | +2573 MB | 175 / 186 ms | 3.6 per s | +52 MB |
| Qwen3-Embedding-0.6B | bf16 | 4 | 1.4 s | 1192 MB | +1372 MB | 261 / 533 ms | 3.5 per s | +33 MB |

bf16 halves the weights and costs a widening of each weight per product, which shows in
the query latency of the larger models and hardly in the batch throughput. The
downloader fetched all-MiniLM-L6-v2 (91 MB) in 3.6 s, nomic-embed-text-v1.5 (547 MB)
in 10 s and Qwen3-Embedding-0.6B (1.2 GB) in 65 s.

**Tests.** The store's tests run a mock Hub on local ports: verification, a wrong hash,
a declared digest that disagrees, a resumed download, unpinned revisions, a commit
that differs from the pinned one, and an operator's directory. The runtime's tests
build tiny BERT and Qwen3 models with random weights and compare them with Candle's
own models, check padding invariance, bf16 against f32, truncation, the idle unload,
the queue limit, query priority and the refusals. Ignored tests run the real
snapshots: Qwen3-Embedding-0.6B reproduces the model card's similarity matrix
(0.7646, 0.1414, 0.1355, 0.6000) in f32 and in bf16, nomic-embed-text-v1.5 matches
the 0.11 reference, and all-MiniLM-L6-v2 separates a paraphrase (0.72) from an
unrelated sentence (−0.05). The server's tests cover the CLI against a mock Hub,
the configuration rules of the `local` kind, the resolution of providers, the model
state, and an in-process local model that embeds, truncates and is replaced when its
settings change. The engine's tests cover an index whose provider resolves to a remote
endpoint with its key and to a local model that embeds queries apart from documents,
the rules of `provider` against `url` and `apiKey`, and the error of an environment
without providers.

**Deferred.**

* The Models section of the UI's settings does not show the local models' state. The
  index card and `GET /$/models` do.
* `Dense` modules, rerankers, ONNX and GGUF weights, and GPUs.
* Verifying a snapshot at load, and sharing one loaded model between processes.
* The container image's build argument, which belongs to the change that owns the
  Dockerfile.
