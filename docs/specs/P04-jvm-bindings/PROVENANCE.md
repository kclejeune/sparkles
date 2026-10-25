# JVM bindings

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

See the [spec landing page](README.md), [design](SPEC.md) and
[delivery record](OUTCOME.md) for their respective contracts.

- **Spec:** [`P04-jvm-bindings.md`](README.md), written on 2026-10-03 from:
  - the Sparkles code, the Python bindings in `crates/sparkles-py`, the Python wheels
    workflow, the bench scripts, the Jena client harness and the ARQ test data in
    `testsuite`, and the P01, G06, G08, F06 and C01 specs;
  - Apache Jena's source (Apache-2.0), a 6.3.0-SNAPSHOT checkout: the `DatasetGraph`
    hierarchy, `GraphView` and `GraphBase`, `Transactional`, `TxnType` and TDB2's
    transaction coordinator, the query and update engine factories and registries,
    `QueryExecDataset`, the function, property function and service registries,
    TDB2's query and update engines, storage, inline node ids, node table, assembler
    and subsystem initialization, the abstract contract tests for datasets, graphs and
    transactions with TDB2's subclasses of them, the ARQ manifest test runner, the
    `fuseki-server` script, and the release notes of Jena 5.0 to 6.2;
  - the UniFFI user guide, its Kotlin templates and changelog (MPL-2.0), and the
    crates.io release list;
  - JNA's `Platform.java` and `Native.java` (Apache-2.0 or LGPL-2.1) and its Maven
    Central release list;
  - the native library loaders of sqlite-jdbc (Apache-2.0) and of RocksDB's Java binding
    (Apache-2.0 or GPL-2.0), and their Maven Central artifact lists;
  - the README and changelog of IronCoreLabs' `uniffi-bindgen-java`;
  - zakgof's `java-native-benchmark` results;
  - the Kotlin documentation on Java interoperability and nullability annotations, and
    the Maven Central listing of `kotlin-stdlib`;
  - JEP 454 and JEP 472;
  - the nixpkgs manual's section on Gradle.
- **Implementation:** not started.
- **Planned dependencies:** `uniffi` 0.32 (MPL-2.0) and its support crates, linked into
  the native library, with the bindings generator built from the same crate for the
  build only. The crate keeps its own workspace and lock, so none of them enters the
  root lock or the `sparkles` binary. On the JVM, `kotlin-stdlib` (Apache-2.0), JNA 5.19
  (Apache-2.0 or LGPL-2.1) and JSpecify 1.0 (Apache-2.0) are runtime dependencies, Apache
  Jena (Apache-2.0) is an API dependency that the user's program provides, and
  `jena-tdb2` (Apache-2.0) is optional, for the import. Gradle, the Kotlin Gradle plugin
  and Dokka (Apache-2.0) are build tools, and JUnit (EPL-2.0), Jena's test jars
  (Apache-2.0) and JMH (GPL-2.0 with the Classpath Exception) are used for tests and
  benchmarks only and never shipped.
- **Rejected** (spec §10):
  - plain Java with hand-written JNI through the `jni` crate;
  - Java with the Panama foreign function API, which needs Java 22;
  - a remote client only;
  - GraalVM interop;
  - implementing TDB2's storage SPI;
  - offloading only basic graph patterns, as TDB2's query engine does;
  - a Rust reader of TDB2's files;
  - RDF Thrift or Apache Arrow as the batch encoding, and a UniFFI record per term;
  - Kotlin coroutines in the API.
