# X13: Per-crate Nix builds

This investigation is [roadmap item 92](../ROADMAP.md). It is deferred. The current
packaging continues to use Crane while the comparison remains incomplete.

## Problem

Crane caches third-party dependencies separately from workspace code. A workspace
source change still invalidates the derivation that compiles the workspace crates.
Per-crate derivations could preserve builds of unaffected workspace crates, reducing
the work after edits to a leaf crate. Changes to `sparkles-core` would still rebuild
its dependents.

The existing build layers, UI wrapper and formatter source filtering are described
in [DEVELOPMENT.md](../DEVELOPMENT.md#how-the-rust-build-is-layered). The ordinary
Cargo GitHub Actions jobs use Cargo directly, so changing the Nix packaging would
not by itself speed up those jobs.

## Existing prototype

A crate2nix prototype built the Linux server successfully on 2026-10-19. It needed
overrides for the native tools used by `aws-lc-sys`, the `CARGO_CRATE_NAME` environment
variable used by `rmcp`, and the directory layout expected by the server's UI build
script. Its generated `Cargo.nix` contained about 27,000 lines.

Preliminary runs covered a no-op build and edits to the client, SHACL and core crates.
They do not establish a speed advantage because a matched Crane comparison and
complete packaging validation have not been completed. The prototype is evidence
of feasibility, rather than a decision to replace Crane.

## Investigation

Compare crate2nix with the current Crane build using the same source revision,
locked dependencies, Rust toolchain, features, optimization settings, linker and
hardware. Record the available binary substitutes and local caches for each run.
Measure a cold build, a no-op build, and source edits to the server, a leaf crate,
SHACL, the core and the formatter. Repeat runs that vary enough to change the
recommendation.

For each case, record wall time, peak memory, evaluation time, cache transfer costs
and the derivations actually rebuilt. Trace dependency reuse rather than inferring
it from elapsed time. Include a lockfile update to measure the cost of regenerating
the build description. Evaluate cargo2nix or layered Crane builds if the prototype
exposes a limitation that those approaches could address.

Verify feature unification, build scripts, proc macros, the vendored spargebra and
the repository's per-package profiles. Check the server and CLI, their shared binary
and UI wrapper, the WASM formatter, Python, JVM and Node bindings, and the existing
Nix checks. Cover Linux x86_64 and arm64 and macOS arm64, and preserve the flake's
existing platform support. State the maintenance cost of generated files, overrides
and any import from derivation.

## Completion

Publish the matched measurements and the remaining limitations, then recommend
retaining Crane, migrating to per-crate builds, or making a smaller change. Any
migration must preserve package interfaces and pass the existing build and test
gates. No packaging migration is included in the CI streamline merge.

## Outcome

The CI streamline work landed separately. The per-crate comparison remains future
work, and no controlled result establishes that Crane or crate2nix is faster here.
