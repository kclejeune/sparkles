# Development

This page covers building Sparkles from source, the everyday tasks, the test suites, the
Nix flake and the third-party license notices. [USAGE.md](USAGE.md) covers running and
operating the binary. The UI has its own development notes in
[ui/README.md](../ui/README.md). The design specs in [specs/README.md](specs/README.md)
explain why each feature is built the way it is.

Sparkles is experimental. The on-disk format, HTTP API, CLI and Rust API may change
between commits without a migration path.

* [Project layout](#project-layout)
* [Building from source](#building-from-source)
* [mise tasks](#mise-tasks)
* [Git hooks](#git-hooks)
* [Testing](#testing)
* [Benchmark scripts](#benchmark-scripts)
* [Continuous integration](#continuous-integration)
* [Releases](#releases)
* [Nix](#nix)
* [Third-party licenses](#third-party-licenses)

## Project layout

[ARCHITECTURE.md](ARCHITECTURE.md) explains how these components fit together.

| Path | Role | Jena analogue |
|---|---|---|
| `crates/sparkles-core` | The engine: ids, vocabulary, indexes, bulk builder, MVCC store and WAL, SPARQL and RDF I/O | jena-core, jena-arq, jena-tdb2 |
| `crates/sparkles` | The library: the engine's modules, the `Dataset` API, its handles and the query builder | jena-querybuilder, in-process RDFConnection |
| `crates/sparkles-server` | The HTTP server and the `sparkles` CLI | jena-fuseki2, jena-cmds |
| `crates/sparkles-alloc` | The mimalloc global allocator of the server, the bindings and the memory examples | — |
| `crates/sparkles-reasoner` | RDFS, OWL 2 RL and Jena rules by semi-naive forward chaining | jena-core `reasoner` |
| `crates/sparkles-shacl`, `crates/sparkles-shex` | SHACL and ShEx validation over store snapshots | jena-shacl, jena-shex |
| `crates/sparkles-backup` | Backup repositories on a file system or S3 | Fuseki `/$/backup` |
| `crates/sparkles-graphql` | The read-only GraphQL adapter | — |
| `crates/sparkles-fmt`, `crates/sparkles-fmt-wasm` | The formatter and linter, and their WebAssembly build | — |
| `crates/sparkles-client` | The Rust client of remote SPARQL endpoints | jena-rdfconnection (remote) |
| `crates/sparkles-py` | The Python package (PyO3, its own cargo workspace) | — |
| `crates/sparkles-ffi`, `jvm/` | The JVM bindings: the UniFFI native library and the Kotlin `sparkles-jena` library | jena-tdb2's `DatasetGraphTDB` |
| `crates/sparkles-node`, `js/` | The Node-API addon, RDF/JS contracts and TypeScript engine and remote client packages | — |
| `vendor/spargebra` | Oxigraph's SPARQL parser, vendored with fixes ([PATCHED.md](../vendor/spargebra/PATCHED.md)) | ARQ's grammar |
| `ui/` | The SvelteKit web UI | jena-fuseki-ui |

## Building from source

```sh
pnpm -C ui install && pnpm -C ui build        # optional: the UI is embedded at compile time
cargo build --release
./target/release/sparkles serve --data ./data --port 3030   # UI at http://localhost:3030/ui/
```

`rust-toolchain.toml` pins Rust and installs the `wasm32-unknown-unknown` target.

The formatter's WebAssembly module lets the UI format text in the page. It is optional.
`mise run ui:wasm` builds `crates/sparkles-fmt-wasm` into `ui/src/lib/wasm/`, which is
git-ignored, and later UI builds keep it up to date. Without the module, the UI formats
through `POST /$/format`. The Nix packages and the Docker image always build it in. See
[ui/README.md](../ui/README.md#formatting-in-the-browser).

The `Dockerfile` builds the module, the UI and the release binary in stages, without
Node, pnpm or Rust on the host. `mise run docker:build` builds the image of
`compose.yaml`, and [USAGE.md](USAGE.md#docker) covers running it.

## mise tasks

[`mise.toml`](../mise.toml) pins Node, pnpm, hyperfine, prek, shfmt, shellcheck, a Java 17
and a Java 21 JDK, and Gradle. Rust comes from `rust-toolchain.toml`. `mise.toml` also defines the everyday tasks, and
`mise tasks` lists them all:

```sh
mise run build        # UI + release binary
mise run serve        # build, then serve ./data on :3030
mise run fmt          # cargo fmt + oxfmt        (fmt:check for CI)
mise run lint         # clippy -D warnings + svelte-check
mise run lint:features # clippy -D warnings over feature combinations (in ci)
mise run lint:doc-paths # every crates/sparkles… path cited in the docs exists (in ci)
mise run fmt:wasm     # clippy for the formatter's wasm32 build (in ci)
mise run fmt:fuzz     # fuzz the formatter with cargo-fuzz (needs nightly and cargo-fuzz; not in ci)
mise run office:fuzz  # fuzz the Office converter of ingestion the same way
mise run test         # all Rust tests with cargo-nextest, then doctests (test:cargo runs plain cargo test)
mise run ui:wasm      # the formatter's WebAssembly module for the UI (optional)
mise run ui:dev       # UI dev server, proxying the API to $SPARKLES_API (default http://localhost:3030)
mise run ui:mock      # mock API server for UI work without the Rust backend
mise run ui:test      # UI unit tests (Vitest)
mise run ui:e2e       # UI end-to-end tests (Playwright; Chromium from `nix develop`, see below)
mise run ui:e2e:mock  # UI end-to-end tests against the mock backend
mise run py:build     # the Python wheel (crates/sparkles-py) into target/wheels, with maturin
mise run py:sdist     # the Python source distribution into target/wheels
mise run py:test      # build the Python extension and run its pytest suite (in ci, with py:lint)
mise run py:lock      # refresh crates/sparkles-py/Cargo.lock from Cargo.lock
mise run jvm:build    # the sparkles-jena jar (jvm/sparkles-jena/build/libs), with the native library
mise run jvm:test     # Jena's contract tests and the binding's own tests (in ci, with jvm:lint and jvm:rust-test)
mise run jvm:sample   # compile, test and run the Java sample (jvm/sample-java)
mise run jvm:lock     # refresh crates/sparkles-ffi/Cargo.lock from Cargo.lock
mise run jvm:nix-deps # refresh nix/jvm-deps.json after the Gradle dependencies change
mise run ci           # every check below, with a log per task and a summary (scripts/ci.sh)
mise run ci:fast      # fmt:check + lint:doc-paths + lint + test, for iterating
mise run doc          # API docs of the library crates
mise run openapi      # rewrite docs/openapi.json after an API change (a test fails until then)
mise run docs:screenshots # the README's screenshots (docs/images): the demo dataset in docs/demo, and the mock backend for branches and the agent pages
mise run docs:screenshots:mock # only the branch and agent screenshots, from the mock backend, without a release build
mise run docker:build # the Docker image of compose.yaml (not in ci)
mise run gen-data 1000000 target/bench-data/10m.nt
mise run bench        # Sparkles vs Fuseki, QLever, Fluree and Oxigraph; `bench 1000000 --runs 5` for 10.5M triples
mise run bench:text   # full-text search: Sparkles vs Fuseki with jena-text vs QLever
mise run bench:watdiv # the same engines on WatDiv's 20 query templates, 11M triples
mise run bench:shacl 100000; mise run bench:reasoner 100000 owl-rl
mise run bench:shacl-write 100000   # 1-triple INSERT DATA latency with validation off / warn / reject
mise run bench:nsc main HEAD        # interleaved A/B of two revisions on a Namespace instance (needs nsc)
mise run licenses     # regenerate the third-party notices after a Cargo.lock or UI dependency change (licenses:check)
```

## Git hooks

The Git hooks are defined in [`.pre-commit-config.yaml`](../.pre-commit-config.yaml) and
run with [prek](https://github.com/j178/prek). Plain `pre-commit` reads the same file. On
staged files, the hooks run:

* `cargo fmt`;
* oxfmt, the UI's formatter (a devDependency), on `ui/`;
* `nix fmt` (nixfmt, from the flake) on `*.nix`;
* shfmt and shellcheck on shell scripts.

Run `mise run hooks:install` once per clone to install the hook. `mise run hooks:run`
runs every hook over the whole tree.

## Testing

```sh
mise run ci            # formatting, clippy, all workspace tests, UI, Python, JVM and JavaScript tests, license notices
mise run ci:fast       # formatting, cited doc paths, workspace clippy and the Rust tests
mise run lint:features # clippy over feature combinations (in ci)
mise run test:w3c      # W3C SPARQL 1.0 / 1.1 query / 1.1 update / 1.2 suites, with a summary
mise run test:shacl    # W3C SHACL Core and SHACL-SPARQL suites, SHACL 1.2 list tests, SHACLC pairs
mise run test:shex     # shexTest: syntax, negative syntax and structure, representation, ShExR, validation
mise run ui:e2e        # Playwright end-to-end tests against a real server
mise run test:jena-clients  # Apache Jena's own HTTP clients against a real server
```

`mise run ci` builds the UI, the Node addon and the JVM native library once, then runs
the checks with the cheap ones first. It prints a line as each task passes or fails, and
it writes each task's output to `target/ci/logs/<task>.log`. While it runs,
`target/ci/summary.txt` shows which tasks are waiting, running, passed or failed, with
their times. Every task runs to the end, and the final summary repeats the last lines of
each failed task's log. `--fail-fast` stops after the first failure, `--jobs N` runs more
tasks at once, and naming tasks runs only those, as in `mise run ci lint test`. Only
one run at a time can use a checkout. `test` and `node:test` take turns whatever the
`--jobs` value, because each links `target/debug/sparkles` with other features. The
binary of `test` has the formatter's fault injection, which its refused-output tests
need, and a concurrent `node:test` used to replace it with a build without it.

The checks declare their input files in `mise.toml`, and mise's task cache records each
pass under a key made from the contents of those files, the task's definition, the tool
versions and `rustc -V`. A check whose inputs have not changed since it passed is skipped
and its log replayed. That also holds in another checkout or worktree with the same
files. Failures are never cached. `mise run ci --force`, `mise run --force <task>` or
`MISE_TASK_CACHE=off` runs the checks regardless, and
`mise run --dry-run --task-cache-explain <task>` lists the inputs of a key.

The Rust tests run under cargo-nextest, which runs test binaries in parallel and lists
every failure at the end. A test that runs past ten minutes is stopped and counted as a
failure. `.config/nextest.toml` holds these settings. Debug builds keep line tables only,
so backtraces have file and line numbers but a debugger cannot show variables. Set
`CARGO_PROFILE_DEV_DEBUG=true` for full debug information.

`crates/sparkles-core/tests/w3c.rs` runs the W3C SPARQL suites vendored in an Apache
Jena checkout. It looks for the checkout at `../../apache/jena`, next to this
repository, or at `SPARKLES_W3C_DIR`. The SHACL suites come from the same checkout, or from
`SPARKLES_SHACL_TESTS`. Without the checkout, the suites are skipped.
`scripts/fetch-test-suites.sh DIR` fetches every suite the tests read, at the commits CI
pins, as shallow sparse checkouts of about 35 MB, and prints the variables that point the
tests at them (`export $(scripts/fetch-test-suites.sh target/test-suites)`).

All of these suites pass: 482/482, 328/328, 157/157 and 269/269 for SPARQL, and 98/98
and 20/20 for SHACL. The eight SHACL 1.2 list tests pass, and all 32 test pairs of the
SHACLC suite read to the expected graph and write back to it.
`crates/sparkles-core/tests/w3c-known-failures.txt` lists known failures and is empty.

For the X08 spill conformance matrix, run `SPARKLES_W3C_SPILL=plain mise run --force test:w3c`
and repeat with `SPARKLES_W3C_SPILL=encrypted`. Cursor query passes then use a 1 MiB
memory budget and a private spill directory per query at batch sizes 1, 2, 3 and 4,096.
Eager passes retain their ordinary options. These runs enable spill for conformance
fixtures; the dedicated spill tests separately force physical spill.

`mise run test:jena-clients` (`scripts/test-jena-clients.sh`) builds a debug server, starts
it on a temporary data directory on port 5230 with `--gsp-direct-naming`, and runs
`testsuite/jena-clients/JenaClients.java` with Java's single-file launcher. The program
uses Jena's `RDFConnectionRemote`, `RDFConnectionFuseki`, `GSP`, `DSP`, `QueryExecHTTP`
and `UpdateExecHTTP` with every query send mode, result format and RDF syntax Jena has,
gzip in both directions, uploads, direct naming and SHACL. It sends patches written by
Jena's text and binary patch writers to the patch endpoint and compares the result with
Jena's own application of them. It also makes the admin calls
Fuseki's clients make, such as creating datasets from forms and assemblers, backups,
compaction, tasks, statistics, offline datasets and the validators. It prints a line per
check and exits with the number of failures. Jena and a JDK come from nixpkgs unless
`JENA_HOME` and `JAVA` name them, `SPARKLES_BIN` picks another server binary, and `PORT`
another port. The task is not part of `mise run ci`.

`scripts/power-loss.sh` runs the crash tests of
[X09](specs/X09-group-commit-by-default.md) sections 10.2 and 10.4 with a release build and
`scripts/crash-harness.rs`, which `rustc --edition 2024 -O` builds as one file. Its
`logwrites` mode needs root and the device-mapper `log-writes` target. It records every
write, flush and FUA of a file system under a 16-writer load and then replays crash images
from the log, 1,000 by default, checking each recovered store. Its `kill9` mode kills the
server at random moments of the same load, 200 times by default, and checks every receipt
after each restart. With a directory in place of `ext4` or `xfs` it needs no root. Neither
mode is part of `mise run ci`. A load that took no receipt, had a failed write or, in
`logwrites`, failed to mark the log fails the run, because its checks would pass without
testing anything. A failed `logwrites` run keeps its log and the first three failing images,
and `crash-harness replay --only` with the same seed rebuilds any other failing image.
`--index text`, `geo` or `embed` configures a full-text, spatial or embedding index on each
store before the load, as section 17.8 asks, and every check then also compares that index
with the recovered data. The `selftest` mode needs no root. It checks that the driver fails a load whose writes the
server refuses with a 507, passes a working one, and fails one whose marks all fail. Run it
after changing the harness. The harness's own tests, which check which `sparkles check`
warnings a recovered store may still show after an open, run in the `test:bench-scripts`
task of `mise run ci`. The `--fs` and `--work`
directories must exist:

```sh
mkdir -p /tmp/plt
scripts/power-loss.sh selftest --fs /tmp/plt --sparkles target/release/sparkles \
  --harness ./crash-harness --work /tmp/plt
```

The shexTest suite comes from the same Jena checkout (`jena-shex`). Set
`SPARKLES_SHEX_TESTS` to use an upstream shexTest checkout instead.
`crates/sparkles-shex/tests/known-failures.txt` lists the tests that fail, with reasons.
Sparkles passes all 425 syntax, 99 negative-syntax, 14 negative-structure and 418
representation tests, and reads and writes all 418 ShExR schemas. Of the validation
tests, 1,062 pass and one is a known failure. The 42 tests that check blank-node labels
are skipped, because the store does not keep them.

`mise run lint:features` (`scripts/lint-features.sh`) runs clippy, with warnings as
errors, over the builds that `mise run lint` does not cover:

* the server with no optional feature, with each default feature but `mimalloc` on its
  own, with `geo-epsg`, and with `mcp,shacl`, `mcp,shex`, `mcp,fmt` and `graphql,shacl`;
* `sparkles-core`, `sparkles` and `sparkles-fmt` with their features off;
* `sparkles-client` without its blocking facade;
* the `sparkles-backup` library without a backend.

Code that only some features use is gated on those features, and this task catches dead
code and missing gates. Every combination shares the workspace target directory. The
first run builds the dependencies once per feature set. After that, a change costs a
minute or two.

`mise run lint:doc-paths` (`scripts/check-doc-paths.py`) checks that every
`crates/sparkles…` path cited in the README, `docs/*.md`, `docs/specs` and the comments of
the Rust sources exists. It expands `{a,b}` groups and ignores line numbers. A file that a
spec describes before it is written goes in the script's `PLANNED` set.

### Python bindings

`crates/sparkles-py` builds the `sparkles` Python package
([USAGE](USAGE.md#python), [spec P01](specs/P01-python-bindings.md)). The crate is its
own cargo workspace, excluded from the root one, so `cargo build`, `mise run lint` and
`mise run test` neither compile PyO3 nor need Python. It has its own `Cargo.lock`. When
the library crates gain a dependency, `py:test`, `py:lint` and `mise run licenses` add it
to that lock, as cargo does with the root lock, and the changed file is committed with
the rest. `mise run py:lock` refreshes the lock from the root one, so that both resolve
the same versions. `py:build`, `licenses:check` and the flake's check use the lock as it
is and fail while it lacks something the crate needs.

`mise run py:test` (`scripts/py-test.sh`) builds the extension with cargo in the root
`target` directory, so it reuses the workspace's compiled dependencies. It assembles the
package in `target/py` and runs `crates/sparkles-py/tests` with pytest. The tests include
mypy's `stubtest`, which checks the `.pyi` stubs against the compiled module, and rdflib
interoperability tests. Both skip when mypy or rdflib is missing. The dev shell's
`python3` has pytest, mypy and rdflib. With another `python3`, the script installs pytest
into a virtual environment in `target/py-venv`. Extra arguments go to pytest, as in
`mise run py:test -- -k transaction`. `mise run py:lint` runs clippy on the crate with its
default features and with none. Both tasks are part of `mise run ci`. The first run
compiles the crate and the engine for the Python build, and later runs take seconds.

`mise run py:sdist` writes the source distribution to `target/wheels` with
`scripts/py-sdist.py`. `maturin sdist` copies the crate and its path dependencies but not
the vendored spargebra that the crate's `[patch.crates-io]` names, so the script adds it
to the archive and points the patch at it. `mise run py:wheel-test -- <wheel or sdist>
<python>...` installs a package into a fresh virtual environment for each interpreter
and runs the pytest suite against the installation (`scripts/py-wheel-test.sh`).

#### Release workflow

`.github/workflows/python-wheels.yml` runs on pushes to `main` and pull requests that
touch the bindings or a crate they build on, on `py-v*` tags and by hand. Tags and manual
runs build abi3 wheels with maturin-action for manylinux 2.28 and musllinux 1.2 on x86_64
and aarch64 and for macOS on Apple silicon, and the source distribution. Pushes and pull
requests build only the x86_64 manylinux wheel, in the dev profile. The arm64 wheels build
on arm64 runners, so their tests run natively. Each job installs what it built with `py-wheel-test.sh` and
runs the tests on CPython 3.10 and 3.14 where the runner has both. The musllinux
wheels are tested in an Alpine container, and the sdist job builds a wheel from the
sdist before testing it. Every package is uploaded as an artifact.

The publish job sends the artifacts to PyPI. [Releases](#releases) describes how to
set it up and run it.

The x86_64 manylinux and musllinux builds can be reproduced locally with Docker in the
`quay.io/pypa/manylinux_2_28_x86_64` and `musllinux_1_2_x86_64` images, with
`maturin build --compatibility manylinux_2_28` or `musllinux_1_2`. Both were built that
way, the musllinux one from the sdist, and passed the suite on CPython 3.10 and 3.14 and
in Alpine. The macOS and aarch64 wheels are built only by the workflow.
`actionlint` checks the workflow file.

### JVM bindings

`crates/sparkles-ffi` is the native library of the JVM bindings
([USAGE](USAGE.md#jvm-apache-jena), [spec P04](specs/P04-jvm-bindings.md)). It exports
the engine through UniFFI 0.32 and is its own cargo workspace with its own `Cargo.lock`,
for the same reasons as `crates/sparkles-py`. UniFFI's bindings generator, with its
template engine and object-file reader, is a binary of the crate behind the `bindgen`
feature, so the generated Kotlin and the library's scaffolding always come from the same
UniFFI version. `mise run jvm:lock` refreshes the lock from the root one.

`mise run jvm:native` (`scripts/jvm-native.sh`) builds the library in release mode in the
root `target` directory and writes its generated Kotlin to `target/jvm/uniffi`.
`JVM_PROFILE=dev` builds it without optimizations. The Gradle build in `jvm/` runs through
its checked-in wrapper and reads both paths, which the Gradle properties
`sparkles.nativeLib` and `sparkles.bindings` can change. It compiles the generated code
on its own, puts the library into the jar under
`io/github/kclejeune/sparkles/native/<os>-<arch>/` with its SHA-256, and compiles the
library in explicit API mode with warnings as errors. `mise run jvm:test` runs Jena's
contract tests from Jena's published test jars, the acceptance examples of the spec, and
jena-core's JUnit 3 graph suite. `mise run jvm:sample` builds the Java sample with
`-Xlint:all -Werror` and runs it.

The library also exports hand-written JNI entry points for the reads that small requests
make most often (`crates/sparkles-ffi/src/jni_calls.rs`). The Kotlin side of them is
`SparklesJni` in `jvm/sparkles-jena/src/ffi/kotlin`. They are in the same library file,
which `SparklesJni` loads a second time with `System.load` so that the JVM binds them, and
they work on the objects UniFFI made. The `ffiBindings` Gradle task adds a helper that
borrows an object's handle to every generated class, and it sends the frees of read
transactions, queries and cursors through JNI. The task fails the build when the
generated code does not have the shape it expects, as after a UniFFI upgrade. The system
property `sparkles.jni=false` keeps every call on UniFFI, and a list of `read`,
`contains`, `find` and `query` turns on only those groups. `mise run jvm:test` runs the
suite twice, as `test` with the JNI calls and as `testUniffi` without them, and
`mise run bench:bindings --jvm-opts=-Dsparkles.jni=false` measures the UniFFI calls alone.

`mise run bench:jena` compares realistic Jena work on Sparkles, TDB2 and Jena's in-memory
dataset at one and four threads. Its cases are Model and Graph calls, Jena's RDFS
reasoner, small SPARQL queries, parameterized queries, small updates and Model writes, a
bulk load and queries through Fuseki. Every arm runs in fresh JVMs in A/B/B/A order, the
answers are checked first, and the results go to `target/bench-jena/results` as
`summary.md` and `raw.json`. An arm is an engine with JVM options, so a system property
that turns a feature off gives a before arm. `scripts/nsc-jena.sh` (`mise run
bench:jena-nsc`) runs the same comparison on an ephemeral Namespace instance. It builds
the bindings here, ships them with a Temurin JDK and a standalone CPython, and downloads
the results to `target/nsc-jena/<time>`.

`./gradlew :sparkles-jena:perfCheck -Pdata=DATA.nt -Pqueries=QUERIES.tsv`, run in `jvm/`,
times queries through Jena on Sparkles, through the native library alone and on an
in-memory TDB2 dataset. `cargo run --release --example perf --manifest-path
crates/sparkles-ffi/Cargo.toml --target-dir target -- DATA.nt QUERIES.tsv 10` times the same
queries through the Rust API. Each line of `QUERIES.tsv` is a name, a tab and a query.

The build assembles `sparkles-jena`, `sparkles-jena-natives` and a Fuseki bundle,
`sparkles-jena-all`. Source and Dokka documentation jars accompany the SDK. Native
resources have SHA-256 checksums; Gradle dependency locks and verification metadata
pin the release graph. The compatibility workflow uses Java 17/Jena 5.6 and Java
21/Jena 6.2 while emitting Java 17 SDK class files. Nix builds include the host's native
library. Ordinary builds and tests do not publish artifacts. [Releases](#releases)
describes publication to Maven Central.

### JavaScript bindings

`crates/sparkles-node` is a separate Cargo workspace with its own lockfile. The
`js/` pnpm workspace contains `@sparkles-rdf/common`, `@sparkles-rdf/engine` and
`@sparkles-rdf/client`. Node 24 is pinned for these tasks.

```sh
pnpm --dir js install --frozen-lockfile
mise run node:build         # host addon and TypeScript packages
mise run node:test          # registered runtime/protocol/lifecycle tests
mise run node:lint          # default-feature and feature-free native checks
mise run node:client:gen    # regenerate from docs/openapi.json
mise run node:client:check  # verify generated types are current
mise run node:fmt:check
```

`scripts/node-native.sh` builds into the root `target` directory and stages the host
addon under `js/engine/native`. Its default is release mode; `NODE_PROFILE=dev` uses
the debug build. `SPARKLES_NODE_NATIVE` selects an explicit addon path. The package
loader also resolves a platform-specific npm package. Generated addons and package
archives are ignored by Git. `mise run node:lock` refreshes the native lockfile.

The runtime suite checks transactions, cancellation, worker cleanup, streams, RDF/JS
terms and adapters. Packed-package checks install concrete archives into a fresh
project and compile a consumer of their published declarations. The remote client has
protocol and streaming-parser tests, and its generated administrative types are
checked against the OpenAPI document. The release workflow builds the native target
matrix and runs runtime checks. [Releases](#releases) describes publication to npm.

### Browser integration tests

`mise run ui:e2e` builds the UI and a debug server. It starts `sparkles serve` on a free
port of 127.0.0.1 with a temporary data directory, a small dataset, and an auth
configuration with one user and one API token. It then runs the Playwright tests in
`ui/tests/e2e` in headless Chromium and stops the server.

To test another binary, set `SPARKLES_BIN=path/to/sparkles`. Extra arguments go to
`playwright test`, as in `mise run ui:e2e -- -g Similar`. The flake's dev shell provides
a Chromium that matches the pinned `@playwright/test`, so run the task as
`nix develop -c mise run ui:e2e`. Elsewhere, the task downloads Chromium with
`playwright install chromium`. The task is not part of `mise run ci`. On Linux, the
flake runs the same tests as its `ui-e2e` check (see [Nix](#nix)).

## Benchmark scripts

[BENCHMARKING.md](BENCHMARKING.md) documents the harnesses, modes, outputs and
measurement limits, including local and cloud regression comparisons.
[BENCHMARKS.md](BENCHMARKS.md) contains published results and their methodology.

`scripts/bench-indexed-commit.py` runs the native indexed group-commit gates of X09
section 17.9 on a quiet Linux host. It takes an externally built Sparkles binary and the
standalone client built from `scripts/write-bench.rs`. Its `--help` describes the matrix
and the pilot overrides. Both alternatives use the same binary, and the ordinary writer
adds `--no-group-commit`. The harness holds the host's heavy lock and runs each server
in a user systemd service with a 12 GiB memory limit and no swap. The CPU lists need to
match the host's topology. It creates fresh store copies, checks indexed answers and
acknowledgements, saves individual latencies and checks a cold reopen after each run.
Generated fixtures and seeds are flushed after preparation. Every fresh store copy is
flushed before server startup, and completed raw evidence is flushed after cleanup.
These owned-file flushes stay outside the writer interval and prevent bulk preparation
writeback from spilling into the next measurement. They add no delay to timed commits.
The embedding worker calls a deterministic local provider, whose cost represents that
provider alone. Its own ordinary commits can appear between client receipts. The harness
checks every acknowledgment against the recovered user history and verifies the
intervening embedding commits, their fallback counters and their derived triples
separately. It keeps the ordinary client's strict receipt checks. A failed acceptance
threshold remains a failed gate in the result.
Cold embedding inspection disables the worker and compares persisted derived triples and
commit identities before they can be regenerated.
The full protocol uses both dataset sizes and five ABBA blocks. A smaller pilot only
checks the harness and estimates runtime.

For memory measurements, the harness saves the server's startup peak, resets its owned
process's Linux high-water mark immediately before the writes and records the peak when
the writes end. It also samples resident memory during that interval. An allocation and
release in a disposable child checks that the kernel supports resetting the peak. A
failed reset stops the measurement. The original Namespace write harness keeps its
existing workload defaults.

Reader requests started while the writers run keep their full latency, including those
that finish after the writes stop. A timeout fails the observation. Both alternatives
must sustain the same offered write rate before their reader tails can be compared.
The harness adds a load-quality requirement beyond X09's numerical thresholds. Each
mixed process must achieve its offered rate within five percent, with maximum schedule
lag no greater than 50 ms or two client periods, whichever is longer.
A violation remains a failed gate even when the reader latency ratio passes. The harness
retains its raw samples and completes the data and cold-recovery checks before moving
to the next process.
When no rate is supplied, calibration runs an unpaced ABBA block with the actual indexed
readers and chooses eighty percent of the slowest observed writer throughput. The harness
then validates that common offered rate on both alternatives, including a supplied rate,
before acceptance processes begin. Either alternative missing the five-percent achieved
rate tolerance stops the matrix and retains all calibration evidence. The rate is fixed
throughout the acceptance matrix. Calibration has its own raw samples and cold-recovery
checks and cannot enter acceptance summaries. Its aggregate rate validation does not
excuse a failed schedule-lag check in an acceptance process.

## Continuous integration

The workflows in `.github/workflows` run on standard GitHub-hosted runners, which are free
for public repositories: `ubuntu-24.04` for Linux x86_64 with 4 vCPUs and 16 GB,
`ubuntu-24.04-arm` for Linux arm64 and `macos-15` for macOS arm64. Since runner time is
free, the workflows split their work into parallel jobs to shorten the wait for a push,
rather than to save runner minutes. `actionlint` checks the workflow files.

`ci.yml` runs on every push to `main` and every pull request. Its jobs start together.

| Job | What it runs |
|---|---|
| Checks | Formatting of the Rust, UI, JavaScript and shell sources, shellcheck, the cited doc paths, the benchmark scripts' tests, the license notices, the generated TypeScript client against `docs/openapi.json`, and the UI's build, type check and unit tests. It needs no Rust build and takes about three minutes. |
| Clippy | Clippy with warnings as errors on the workspace, on the formatter's wasm32 build and on the three binding crates with and without their default features. |
| Build tests | `cargo nextest archive` builds every test binary once and uploads the archive. The doctests run here too. |
| Test | Five jobs run the archive. Four take a quarter of the tests each (`--partition slice:N/4`), and one runs the `timing` group of `.config/nextest.toml`, which needs a machine to itself. |
| Jena clients | Jena's own HTTP clients against the server binary from the archive (`mise run test:jena-clients`). |

The transcript import integration test reserves all test slots while it runs in its
shard. It sends a 5 MiB transcript through the real server, whose query deadline stays
at 30 seconds. Other tests resume after it finishes.

The test jobs fetch the W3C SPARQL and RDF suites, the SHACL and ShEx suites that Jena
vendors and Oxigraph's JSON-LD documents with `scripts/fetch-test-suites.sh`, so the
conformance tests run in CI as they do on a machine with the sibling checkouts. Raising
the pinned commits in that script may need updates to the known-failure lists.

A change only to the UI, the JavaScript packages, the README or the specs runs the checks
job alone, which a first job decides with `dorny/paths-filter`. Changes only to the
benchmark records, the images, the Kotlin sources or the binding workflows do not start
`ci.yml`. In all five workflows, main runs use distinct concurrency groups so a newer
push preserves both running and queued checks. Pull request updates cancel earlier
checks, while binding release tags remain protected from cancellation. A manual run
with **all-platforms** checked also runs Clippy and the tests on Linux arm64
and macOS. The engine's shared state goes through snapshot swaps, locks and channels, and
its relaxed atomics are counters and flags, so the risk of a fault that only arm64's
weaker memory ordering or macOS would show is low, and pushes test Linux x86_64 only.

The binding workflows, `python-wheels.yml`, `node-packages.yml` and `jvm-bindings.yml`,
run on pushes to `main` and pull requests that touch the binding or a crate it builds on,
leaving out those crates' tests, benches and examples. Such a run builds the binding in
the dev profile and tests it on Linux x86_64 only, in one job. The Node workflow also
tracks the default-feature server and its dependencies for the remote client test. Shared
core patch fixtures also trigger the Python wheel tests that read them. Pull requests also run
the JVM compatibility matrix. Release tags and manual runs build and test every platform
in the release profile. Every job has a timeout, so a hung job stops instead of running
for GitHub's default six hours.

Jena 6 removed the query builder's initial-binding API. Its compatibility run skips
that API's test, while Jena 5 still runs it. The remaining small-query tests run on both
versions. Substitution tests remain separate because their semantics differ.

`nix.yml` builds the flake's `cacheable` output with nix-fast-build on x86_64 Linux,
arm64 Linux and arm64 macOS for pushes, pull requests and manual runs
when their changed paths select the workflow. The [Nix](#nix) section describes its build set.

### Caches

GitHub keeps 10 GB of Actions caches per repository and evicts the least recently used
entries beyond that. Caches saved on a branch serve only that branch, while those of
`main` serve every branch and pull request, so the jobs that cache Cargo artifacts save
only from pushes to `main`. `.github/actions/setup-rust` installs the toolchain that
`rust-toolchain.toml` pins and restores `Swatinem/rust-cache` under a name per job, so the
Clippy, test build, binding and JVM `uniffi-bindgen` jobs do not replace each other's
entries. rust-cache keeps the dependencies' build artifacts and drops the workspace's
own, which change on nearly every push. The Python container build caches its target
directory the same way, `gradle/actions/setup-gradle` saves Gradle's caches from `main`
only by default, and `actions/setup-node` caches pnpm's store. CI builds carry no debug
information (`CARGO_PROFILE_DEV_DEBUG=0`, which `setup-rust-toolchain` sets), which keeps
linking short. A cold test build still peaks above the runner's 16 GB, so that job adds
16 GB of swap.

Measured on the pull request that introduced this layout, a push with warm caches waits
about 13 minutes for `ci.yml` and a cold one about 25. The single job it replaced took
about 27 minutes warm and 40 cold, of which the test run alone took 14 to 21. The test
build takes 7 minutes warm and 15 cold, and each test job about 5 to 6 minutes, of which
downloading the test archive takes one to two. Compressing with a long zstd window shrinks the
archive from 2.2 GB to about 1.4 GB, because the test binaries share most of their code.

## Releases

Each binding has its own release tag and workflow. A tag that starts with `py-v` runs
`python-wheels.yml` and publishes the `sparkles-rdf` distribution to PyPI. A `node-v`
tag runs `node-packages.yml` and publishes the `@sparkles-rdf` packages to npm. A
`jvm-v` tag runs `jvm-bindings.yml` and publishes `sparkles-jena`,
`sparkles-jena-natives` and `sparkles-jena-all` to Maven Central under the group in
`jvm/gradle.properties`. The version after the prefix must equal the version in
`crates/sparkles-py/Cargo.toml`, the `js/*/package.json` files or
`jvm/gradle.properties`, and each workflow checks that before it publishes. The build
and test jobs run first, and the publish job runs only when they all pass.

Nothing is published until a repository variable switches it on. Set `PYPI_PUBLISH`,
`NPM_PUBLISH` or `MAVEN_PUBLISH` to `true` under the repository's Actions variables.
Each publish job also runs in a GitHub environment, `pypi`, `npm` or `maven`, and
adding required reviewers to an environment makes every release wait for approval.

**PyPI.** Add a pending trusted publisher for the project `sparkles-rdf` on PyPI, with
this repository, the workflow `python-wheels.yml` and the environment `pypi`. The first
release then creates the project, and no token is stored. As a fallback, an API token
in the `PYPI_API_TOKEN` secret of the `pypi` environment is used instead.

**npm.** Create the `sparkles-rdf` organization on npmjs.com. npm accepts trusted
publishers only for packages that already exist, so the first release needs an
automation token with publish rights to the organization in the `NPM_TOKEN` secret of
the `npm` environment. After it, add this repository, the workflow `node-packages.yml`
and the environment `npm` as the trusted publisher of each of the ten packages, and
delete the secret. Every release carries a provenance statement either way. A rerun
skips the packages that are already published.

**Maven Central.** Sign in to the Central Portal (central.sonatype.com) with GitHub,
which verifies the `io.github.kclejeune` namespace. A namespace under your own domain is
verified with a DNS TXT record instead. Generate a Portal user token and store its
username and password in the `MAVEN_USERNAME` and `MAVEN_PASSWORD` secrets of the
`maven` environment. Create a signing key with `gpg --quick-generate-key`, publish its
public key to `keys.openpgp.org`, and store the ASCII-armored private key
(`gpg --armor --export-secret-keys <id>`) and its passphrase in `MAVEN_SIGNING_KEY` and
`MAVEN_SIGNING_PASSWORD`. The job deploys the signed artifacts through the Portal's
OSSRH Staging API and then moves them into the Portal. By default they wait there
until someone presses Publish. Set the variable `MAVEN_PUBLISHING_TYPE` to `automatic`
to release them as soon as they validate. Artifacts on Maven Central can never be
changed or deleted, so check a deployment in the Portal before you release it.

### Changing an on-disk format

`crates/sparkles-core/src/formats.rs` declares every kind of file Sparkles keeps, the
version it writes and the versions it reads, and every settings file with its version
member. A change to a file's format starts there. Its module documentation describes the
four kinds of change, from an additive member that needs no version to an upgrade step
in `crates/sparkles-core/src/migrate.rs`. A new file kind or settings file is added to
the registry in the same commit that first writes it, and its reader refuses a newer
version through `FileKind::check` or `formats::check_settings`. A change that an older
release would misread also raises `minimumReader` before the first file in the new form
is written ([`commit::require_reader`](../crates/sparkles-core/src/commit.rs)).

A store checks every settings file of a dataset against the registry when it opens the
dataset, before it writes any file, so a settings file from a newer release refuses the
whole dataset. A settings file that a release rewrites through a typed struct drops the
members that struct does not know, so an additive member of such a file must be safe to
lose when an older release rewrites the file, or the change needs a `format` bump.
`dataset.json` keeps unknown members on a rewrite, and so do the files that the server
writes as raw JSON values.

`testsuite/formats/<label>/` holds databases written by earlier commits or releases,
made by `scripts/format-fixture.sh BIN LABEL` with that release's binary. The tests
`crates/sparkles-core/tests/formats.rs` and `crates/sparkles-server/tests/cli_formats.rs`
open every fixture, compare its recorded answers, write, compact and restore its
backups. A change that breaks a fixture inside the window needs a reader for the old
form or a migration step, not a new fixture.

## Nix

The flake is built on flake-parts, rust-overlay and crane, with the toolchain from
`rust-toolchain.toml`. It provides:

* **Packages:**
  * `sparkles` (default): the binary with the UI and the formatter's WebAssembly module
    embedded, the third-party licenses and notices and the OpenAPI description in
    `share/doc/sparkles/`, shell completions for bash, zsh and fish, and man pages.
  * `sparkles-cli`: the same binary without the UI, so the build needs no Node.js.
  * `sparkles-ui`: the static UI build.
  * `sparkles-fmt-wasm`: the formatter's WebAssembly module, which `sparkles-ui` builds in.
  * `sparkles-py`: the Python package for nixpkgs' `python3`, built into an abi3 wheel by
    maturin. The wheel is in its `dist` output.
  * `sparkles-ffi`: the JVM bindings' native library in `lib/`, with the Kotlin that its
    `uniffi-bindgen` generates in `share/uniffi`.
  * `sparkles-jena`: the JVM library's jar in `share/java`, built by Gradle offline with the
    host's native library inside. Its Maven dependencies are pinned in
    `nix/jvm-deps.json`, which `mise run jvm:nix-deps` refreshes through
    `gradle.fetchDeps`.
  * `sparkles-node-native`: the host's Node-API library in `lib/`.
  * `sparkles-node`: npm archives for the JavaScript packages, the host addon and
    their pinned runtime/type dependencies.
* **Other outputs:**
  * `overlays.default`;
  * a dev shell;
  * `checks`:
    * the packages;
    * `sparkles-tests`, which runs the unit tests of the engine and the facade
      (`cargo test -p sparkles-core -p sparkles --lib`);
    * `fmt`, which runs rustfmt over the workspace, `crates/sparkles-py` and
      `crates/sparkles-ffi` and `crates/sparkles-node` as `mise run fmt:check` does;
    * `python-bindings`, which builds `sparkles-py` and runs the pytest suite on the
      installed package;
    * `node-bindings`, which installs the packed JavaScript packages offline and
      checks their runtime behavior and declarations;
    * on Linux, `jvm-bindings`, which builds `sparkles-jena` and runs its tests and the
      Java sample's tests offline;
    * `ui-licenses`, which checks that `THIRD_PARTY_LICENSES-UI.md` matches the UI build;
    * on Linux, a NixOS VM test of the module behind nginx;
    * on Linux, `home-module`, which builds the Home Manager module's files for a test user
      and checks that the CLI reads them;
    * on Linux, `ui-e2e`, which runs the Playwright UI tests against the release binary
      in nixpkgs' headless Chromium, inside the build sandbox on 127.0.0.1;
    * on Linux, `jena-clients`, which runs the Jena client tests against `sparkles-cli`
      with nixpkgs' `apache-jena` and JDK, inside the build sandbox;
  * `nixosModules.default` (see [Deploying on NixOS](USAGE.md#deploying-on-nixos));
  * `homeModules.default` (see [Home Manager](USAGE.md#home-manager)).

```sh
nix run github:kclejeune/sparkles -- serve --data ./data
nix build .#sparkles-cli
nix flake check          # packages + NixOS VM test (Linux, needs KVM) + UI end-to-end tests
nix build .#checks.x86_64-linux.ui-e2e -L   # only the UI end-to-end tests
```

### Binary cache

`.github/workflows/nix.yml` builds the flake's `cacheable.<system>` output with
nix-fast-build and pushes what it built to the binary cache at `https://cache.kclj.io`
with nimbus. The output holds the packages, the dependency layers of `sparkles` and the
formatter's WebAssembly module, every package the benchmark runners resolve (see
[BENCHMARKING.md](BENCHMARKING.md#pinned-comparison-engines)), the dev shell and the
checks other than the NixOS VM test and the UI end-to-end tests. `--skip-cached` skips
whatever a substituter already holds, so a run builds only what changed since the last
push. The workflow pushes only from pushes to `main` and manual runs, and only when the
repository has the `NIMBUS_ENDPOINT` variable and the `NIMBUS_AUTH_TOKEN` secret. Pull
requests build without pushing. Pushes, pull requests and manual runs build all three
cacheable systems, `x86_64-linux`, `aarch64-linux` and `aarch64-darwin`, on native
GitHub runners.

The flake's `nixConfig` names the cache and its key, which Nix uses after asking or with
`accept-flake-config = true`. Machines that build unattended should list the cache in
their own `nix.conf`.

### How the Rust build is layered

Per-crate Nix builds remain an investigation under [roadmap item 92](ROADMAP.md)
and [X13](specs/X13-per-crate-nix-builds.md). A working crate2nix prototype has not
completed a matched comparison or packaging validation, so these builds keep Crane.

crane builds `sparkles`, `sparkles-cli` and `sparkles-fmt-wasm` in two derivations each.
The first one compiles only the dependencies. It sees the manifests and `Cargo.lock`,
and every workspace source file is replaced by an empty stub, so it is rebuilt only when
a `Cargo.toml` or `Cargo.lock` changes. The second one unpacks that target directory and
compiles the workspace crates. A change to a Rust source file therefore recompiles the
workspace crates and nothing else.

`sparkles` and `sparkles-cli` share one binary. The Nix build embeds no UI in it, and
`sparkles` is a small wrapper that sets `SPARKLES_UI_DIR` to the UI's store path, from
which the server reads the UI at run time. A UI change therefore rebuilds the UI and the
wrapper, and no Rust code. Builds outside Nix (`mise run build`, the Dockerfile) still
embed `ui/build` in the binary. The `sparkles-tests` check reuses the same dependency
derivation, which also compiles the dependencies of
`cargo test -p sparkles-core -p sparkles --lib`, because the engine alone enables fewer
features than the server does.

The formatter's WebAssembly module has its own dependency derivation for
`wasm32-unknown-unknown` with the `fmt-wasm` profile from `Cargo.toml`, which
`scripts/build-fmt-wasm.sh` also uses. Its second derivation compiles a source tree in
which every workspace crate is stubbed except `sparkles-fmt`, `sparkles-fmt-wasm` and the
vendored spargebra, so an edit to the server or the engine does not rebuild the module,
and with it the UI.

Dependency updates need no hash in the Nix files. crane reads `Cargo.lock` and fetches
each crate by the checksum recorded there, so `cargo update` followed by `nix build` is
enough. Two exceptions remain. A new version of the `wasm-bindgen` crate needs the same
version of `wasm-bindgen-cli` in `nix/fmt-wasm.nix`, with its two hashes, and the build
stops with a message until they agree. The Python package is built by maturin through
nixpkgs' own Rust hooks, from `crates/sparkles-py/Cargo.lock`, and has no separate
dependency layer. maturin runs cargo itself with PyO3's interpreter settings, so a
dependency layer built by crane would not match its build.

The `.drv` paths show what a change rebuilds. After a change,
`nix path-info --derivation .#packages.x86_64-linux.sparkles.cargoArtifacts` prints the
same path as before unless a manifest or the lockfile changed. In the same way,
`.#packages.x86_64-linux.sparkles-cli`, which is also `sparkles.passthru.unwrapped`, keeps
its path across a UI change, and `.#packages.x86_64-linux.sparkles-fmt-wasm` keeps its
path across a change to any crate but the formatter's two.

## Third-party licenses

[`THIRD_PARTY_LICENSES.md`](../THIRD_PARTY_LICENSES.md) holds the license and NOTICE
files of every crate the binary links on Linux and macOS, with each text included once.
Crates that ship no license file get their license's standard text.
`scripts/third-party-licenses.py` generates the file from `cargo metadata`, so it changes
only when `Cargo.lock` does. Ship it with binaries. The Nix packages install it as
`share/doc/sparkles/THIRD_PARTY_LICENSES.md`.

The script also writes `crates/sparkles-py/THIRD_PARTY_LICENSES.md` for the Python wheel
from that crate's own lock. It lists the crates the extension module links, which are the
engine's and PyO3's. maturin puts it in the wheel's `licenses/` directory with the
project's `LICENSE`, as `pyproject.toml` declares. It writes
`jvm/sparkles-jena/THIRD_PARTY_LICENSES.md` for the JVM library from the lock of
`crates/sparkles-ffi`, and the jar carries it in `META-INF` with the project's `LICENSE`.
Crates that state MPL-2.0 without a license file, such as UniFFI's, get the MPL-2.0 text
that another crate ships. It also writes `js/engine/THIRD_PARTY_LICENSES.md` from the
Node addon's own lockfile. `licenses:check` checks all four Rust notices files.

[`THIRD_PARTY_LICENSES-UI.md`](../THIRD_PARTY_LICENSES-UI.md) does the same for the npm
packages whose code or fonts end up in the embedded web UI. These are CodeMirror,
Cytoscape, MapLibre GL JS and its dependencies, the Svelte and SvelteKit runtime, Vite's
and Rolldown's runtime helpers, and the Fontsource fonts and their dependencies. A
package is listed whether it is a dependency or a devDependency. Build tools that ship
nothing are left out.

A Vite plugin, `ui/scripts/licenses.js`, finds these packages through the bundle's module
graph and the source files of its assets. It reads their license files from
`node_modules` and writes the notices into the build as `licenses.txt`. The server serves
that file at `/ui/licenses.txt`, and the `sparkles` Nix package installs it as
`share/doc/sparkles/THIRD_PARTY_LICENSES-UI.md`. The UI build fails when a shipped
package's license allows none of MIT, MIT-0, ISC, 0BSD, BSD-2-Clause, BSD-3-Clause,
Apache-2.0, Zlib, Unlicense, CC0-1.0, BlueOak-1.0.0 or OFL-1.1, the fonts' license.

The UI notices are a separate file because they change with `ui/pnpm-lock.yaml` and need
the UI build, and because `sparkles-cli` ships without the UI. `mise run licenses` builds
the UI and copies the file. `mise run licenses:check` fails when either file is out of
date. It is part of `mise run ci`, and the flake check `ui-licenses` fails in the same
case.
