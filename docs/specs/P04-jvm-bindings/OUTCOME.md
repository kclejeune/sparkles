# P04: JVM bindings behind Apache Jena's API — Delivery record

Current delivery status is summarized in the [landing page](README.md).
The [design Outcome overview](SPEC.md#outcome) links the supported scope to this record.
The recorded dates and measurements below describe their original delivery stages.

## Historical opening status

> **Status:** implemented in part (integration and primary administration)
>
> **Phases:** The engine/Jena contract surface, Fuseki assembler, historical reads,
> dumps, native DESCRIBE and refined ARQ fallback are implemented. Typed catalog,
> branches, backups, settings, history, indexes, stored queries, schema, GraphQL,
> reasoning and validation extend the SDK. Release workflows assemble three native
> platforms (Linux on x86_64 and arm64, macOS on Apple silicon), classifier jars, a Fuseki bundle and source/documentation artifacts;
> Linux is validated locally. Administration helpers are implemented; batched fallback graph patterns,
> Java callbacks, musl and comparative performance targets remain follow-up work.
> The [Outcome](SPEC.md#outcome) records the supported scope and deferrals.
>
> **User docs:** [Usage: JVM](../../USAGE.md#jvm-apache-jena) ·
> [Features](../../FEATURES.md#known-gaps) · [Development](../../DEVELOPMENT.md#jvm-bindings)

## Outcome

The JVM algorithm handle exposes projection list, get, put and delete, query text, query execution, statistics and write-back. Write-back uses batched commits and can retain earlier batches after a later failure. Further algorithm surfaces and acceptance remain under roadmap item 8.

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
* The native library uses mimalloc for its own allocations by default. With it a bulk
  load took 37 ms instead of 61 ms and small Model writes about 15% less time, and reads
  moved within the noise. The cost is memory, because the process's peak RSS rose from
  653 to 962 MiB on a mixed run. The maintainer chose speed, so `mimalloc` is a default
  feature, and a build with `--no-default-features` plus the features it needs keeps the
  system allocator.

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

**Small queries and the query text (2026-10-10).** A further round profiled the small
queries that stay in Sparkles' engine, with Java Flight Recorder sampling every
millisecond, on the in-memory store against TDB2 and TIM. One change followed.

* `QueryEngineSparkles` wrote each query's text through Jena's `IndentedLineBuffer`,
  which appends one character at a time to a synchronized `StringBuffer` that starts at
  16 characters. That was 8.7% of the `VALUES` query over five subjects. The text now
  goes through an `IndentedWriter` over a plain `StringBuilder` of 1,024 characters. On
  a quiet laptop, in A/B/B/A runs of fresh JVMs, the `VALUES` query went from 133 to
  125 µs and the star lookup from 188 to 181 µs.

The profile of the `VALUES` query splits its time as follows. Jena's own parsing of the
query text is 54%, or about 70 µs, and every engine pays it, because each operation
builds a new query. The native call is about 31%, or 40 µs, which is Sparkles' parsing,
planning and evaluation. The text serialization and the conversion of the solutions
make up the rest. The query's estimate is 35 `find` calls, above the limit of 32 for
`SmallQueries`, and the star lookup's estimate is above it too, so both stay in the
engine.

Two other changes were considered and not made. Handing Jena's algebra to the engine
instead of the query text would save Sparkles' parsing, about 8 to 10 µs, and the
serialization, but it needs a translation of the whole algebra into the engine's query
form. A cache of parsed queries keyed by the text would not hit, because each operation's
text names other resources. A plan cache that treats constants as parameters, as P01's
Outcome designs, removes both the parsing and the planning, about 50 of the 62 µs that
the engine spends on such a query.

`mise run bench:jena` ran once more on an 8-vCPU AMD EPYC instance on Namespace with the
change, at one thread, with two timing processes per arm. Against TDB2, Sparkles is
faster on every Model and Graph call, on RDFS, on `ASK` and the small `SELECT`s, on the
star lookup (231 against 258 µs), on substitution and initial bindings, on small writes
and on the bulk load. It ties on the friends query, CONSTRUCT, the parameterized query
and `update-modify`. It is behind on the `VALUES` query (185 against 119 µs), the
filtered employees (243 against 139 µs), the COUNT (94 against 81 µs), DESCRIBE (59
against 34 µs) and the two Fuseki cases (1.6 and 2.3 ms against 1.3 and 1.5 ms). TIM is
faster than both on every case except iterating every triple and the repeated query,
because it answers from Java maps without crossing JNI. On the laptop, quiet and in
memory, Sparkles took 127 µs on the `VALUES` query against 111 µs for TDB2 and 83 µs
for TIM, 185 µs on the star lookup against 341 and 126 µs, and 42 to 44 µs on `ASK` and
`SELECT ?o` against 40 to 46 µs for the other two.

`update-modify` was measured without changes. It took 4.2 ms on the in-memory store
against 3.9 ms for TDB2 and 0.07 ms for TIM, down from the 13 to 15 ms of the previous
round. The time is still in the engine's write path.

### Graph overview

`DatasetGraphSparkles.graph().groups(request, options)` binds the library's
`ds.graph().groups` from [U06](../U06-large-graph-rendering.md#outcome). The request is a
`GroupsRequest` with a `GroupsWithin` that is `OfClass` or `OfGroup`, a `GroupsBy` that is
`Auto`, `Members`, `Community`, `Property` or `Reports`, a `limit`, and an `at` that takes `commit:N`,
`time:` or `snapshot:` points. `Reports` groups the whole dataset by its GraphRAG communities
from [C22](../C22-graph-rag-community-summaries.md), and it is the one grouping whose request leaves `within`
null. A call reads what the thread's other reads do, which is the head,
the snapshot of its read transaction, the view of its write transaction with the changes so far,
or the commit that a dataset pinned with `at(...)` holds. Only a call on the head can name a point
in history in `at`. The default graph is the union of the named graphs when the dataset's setting
or its context says so, as for a query. `GroupsOptions` holds the options of a query that bear on the view and
the budgets, which are `defaultGraph`, `namedGraphs`, `includeInferred`, `maxRows`,
`maxMemoryBytes`, `maxRowsProduced` and `noCache`. The timeout and the cancellation belong
to the `SparklesOperation` of the third form, and its timeout is one deadline for all of
the overview's queries, not a timeout for each. The answer is the object of API.md as a
Jena `JsonObject`, like the other structured administration calls, and a malformed request
throws `SparklesInvalidException`. The native call is `FfiDataset::graph_groups`. The JVM
binding has no limit on concurrent queries, so unlike the Node.js call this one waits for
no permit.

### Roadmap follow-ups (2026-10-12)

* **Reusable plans** ([ROADMAP](../../ROADMAP.md) item 32). [X10](../X10-reusable-query-plans.md)
  adds `FfiPlan`: `prepare_plan` parses once, and `FfiPlan::bind` returns an `FfiQuery`
  that instantiates a cached plan with new bindings. `QueryEngineSparkles` keeps a map
  from a parsed Jena `Query` to its `FfiPlan`, so substitution on a parsed query skips
  Sparkles' parsing and planning. Text from `ParameterizedSparqlString` hits X10's shape
  level after a parse. The Outcome above attributes about 50 of the 62 µs that the engine
  spends on a small query to parsing and planning, which this removes on a hit.
* **Per-call overhead** (item 33). After the hand-written JNI calls, single-thread medians
  on atlas were 1.68 µs for `Graph.contains` against TIM's 0.53, 2.64 µs for
  `getProperty` against 0.68 and 5.20 µs for `find` on a subject against 1.71, about 3 to 4
  times TIM. TIM answers from Java maps without a native call. Sketch, each change kept
  only if it removes more than 10% of an operation under the rule of §5.4:
  * *Fewer crossings.* A `contains` or `find` outside a transaction makes separate calls
    to begin a read transaction, to look up and to free it. One JNI entry point on the
    head snapshot does all three.
  * *Cached term ids.* A Kotlin cache from Jena `Node` to the engine's id, keyed by the
    generation's identity and the delta vocabulary's length, lets a pattern cross as 8-byte
    ids instead of encoded terms. A miss or a changed generation falls back to the term.
  * *Batched calls.* `containsAll` and a `find` over several subjects in one call, used
    by `SmallQueries` when ARQ asks for a run of patterns, return their answers in one
    buffer.
* **JVM metrics** ([ROADMAP](../../ROADMAP.md) item 76). [FEATURES](../../FEATURES.md) lists no
  JVM metrics. The server's metrics are counted in `crates/sparkles-server/src/obs.rs` and
  are not reachable from the library, and the FFI exposes no dataset statistics. The
  library already has the counters: `Dataset::stats` (`crates/sparkles/src/stats.rs`)
  returns quads, delta inserts and deletes, disk bytes and the block, result and SERVICE
  cache counts, and the store has `wal_bytes`, `disk_bytes`, `Store::rebuilds(reason)` and
  the compaction status. Sketch:
  * *FFI.* `FfiDataset::metrics()` returns a record of these counters only, without the
    lists of graphs, predicates and classes that `DatasetStats` also computes, so a scrape
    is a few atomic loads and stays cheap enough to call every few seconds.
  * *Kotlin counters.* The library counts what only the JVM side sees: queries and updates
    by outcome, their durations, budget failures by kind, queries routed to ARQ by
    `SmallQueries` and fallbacks to ARQ, and calls by bridge (JNI or UniFFI).
  * *Micrometer.* An optional `SparklesMetrics : MeterBinder`, with Micrometer as a
    `compileOnly` dependency, registers gauges and function counters tagged with the
    dataset's name. Their names follow the server's (`sparkles.block.cache.bytes` for
    `sparkles_block_cache_bytes`), so the same dashboards work. The Fuseki assembler binds
    it to Fuseki's Micrometer registry when one is present.
  * *JMX.* `SparklesJmx.register(dataset)` registers a `SparklesDatasetMXBean` with the
    same values under `io.github.kclejeune.sparkles:type=Dataset,name=<name>`, for
    programs without Micrometer.
  With [X08](../X08-disk-spill-and-server-memory.md), the cache gauges become catalog-wide,
  as on the server.
* **Release work** (item 58), designed in [X12](../X12-release-engineering.md):
  * *Maven Central.* `jvm-bindings.yml` has a publish job for `jvm-v*` tags behind the
    repository variable `MAVEN_PUBLISH`, which uploads through the Central Portal's staging
    API and leaves the deployment for a person to release. Switching it on needs the
    namespace, signing keys and X12's checklist.
  * *musl.* The natives jar gains `linux-x86_64-musl` and `linux-aarch64-musl`
    directories, built in an Alpine container, and the loader, which already detects musl
    and refuses, picks them instead.
  * *JPMS descriptor.* The main artifact supplies an automatic module name. An explicit
    `module-info.class` would require JNA's and Jena's module names and export the
    public packages only, and it needs a test that runs the library on the module path.

### Remaining integration contract

Typed branch relink is available, as the later
[F09 Outcome](../F09-branches-and-merges.md#outcome) records. Earlier omissions above
describe the original landing. Roadmap item 76 owns batched ARQ fallback graph
patterns, JVM metrics and expansion of the standalone HTTP harness to its
accepted 77-case scope. Acceptance compares supported native and fallback queries
with reference Jena datasets, including blank nodes, transactions, access and
cancellation, and checks that batching preserves duplicate solutions and filters.

Reusable plans, native foreign callbacks and per-call performance remain items 32,
25 and 33. Publication and platform verification remain item 58. The optional explicit
JPMS descriptor, allocator changes and worker-free transaction design need recorded
scope decisions under 76 before being treated as part of a completion claim. A
proposal to remove an accepted requirement needs maintainer approval. This avoids
mistaking every optional JVM engineering alternative for required Jena parity.
