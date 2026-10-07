# X01: Compression codecs (zstd, brotli, LZ4, gzip)

> **Status:** implemented in part (Phase 1)
>
> **Phases:** Phase 1 shipped. It covers the codec module, compressed inputs and request
> bodies, codecs for dumps and backups, configurable response compression, precompressed
> UI assets and a zstd full-text doc store. The maintainer decided not to build Phase 2
> (zstd index blocks, vocabulary and spill files). Phase 3's blobs shipped with
> [F05](F05-snapshot-repositories.md), compressed with LZ4 rather than zstd.
>
> **User docs:** [API: Compression](../API.md#compression) · [Features](../FEATURES.md#storage-tdb2-equivalent) · [Benchmarks: Compression](../BENCHMARKS.md#compression-105m-triples)
>
> This is the design as written before implementation; the [Outcome](#outcome) section at
> the end records how it landed.

This design is Sparkles' own engineering and does not derive from any other database
product. It draws on the codec formats' specifications and the Rust crates' docs, listed
in §10.

## 1. Summary

Sparkles uses **LZ4** for index blocks and **gzip** for backups and `.gz` inputs. It
negotiates response compression through tower-http. This spec adds **zstd** and
**brotli** wherever compression helps, behind one shared codec module. They apply to:
- inputs: files and request bodies;
- outputs: dumps, backups, HTTP responses and UI assets;
- the full-text doc store;
- later, after measurement, storage formats: index blocks, the vocabulary and spill
  files, and the blobs of snapshot repositories ([F05](F05-snapshot-repositories.md)).

LZ4 stays where decode speed matters most, and gzip stays where compatibility matters.

Goals:
- The CLI, HTTP API and library name codecs the same way.
- Query latency does not change unless a user opts into a measured storage codec.
- The library's default features gain no C dependencies.
- Every input path is safe against decompression bombs.

Non-goals:
- Compressing the WAL, `delta.vocab` or `commits.bin`. Their records are fixed-size and
  latency-bound.
- In-memory compressed caches.
- Compressed network protocols other than HTTP `Content-Encoding`.

## 2. Inventory (as of `c10152b`)

| # | Use | Today | After |
|---|---|---|---|
| U1 | Load inputs (`sparkles load`, upload, `Source::from_path`) | `.gz` by extension | gzip, zstd and LZ4 frames detected by magic bytes. Brotli by the `.br` extension. |
| U2 | HTTP request bodies (update, GSP PUT/POST, upload, query POST) | None | `Content-Encoding: gzip, br, zstd, deflate` |
| U3 | Dumps and backups (`sparkles dump`, `sparkles backup`, `POST /$/backup/{ds}`) | Backups are gzip N-Quads. Dumps are uncompressed. | The codec and level are selectable. Backups default to gzip, as in Fuseki. A dump to stdout defaults to none. |
| U4 | HTTP responses | tower-http `compression-full` at default levels | Levels and algorithms are configurable. Bodies that are already encoded are never compressed again. |
| U5 | UI assets (rust-embed) | Stored raw and compressed per request | `ui:build` writes brotli and gzip siblings. The server picks one by `Accept-Encoding`. |
| U6 | Tantivy doc store | LZ4 | zstd, configurable in `text.json` |
| U7 | Index blocks | LZ4 per column | Each generation picks LZ4 (the default) or zstd, with optional dictionaries. Phase 2, after measurement. |
| U8 | Vocabulary `vocab.dat` | Front coding only | zstd per block. Phase 2, after measurement. |
| U9 | Bulk-load spill files | Uncompressed | LZ4 or zstd-1. Phase 2, after measurement. |
| U10 | Snapshot repository blobs (F05) | — | zstd. Phase 3, with F05. |

## 3. The codec module

`crates/sparkles-core/src/codec.rs`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec { None, Gzip, Zstd, Brotli, Lz4 }

#[derive(Clone, Copy, Debug)]
pub struct Level(pub i32);        // codec-specific; clamped (gzip 0–9, zstd 1–22 (neg = fast), brotli 0–11)

impl Codec {
    pub fn parse(s: &str) -> Result<Codec>;            // "none|gzip|gz|zstd|zst|brotli|br|lz4"
    pub fn extension(self) -> &'static str;            // "", ".gz", ".zst", ".br", ".lz4"
    pub fn content_encoding(self) -> Option<&'static str>; // gzip, zstd, br (lz4: none)
    pub fn from_extension(path: &Path) -> Option<Codec>;
    pub fn sniff(prefix: &[u8]) -> Option<Codec>;      // magic bytes, see below
    pub fn reader<'a>(self, r: impl Read + 'a, limit: Option<u64>) -> Result<Box<dyn Read + 'a>>;
    pub fn writer<'a>(self, w: impl Write + 'a, level: Option<Level>, threads: usize)
        -> Result<Box<dyn FinishWrite + 'a>>;          // finish() flushes the frame footer
}
```

- **Magic bytes.** gzip starts with `1f 8b`, a zstd frame with `28 b5 2f fd` and an LZ4
  frame with `04 22 4d 18`. zstd skippable frames start with `5? 2a 4d 18`. Brotli has
  no magic number, so only the `.br` extension or `Content-Encoding: br` selects it.
  Sniffing never does.
- **Detection order for files:**
  1. the explicit option (`--compression`);
  2. magic bytes;
  3. the extension;
  4. none.

  An explicit option that contradicts the magic bytes is an error, for example
  "file.ttl.zst is gzip data".
- **Concatenated frames** decode in full. gzip uses `MultiGzDecoder`, and the zstd and
  LZ4 readers handle multiple frames.
- **Limits.** `reader(…, limit)` checks the decompressed size after every chunk. Past
  `limit` bytes it fails with `Error::BudgetExceeded(ResultBytes-like
  "decompressed-bytes")`. For request bodies the limit is the server's
  `--max-decompressed-mb`, 64 GiB by default. CLI files have no limit.
- **Threads.** When `threads > 1`, the zstd writer uses
  `zstd::stream::Encoder::multithread(n)`. Dumps and backups default to `min(8, cores)`
  threads. gzip, brotli and LZ4 are single-threaded.

**Crates and features.** All have permissive licenses.

| Crate | License | Feature | Default in `sparkles` | Default in server/CLI |
|---|---|---|---|---|
| `flate2` (miniz_oxide backend) | MIT/Apache-2.0 | always | yes | yes |
| `lz4_flex` (frame format) | MIT | always | yes | yes |
| `zstd` (zstd-sys → libzstd, BSD) | BSD-3-Clause (zstd 0.14; tantivy pulls 0.13, MIT) | `zstd` | no | yes |
| `ruzstd` (pure Rust decoder) | MIT | `zstd-decode-pure` | no | no |
| `brotli` (pure Rust) | BSD-3-Clause AND MIT (both notices ship) | `brotli` | no | yes |

`zstd` builds C code through `cc`, as mimalloc does. That works for Nix and the static
build, and CI checks it. Embedded users who want no C can enable `zstd-decode-pure`,
which reads zstd with `ruzstd`. Writing zstd then returns `Error::Unsupported`. With
neither feature, a zstd input fails with a clear "built without zstd" error. Brotli
behaves the same way without its feature.

## 4. Behavior per use

### U1: load inputs
- `Source` gets a `codec: Codec` field in place of `gzip: bool`. `gzip()` stays for one
  release as a deprecated helper. `format_for_path` strips `.gz|.zst|.br|.lz4` before
  it guesses the RDF format.
- The CLI takes `sparkles load --compression auto|none|gzip|zstd|brotli|lz4`. The
  default is `auto`.
- The bulk-load estimate (`estimated_quads`) multiplies a compressed file's size by a
  factor per codec: 8 for gzip, 10 for zstd, 10 for brotli and 4 for LZ4.

### U2: request bodies
- The write and query routes get tower-http's `RequestDecompressionLayer`
  (`decompression-gzip,br,zstd,deflate`), placed **inside** the body limit. The
  decompressed stream is wrapped in the decompressed-size limit.
- An unsupported `Content-Encoding` gets `415`, with an `Accept-Encoding` header that
  names the supported codings (RFC 9110 §15.5.16).
- Request bodies over 16 MiB still spool to a temporary file. The spool holds the
  decompressed bytes.
- Multipart upload parts accept files named `*.zst`, `*.br` or `*.gz`. The codec is
  detected by name and magic bytes, as in U1.

### U3: dumps and backups
- `sparkles dump [--compress none|gzip|zstd|brotli|lz4] [--level N] [--threads N]`
  writes uncompressed output to stdout by default. With `--out`, the file's extension
  picks the codec.
- `sparkles backup --compress …` defaults to gzip.
- `POST /$/backup/{ds}?compression=zstd&level=3` defaults to gzip (`.nq.gz`), for Fuseki
  compatibility. The file name carries the codec's extension. Task messages report the
  ratio and the time taken.
- Restores and loads accept every codec (U1), so `sparkles load` restores a zstd backup.
- A GSP GET of a whole dataset ignores these options. HTTP `Accept-Encoding` governs it
  (U4).

### U4: HTTP responses
- `serve` takes `--http-compression auto|off`,
  `--http-compression-level fastest|default|best|N` and
  `--http-compression-algorithms zstd,br,gzip,deflate`. The order of the algorithms is
  the server's preference when the client's q-values tie.
- **Defaults:**
  - zstd level 3, br quality 4 and gzip level 6.
  - Bodies under 256 B are sent uncompressed (tower-http `SizeAbove`).
  - Types that are already compressed are skipped: `application/gzip`,
    `application/zstd`, images, and any response that already has `Content-Encoding`
    set, such as UI assets and backup downloads.
  - A test must verify the skip. If tower-http re-encodes such bodies, use
    `compress_when` with a predicate on `Content-Encoding`.
- **Streamed bodies**, such as streamed query results and GSP GET, are compressed
  incrementally. Compression becomes part of the serialize thread's back-pressure, so
  measure the throughput of a 1.1 GB N-Quads export for each algorithm (§6).

### U5: UI assets
- A `ui/scripts/precompress.mjs` step runs at the end of `ui:build`. It looks at every
  text asset over 1 KiB (`.js`, `.css`, `.html`, `.svg`, `.json`, `.map`). For each, it
  writes a `.br` sibling (quality 11, with Node's built-in zlib) and a `.gz` sibling
  (level 9), keeping each one only if it is at least 10% smaller.
- The `/ui/*` handler serves the best sibling that `Accept-Encoding` allows, preferring
  br, then gzip, then identity. It sets `Content-Encoding` and `Vary: Accept-Encoding`,
  and its `ETag` is the identity `ETag` with a codec suffix.
- The mock and the dev server do not change, because Vite serves the sources.
- The siblings add about 30% of the asset bytes to the binary. That is acceptable, but
  measure and report it.

### U6: Tantivy doc store
- Enable Tantivy's `zstd-compression` feature, and set
  `IndexSettings.docstore_compression = Zstd(level 3)` on new indexes.
- `TextConfig` gains `docstoreCompression: "lz4" | "zstd" | "none"`, defaulting to zstd.
  Changing it triggers a rebuild through the existing config hash.
- Existing LZ4 indexes keep working, because Tantivy records the compressor per
  segment.

### U7: index blocks (Phase 2)
- `IndexMeta` gains `block_codec: { codec: "lz4" | "zstd", level?, dictionary?: bool }`.
  A missing field means LZ4, so readers need no format bump for existing generations.
  Writers raise the format version by one only when the codec is not LZ4.
- `encode_column` and `decode_column` dispatch on the generation's codec. `PermIndex`
  holds the decoder. Each thread reuses a zstd `DecoderDictionary` through a
  thread-local `zstd::bulk::Decompressor`.
- **Dictionaries** are optional. The build trains one per permutation, shared by all
  columns, from a sample of about 1000 encoded blocks with `zstd::dict::from_samples`.
  The target size is about 64 KiB, and the file is `<perm>.dict`. `sparkles check`
  verifies the dictionary hash recorded in the meta.
- **Selection.** Bulk load and compaction take `--block-codec lz4|zstd[:level]`. The
  library takes `StoreOptions.build.block_codec`. Compaction may change the codec, which
  re-encodes the blocks.
- **Decision gate** (§6). zstd becomes the default only if it costs at most 5% on the
  warm suite and at most 15% on a cold run while saving at least 25% of disk. Otherwise
  LZ4 stays the default, and the docs recommend zstd for disk-bound or remote-storage
  deployments.

### U8: vocabulary (Phase 2)
- Each `vocab.dat` block is compressed with zstd. The blocks' first keys are already
  stored uncompressed and stay that way, so binary search decodes nothing.
- Measure lookup latency and on-disk size. The `distinct-obj`, `lang-filter` and
  `regex-iri` queries stress lookups. Before committing to zstd, compare it with FSST,
  which QLever uses.

### U9: spill files (Phase 2)
- The builder writes its temporary files (`b{id}.voc`, `.map` and the sorted runs)
  through the codec set by `BuildOptions.spill_codec`. The default is LZ4, which is fast
  and cheap on CPU.
- Measure a 100M-quad load on NVMe. If the codec does not help, the default becomes
  none.

### U10: repository blobs (Phase 3, with F05)
- A blob's hash covers the **plaintext**, so deduplication does not depend on the codec.
  The blob is then compressed with zstd at level 3, configurable per repository. The
  manifest records the codec and the sizes.
- Payloads that are already compressed, such as gzip backups, are stored with `none`.

## 5. Configuration summary

| Surface | Option | Default |
|---|---|---|
| CLI load | `--compression auto\|…` | auto |
| CLI dump/backup | `--compress`, `--level`, `--threads` | none / gzip |
| HTTP backup | `?compression=&level=` | gzip |
| serve | `--http-compression`, `--http-compression-level`, `--http-compression-algorithms`, `--max-decompressed-mb` | auto, default, zstd,br,gzip,deflate, 65536 |
| text.json | `docstoreCompression` | zstd |
| build | `--block-codec`, `spill_codec` | lz4, lz4 (Phase 2, subject to §6) |

## 6. Measurements (benchmark harness, quiet machine)

Run one engine at a time, with nothing else running.
1. **HTTP.** Export the 1.1 GB of N-Quads at 10.5M triples through GSP. Record wall time,
   bytes and server CPU for identity, gzip-6, br-4, zstd-3 and streamed zstd-1. Do the
   same for `SELECT * LIMIT 500000` as TSV.
2. **Backups.** Record the time and size of `/$/backup` with gzip-6, multithreaded zstd-3
   and brotli-5.
3. **Inputs.** Compare the load time of `sparkles load` on `data.nt.{gz,zst,br}` with the
   raw file at 10.5M triples. Decompression must not starve the parallel parser.
4. **Doc store.** Compare the text index's size and build time with LZ4 and zstd-3.
5. **Index blocks (Phase 2).**
   - Run the full suite warm and cold. For a cold run, drop caches with a fresh process
     and `vmtouch -e`. Without `vmtouch`, report warm numbers only.
   - Record disk size, load time and RSS.
   - Compare LZ4 with zstd-1, zstd-3 and zstd-3 with a dictionary.
6. **Vocabulary (Phase 2).** Record the size and the times of the vocabulary-heavy
   queries.

Only measured numbers go into `docs/BENCHMARKS.md`.

## 7. Phasing

- **Phase 1, about 1.5–2 days.** The codec module, U1 to U6, tests, docs (the API.md
  "Compression" section and the README's flags) and measurements 1–4.
- **Phase 2, about 3 days, gated on measurement.** U7, U8 and U9 with measurements 5–6.
  The gate in §4 U7 decides the defaults.
- **Phase 3.** U10, built together with [F05](F05-snapshot-repositories.md).

## 8. Acceptance examples

- **A1.** `sparkles load --loc db data.ttl.zst` loads the same quads as `data.ttl`, and
  so do the `.br` and `.gz` files. A file named `x.ttl.zst` that holds gzip data loads
  with a warning, because the magic bytes win. `--compression brotli` on gzip data is an
  error.
- **A2.** `POST /ds/data?default` with `Content-Encoding: zstd`, `br` or `gzip` inserts
  the quads. `Content-Encoding: compress` returns 415 with an `Accept-Encoding` header.
  A 1 KB zstd body that inflates past `--max-decompressed-mb 1` returns 507 with
  `budget: "decompressed-bytes"` and commits nothing.
- **A3.** A 20 MiB zstd request body takes the spooling path. It is decompressed into a
  temp file and bulk loaded.
- **A4.** `sparkles dump --compress zstd --out x.nq.zst` round-trips through
  `sparkles load`. `POST /$/backup/ds?compression=zstd` writes `ds_<ts>.nq.zst`, and
  loading it restores the same quad count. The default backup is still `.nq.gz`.
- **A5.** `GET /ds/data` with `Accept-Encoding: zstd` returns `Content-Encoding: zstd`,
  and the decoded body equals the identity body. The same holds for `br` and `gzip`. A
  streamed response over 1 MiB decodes correctly with each codec. A 100-byte response
  is not compressed.
- **A6.** `GET /ui/` with `Accept-Encoding: br, gzip` serves the prebuilt `.br` with
  `Content-Encoding: br` and `Vary`, and does not compress it again. Without
  `Accept-Encoding` it serves identity.
- **A7.** Enabling text search writes a zstd doc store, and the Tantivy meta shows
  `docstore_compression: zstd`. Changing `docstoreCompression` to `lz4` rebuilds the
  index, and queries return the same hits.
- **A8.** In a build without the `zstd` feature, a `.zst` load fails with
  `Unsupported: built without zstd`. With `zstd-decode-pure` it loads.
- **A9 (Phase 2).** A generation built with `--block-codec zstd:3` gives the same
  answers as LZ4 on the W3C suite and the benchmark queries, and `sparkles check`
  passes. A pinned old LZ4 generation and a new zstd generation can both be read.
  Compaction with a different codec re-encodes the blocks.
- **A10 (Phase 2).** Dictionary-compressed blocks decode after a restart. `sparkles
  check` reports a damaged `.dict` as an error.

## 9. Open questions

1. **Backup default.** Should a later major version switch backups to zstd? That would
   break Fuseki tooling that expects `.nq.gz`. Default: keep gzip.
2. **HTTP preference.** When a client accepts `br` and `zstd` at the same q-value,
   should the server prefer zstd? For RDF text it is faster at a similar ratio.
   Default: zstd, then br.
3. **A pure-Rust zstd encoder for C-free builds.** `ruzstd`'s encoder supports only the
   "fastest" level today. Default: the pure-Rust option decodes only.
4. **Compressing `delta.vocab`.** It could help update-heavy datasets with long
   literals, but it adds append latency. Default: no.

## 10. Sources

- Formats:
  - RFC 8878, Zstandard: magic `0xFD2FB528` and skippable frames.
  - RFC 7932, Brotli, which has no magic number.
  - RFC 1952, gzip: magic `1f 8b`.
  - The LZ4 frame format specification: magic `0x184D2204`.
- RFC 9110: §8.4 `Content-Encoding`, §12.5.3 `Accept-Encoding` and §15.5.16 for 415.
- Crate docs:
  - `zstd`: the bulk and stream APIs, `Encoder::multithread` and `dict::from_samples`.
  - `ruzstd`, `brotli`, `flate2` and `lz4_flex`.
  - tower-http 0.7: `CompressionLayer::quality`, `no_br`/`no_zstd`, `compress_when`,
    `RequestDecompressionLayer` and the `decompression-*` features.
  - rust-embed 8. Its `compression` feature supports only deflate, through
    include-flate, which is why the UI siblings come from a build step.
  - Tantivy 0.26: `IndexSettings::docstore_compression` and the `zstd-compression`
    feature.
- Sparkles code: `index.rs` (column encoding), `vocab.rs`, `builder.rs` (spill files),
  `io.rs` (`Source`, gzip), `store.rs` (`backup`, `dump_nquads`, `estimated_quads`),
  `http.rs` (the compression layer and spooling) and `text.rs`.

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30 (`2aef5f4`) and covers U1–U6 as specified:
- the shared `sparkles::codec` module for gzip, zstd, brotli and LZ4 frames. It detects
  codecs by magic bytes, brotli by extension only, and caps the decompressed size;
- compressed loads with `--compression`;
- compressed request bodies through tower-http's request decompression, limited by
  `--max-decompressed-mb`;
- `--compress`, `--level` and `--threads` on dumps and backups, and
  `?compression=&level=` on `/$/backup/{ds}`;
- the `--http-compression*` flags;
- brotli and gzip siblings of the UI assets, built by `ui/scripts/precompress.mjs`;
- a zstd Tantivy doc store (`docstoreCompression`).

The `zstd` feature (libzstd, a C build) and the `brotli` feature are off in the
`sparkles` library and on in the server.

**Additional file codecs.** The shared codec module also reads and writes xz
(`xz`, `.xz`) and bzip2 (`bzip2` or `bz2`, `.bz2`). Both are available in minimal
library builds. xz uses `liblzma` with static linking and pregenerated bindings,
adding a C build dependency; bzip2 uses the Rust `libbz2-rs-sys` backend through
`bzip2`. This supersedes the original no-new-C-dependencies goal for native xz
support. Six magic bytes identify xz (`fd 37 7a 58 5a 00`); bzip2 is `BZh` followed
by a block-size digit from `1` to `9`. Both decoders read concatenated streams to
EOF, reject truncated later members and trailing garbage, and enforce the same
decompressed-byte limit as the other codecs. xz decoder working memory is capped
at 256 MiB independently of the decompressed-byte limit; custom dictionaries
above that bound fail instead of allocating unbounded memory. Writer levels are
clamped to 0–9 for xz and 1–9 for bzip2, defaulting to 6; both writers are
single-threaded and `finish` writes the footer and flushes the destination.
Neither adds an HTTP `Content-Encoding` token or response-negotiation algorithm.
See the [liblzma stream API](https://docs.rs/liblzma/latest/liblzma/stream/struct.Stream.html)
and [bzip2 multi-stream API](https://docs.rs/bzip2/latest/bzip2/read/struct.MultiBzDecoder.html).

**Deviations and decisions.**
- A body that inflates past `--max-decompressed-mb` gets `413` and commits nothing. That
  matches the other request-body limits. A2 asked for `507`.
- The `zstd-decode-pure` feature (`ruzstd`) was not built. Without the `zstd` feature, a
  zstd input fails with "built without zstd".
- When `docstoreCompression` is unset, the doc store keeps the build default, so
  existing text configurations keep their hash. Indexes built before zstd keep LZ4
  until they are rebuilt.
- After the measurements, the maintainer answered open question 1 the other way.
  `/$/backup/{ds}`, `sparkles backup` and the library's backups default to zstd level 3
  (`.nq.zst`; `548fb7d`, `83c152e`). `?compression=gzip` or `--compress gzip` still
  writes Fuseki's `.nq.gz`. The change is recorded as a [design decision](../COMPARISON.md#divergences-from-jena--qlever-decisions).
- The maintainer skipped Phase 2 (U7–U9), because its gate needs a prototype of zstd
  index blocks. LZ4 stays the block codec. The docs list zstd blocks and FSST as
  possible upgrades for disk-bound deployments.
- U10 shipped with the backup repositories of [F05](F05-snapshot-repositories.md). Blob
  hashes cover the plaintext, as specified, but blobs are stored raw or compressed with
  LZ4. The zstd codec byte is reserved and never written.

**Measurements.** These cover items 1–4 of §6 at 10.5M triples. The full results are in
the [benchmarks](../BENCHMARKS.md#compression-105m-triples).
- zstd-3 streams the 1.1 GB N-Quads export as fast as identity (3.74 s against 3.78 s)
  and makes it 13.5× smaller. gzip-6 doubles the wall time and triples the server CPU.
- `/$/backup` takes 8.2 s with zstd-3 (81.5 MB) and 41 s with gzip-6 (74.9 MB).
- Compressed loads take 0.5–0.9 s longer than raw ones.
- The full-text doc store is 106 MB with zstd and 118 MB with LZ4, with the same build
  time.

**Not built.** The Phase 2 codecs for index blocks, the vocabulary and spill files;
dictionaries; the pure-Rust zstd decoder; and zstd repository blobs.
