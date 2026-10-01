# X01: Compression codecs (zstd, brotli, LZ4, gzip)

> **Status:** implemented in part
>
> **Phases:** Phase 1 (the codec module; compressed inputs and request bodies, codecs for
> dumps and backups, configurable response compression, precompressed UI assets, a zstd
> full-text doc store) shipped. Phase 2 (zstd index blocks, vocabulary and spill files)
> was not built, by the maintainer's decision. Phase 3's blobs shipped with
> [F05](F05-snapshot-repositories.md), LZ4-compressed rather than zstd.
>
> **User docs:** [API: Compression](../API.md#compression) · [Features](../FEATURES.md#storage-tdb2-equivalent) · [Benchmarks: Compression](../BENCHMARKS.md#compression-105m-triples)
>
> This is the design as written before implementation; the [Outcome](#outcome) section at
> the end records how it landed.

The design is internal engineering, not derived from any other database product. Its
sources are the codec formats' own specifications and the Rust crates' docs (listed at the end).

## 1. Summary

Sparkles uses **LZ4** for index blocks and **gzip** for backups and `.gz` inputs, and
negotiates response compression through tower-http. This spec adds **zstd** and
**brotli** wherever compression helps, behind one shared codec module:
- **inputs:** files and request bodies;
- **outputs:** dumps, backups, HTTP responses and UI assets;
- **the full-text doc store**;
- **later, measured storage formats:** index blocks, vocabulary and spill files, plus
  the blobs of snapshot repositories ([F05](F05-snapshot-repositories.md)).

LZ4 stays where decode speed dominates. gzip stays where compatibility matters.

Goals:
- one codec vocabulary across CLI, HTTP and library;
- no change to query latency unless a measured storage codec is opted into;
- zero new C dependencies in the library's default features;
- decompression-bomb safety on every input path.

Non-goals:
- compressing the WAL, `delta.vocab` or `commits.bin` (fixed-size, latency-bound
  records);
- in-memory compressed caches;
- compressed network protocols other than HTTP `Content-Encoding`.

## 2. Inventory (as of `c10152b`)

| # | Use | Today | After |
|---|---|---|---|
| U1 | Load inputs (`sparkles load`, upload, `Source::from_path`) | `.gz` by extension | gzip, zstd, LZ4 frame sniffed by magic bytes; brotli by `.br` extension |
| U2 | HTTP request bodies (update, GSP PUT/POST, upload, query POST) | none | `Content-Encoding: gzip, br, zstd, deflate` |
| U3 | Dumps and backups (`sparkles dump`, `sparkles backup`, `POST /$/backup/{ds}`) | gzip N-Quads for backups; plain for dump | selectable codec and level; default gzip for backups (Fuseki), none for dump to stdout |
| U4 | HTTP responses | tower-http `compression-full`, default levels | configurable levels and algorithms; never re-compress already encoded bodies |
| U5 | UI assets (rust-embed) | raw; compressed per request | brotli and gzip siblings built at `ui:build`, served by `Accept-Encoding` |
| U6 | Tantivy doc store | LZ4 | zstd (configurable in `text.json`) |
| U7 | Index blocks | LZ4 per column | per-generation codec: LZ4 (default) or zstd, optional dictionaries (Phase 2, measured) |
| U8 | Vocabulary `vocab.dat` | front coding only | blockwise zstd (Phase 2, measured) |
| U9 | Bulk-load spill files | raw | LZ4 or zstd-1 (Phase 2, measured) |
| U10 | Snapshot repository blobs (F05) | — | zstd (Phase 3, with F05) |

## 3. The codec module

`crates/sparkles/src/codec.rs`:

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

- **Magic bytes.** gzip `1f 8b`; zstd frame `28 b5 2f fd` (and skippable frames
  `5? 2a 4d 18`); LZ4 frame `04 22 4d 18`. **Brotli has no magic**, so it is chosen by
  `.br` or `Content-Encoding: br` only, never by sniffing.
- **Detection order for files:**
  1. the explicit option (`--compression`);
  2. magic bytes;
  3. the extension;
  4. none.

  A mismatch between an explicit option and the magic bytes is an error, e.g.
  "file.ttl.zst is gzip data".
- **Concatenated frames** decode fully (`MultiGzDecoder`; zstd and LZ4 multi-frame).
- **Limits.** `reader(…, limit)` fails with `Error::BudgetExceeded(ResultBytes-like
  "decompressed-bytes")` past `limit` decompressed bytes, checked every chunk.
  Default: `--max-decompressed-mb` (server, 64 GiB) for request bodies; unlimited for
  CLI files.
- **Threads.** The zstd writer uses `zstd::stream::Encoder::multithread(n)` when
  `threads > 1` (dumps and backups default to `min(8, cores)`). gzip, brotli and LZ4
  are single-threaded.

**Crates and features** (all permissive):

| Crate | License | Feature | Default in `sparkles` | Default in server/CLI |
|---|---|---|---|---|
| `flate2` (miniz_oxide backend) | MIT/Apache-2.0 | always | yes | yes |
| `lz4_flex` (frame format) | MIT | always | yes | yes |
| `zstd` (zstd-sys → libzstd, BSD) | BSD-3-Clause (zstd 0.14; tantivy pulls 0.13, MIT) | `zstd` | no | yes |
| `ruzstd` (pure Rust decoder) | MIT | `zstd-decode-pure` | no | no |
| `brotli` (pure Rust) | BSD-3-Clause AND MIT (both notices ship) | `brotli` | no | yes |

`zstd` is a C build (`cc`), like mimalloc: fine for Nix and the static build, checked in
CI. Embedded users who want no C can enable `zstd-decode-pure` to read zstd with
`ruzstd`; writing zstd then returns `Error::Unsupported`. With neither feature, zstd
inputs fail with a clear "built without zstd" error, and likewise for brotli.

## 4. Behavior per use

### U1: load inputs
- `Source` gets `codec: Codec`, replacing `gzip: bool` (keep `gzip()` as a deprecated
  helper for one release). `format_for_path` strips `.gz|.zst|.br|.lz4` before guessing
  the RDF format.
- CLI `sparkles load --compression auto|none|gzip|zstd|brotli|lz4` (default `auto`).
- The bulk-load estimate (`estimated_quads`) multiplies compressed sizes by the codec
  factor: gzip 8, zstd 10, brotli 10, LZ4 4.

### U2: request bodies
- tower-http `RequestDecompressionLayer` (`decompression-gzip,br,zstd,deflate`) on the
  write and query routes, **inside** the body limit. The decompressed stream is wrapped
  with the decompressed-size limit, and an unsupported `Content-Encoding` gets `415`
  with `Accept-Encoding` naming what is supported (RFC 9110 §15.5.16).
- Request bodies over 16 MiB still spool to a temporary file, and the spool holds the
  decompressed bytes.
- Multipart upload parts also accept files named `*.zst`, `*.br` or `*.gz` (by name and
  magic, as U1).

### U3: dumps and backups
- `sparkles dump [--compress none|gzip|zstd|brotli|lz4] [--level N] [--threads N]`:
  default none to stdout, or by the `--out` file's extension.
- `sparkles backup --compress …`: default gzip.
- `POST /$/backup/{ds}?compression=zstd&level=3`: default gzip (Fuseki compatibility,
  `.nq.gz`). The file name carries the codec extension. Task messages report the ratio
  and time.
- Restores and loads accept every codec (U1), so a zstd backup restores with
  `sparkles load`.
- GSP GET of a whole dataset stays governed by HTTP `Accept-Encoding` (U4), not these
  options.

### U4: HTTP responses
- `serve --http-compression auto|off` and
  `--http-compression-level fastest|default|best|N`, plus
  `--http-compression-algorithms zstd,br,gzip,deflate` (order = server preference when
  the client's q-values tie).
- **Defaults:**
  - zstd level 3, br quality 4, gzip 6;
  - bodies under 256 B are left alone (tower-http `SizeAbove`);
  - already compressed types are skipped: `application/gzip`, `application/zstd`, images,
    and anything with `Content-Encoding` already set (UI assets, backup downloads).
  - The skip must be verified in a test, not assumed; if tower-http re-encodes, use
    `compress_when` with a predicate on `Content-Encoding`.
- **Streamed bodies** (query streaming, GSP GET) are compressed incrementally. Their
  compression is part of the serialize thread's back-pressure; measure the throughput
  of a 1.1 GB N-Quads export per algorithm (§6).

### U5: UI assets
- A `ui/scripts/precompress.mjs` step runs at the end of `ui:build`. For every text asset
  over 1 KiB (`.js`, `.css`, `.html`, `.svg`, `.json`, `.map`), it writes `.br`
  (quality 11, Node's built-in zlib) and `.gz` (level 9) siblings when they are at least
  10% smaller.
- The `/ui/*` handler picks the best sibling that `Accept-Encoding` allows (br > gzip >
  identity), sets `Content-Encoding` and `Vary: Accept-Encoding`, and keeps the
  identity `ETag` with a codec suffix.
- The mock and dev server are unaffected (Vite serves sources).
- The siblings grow the binary by roughly 30% of the asset bytes. That is acceptable;
  measure and report it.

### U6: Tantivy doc store
- Enable tantivy `zstd-compression`, and set `IndexSettings.docstore_compression =
  Zstd(level 3)` on new indexes.
- `TextConfig` gains `docstoreCompression: "lz4" | "zstd" | "none"` (default zstd);
  changing it triggers a rebuild through the existing config hash.
- Existing LZ4 indexes keep working; Tantivy records the compressor per segment.

### U7: index blocks (Phase 2)
- `IndexMeta` gains `block_codec: { codec: "lz4" | "zstd", level?, dictionary?: bool }`.
  A missing field means LZ4, so existing generations need no format bump for reading.
  Writers set the format version to +1 only when not LZ4.
- `encode_column` / `decode_column` dispatch on the generation's codec.
  `PermIndex` holds the decoder: zstd `DecoderDictionary` reuse per thread through a
  thread-local `zstd::bulk::Decompressor`.
- **Dictionaries** (optional): trained at build time per permutation (column-agnostic)
  from a sample of about 1000 encoded blocks with `zstd::dict::from_samples`, targeting
  about 64 KiB, and stored as `<perm>.dict`. `sparkles check` verifies the dictionary
  hash in the meta.
- **Selection.** Bulk load and compaction take `--block-codec lz4|zstd[:level]`, and
  the library takes `StoreOptions.build.block_codec`. Compaction may change the codec
  (a re-encoding path).
- **Decision gate** (§6): LZ4 stays the default unless zstd costs ≤ 5% on the warm suite
  and ≤ 15% on a cold run while saving ≥ 25% disk. Otherwise zstd is documented as the
  choice for disk-bound or remote-storage deployments.

### U8: vocabulary (Phase 2)
- Blockwise zstd over `vocab.dat` blocks, with block first-keys kept uncompressed (they
  already are) so binary search stays decode-free.
- Measure lookup latency (the `distinct-obj`, `lang-filter` and `regex-iri` queries
  stress it) and on-disk size. Compare with FSST (QLever's approach) as an alternative
  before committing.

### U9: spill files (Phase 2)
- Builder temp files (`b{id}.voc`, `.map` and sorted runs) are written through the
  codec: `BuildOptions.spill_codec`, default LZ4 (fast, cheap CPU).
- Measure a 100M-quad load on NVMe. If the codec isn't a win, the default is none.

### U10: repository blobs (Phase 3, with F05)
- Blobs are hashed over the **plaintext** (dedup is codec-independent), then
  zstd-compressed (level 3, configurable per repository). The manifest records codec
  and sizes.
- Already compressed payloads (gzip backups) are stored with `none`.

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

One engine at a time, nothing else running.
1. **HTTP:** 1.1 GB N-Quads GSP export at 10.5M; wall time, bytes and server CPU for
   identity, gzip-6, br-4, zstd-3, and zstd-1 streamed. Also `SELECT * LIMIT 500000`
   as TSV.
2. **Backups:** time and size of `/$/backup` for gzip-6 vs zstd-3 (multithreaded) vs
   brotli-5.
3. **Inputs:** `sparkles load` of `data.nt.{gz,zst,br}` vs raw at 10.5M (load time; the
   parallel parser must not starve).
4. **Doc store:** text index size and build time, LZ4 vs zstd-3.
5. **Index blocks (Phase 2):**
   - the full suite, cold (drop caches via a fresh process + `vmtouch -e` if available,
     else report warm only) and warm;
   - disk size, load time and RSS;
   - LZ4 vs zstd-1, zstd-3 and zstd-3+dictionary.
6. **Vocabulary (Phase 2):** size and the vocabulary-heavy queries.

Numbers go to `docs/BENCHMARKS.md` (measured only).

## 7. Phasing

- **Phase 1 (about 1.5–2 days).** The codec module; U1, U2, U3, U4, U5 and U6; tests; docs
  (API.md "Compression" section, README flags); measurements 1–4.
- **Phase 2 (about 3 days, measurement-gated).** U7, U8 and U9 with measurements 5–6;
  defaults decided by §4 U7's gate.
- **Phase 3.** U10, together with [F05](F05-snapshot-repositories.md).

## 8. Acceptance examples

- **A1.** `sparkles load --loc db data.ttl.zst` equals loading `data.ttl` (same quads).
  The same for `.br` and `.gz`. A file named `x.ttl.zst` that holds gzip data loads
  (magic wins) with a warning. `--compression brotli` on gzip data is an error.
- **A2.** `POST /ds/data?default` with `Content-Encoding: zstd` (and `br`, `gzip`)
  inserts the quads. `Content-Encoding: compress` returns 415 with an `Accept-Encoding`
  header. A 1 KB zstd body that inflates past `--max-decompressed-mb 1` returns 507 with
  `budget: "decompressed-bytes"` and commits nothing.
- **A3.** A 20 MiB zstd request body is spooled (decompressed) to a temp file and bulk
  loaded (the spooling path).
- **A4.** `sparkles dump --compress zstd --out x.nq.zst` round-trips through
  `sparkles load`. `POST /$/backup/ds?compression=zstd` writes `ds_<ts>.nq.zst`, and
  loading it restores the same quad count. The default backup is still `.nq.gz`.
- **A5.** `GET /ds/data` with `Accept-Encoding: zstd` returns `Content-Encoding: zstd`,
  and the decoded body equals the identity body. `br` and `gzip` likewise. A streamed
  (>1 MiB) response decodes correctly for each. A 100-byte response is not compressed.
- **A6.** `GET /ui/` with `Accept-Encoding: br, gzip` serves the prebuilt `.br` with
  `Content-Encoding: br` and `Vary`, and it is not compressed again. Without
  `Accept-Encoding` it serves identity.
- **A7.** Enabling text search writes a zstd doc store (Tantivy meta shows
  `docstore_compression: zstd`). Changing `docstoreCompression` to `lz4` rebuilds, and
  queries return identical hits.
- **A8.** Built without the `zstd` feature, a `.zst` load fails with
  `Unsupported: built without zstd`, and `zstd-decode-pure` makes it load.
- **A9 (Phase 2).** A generation built with `--block-codec zstd:3` answers the W3C
  suite and the benchmark queries identically to LZ4. `sparkles check` passes. Mixed
  generations (a pinned old LZ4 generation plus a new zstd one) both read. Compaction
  with a different codec re-encodes.
- **A10 (Phase 2).** Dictionary blocks decode after a restart; a damaged `.dict` is a
  `sparkles check` error.

## 9. Open questions

1. **Backup default:** switch to zstd in a later major version (breaking `.nq.gz`
   expectations of Fuseki tooling)? Default: keep gzip.
2. **HTTP preference** when a client accepts both `br` and `zstd` at the same q:
   prefer zstd (faster at similar ratio for RDF text)? Default: zstd, then br.
3. **A pure-Rust zstd encoder** for C-free builds: `ruzstd`'s encoder is currently
   "fastest" level only. Default: decode-only pure option.
4. **Compression of `delta.vocab`** for update-heavy datasets with long literals?
   Default: no (append latency).

## 10. Sources

- RFC 8878 (Zstandard format, magic `0xFD2FB528`, skippable frames); RFC 7932
  (Brotli, no magic number); RFC 1952 (gzip, `1f 8b`); the LZ4 frame format
  specification (magic `0x184D2204`).
- RFC 9110 §8.4 (`Content-Encoding`), §12.5.3 (`Accept-Encoding`), §15.5.16 (415).
- Crate docs: `zstd` (bulk/stream APIs, `Encoder::multithread`, `dict::from_samples`),
  `ruzstd`, `brotli`, `flate2`, `lz4_flex`, and tower-http 0.7 (`CompressionLayer::quality`,
  `no_br`/`no_zstd`, `compress_when`, `RequestDecompressionLayer`, features
  `decompression-*`). rust-embed 8 (its `compression` feature is deflate-only via
  include-flate, hence the build-step siblings). Tantivy 0.26 (`IndexSettings::docstore_compression`,
  `zstd-compression` feature).
- Sparkles code: `index.rs` (column encoding), `vocab.rs`, `builder.rs` (spill files),
  `io.rs` (`Source`, gzip), `store.rs` (`backup`, `dump_nquads`, `estimated_quads`),
  `http.rs` (compression layer, spooling), `text.rs`.

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30 (`2aef5f4`) as specified for U1–U6: the
shared `sparkles::codec` module (gzip, zstd, brotli, LZ4 frame; magic-byte detection with
brotli by extension only; a decompressed-size cap), compressed loads with
`--compression`, compressed request bodies (tower-http request decompression) with
`--max-decompressed-mb`, `--compress`/`--level`/`--threads` on dumps and backups and
`?compression=&level=` on `/$/backup/{ds}`, the `--http-compression*` flags, brotli and
gzip siblings of the UI assets built by `ui/scripts/precompress.mjs`, and a zstd
Tantivy doc store (`docstoreCompression`). The `zstd` (libzstd, a C build) and `brotli`
features are off in the `sparkles` library and on in the server.

**Deviations and decisions.**
- A body that inflates past `--max-decompressed-mb` answers `413` (and commits nothing),
  like the other request-body ceilings, not the `507` of A2.
- The `zstd-decode-pure` feature (`ruzstd`) was not built; without the `zstd` feature a
  zstd input fails with "built without zstd".
- The doc store keeps the build default when `docstoreCompression` is unset, so existing
  text configurations keep their hash; indexes built before zstd keep LZ4 until rebuilt.
- The maintainer settled open question 1 the other way after the measurements:
  `/$/backup/{ds}`, `sparkles backup` and the library backups default to
  zstd level 3 (`.nq.zst`; `548fb7d`, `83c152e`). `?compression=gzip` or `--compress gzip`
  still writes Fuseki's `.nq.gz`. This is recorded as a [design decision](../COMPARISON.md#divergences-from-jena--qlever-decisions).
- The maintainer skipped Phase 2 (U7–U9) for now: its gate needs a zstd index-block
  prototype. LZ4 stays the block codec, and zstd blocks and FSST are documented as
  possible upgrades for disk-bound deployments.
- U10 shipped with the backup repositories of [F05](F05-snapshot-repositories.md). Blobs
  are hashed over the plaintext, as specified, but stored raw or LZ4-compressed; the zstd
  codec byte is reserved and not written.

**Measurements** ([benchmarks](../BENCHMARKS.md#compression-105m-triples), 10.5M
triples, items 1–4 of §6): zstd-3 streams the 1.1 GB N-Quads export as fast as identity
(3.74 s vs 3.78 s) at 13.5× smaller, while gzip-6 doubles the wall time and triples the
server CPU; `/$/backup` takes 8.2 s with zstd-3 (81.5 MB) against 41 s with gzip-6
(74.9 MB); compressed loads cost 0.5–0.9 s more than raw; the full-text doc store is
106 MB with zstd against 118 MB with LZ4 at the same build time.

**Not built.** Index-block, vocabulary and spill-file codecs (Phase 2), dictionaries, the
pure-Rust zstd decoder, and zstd repository blobs.
