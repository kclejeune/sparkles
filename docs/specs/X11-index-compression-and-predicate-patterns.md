# X11: Index compression and predicate patterns

> **Status:** Phase 1 built. Phase 2a, FSST suffixes, is built as an option that is off by
> default. Phase 2b and Phase 3 are not built. Their measured savings are below their
> gates, and the [Outcome](#phases-not-built) gives the figures.
>
> **Phases:** Phase 1 is predicate patterns: a per-subject pattern table built with the
> generation, `spk:hasPredicate`, and the planner's use of patterns for predicate listings
> and counts. Phase 2 adds the vocabulary codecs: FSST-compressed suffixes and encoded
> IRIs. Phase 3 adds index column codecs and optional zstd blocks with dictionaries. Each
> phase that changes the files raises the index format version under the rules of §7.
>
> **User docs:** [Predicate patterns and `spk:hasPredicate`](../API.md#predicate-patterns-and-spkhaspredicate),
> the `--no-patterns`, `--pattern-memory-mb` and `--vocab-codec` flags and
> `sparkles stats --patterns` in [USAGE.md](../USAGE.md), and the `predicate_patterns` flag in
> [OPTIMIZATIONS.md](../internals/OPTIMIZATIONS.md).
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how Phases 1 and 2a landed and where they depart from the design.

This design was written from H. Bast and B. Buchhold, "QLever: A Query Engine for
Efficient SPARQL+Text Search", CIKM 2017, and the public QLever documentation of
`ql:has-predicate`, for predicate patterns; P. Boncz, T. Neumann and V. Leis, "FSST: Fast
Random Access String Compression", PVLDB 13(11), 2020, for the string codec; C. Neumann
and G. Moerkotte, "Characteristic Sets: Accurate Cardinality Estimation for RDF Queries
with Multiple Joins", ICDE 2011, which Sparkles' statistics already follow; D. Lemire and
L. Boytsov, "Decoding billions of integers per second through vectorization", Software:
Practice and Experience 45(1), 2015, for frame-of-reference bit packing; the zstd
documentation of dictionary training; and the Sparkles code at `fc123d04` (the index
builder and its statistics in `crates/sparkles-core/src/builder.rs`, the permutation
files in `index.rs`, the vocabulary in `vocab.rs`, ids in `id.rs`, partial compaction in
`store/partial.rs`) with the specs [X01](X01-compression-codecs.md),
[C13](C13-automatic-compaction.md), [C02](C02-schema-discovery.md),
[C12](C12-graph-access-control.md), [C12b](C12b-triple-access-control.md) and
[F05](F05-snapshot-repositories.md). QLever's design is taken from the paper and its
documentation, not from its code.

## 1. Summary, goals, non-goals

[COMPARISON](../COMPARISON.md) lists two of QLever's techniques that Sparkles lacks:
predicate patterns (the "pattern trick", with `ql:has-predicate`) and a compressed
vocabulary (FSST string compression, and IRIs whose numeric suffix is encoded in the
id). X01 planned zstd index blocks, a zstd vocabulary and dictionaries as its Phase 2,
compared with FSST, and the maintainer skipped that phase because its gate needed a
prototype (X01 Outcome).

The current state:

* A generation's vocabulary (`vocab.dat`) is front-coded in blocks of 16 keys
  (`vocab::FC_BLOCK`), each key stored as a shared-prefix length and its suffix, with
  block offsets in `vocab.off`, a sparse index of every 128th block's first key in
  `vocab.idx` and numeric values in `vocab.num`. On the 1.24-billion-line DBpedia load,
  `vocab.dat` is 8.0 GB of the 28.7 GiB index (BENCHMARKS, "Numeric literals in the
  vocabulary"). Keys sort literals first, then triple terms, then IRIs.
* Each permutation file (`<perm>.dat`, `<perm>.meta`) stores blocks of up to 32,768 rows
  (`index::BLOCK_ROWS`), each of four columns encoded by `encode_column`: zig-zag deltas
  as varints, then LZ4. Block metadata stays in memory.
* The statistics (`builder::Stats`) already hold each predicate's count and distinct
  subjects and objects, and the 10,000 most common characteristic sets of subjects
  (`Stats::charsets`), which `StatsCollector` computes while SPO is streamed. Sparkles
  keeps no per-subject record of which set a subject has.
* `IndexMeta::format_version` is 2 (`builder::FORMAT_VERSION`), and `Generation::open_with`
  in `store.rs` refuses any other version with "dump and reload the data".

**Goals**

1. Answer "which predicates do these subjects have, and for how many of them" from a
   per-subject pattern table, without scanning each subject's triples, for the predicate
   listings of schema discovery, faceted browsing and `ql:has-predicate`-style queries.
2. Shrink the vocabulary with FSST and encoded IRIs, measured on DBpedia, without slowing
   lookups or the queries that decode many terms.
3. Measure better integer codecs for index columns, and zstd blocks with dictionaries,
   against X01's gate, and adopt only what passes.
4. Change the on-disk format in a way that existing generations stay readable, and record
   every new feature in the generation's metadata.

**Non-goals**

* Text and entity co-occurrence (QLever's `ql:contains-word` and `ql:contains-entity`).
* Changing the id layout of existing tags, or the order of vocabulary ids within the
  vocabulary.
* A migration tool. A generation is rewritten by compaction or reload. The stability
  policy for formats belongs to [X12](X12-release-engineering.md).

## 2. User-visible behaviour

### 2.1 Predicate patterns

`spk:hasPredicate` (`urn:x-sparkles:hasPredicate`) is a property: `?s spk:hasPredicate ?p`
holds when `?s` is the subject of at least one triple with predicate `?p` in the active
graph. QLever's `ql:has-predicate`
(`<http://qlever.cs.uni-freiburg.de/builtin-functions/has-predicate>`) is accepted as a
synonym. Both work with any combination of bound and unbound arguments, with and without
the pattern table: without it, they are evaluated from the SPO index as
`SELECT DISTINCT ?s ?p { ?s ?p [] }`.

The planner uses the table for these shapes, whether written with `spk:hasPredicate` or
in plain SPARQL:

```sparql
# predicates of a set of subjects, with the number of subjects having each
SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s a dbo:Film . ?s ?p ?o } GROUP BY ?p

SELECT ?p (COUNT(?s) AS ?n) WHERE { ?s a dbo:Film . ?s spk:hasPredicate ?p } GROUP BY ?p

# predicates of one subject
SELECT DISTINCT ?p WHERE { <s> ?p ?o }
```

A plain `?s ?p ?o` qualifies only when `?o` is used nowhere else and the aggregate counts
distinct subjects, since the table records which predicates a subject has and not how
many triples. The plan names the operator `PatternCount` or `PatternScan`. C02's class
profiles and predicate listings, and the UI's schema browser, use the same operator.

`sparkles stats --patterns` prints the number of patterns, the subjects per pattern and
the table's size.

### 2.2 Vocabulary and index codecs

`BuildOptions` and the bulk load and compaction flags gain:

| Option | Values | Default |
|---|---|---|
| `--vocab-codec` | `front` (today's), `fsst` | `front` until the gate of §4.4 passes |
| `--iri-encoding` | `off`, `auto`, or a list of prefixes | `off` until the gate passes |
| `--block-codec` | `lz4` (today's), `pfor`, `zstd[:level]` | `lz4` |
| `--block-dict` | on or off, for `zstd` | off |
| `--patterns` | on or off | on |

The dataset settings (C19) hold the same choices per dataset, and compaction applies
them. `sparkles stats` reports the codecs of the current generation, and `sparkles check`
verifies their tables (§7.3).

## 3. Predicate patterns (Phase 1)

### 3.1 Files

A pattern is a sorted set of predicate ids. The build writes:

* `patterns.dat`: the patterns, each as a varint count and zig-zag delta varints of its
  predicate ids, numbered in order of decreasing subject count, with an offset table at
  the end. Each pattern also stores its number of subjects.
* `subject_patterns.dat` and `.meta`: the map from subject id to pattern id, sorted by
  subject. It is written with `PermWriter` as keys `(subject, pattern, 0, 0)`, so it reuses
  the block format, the block metadata in memory, the block cache and the scan code. The
  two zero columns compress to almost nothing.

Patterns are computed over the default graph and, when the generation has named graphs,
again over the union of all graphs. A second set of files (`patterns-all.dat`,
`subject_patterns-all.*`) holds the union. A generation with only the default graph
writes one set. Classes are not part of a pattern, unlike a characteristic set, because
`?s a <C>` is a join with POS that the table does not need to answer.

### 3.2 Building

`StatsCollector` already sees each subject's predicates in SPO order in `charset_done`.
A `PatternCollector` beside it, fed by the same stream, keeps an `FxHashMap<Box<[u64]>,
u32>` from predicate set to pattern number and appends `(subject, pattern)` to a
temporary run. For the default-graph table it skips keys whose graph column is not the
default graph. Unlike the characteristic sets, every pattern is kept. On DBpedia-scale
data the number of distinct patterns is expected in the millions. The map's memory is
charged to the builder's `hot_key_bytes`-style budget, a new `BuildOptions::pattern_bytes`
(default 1 GiB). When the map exceeds it, patterns are not built for that generation, the
reason is logged and stored in the metadata, and queries fall back.

Patterns are renumbered by decreasing subject count after the stream ends, and the
`(subject, pattern)` run is rewritten with the new numbers, so frequent patterns have
small ids that compress well.

### 3.3 The delta and partial compaction

The table describes the base. A snapshot with pending changes corrects it per subject:

* The delta's inserted and deleted keys in SPO give the subjects it touches. For each, the
  operator computes the subject's current predicate set from the base's SPO scan and the
  delta, and uses that instead of its base pattern.
* `PatternCount` subtracts the touched subjects' base patterns from the counts and adds
  their current sets. Its cost grows with the touched subjects, which compaction bounds.

A partial compaction (C13 §10.2) already recomputes the characteristic set of each
subject it touches. It updates the pattern table in the same pass: new patterns are
appended with the next numbers, the touched subjects' entries in `subject_patterns` are
rewritten in their blocks, and the pattern subject counts change. Patterns that lose their
last subject stay until the next full build, like unused vocabulary terms.

### 3.4 Operators

* **`PatternScan(?s, ?p)`** reads `subject_patterns` (all of it, or a range when `?s` is
  bound) and expands each subject's pattern to rows. With `?p` bound it keeps the
  subjects whose pattern holds `?p`, after computing the set of matching pattern ids once.
* **`PatternCount(input, ?s) → (?p, ?n)`** takes an input with a subject column, probes
  each distinct subject's pattern id (a merge with `subject_patterns` when the input is
  sorted on `?s`, a lookup otherwise), counts subjects per pattern id, and then adds each
  pattern's count to each of its predicates. QLever's paper describes this trick: the
  work is one lookup per subject plus one addition per pattern and predicate, instead of
  a scan of every triple of every subject.
* With no input (all subjects), `PatternCount` reads the per-pattern subject counts
  directly and touches no subject.

The rewrite recognizes, in the planner (`sparql/plan.rs`, near the group-count rewrites
that `exec.rs::group_count_scan` serves), a GROUP BY on `?p` over a join of an input
binding `?s` with `?s ?p ?o` or `?s spk:hasPredicate ?p`, where `?o` is otherwise unused,
and an aggregate that is `COUNT(DISTINCT ?s)` (or `COUNT(?s)` over `spk:hasPredicate`).
DISTINCT over `?p` with a bound or input-driven `?s` qualifies too. The rewrite does not
apply when the active graph is a set of named graphs other than the default graph or
their union, under C12 graph restrictions that do not cover every graph, or under a C12b
mask. Those queries run as today.

### 3.5 Cost

The table costs a few bytes per subject and the pattern list. The build reports both,
and the gate (§8) requires the total to stay under 3% of the index on DBpedia and 10.5M.

## 4. Vocabulary codecs (Phase 2)

### 4.1 FSST suffixes

FSST replaces frequent substrings of up to 8 bytes with one-byte codes from a table of at
most 255 symbols, trained on a sample, and decodes each string independently. Sparkles
keeps front coding, which removes the prefixes that neighbouring keys share, and encodes
each block's suffixes with FSST.

* Two symbol tables per vocabulary, one for literal keys and one for IRI and triple-term
  keys, since the two differ in content. The id boundary between them is `first_triple`.
  The tables are trained during the vocabulary merge (`builder/vocabmerge.rs`) on a sample
  of every `n`th key, and stored in `vocab.fsst`.
* A front-coded block stores its first key uncompressed, as now, so the sparse index and
  binary search over blocks decode nothing. The other 15 keys store their shared-prefix
  length and their FSST-encoded suffix.
* Decoding a key decodes the block's suffixes up to it, as front coding already requires.
  Lookup by term compares the probe with the block's first keys, then decodes the one
  block.
* The numeric column, the sparse index and the offsets are unchanged.

The delta vocabulary (`vocab/delta.rs`) stays uncompressed. It is small and append-only.

### 4.2 Encoded IRIs

Many large datasets use IRIs that are a fixed prefix followed by a decimal number, such
as Wikidata's entity IRIs. Such an IRI can live in the id instead of the vocabulary:

* A new tag, `Tag::EncodedIri` (12), whose payload is an 8-bit prefix index and a 52-bit
  number. The prefix table, of at most 255 prefixes, is stored in `meta.json`.
* An IRI is encoded when it is a table prefix followed by the canonical decimal form of
  a number below 2^52 (no leading zero, unless the number is zero). Any other IRI with the
  prefix goes to the vocabulary, so the mapping stays bijective, as for inline literals
  (`id.rs`).
* With `--iri-encoding auto`, the build counts, during parsing, the IRIs that end in
  digits by their prefix up to the last non-digit, and takes the prefixes that cover at
  least 1% of the distinct IRIs, at most 255.
* Updates encode IRIs with the generation's table too, so they add no delta vocabulary
  terms. A compaction that changes the table is a full one.

Encoded IRIs change three assumptions of the read path, each of which Phase 2 handles:

* **Order.** Index order is raw id order. Encoded IRIs sort after all vocabulary terms,
  by prefix and then by number, which is not the IRIs' string order. ORDER BY already
  compares terms through the comparator, which decodes them, so its results do not
  change. Code that assumes that id order is string order inside the IRI segment, such
  as the prefix ranges of `STRSTARTS` and anchored `REGEX` (`keyfilter.rs`), treats the
  encoded segment separately: a whole prefix's ids qualify when the filter's prefix is a
  prefix of the table prefix, a range of numbers qualifies when the filter's prefix
  extends the table prefix by digits, and nothing else qualifies.
* **Decoding.** Every place that turns an id into a term (result writers, the bindings'
  term tables, `Ctx` decoding, the full-text index's keys) formats the prefix and the
  number. One function in `id.rs` does this, and the tag joins the existing dispatch.
* **Statistics.** Class items in characteristic sets take vocabulary ids only
  (`builder::class_item`). They gain the encoded tag, so classes with encoded IRIs keep
  their statistics.

### 4.3 What FSST and encoding cost

FSST adds a table lookup per encoded byte to every decode. Sparkles decodes terms for
every result row, for filters that read strings (`regex-iri`, `lang-filter`), for
`distinct-obj`-style queries and for exports. The X01 plan named these queries for the
measurement, and the gate below adds the 500,000-row export.

### 4.4 Gate

On DBpedia and on 10.5M, warm and cold, FSST becomes the default when `vocab.dat`
shrinks by at least 25%, the suite's warm queries regress by at most 5% and cold by at most
15% (X01's thresholds), and the export by at most 5%. IRI encoding becomes `auto` by
default when it shrinks the vocabulary by at least 10% on DBpedia with the same limits.
Otherwise each stays an option, documented for disk-bound deployments.

### 4.5 Notes for the build (2026-10-23)

These notes settle what §4.1 and §7 left open, after measuring where the bytes go. On
the DBpedia 50M slice, `vocab.dat` takes 485 MB of the 1.51 GB index, with 239 MB of
front-coded literals and 246 MB of IRIs. At 10.5M synthetic triples it takes 25.9 MB of
306 MB, and the permutations take the rest. FSST as §4.1 describes it, with each block's
first key plain, shrinks the DBpedia slice's `vocab.dat` to 343 MB and the 10.5M one to
17.7 MB, by 29% and 32%. Coding the first keys as well would reach 36% and 45%, but the
binary search would then decode every key it compares, so they stay plain.

* Format 3 is the segmented layout of X07 Phase 3, so the version that §7.1 calls 3 is
  4 here. A generation with a feature is written as format 4, with
  the `features` list in `meta.json`, and one without stays format 3. A release that
  reads formats 2 and 3 checks only `format_version`, so it refuses an FSST generation as
  newer and still opens a default build.
* An entry after the first of its block stores its length shifted left by
  one, with the low bit set when its bytes are FSST codes. A suffix whose codes would not
  be shorter is stored plain, so no key grows. The 10.5M IRIs show why this matters,
  since their suffixes after front coding are digits that FSST cannot shorten.
* `vocab.fsst` holds the id where the second table starts, so a reader does
  not have to find the first triple-term key before it can decode. The tables are
  trained on the suffixes of keys read from about 64 blocks of the partial vocabularies
  per kind of key. A suffix there is taken against the previous key of the same batch,
  and a table trained that way coded the merged vocabularies of both datasets within
  0.5% of one trained on their true suffixes.
* The writer collects 65,536 keys, or 64 MiB of suffixes at the end of a
  block, and codes their blocks in parallel, since coding in the merge's one appending
  thread would add about 3 s to the DBpedia slice's load.
* `--vocab-codec front|fsst` sets the codec of new generations.
  Without it, a new database is front-coded and a compaction keeps the codec of the
  generation it replaces. A partial compaction copies `vocab.fsst` with the vocabulary,
  and a compaction asked for another codec is a full one. §2.2's dataset setting is not
  built.
* No IRI prefix up to the last non-digit covers 1% of the IRIs of the
  DBpedia slice. The most common, `http://books.google.com/books%3Fvid=ISBN`, covers
  0.13%. `--iri-encoding auto` would therefore encode nothing there, and §4.4's gate of a
  10% smaller vocabulary on DBpedia cannot pass. At 10.5M three prefixes cover 1,520,000
  of the 1,520,036 IRIs. Phase 2b is not built, and the [Outcome](#phases-not-built) says
  what it would serve.

## 5. Index column codecs (Phase 3)

`encode_column` takes deltas across the whole column. That suits the leading column of a
sorted permutation, where most deltas are 0 or small. For the second and third columns
the delta crosses each boundary of the leading key, where the value jumps, and the graph
column is mostly one value.

* **`pfor`.** Each column of a block is cut into frames of 128 values. A frame stores its
  minimum and a bit width, and packs each value minus the minimum in that width, with
  exceptions for outliers (patched frame of reference, after Lemire and Boytsov). The
  second column takes deltas only within a run of equal leading values and restarts at
  each run. A column that holds one value is stored as that value. Decoding unpacks a
  frame with fixed-width shifts, which the compiler vectorizes.
* **`zstd`** replaces LZ4 after the varint step, as X01 §U7 specified, with an optional
  dictionary per permutation of about 64 KiB trained on 1,000 sample blocks
  (`<perm>.dict`). Each thread keeps a decompressor with the dictionary loaded.

`BlockMeta` gains nothing. The codec is per generation, in `IndexMeta`. Decoding stays
column by column, so selective decoding and the block cache are unchanged.

The gate is X01's: a codec becomes the default only if it costs at most 5% on the warm
suite and 15% cold while saving at least 25% of the index. Sparkles' index is already
smaller than QLever's on DBpedia (28.7 against 33.4 GiB) and close at 10.5M (286 against
276 MiB), so a codec that saves disk at a cost in speed stays an option.

## 6. Architecture

| Part | Location | Change |
|---|---|---|
| `PatternCollector`, pattern files | `crates/sparkles-core/src/builder.rs`, new `builder/patterns.rs` | Built beside `StatsCollector` from the SPO stream |
| `Patterns` reader | new `crates/sparkles-core/src/patterns.rs`, `store.rs` (`Generation`) | Opened with the generation, absent for older generations |
| Partial compaction | `crates/sparkles-core/src/store/partial.rs` | Rewrites touched subjects' entries |
| `PatternScan`, `PatternCount` | `sparql/plan.rs` (`Kind`), `exec.rs`, `cursor/` | Planner rewrite and operators, eager and cursor |
| `spk:hasPredicate` | `sparql/propertyext.rs` or `arqpf.rs` | Property with an index fallback |
| FSST | `crates/sparkles-core/src/vocab.rs`, new `vocab/fsst.rs`, `builder/vocabmerge.rs` | Tables, encode and decode |
| `Tag::EncodedIri` | `crates/sparkles-core/src/id.rs`, `vocab.rs`, `sparql/keyfilter.rs`, writers, FFI term tables | Encoding, decoding, prefix filters |
| Column codecs | `crates/sparkles-core/src/index.rs` (`encode_column`, `decode_column`), `builder/codec.rs` | Dispatch on the generation's codec |
| Metadata | `builder.rs` (`IndexMeta`, `FORMAT_VERSION`), `store.rs` (`Generation::open_with`), `store/backup.rs` | Features and version (§7) |
| Bindings | `sparkles-ffi`, `sparkles-py`, `sparkles-node` | Decode the new tag in term tables |

## 7. The on-disk format

### 7.1 Versions and features

`IndexMeta` gains `features: Vec<String>` and the fields each feature needs:
`iri_prefixes`, `vocab_codec`, `block_codec`, and `patterns` (whether the files exist and
over which graphs). The format version becomes 3 for any generation that has a feature a
version-2 reader cannot read:

| Feature | Readers of version 2 | Version |
|---|---|---|
| `patterns` | Ignore unknown files and would answer correctly without them | 2 |
| `fsst-vocab` | Cannot read the vocabulary | 3 |
| `iri-encoding` | Cannot decode ids with tag 12 | 3 |
| `pfor-blocks`, `zstd-blocks` | Cannot decode blocks | 3 |

A generation with patterns only stays version 2, and an older binary ignores the files.
A new binary reads versions 2 and 3. `Generation::open_with` accepts both, and refuses a
version-3 generation that lists a feature it does not know, naming the feature, instead
of the present message that tells the user to dump and reload.

### 7.2 Writing

A build writes version 3 only when an option asks for a version-3 feature. Until a gate
makes one the default, every default build stays version 2, so data written by a new
binary with default options stays readable by the current release. When a default
changes, the release notes say which version new generations have. The policy for how
long old versions stay readable is X12's.

### 7.3 Checks, backups and branches

* `sparkles check` verifies the pattern table against SPO (every subject's pattern equals
  its predicate set), the FSST tables (every key decodes and re-encodes to the same
  bytes) and the IRI encoding (every encoded IRI's text is not also in the vocabulary).
* Backups record `index_format` (`store/backup.rs`) and the features. A restore into a
  binary that lacks a feature fails before writing anything.
* Branches linked to a generation (F09) share its files, and a branch's own generation
  takes the dataset's settings at its build.

## 8. Phases

| Phase | Deliverable | Gate |
|---|---|---|
| 1 | Patterns: files, build, delta correction, partial compaction, operators, `spk:hasPredicate`, C02 use | A1–A6. Size under 3% of the index. `predicate-counts`-style and class-profile queries measured on DBpedia and 10.5M. |
| 2a | FSST suffixes | A7, A8 and §4.4 |
| 2b | Encoded IRIs | A9, A10 and §4.4 |
| 3 | `pfor` and `zstd` blocks, dictionaries | A11 and §5's gate |

## 9. Acceptance examples

* **A1 — Pattern listing.** On a fixture where films have subsets of five predicates,
  `SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s a :Film . ?s ?p ?o } GROUP BY ?p`
  returns the same rows with patterns on and off, and the plan shows `PatternCount`.
* **A2 — hasPredicate.** `?s spk:hasPredicate :p` and the QLever IRI return the subjects
  with `:p`, with and without the table, bound and unbound.
* **A3 — Graphs.** With triples of `:p` only in a named graph, a query over the default
  graph does not list `:p`, and one with the union default graph does.
* **A4 — Delta.** After inserting `:s :q 1` and deleting the last `:p` triple of `:s`, A1's
  query counts `:s` under `:q` and not under `:p` before any compaction, and after a full
  and a partial compaction.
* **A5 — Access.** Under a C12b protection hiding `:p`, A1's query does not use the table
  and does not list `:p`.
* **A6 — Budget.** With `pattern_bytes` set to 1 KiB, the build skips patterns, logs the
  reason, and queries answer the same.
* **A7 — FSST round trip.** Every key of the 10.5M vocabulary and of DBpedia decodes to its
  input bytes, and lookups by term return the same ids as with `front`.
* **A8 — FSST performance.** §4.4's gate figures, reported in BENCHMARKS.
* **A9 — Encoded IRIs.** With the prefix `http://www.wikidata.org/entity/Q`, `wd:Q42` gets
  tag 12, `<http://www.wikidata.org/entity/Q042>` stays in the vocabulary, both round-trip
  through every result format, and `ORDER BY ?x` over a mix returns the string order.
* **A10 — Prefix filters.** `FILTER(STRSTARTS(STR(?x), "http://www.wikidata.org/entity/Q4"))`
  returns `Q4`, `Q42` and `Q4000` and no other, against a run with encoding off.
* **A11 — Column codecs.** Every permutation decodes to the same keys under `lz4`, `pfor`
  and `zstd` with and without dictionaries, and §5's gate figures are reported.
* **A12 — Versions.** A version-2 generation opens in the new binary. A version-3
  generation with an unknown feature is refused with that feature's name. A generation
  with patterns only opens in the previous release.

## 10. Testing and measurement

* Unit tests for the pattern collector, the FSST encoder and decoder over random and
  adversarial strings (empty, all 255 byte values, keys longer than a block), the IRI
  encoder's bijectivity and the column codecs over random and sorted columns.
* The W3C suites and X05's differential test run with each feature on.
* Measurements on 1.05M, 10.5M and DBpedia: index size per file, build time and peak
  memory, and the warm and cold suites, with A/B/B/A runs as BENCHMARKS describes.

## 11. Rejected alternatives

* **Patterns as triples in the main permutations**, with a special predicate. They would
  appear in every scan with an unbound predicate and need filtering everywhere.
* **Characteristic sets as the pattern table.** The statistics keep only the 10,000 most
  common sets, and they include classes. The table needs every subject's exact set.
* **zstd for the whole vocabulary block.** Random access needs the whole block decoded,
  and FSST decodes a single suffix at a time with a shared table. X01 asked for the
  comparison, which Phase 2 measures before choosing.
* **Order-preserving dictionary compression** (so that compressed keys compare like
  plain ones). It compresses less than FSST and is not needed, since the first key of
  each block stays plain.
* **Encoding IRIs without a prefix table**, by hashing. It breaks the bijection between
  ids and terms that `sameTerm` relies on.

## 12. Open questions

1. Should `?s ?p ?o` with `COUNT(*)` per predicate use the table when every subject has
   exactly one triple per predicate in its pattern? The table would need a flag per
   pattern.
2. Should the delta keep its own small pattern table so that correction does not scan
   touched subjects in the base?
3. Should IRI encoding cover other numeric forms, such as zero-padded numbers with a fixed
   width?

## Outcome

Phase 1 is built. Phase 2a is built as the option `--vocab-codec fsst`, which stays off
by default because the warm queries that decode many terms miss §4.4's gate. Phase 2b,
Phase 3 and open question 1 are not built. [Phases not built](#phases-not-built) gives
what each would save on the measured data, and they remain under [ROADMAP](../ROADMAP.md)
item 16.

### What was built

Every bulk load, full compaction and partial compaction writes `patterns.bin` into the
generation directory before `meta.json`, so the patterns are published with the generation
they describe and a crash leaves either both or neither. The file is a derived kind in the
format registry with the id `patterns` and version 1. A reader refuses any other version
with a message that names it, leaves the patterns unused and reads the permutations, and
the next full compaction writes the file again. `sparkles check` reports such a file as a
warning, a damaged file as an error, and in full mode compares every subject's pattern and
every count with SPO. `meta.json` records the number of patterns and subjects per table,
the file's size and, when a build skipped the patterns, the reason. The index format
version stays 2, as §7.1 allows, and a release without patterns ignores the file.

The collector reads the SPO stream that the statistics already read. Its memory is charged
to `BuildOptions::pattern_bytes`, which defaults to 1 GiB and which `--pattern-memory-mb`
sets. A build that would exceed it writes no patterns and records why. `--no-patterns`
turns the patterns off. A partial compaction rewrites the file in parallel with the
statistics. It appends new patterns, changes the counts of the subjects it touches and
copies the rest of the subjects, so its cost is a pass over the old file and not over SPO.

The planner answers `GROUP BY ?p` with `COUNT(DISTINCT ?s)` over `?s ?p ?o`, and
`COUNT(?s)` or `COUNT(*)` over `?s spk:hasPredicate ?p`, with a `PatternCount` operator
when the rest of the group binds `?s` or when the group is the triple alone.
`SELECT DISTINCT ?p` over the same shape uses `PatternCount` without counts, and
`spk:hasPredicate` and `ql:has-predicate` with a variable predicate run as `PatternScan`.
With no input, `PatternCount` reads per-predicate subject totals stored in the file and
touches no subject. `PatternScan` admits its output against the query's memory budget
as it grows and stops early under an eager LIMIT. A streaming cursor runs `PatternCount`
as a native operator, which reads its input before its first row as a sort does, and
plans `spk:hasPredicate` pairs as the streaming distinct scan of the permutations.
C02's class profiles take the union of the patterns of the profiled
instances and skip the PSO pass for predicates outside it.

Under MVCC the patterns describe the base generation. A snapshot's delta gives the
subjects it touches. Their predicate sets are read again from the snapshot's SPO, which
covers the delta, a transaction's own writes and a linked branch's base delta, and they
replace the subjects' base patterns in the counts. The correction is cached per snapshot
in the count cache. History reads at a commit within the retained generations use that
snapshot's delta in the same way. Differential tests compare every rewritten shape with
the generic operators on the delta, an open transaction, a partial and a full compaction,
a reopened store, a branch, history reads, a C12b protection, a store built without
patterns, a pattern budget of 1 KiB and random stores with random deltas. The
triple-protection differential suite includes the shapes.

### Departures from the design

* The patterns are one file, `patterns.bin`, with their own block layout of 1,024
  subjects per block, a checksum and a footer. §3.1 proposed writing the subject map with
  `PermWriter`. Another change was replacing the permutation block format at the same
  time, and a separate format keeps the patterns independent of it.
* The second table covers the union of the named graphs, without the default graph,
  because that is what Sparkles' union default graph and `GRAPH <urn:x-arq:UnionGraph>`
  read. §3.1 said the union of all graphs. Both tables are always written.
* The permutations answer instead of the patterns under a protection of triples, with
  RDFS on read, for a set of named graphs, for the union of the named graphs under a C12
  graph view, and when the delta touches more than one subject per 32 quads of the snapshot,
  with a floor of 1,024 subjects and a ceiling of one million. §3.3 corrected any delta.
* A constant predicate in `spk:hasPredicate` reads the predicate's subjects from PSO,
  because that range is smaller than a walk over every subject's pattern. On the DBpedia
  slice the walk took 43 ms for `dbo:birthDate`.
* `SELECT DISTINCT ?p WHERE { <s> ?p ?o }` keeps its SPO range scan, which reads one
  subject's triples and needs no table.
* §7.1's `features` list in `IndexMeta` is not added, since patterns alone keep version 2.
  Phase 2 adds it with the first feature that needs version 3.
* Open question 1 is not built, and [Phases not built](#phases-not-built) gives why.
  `COUNT(*)` per predicate over `?s ?p ?o` counts triples and reads the permutations.

### Measurements

On the DBpedia 50M slice on forge, with 41,127,700 quads, `patterns.bin` takes 24.1 MB,
1.6% of the 1.53 GB index. Writing it took 0.36 s of a 26.0 s load, and the load's wall
time and peak RSS (3.23 GB) did not change measurably. The pattern queries below are
medians of 40 requests of the server's execution time, from two rounds of A, B, B, A
server processes, with the commit before the patterns as the baseline.

| Query | Before | After |
|---|---|---|
| `COUNT(DISTINCT ?s)` per predicate over every subject | 1,915 ms | 1.3 ms |
| The same for `dbo:Person` instances | 264 ms | 21 ms |
| The same for `dbo:Film` instances | 24 ms | 11.7 ms |
| `SELECT DISTINCT ?p` of `dbo:Film` instances | 17.4 ms | 10.8 ms |

`COUNT(?s)` over `?s spk:hasPredicate dbo:birthDate` takes 2.0 ms from PSO. The class
profiles endpoint took 24.2 s with and without the patterns on this slice, because almost
every predicate is used by an instance of some class, so the patterns skip almost no PSO
pass. The other 31 queries of the DBpedia suite returned the same answers, and none of
those above 0.3 ms was more than 5% slower.

At 10.5M synthetic triples the file takes 2.9 MiB, 1.0% of the index, with 24 patterns
for 1.52 million subjects, so both datasets meet the gate of 3%. On forge
`predicate-subjects` took 0.020 ms against 469 ms, `class-predicates` 10.2 ms against
508 ms and `class-predicate-list` 10.2 ms against 73 ms. The load peaked at 2.07 GB
against 1.96 GB, in one load each.

### Phase 2a, FSST suffixes

A bulk load or a full compaction with `--vocab-codec fsst` writes each front-coded
block's suffixes after the first key as FSST codes when the codes are shorter, with two
symbol tables in `vocab.fsst`, one for literals and one for IRIs and triple terms. §4.5
gives the entry format and how the tables are trained. The file is the primary kind
`vocab-fsst` version 1 in the format registry, with the magic `SPKFSST\0` and a checksum.
The vocabulary cannot be decoded without it, so a reader refuses a generation whose tables
are missing, damaged or of another version, and nothing rebuilds them. The generation is index format 4,
whose `meta.json` lists the feature `fsst-vocab`. This build refuses a generation that
lists a feature it does not know and names the feature, and a release that reads formats
2 and 3 refuses format 4 as newer. A new database built without the flag is front-coded
and written as format 3, so the default index is unchanged. A load into an FSST store or a
compaction of one keeps FSST and format 4 unless the flag asks for `front`.

The decoder reserves eight bytes per code, writes each symbol as one unaligned 8-byte
store and advances by the symbol's length. `sparkles
check` reads the tables, and in full mode checks that every coded suffix uses only codes
its table holds and decodes to strictly increasing keys. A partial compaction copies the
tables with the vocabulary, and a full compaction keeps the codec unless the flag asks
for another one. Backup manifests record the generation's features as `indexFeatures`,
and a restore refuses a manifest with a feature this build does not know. The migration
preflight checks the features and the table version before it plans anything.
`sparkles stats` names the codec.

Departures from §4 are in §4.5. In short, the format number is 4 because X07 took 3, the
second table starts at an id stored in `vocab.fsst`, the codec is chosen per build with a
global flag and inherited by compactions, and §2.2's dataset setting is not built.

### Phase 2a measurements

These figures come from forge on 2026-10-23, with the release binary of `e294b817` as
main and the branch at `1ed20337` loading with `--vocab-codec fsst`. Each variant loaded
both datasets twice in A, B, B, A order, and the query rounds followed in the same order.
Warm 10.5M cells are medians of four rounds of 15 requests on two server cores, warm
DBpedia cells medians of two rounds of 10, and cold cells medians of two rounds of five
`bench-regressions.py --cold` samples.

| Measurement | 10.5M front | 10.5M FSST | DBpedia 50M front | DBpedia 50M FSST |
|---|---:|---:|---:|---:|
| `vocab.dat` | 25.87 MB | 17.69 MB | 485.06 MB | 344.48 MB |
| Store on disk | 312.8 MB | 304.7 MB | 1,577.5 MB | 1,436.9 MB |
| Load time | 3.8 s | 3.8 s | 24.0 s | 24.9 s |
| Load peak RSS | 2.0 to 2.1 GB | 2.0 to 2.1 GB | 3.18 to 3.33 GB | 3.08 to 3.20 GB |

`vocab.dat` shrinks by 31.6% at 10.5M and 29.0% on the DBpedia slice, which meets §4.4's
25%. The whole store shrinks by 2.6% and 8.9%, because the permutations hold most of the
bytes. The load times are the means of two loads each, so the DBpedia load is about 4%
slower, and peak RSS did not change beyond the spread between loads.

Cold queries meet the gate's limit of 15%. Their geometric means over the queries at
0.3 ms or more are 0.966 at 10.5M and 0.977 on DBpedia, since reading fewer vocabulary
pages offsets the decoding. The export misses its own limit of 5% both warm and cold,
because cold `export-500k` takes 1.11 times as long as with front coding.

Warm queries miss it. At 10.5M the geometric mean is 1.018 over 23 queries, but the
queries that decode many strings are slower by more than 5%.

| Warm query | Main | FSST | Ratio |
|---|---:|---:|---:|
| `export-500k` at 10.5M | 161.7 ms | 180.6 ms | 1.117 |
| `contains` at 10.5M | 24.6 ms | 27.7 ms | 1.125 |
| `lang-filter` at 10.5M | 18.1 ms | 20.0 ms | 1.106 |
| `star-join` at 10.5M | 30.4 ms | 31.9 ms | 1.050 |
| `entity-summary-1` on DBpedia | 0.55 ms | 0.62 ms | 1.127 |
| `film-director-optional` on DBpedia | 0.85 ms | 0.96 ms | 1.125 |
| `geo-box` on DBpedia | 3.55 ms | 3.96 ms | 1.115 |

On DBpedia the geometric mean over 34 queries is 1.054. Several short queries there vary
by more than a factor of two between rounds, but `entity-facts-1`, `entity-facts-2` and
`predicate-counts` are 6% to 8% slower with tight ranges. A microbenchmark that decodes
every key in order measured the cost at 1.5 ns per key at 10.5M and 11 ns per key on
DBpedia, where literal suffixes are long. Unrolling the decode four codes at a time did
not help. FSST therefore stays an option for deployments bound by disk or page cache,
where the cold figures apply. Every query returned the same answers with both codecs.

### Phases not built

Roadmap item 108 follows up on two of these results. It looks for a faster FSST decode path that meets the warm and export gates, and it measures Phase 2b on a dataset where a few prefixes cover most IRIs.

#### Phase 2b, encoded IRIs

§4.5 records that no IRI prefix covers even 1% of the DBpedia
slice's IRIs, so `auto` would encode nothing there and §4.4's gate of 10% on DBpedia
cannot pass. Built as an option, the phase would serve datasets whose IRIs are mostly a
few prefixes followed by numbers, at the cost of a new id tag, string order across
encoded and plain IRIs, and prefix filters.

Roadmap item 108 measured what `auto` would save on four datasets where a few prefixes
dominate. `scripts/x11-iri-encoding.py` read each front-coded generation's keys, chose the prefixes as §4.2
does, and front-coded the keys it would not encode again with the writer's rules. Run on
the keys unchanged, the same script reproduced each `vocab.dat` to the byte.

| Dataset | Triples | Prefixes taken | IRIs encoded | `vocab.dat` before | After |
|---|---:|---:|---:|---:|---:|
| Synthetic, 1M people | 10.5M | 3 | 100% | 25.9 MB | 18.4 MB, 28.9% smaller |
| WatDiv, scale 100 | 10.9M | 5 | 98.7% | 41.6 MB | 38.5 MB, 7.4% smaller |
| Wikidata truthy, first 20M lines | 20.0M | 7 | 63.5% | 104.9 MB | 99.8 MB, 4.9% smaller |
| Wikidata lexemes, first 20M lines | 20.0M | 2 | 8.6% | 68.0 MB | 66.5 MB, 2.1% smaller |
| DBpedia 50M slice | 41.1M | 0 | 0% | 485.1 MB | unchanged |

Only the synthetic data would pass the gate of 10%. On WatDiv and Wikidata almost every
entity IRI is encodable, but literals hold most of the vocabulary's bytes. In the
Wikidata truthy slice IRIs are 15% of the 8.2 million keys, since labels and
descriptions come in many languages, and the IRIs that front coding already stores as a
few digits after a shared prefix save little more when encoded. Lexemes name their forms
and senses `L123-F1` and `L123-S2`, so their prefixes up to the last non-digit are one
per lexeme and none but the two entity prefixes reaches 1%. Phase 2b therefore stays
unbuilt.

#### Phase 3, column codecs

The probe that
[X01](X01-compression-codecs.md#index-codecs-measured-2026-10-23) describes re-encoded
every frame of the permutations. zstd at level 3 saves 21% of the permutations at 10.5M
and 17% on DBpedia, so the whole index would shrink by about 19% and 12% with the
vocabulary, short of §5's 25% before any query cost. `pfor` without LZ4 took 2.6 to 2.7
times LZ4's bytes, and restarting the second column's deltas at each run of the first made
LZ4's frames 18% to 23% larger.

#### Open question 1, `COUNT(*)` per predicate

A flag per pattern, set when each of its subjects has exactly one triple per predicate,
would let `PatternCount` answer `COUNT(*)` for subjects whose patterns all carry it. It
would be sound, but it would answer almost nothing on DBpedia. None of the 75,648
`dbo:Person` subjects and none of the 5,959 `dbo:Film` subjects has a flagged pattern,
because `rdf:type` and `dbo:wikiPageWikiLink` take many values, and 2,495,282 of 3,222,514
subjects overall have some predicate with more than one triple. At 10.5M, 603,571 of
1,520,018 subjects are single-valued, but other subjects with the same pattern are not, so
only 15 of the 24 patterns, with 311,843 subjects, could carry the flag. The all-subject
`predicate-counts` query already takes 5 ms on DBpedia, so the flag would mainly help
counts over a class whose instances are single-valued.
