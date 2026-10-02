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
through `POST /$/format`. The Nix packages always build it in. See
[ui/README.md](../ui/README.md#formatting-in-the-browser).

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
mise run fmt:wasm     # clippy for the formatter's wasm32 build (in ci)
mise run fmt:fuzz     # fuzz the formatter with cargo-fuzz (needs nightly and cargo-fuzz; not in ci)
mise run test         # all Rust tests            (test:w3c, test:shacl, test:shex for suite summaries)
mise run ui:wasm      # the formatter's WebAssembly module for the UI (optional)
mise run ui:dev       # UI dev server, proxying the API to $SPARKLES_API (default http://localhost:3030)
mise run ui:mock      # mock API server for UI work without the Rust backend
mise run ui:test      # UI unit tests (Vitest)
mise run ui:e2e       # UI end-to-end tests (Playwright; Chromium from `nix develop`, see below)
mise run ui:e2e:mock  # UI end-to-end tests against the mock backend
mise run ci           # fmt:check + lint + lint:features + fmt:wasm + test + ui:test + licenses:check
mise run doc          # API docs of the library crates
mise run docs:screenshots # the README's screenshots (docs/images) from the demo dataset in docs/demo
mise run gen-data 1000000 target/bench-data/10m.nt
mise run bench        # Sparkles vs Fuseki vs QLever; `bench 1000000 --runs 5` for 10.5M triples
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
mise run ci            # formatting, clippy, all workspace tests, svelte-check, UI unit tests, license notices
mise run lint:features # clippy over feature combinations (in ci)
mise run test:w3c      # W3C SPARQL 1.0 / 1.1 query / 1.1 update / 1.2 suites, with a summary
mise run test:shacl    # W3C SHACL Core and SHACL-SPARQL suites
mise run test:shex     # shexTest: syntax, negative syntax and structure, representation, ShExR, validation
mise run ui:e2e        # Playwright end-to-end tests against a real server
```

`crates/sparkles/tests/w3c.rs` runs the W3C SPARQL suites vendored in an Apache Jena
checkout. It looks for the checkout at `../../apache/jena`, next to this repository, or
at `SPARKLES_W3C_DIR`. The SHACL suites come from the same checkout, or from
`SPARKLES_SHACL_TESTS`. Without the checkout, the suites are skipped.

All of these suites pass: 482/482, 328/328, 157/157 and 269/269 for SPARQL, and 98/98
and 20/20 for SHACL. `crates/sparkles/tests/w3c-known-failures.txt` lists known failures
and is empty.

The shexTest suite comes from the same Jena checkout (`jena-shex`). Set
`SPARKLES_SHEX_TESTS` to use an upstream shexTest checkout instead.
`crates/sparkles-shex/tests/known-failures.txt` lists the tests that fail, with reasons.

`mise run lint:features` (`scripts/lint-features.sh`) runs clippy, with warnings as
errors, over the builds that `mise run lint` does not cover:

* the server with no optional feature, with each default feature on its own, and with
  `mcp,shacl` and `mcp,shex`;
* `sparkles` and `sparkles-fmt` with their features off;
* the `sparkles-backup` library without a backend.

Code that only some features use is gated on those features, and this task catches dead
code and missing gates. Every combination shares the workspace target directory. The
first run builds the dependencies once per feature set. After that, a change costs a
minute or two.

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

The flake is built on flake-parts and rust-overlay, with the toolchain from
`rust-toolchain.toml`. It provides:

* **Packages:**
  * `sparkles` (default): the binary with the UI and the formatter's WebAssembly module
    embedded, and the third-party licenses and notices in `share/doc/sparkles/`.
  * `sparkles-cli`: the same binary without the UI, so the build needs no Node.js.
  * `sparkles-ui`: the static UI build.
  * `sparkles-fmt-wasm`: the formatter's WebAssembly module, which `sparkles-ui` builds in.
* **Other outputs:**
  * `overlays.default`;
  * a dev shell;
  * `checks`:
    * the packages;
    * `ui-licenses`, which checks that `THIRD_PARTY_LICENSES-UI.md` matches the UI build;
    * on Linux, a NixOS VM test of the module behind nginx;
    * on Linux, `ui-e2e`, which runs the Playwright UI tests against the release binary
      in nixpkgs' headless Chromium, inside the build sandbox on 127.0.0.1;
  * `nixosModules.default` (see [Deploying on NixOS](USAGE.md#deploying-on-nixos)).

```sh
nix run github:kclejeune/sparkles -- serve --data ./data
nix build .#sparkles-cli
nix flake check          # packages + NixOS VM test (Linux, needs KVM) + UI end-to-end tests
nix build .#checks.x86_64-linux.ui-e2e -L   # only the UI end-to-end tests
```

## Third-party licenses

[`THIRD_PARTY_LICENSES.md`](../THIRD_PARTY_LICENSES.md) holds the license and NOTICE
files of every crate the binary links on Linux and macOS, with each text included once.
Crates that ship no license file get their license's standard text.
`scripts/third-party-licenses.py` generates the file from `cargo metadata`, so it changes
only when `Cargo.lock` does. Ship it with binaries. The Nix packages install it as
`share/doc/sparkles/THIRD_PARTY_LICENSES.md`.

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
