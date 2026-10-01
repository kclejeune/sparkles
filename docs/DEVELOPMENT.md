# Development

Building Sparkles from source, the everyday tasks, the test suites, the Nix flake and the
third-party license notices. Running and operating the binary is in [USAGE.md](USAGE.md);
the UI's own development notes are in [ui/README.md](../ui/README.md), and the design
specs, which say why each feature is built the way it is, in
[specs/README.md](specs/README.md).

Sparkles is experimental: the on-disk format, HTTP API, CLI and Rust API may change
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

Rust comes from `rust-toolchain.toml` (which also installs the `wasm32-unknown-unknown`
target). The formatter's WebAssembly module, which lets the UI format in the page, is
optional: `mise run ui:wasm` builds `crates/sparkles-fmt-wasm` into `ui/src/lib/wasm/`
(git-ignored), and later UI builds keep it up to date; without it the UI formats through
`POST /$/format`. The Nix packages always build it in. See
[ui/README.md](../ui/README.md#formatting-in-the-browser).

## mise tasks

[`mise.toml`](../mise.toml) pins Node, pnpm, hyperfine, prek, shfmt and shellcheck; Rust
comes from `rust-toolchain.toml`. It also defines the everyday tasks (`mise tasks` lists
them all):

```sh
mise run build        # UI + release binary
mise run serve        # build, then serve ./data on :3030
mise run fmt          # cargo fmt + oxfmt        (fmt:check for CI)
mise run lint         # clippy -D warnings + svelte-check
mise run lint:features # clippy -D warnings over feature combinations (in ci)
mise run fmt:wasm     # clippy for the formatter's wasm32 build (in ci)
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

Git hooks live in [`.pre-commit-config.yaml`](../.pre-commit-config.yaml) and run with
[prek](https://github.com/j178/prek) (plain `pre-commit` reads the same file). On staged
files they run `cargo fmt`, oxfmt on `ui/` (the UI's formatter, a devDependency),
`nix fmt` (nixfmt, from the flake) on `*.nix`, and shfmt and shellcheck on shell scripts.
`mise run hooks:install` installs the hook once per clone; `mise run hooks:run` runs every
hook over the whole tree.

## Testing

```sh
mise run ci            # formatting, clippy, all workspace tests, svelte-check, UI unit tests, license notices
mise run lint:features # clippy over feature combinations (in ci)
mise run test:w3c      # W3C SPARQL 1.0 / 1.1 query / 1.1 update / 1.2 suites, with a summary
mise run test:shacl    # W3C SHACL Core and SHACL-SPARQL suites
mise run test:shex     # shexTest: syntax, negative syntax and structure, representation, ShExR, validation
mise run ui:e2e        # Playwright end-to-end tests against a real server
```

`crates/sparkles/tests/w3c.rs` runs the W3C SPARQL suites vendored in the Apache Jena
checkout (`../../apache/jena` next to this repository, or `SPARKLES_W3C_DIR`); the SHACL
suites come from the same checkout (or `SPARKLES_SHACL_TESTS`). Without the checkout the
suites are skipped. All of them pass (482/482, 328/328, 157/157, 269/269; SHACL 98/98 and
20/20); `crates/sparkles/tests/w3c-known-failures.txt` lists known failures and is empty.
The shexTest suite comes from the same checkout (`jena-shex`, or `SPARKLES_SHEX_TESTS`
for an upstream shexTest checkout); `crates/sparkles-shex/tests/known-failures.txt` lists
what fails, with reasons.

`mise run lint:features` (`scripts/lint-features.sh`) runs clippy with warnings as errors
over the builds `mise run lint` does not see: the server with no optional feature, with
each default feature on its own, and with `mcp,shacl` and `mcp,shex`; `sparkles` and
`sparkles-fmt` with their features off; the `sparkles-backup` library without a backend.
Code used only under some feature is gated on it, so this catches dead code and missing
gates. Every combination shares the workspace target directory: the first run builds the
dependencies once per feature set, and after that a change costs a minute or two.

`mise run ui:e2e` builds the UI and a debug server, starts `sparkles serve` on a free
port of 127.0.0.1 with a temporary data directory, a small dataset and an auth
configuration (one user, one API token), runs the Playwright tests in `ui/tests/e2e` in
headless Chromium and stops the server. `SPARKLES_BIN=path/to/sparkles` tests another
binary; extra arguments go to `playwright test` (`mise run ui:e2e -- -g Similar`). The
flake's dev shell provides a Chromium matching the pinned `@playwright/test`
(`nix develop -c mise run ui:e2e`); elsewhere the task downloads one with
`playwright install chromium`. The task is not part of `mise run ci`; on Linux the flake
runs the same tests as the check `ui-e2e` (see [Nix](#nix) below).

## Benchmark scripts

The results are in [BENCHMARKS.md](BENCHMARKS.md), which also says how each run was set up.

* `scripts/bench.sh` (`mise run bench [people] [workdir]`) runs the engine comparison.
* `scripts/gen-data.py N` generates a synthetic dataset for benchmarking.
* `scripts/gen-geo.py N` generates a GeoSPARQL one (points around cities, lines,
  polygons, an administrative hierarchy) with its queries, and `scripts/bench-geo.sh N`
  times them with and without the spatial index (and the nearest-neighbour query with the
  spatial rewrites off) after checking that every run gives the same answers.
* `scripts/geosparql-benchmark.sh` runs the GeoSPARQL Compliance Benchmark. The
  benchmark is GPL-2.0, so the script fetches it into `target/` at a pinned commit only
  with `SPARKLES_ALLOW_GPL_BENCHMARK=1`, and it is never added to the repository.
* `scripts/backup-bench.sh DB` (`mise run bench:backup DB`) backs up an existing database
  to an `fs` repository and times a full backup, an incremental one after small commits,
  a restore and a data verification.

## Nix

The flake (flake-parts + rust-overlay, using the toolchain from `rust-toolchain.toml`)
provides:

* **Packages:**
  * `sparkles` (default): the binary with the UI embedded (formatter WebAssembly module
    included), and the third-party licenses and notices in `share/doc/sparkles/`.
  * `sparkles-cli`: the same binary without the UI, so the build needs no Node.js.
  * `sparkles-ui`: the static UI build.
  * `sparkles-fmt-wasm`: the formatter's WebAssembly module, which `sparkles-ui` builds in.
* **Other outputs:**
  * `overlays.default`;
  * a dev shell;
  * `checks`: the packages, `ui-licenses` (`THIRD_PARTY_LICENSES-UI.md` matches the UI
    build); on Linux also a NixOS VM test of the module behind nginx and
    `ui-e2e`, the Playwright UI tests against the release binary in nixpkgs' headless
    Chromium (in the build sandbox, on 127.0.0.1);
  * `nixosModules.default` (see [Deploying on NixOS](USAGE.md#deploying-on-nixos)).

```sh
nix run github:kclejeune/sparkles -- serve --data ./data
nix build .#sparkles-cli
nix flake check          # packages + NixOS VM test (Linux, needs KVM) + UI end-to-end tests
nix build .#checks.x86_64-linux.ui-e2e -L   # only the UI end-to-end tests
```

## Third-party licenses

[`THIRD_PARTY_LICENSES.md`](../THIRD_PARTY_LICENSES.md) holds the license and NOTICE files of
every crate the binary links (on Linux and macOS), each text once; crates that ship no
license file get their license's standard text. `scripts/third-party-licenses.py`
generates it from `cargo metadata`, so it changes only with `Cargo.lock`. Ship it with
binaries (the Nix packages install it as `share/doc/sparkles/THIRD_PARTY_LICENSES.md`).

[`THIRD_PARTY_LICENSES-UI.md`](../THIRD_PARTY_LICENSES-UI.md) does the same for the npm
packages whose code or fonts end up in the embedded web UI (CodeMirror, Cytoscape,
MapLibre GL JS and its dependencies, the Svelte and SvelteKit runtime, Vite's and
Rolldown's runtime helpers, the Fontsource fonts and their dependencies), whether
dependencies or devDependencies; build tools that ship nothing are left out. A Vite
plugin (`ui/scripts/licenses.js`) takes them from the bundle's module graph and the
source files of its assets, reads their license files from `node_modules`, and writes the
notices into the build as `licenses.txt`, which the server serves at `/ui/licenses.txt` (the `sparkles`
Nix package also installs it as `share/doc/sparkles/THIRD_PARTY_LICENSES-UI.md`). The UI
build fails when a shipped package's license allows none of MIT, MIT-0, ISC, 0BSD,
BSD-2-Clause, BSD-3-Clause, Apache-2.0, Zlib, Unlicense, CC0-1.0, BlueOak-1.0.0 or
OFL-1.1 (the fonts' license). The UI notices are a separate file because they change with
`ui/pnpm-lock.yaml` and need the UI build, and `sparkles-cli` ships without the UI.
`mise run licenses` builds the UI and copies the file; `mise run licenses:check` (part of
`mise run ci`) fails when either file is out of date, and so does the flake check
`ui-licenses`.
