# X12: Distribution and format compatibility

> **Status:** partially implemented (Phase 3)
>
> **Phases:** Phase 3 shipped: the on-disk format policy with older-format fixtures,
> in-place upgrades and `sparkles migrate`. Phase 1 is one release workflow for every
> artifact, a single version across the server, crates and bindings, server binaries
> with checksums and provenance attestations, and published container images. Phase 2
> widens the platform matrix to Windows, Intel macOS, musl for the JVM, and PyPy and
> free-threaded CPython wheels. Phase 4 is the HTTP, CLI and library compatibility
> policy with its checks.
>
> **User docs:** [USAGE: Upgrading databases](../USAGE.md#upgrading-databases) and
> [DEVELOPMENT: Changing an on-disk format](../DEVELOPMENT.md#changing-an-on-disk-format).
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it lands.

This design was written from the Sparkles code at `537e048`: the four workflows in
`.github/workflows`, the `Dockerfile`, `nix/image.nix`, `nix/package.nix`, the Helm chart
in `deploy/helm/sparkles`, the manifests of every crate and binding, and the version
checks of the store (`crates/sparkles-core/src/builder.rs`, `store.rs`, `commit.rs`,
`vocab/delta.rs`, `check.rs`) and of backups (`crates/sparkles-backup`). It also draws on
[DEVELOPMENT.md](../DEVELOPMENT.md#releases), the stability notes in
[USAGE.md](../USAGE.md) and the README, and the specs [P01](P01-python-bindings.md),
[P04](P04-jvm-bindings.md), [P05](P05-node-bindings.md), [F05](F05-snapshot-repositories.md)
and [F11](F11-encryption-at-rest.md). The external sources are Semantic Versioning 2.0.0,
the Cargo book's chapter on SemVer compatibility, PEP 425 and PEP 600 (wheel tags and
manylinux), PEP 656 (musllinux), PEP 703 (free-threaded CPython), PEP 740 (index
attestations), the SLSA 1.0 build provenance specification, the in-toto attestation
format, the Sigstore documentation of keyless signing, GitHub's documentation of
artifact attestations, npm's documentation of `--provenance`, the OCI image index
specification, RFC 8594 (the `Sunset` header) and RFC 9745 (the `Deprecation` header).

## 1. Summary, goals, non-goals

Sparkles has no releases. The README and USAGE.md say that the on-disk format, the HTTP
API, the CLI and the library APIs can change in any commit without a migration path.
The release machinery that exists is partial:

* Three binding workflows (`python-wheels.yml`, `jvm-bindings.yml`,
  `node-packages.yml`) build and test on every pull request that touches their sources,
  and build every platform for a `py-v*`, `jvm-v*` or `node-v*` tag. Each has a publish
  job that runs only when the repository variable `PYPI_PUBLISH`, `MAVEN_PUBLISH` or
  `NPM_PUBLISH` is `true`. None is set, so nothing has been published.
* The platforms are Linux x86_64 and arm64 and macOS on Apple silicon. Python adds
  musllinux wheels and Node adds musl addons. No workflow builds for Windows or Intel
  macOS, the JVM loader refuses musl (`NativeLoader.kt`), and the Python wheel is abi3
  for CPython 3.10 and later only.
* No workflow builds the `sparkles` server binary for distribution or publishes an
  image. The `Dockerfile` and `nix build .#image` build images locally, and the Helm
  chart's `image.repository` defaults to `sparkles` with the note that the project
  publishes none.
* Versions are 0.1.0 everywhere, but they are kept in five places: the workspace
  `Cargo.toml`, `crates/sparkles-py/Cargo.toml`, `crates/sparkles-ffi/Cargo.toml`,
  `crates/sparkles-node/Cargo.toml` with `js/*/package.json`, and `jvm/gradle.properties`.
  The Helm chart has its own `version` and `appVersion`.
* The store refuses a generation whose `meta.json` has an index format other than
  `FORMAT_VERSION` (2), with the message "dump and reload the data"
  (`store.rs`, `Generation::open_with`). A backup whose `indexFormat` differs is refused
  with `422 incompatible-format` (`sparkles-backup/src/restore.rs`).

**Goals**

* One tag releases everything at one version: crates, the server binary, images, wheels,
  jars and npm packages.
* Every published artifact has a SHA-256 checksum and a provenance attestation that a
  user can verify, and the registries' own signing (PyPI attestations, npm provenance,
  Maven Central's PGP signatures) is used where it exists.
* The platform matrix covers Linux (glibc and musl, x86_64 and arm64), macOS (arm64 and
  x86_64) and Windows x86_64 for the server and every binding, with PyPy and
  free-threaded CPython wheels.
* Each on-disk file kind has a version that its reader checks. A release opens the
  databases of the releases inside a stated window, either directly or after an
  in-place upgrade, and tests prove it against fixtures written by those releases.
* The HTTP API, CLI and library APIs have a written compatibility policy with automated
  checks in CI.
* Every artifact is installed from its registry on every platform and smoke-tested
  after publishing.

**Non-goals**

* A stable on-disk format before 1.0. Phase 3 gives 0.x releases a window of one minor
  release, so an upgrade from 0.3 to 0.4 needs no dump, but 0.2 to 0.4 may.
* Downgrades in place. `minimumReader` already stops an older release from misreading a
  newer database. Going back stays a dump and reload.
* Linux distribution packages (deb, rpm), Homebrew taps, winget and Chocolatey. They can
  follow once binaries exist.
* Code signing for macOS (notarization) and Windows (Authenticode) in Phase 1. Open
  question 3.
* Windows on arm64 and FreeBSD. P05's open question 7 keeps them.

## 2. User-visible behaviour

### 2.1 What a release publishes

A tag `vX.Y.Z` on `main` produces:

| Artifact | Where | Platforms |
|---|---|---|
| `sparkles` server and CLI binary, `.tar.gz` (`.zip` on Windows) with the man pages, completions, `openapi.json`, `LICENSE` and the third-party notices | GitHub Release assets | §3.1 |
| `SHA256SUMS` and its Sigstore bundle | GitHub Release assets | one file for all |
| Container images `sparkles` and `sparkles` with the `-ocr` suffix | `ghcr.io/kclejeune/sparkles` | `linux/amd64`, `linux/arm64` |
| Helm chart | `oci://ghcr.io/kclejeune/charts/sparkles` | — |
| `sparkles-rdf` wheels and sdist | PyPI | §3.2 |
| `io.github.kclejeune:sparkles-jena`, `-natives`, `-all` | Maven Central | §3.3 |
| `@sparkles-rdf/*` packages | npm | §3.4 |
| `sparkles`, `sparkles-client` and the crates they depend on | crates.io | source |

The GitHub Release is created as a draft by the workflow and published by the
maintainer after the smoke tests of §6 pass. Its notes come from `CHANGELOG.md`, which
this spec adds, with a **Format** and a **Compatibility** heading in every entry (§4.5,
§5.5).

### 2.2 Verifying a download

```sh
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify sparkles-0.4.0-x86_64-unknown-linux-gnu.tar.gz \
  --repo kclejeune/sparkles
```

The attestation is GitHub's artifact attestation: an in-toto statement with a SLSA build
provenance predicate, signed with a short-lived Sigstore certificate bound to the
workflow's identity, and recorded in the Sigstore transparency log. Images carry the
same attestation, pushed to the registry next to the image, and
`gh attestation verify oci://ghcr.io/kclejeune/sparkles:0.4.0 --repo kclejeune/sparkles`
checks it. npm packages already publish with `--provenance` (`node-packages.yml`). PyPI
uploads made through trusted publishing with `pypa/gh-action-pypi-publish` carry PEP 740
attestations. Maven Central requires PGP signatures, which the JVM workflow already
makes.

### 2.3 Image tags

| Tag | Meaning |
|---|---|
| `0.4.0`, `0.4.0-ocr` | That release. Never moved. |
| `0.4`, `0.4-ocr` | The newest patch release of 0.4. |
| `latest`, `latest-ocr` | The newest release. |
| `sha-<12 hex>` | A build of `main`, published by a manual run for testing. Kept 30 days. |

Each tag names an OCI image index with `linux/amd64` and `linux/arm64` manifests. The
Helm chart's `appVersion` is the release version, and its `image.repository` default
becomes `ghcr.io/kclejeune/sparkles`.

### 2.4 Version reporting

`sparkles --version` prints the version, the commit, the target triple and the cargo
features, for example `sparkles 0.4.0 (3f1c9a2e1b04 x86_64-unknown-linux-gnu; features:
default, embed-local, backup-encryption)`. `GET /$/server` adds `commit`, `target` and
`formats`, the last being the reader and writer versions of §4.1, so an operator can see
which databases a running server opens. The Python, JVM and Node packages expose the
same object (`sparkles.build_info()`, `Sparkles.buildInfo()`, `buildInfo()`).

### 2.5 Opening an older database

A release opens a database inside its window (§4.2) without a command. If the database
needs an upgrade that is not done on open, the server refuses that dataset at start with
the message

```
dataset "books" was written by Sparkles 0.3 (index format 2); this release reads index
format 3 and upgrades format 2 with `sparkles migrate --data /data --dataset books`
```

and keeps serving the other datasets. `sparkles migrate` (§4.4) does the upgrade
offline. A database outside the window gets the same refusal with `sparkles dump` and
`sparkles load` from the release that wrote it as the remedy.

## 3. Platforms

### 3.1 Server binary

| Target | Runner | Notes |
|---|---|---|
| `x86_64-unknown-linux-gnu` | `ubuntu-24.04`, build in the manylinux 2.28 image | glibc 2.28 or later |
| `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm`, same image | glibc 2.28 or later |
| `x86_64-unknown-linux-musl` | `ubuntu-24.04`, Alpine container | static |
| `aarch64-unknown-linux-musl` | `ubuntu-24.04-arm`, Alpine container | static |
| `aarch64-apple-darwin` | `macos-15` | macOS 11 or later |
| `x86_64-apple-darwin` | `macos-15-intel` | macOS 11 or later (Phase 2) |
| `x86_64-pc-windows-msvc` | `windows-2025` | Phase 2 |

The glibc builds use the manylinux 2.28 image that the Python wheels already use, so the
binary runs on the same distributions as the wheels. The Debian bookworm image the
`Dockerfile` builds in links against glibc 2.36, which is too new for older
distributions. The musl builds use the server's default `mimalloc` allocator, so musl's
allocator does not slow them down.

Every binary uses one feature set: the defaults of `sparkles-server`, `embed-local` and
`backup-encryption`. Today the three image paths disagree. The `Dockerfile` adds
`embed-local` only, `nix/package.nix` adds `embed-local` and, on Linux,
`backup-encryption`, and a plain `cargo build` adds neither. §8's C19 dependency on
sealed runtime secrets needs the default `backup` feature in published builds.
Repository encryption keeps its separate `backup-encryption` feature and platform
requirements. The `pdf-ocr`
feature is in the `-ocr` image only, because it needs PDFium and ONNX Runtime at run time.

The release profile keeps line tables (`debug = 1`), which the `Dockerfile` notes make
up most of a 450 MB binary. Release binaries are stripped with `objcopy --strip-debug`
as the image is, and the debug information goes to a separate `sparkles-X.Y.Z-<target>.debug.tar.gz`
asset.

### 3.2 Python

The current wheels are abi3 (`abi3-py310`) for manylinux 2.28 and musllinux 1.2 on
x86_64 and aarch64 and for macOS 11 on arm64, plus an sdist. Phase 2 adds:

* **Windows x64 and macOS x86_64** abi3 wheels, on `windows-2025` and `macos-15-intel`.
  P01's Outcome records that these jobs existed and were removed on 2026-10-09 to save
  CI time, and that the code paths remain.
* **Free-threaded CPython 3.14t.** abi3 does not cover the free-threaded build (PEP 703),
  so these are version-specific `cp314t` wheels. The module declares free-threading
  support only after the suite passes on 3.14t with `PYTHON_GIL=0`, and the wheel test
  runs that way. Until then a 3.14t interpreter installs the sdist, which re-enables the
  GIL on import with a warning, as CPython does for a module without the declaration.
* **PyPy 3.11** wheels (`pp311`), which PyO3 supports, built for manylinux x86_64 and
  aarch64 and macOS arm64, and tested with the same suite.
* **Async wrappers**, P01's remaining item, are API work rather than packaging. They
  belong to P01's addendum.

### 3.3 JVM

`sparkles-jena-natives` builds `linux-x86_64`, `linux-aarch64` and `macos-aarch64`
(`jvm/sparkles-jena-natives/build.gradle.kts`), and the loader already knows
`macos-x86_64` and `windows-x86_64`. Phase 2 adds those two and a musl directory,
`linux-musl-x86_64` and `linux-musl-aarch64`, built in Alpine as the Node workflow does.
The loader's musl check (`NativeLoader.kt`, `isMusl`) then selects the musl directory
instead of failing. Each new platform gets a classifier jar.

### 3.4 Node.js

The engine has five platform packages: `linux-x64-gnu`, `linux-arm64-gnu`,
`linux-x64-musl`, `linux-arm64-musl` and `darwin-arm64` (`js/scripts/platforms.mjs`).
Phase 2 adds `darwin-x64` and `win32-x64-msvc`, which [P05 §7.1](P05-node-bindings.md) already names.

### 3.5 Windows

The engine has Windows code paths: `write_all_at` in `store/wal.rs`, and
`#[cfg(windows)]` branches in `sparkles-backup/src/restore.rs`, `lsp.rs` and
`remote/login.rs`. No CI job compiles them. Phase 2 starts with a Windows job in
`rust.yml`'s manual all-platforms run that builds the workspace and runs the store,
catalog and backup tests. The bindings and the server join the release matrix only after
that job passes. Two behaviours need decisions on Windows and are listed as open
question 4: a memory-mapped file cannot be deleted or replaced while mapped, which
affects generation switches and catalog swaps, and the data directory lock uses file
locking whose semantics differ from `flock`.

## 4. On-disk formats

### 4.1 File kinds and their versions

| Kind | Files | Version marker today | Class |
|---|---|---|---|
| Dataset | `dataset.json` | `format` 1 and `minimumReader`, read up to `DATASET_READER` 4 (`commit.rs`) | primary |
| Generation index | `gen-*/meta.json` and the index files | `format_version`, must equal `FORMAT_VERSION` 2 (`builder.rs`) | primary |
| Vocabulary | `vocab.idx` (`SPKVIDX1`), numeric vocabulary (`SPKVNUM2`) | magic | primary |
| Delta vocabulary | `delta.vocab` | framed header `\x89SDV`, `VERSION` 2, or legacy without a header | primary |
| Write-ahead log | `wal.log` | none: 33-byte records | primary |
| Commit catalog | `commits.bin` | magic `SPKCMTS\0`, 64-byte records, flag bits added compatibly | primary |
| Change log | segments and index | magics `SPKCHGL\0`, `SPKCHGI\0`, `FORMAT` 1 | primary |
| Annotations | `annotations.bin` | magic `SPKANNO\0`, `FORMAT` 1 | primary |
| Catalog | `config.json`, `catalog.lock` | none | primary |
| Runtime secrets | `secrets/NAME` in the server data directory | Raw plaintext is version 0 at the server wrapper. Sealed values use `sparkles-sealed-secret/1 `, and encrypted version 0 is refused. | primary |
| Generated secrets key | `secrets.key` in the server data directory | The existing opaque key representation is unversioned format 0. | primary |
| Full-text index | Tantivy directory and `text.json` | `FORMAT` 2 (`text.rs`); another format is rebuilt on open | derived |
| Vector index | `vector.json`, index files | config `FORMAT_VERSION` 1, file `SPKVEC` `FILE_VERSION` 1 | derived |
| Spatial index | `geo.json`, index files | config `FORMAT_VERSION` 1, `LITERALS_VERSION` 2, file `SPKGEO` `FILE_VERSION` 1 | derived |
| Checked migration state | `.migrate-checked` at the root or checked copy | JSON format 1 records the cleanup mode and the exact old sibling directory. Unknown versions and ambiguous bare legacy markers are refused. | config |
| Settings and state | `describe.json`, `compaction.json`, `queries.json`, `validation.json`, `models.json`, `assistant.json` and others | `FORMAT` fields on some (stored queries 1, GraphQL 1, ShEx guard 2), none on most | config |
| Backup repository | layout, blobs, manifests, key slots | layout `FORMAT` 1, blob `FORMAT` 1, slot `SLOT_FORMAT` 2, manifest `indexFormat` | archive |

Primary files hold data that cannot be recomputed. Derived files are indexes that can be
rebuilt from primary files. Config files are small JSON settings. Archive formats live in
backup repositories, possibly for years.

Three kinds have no version marker: `wal.log`, the catalog's `config.json` and most
settings files. Phase 3 adds one to each before the first release, so that a later
change can be detected rather than misread:

* `wal.log` gains a 16-byte header with a magic and a version on files created after the
  change. A file without the header is version 0, read as today.
* `config.json` and every settings file gain a `format` member, absent meaning 1. Readers
  keep accepting unknown members, as serde does by default in these types today, so that
  additive changes need no version change.

### 4.2 The compatibility window

* **Before 1.0.** Release 0.N opens databases written by 0.N and 0.(N−1), and repository
  backups written by both.
* **From 1.0.** Release X.N opens databases and backups written by any X.* release and by
  the last minor release of X−1.

"Opens" means with no step other than, where §4.4 says so, `sparkles migrate`. A database
outside the window is refused with a message that names the release that wrote it and
the dump-and-reload path.

The release that wrote a database is recorded: `dataset.json` gains `writtenBy` (the
version of the last release that opened it for writing) next to `minimumReader`, so the
message can name it.

### 4.3 How a format changes

Each change is one of four kinds, chosen in this order:

1. **Additive.** A new member with a default, a new flag bit that old readers ignore, a
   new optional file. No version changes. Example: the commit catalog's flag bit for an
   unchanged default graph, which "records written before the flag existed read as may
   have changed it" (`commit.rs`).
2. **Lazy in place.** The new release reads both forms and writes the new form the next
   time it writes the file, then raises `minimumReader`. This is how framed delta
   vocabularies landed (`vocab/delta.rs`, `FRAMED_READER` 4): a legacy file is read and
   rewritten framed at the first write, and the dataset then requires reader 4. Use it
   when a file is rewritten as part of normal operation.
3. **At compaction.** The new release reads generation index formats N and N−1 and
   writes N. The next compaction, automatic ([C13](C13-automatic-compaction.md)) or
   manual, builds the new generation in format N. `store.rs` replaces its equality test
   with a set of readable formats, `READABLE_INDEX_FORMATS`, and `check.rs` follows it.
   Retained generations ([F06](F06-snapshots-and-point-in-time.md)) in format N−1 stay
   readable until they expire.
4. **Explicit migration.** When a reader cannot practically read the old form, the
   release ships an upgrade step in `sparkles migrate` and refuses the old form at open
   with the message of §2.5.

Derived files never need a migration. A derived index whose version the release does not
read is rebuilt in the background on open, as the full-text index already does, and the
feature reports `rebuilding` in its status until it is ready.

Config files change additively or with a `format` bump. A bump comes with an upgrade
function in the reader, which rewrites the file at the next settings write.

### 4.4 `sparkles migrate`

```
sparkles migrate --data DIR [--dataset NAME ...] [--check] [--keep-old]
sparkles migrate --loc DIR [--check] [--keep-old]
```

* Runs offline, holding the catalog lock as `sparkles backup policy run` does.
* `--check` lists each dataset, its formats, whether it is inside the window, and which
  steps an upgrade would run, and changes nothing.
* For each dataset it runs the registered steps in order. A step is a function from one
  version of a file kind to the next, registered in `crates/sparkles-core/src/migrate.rs`.
  Steps run on copies in a sibling directory, the result is checked with `sparkles check`,
  and the directory is swapped in with the catalog's existing swap
  (`crates/sparkles/src/catalog/swap.rs`). `--keep-old` keeps the previous directory as
  `.pre-migrate-<version>`.
* A generation index step is a compaction into the new format, which keeps the commit
  sequence, the dataset id and the commit catalog. Unlike dump and reload, history inside
  the current generation and the dataset id survive.
* It refuses a database outside the window, naming the release to dump it with.

The server never runs migration steps itself. It runs lazy upgrades (kind 2) and the
background rebuild of derived files, which are both safe to interrupt.

### 4.5 Backups

A restore accepts a backup whose `indexFormat` is inside the window and restores it as
written, so the restored dataset opens under the same rules as a database. The
`422 incompatible-format` answer stays for formats outside the window, and its message
names the release that wrote the backup, which the manifest already records in
`server.version`. The repository layout, blob and key-slot formats follow the same window.
Each CHANGELOG entry's **Format** heading lists every version that changed.

### 4.6 Fixtures

`testsuite/formats/<version>/` holds databases written by each release, created by
`scripts/format-fixture.sh` with the release's binary and committed as a compressed tar
of under 1 MB. Each fixture is a catalog with:

* a persistent dataset with two generations, a delta, named graphs, RDF 1.2 triple
  terms, blank nodes, a branch, a named snapshot, stored queries and settings;
* the full-text, vector (with a fixed embedding provider) and spatial indexes;
* a repository backup of the dataset in an `fs` repository, plaintext and encrypted.

`tests/formats.rs` in `sparkles-core` and a server test unpack every fixture inside the
window and run the same checks: `sparkles check` reports nothing, a fixed set of queries
answers as recorded in the fixture, `?at=` reads reach the recorded commits, a write and
a compaction succeed, the restored backup equals the source, and the database opens
again. For each fixture outside the window, the test checks the refusal message. The
fixture of the release being cut is generated by the release workflow and committed in
the release's changelog commit.

Until 0.1 ships there is no release to write a fixture, so the first fixture is written
at the commit that introduces this test, labelled with that commit.

## 5. API compatibility

### 5.1 What is covered

| Surface | Covered | Not covered |
|---|---|---|
| HTTP | Paths, methods, parameters, request and response members, status codes and error `code`s in `docs/openapi.json` | The text of error messages, members marked `x-sparkles-experimental`, the UI's private routes |
| SPARQL and Fuseki | Behaviour that matches Jena and Fuseki | Deliberate differences listed in COMPARISON.md, which change only with a CHANGELOG entry |
| CLI | Subcommand names, flags, exit codes, `--format json` output | Human-readable output, hidden flags such as `--experimental-group-commit` |
| Rust | Public items of `sparkles` and `sparkles-client` | `sparkles-core` and the other internal crates, which are published only as dependencies |
| Python, JVM, Node | Public API of the packages | Modules and classes whose names start with `_` or are under `internal` |
| MCP | Tool names, input schemas and output members | Tool descriptions |
| Metrics | Metric names and labels | Help text |

### 5.2 Rules

Before 1.0, a minor release may break any surface, and the CHANGELOG's
**Compatibility** heading lists every break. A patch release breaks nothing.

From 1.0, a minor release may add but not remove or change. A removal is announced in a
minor release, and the old form keeps working until the next major release. During
that time HTTP responses on a deprecated route or parameter carry `Deprecation` (RFC
9745) and, once a removal release is planned, `Sunset` (RFC 8594), the CLI prints a
warning on standard error, and the bindings emit their language's deprecation warning.

### 5.3 Checks

* **HTTP.** `openapi::tests` gains a check against the description of the last release,
  stored as `docs/openapi-<version>.json`. It fails on a removed operation, a removed
  response member, a new required request member, a narrowed type or enum, or a removed
  status code, unless the operation or member is experimental or the change is listed in
  `docs/api-breaks.toml` for the next release. The checked-in description's
  `info.version` is `0.0.0` today, while the served one is the package version; the
  release workflow writes the release's version into the stored copy.
* **Rust.** `cargo semver-checks` against the last published versions of `sparkles` and
  `sparkles-client`, in the release workflow and on pull requests that touch them.
* **Python, Node.** The type stubs (`.pyi`) and `.d.ts` files of the last release are
  compared with the current ones by a script that reports removed names and changed
  signatures.
* **JVM.** The compatibility job (`jvm-bindings.yml`, Java 17 with Jena 5.6.0 and Java 21
  with Jena 6.2.0) stays, and a binary compatibility check of `sparkles-jena` against
  the last release's jar is added.
* **CLI.** `tests/cli_docs.rs` stores the clap command tree of the last release as JSON
  and fails on a removed subcommand or flag.

### 5.4 Wire formats inside the bindings

The JVM binding's value encoding has `ENCODING_VERSION` 1 (`sparkles-ffi/src/encode.rs`).
Because the native library and the Kotlin code ship in the same release, the version only
guards against a mismatched classpath, and the loader refuses a mismatch. This stays
outside the window rules.

### 5.5 Changelog

`CHANGELOG.md` at the repository root, one entry per release, with **Added**,
**Changed**, **Fixed**, **Format** and **Compatibility** headings. The release workflow
refuses a tag whose version has no entry.

## 6. Install smoke tests

After the publish jobs, a `smoke` workflow installs each artifact from its registry on a
clean runner and runs the same short script: create an in-memory dataset, load 10
triples, run a SELECT and an ASK, check the answers, and for the server, check
`/$/ready` and `sparkles --version`.

| Artifact | Runners or images |
|---|---|
| Server tarball | `ubuntu-24.04`, `ubuntu-24.04-arm`, `macos-15`, `macos-15-intel`, `windows-2025`; containers `debian:11`, `rockylinux:8`, `alpine:3` for the musl build. The script also runs `sha256sum --check` and `gh attestation verify`. |
| Image | `docker run` on amd64 and arm64 runners, waiting for the health check to report healthy; the `-ocr` image converts a one-page PDF |
| Helm chart | `scripts/helm-smoke.sh` against a kind cluster with the published chart and image |
| PyPI | `pip install sparkles-rdf==X.Y.Z` with CPython 3.10 and 3.14 on each OS, 3.14t, PyPy 3.11, and `python:3.13-alpine` |
| npm | `npm install @sparkles-rdf/engine@X.Y.Z` on Node 22, 24 and 26 on each OS and on `node:24-alpine`, plus Bun and Deno as `node-packages.yml` runs them today |
| Maven Central | A Gradle project that depends on `sparkles-jena-all:X.Y.Z`, on Java 17 and 21 on each OS |
| crates.io | `cargo install sparkles-server --version X.Y.Z --locked` on Linux |

Maven Central artifacts cannot be changed or deleted. The release workflow therefore
keeps `MAVEN_PUBLISHING_TYPE` at `user_managed`, runs the JVM smoke test against the
jars staged in the workflow before the Portal deployment is released, and runs it again
against Central afterwards. A failed smoke test on PyPI or npm leads to a yanked or
deprecated version and a patch release; it never leads to replacing a published file.

## 7. Architecture

* `.github/workflows/release.yml`, triggered by a `v*` tag. Its first job checks that
  every manifest holds the tag's version (`scripts/check-versions.sh` replaces the three
  separate checks of today) and that `CHANGELOG.md` has the entry. It then calls the
  three binding workflows, which gain `on: workflow_call`, and runs the new jobs:
  `server` (matrix of §3.1), `image` (one job per architecture, then the index), `chart`,
  `crates` and `fixture`. The `checksums` job collects every release asset, writes
  `SHA256SUMS`, and runs `actions/attest-build-provenance` over the assets and the
  image digests. `release` creates the draft GitHub Release. The publish jobs keep their
  environments and repository variables. `smoke` runs last.
* The prefix tags (`py-v*`, `jvm-v*`, `node-v*`) remain for re-running one binding's
  publish of an already released version, and their version checks now require the
  version to match the workspace's.
* **Images** are built from the `Dockerfile` with BuildKit on native amd64 and arm64
  runners, pushed by digest, and joined into an index with `docker buildx imagetools
  create`. The `Dockerfile` gains `--build-arg FEATURES="embed-local backup-encryption"`
  as its default. `nix build .#image` stays as the reproducible alternative and is built
  in CI on Linux to keep it working (rejected alternative 2).
* **Versions.** One version, in the workspace `Cargo.toml`. The binding crates are
  outside the workspace, so `scripts/set-version.sh X.Y.Z` writes it into their
  manifests, `js/*/package.json`, `jvm/gradle.properties` and `Chart.yaml`, and
  `check-versions.sh` fails CI when they differ. The internal crates
  (`sparkles-core`, `sparkles-shacl` and the rest) publish at the same version.
* **Store.** `crates/sparkles-core/src/formats.rs` holds every reader and writer version
  of §4.1 in one table, which `GET /$/server`, `sparkles --version --verbose`,
  `sparkles migrate --check` and the fixture test read. `store.rs`, `check.rs` and
  `sparkles-backup` read the window from it instead of comparing with `FORMAT_VERSION`.

## 8. Phases

**Phase 1 (M).** The release workflow, single versioning, `CHANGELOG.md`, server
binaries for the current three platforms plus the two musl targets, checksums and
attestations, images on GHCR, the Helm chart as an OCI artifact, crates.io publishing,
`--version` build info, the smoke workflow for what exists, and switching on the three
publish variables for the first release.

**Phase 2 (M–L).** Windows CI for the workspace, then Windows and Intel macOS for the
server and every binding, musl JVM natives, PyPy and free-threaded wheels.

**Phase 3 (M).** Version markers on `wal.log`, `config.json` and the settings files,
`formats.rs`, `writtenBy`, readable sets for the index format, `sparkles migrate`,
restoring older backups, the fixture directory and test.

**Phase 4 (S–M).** The compatibility rules in DEVELOPMENT.md, the OpenAPI, Rust, binding
and CLI checks, deprecation headers and warnings.

Phase 3 should land before the first release so that 0.1's databases can be read by
0.2. The other phases can follow the first release.

## 9. Acceptance examples

* **A1.** Pushing `v0.1.0` with every manifest at 0.1.0 produces a draft release with
  five server archives, `SHA256SUMS`, two image tags in two architectures, and the
  binding artifacts. Pushing `v0.1.0` while `jvm/gradle.properties` says `0.1.1` fails
  in the first job and publishes nothing.
* **A2.** `gh attestation verify` succeeds for each server archive and for
  `oci://ghcr.io/kclejeune/sparkles:0.1.0`, and fails for an archive with one byte
  changed.
* **A3.** `docker run ghcr.io/kclejeune/sparkles:0.1.0` on an arm64 host pulls the arm64
  manifest, and the container reports healthy within the health check's start period.
* **A4.** The `x86_64-unknown-linux-gnu` binary runs on Rocky Linux 8 (glibc 2.28). The
  musl binary runs on Alpine with no other package installed.
* **A5.** `pip install sparkles-rdf` on Windows x64 with CPython 3.12, on PyPy 3.11, and
  on CPython 3.14t installs a wheel, not the sdist, and the suite passes. On 3.14t,
  `sys._is_gil_enabled()` is false after import.
* **A6.** On Alpine with a JDK, `sparkles-jena-all` loads its musl library and runs the
  Jena contract tests.
* **A7.** Release 0.2 opens the 0.1 fixture, answers its recorded queries, writes, and
  compacts. After the compaction, `meta.json` of the new generation has the 0.2 index
  format, and retained 0.1 generations are still readable through `?at=`.
* **A8.** Release 0.3 refuses the 0.1 fixture at start with a message naming 0.1 and
  `sparkles dump`, and serves the other datasets.
* **A9.** A release that changes the generation index in a way it cannot read refuses
  the previous release's fixture with the `sparkles migrate` message. `sparkles migrate
  --check` lists the step. `sparkles migrate` runs it, keeps the dataset id, the commit
  numbers and the commit catalog, and the database then opens.
* **A10.** A 0.1 repository backup restores under 0.2, plaintext and encrypted.
* **A11.** A pull request that removes a member from `DatasetInfo` in the OpenAPI
  description fails `openapi::tests` until the change is listed in
  `docs/api-breaks.toml`.
* **A12.** A full-text index in an older format is rebuilt in the background on open,
  `GET /$/text/{ds}` reports it as rebuilding, and `text:query` answers once it is done.

## 10. Testing

* The release workflow runs end to end on a fork or with `workflow_dispatch` and a
  `dry-run` input that skips every publish step and the GitHub Release.
* `tests/formats.rs` and its server counterpart, over every fixture (§4.6).
* Unit tests of each `sparkles migrate` step on synthesized old files, as
  `vocab/delta.rs` tests the legacy-to-framed conversion today, plus an interruption
  test that kills the process during the swap and checks that the next run finishes or
  leaves the old directory intact.
* The compatibility checks of §5.3 run on every pull request.
* The smoke workflow of §6 runs after every release and weekly against the latest
  release, so that a registry or platform change that breaks installation shows up
  between releases.

## 11. Rejected alternatives

1. **Per-binding versions.** The bindings embed the engine, so a binding version would
   have to name an engine version anyway, and a database written by one binding is read
   by the server. One version makes "which release wrote this" a single answer.
2. **Images from Nix only.** `nix build .#image` is reproducible and smaller, but the
   documented setup (`compose.yaml`, USAGE.md's Docker section) builds the `Dockerfile`,
   its OCR stage pins PDFium and ONNX Runtime by checksum, and its health check script
   honours `SPARKLES_HEALTHCHECK_PORT`. Publishing the image users already build avoids
   two images with different behaviour.
3. **A fully static glibc binary.** glibc does not support static linking with name
   service lookups well, and the musl build already gives a static option.
4. **Migrations run by the server at start.** A migration of a large database takes as
   long as a compaction, during which the dataset cannot serve, and a failure in the
   middle of a start is harder to recover from than a failure of an offline command that
   works on a copy.
5. **Dump and reload as the only path.** It loses the dataset id, the commit sequence,
   retained history, branches and snapshots. [CI](CI-commit-identity.md) made those
   durable, so an upgrade path that drops them defeats that work.
6. **Keeping every old reader forever.** Each reader is code that must be tested against
   every later change. A window of one minor release before 1.0 and one major release
   after bounds that.
7. **cosign signatures with a long-lived key.** Keyless signing through GitHub's
   attestations needs no key to store or rotate, and the npm and PyPI paths already use
   the same identity model.

## 12. Open questions

1. **Registry.** GHCR keeps images next to the repository and its attestations. Docker
   Hub would be more visible. Decided (2026-10-13): Docker Hub is the primary registry,
   and GHCR may carry the same images. The Docker Hub namespace `sparkles` belongs to
   another account, so the image name is chosen at Phase 1.
2. **crates.io names.** On 2026-10-13 the crate `sparkles` was taken by an unrelated
   project (0.1.8, last updated July 2025). Decided: ask its owner for the name before
   Phase 1, and fall back to `sparkles-rdf` as the Python and npm names do.
3. **Code signing.** Unsigned macOS binaries downloaded with a browser are quarantined by
   Gatekeeper, and unsigned Windows binaries raise SmartScreen warnings. Both need paid
   certificates. Default: document `xattr -d com.apple.quarantine` and the Windows
   prompt, and sign once the project has the certificates. Decided (2026-10-13): keep
   this default.
4. **Windows file semantics.** Whether generation switches and catalog swaps need a
   deferred delete of mapped files on Windows. Default: defer deletes to the next start,
   as the JVM loader already does for its DLL copies.
5. **The 0.x window.** One minor release may be too short if releases are frequent.
   Default: one, revisited after three releases. Decided (2026-10-13): keep this default.

## Outcome

Phases 1, 2 and 4 are not built. See [ROADMAP](../ROADMAP.md) items 58–60.

### Phase 3 (2026-10-13)

**The registry.** `crates/sparkles-core/src/formats.rs` declares every file kind of §4.1
with its class, its marker, the version this build writes and the versions it reads,
and a second table lists every settings and state file with the member that holds its
version. Thirty-five settings files carry a member. Six cannot, because their top level
is a map of user keys or an array, or because they are a cache or a transient intent,
and the registry lists them with the reason. The module documentation describes the
four kinds of change of §4.3. `GET /$/server` reports the registry in a `formats` member
for callers that see the version, and `sparkles migrate --check --format json` prints it.
The backup crate's layout, blob and key-slot constants are now defined from the
registry, and the module constants of the core files are checked against it by a test.

The registry also records the server's existing runtime-secret envelope and generated
key. The server wrapper accepts legacy plaintext as version 0 and sealed version 1, but
an encrypted version-zero envelope is refused. The generated key remains opaque format
0 and is not a JSON settings file. Envelope versions are checked before authentication
fallback, and the sealer recognizes the whole family while decoding only v1. This
registration changes neither the v1 encoding and AAD nor the dataset reader capability.

**Version markers.** A write-ahead log created by this build starts with a header
record: the magic `\x89SPKWAL\n`, the version (1) and zeros. The spec asked for a 16-byte
header. The header is instead 33 bytes, the size of every record, so the records after
it keep their alignment. Every reader that steps through records at multiples of 33
from the start of the log, and every saved position of byte 0 meaning "before the first
commit" (the history index, branch link segments, the backward replay), works unchanged
by skipping the record where the log starts. A log without the header is version 0 and
reads as before. Readers of the log header and of a generation's `meta.json` take the
version before anything else and refuse an unknown one as `Unsupported`, so a newer
release may use the header's trailing bytes or give `meta.json` another shape, and the
catalog still refuses only that dataset. Existing logs are never rewritten. The header arrives with the next
generation, at a compaction, a bulk load, a branch or a clone. Because an older release
would read the header as a damaged record and could truncate the log as a torn tail,
`dataset.json` gains reader capability 5 (`DATASET_READER`), and a store raises
`minimumReader` to 5 when it opens a database for writing, after every file it read
proved readable.

The catalog's `config.json` and the settings files gain a member, absent meaning 1, read
and written through `formats::settings_from_slice`, `settings_to_vec`,
`check_settings` and `stamp_settings`. The server's dataset settings kinds go through
one pair of helpers (`assist::read_file` and `write_file`), and its server-wide kinds
through another, so `assistant.json`, `memory.json`, `ingest.json`, `asks.json`,
`query-suggestions.json`, `maintenance.json`, `review.json`, `models.json` and
`notifications.json` are covered in two places. Files whose types already had a member
(`reasoningFormat`, `rdfsFormat`, `originFormat`, `restoreFormat`, `version`) keep it, and
the registry names it. A newer version is refused, except for the informational
`origin.json` and `restore.json`, which are then ignored with a warning. A store checks
every settings file and `dataset.json` of a dataset and each physical branch directory,
including retained branches, before it writes dataset metadata. The process lock may
record its holder, but a refused open leaves every metadata file unchanged and agrees
with `migrate --check`. Startup and planning share the read-only branch enumeration,
which surfaces directory errors and never opens or prunes a branch just to preflight it.
Recognized unpublished `.create-*` staging and `.deleting` tombstones are excluded from
that enumeration with the same classification as branch startup recovery. Planning
leaves them untouched, and startup recovers them only after real branch compatibility
checks succeed. An empty or partial unpublished dataset.json therefore does not block
recovery, while an incompatible active or retained branch still refuses before cleanup. `dataset.json` keeps the members it does not know when a release
rewrites it. Settings files that a release writes through a typed struct drop unknown
members, so an additive member of such a file must be safe to lose or needs a `format`
bump.

**`writtenBy`.** `dataset.json` records the release that last opened the database for
writing, and refusals name it.

**Readable sets.** `formats::READABLE_INDEX_FORMATS` replaces the equality test with
`FORMAT_VERSION` in `store.rs`, `check.rs` and the backup manifest's validation, which
now takes the set. A refusal says whether the database is newer than the release, can be
upgraded with `sparkles migrate`, or must be dumped with the release that wrote it.

**Refusing one dataset.** When `Catalog::open` meets a persistent dataset in a format
this release does not read (`Error::Unsupported`), it logs the reason, leaves the
dataset closed and registered, and serves the others. `GET /$/server` lists such datasets
under `refusedDatasets` to the callers that may read them, with the reason only for server
administrators. This is the part of §2.5 that roadmap item 64 needs only for
format refusals. Other open failures still stop the catalog.

**`sparkles migrate`.** `crates/sparkles-core/src/migrate.rs` holds the step registry,
`plan`, `migrate` and `recover`, and `crates/sparkles-server/src/migrate_cmd.rs` the
command of §4.4 with `--data`, `--loc`, `--dataset`, `--check`, `--keep-old` and `--format
json`. The swap is not the catalog's `swap.rs`, which needs a live catalog. The command
works on a stopped data directory, so it takes the catalog lock and the database's own
lock, copies the database to `.migrate-<name>`, runs the steps, checks the copy with
`check::check`, and swaps it in with two renames. A copy that passed the check carries a
`.migrate-checked` mark in JSON format 1. It records a strict cleanup mode and the exact
old sibling basename, so a later release recovers and cleans up the originating copy
without guessing from its own version or deleting an unrelated kept copy. Recovery
validates the version and safe sibling name before creating lock files or changing
recovery state, and rereads the marker before publication or cleanup. Unknown
versions, malformed state, duplicate known fields and bare legacy mode markers refuse
recovery while preserving every directory and byte. Legacy markers require inspection of the sibling copies and
manual recovery through a known original location or a restore. A run that stopped
between the renames is finished by the next run only with understood checked state. The command takes
a lock file `.migrate.lock-<name>` next to the database for planning, recovery, the copy
and the swap, and the database's own lock until the renames start. `plan` is read-only
and reports a pending run as `interrupted`, so `--check` changes nothing. A store refuses
to open a database whose `.migrate-<name>` exists, before it creates the directory and
again under its lock, so a stopped run never leaves an empty database. After the pending
check, startup compares file identities to verify that its acquired lock still belongs
to the current directory. It refuses an old descriptor that survived a migration swap,
including for branch roots. Migration recovery checks the locked file's identity while
allowing the pending state it must finish. A backup restore also refuses to publish over
a database with a pending migration, even when the target directory is missing, and
verifies its target lock's identity after checking pending state. Backup captures and clones leave out the migration check mark.
The command resolves a relative `--loc` to an absolute path. When the input has parent
components, it canonicalizes the existing parent before mutation and appends the final
basename unchanged. This respects physical symlink-parent semantics and keeps renames
independent of a root component that the first rename removes. An alias whose parent no
longer resolves refuses before mutation with guidance to use the stable existing parent.
Inputs without parent components still support new nested and missing roots. The
command refuses symlinks and special files within the database, and does not start while
`.pre-migrate-<version>-<name>` exists. Unit tests interrupt the run
at each point and check the result, and cover a check during a copy, a live store, the
lock file's name and a root that reappears next to a checked copy. One step is registered: a legacy delta
vocabulary to the framed format, which is the lazy upgrade of §4.3 run ahead of time. The
log header has no step, because the byte offsets of branch link segments and of backup
manifests point into existing logs, and rewriting a log would move them. A generation
index step will be a compaction into the new format when an index format change needs
one.

**Backups.** A restore accepts any index format in the readable set. The `422
incompatible-format` message names the release in `server.version` and says what to do.

**Fixtures.** `scripts/format-fixture.sh BIN LABEL` writes a data directory with a
catalog, a dataset `books` with two generations, a delta, named graphs, blank nodes, an
RDF 1.2 triple term, a named snapshot that keeps the first generation, a branch with its
own commit, a stored query, DESCRIBE and compaction settings, the full-text, spatial and
vector indexes (with literal vectors, so no embedding provider is needed), and a
plaintext and an encrypted backup repository, with the answers of twelve recorded query
cases. The first fixture, `testsuite/formats/c7a6811c/` (36 KB), was written by the
commit before this phase, so it holds headerless logs, settings without members and a
`dataset.json` without `writtenBy`. `crates/sparkles-core/tests/formats.rs` opens each
fixture's dataset, checks it, compares the quad counts at the head and at the snapshot,
writes, compacts and opens it again. `crates/sparkles-server/tests/cli_formats.rs` runs
the binary over each fixture: `migrate --check` places it inside the window, `check`
finds no error, every case answers as recorded (past states and the branch included),
the stored query runs, the catalog lists the dataset, both backups restore to the same
count, and after a write and a compaction the new log has its header and `dataset.json`
names this release. A second test raises the fixture's index format to 99 and checks
the refusals of `migrate --check`, `query` and the catalog.

A minimal Unreleased **Format** note records checked migration state in `CHANGELOG.md`.
The broader Phase 1 changelog and release work remain pending. Not built in this phase
are the fixture job of the release workflow, `sparkles --version --verbose`, and
the background rebuild of a vector or spatial index whose file version is unknown, which
already happens for the full-text index (A12).
