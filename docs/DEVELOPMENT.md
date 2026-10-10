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
one run at a time can use a checkout.

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

All of these suites pass: 482/482, 328/328, 157/157 and 269/269 for SPARQL, and 98/98
and 20/20 for SHACL. The eight SHACL 1.2 list tests pass, and all 32 test pairs of the
SHACLC suite read to the expected graph and write back to it.
`crates/sparkles-core/tests/w3c-known-failures.txt` lists known failures and is empty.

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

`.github/workflows/python-wheels.yml` runs on pull requests that touch the crates, on
`py-v*` tags and by hand. It builds abi3 wheels with maturin-action for manylinux 2.28
and musllinux 1.2 on x86_64 and aarch64 and for macOS on Apple silicon, and the source
distribution. The arm64 wheels build on arm64 runners, so their tests run natively.
Pull requests build the wheels in the dev profile, and tags and manual runs in the
release profile. Each job installs what it built with `py-wheel-test.sh` and
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

## Continuous integration

The workflows in `.github/workflows` run on standard GitHub-hosted runners:
`ubuntu-24.04` for Linux x86_64, `ubuntu-24.04-arm` for Linux arm64 and `macos-15` for
macOS arm64. These runners are free for public repositories. Jobs check out the
repository with `actions/checkout` and use the GitHub Actions cache. `Swatinem/rust-cache`
caches Cargo's registry and dependency build artifacts for host builds, `actions/setup-node`
caches pnpm's store, and `gradle/actions/setup-gradle` caches Gradle's dependencies.
Container builds use `actions/cache`: Python caches its target directory separately
for manylinux and musllinux, and Node's Alpine builds cache the target directory,
Rust toolchain, Cargo registry and pnpm store. Cache keys distinguish architectures,
build profiles and toolchains. GitHub scopes caches to a branch or pull request, with
the default branch's caches available to other branches. Successful runs can save
caches, including binding jobs that run only for pull requests, tags or manual dispatch.
`actionlint` checks the workflows without custom runner labels. Namespace remains
available for development benchmarks through `bench:nsc` and `bench:jena-nsc`.

Several choices keep CI time down. The Rust workflow runs for pushes to `main`
and for pull requests, but not for the pushes to a pull request's branch. Changes to the
UI, the bindings' JavaScript and Kotlin sources, the README, the specs or the benchmark
and image directories do not start it, while other edits under `docs/` do. A newer push
cancels a running check of the same branch. Each platform runs Clippy and the tests in
one job, and the Linux x86_64 job also checks
formatting. A formatting or Clippy failure still lets the tests report. The W3C, SHACL
and ShEx suites skip themselves in CI, because the runners have no Jena or shexTest
checkout. The binding workflows
run for pull requests that touch their sources, for release tags and by hand. For pull
requests they build in the dev profile and only for Linux x86_64. Every job has a
timeout, so a hung job stops instead of running for GitHub's default six hours. Builds
cover Linux on x86_64 and arm64 and macOS on Apple silicon.

Pushes and pull requests test Linux x86_64 only. The engine's shared state goes through
snapshot swaps, locks and channels, and its relaxed atomics are counters and flags, so
the risk of a fault that only arm64's weaker memory ordering or macOS would show is low.
Before a release, run the Rust workflow by hand with **all-platforms** checked to test
Linux arm64 and macOS as well. Running a binding workflow by hand, or pushing its
release tag, builds and tests every platform.

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

### How the Rust build is layered

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
