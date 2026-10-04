# Development

This page covers building Sparkles from source, the everyday tasks, the test suites, the
Nix flake and the third-party license notices. [USAGE.md](USAGE.md) covers running and
operating the binary. The UI has its own development notes in
[ui/README.md](../ui/README.md). The design specs in [specs/README.md](specs/README.md)
explain why each feature is built the way it is.

Sparkles is experimental. The on-disk format, HTTP API, CLI and Rust API may change
between commits without a migration path.

* [Building from source](#building-from-source)
* [mise tasks](#mise-tasks)
* [Git hooks](#git-hooks)
* [Testing](#testing)
* [Benchmark scripts](#benchmark-scripts)
* [Nix](#nix)
* [Third-party licenses](#third-party-licenses)

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

[`mise.toml`](../mise.toml) pins Node, pnpm, hyperfine, prek, shfmt and shellcheck. Rust
comes from `rust-toolchain.toml`. `mise.toml` also defines the everyday tasks, and
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
mise run test         # all Rust tests            (test:w3c, test:shacl, test:shex for suite summaries)
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
mise run ci           # fmt:check + lint + lint:features + lint:doc-paths + fmt:wasm + test + ui:test + py:lint + py:test + licenses:check
mise run doc          # API docs of the library crates
mise run openapi      # rewrite docs/openapi.json after an API change (a test fails until then)
mise run docs:screenshots # the README's screenshots (docs/images) from the demo dataset in docs/demo
mise run docker:build # the Docker image of compose.yaml (not in ci)
mise run gen-data 1000000 target/bench-data/10m.nt
mise run bench        # Sparkles vs Fuseki, QLever, Fluree and Oxigraph; `bench 1000000 --runs 5` for 10.5M triples
mise run bench:text   # full-text search: Sparkles vs Fuseki with jena-text vs QLever
mise run bench:watdiv # the same engines on WatDiv's 20 query templates, 11M triples
mise run bench:shacl 100000; mise run bench:reasoner 100000 owl-rl
mise run bench:shacl-write 100000   # 1-triple INSERT DATA latency with validation off / warn / reject
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
mise run ci            # formatting, clippy, all workspace tests, svelte-check, UI unit tests, Python binding tests, license notices
mise run lint:features # clippy over feature combinations (in ci)
mise run test:w3c      # W3C SPARQL 1.0 / 1.1 query / 1.1 update / 1.2 suites, with a summary
mise run test:shacl    # W3C SHACL Core and SHACL-SPARQL suites, SHACL 1.2 list tests, SHACLC pairs
mise run test:shex     # shexTest: syntax, negative syntax and structure, representation, ShExR, validation
mise run ui:e2e        # Playwright end-to-end tests against a real server
mise run test:jena-clients  # Apache Jena's own HTTP clients against a real server
```

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
and musllinux 1.2 on x86_64 and aarch64, macOS x86_64 and arm64, and Windows x64, and
the source distribution. The arm64 Linux wheels build on GitHub's arm64 runners, so
their tests run natively. Each job installs what it built with `py-wheel-test.sh` and
runs the tests on CPython 3.10 and 3.14 where the runner has both. The musllinux
wheels are tested in an Alpine container, and the sdist job builds a wheel from the
sdist before testing it. Every package is uploaded as an artifact.

The publish job sends the artifacts to PyPI with trusted publishing, and as committed it
never runs. It needs a `py-v<version>` tag that matches the crate's version, the
repository variable `PYPI_PUBLISH` set to `true`, and a `pypi` environment that PyPI
trusts for the `sparkles-rdf` project. No token is stored. To publish, register the
workflow as a trusted publisher on PyPI, create the `pypi` environment, preferably with
required reviewers, set the variable, and push the tag.

The x86_64 manylinux and musllinux builds can be reproduced locally with Docker in the
`quay.io/pypa/manylinux_2_28_x86_64` and `musllinux_1_2_x86_64` images, with
`maturin build --compatibility manylinux_2_28` or `musllinux_1_2`. Both were built that
way, the musllinux one from the sdist, and passed the suite on CPython 3.10 and 3.14 and
in Alpine. The macOS, Windows and aarch64 wheels are built only by the workflow.
`actionlint` checks the workflow file.

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

[BENCHMARKS.md](BENCHMARKS.md) has the results and describes how each run was set up.

* `scripts/bench.sh` (`mise run bench [people] [workdir]`) runs the engine comparison.
  It compares Sparkles, Jena/Fuseki, QLever, Fluree and Oxigraph over HTTP. Its
  `--mode` flag, or the `MODE` variable, chooses what it measures.

  ```sh
  mise run bench 100000 target/bench                   # load, queries and memory
  mise run bench 100000 target/bench --mode updates    # the queries after 5,000 commits
  mise run bench 100000 target/bench --mode mixed      # readers and a writer at once
  mise run bench 100000 target/bench --mode cold       # a restart with a cold page cache per query
  ```

  The default mode loads every engine, checks the answers and times each query, a
  single-triple update and the throughput of 16 concurrent clients. Each load runs under
  GNU `time`, which records its peak RSS, and the index size is the disk space the store
  takes. The harness reads each server's RSS 2 seconds after it starts, after every query
  has run once, and after the run. It also records the peak RSS during the throughput
  run. While it checks the answers, it resets each server's peak RSS before every query
  (through `/proc/<pid>/clear_refs`) and records how far the peak rose. For Sparkles it
  also reads the size of the decoded-block cache from `/$/stats`, so the summary can show
  the RSS without it. These readings go to `results/mem.json` and the memory tables of
  `results/summary.md`.

  The `updates` mode measures a store that has taken writes. It copies every engine's
  store into `scratch/` in the workdir and serves the copies. Then it sends each engine
  the same 5,000 commits (`--churn`), one request each. About 70% insert new
  `foaf:knows` edges, types and tags, and the rest delete existing `foaf:knows`,
  `foaf:name` and `rdf:type` triples. `scripts/bench-writes.py` generates these commits
  once per workdir from `data.nt` with a fixed seed, so every engine gets the same input
  and the answers stay comparable. The commit rate and latency go to `churn.json`. The
  queries, the answer check and the memory readings then run on the changed stores, and
  all results go to `results-updates/`. The harness never compacts the Sparkles copy,
  and 5,000 single-triple commits stay below the 10,000-quad minimum of automatic
  compaction, so its queries read the base index merged with the delta of the commits.

  The `mixed` mode runs each engine alone on a fresh copy of its store. For 30 seconds
  (`--mixed-seconds`), 16 clients run `star-join` through `oha` while one writer commits
  single-triple inserts as fast as the engine answers, or at `--write-rate` commits per
  second. It reports the reads and writes per second and their latency percentiles.
  Every request opens a new connection, as the curl requests of the other measurements
  do, because on a reused connection QLever's answers wait about 40 ms for a delayed TCP
  acknowledgement.

  The `cold` mode stops each engine before every query and evicts its files from the
  page cache. Then it starts the engine, records the time until it answers, and times one
  query. It repeats this 3 times (`--cold-runs`) and keeps the median.

  The copies of the `updates` and `mixed` modes are removed at the end of the run unless
  `KEEP_SCRATCH=1` is set. The servers listen on five ports from `--port-base` (default
  3931), so runs with different bases can share a machine. Results merge per engine, so
  `--engines qlever` measures QLever again and keeps the other engines' numbers.

  On a machine with less memory than the engines can use, set `SERVER_MEM_MAX` (for
  example `SERVER_MEM_MAX=12G`). Each server then runs in a systemd scope with that memory
  limit and no swap. An engine that exceeds it is killed on its own, and its remaining
  queries are reported as errors. Without the limit, an engine that grows past the
  machine's memory can make the whole machine stop responding.
* `scripts/bench-text.sh` (`mise run bench:text [people] [workdir]`) compares full-text
  search on the same generated data. Sparkles and Jena Fuseki both answer `text:query`.
  Fuseki serves a TDB2 store wrapped in a jena-text dataset with a Lucene index, and QLever
  answers `ql:contains-word` over a text index built from its literals. Fluree is left out
  because its documentation offers full-text scoring only in JSON-LD queries, and
  Oxigraph has no text search.

  The script loads each store without timing it, then times each text index build and
  records the index size on disk. Sparkles and Jena index `foaf:name`, `ex:title` and
  `rdfs:label`. QLever indexes every literal, since its text index cannot be limited to
  predicates, so its queries join the matching literal with the predicate. There are seven
  queries. Two take the top 10 by score, one for a rare word and one for a common word.
  The third counts all hits of the common word, the fourth joins them with a structural
  pattern, and the fifth asks for two words that must both occur. The sixth returns the
  hits of the common word with highlighted literals, which QLever cannot produce, so its
  form returns plain literals and only the counts are compared. The seventh is a stemmed
  search in English titles, which QLever runs for the indexed word, since it does not stem.
  Before timing, the script
  compares every engine's hit counts and hit sets with `scripts/bench-answers.py`. Scores and their order are never compared,
  because the engines rank differently. QLever builds its index with explicit scoring,
  because its BM25 and TF-IDF scoring fail on language-tagged literals in version 0.5.48.

  The query words are ones that the Lucene standard analyzer, the Tantivy tokenizer in
  Sparkles and QLever split and lowercase the same way. A prefix query is left out because
  QLever returns a row for each matching word, so a name with two matching words counts
  twice. Sparkles and Jena both take `al*`. The script writes
  `results/text-summary.md` in the work directory. `DATA` reuses a dataset that
  `scripts/bench.sh` generated. `PORT_BASE` moves the three servers to the ports after
  it, and its default is 3940.
* `scripts/bench-billion.sh` (`mise run bench:billion [scale]`) runs a comparison on real
  data: English DBpedia, release 2022.12.01 (every English file of its generic, mappings
  and text groups, and the DBpedia ontology), 1.24 billion triples in all. The files, their
  URLs and checksums are in `scripts/bench-billion/dbpedia-2022.12.tsv`. The scale is a
  parameter, so routine runs stay small and the full dataset is one command away:

  ```sh
  mise run bench:billion            # about 50M triples, Sparkles and QLever
  mise run bench:billion 10m --no-cold --runs 3
  mise run bench:billion 250m --engines "sparkles qlever oxigraph"
  mise run bench:billion full       # every file: 1.24B triples
  ```

  The first run downloads 12.7 GB (resumable, checksum-verified) into
  `target/bench-billion` (`--workdir`) and recompresses it as N-Triples with zstd (16 GB
  more, no uncompressed copies). The release's `.ttl` files are N-Triples except for
  96,021 lines of `images` with a `\n` escape in an IRI, which no N-Triples parser takes:
  they are dropped, so every engine loads the same triples. Some IRIs hold U+FFFD and are
  not valid RFC 3987 IRIs; QLever, Jena and lenient Oxigraph keep them, and Sparkles
  loads them with `sparkles load --lenient`.

  A scale below `full` keeps a fixed sample of the subjects (by a hash of the subject),
  with all their triples in every file, about that many lines; files repeat some
  triples, so the engines hold fewer distinct ones (41.1M at `50m`). Every slice contains
  the smaller ones. Each scale has its own directory with its data, the engines' indexes
  and `results/summary.md`. Downloads, slices and indexes are reused by later runs;
  `--reload` rebuilds the indexes, and steps can run alone (`--steps load`,
  `--steps "queries report"`).

  The queries (`scripts/bench-billion/queries`) are DBpedia-style lookups instantiated
  with a fixed seed from the entities of the `10m` slice, in the manner of the DBpedia
  SPARQL Benchmark (whose templates are not copied: they were published without a license), plus
  analytic queries over the whole dataset: counts, group-bys, a property path, text and
  range filters. `scripts/bench-billion/instantiate.py` regenerates them. Every engine's
  answers are compared first (`scripts/bench-answers.py`), then each query is timed warm
  (hyperfine), cold (one run after a restart with the engine's files evicted from the
  page cache; `--no-cold` skips it), and two of them under 16 concurrent clients. Loads
  record their time, peak RSS (GNU `time`) and index size. `--engines` also takes `jena`
  (TDB2 `xloader` and Fuseki) and `oxigraph`. Jena and Oxigraph store `xsd:float`
  literals as values, so latitudes written with different precision that round to the
  same float are one triple there and two in Sparkles and QLever (3 triples at `50m`):
  `count-all` and `predicate-counts` then have no majority answer and are not ranked.
* `scripts/bench-watdiv.sh` (`mise run bench:watdiv [scale] [workdir]`) runs the five
  engines of `scripts/bench.sh` on WatDiv, the Waterloo SPARQL Diversity Test Suite. Its
  20 basic query templates cover linear, star, snowflake and complex query shapes. Scale
  factor 1 generates about 112,000 triples. The default of 100 generates 11.0 million,
  of which 10.9 million are distinct. The workdir defaults to `target/bench-watdiv`.

  WatDiv's generator is a C++ program, and its source is not part of this repository.
  The script builds it with Nix from the WatDiv v0.6 release, which
  `scripts/bench-watdiv/watdiv.nix` pins by checksum, against the nixpkgs revision in
  `flake.lock`. The release seeds its random generators from the clock and reads the
  system word list, so the build patches it to use a fixed seed and a pinned word list.
  The same scale and seed always produce the same data and the same queries. The seed is
  `WATDIV_SEED` and defaults to 1. The generator, the data and the queries are cached in
  the workdir.

  The generator often draws the same query instance more than once. The script keeps the
  first five distinct instances of each template, and `--instances` changes the count.
  The three complex templates have no parameters, so each of them has a single instance.
  The engines load and serve the data exactly as in `scripts/bench.sh`, and every
  engine's answer to every instance is compared before anything is timed.

  `results/summary.md` gives each template's geometric mean over its instances. It then
  gives the geometric mean of each category and of all templates. These rows only use
  the templates that every engine answered correctly, so all engines are measured on the
  same queries. `results/instances.md` lists every instance with its row count and
  times. `ENGINES=""` only generates the data and the queries.

  WatDiv may be used freely on the condition that publications cite G. Aluç, O. Hartig,
  M. T. Özsu and K. Daudjee, "Diversified Stress Testing of RDF Data Management
  Systems", ISWC 2014.
* `scripts/gen-data.py N` generates a synthetic dataset for benchmarking.
* `scripts/gen-geo.py N` generates a GeoSPARQL dataset and its queries. The data has
  points around cities, lines, polygons and an administrative hierarchy.
* `scripts/bench-geo.sh N` times the GeoSPARQL queries with and without the spatial
  index, and the nearest-neighbour query with the spatial rewrites off. It first checks
  that every run gives the same answers.
* `scripts/geosparql-benchmark.sh` runs the GeoSPARQL Compliance Benchmark. The
  benchmark is GPL-2.0, so it is never added to the repository. The script fetches it
  into `target/` at a pinned commit, and only when `SPARKLES_ALLOW_GPL_BENCHMARK=1` is
  set.
* `scripts/backup-bench.sh DB` (`mise run bench:backup DB`) backs up an existing
  database to an `fs` repository. It times a full backup, an incremental backup after
  small commits, a restore and a data verification.

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
* **Other outputs:**
  * `overlays.default`;
  * a dev shell;
  * `checks`:
    * the packages;
    * `sparkles-tests`, which runs the unit tests of the engine and the facade
      (`cargo test -p sparkles-core -p sparkles --lib`);
    * `fmt`, which runs rustfmt over the workspace and `crates/sparkles-py` as
      `mise run fmt:check` does;
    * `python-bindings`, which builds `sparkles-py` and runs the pytest suite on the
      installed package;
    * `ui-licenses`, which checks that `THIRD_PARTY_LICENSES-UI.md` matches the UI build;
    * on Linux, a NixOS VM test of the module behind nginx;
    * on Linux, `ui-e2e`, which runs the Playwright UI tests against the release binary
      in nixpkgs' headless Chromium, inside the build sandbox on 127.0.0.1;
    * on Linux, `jena-clients`, which runs the Jena client tests against `sparkles-cli`
      with nixpkgs' `apache-jena` and JDK, inside the build sandbox;
  * `nixosModules.default` (see [Deploying on NixOS](USAGE.md#deploying-on-nixos)).

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
project's `LICENSE`, as `pyproject.toml` declares. `licenses:check` checks both files.

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
