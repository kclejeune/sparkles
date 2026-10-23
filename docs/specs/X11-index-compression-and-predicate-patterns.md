# X11: Index compression and predicate patterns

> **Status:** Phase 1 built. Phases 2 and 3 are not built.
>
> **Phases:** Phase 1 is predicate patterns: a per-subject pattern table built with the
> generation, `spk:hasPredicate`, and the planner's use of patterns for predicate listings
> and counts. Phase 2 adds the vocabulary codecs: FSST-compressed suffixes and encoded
> IRIs. Phase 3 adds index column codecs and optional zstd blocks with dictionaries. Each
> phase that changes the files raises the index format version under the rules of §7.
>
> **User docs:** [Predicate patterns and `spk:hasPredicate`](../API.md#predicate-patterns-and-spkhaspredicate),
> the `--no-patterns` and `--pattern-memory-mb` flags and `sparkles stats --patterns` in
> [USAGE.md](../USAGE.md), and the `predicate_patterns` flag in
> [OPTIMIZATIONS.md](../internals/OPTIMIZATIONS.md).
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how Phase 1 landed and where it departs from the design.

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

Phase 1 is built. Phases 2 and 3 are not, and they remain under [ROADMAP](../ROADMAP.md)
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
* Open question 1 stays open. `COUNT(*)` per predicate over `?s ?p ?o` counts triples and
  reads the permutations.

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
