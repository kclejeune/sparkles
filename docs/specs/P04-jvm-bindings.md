# P04: JVM bindings behind Apache Jena's API

> **Status:** implemented in part (integration and primary administration)
>
> **Phases:** The engine/Jena contract surface, Fuseki assembler, historical reads,
> dumps, native DESCRIBE and refined ARQ fallback are implemented. Typed catalog,
> branches, backups, settings, history, indexes, stored queries, schema, GraphQL,
> reasoning and validation extend the SDK. Release workflows assemble three native
> platforms (Linux on x86_64 and arm64, macOS on Apple silicon), classifier jars, a Fuseki bundle and source/documentation artifacts;
> Linux is validated locally. Administration helpers are implemented; batched fallback graph patterns,
> Java callbacks, musl and comparative performance targets remain follow-up work.
> The [Outcome](#outcome) records the supported scope and deferrals.
>
> **User docs:** [Usage: JVM](../USAGE.md#jvm-apache-jena) ·
> [Features](../FEATURES.md#known-gaps) · [Development](../DEVELOPMENT.md#jvm-bindings)

This spec draws on the Sparkles code, Apache Jena's source (Apache-2.0, the 6.3.0-SNAPSHOT
checkout and the release notes of 5.0 to 6.2), the UniFFI user guide and its Kotlin
templates (MPL-2.0), the JNA source and documentation, the loaders of sqlite-jdbc and
RocksDB's Java binding, the Kotlin and Gradle documentation, and JEP 454 and JEP 472. The
[Sources](#12-sources) section lists them.

The bindings depend on these parts of Sparkles:

* the embedded API in `crates/sparkles/src/dataset.rs` (`Dataset`, `Transaction`,
  `QuadIter`, `Dataset::quads`, `Transaction::insert_linked` and `query_with`);
* `sparkles::sparql::{query, QueryOptions, QueryResult}`, `sparkles::sparql::update` and
  the extension catalog in `sparkles::sparql::catalog`;
* `sparkles::store` for snapshots, the writer lock, `snapshot_at` and receipts, and
  `sparkles::history::At`;
* the Python bindings of [P01](P01-python-bindings.md), whose surface, error mapping and
  transaction worker this spec follows.

It closes the known gap "Embedding" in [FEATURES.md](../FEATURES.md#known-gaps) for the
JVM, where the database has Rust and Python APIs only.

## 1. Summary, goals, non-goals

Most RDF applications on the JVM are written against Apache Jena. They hold a `Dataset`
or a `Model`, run queries through `QueryExecution`, and deploy with Fuseki and an
assembler file. Such a program can use Sparkles today only over HTTP, through
`RDFConnectionFuseki`, which passes the client suite in `testsuite/jena-clients`. This
spec embeds the engine in the JVM process behind Jena's own interfaces, so that code
written for TDB2 runs on Sparkles after a change to the line that opens the dataset.

The work has three layers. A Rust crate, `sparkles-ffi`, exposes the engine through
UniFFI. UniFFI generates Kotlin bindings for it, which call the native library through
JNA. A Kotlin library, `sparkles-jena`, implements `DatasetGraph`, `Transactional`, a
`QueryEngineFactory`, an `UpdateEngineFactory` and an assembler on top of the generated
bindings, and gives Java programs a small, conventional API.

**Goals**

* **Jena code runs unchanged.** A `DatasetGraph` from `SparklesDatasets.open(path)`
  passes Jena's contract tests for datasets, graphs and transactions, and the Model,
  RIOT, `Txn` and `QueryExecution` APIs work on it as they do on TDB2.
* **Whole queries run in Sparkles.** SPARQL queries and updates go to Sparkles' planner
  and executor in one call. Their solutions come back to Jena as `Binding`s in batches.
* **ARQ is the safety net.** A query that uses a function, property function, aggregate
  or SERVICE executor registered in Java, which Sparkles cannot see, runs in ARQ over the
  dataset's `find()`. The choice is made per query and logged.
* **Batches, not terms, cross the boundary.** Every call that moves RDF terms moves a
  batch of them in a compact binary encoding, so the cost of a JNA call is paid once per
  batch.
* **A Java-friendly surface.** Java callers see static factories, builders, overloads,
  nullability annotations, Jena's exception types and `AutoCloseable`. No coroutines,
  flows, sealed hierarchies or Kotlin function types appear in the public API.
* **Fuseki serves it.** An assembler type, `sparkles:DatasetSparkles`, lets a Fuseki
  `config.ttl` serve a Sparkles database with Fuseki's own HTTP layer.
* **One jar per use.** A Maven artifact carries the native libraries for Linux x86_64 and
  aarch64, macOS x86_64 and aarch64, and Windows x86_64, and loads the right one from the
  jar. Programs need no Rust toolchain and no system library.

**Non-goals**

* **The Sparkles server or CLI on the JVM.** `sparkles serve` stays a separate binary.
  Fuseki is the JVM's server for this binding.
* **Kotlin-first APIs.** The library is written in Kotlin and is usable from Kotlin, but
  it adds no coroutine, flow or DSL layer.
* **Android.** UniFFI supports Android, but Sparkles' storage, threads and memory use are
  sized for servers and desktops.
* **Reading TDB2's files from Rust.** Sparkles does not open a TDB2 database directory.
  Data moves through Jena's own reader, either in-process with
  `SparklesDatasets.importTdb2` or with `tdb2.tdbdump` and `sparkles load` as in
  [G08](G08-fuseki-configuration.md) (§4.6).
* **A remote client.** `RDFConnectionFuseki` already covers that (§10).

## 2. Layers and build

### 2.1 Layout

```
crates/sparkles-ffi/            the cdylib crate, its own workspace and Cargo.lock
  Cargo.toml
  uniffi.toml                   Kotlin package, library name, cleaner settings
  src/lib.rs                    uniffi::setup_scaffolding!, the exported objects
  src/encode.rs                 the batch encoding of §2.4
  src/bin/uniffi-bindgen.rs     the bindings generator, built from the same lock
jvm/                            the Gradle build (settings.gradle.kts, wrapper, catalog)
  sparkles-jena/                the library
    src/main/kotlin/            the Jena integration and the public API
    src/main/java/              package-info.java files with @NullMarked
    src/test/kotlin/            Jena's contract tests and the library's own tests
    src/jmh/kotlin/             the benchmarks of §5.4
  sparkles-jena-natives/        the native libraries as jar resources
  sample-java/                  a Java 17 project built against the published API
```

The generated Kotlin goes to `jvm/sparkles-jena/build/generated/uniffi` at build time and
is not checked in. It lives in the package `io.github.kclejeune.sparkles.jena.internal.ffi`,
which is not part of the API (§4.2).

### 2.2 The `sparkles-ffi` crate

The crate follows `crates/sparkles-py` in staying out of the root workspace. It is listed
in the root's `exclude`, declares its own `[workspace]`, keeps its own `Cargo.lock`
copied from the root lock, and repeats the root's spargebra patch and profiles. P01 §2.2
gave the reasons, and one of them applies here with more force. UniFFI's bindings
generator brings in a template engine, an object-file reader and their dependencies,
which neither the server nor the root workspace's lint and test runs should compile. A
`mise run jvm:lock` task refreshes the lock, and the flake check fails when the lock no
longer matches.

The crate uses UniFFI's procedural macros (`uniffi::setup_scaffolding!`,
`#[uniffi::export]`, `#[derive(uniffi::Object)]`, `#[derive(uniffi::Record)]` and
`#[derive(uniffi::Error)]`) rather than a UDL file. The interface is then declared once,
next to the Rust code that implements it, and a test cannot drift from a second file.
UniFFI is pinned to the 0.32 series, which was current when this spec was written
(0.32.2, released on 2026-09-23). The generated Kotlin and the scaffolding must come from
the same UniFFI version, and the crate's `uniffi-bindgen` binary guarantees that because
it is built from the crate's own lock.

The cargo features have the server's names, as in P01: `reasoning`, `shacl`, `shex`,
`text` and `geo`, all on by default, with `zstd` and `brotli` always on. A build without a
feature still exports its functions, and they fail with `Unsupported` naming the feature.
The native library uses the system allocator, as the Python extension does. A
`#[global_allocator]` in a cdylib governs only the Rust allocations inside it, so
mimalloc would be safe, and Phase 3 measures it before choosing (open question 9).

The library is built with `panic = "unwind"`, because UniFFI catches a panic at the
boundary and turns it into an error on the Kotlin side. An abort would take down the JVM.

### 2.3 What crosses the boundary

The exported interface is small. Each object is an `Arc` on the Rust side and an
`AutoCloseable` Kotlin class on the other (§4.4).

| Object | Operations | Notes |
|---|---|---|
| `FfiDataset` | `open(path, options)`, `memory(options)`, `close`, `dataset_id`, `head_commit`, `commits`, `prefixes`, `set_prefix`, `remove_prefix`, `begin_read`, `begin_read_at(at)`, `begin_write`, `load_files`, `load_bytes`, `capabilities`, `version` | One per open database. `options` is a record with the union default graph flag, the blank node label mode and the store options. |
| `FfiReadTxn` | `commit_seq`, `find(pattern, first_rows)`, `contains(quad)`, `count(pattern)`, `graph_names`, `prepare_query(text, options)` | Holds an `Arc<Snapshot>`. Calls run on the caller's thread. |
| `FfiWriteTxn` | `base_seq`, `apply(batch)`, `find`, `contains`, `count`, `graph_names`, `remove_matching(pattern)`, `prepare_query`, `update(text, options)`, `commit`, `abort` | Owns a worker thread that holds the writer lock (§3.3). |
| `FfiQuery` | `cancel`, `execute(first_rows)`, `variables`, `kind`, `boolean`, `next_batch(max_rows)`, `timing`, `close` | A prepared query and, after `execute`, its result table. `cancel` works from any thread. |
| `FfiCursor` | `next_batch(max_rows)`, `close` | The rest of the quads of a `find`, read from one snapshot. |
| `FfiError` | an error enum with fields (§4.3) | Mapped to Jena's exceptions in Kotlin. |
| records | `CommitInfo`, `Receipt`, `UpdateStats`, `Timing`, `Capabilities` | Plain values, copied across. |

Phase 2 adds `dump_to_path`, `dump_cursor` (which hands out a dump in byte chunks),
`compact`, `backup` and `apply_patch` on `FfiDataset`. Phase 3 adds reasoning,
validation, index administration and history.

Small requests cross the boundary as few times as possible. `find` returns its first
batch together with the cursor, and the cursor is null when the first batch held every
match, so a lookup with a few matches is one call. `execute` likewise returns the first
batch of rows with the variables and the query kind, and frees the result table in the
same call when no rows remain. A one-row query is then two calls, `prepare_query` and
`execute`, and `prepare_query` does no work beyond copying the text and options, so the
two could be fused later without changing the engine (§5.4).

Nothing on the boundary is a single RDF term. Patterns, quads to write, quads read and
query solutions all travel as byte batches in the encoding of §2.4. A UniFFI record per
term would make a JNA call, a record serialization and several string copies per term,
which is the cost this design avoids.

The engine needs four additive APIs, which Rust embedders get too:

* a `union_default_graph` flag in `QueryOptions` that turns the union default graph on or
  off for one request (§3.7);
* `Dataset::quads_in(snapshot, pattern)`, the batched `QuadIter` of P01 over a given
  snapshot rather than the current one, for reads inside a read transaction;
* `Transaction::remove_matching(pattern)`, which removes every quad of a pattern in the
  transaction without sending the matches to the caller, for `deleteAny` and
  `removeGraph`;
* `sparkles::embed::TxnWorker`, the worker thread that owns a write transaction and runs
  the operations sent to it. It moves the design of P01's `txn.rs` into the core crate
  without its PyO3 types, so that both bindings can use it (open question 10).

The extension catalog that `sparkles::sparql::catalog` already keeps for the service
description gives the function, aggregate and property function IRIs of §3.5 through
`FfiDataset::capabilities`.

### 2.4 The batch encoding

Terms travel in one binary encoding in both directions. A string is a LEB128 length
followed by UTF-8 bytes. Integers are little-endian.

| Tag | Term | Payload |
|---|---|---|
| 0 | none (a wildcard in a pattern) | nothing |
| 1 | IRI | string |
| 2 | blank node | label string |
| 3 | `xsd:string` literal | lexical form |
| 4 | language-tagged string | lexical form, language tag |
| 5 | directional language-tagged string | lexical form, language tag, direction byte |
| 6 | literal of a common datatype | lexical form, one byte that names the datatype |
| 7 | literal of another datatype | lexical form, datatype IRI |
| 8 | triple term | three terms |
| 9 | the default graph | nothing |
| 10 | the union graph (`urn:x-arq:UnionGraph`) | nothing |
| 11 | a repeat of an earlier term of the same batch | its position, as LEB128 |

The datatype byte of tag 6 indexes a fixed table of 24 XSD and RDF datatypes, from
`xsd:integer` and `xsd:decimal` to `rdf:JSON`. Tag 11 lets the Kotlin side write a
batch of quads without repeating the IRIs that most quads share. The Kotlin encoder keeps
an identity map from `Node` objects to batch positions for this, which costs one hash
lookup per term.

**Rows out.** A query result or a `find` returns batches of rows. A batch starts with a
version byte and a flags byte, followed by the terms that appear in it for the first
time and then the rows:

```
u8   version
u8   flags          bit 0: the term table restarts at this batch
u32  new terms      then that many terms, in the encoding above
u32  rows
u16  columns
u32  cells[rows × columns]   0 = unbound, n = term n − 1 of the table
```

The Rust side gives each distinct term of a result a dense index the first time it sends
it, keyed by its 64-bit Sparkles id, and the Kotlin side keeps an `ArrayList<Node>` of
the decoded nodes. A term that repeats across rows or batches is decoded and allocated
once per result, which matters for the result shapes Sparkles is good at, such as joins
whose columns repeat the same IRIs. Ids are stable within one snapshot, and a result
reads one snapshot, so the table is valid for the whole result. When the table reaches
its limit (§5.2), the Rust side sets the restart flag, and both sides start a new table.

A `find` cursor uses the same format with four columns, the graph first. The default
graph is tag 9, so Jena gets `Quad.defaultGraphIRI` without a string comparison.

**Writes in.** `FfiWriteTxn::apply` takes a batch of operations. Each operation is one
byte (1 for an add, 2 for a delete) followed by four terms. UniFFI 0.32 documents
borrowed byte slices (`&[u8]`) as direct `ByteBuffer`s on the Kotlin side, so the Kotlin
library writes the batch into a pooled direct buffer and Rust reads it in place. Where the
generated code cannot pass a direct buffer, the batch is a `ByteArray` copied once.
Batches returned from Rust are a `Vec<u8>`, which UniFFI copies once into a `ByteArray`.

RDF Thrift and Apache Arrow were considered for this encoding and rejected in §10.

### 2.5 Generated Kotlin bindings

`uniffi.toml` sets the Kotlin package, `cdylib_name = "sparkles_ffi"`, and immutable
records. The generated code calls the library through JNA's direct mapping
(`Native.register`), which UniFFI has used since 0.30. It needs JNA and the Kotlin
standard library and nothing else, because the crate exports no async functions and the
coroutine support is generated only for them.

Generated objects implement `AutoCloseable`. `close()` frees the Rust object once every
call in flight on it has returned, and a Cleaner frees it when the Kotlin object is
garbage-collected without being closed. The library closes every native object it creates
explicitly (§4.4), so the Cleaner is a backstop and not the plan.

The generated exceptions extend `kotlin.Exception` and cannot be given another base class.
They never reach a caller. Every call into the generated code goes through one internal
helper that maps `FfiError` onto the exceptions of §4.3.

### 2.6 The `sparkles-jena` library

| Package | Contents |
|---|---|
| `io.github.kclejeune.sparkles.jena` | `SparklesDatasets`, `DatasetGraphSparkles`, `SparklesOptions`, `Sparkles` (context symbols and `init`), the enums, `CommitInfo`, `CommitReceipt`, the exception classes |
| `io.github.kclejeune.sparkles.jena.engine` | `QueryEngineSparkles`, `UpdateEngineSparkles`, the fallback detector |
| `io.github.kclejeune.sparkles.jena.assembler` | `VocabSparkles`, `DatasetAssemblerSparkles` |
| `io.github.kclejeune.sparkles.jena.internal` | the codec, cursors, the native loader, the generated bindings |

`InitSparkles` implements `JenaSubsystemLifecycle` and is listed in
`META-INF/services/org.apache.jena.sys.JenaSubsystemLifecycle`, as TDB2's `InitTDB2` is.
It registers the query and update engine factories and the assembler when Jena
initializes, at a level after ARQ and TDB2 (level 60). Its `start` does not load the
native library. The first dataset opened does that.

## 3. Jena integration

### 3.1 The dataset

`DatasetGraphSparkles` extends Jena's `DatasetGraphBaseFind`, the base class of
`DatasetGraphStorage` and therefore of TDB2's `DatasetGraphTDB`. It overrides `find`,
`findNG`, `contains`, `deleteAny`, `listGraphNodes`, `isEmpty` and `clear` wholesale,
because the base class splits a wildcard graph into a default-graph call and a
named-graph call, and Sparkles answers either in one scan.

* **`find(g, s, p, o)`** sends the pattern as one encoded batch and returns a
  prefetching iterator. The iterator holds the first chunk of matches, which `find`
  returns with the cursor, and fetches the next chunk only when the current one is used
  up. Chunks start at 64 quads and double up to 4096 (§5.2). A null or `Node.ANY` graph
  matches every graph including the default graph, `Quad.defaultGraphIRI` and
  `Quad.defaultGraphNodeGenerated` match the default graph, and `Quad.unionGraph`
  matches the union of the named graphs with duplicates removed. These are Jena's
  rules, and Sparkles' `Dataset::quads` has the same three cases.
* **`add` and `delete`** append to the thread's write buffer (§3.3). Adding to the union
  graph throws `AddDeniedException`, and deleting from it throws
  `DeleteDeniedException`, as `DatasetGraphStorage` does.
* **`deleteAny`** is one `remove_matching` call. The base class reads the matches into
  Java and deletes them in slices of 1000, which would send every match across twice.
* **`size()`** is the number of named graphs, which is what Jena's `DatasetGraph.size`
  means. `getDefaultGraph().size()` is a triple count that Sparkles reads from its index.
* **`listGraphNodes`**, **`containsGraph`**, **`addGraph`** and **`removeGraph`** follow
  TDB2. A graph exists while it has quads, so there are no empty graphs. `addGraph`
  copies the graph's triples in batches and its prefixes into the dataset's prefixes.
* **`supportsTransactions`** and **`supportsTransactionAbort`** return true.
* **`getContext`** returns the dataset's `Context`, where the symbols of §3.8 can be set
  for every query on it.
* **`close`** releases the handle (§4.4).

### 3.2 Graphs, prefixes and blank nodes

`getDefaultGraph`, `getGraph` and `getUnionGraph` return a `GraphViewSparkles`, a subclass
of Jena's `GraphView`, whose constructor is protected for this purpose. `GraphView` keeps
Jena's graph semantics and its event manager. The subclass overrides `graphBaseSize`,
`graphBaseContains` and `clear` with single native calls, and `graphBaseFind` with a
`find` on the graph that skips the conversion of quads to triples in a separate
iterator.

**Prefixes.** `prefixes()` returns a `PrefixMap` backed by the dataset's prefixes in
Sparkles, and every graph view shares it, as in TDB2 since Jena 4. Sparkles saves prefix
changes at once and outside any transaction, while TDB2 makes them part of the write
transaction. A prefix set in a transaction that aborts therefore stays. Open question 7
asks whether the engine should make prefixes transactional.

**Blank nodes.** In TDB2, a blank node keeps its label, so a `Node` created with
`NodeFactory.createBlankNode()` and added in one transaction matches the same stored node
in a later one. Sparkles hands out labels of the form `b<hex>` that name stored nodes,
and scopes any other label to the transaction that inserts it, as P01 §3.5 describes.
Jena code relies on the first behaviour, so the binding defines the mapping between
Sparkles' blank node ids and Jena's labels in two layers.

* **Stored nodes.** A stored blank node's id carries a payload that the store allocates
  from a persisted counter and never reuses. Its Jena label is `b` followed by the
  payload in lowercase hexadecimal without leading zeros, which is the label
  `sparkles::id::bnode_label` writes. The mapping is a bijection, and it is stable across
  transactions, compactions, restarts and processes for as long as the node is stored. A
  `Node_Blank` whose label has exactly this form names that stored node in every read
  and write. Blank nodes that a query mints, through `BNODE()` or a CONSTRUCT template,
  get labels of the form `q<hex>` and never name a stored node.
* **Labels that Jena made.** Any other label, such as the UUID-like labels of
  `NodeFactory.createBlankNode()` and Jena's parsers, is mapped through a table per open
  dataset in the FFI layer. When a write with such a label commits, the table records
  the stored node it became, in both directions. Later writes and patterns with the
  label reach the same node, and reads return the stored node with the label it was
  added with rather than its `b<hex>` label. A label written in a transaction that
  aborts leaves no entry.

The table is kept in memory for as long as the dataset is open and costs about 150 bytes
per distinct label. A program that writes millions of fresh blank nodes through
`add`, such as `RDFDataMgr.read` into the dataset, pays that in memory. The option
`SparklesOptions.blankNodeLabels(BlankNodeLabels.TRANSACTION)` drops the table and gives
Sparkles' own scoping, which is the better choice for loading data. The bulk loads of
§4.5 and the TDB2 import of §4.6 never add to the table. The table lives as long as the
open dataset, so after the dataset is reopened, stored blank nodes have their `b<hex>`
labels, while TDB2 would still report the labels they were added with. Open question 3
asks which mode should be the default.

### 3.3 Transactions

Sparkles has many readers and one writer, like TDB2. A reader works on an immutable
snapshot, and the writer works in a transaction that holds the store's writer lock. Jena's
`Transactional` maps onto it as follows. The transaction state is per thread, held in a
`ThreadLocal` of the dataset, as TDB2 holds it.

| Jena | Sparkles |
|---|---|
| `begin(READ)` | `begin_read` pins the head snapshot. |
| `begin(WRITE)` | `begin_write` starts a worker that waits for the writer lock and then begins a write transaction. `begin` blocks until the lock is free, as in TDB2. |
| `begin(READ_PROMOTE)`, `begin(READ_COMMITTED_PROMOTE)` | `begin_read`, remembering the snapshot's commit. |
| `promote()` in `READ_PROMOTE` (`Promote.ISOLATED`) | Returns false at once if a commit has landed since `begin`. Otherwise it takes the writer lock, waiting if another writer holds it, and checks the head again under the lock. It returns false and releases the lock if the head moved. This is TDB2's sequence in `TransactionCoordinator`. |
| `promote()` in `READ_COMMITTED_PROMOTE` | Takes the writer lock and continues as a write transaction on the newest commit. |
| `promote()` in `READ` | Returns false, and a write throws `JenaTransactionException`. |
| a write in `READ_PROMOTE` | Promotes first, as TDB2's `ensureWriteTxn` does, and throws `JenaTransactionException` when promotion fails. |
| `commit()` | Flushes the write buffer and commits. The receipt is kept for `lastReceipt()`. |
| `abort()` | Discards the buffer and the Sparkles transaction. |
| `end()` | Ends a read. On a write that neither committed nor aborted, it aborts and throws `JenaTransactionException`, as TDB2 does. |

**Promotion, precisely.** Let *r* be the sequence number of the commit that a
`READ_PROMOTE` transaction's snapshot shows, recorded at `begin`. Promotion succeeds if
and only if the dataset's head commit is still *r* at the moment the promoting thread
holds the writer lock. If the head is already past *r* when promotion starts, it fails
without waiting. Otherwise the thread waits for the writer lock, and once it holds the
lock no other commit can land, so the second check is final. On success the write
transaction starts from commit *r*, which is the state the transaction has been reading,
so no read it made is invalidated. On failure the transaction stays a read transaction
on *r*, `promote()` returns false, and a write that triggered the promotion throws
`JenaTransactionException`, as TDB2's `Transaction.ensureWriteTxn` does. Sparkles makes
no commit for a write transaction that changed nothing, so such a writer does not make
promotion fail. TDB2's data version counter counts every write transaction, so TDB2
refuses some promotions that Sparkles allows, and every promotion Sparkles allows reads
the same data that TDB2 would. In `READ_COMMITTED_PROMOTE`, promotion waits for the
lock and always succeeds, and the write transaction starts from the head, which may be
past *r*.

**The writer's thread.** The store's writer lock guard must stay on the OS thread that
took it, and Java threads, virtual threads above all, may run a transaction across
several carrier threads. Each write transaction therefore runs on its own native worker
thread, as in P01 §6. The Kotlin side sends it operations in batches and waits for the
answer. A round trip costs a thread hand-off, measured at 5 to 20 µs in the Python
bindings, which is why writes are buffered.

**The write buffer.** `add` and `delete` in a write transaction append to an ordered
buffer in Kotlin. The buffer goes to the worker as one `apply` batch when it holds 4096
operations, before any read in the same transaction, and at commit. Reads in a write
transaction (`find`, `contains`, queries) see the transaction's own changes, because the
buffer is flushed before they run.

**Iterators.** An iterator from `find` inside a transaction throws
`JenaTransactionException` when it is used after the transaction ends, as TDB2's
`IteratorTxnTracker` makes it. Outside a transaction, `find` pins the head snapshot for
the life of its iterator.

**Outside a transaction.** Reads outside a transaction each read the head snapshot.
Writes outside a transaction throw `JenaTransactionException`, which is what TDB2 does
in `StorageTDB.ensureWriteTxn`. Jena's in-memory dataset instead runs each write in a
transaction of its own. That would make one Sparkles commit per quad, which is slow
enough to be a trap. `SparklesOptions.autocommit(true)` turns it on for code written
against the in-memory dataset (open question 4).

**Failures inside a write transaction.** Sparkles has no savepoints. When a SPARQL Update
fails after it began to change data, the engine aborts the whole transaction, as P01's
Outcome records. The binding then marks the Jena transaction as aborted. Every later
operation in it throws, and `commit()` throws `JenaTransactionException`. TDB2 keeps the
partial changes in the transaction and leaves the decision to the caller, and this
difference is documented.

### 3.4 The query engine

`QueryEngineSparkles.factory` implements `QueryEngineFactory`. Jena tries the newest
registered factory first, and `QueryEngineFactoryWrapper` already unwraps plain
`DatasetGraphWrapper`s, so a Sparkles dataset inside a plain wrapper reaches the factory
too. Views such as `DatasetGraphFilteredView` are not unwrapped, by Jena's own rule, and
run in ARQ.

**`accept(query, dataset, context)`** returns true when the dataset is a
`DatasetGraphSparkles`, the fallback mode is not `ALWAYS`, and the detector of §3.5 finds
nothing in the query that only Java can evaluate.

**`create(query, dataset, binding, context)`** builds a `Plan` whose iterator is a
`QueryIterSparkles`. In Jena, the engine evaluates only the query pattern and its
solution modifiers, and `QueryExecDataset` builds the result of the query form from the
bindings. It applies the CONSTRUCT template itself, takes the first binding for ASK, and
runs the DESCRIBE handlers over the dataset. The plan therefore sends Sparkles a SELECT:

* a SELECT query as it is;
* a CONSTRUCT query as `SELECT` of the template's variables, keeping `ORDER BY`, `LIMIT`
  and `OFFSET`;
* an ASK query as `SELECT *` with `LIMIT 1`;
* a DESCRIBE query as `SELECT` of its variables over its pattern, with the DESCRIBE
  handlers running in Java over `find()` in Phase 1. Phase 2 adds a describe handler that
  asks Sparkles' DESCRIBE, with the dataset's DESCRIBE setting, for all resources in one
  call.

The text is produced with `Query.cloneQuery()` and `Query.serialize(Syntax.syntaxARQ)`,
which keeps `BASE`, `PREFIX`, `VALUES` and ARQ's syntax extensions. Sparkles parses
ARQ's `LATERAL`, path ranges and CONSTRUCT templates with `GRAPH`
([G06](G06-arq-query-extensions.md)).
The initial binding becomes `QueryOptions::initial_bindings`, and the context symbols of
§3.8 become the other options.

**Execution.** `QueryIterSparkles` prepares the query when it is created and executes it
when Jena first asks for a binding. Execution happens then, and not in `create`, because
Jena's first timeout counts the time to the first result from that point. The query runs
on the thread's snapshot, in the thread's write transaction when there is one, or on the
head snapshot outside a transaction. Sparkles evaluates a whole query before it returns
its first row, so the time to the first row equals the execution time. The iterator then
pulls row batches and turns each row into a `BindingSparkles`, a `Binding` that holds the
variables and an array of `Node`s and whose parent is the input binding.

**Cancellation.** `QueryExecDataset` handles timeouts and `abort()` by calling
`QueryIterator.cancel()` from another thread. `QueryIterSparkles.requestCancel` calls
`FfiQuery.cancel`, which sets the `cancel` flag of the query's `QueryOptions`. Sparkles
checks it while it runs, and the blocked execute call returns `Cancelled`, which Jena
reports as `QueryCancelledException`.

**`FROM` and `FROM NAMED`.** Sparkles evaluates them by selecting graphs of the dataset,
which is what Jena's `DynamicDatasets` does when a dataset is given. Neither engine reads
them from the web.

**The `Op` path.** `accept(op, …)` and `create(op, …)` serve callers that execute
algebra directly, such as `Algebra.exec`. Phase 1 declines them, so ARQ runs them. Phase 2
converts the `Op` with Jena's `OpAsQuery` and accepts it when the conversion succeeds.

### 3.5 Falling back to ARQ

When the factory declines a query, the next factory in Jena's registry takes it. That is
`QueryEngineMain`, which runs the query in ARQ over `find()`. The decision is made per
query, in `accept`, by a detector that walks the query's syntax tree with Jena's
`ElementWalker` and expression walker. The walk takes a few microseconds.

A query goes to ARQ when one of these holds:

1. **A Java function.** The query calls a function IRI that Jena's `FunctionRegistry`
   for the query's context resolves and Sparkles' catalog does not list, or a `java:`
   IRI, which ARQ loads as a class.
2. **A Java property function.** A triple pattern's predicate is registered in the
   context's `PropertyFunctionRegistry`, property functions are enabled, and Sparkles'
   catalog does not list the IRI.
3. **A Java aggregate.** A custom aggregate's IRI is registered in Jena's
   `AggregateRegistry` and not in Sparkles' catalog.
4. **A Java SERVICE executor.** The query has a SERVICE clause and the context's
   `ServiceExecutorRegistry` holds executors beyond Jena's default ones. Sparkles' own
   SERVICE IRIs, such as `path:search` ([F07](F07-path-search.md)), never cause this.
5. **A custom ARQ engine hook.** The context sets `ARQ.stageGenerator` or the
   `OpExecutorFactory` to something other than ARQ's defaults. A program that installs
   these expects its code to see the query's patterns.
6. **The fallback mode is `ALWAYS`.**

An IRI that both Sparkles' catalog and Jena's registry know, such as `afn:localname`,
`geof:distance` or `text:query`, runs in Sparkles. Phase 2 refines this. It records the
factory registered for each IRI when `InitSparkles` starts, and counts an IRI as
overridden by the program when its factory is later replaced by a class outside the
`org.apache.jena` packages. An overridden IRI then sends the query to ARQ, so that a
program's own `fn:` implementation is used where it replaced Jena's.

A second, late check catches what the walk cannot. If Sparkles rejects the query text with
a syntax error or `Unsupported` before it produces any row, `QueryIterSparkles` replaces
itself with a `QueryEngineMain` plan over the same dataset and logs why. This covers ARQ
syntax that Sparkles does not parse, such as `LET`.

**Fallback modes.** `SparklesFallback.AUTO` is the behaviour above. `NEVER` throws
`QueryExecException` with the reason instead of falling back, for programs that must not
silently run in ARQ. `ALWAYS` sends every query to ARQ, which is useful for comparisons.
The mode is set in `SparklesOptions` and can be overridden per query with the context
symbol `Sparkles.FALLBACK`. Each fallback is logged at debug level with its reason, and
`DatasetGraphSparkles.stats()` counts queries by path.

**Prefetching in the fallback.** ARQ evaluates a query that falls back by calling
`find()` for each triple pattern, once per incoming binding in its nested loop joins.
Each of those calls gets the prefetching iterator of §3.1, which fetches matches in
chunks. A pattern whose matches fit in the first chunk of 64 costs one native call,
because `find` returns the first chunk with the cursor (§2.3), and a pattern with many
matches costs one call per chunk of up to 4096 rather than one per match.

**What ARQ cannot do.** In a query that falls back, Sparkles' own extensions that Jena
lacks are unavailable. `text:query` over a Sparkles text index, `spk:vectorSearch` and
`path:search` then fail or match nothing, because ARQ sees them as unknown property
functions or services. The fallback is correct for queries written for Jena, and Phase 3
narrows the gap twice (§8).

### 3.6 The update engine

`UpdateEngineSparkles.factory` accepts a `DatasetGraphSparkles` whose fallback mode is not
`ALWAYS`. Its engine implements Jena's `UpdateEngine` in three parts.

* **`startRequest`** makes sure the request has a write transaction. Inside the thread's
  write transaction it uses that one, and inside a `READ_PROMOTE` transaction it promotes
  as a write would. Outside a transaction it begins a Jena write transaction on the
  dataset for the request and commits it in `finishRequest`, so that the request is one
  commit, as SPARQL Update over HTTP is in Sparkles. Inside a `READ` transaction it
  throws `JenaTransactionException`.
* **`getUpdateSink`** returns a sink that takes the request one operation at a time.
  `INSERT DATA` and `DELETE DATA` arrive through the sink's `QuadDataAccSink`s, and their
  quads go to the write transaction as `apply` batches without being turned into text.
  Every other operation is checked by the detector of §3.5 and serialized with Jena's
  `UpdateWriter`. Consecutive native operations are sent as one request text to
  `FfiWriteTxn::update`, so that `DELETE … INSERT … WHERE` and its neighbours keep
  Sparkles' planning.
* **An operation that needs Java** runs through ARQ's `UpdateEngineWorker` over the same
  dataset and transaction, where its writes reach Sparkles through `add` and `delete`.
  Operations before and after it still run in Sparkles. The order of operations is kept,
  because the pending native text is sent before the Java operation runs.

A syntax error in the text sent to Sparkles changes nothing, so the engine falls back for
it as a query does. Any other error ends the request and the transaction (§3.3).

`LOAD` follows Sparkles' defaults for embedders, where `LOAD <file:…>` may read any file
the process can read. SERVICE and remote `LOAD` follow §3.8.

### 3.7 Union default graph

Jena TDB2 has a union default graph setting, which a program sets with the context
symbol `tdb2:unionDefaultGraph`, through `ja:context`, or with the assembler property
`tdb2:unionDefaultGraph`. With it on, queries see the union of the named graphs as their
default graph, while `getDefaultGraph()` and `find` on the default graph still reach the
stored default graph, because only `QueryEngineTDB` applies the setting.

The binding maps every one of those spellings onto Sparkles' union default graph and
keeps TDB2's scope. The setting changes what queries and updates in Sparkles' engine see
as the default graph, and the `DatasetGraph` and `Graph` APIs keep the stored default
graph. Sparkles has the setting per store (`StoreOptions::union_default_graph`), and a
protocol dataset whose default graph is `urn:x-arq:UnionGraph` gives it per request but
empties the named graphs. The binding therefore needs one more additive engine API, a
`union_default_graph` flag in `QueryOptions` that overrides the store's flag for one
request and leaves the named graphs alone. The binding sets that flag on every request,
from the dataset's setting unless the query's context overrides it.

`SparklesOptions.unionDefaultGraph(true)` and the assembler property
`sparkles:unionDefaultGraph` set it for the dataset. `Sparkles.UNION_DEFAULT_GRAPH` and
TDB2's symbols set it per query. `DatasetAssemblerSparkles` also reads
`tdb2:unionDefaultGraph`, so a Fuseki configuration whose dataset type was the only
change keeps its setting. A query that falls back to ARQ gets the same view, because the
fallback plan rewrites its default graph to `Quad.unionGraph`, as `QueryEngineTDB` does.

### 3.8 Context symbols

The engine reads these symbols from the query's context, which Jena builds from the
global context, the dataset's context and the execution's own settings.

| Symbol | Sparkles option |
|---|---|
| `Sparkles.AT` (a string in the syntax of `history::At`) | run on the commit it names, as `?at=` does on the server ([F06](F06-snapshots-and-point-in-time.md)). Phase 2. |
| `Sparkles.UNION_DEFAULT_GRAPH`, and TDB2's `unionDefaultGraph` symbols by name | the union default graph of §3.7 |
| `Sparkles.INCLUDE_INFERRED` | the reasoner's `urn:x-sparkles:inferred` graph is merged into the default graph |
| `Sparkles.MAX_ROWS`, `Sparkles.MAX_MEMORY_BYTES`, `Sparkles.MAX_ROWS_PRODUCED` | the budgets of [C01](C01-observability-and-budgets.md) |
| `Sparkles.FALLBACK` | the mode of §3.5 |
| `ARQ.httpServiceAllowed` | `allow_service`. SERVICE is allowed unless the context says false, as in Jena. |
| `ARQ.queryTimeout` | handled by Jena through cancellation, and passed on as `timeout` as a backstop |

TDB2's symbols are matched by their IRIs, so the library does not depend on `jena-tdb2`.
A program moved from TDB2 that sets `tdb2:unionDefaultGraph` keeps working.

The binding allows SERVICE and remote `LOAD` to every destination by default, as Jena
does, through an outbound policy that permits loopback and private addresses. The
server's default refuses them, because the server runs requests from remote callers, and
an embedding program already has the network access its code has.
`SparklesOptions.outboundPolicy` restores the server's rules.

### 3.9 The assembler and Fuseki

Phase 2 adds an assembler type in the namespace `urn:x-sparkles:assembler#`, written
here with the prefix `sparkles:`.

```turtle
PREFIX fuseki:   <http://jena.apache.org/fuseki#>
PREFIX ja:       <http://jena.hpl.hp.com/2005/11/Assembler#>
PREFIX sparkles: <urn:x-sparkles:assembler#>

<#service> a fuseki:Service ;
    fuseki:name "ds" ;
    fuseki:endpoint [ fuseki:operation fuseki:query ] ,
                    [ fuseki:operation fuseki:update ] ,
                    [ fuseki:operation fuseki:gsp-rw ] ;
    fuseki:dataset <#dataset> .

<#dataset> a sparkles:DatasetSparkles ;
    sparkles:location "/var/lib/sparkles/ds" ;
    sparkles:unionDefaultGraph false ;
    sparkles:fallback "auto" ;
    ja:context [ ja:cxtName "arq:queryTimeout" ; ja:cxtValue "10000" ] .
```

`DatasetAssemblerSparkles` extends Jena's `DatasetAssembler`, as TDB2's
`DatasetAssemblerTDB2` does. `sparkles:location` names the database directory, and
`sparkles:memory true` makes an in-memory dataset instead. `sparkles:unionDefaultGraph`,
`sparkles:fallback`, `sparkles:blankNodeLabels` and `sparkles:autocommit` set the options
of §4.1. `ja:context` is merged into the dataset's context with
`AssemblerUtils.mergeContext`, as TDB2's assembler does. `VocabSparkles` registers the
type with `AssemblerUtils.registerDataset` when `InitSparkles` starts.

Two assemblers or `open` calls for the same directory in one JVM share one native
handle, which a registry keyed by the canonical path counts, as TDB2's
`StoreConnection` shares a location. A second process that opens the directory gets
`SparklesDatasetLockedException`.

**Running in Fuseki.** Fuseki's `fuseki-server` script adds every jar in
`$FUSEKI_BASE/extra` to the classpath. The project publishes `sparkles-jena-all`, one jar
with the library, the Kotlin standard library, JNA and the native libraries, for that
directory. Fuseki 6 needs Java 21, which runs the binding's Java 17 bytecode. Fuseki's
query, update, Graph Store and upload operations then go through the engines of §3.4 and
§3.6. Its backup operation dumps through `find()` and works, slowly. Its compaction
operation is specific to TDB2 and refuses the dataset, and Phase 2 documents
`DatasetGraphSparkles.compact()` and `sparkles compact` instead.

### 3.10 Models, listeners and inference

**Model and Graph.** `DatasetFactory.wrap(dsg)` gives a `Dataset`, and its models wrap the
graph views of §3.2. Every Model call that reads data is a `find()`. A `find()` costs one
native call for its first batch, so a `getProperty` or `listStatements` with a bound
subject costs a few microseconds more than in TDB2, where it reads a B+tree in the same
process. The first batch of a cursor holds 64 quads, and later batches grow to 4096, so
point lookups stay cheap and scans stay fast.

**Listeners.** `GraphView` keeps Jena's `GraphEventManager`, so listeners see changes
made through the Graph and Model APIs. Changes made by SPARQL Update in Sparkles, by bulk
loads and through `DatasetGraph.add` are not reported to graph listeners, which is the
same in TDB2. A program that needs every change can wrap the dataset in Jena's
`DatasetGraphMonitor`. Queries still reach Sparkles through Jena's wrapper factory, and
updates then run in ARQ through the wrapper, so that the monitor sees their writes.

**InfModel and OntModel.** Jena's reasoners and `OntModel` work over any `Graph`, and
they work over a Sparkles graph view. Rule reasoners call `find` very many times, so they
are much slower over the binding than over an in-memory graph, and slower than over TDB2.
The documentation says so and points to Sparkles' own reasoning, which materializes RDFS
and OWL RL into `urn:x-sparkles:inferred` inside the engine (Phase 3, §4.5), and to
`Sparkles.INCLUDE_INFERRED`. A common pattern is to copy the graph an `InfModel` needs
into memory first.

### 3.11 Differences from TDB2

The documentation lists these differences for programs that move from TDB2.

* **Literals by value.** TDB2 stores many literals by value inside the node id
  (`NodeIdInline`). A valid `xsd:integer`, `xsd:decimal`, `xsd:dateTime`, `xsd:date` or
  `xsd:boolean` literal, among others, reads back in canonical form, so
  `"01"^^xsd:integer` comes back as `"1"^^xsd:integer` and derived integer types can come
  back with another datatype. Sparkles inlines only literals whose lexical form is
  already canonical and keeps every other lexical form as written. Data migrated from
  TDB2 matches what TDB2 returned, because TDB2 canonicalized it when it was stored and
  the import reads it back through TDB2 (§4.6). New writes can differ. A program that
  writes `"01"^^xsd:integer` and compares lexical forms or uses `sameTerm` gets `"01"`
  back from Sparkles where TDB2 gave `"1"`. Value comparisons, `=` in SPARQL and
  `Node.sameValueAs` agree on both.
* **Promotion.** §3.3 defines it. The only difference is that Sparkles allows a
  promotion after a write transaction that committed no change.
* **Blank node labels** survive transactions within a process, and come back as
  `b<hex>` labels after the dataset is reopened (§3.2).
* **Prefixes** are saved outside the write transaction (§3.2).
* **A failed SPARQL Update** aborts the transaction it ran in (§3.3).
* **Union default graph** has TDB2's meaning and scope (§3.7).
* **Inference and listeners** behave as in TDB2, with the costs of §3.10.
* **Compaction** is `DatasetGraphSparkles.compact()` or `sparkles compact`, and Fuseki's
  compaction operation refuses the dataset (§3.9).

## 4. The Java API

### 4.1 Entry points and options

```java
import io.github.kclejeune.sparkles.jena.*;

try (DatasetGraphSparkles dsg = SparklesDatasets.open(Path.of("DB"))) {
    Dataset ds = DatasetFactory.wrap(dsg);
    dsg.loadFiles(List.of(Path.of("data.ttl.gz")));       // one bulk commit
    Txn.executeRead(ds, () -> {
        try (QueryExecution qe = QueryExecution.dataset(ds)
                .query("SELECT ?s WHERE { ?s a <http://ex.org/T> }").build()) {
            ResultSetFormatter.out(qe.execSelect());
        }
    });
    CommitReceipt r = dsg.lastReceipt();
}
```

`SparklesDatasets` is a Kotlin `object` whose members are `@JvmStatic`:

| Method | Returns |
|---|---|
| `open(Path)`, `open(String)`, `open(Path, SparklesOptions)` | a persistent dataset, created if the directory is empty |
| `memory()`, `memory(SparklesOptions)` | an in-memory dataset |
| `openDataset(Path)`, `openDataset(Path, SparklesOptions)` | the same, wrapped as a Jena `Dataset` |

`SparklesOptions` is immutable and built with `SparklesOptions.builder()`. Its settings are
`unionDefaultGraph(boolean)`, `fallback(SparklesFallback)`,
`blankNodeLabels(BlankNodeLabels)`, `autocommit(boolean)`, `readOnly(boolean)`,
`outboundPolicy(SparklesOutbound)` and `termCacheSize(int)`. `SparklesOptions.DEFAULT`
holds the defaults. The enums are Java enums.

### 4.2 Java interop rules

The public API follows these rules, and the sample project of §7 checks them by compiling
Java code against the jar.

* **Static entry points.** Members of objects and companions that Java calls are
  `@JvmStatic`, and constants are `@JvmField` or `const`.
* **Overloads instead of defaults.** Functions with default parameters are
  `@JvmOverloads`, and the important combinations also have explicit overloads, so Java
  never needs to pass every argument.
* **Nullability is declared.** The Kotlin compiler writes JetBrains' `@NotNull` and
  `@Nullable` into the class files. Each public package also has a `package-info.java`
  with JSpecify's `@NullMarked`, so tools that read JSpecify treat unannotated types as
  non-null. The JSpecify jar adds a few kilobytes.
* **No Kotlin-only types.** Public signatures use no `suspend` functions, `Flow`s,
  `Sequence`s, sealed classes, value classes, `Result`, `Pair` or Kotlin function types.
  Callbacks take `java.util.function` interfaces, and collections are `java.util` types.
  Value types such as `CommitInfo` are final classes with getters, `equals`, `hashCode`
  and `toString`, and not data classes, so Java does not see `component1` and `copy`.
* **Iterators and streams.** Reads return Jena's `Iterator<Quad>` and `Stream<Quad>`, as
  `DatasetGraph` declares, and the iterators implement `org.apache.jena.atlas.lib.Closeable`
  and `AutoCloseable`.
* **Unchecked exceptions.** Every exception is unchecked, as Jena's are, so no `@Throws`
  is needed.
* **Interfaces with default methods.** The compiler runs with
  `jvmDefault = NO_COMPATIBILITY`, so Kotlin interfaces compile to Java interfaces with
  real default methods.
* **Explicit API mode.** `explicitApi()` makes every public declaration state its
  visibility and type, so nothing becomes public by accident. The generated bindings are
  public Kotlin, so they sit in an `internal` package, and the jar's module descriptor
  (Phase 2) does not export it.

### 4.3 Exceptions

Each `FfiError` becomes the Jena exception that Jena's own code raises in the same
situation. Each class also implements the marker interface `SparklesError`, which gives
the error's kind and the engine's message, so a caller can catch Jena's type or ask
whether Sparkles raised it.

| Sparkles error | Exception | Base |
|---|---|---|
| `SparqlSyntax` in a query or update | `SparklesQueryParseException` | `QueryParseException`, with the line and column |
| `RdfParse` | `SparklesRiotException` | `RiotException` |
| `Timeout`, `Cancelled` | `QueryCancelledException` | Jena's own class, as Jena raises it for timeouts |
| `BudgetExceeded` | `SparklesBudgetExceededException` | `QueryExecException`, with the kind, limit and request |
| `Unsupported` | `SparklesUnsupportedException` | `QueryExecException` |
| `Invalid` | `SparklesInvalidException` | `JenaException` |
| `Conflict`, `WriterBusy`, `PreconditionFailed` | `SparklesTransactionException` | `JenaTransactionException` |
| `Rejected`, `GuardMissing` | `SparklesWriteRejectedException` | `JenaTransactionException`, with the validation report |
| `NotFound`, `HistoryGone`, `HistoryUnsupported` | `SparklesNotFoundException` | `JenaException` |
| `NotPermitted` | `SparklesNotPermittedException` | `JenaException` |
| `Service` | `QueryExecException` | Jena's own class |
| `Corrupt`, `Poisoned`, `StorageFull`, `Io` | `SparklesStorageException` | `JenaException`, with the I/O cause |
| a locked database directory | `SparklesDatasetLockedException` | `SparklesStorageException` |
| a panic in the native library | `SparklesInternalException` | `JenaException` |

Transaction misuse that Jena checks for, such as `begin` inside a transaction, throws
`JenaTransactionException` from the Kotlin side before any native call.

### 4.4 Lifetimes and closing

* `DatasetGraphSparkles` is `Closeable`, as every `DatasetGraph` is. `close()` releases
  this handle's count in the registry of §3.9. The native dataset closes and its
  directory lock is released when the last handle is closed and the last transaction and
  cursor on it have ended. A closed dataset throws on every method.
* Cursors and query results are closed when they are exhausted, when Jena closes the
  iterator or plan, or when the caller closes them. A cursor that is dropped while open
  is freed by the generated code's Cleaner when it is garbage-collected. Until then it
  pins a snapshot, and a pinned snapshot keeps a retired index generation on disk after a
  compaction. The documentation says to close or exhaust iterators.
* A write transaction that is never ended keeps the writer lock. `DatasetGraphSparkles`
  registers its worker with a Cleaner too, so a transaction lost with its thread is
  aborted when the dataset is garbage-collected or closed.
* `Sparkles.init()` forces Jena's initialization and the native library's loading, for
  programs that want a load failure at start-up rather than at the first open.

### 4.5 Sparkles features beyond SPARQL

Most of Sparkles' additions to SPARQL need no Java API, because the query runs in
Sparkles. Full-text search with `text:query` ([F03](F03-full-text-search.md)), vector
search with `spk:vectorSearch` and the similarity functions
([F04](F04-vector-search.md)), GeoSPARQL ([G01](G01-geosparql.md)), path search
([F07](F07-path-search.md)), history queries ([F06](F06-snapshots-and-point-in-time.md))
and ARQ's extensions all work through `QueryExecution`. These get Java methods on
`DatasetGraphSparkles`:

| Feature | Java API | Phase |
|---|---|---|
| Bulk loads | `loadFiles(List<Path>)`, `loadFiles(List<Path>, Node graph)`, `load(InputStream, Lang)` | 1 |
| Migration from TDB2 | `SparklesDatasets.importTdb2(Path, Path)` (§4.6) | 1 |
| Commit receipts | `lastReceipt()` after `commit()` on the same thread, and `headCommit()` | 1 |
| Point-in-time reads | `at(String)` and `at(long commit)`, which return a read-only `DatasetGraphSparkles` pinned to that commit, and the context symbol `Sparkles.AT` | 2 |
| Dumps | `dump(Path)`, `dump(OutputStream, Lang)` | 2 |
| Maintenance | `compact()`, `backup(Path)`, `stats()` | 2 |
| Commit history | `commits(CommitRange, int limit)` | 2 |
| Reasoning | `reason(String profile)`, `reason(String profile, String rules)`, `clearInferences()` | 3 |
| Validation | `validateShacl(…)` and `validateShex(…)`, returning a report with `conforms`, the results and the W3C report as a Jena `Graph` | 3 |
| Index administration | `enableTextIndex(String json)`, `createVectorIndex(String json)`, `setValidationGuard(String json)` in the engine's JSON shapes, as P01 Phase 2 takes them | 3 |
| Named snapshots and clones | `createSnapshot(String)`, `cloneTo(Path)` | 3 |

`load(InputStream, Lang)` reads with Sparkles' parsers in one commit. A program that wants
Jena's parsers, for a syntax Sparkles lacks, uses `dsg.bulkSink()` instead. It returns a
Jena `StreamRDF` that sends quads in batches of 65,536 to one write transaction and
commits when the stream finishes.

Bulk loads, dumps and maintenance run outside any Jena transaction, as Sparkles' own
commands do, and throw `JenaTransactionException` when the calling thread holds one,
because a bulk load needs the writer lock that the thread's transaction would hold.

### 4.6 Migrating from TDB2

Sparkles does not read TDB2's files, and this spec adds no Rust reader of them. TDB2's
storage is a set of internal structures of Jena: a node table of RDF terms encoded with
RDF Thrift in an append-only file, B+tree indexes of node hashes and of tuples of node
ids with their own block managers, and a journal and generation directories. None of it
is documented outside Jena's code, and it changes between releases. Jena 6.0's notes
recommend reloading TDB2 databases because of a fix to the encoding of `xsd:decimal`,
for example. A Rust reader would have to follow Jena's code release by release and would
still miss the canonicalization of §3.11 unless it copied it exactly. Jena's own code
reads every database that Jena can open, so the migration uses it.

`SparklesDatasets.importTdb2(Path tdbDir, Path sparklesDir)` does this in one JVM
process. It needs `jena-tdb2` on the classpath, which the library declares as an
optional dependency and checks for at the call, with an error that names the missing
artifact. The import:

1. opens the TDB2 database with `DatabaseMgr.connectDatasetGraph` and begins a read
   transaction on it;
2. opens or creates the Sparkles database, which must be empty unless
   `ImportOptions.append(true)` is given;
3. streams `find()` over every quad of the TDB2 dataset into the bulk sink of §4.5, in
   batches of 65,536 quads, so that Sparkles builds its index once and the import is one
   commit;
4. copies the TDB2 dataset's prefixes;
5. returns an `ImportReport` with the quad count, the named graph count, the commit
   receipt and the time taken.

Blank nodes keep their identity within the import, because the whole import is one
Sparkles transaction, and they get `b<hex>` labels in Sparkles. The import never touches
the TDB2 directory beyond a read transaction, so it can run while nothing else has the
database open. A TDB2 database that another process is serving cannot be opened, as
TDB2 itself refuses. The union default graph setting is not stored in a TDB2 database. It
lives in the assembler or the code that opens the database, and §3.7 carries it over.

The path through a dump stays available and is what [G08](G08-fuseki-configuration.md)
writes. `sparkles config import fuseki` emits a script that runs Jena's
`tdb2.tdbdump` on each TDB2 location and loads the dump with `sparkles load`. Both paths
read the data through TDB2, so they give the same quads. The in-process import avoids
the intermediate N-Quads file and the second parse, and suits programs that already have
Jena on the classpath. The dump suits migrations to the Sparkles server, where no JVM
runs after the move.

## 5. Performance

### 5.1 Where the bridge's overhead matters

Published benchmarks put a JNA call at about 0.7 to 4 µs, against about 0.25 µs for JNI
and less for the Panama API. A UniFFI call adds a status record and a copy of the
returned buffer. Whether a few microseconds per call matter depends on how many calls a
piece of work makes and how long the work takes.

* **Whole queries.** A query run in Sparkles' engine crosses the boundary a handful of
  times, to prepare it, to execute it with its first batch, and once per later batch.
  Against execution times of milliseconds, a few calls of a few microseconds are
  negligible.
* **Rows.** Each row of a result costs the construction of its Jena `Binding` and of the
  `Node`s it holds the first time they appear. That cost is the same whichever bridge
  carries the bytes, because Jena's objects must be built in any case, and it dominates
  the per-row cost. The design attacks it with batches, so that call overhead is spread
  over thousands of rows, about 1 ns per row at 4096 rows, and with the term table of
  §2.4, so that each distinct term is decoded once per result.
* **Small queries at high rates.** A point lookup, an ASK on a bound triple or a
  star-shaped query on one subject runs in tens of microseconds in TDB2 in the same
  process, and in a similar time in Sparkles. There, the two to four calls of a query at
  a few microseconds each can add 10 to 20% to the query's time and erase Sparkles'
  advantage. The same holds for Model code that makes many `find` calls with one or two
  matches each, and for the ARQ fallback. This is where the bridge matters. The design
  keeps such work to as few calls as it can (§2.3), and the benchmark of §5.4 measures it
  against TDB2, with a rule for moving a hot call to JNI.

The other costs are these.

* **Query text.** Jena serializes the query to text, and Sparkles parses it again. For the
  queries of `scripts/bench.sh`, both take tens of microseconds, which is small next to
  execution but sets the fixed cost of a query.
* **Term conversion.** Building a Jena `Node` costs a UTF-8 decode and an allocation.
  With the term table of §2.4, this is paid once per distinct term of a result, and each
  cell then costs an array read and a store into the binding.
* **Native memory.** Sparkles evaluates a whole query before returning rows and holds its
  result table at 8 bytes per cell plus its local vocabulary. This memory is outside the
  Java heap and is not limited by `-Xmx`. The memory budget of `Sparkles.MAX_MEMORY_BYTES`
  bounds it. A large export therefore holds the whole result in native memory while Java
  converts it, which is the cost of Sparkles' faster execution and is stated in the
  documentation.
* **Thread hops.** A read in a write transaction crosses to the worker thread, which
  costs 5 to 20 µs. Reads in a read transaction run on the caller's thread.
* **Virtual threads.** A native call pins a virtual thread to its carrier for the call's
  duration, so long queries on virtual threads occupy a carrier thread each.

### 5.2 Batch sizes and memory

| Stream | First batch | Later batches |
|---|---|---|
| `find()` | 64 quads | doubling up to 4096 quads |
| query results | 256 rows | up to 8192 rows or 1 MiB, whichever comes first |
| writes in a transaction | sent at 4096 operations or 1 MiB | the same |
| `bulkSink()` | sent at 65,536 quads | the same |

The term table of a result resets at the next batch after reaching `termCacheSize`
nodes, 262,144 by default, so it can exceed that threshold by one batch. At
roughly 100 to 200 bytes per node with its strings, a full table costs about 25 to 50 MB
of heap for one result. A larger table can avoid decoding repeated terms again across
batches and costs more heap. Results containing only unique terms get no reuse benefit;
the option exposes that trade.

### 5.3 Targets

These targets are measured on the development machine with the engines alone and the
machine quiet, and the Outcome records the numbers.

* **T1. Fixed cost.** An ASK query and a one-row aggregate through `QueryExecution` cost
  at most 50 µs more than the same query's `timing.total_ms` inside Sparkles, at the
  median, on a warm JVM.
* **T2. Full-engine queries.** For every query of `scripts/bench.sh`, the time through
  Jena is at most the in-process time (`timing.total_ms`) plus 10%, plus result
  conversion. Conversion costs at most 200 ns per cell and 1 µs per distinct term.
* **T3. Scans.** `find()` over the whole dataset reaches at least 3 million quads per
  second on one thread and is no slower than TDB2's `find()` on the same data.
* **T4. Writes.** `add` in a write transaction followed by `commit` loads at least 70% of
  the quads per second that Sparkles' own `Dataset::extend` loads.
* **T5. Against TDB2.** On every bench query where the Sparkles server beats Fuseki in
  the latest `bench.sh` results, the binding beats TDB2 embedded in the same JVM. On the
  others, it is within 20% of TDB2.
* **T6. Fallback.** Queries that fall back to ARQ over `find()` are measured and reported
  against TDB2, without a target. They are expected to be several times slower on
  join-heavy queries, because each triple pattern crosses the boundary once per incoming
  binding, even with prefetching.
* **T7. Small queries.** On the small-query set of §5.4, the binding's throughput on one
  thread and on as many threads as cores is at least TDB2 embedded's, and the bridge's
  share of a query's median time, computed as calls per query times the measured cost of
  a call, is at most 10%.

### 5.4 Benchmark plan

A JMH module, `jvm/sparkles-jena/src/jmh`, and `scripts/bench-jvm.sh` (with the task
`mise run bench:jvm`) run the benchmark.

* **Data.** The generator and queries of `scripts/bench.sh`, at its default size, and the
  WatDiv instances of `scripts/bench-watdiv.sh`. Sparkles loads with `sparkles load`, and
  TDB2 with `tdb2.tdbloader`, so neither load goes through the binding.
* **Configurations.** Sparkles through Jena's `QueryExecution`, Sparkles with
  `SparklesFallback.ALWAYS`, TDB2 embedded through the same Jena calls, and Sparkles
  through `FfiQuery` alone, which gives the in-process time of T1 and T2 without Jena.
* **Method.** Each configuration runs in its own JMH fork with the same heap, `-Xmx8G`
  as in `bench.sh`, with 5 warm-up and 10 measured iterations, one engine at a time on a
  quiet machine. Each query's answer is fingerprinted with `scripts/bench-answers.py`'s
  method before timing, and a configuration whose answer differs is reported and not
  ranked.
* **Small queries.** A throughput benchmark runs short queries back to back on 1, 4 and
  as many threads as cores, against Sparkles through the binding and TDB2 embedded, and
  reports queries per second with the median and 99th percentile latency. The set is a
  `SELECT ?o` on a bound subject and predicate, `ASK` on a bound triple, `bench.sh`'s
  `star-lookup` and `values-star`, a `Model.getProperty` and a `Graph.contains` with
  bound terms, and `find` on a bound subject. Each run also counts the native calls per
  operation, so the bridge's share of T7 is computed from measurements.
* **Micro-benchmarks.** A no-op native call, a 4096-row batch decode, `find()` with a
  bound subject, `add` throughput in a transaction, and promotion.
* **Moving a call to JNI.** A call is moved to a hand-written JNI entry point when both
  hold on the small-query benchmark: the binding is slower than TDB2 embedded on a query
  or operation, and the UniFFI calls account for more than 10% of its median time. The
  candidates are the calls a small request makes, which are `find` with its first batch,
  `contains`, `prepare_query` and `execute` with its first batch. The JNI entry point
  calls the same Rust function and passes the same byte encoding, so only the transport
  changes. It is kept only if the benchmark shows it closes the gap, and the Outcome
  records the before and after numbers.
* **Output.** A Markdown summary in the work directory, in the format of the other bench
  scripts. The Outcome records it.

## 6. Packaging

### 6.1 Versions

* **JVM.** The library compiles to Java 17 bytecode with a Java 17 toolchain. Jena 5.x
  needs Java 17 and Jena 6 needs Java 21, and the same jar serves both.
* **Jena.** The library compiles against Jena 5.6.0, the last 5.x release, and declares
  `jena-arq` 5.6.0 as an `api` dependency, so a program that depends on Jena 6 resolves
  to its own version. Jena 5.6 deprecated what Jena 6.0 removed, so the build treats
  deprecation warnings as errors, and the library uses only the API that both lines
  share. CI tests both lines (§6.5, open question 2).
* **Kotlin.** Kotlin 2.4 with the Kotlin JVM Gradle plugin. The library depends on
  `kotlin-stdlib`, which is 1.8 MB for 2.4.10, and on JNA 5.19.1, which is 2.0 MB and
  carries JNA's own native dispatch libraries.
* **Gradle.** Gradle 9 with the wrapper checked in, a version catalog, dependency locking
  and Gradle's dependency verification metadata.

### 6.2 Artifacts and native libraries

| Artifact | Contents | Size |
|---|---|---|
| `io.github.kclejeune:sparkles-jena` | the classes, depending on `sparkles-jena-natives`, `jena-arq`, `kotlin-stdlib`, JNA and JSpecify, with `jena-tdb2` optional for the import | under 1 MB |
| `io.github.kclejeune:sparkles-jena-natives` | the five native libraries in the main jar, and one platform each in the classifier jars `linux-x86_64`, `linux-aarch64`, `macos-x86_64`, `macos-aarch64` and `windows-x86_64` | about 50 MB, about 10 MB per classifier |
| `io.github.kclejeune:sparkles-jena-all` | everything above in one jar, for Fuseki's `extra` directory | about 55 MB |

The sizes follow P01's measurement, where the stripped extension module is 25 MB
unpacked and 10.5 MB in a compressed wheel. sqlite-jdbc and RocksDB's Java binding both
ship one jar with every platform and add classifier jars for one platform, and this
layout does the same. A program that wants a small deployment excludes the transitive
natives jar and adds one classifier.

The native libraries are stored in the jar as
`io/github/kclejeune/sparkles/native/<os>-<arch>/<file>` with a SHA-256 file beside each.
The file names are `libsparkles_ffi.so`, `libsparkles_ffi.dylib` and `sparkles_ffi.dll`.
Linux builds target glibc 2.28, in the manylinux 2.28 images that the Python wheels use,
and macOS builds target macOS 11. Each library is stripped and built with thin LTO.

### 6.3 Loading the native library

The library loads its native part itself and does not rely on JNA's search of the
classpath, for three reasons. JNA's resource prefixes have no musl variant. Its extraction
uses one name per library, so two versions in one machine's temp directory collide. And a
missing platform should fail with a message that names the platform.

`NativeLoader` runs before the first generated call:

1. If the system property `sparkles.native.path` names a file, it uses that file.
2. Otherwise it maps `os.name` and `os.arch` to one of the five platform directories. On
   Linux it checks for musl and fails with a clear message, because Phase 1 and Phase 2
   ship glibc builds only.
3. It extracts the resource into a new directory under `sparkles.native.dir` or
   `java.io.tmpdir`. On POSIX systems the directory is created with mode 0700 and the file
   with mode 0700, so no other user can replace the library between the check and the
   load. The SHA-256 is computed from the bytes as they are written, and a mismatch
   deletes the copy and fails. Each JVM extracts its own copy and never reuses a directory
   it did not create, because a shared, predictable path lets a local attacker swap the
   file after the hash check. The copy is deleted when the JVM exits. Windows cannot
   delete a loaded DLL, so on Windows the loader also deletes this user's earlier copies
   that no running JVM still holds open.
4. It sets UniFFI's `uniffi.component.sparkles.libraryOverride` property to the absolute
   path, which makes the generated code's `Native.register` load that file.
5. It calls `version()` and compares the native library's encoding version and crate
   version with the jar's. A mismatch fails with both versions in the message.

On Java 24 and later, the JVM prints a warning when JNA loads native code, as JEP 472
prepares for restricting JNI. The documentation tells users to pass
`--enable-native-access=ALL-UNNAMED`, and the `sparkles-jena-all` jar's manifest carries
`Enable-Native-Access: ALL-UNNAMED` for use as an executable jar.

### 6.4 Nix

* `nix/jvm.nix` builds `packages.sparkles-ffi`, the native library for the host platform
  with nixpkgs' `rustPlatform` and the crate's `Cargo.lock`, as `nix/python.nix` builds
  the Python extension. The same derivation runs the crate's `uniffi-bindgen` and outputs
  the generated Kotlin.
* `packages.sparkles-jena` builds the jar with nixpkgs' Gradle support. The Gradle
  dependencies are fetched through `gradle.fetchDeps`, whose lock file
  `nix/jvm-deps.json` is updated by `mise run jvm:nix-deps`. The jar holds the host's
  native library only.
* `checks.jvm-bindings` runs the Gradle test task offline on the built jar.
* The dev shell gains a JDK 17 and Gradle. The flake's `jena-clients` check keeps using
  nixpkgs' `apache-jena`.

### 6.5 CI and publishing

`mise run jvm:build`, `jvm:test`, `jvm:lock` and `jvm:sample` run the build locally, with
the native library built by cargo in the root `target` directory so that it shares
compiled dependencies. `jvm:test` joins `mise run ci` if it stays under a minute on a warm
cache, as P01 decided for `py:test`.

`.github/workflows/jvm.yml` follows `python-wheels.yml`:

* **natives.** A matrix builds the native library for Linux x86_64 and aarch64 in the
  manylinux 2.28 containers on GitHub's x86 and arm runners, for macOS x86_64 and arm64 on
  `macos-15-intel` and `macos-15`, and for Windows x64 on `windows-2025`. Each job
  uploads its library.
* **jar.** One job downloads the five libraries, generates the bindings, and builds the
  three artifacts, with sources and Javadoc jars made by Dokka.
* **test.** A matrix runs the test suite of §7 on each platform against the assembled jar,
  with JDK 17 and Jena 5.6.0, and with JDK 21 and 25 and Jena 6.2.0 on Linux x86_64.
* **publish.** It runs only for a `jvm-v*` tag when the repository variable
  `MAVEN_PUBLISH` is `true`. It signs the artifacts with a GPG key from the repository's
  secrets and uploads them to Maven Central through the Central Portal. As committed, the
  workflow builds and tests and publishes nothing.

The workflow runs on pull requests that touch `crates/`, `jvm/`, `vendor/` or the
workflow itself. `scripts/third-party-licenses.py` also writes
`jvm/sparkles-jena-natives/THIRD_PARTY_LICENSES.md` for the crates the native library
links, and the jars carry it with the project's license.

## 7. Testing

* **Jena's contract tests.** `jena-arq` and `jena-core` publish their test classes as
  `tests` jars. The library's test source set subclasses Jena's abstract suites with a
  Sparkles dataset, as TDB2's tests do. TDB2's `TestDatasetGraphTDB2` runs each test
  inside a write transaction that it aborts afterwards, and the binding's tests do the
  same where the suite expects it. In the checkout read for this spec the suites and
  their test counts are `AbstractDatasetGraphTests` (32), `AbstractDatasetGraphFind`
  (31), `AbstractDatasetGraphFindPatterns` (16), `AbstractTestGraphOverDatasetGraph`
  (14), `AbstractTestGraphAddDelete` (16), `AbstractTestTransactionLifecycle` (51),
  `AbstractTestTransPromote` (25), `AbstractTestDynamicDataset` (27),
  `AbstractTestUpdateGraph` (28) and `AbstractTestPrefixMappingView` (8), with
  jena-core's `BaseTestGraph` for the Graph contract. The suites differ between Jena 5.6
  and 6.x, so the subclasses live in one source set per Jena line.
* **ARQ's scripted suites.** Jena's manifest runner builds each test's dataset in
  `QueryEvalTest.createEmptyDataset` and takes a dataset creator in `UpdateEvalTest`. A
  test runner in Kotlin uses these hooks to run the SPARQL 1.1 query and update suites
  and ARQ's own suites (`jena-arq/testing/ARQ`) with Sparkles datasets through Jena's
  query execution. Sparkles already runs the W3C suites in Rust, so this run tests the
  binding's translation: the serialized query text, the CONSTRUCT and ASK rewriting, the
  bindings and the fallback. Tests that fail for reasons Sparkles already records in
  `testsuite/jena-arq/expected-failures.txt` are skipped with the same reason.
* **The TDB2 import.** A test builds a small TDB2 database with Jena, imports it, and
  compares the result with the database's `tdb2.tdbdump` loaded by `sparkles load`.
* **The binding's own tests.** The acceptance examples of §9, the encoding of every term
  kind in both directions, cancellation, the fallback detector's cases, the native
  loader's platform mapping, and promotion races between threads.
* **The Java sample.** `jvm/sample-java` is a Java 17 Gradle project that depends on the
  built jar and contains a program and JUnit tests written only against the public API.
  It compiles with `-Xlint:all -Werror`, so raw types, unchecked conversions and
  deprecations fail the build, and it runs on a JDK without Kotlin installed. It proves
  that the API is pleasant from Java, which a Kotlin test suite cannot show.
* **The Jena client harness, embedded.** `testsuite/jena-clients/JenaClients.java` checks
  77 behaviours of Jena's HTTP clients against the Sparkles server. A Phase 2 variant runs
  the same checks through `RDFConnection.connect(dataset)` on an embedded Sparkles
  dataset, and runs them a third time against Fuseki serving `sparkles:DatasetSparkles`,
  so that the three paths agree.

## 8. Phasing

**Phase 1** makes the binding useful on one platform. It is about four weeks of work for
one developer who knows the engine (size L).

* the `sparkles-ffi` crate with the objects of §2.3, the encoding of §2.4, the additive
  engine APIs, and the `jvm:lock` task;
* `DatasetGraphSparkles`, the graph views, prefixes and the blank node table;
* transactions with promotion, the write buffer and the worker thread;
* the query and update engines, with the detector's rules 1 to 3 and 6 and the late check
  on syntax and `Unsupported` errors;
* `SparklesDatasets`, `SparklesOptions`, the exception mapping, `loadFiles`, `load`,
  `bulkSink`, `lastReceipt` and `importTdb2`;
* the union default graph of §3.7 with its `QueryOptions` flag;
* the native loader and a jar for Linux x86_64;
* Jena's contract tests, the Java sample and `mise run jvm:build`, `jvm:test` and
  `jvm:sample`, with the flake's package and check.

**Phase 2** completes the platforms and the integration. It is about three weeks (size M
to L).

* the natives for Linux aarch64, macOS x86_64 and aarch64 and Windows x86_64, the
  `natives`, `jar` and `test` jobs, and the gated publish job;
* the assembler and the Fuseki bundle, with a Fuseki test;
* the detector's rules 4 and 5, the overridden-IRI refinement, the `Op` path and the
  fallback statistics;
* `at`, `Sparkles.AT`, dumps, compaction, backups and commit history;
* DESCRIBE through Sparkles, and the module descriptor;
* ARQ's scripted suites, the Jena 6 test matrix and the embedded client harness;
* the benchmark of §5.4, with the numbers in the Outcome, including the small-query
  benchmark and any call moved to JNI under its rule.

**Phase 3** adds what goes beyond Jena's interfaces. It is about four weeks (size L), and
its items are independent.

* reasoning, validation, index administration, named snapshots and clones from Java
  (§4.5);
* a fallback that keeps basic graph patterns in Sparkles. ARQ runs the query, and a
  `StageGenerator` sends each basic graph pattern with its incoming bindings as `VALUES`
  to Sparkles in batches, rather than one `find()` per triple pattern;
* Java functions in Sparkles' engine. Once the extension API of P03 exists, functions
  registered in Jena are registered with the engine through a UniFFI foreign trait, and
  Sparkles calls them for each batch of values. Queries with Java functions then stop
  falling back. The cost is an upcall per batch;
* musl builds, with a musl directory in the jar that the loader picks;
* a measurement of mimalloc in the native library;
* a write transaction that holds the writer lock without a worker thread, if the engine
  gains an owned lock guard (open question 8).

## 9. Acceptance examples

* **A1.** `SparklesDatasets.memory()` is empty. After `Txn.executeWrite(dsg, () ->
  dsg.add(q))` with an IRI quad in a named graph, `Txn.calculateRead(dsg, () ->
  dsg.contains(q))` is true, `dsg.listGraphNodes()` gives that graph, and
  `dsg.getDefaultGraph().isEmpty()` is true.
* **A2.** `dsg.add(q)` outside a transaction throws `JenaTransactionException`. With
  `autocommit(true)` it commits at once, and `lastReceipt().getCommit().getSeq()` grows
  by one.
* **A3.** A persistent dataset written, closed and reopened has the same quads. Opening
  the same directory twice in one JVM returns handles that share one native dataset, and
  opening it from a second JVM throws `SparklesDatasetLockedException`.
* **A4.** `QueryExecution.dataset(ds).query("SELECT ?s ?o WHERE { ?s ?p ?o } ORDER BY
  ?o")` returns the same solutions as the same query on a TDB2 dataset with the same
  data, including an unbound OPTIONAL variable. Its plan is a Sparkles plan, and
  `dsg.stats()` counts one native query.
* **A5.** A CONSTRUCT query with a `GRAPH` template, an ASK query and a DESCRIBE query
  return what TDB2 returns on the same data.
* **A6.** A query that calls a function registered with `FunctionRegistry.get().put(iri,
  MyFunction.class)` runs in ARQ and returns the function's results. With
  `Sparkles.FALLBACK` set to `NEVER` in the context, it throws `QueryExecException` that
  names the IRI. A query that calls `afn:localname` runs in Sparkles.
* **A7.** `"SELECT * WHERE {"` throws `QueryParseException` from Jena's parser. A query
  that Jena parses and Sparkles rejects as a syntax error, such as one with `LET`, runs in
  ARQ and is logged as a fallback.
* **A8.** A query with a 100 ms timeout over a cross product of 10⁶ × 10⁶ triples throws
  `QueryCancelledException` within 200 ms, and the dataset then answers other queries.
* **A9.** In a `READ_PROMOTE` transaction, a write promotes it. When another thread
  commits between `begin` and the write, the write throws `JenaTransactionException`. In
  `READ_COMMITTED_PROMOTE`, the same sequence promotes and sees the other commit.
* **A10.** Inside `Txn.executeWrite`, `UpdateExec.dataset(dsg).update("INSERT DATA { <a:s>
  <a:p> 1 }").execute()` followed by `dsg.find(…)` sees the quad, and a query in another
  thread does not until the transaction commits. An update request outside a transaction
  is one commit.
* **A11.** An update request whose second operation uses a Java function runs its first
  and third operations in Sparkles and its second in ARQ, in order, in one commit.
* **A12.** A blank node created with `NodeFactory.createBlankNode()` and added in one
  transaction is found by `find(null, bnode, null, null)` in a later transaction, and the
  quad returned has the same label. With `BlankNodeLabels.TRANSACTION`, the second
  transaction finds nothing.
* **A13.** `find()` over 10,000 quads returns them all across several batches, and an
  iterator used after its transaction ended throws `JenaTransactionException`.
* **A14.** A literal of every datatype of the encoding's table, a language-tagged string,
  a directional string and a triple term round-trip through `add` and `find` with equal
  `Node`s.
* **A15.** Fuseki 6, given the `config.ttl` of §3.9 and `sparkles-jena-all` in `extra`,
  serves queries, updates and Graph Store requests on the Sparkles database, and the
  embedded client harness passes against it.
* **A16.** The Java sample compiles with `-Xlint:all -Werror` against the published API and
  its tests pass on JDK 17 with Jena 5.6.0 and on JDK 21 with Jena 6.2.0.
* **A17.** With a jar that lacks the host's platform, `SparklesDatasets.memory()` throws an
  `UnsatisfiedLinkError` whose message names the platform and the property
  `sparkles.native.path`.
* **A18.** `SparklesDatasets.importTdb2` on a TDB2 database with named graphs, blank
  nodes and prefixes produces a Sparkles database whose quads equal those of the
  `tdb2.tdbdump` of the same database loaded with `sparkles load`, up to blank node
  labels, in one commit. Without `jena-tdb2` on the classpath it throws an error that
  names the artifact.
* **A19.** A TDB2 database holding `"01"^^xsd:integer`, imported, returns
  `"1"^^xsd:integer`, as TDB2 does. The same literal written through the binding returns
  `"01"^^xsd:integer`, and `FILTER(?x = 1)` matches it in both.
* **A20.** With `tdb2:unionDefaultGraph true` in a query's context, `SELECT * { ?s ?p ?o
  }` sees the named graphs' triples, `GRAPH ?g { … }` still sees each named graph, and
  `dsg.getDefaultGraph().find()` sees only the stored default graph.

## 10. Rejected alternatives

* **Plain Java with hand-written JNI through the `jni` crate.** JNI calls cost a fraction
  of a JNA call, and the library would need neither JNA nor Kotlin's standard library.
  The cost is glue written by hand on both sides for every method, with object
  lifetimes, exception translation and local reference management in unsafe Rust. UniFFI
  generates this glue and its lifetime handling. Because every call here moves a batch,
  the call overhead that JNI would save is about 1 ns per row, and the maintenance it
  would add is large. A hand-written JNI entry point for a single hot call of small
  requests stays possible under the rule of §5.4.
* **A Rust reader of TDB2's files.** It would let `sparkles load` read a TDB2 directory
  without a JVM. TDB2's format is internal to Jena, undocumented and changed between
  releases, and the reader would have to reproduce TDB2's canonicalization of literals to
  give the same data. The import of §4.6 and the dump path of G08 read the data through
  Jena's own code instead.
* **Java with the Panama foreign function API.** The FFM API became final in Java 22
  (JEP 454) and is faster than JNA. Jena 5.x programs run on Java 17, and a Java 22
  floor would exclude them. The one generator that targets it from UniFFI,
  `uniffi-bindgen-java`, is a pre-1.0 project that describes itself as unstable. A
  multi-release jar with an FFM path for Java 22 and later is open question 6.
* **A remote client only.** It works today. `RDFConnectionFuseki` passes the client suite
  against `sparkles serve`. Every request then costs HTTP and result serialization, there
  are no transactions across requests, and Model and InfModel code that calls `find` per
  triple becomes a request per triple. Embedding exists to remove those costs.
* **GraalVM interop.** GraalVM's polyglot API runs LLVM bitcode or WebAssembly rather than
  a native library, and native-image needs a closed world built with GraalVM. Both tie
  users to one JDK distribution and give nothing that UniFFI over JNA does not on a
  standard JVM.
* **Implementing TDB2's storage SPI.** Sparkles could sit under `DatasetGraphStorage` as
  a `StorageRDF`, which would reuse Jena's dataset and graph glue. It would also force
  Jena's transaction components onto Sparkles' own and limit the engine to the triple
  pattern calls that TDB2's query engine makes, which loses Sparkles' planning.
* **Offloading basic graph patterns only.** TDB2's `QueryEngineTDB` extends
  `QueryEngineMain` and has its storage answer basic graph patterns while ARQ runs the
  rest. Doing the same would keep every Jena extension working, but joins, aggregates,
  paths and filters would run in ARQ, which is where Sparkles is fastest. It is kept as
  the improved fallback of Phase 3.
* **RDF Thrift as the batch encoding.** Jena reads RDF Thrift natively, which would save
  the decoder. Thrift writes each term in full in every row, so repeated terms are
  decoded and allocated again, and it costs more per term than the table of §2.4.
* **Apache Arrow as the batch encoding.** Arrow's columnar format would cross without a
  copy, but its Java library is several megabytes with its own memory allocator and
  needs JVM flags on recent JDKs. The rows here are small integers into a term table,
  which a `ByteBuffer` reads as fast.
* **A UniFFI record per term or per quad.** It is the simplest interface and costs a
  JNA call and a record copy per element, which is two orders of magnitude slower than a
  batch for scans and results.
* **Kotlin coroutines in the API.** Suspending functions and flows suit Kotlin callers,
  but Java cannot call them comfortably, and Jena's interfaces are blocking.

## 11. Open questions

1. **Coordinates and package.** Decided on 2026-10-03: group `io.github.kclejeune`, a
   Maven Central namespace that the GitHub account can verify, with artifacts
   `sparkles-jena`, `sparkles-jena-natives` and `sparkles-jena-all` and package
   `io.github.kclejeune.sparkles.jena`.
2. **One jar for Jena 5 and 6.** Compiling against 5.6 without deprecated API should
   work on 6.x, and CI tests both. If an API needed by the binding changed shape between
   them, the alternative is two artifacts. Decided on 2026-10-03: one artifact if the
   Jena 6 test matrix passes with it, and two otherwise, whichever keeps the build simpler.
3. **Blank node labels.** Default: `DATASET`, which keeps Jena's behaviour within a
   process and costs memory per label, with `TRANSACTION` documented for loading data.
4. **Writes outside a transaction.** Decided on 2026-10-03: refuse them by default, as
   TDB2 does, and let `autocommit(true)` turn each such write into its own commit, as
   Jena's in-memory dataset behaves. The refusal is a default, not a requirement of the
   design.
5. **The natives jar.** Default: the main natives jar carries all five platforms, with
   classifier jars for slim deployments. The alternative makes the classifiers the
   default and needs the user to choose a platform.
6. **An FFM path.** A multi-release jar could call the native library through the Panama
   API on Java 22 and later and through JNA on 17 to 21. Default: not before the Phase 2
   benchmark shows that call overhead matters after batching.
7. **Transactional prefixes.** Default: keep Sparkles' immediate prefix saves and document
   the difference. The engine change would apply to the server too.
8. **The writer's thread hop.** A write transaction could hold an owned writer lock guard
   (parking_lot's `ArcMutexGuard`) that may move between threads, which removes the hop.
   Default: keep the worker, which is also correct for virtual threads.
9. **Allocator.** Default: the system allocator in Phase 1, with mimalloc measured in
   Phase 3.
10. **Sharing with P01.** The transaction worker moves into `sparkles::embed`. Default:
    P04 uses it, and the Python bindings move onto it when they next change.
11. **Generated code's license.** Decided on 2026-10-03. UniFFI is licensed MPL-2.0,
    which is already in the tree through `imbl` and `option-ext`, and the generated
    Kotlin comes from its templates. Sparkles uses UniFFI unmodified as a build tool and
    a library. MPL-2.0 is a file-level license, so it covers UniFFI's own files and any
    generated file derived from its templates, and it places no conditions on the rest of
    Sparkles or on programs that use the bindings. The obligations are to keep UniFFI's
    notices and to make the source of MPL-covered files available, which the public
    repository and the sources jar already do. Users of the bindings who redistribute the
    jars unmodified carry the same notices and nothing more. UniFFI is listed in the
    natives jar's third-party licenses and in the jar that holds the generated classes,
    and the generated files keep their license header.

## 12. Sources

* Apache Jena (Apache-2.0), the 6.3.0-SNAPSHOT source checkout:
  * in `jena-arq` and `jena-core`, `DatasetGraph`, `DatasetGraphBase`,
    `DatasetGraphBaseFind`, `DatasetGraphTriplesQuads`, `DatasetGraphWrapper`,
    `DatasetGraphWrapperView`, `DatasetGraphInMemory`, `GraphView`, `GraphBase`,
    `Transactional`, `TxnType`, `QueryEngineFactory`, `QueryEngineRegistry`,
    `QueryEngineBase`, `QueryEngineFactoryWrapper`, `QueryEngineMain`, `QueryExecDataset`,
    `QueryExecDatasetBuilder`, `UpdateEngineFactory`, `UpdateEngineRegistry`,
    `UpdateEngine`, `UpdateSink`, `NodeFactory`, `ARQ`, `FunctionRegistry`,
    `PropertyFunctionRegistry` and `ServiceExecutorRegistry`;
  * in `jena-db`, `DatasetGraphStorage`, `DatasetGraphTxnCtl`, `TransactionCoordinator`,
    `TransactionalBase` and `Transaction`;
  * in `jena-tdb2`, `QueryEngineTDB`, `UpdateEngineTDB`, `DatasetGraphTDB`,
    `DatasetGraphSwitchable`, `StorageTDB`, `TDBInternal`, `TDB2`, `InitTDB2`,
    `VocabTDB2`, `DatasetAssemblerTDB2`, `DatabaseMgr`, `StoreConnection`, `NodeIdInline`
    and `NodeTableTRDF`;
  * the test classes named in §7, TDB2's subclasses of them (`TestDatasetGraphTDB2`,
    `TestStorageDatasetGraphTests` and `TestTransPromoteTDB`), and the ARQ test runner's
    `QueryEvalTest` and `UpdateEvalTest`;
  * the `fuseki-server` script, and the release notes of Jena 5.0 to 6.2 in
    `CHANGES.txt` for the Java versions and the removals in 6.0.
* UniFFI (MPL-2.0): the user guide for procedural macros, errors, foreign traits, bytes,
  Kotlin configuration and Gradle, the Kotlin templates `ObjectTemplate.kt`,
  `ErrorTemplate.kt`, `NamespaceLibraryTemplate.kt`, `CallbackInterfaceImpl.kt` and
  `Async.kt`, the changelog, and the crates.io release list.
* JNA (Apache-2.0 or LGPL-2.1): `Platform.java` and `Native.java` for resource prefixes,
  library search and extraction, and the Maven Central release list.
* sqlite-jdbc (Apache-2.0): `SQLiteJDBCLoader` and `OSInfo` for extraction, overrides
  and musl detection, and the artifact list for its classifier jars.
* RocksDB's Java binding (Apache-2.0 or GPL-2.0): `NativeLibraryLoader` and `Environment`
  for extraction, library names and musl detection, and the artifact list.
* IronCoreLabs' `uniffi-bindgen-java` README and changelog.
* zakgof's `java-native-benchmark` results for JNA, JNI and FFM call costs.
* The Kotlin documentation on calling Kotlin from Java, Java interop and nullability
  annotations including JSpecify, and the Maven Central listings of `kotlin-stdlib`.
* JEP 454 (Foreign Function and Memory API) and JEP 472 (Prepare to Restrict the Use of
  JNI).
* The nixpkgs manual's section on Gradle and `gradle.fetchDeps`.
* Sparkles code and documents: `dataset.rs`, `store.rs` (the writer lock, `snapshot_at`,
  `WriteTxn`), `id.rs`, `history.rs`, `commit.rs`, `error.rs`, `sparql/mod.rs`
  (`QueryOptions`, `QueryResult`), `sparql/catalog.rs`, `sparql/ctx.rs`, the Python
  bindings in `crates/sparkles-py`, `.github/workflows/python-wheels.yml`,
  `scripts/bench.sh`, `scripts/bench-watdiv.sh`, `scripts/test-jena-clients.sh`,
  `testsuite/jena-clients` and `testsuite/jena-arq`, the "Migrating from Fuseki" section
  of `docs/USAGE.md`, and the specs P01, G06, G08, F06 and C01.

## Outcome

**Delivered.** The initial implementation landed on 2026-10-03 on Linux x86_64.
The integration and administration extension below follows on 2026-10-05. The initial
engine and Jena contract surface includes:

* the engine APIs of §2.3. `QueryOptions::union_default_graph` overrides the store's
  setting for one request, and the result cache keys on it. `sparkles::embed` in the
  facade has `QuadPattern` with `GraphMatch`, `Dataset::quads_in` over a given snapshot,
  `count_in`, `QuadIter::next_ids`, `Transaction::remove_matching`,
  `Transaction::snapshot` and `TxnWorker`. `TxnWorker::begin` takes the commit a promotion
  expects, and `TxnWorker::run` runs a closure in the transaction on the worker thread;
* `crates/sparkles-ffi`, a cdylib in its own cargo workspace with its own lock, on UniFFI
  0.32.2 with procedural macros. It has `FfiDataset`, `FfiReadTxn`, `FfiWriteTxn`,
  `FfiQuery` and `FfiCursor`, the batch encoding of §2.4 with Rust tests for every term
  kind, and the `uniffi-bindgen` binary behind the `bindgen` feature;
* `jvm/sparkles-jena`, the Kotlin library of §2.6 to §4.6 for Phase 1:
  `DatasetGraphSparkles`, its graph views, prefixes and blank node table, transactions
  with both kinds of promotion, the write buffer, the query and update engines with
  detector rules 1 to 3 and 6 and the late check, `SparklesDatasets`, `SparklesOptions`,
  the exceptions of §4.3, `loadFiles`, `load`, `bulkSink`, `lastReceipt`, `headCommit`,
  `stats` and `importTdb2`, the union default graph of §3.7, and the native loader of §6.3;
* the tests of §7 for Phase 1. Jena 5.6.0's contract suites run on Sparkles datasets from
  Jena's published test jars: `AbstractDatasetGraphTests` (32), `AbstractDatasetGraphFind`
  (20), `AbstractDatasetGraphFindPatterns` (16), `AbstractTestGraphOverDatasetGraph` (11),
  `AbstractTestGraphAddDelete` on the default and a named graph (16 each),
  `AbstractTestTransactionLifecycle` (51), `AbstractTestTransPromote` (25),
  `AbstractTestDynamicDataset` (27), `AbstractTestUpdateGraph` (29),
  `AbstractTestPrefixMappingView` (8) and jena-core's `AbstractTestGraph` (55). The
  acceptance examples A1 to A14, A18, A19 and A20 are tests, A3 with a second JVM;
* `jvm/sample-java`, compiled with `-Xlint:all -Werror` against the library;
* `mise run jvm:native`, `jvm:build`, `jvm:test`, `jvm:sample`, `jvm:lint`,
  `jvm:rust-test`, `jvm:lock` and `jvm:nix-deps`, with `jvm:lint`, `jvm:rust-test` and
  `jvm:test` in `mise run ci`. Java 17 and 21 and Gradle 9.7.1 come from mise, and the
  Gradle wrapper is checked in;
* `nix/jvm.nix` with `packages.sparkles-ffi`, `packages.sparkles-jena` and
  `checks.jvm-bindings`, a JDK 17 and Gradle 9 in the dev shell, and the Gradle
  dependencies pinned in `nix/jvm-deps.json` through `gradle.fetchDeps`;
* `jvm/sparkles-jena/THIRD_PARTY_LICENSES.md`, written by
  `scripts/third-party-licenses.py` and carried in the jar's `META-INF` with the project's
  license.

**Deviations and decisions.**

* Batches cross as `Vec<u8>` and `ByteArray`. UniFFI 0.32's Kotlin generator passes
  bytes through a `RustBuffer` in both directions and has no direct `ByteBuffer` for
  `&[u8]`, so each batch is copied once each way, as §2.4 allowed.
* The library override property is `uniffi.component.sparkles_ffi.libraryOverride`,
  because the component's name is the crate's namespace. The generated exception has one
  case, `Engine`, with the kind and a `detail` field, since Kotlin exceptions already
  have `message`. Cursors and queries free their state with `release()`, because a
  method named `close` clashes with the generated `AutoCloseable.close`.
* `FfiDataset` also has `find`, `count`, `contains`, `graph_names` and `prepare_query` on
  the head snapshot, so a read outside a transaction is one call. `begin_write` takes the
  commit a promotion expects and whether the transaction's blank node labels enter the
  dataset's table, which bulk loads turn off.
* Reads in a write transaction run on the caller's thread over a view of the
  transaction's state. The view is taken on the worker once after each change, so a run
  of reads costs one thread hop rather than one per read.
* `sparkles-jena` contains SDK and generated native bindings, while
  `sparkles-jena-natives` contains native resources and platform classifiers.
  `sparkles-jena-all` bundles the SDK, Kotlin/JNA/JSpecify runtime and native resources
  for Fuseki; Jena and SLF4J remain supplied by its host. Every bundled jar's legal
  notices are preserved under an artifact-specific directory in `META-INF/licenses`.
* The query engine accepts every query on a `DatasetGraphSparkles` and builds ARQ's plan
  itself when it falls back, rather than declining in `accept`. The fallback plan then
  applies the union default graph, as §3.7 asks.
* A seventh fallback rule sends a query to ARQ when its FROM or FROM NAMED names
  `urn:x-arq:UnionGraph` or `urn:x-arq:DefaultGraph`, or when it has FROM NAMED and a
  GRAPH on one of them. Jena gives these names a meaning in dynamic datasets that
  Sparkles does not share, and `AbstractTestDynamicDataset` covers it.
* Jena 5.6 signals a timeout by setting an `AtomicBoolean` in the query's context rather
  than by calling `QueryIterator.cancel`. One daemon thread checks the signals of the
  running native queries every 10 ms and cancels the one whose signal is set.
* Sparkles parses ARQ's `LET`, so A7's late check is tested with ARQ's two-argument
  `IRI()`, which Sparkles rejects.
* IRIs cross unchecked, because Jena accepts relative IRIs and the contract tests use them.
* Default native features are `text`, `geo`, `reasoning`, `shacl`, `shex`, `backup`
  and `graphql`. The same methods remain exported in a feature-disabled build and
  return an unsupported-feature exception.
* `Sparkles.NO_CACHE` turns the result cache off for a request, which the performance
  check uses.
* Three inherited tests are disabled with their reasons. `promote_active_writer_1`
  expects a promotion to fail after a writer that committed nothing, which §3.11 records
  as a difference. jena-core's `testContainsConcrete` and `testContainsNode` add
  generalized triples, which Sparkles refuses.
* The Nix build compiles the library and the generator in one cargo build with the
  `bindgen` feature, and runs the generator with `--metadata-no-deps` and the crate's
  `bindgen.toml`, which names the crate's root.
* Gradle dependency locks and SHA256 verification metadata pin the release graph.
  Dokka produces public HTML documentation and its Maven documentation jar. The
  main artifact currently supplies an automatic JPMS module name; an explicit
  descriptor remains deferred.

**Integration and administration extension.** The JVM SDK now includes:

* a Fuseki assembler (`urn:x-sparkles:assembler#DatasetSparkles`) and a client integration
  suite exercising ordinary Jena `RDFConnection` query, update, graph-store replacement,
  deletion and construction against reference memory datasets, embedded Sparkles and
  a real loopback Fuseki instance; the full 77-case standalone HTTP harness remains a
  separate expansion;
* Java-extension-aware ARQ fallback, registered IRI overrides, context/query hooks,
  `Op` execution, native DESCRIBE and execution counters. Fallback still uses Jena's
  ordinary graph access rather than the proposed batched BGP stage generator;
* point-in-time read handles and `Sparkles.AT`, commit history/status/diff/change pages,
  named snapshots, native streamed dumps, compaction, clones and repository backups;
* typed catalog handles, identity-aware dataset wrapper sharing, metadata inspection,
  reservations, clone/restore, repository registration, policy execution and retention;
  datasets keep their branches. Persistent rename requires all dataset handles to
  close, as the core directory-move contract requires;
* branch creation/deletion/rename/protection/notes, merge exemption settings, merge,
  revert and cherry-pick, including previews and structured conflict reports. Metadata
  ownership checks run before snapshot acquisition so a transaction cannot block on
  its own branch writer;
* compaction/quota/retention/DESCRIBE settings; versioned stored queries with checked
  parameters; schema reports, pages, diffs, profiles and shape drafts; versioned GraphQL
  configuration, SDL, drafts and execution;
* text/vector/geo index lifecycle methods, native reasoning and RDFS configuration,
  SHACL and ShEx result records and SHACL write guards; `SparklesOperation` provides
  cancellation, deadlines and progress. Nested administration documents use Jena JSON
  trees rather than requiring serialized JSON strings;
* reproducible host Nix packaging and release workflow jobs for Linux x86_64/aarch64,
  macOS x86_64/aarch64 and Windows x86_64, with aggregate natives, classifier and Fuseki
  bundle jars, source/docs artifacts, signed publishing behind a release gate, and
  Java17/Jena5.6 plus Java21/Jena6.2 compatibility jobs. The other operating systems
  are workflow targets; they have not been built locally on the Linux host.

The SDK's writer ownership checks cover aliases, branches and long captures. Dataset,
query, sink, historical view, repository and reservation handles have explicit close
semantics. Live handles on one native dataset share each thread's transaction, but a
historical view from `at()` keeps a transaction of its own, so a read transaction on the
view does not change what the live dataset reads or writes on that thread. When another
thread closes the dataset, its open write transactions are aborted, and a later `commit()`,
`add()` or buffered flush on them throws rather than succeeding silently.
`DatasetGraphSparkles.applyPatch` applies an RDF Patch in one commit and returns a
`PatchReport`, and the parity test checks that every concrete JVM and Node name in
`crates/sparkles/bindings.toml` is declared in the binding's sources. Bounded child-JVM tests check capture misuse without allowing a native
writer wait to hang the suite. Native default and feature-disabled builds are linted.

The administration helper surface also includes direct index iterators/diagnostics,
native cache/EXPLAIN control, standalone memory clone, schedule-preview and commit-DAG
documents, formatter/linter/RDF/term validation utilities, geometry conversion,
embedding regeneration and reasoning diagnostics. All 162 entries of the original
administration mapping now have concrete JVM decisions. The newer Rust branch-relink
operation remains an explicit binding follow-up. Java callbacks
await P03; batched fallback BGPs, musl, mimalloc and worker-free transactions remain
separate extensions. The earlier throughput numbers below are a loaded-machine sanity
check. No quiet-machine performance conclusion or JNI/allocator change follows from
concurrent implementation tests.

**Performance.** A sanity check, not the benchmark of §5.4, ran on 2026-10-03 with the
data of `scripts/bench.sh` at its default size (1,052,801 triples) and its 28 queries,
with the result cache off. Other work kept the machine's load average between 18 and 40,
so the numbers are rough. Each is the median of 10 runs after 10 warm-up runs, in
milliseconds. "Rust API" is `Dataset::query_with` with every cell decoded to a term
(`crates/sparkles-ffi/examples/perf.rs`). "Native" is the query through `FfiQuery` and
the batch decoder without Jena, and "Jena" is `QueryExec` on a `DatasetGraphSparkles`.
TDB2 is an in-memory TDB2 dataset with the same data in the same JVM
(`./gradlew :sparkles-jena:perfCheck`).

| Query | Rust API | Native | Jena on Sparkles | Jena on TDB2 | Rows |
|---|---|---|---|---|---|
| count-all | 0.03 | 0.36 | 0.99 | 1,034 | 1 |
| star-join | 4.68 | 3.61 | 5.77 | 45.3 | 2,916 |
| two-hop-count | 1.99 | 1.49 | 2.03 | 2,515 | 1 |
| group-avg | 9.56 | 11.9 | 13.2 | 575 | 10 |
| order-by-full | 60.6 | 70.9 | 80.0 | 514 | 102,000 |
| export-500k | 309 | 71.3 | 109 | 607 | 500,000 |
| knows-reach | 11.6 | 10.0 | 10.9 | 845 | 1 |
| star-lookup | 0.15 | 0.34 | 1.06 | 1.39 | 35 |
| values-star | 0.10 | 0.25 | 0.99 | 0.57 | 16 |
| employee-docs | 0.09 | 0.23 | 0.72 | 0.75 | 17 |

On queries that do real work, the time through Jena is within a few milliseconds of the
Rust API's. A large export is faster through Jena than through the Rust API, because the
term table decodes each distinct term once while the Rust check decodes every cell. The
fixed cost of a query through Jena is 0.6 to 0.9 ms, against the 50 µs of target T1.
About 0.3 ms of it is the native path and the rest is Jena's parsing, the query's
serialization and the plan, so the small-query work of Phase 2 starts there. TDB2 was
faster on `values-star` and about even on `employee-docs` and `path-plus`, and Sparkles
was faster on the other 25 queries.

**Release platforms (2026-10-09).** The release workflows now build only for Linux on
x86_64 and arm64 and for macOS on Apple silicon, on Namespace runners. The Windows and
Intel macOS jobs were removed to keep CI time down. The code paths for those platforms
remain, and they come back by adding their jobs again.

**Fine-grained calls (2026-10-09).** The binding comparison of `mise run
bench:bindings` counted 13 native calls per small query through Jena and found
`Graph.contains`, `Model.getProperty` and `find` on a bound subject 6 to 15 times
slower than TDB2, with poor scaling from 1 to 12 threads. Profiles showed little of
that time in Rust. UniFFI's call helper made a new call status record, a JNA
`Structure`, for every native call, and JNA registered its native memory in a global
table and with its cleaner until the collector found it. That bookkeeping and the
collector's work on it cost more than the calls. Three changes followed, and none
changes the transport.

* The build rewrites the generated call helper to take its status record from a
  per-thread pool (the `ffiBindings` task and `UniffiCallStatusPool`), and fails if the
  generated helper is not the expected one.
* A query result or `find` cursor that reported its last batch has freed its rows
  natively, so its `release` is skipped. A query makes 11 native calls instead of 13.
* The engine writes the query text without a base unless the query has `BASE`, which
  saves jena-iri's attempt to make every IRI relative to Jena's system base, and runs
  ASK as ASK, where it ran `SELECT * LIMIT 1`.

In A/B/B/A comparisons on a busy laptop at 105,000 triples in memory, one thread,
`Graph.contains` with its own read transaction went from 34 to 11 µs, `getProperty`
from 40 to 16 µs, `find` on a bound subject from 63 to 20 µs, and the small `SELECT`
from 152 to 83 µs. On four threads, `contains` went from 44,000 to 210,000 operations
per second with a p99 of 0.03 ms instead of 0.12 ms, `find` from 26,000 to 120,000, and
the small `SELECT` from 12,800 to 39,000. These figures are directional, and a
quiet-machine run of §5.4's benchmark is the authority. After these changes the §5.4
rule for moving a call to JNI is still met on `contains`, `getProperty` and `find`:
TDB2 and TIM are faster, and the bridge, not Rust, takes most of their time. No call
has been moved.

**Hand-written JNI calls (2026-10-09).** Four groups of calls now go through
hand-written JNI entry points instead of UniFFI and JNA. They are beginning and freeing a
read transaction, `contains`, `find` with its cursor's later batches and the cursor's
free, and preparing, executing and freeing a query, each on the head snapshot or in a
read transaction. Every other call stays on UniFFI, and the public Kotlin and Java API
did not change.

The entry points are in the same native library (`crates/sparkles-ffi/src/jni_calls.rs`),
so the natives jars and their platforms are unchanged. `SparklesJni` loads the file that
UniFFI loaded a second time with `System.load`, which gives the same library instance
and lets the JVM bind the `Java_` symbols. The entry points call the same Rust methods as
the UniFFI exports and pass the same bytes, including UniFFI's own serialization of
`QueryOpts` and `Execution`, so only the transport changed. They work on the objects
UniFFI made. A call borrows the object's handle while it holds the object's UniFFI call
counter, which is UniFFI's own lifetime rule without the handle clone that costs a call
of its own. A concurrent `close` therefore frees the object only after the call. The
read transactions, cursors and queries that the entry points create are handed out as
UniFFI handles and wrapped in the generated classes, so they keep UniFFI's lifecycle. The
`ffiBindings` task adds the borrowing helper to every generated class and sends the
frees of these three classes through JNI, and it fails the build when the generated code
changes shape. Each entry point runs inside `catch_unwind`. An error crosses as the
`FfiError` in UniFFI's serialization and is thrown as the same `FfiException`, and a
panic is thrown as UniFFI's `InternalException`. Cancellation and the memory budget run
through the same `FfiQuery`, so they behave as before, and the suite's timeout and
budget tests pass on both paths. The system property `sparkles.jni=false` turns all of
it off, and a list such as `sparkles.jni=read,contains` turns on only the named groups.
JNI is also off when the library cannot be loaded for it, as when another class loader
in the JVM has loaded it. `mise run jvm:test` runs the suite with the JNI calls and again
without them (`testUniffi`).

Following §5.4's rule, each group was measured on its own against the UniFFI path on
atlas, with the data of `mise run bench:bindings` at 10,000 people (105,302 triples) in
memory. Each arm ran in a fresh JVM pinned to CPUs 0 to 11, in A/B/B/A order twice, and
the figures are the medians of the four runs' median latencies on one thread, in
microseconds. Before is every group off and after is every group on. The share is the
time that moving the group alone to JNI removed, as a fraction of the time before.

| Group | Operation | Before | That group alone | After | TDB2 | TIM | Share |
|---|---|---:|---:|---:|---:|---:|---:|
| read transaction | `Graph.contains` | 9.91 | 7.24 | 1.68 | 2.47 | 0.53 | 27% |
| `contains` | `Graph.contains` | 9.91 | 4.25 | 1.68 | 2.47 | 0.53 | 57% |
| `find` | `Model.getProperty` | 14.29 | 5.27 | 2.64 | 3.46 | 0.68 | 63% |
| `find` | `find` on a subject | 17.61 | 7.75 | 5.20 | 5.59 | 1.71 | 56% |
| query | `ASK` | 61.75 | 46.21 | 42.69 | 28.75 | 26.18 | 25% |
| query | `SELECT ?o` | 60.85 | 45.96 | 42.61 | 27.48 | 23.92 | 24% |
| query | `star-lookup` | 272.2 | 237.4 | 233.7 | 209.8 | 115.5 | 13% |
| query | `values-star` | 232.1 | 204.6 | 195.8 | 101.7 | 88.9 | 12% |

The binding was slower than TDB2 and TIM on every operation before, and each group
removed more than 10% of an operation's time on its own, so every group is kept. A bare
UniFFI call measured 0.70 µs on the same machine and a JNI call 0.01 µs. Native calls
per operation went from 6 to 3 for `contains` with its own read transaction, from 7 to 3
for `getProperty` and `find`, and from 11 to 5 for a small query, all of them through
JNI. The bridge's share after the change is then well under 1% of a small query and
about 2% of `contains`, which meets T7's 10%.

`contains`, `getProperty` and `find` on a subject are now faster than TDB2. On four
threads, `contains` went from 270,000 to 1,680,000 operations per second (TDB2 678,000),
`getProperty` from 170,000 to 1,130,000 (TDB2 657,000) and `find` on a subject from
149,000 to 704,000 (TDB2 446,000). Small queries remain slower than TDB2, by about 14 µs
on `ASK` and `SELECT ?o`, and on four threads `ASK` went from 37,800 to 43,200 queries
per second against TDB2's 45,500. That gap is not the bridge. It is Jena's
serialization of the query, Sparkles' parsing and planning of the text, and the eager
`NOW()`, which the next steps for small queries address.

**Realistic Jena work (2026-10-09).** `mise run bench:jena` measures 32 cases of ordinary
Jena use on Sparkles, TDB2 and TIM at one and four threads. Its runner is
`scripts/bench-bindings/jena.py`, and the cases are in
`jvm/sparkles-jena/src/test/kotlin/io/github/kclejeune/sparkles/jena/bench/JenaUseCases.kt`.
The cases are Model and Graph calls, Jena's RDFS reasoner over the
dataset, small SPARQL SELECT, ASK, CONSTRUCT and DESCRIBE queries, ParameterizedSparqlString,
substitution and initial bindings on a parsed query, small updates and Model writes in
transactions, a bulk load with RDFDataMgr, and queries through Fuseki. Every arm runs in
fresh JVMs in A/B/B/A order, and a check process per arm fingerprints the answers first.
TDB2 differs alone where salaries appear, because it writes inlined `xsd:decimal` literals
in canonical form. `scripts/nsc-jena.sh` runs the comparison on an ephemeral Namespace
instance. The figures below come from 8-vCPU AMD Zen 4 (EPYC) instances with 105,000
triples in memory, two timing processes per arm and 5 seconds per case. They are
directional, and runs on different instances differed by up to 30% at four threads.

Five changes came out of it.

* Small queries run in ARQ over the JNI `find` when an estimate says that is cheaper
  (`SmallQueries`). A query qualifies when its pattern is a basic graph pattern of up to
  eight triples, with an optional leading VALUES block, filters made of comparisons,
  logic and arithmetic other than division, and COUNT as its only aggregate. ARQ is
  estimated to make at most 32 `find` calls for it. A pattern on a bound subject is
  assumed to match two triples, and a pattern with an unbound subject and a constant
  predicate and object is counted in the index with one native call. Only forms whose
  answers the SPARQL specification fixes qualify, and the tests compare ARQ's answers with
  Sparkles' engine query by query. RDFS on read, the inferred graph, row and memory
  budgets and write transactions keep a query in Sparkles. The system property
  `sparkles.smallQueries` sets the limit, and 0 turns the routing off. A limit of 64 was
  slower than 32 on the cases between the two.
* DESCRIBE of named resources runs one native query instead of two. ARQ finds the one
  empty solution itself, and the DESCRIBE handler writes its query text directly and runs
  it over JNI. The description still comes from the engine, so the configured DESCRIBE
  strategy applies.
* The parser shares the set of custom aggregate IRIs instead of building it for every
  query, which took 3.5% of a small star lookup.
* A `find` pattern is encoded without the table that lets a batch send a repeated term
  once. Encoding fell from 13% to 7% of a Model navigation's time.
* The native library has an opt-in `mimalloc` feature. With it a bulk load took 37 ms
  instead of 61 ms and small Model writes about 15% less time, and reads moved within the
  noise. The process's peak RSS rose from 653 to 962 MiB on a mixed run, so the feature is
  off by default.

Against TDB2 at one thread, Sparkles is now faster on every Model and Graph case, on
RDFS, on ASK, SELECT, CONSTRUCT, star lookups, COUNT and parameterized queries, and on
small writes and bulk loads. These are the medians of the last run, with the routing
turned off for the earlier figure.

| Case | Before | After | TDB2 |
|---|---:|---:|---:|
| `ASK` on a bound triple | 41 µs | 25 µs | 27 µs |
| `SELECT ?o` on a subject | 42 µs | 22 µs | 23 µs |
| friends and their names | 98 µs | 54 µs | 64 µs |
| COUNT of an organization's employees | 89 µs | 74 µs | 94 µs |
| initial binding on a parsed query | 26 µs | 10 µs | 12 µs |
| DESCRIBE of one resource | 120 µs | 52 µs | 36 µs |

The DESCRIBE figure before is from the first run, which preceded that change. At four
threads Sparkles is ahead on the Model calls, star lookups and writes. The remaining
losses have these causes.

* `update-modify`, a DELETE/INSERT WHERE on one resource, takes 13 to 15 ms on the
  in-memory store against 2.3 to 3.2 ms for TDB2, and 2.4 ms on Sparkles' disk store.
  The time is in the native update, begin and commit calls, and it grows with the number
  of CPUs the JVM may use. This is the engine's write path and not the binding.
* VALUES over five subjects and a FILTER over an organization's employees stay in
  Sparkles' engine and take 160 to 200 µs against TDB2's 115 to 160 µs. Their estimates
  are 35 and 36 `find` calls, and in ARQ they ran no faster. The cost is Sparkles'
  parsing and planning of each new query text. A plan cache keyed by the query text would
  not help, because each operation's text differs.
* DESCRIBE still costs about 15 µs more than TDB2, which is the one native query and the
  engine's DESCRIBE strategy.
* Jena's RDFS reasoner serializes its callers, so at four threads no engine scales.
  Sparkles' longer `find` holds that lock longer and reaches 0.6 to 0.7 of TDB2.
* Small queries at four threads vary by more than the gap between engines on these
  instances, so they are ties at best. NOW() is still computed eagerly for every query.
