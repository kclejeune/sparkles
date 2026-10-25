# Shared design and dependency provenance

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

These records span several features or have no single spec owner.
The [provenance index](../PROVENANCE.md) links the per-spec records.

## Allocator (server and CLI binaries)

- **Adopted:** `mimalloc` 0.1.52 (MIT), with `libmimalloc-sys` 0.1.49 (MIT). The
  `extended` feature of `libmimalloc-sys` provides `mi_collect`. The crate bundles
  Microsoft mimalloc 3.3.2 (MIT). The allocator is a default-on feature of
  `sparkles-server`. The `sparkles` library stays allocator-neutral.
- **Design source:** our own measurements of allocator memory retention; the mimalloc
  option documentation and source (`src/options.c`: `purge_delay`, `purge_decommits`);
  and glibc `mallopt(3)` and `malloc_trim(3)`.
- **Rejected:**
  - glibc `mallopt` fixed thresholds: slower throughput.
  - `tikv-jemallocator` 0.7 (MIT/Apache-2.0): slower, and needs `make`.
  - mimalloc `PURGE_DELAY=0`: slower throughput.

## TriX

- **Design source:** Apache Jena's `ReaderTriX`, `WriterTriX`, `StreamWriterTriX` and
  `TriX` classes and their tests `TestTriXReader`, `TestTriXBad` and `TestTriXWriter`
  (Apache-2.0), read for behaviour: the element names of the HPL report and the W3C DTD,
  the unnamed graph as the default graph, plain and typed literals, `rdf:XMLLiteral`
  content, `<qname>`, triple terms, and the writer's layout. The format itself is
  Carroll and Stickler's "TriX: RDF Triples in XML" (HP Labs, HPL-2004-56). No code was
  copied.
- **Test files:** Jena's `jena-arq/testing/RIOT/Lang/TriX` directory is copied unchanged
  into `testsuite/trix/jena`, with Jena's `LICENSE` (as `LICENSE-APACHE`) and `NOTICE`, as
  the Apache License 2.0 allows. The unit tests of `sparkles::trix` run Jena's cases
  against them.
- **Implementation** (2026-10-02): `sparkles::trix` in the core crate, used by the
  server's Jena formats, `sparkles load` and `sparkles convert`, and SPARQL Update's
  `LOAD`. **Dependencies:** `quick-xml` 0.37 (MIT), already in the tree through
  `oxrdfxml` and `sparesults`, became a direct dependency of `sparkles`.

## Patched spargebra (vendored)

- **Adopted:** `spargebra` 0.4.7 from crates.io (2026-09-30). Its `Cargo.toml` gives the
  license as MIT OR Apache-2.0, and the copyright belongs to the Oxigraph developers. The
  crate is copied to `vendor/spargebra` and used through `[patch.crates-io]` in the
  workspace `Cargo.toml`.
- **Changes** (`vendor/spargebra/PATCHED.md`): written against 0.4.7's rust-peg grammar
  from the W3C SPARQL 1.1 and 1.2 Query grammars and the W3C test suite. None were
  ported from upstream.
  - left-associative `+ -` and `* /`;
  - case-insensitive `true` / `false`;
  - longest match for `<` against `IRIREF`;
  - the FILTER scope in OPTIONAL;
  - SELECT expressions see earlier aliases;
  - no nested aggregates;
  - triple-term subjects restricted per the SPARQL 1.2 grammar rules [123] and [138];
  - Jena ARQ's syntax extensions behind one switch, `with_arq_syntax`: the aggregate
    keywords, `LATERAL` (spargebra's own `sep-0006` code), path ranges and CONSTRUCT
    templates with `GRAPH`, written from ARQ's grammar for spec G06.
- **Notes:** like the published crate, the copy has no `LICENSE` files. The license is
  the `Cargo.toml` field. The copy will be dropped once a spargebra release has these
  fixes.
- **Rejected:** waiting for an upstream release, and porting the rewritten parser of
  Oxigraph's development version.

## Evidence, durable work and maintained analysis

The new specifications are [C25](../C25-evidence-and-assessments.md),
[C26](../C26-durable-jobs-and-delivery.md), [C27](../C27-valid-time-profiles.md),
[F22](../F22-analysis-lifecycle.md) and [C22b](../C22b-report-configurations.md). They are
specified and unimplemented. No code or runtime dependency was adopted.

The designs use the W3C RDF 1.2 Concepts, PROV-O and OWL-Time documents, HTTP semantics
in RFC 9110, CloudEvents 1.0.2 and the Standard Webhooks specification. They were
checked against Sparkles' existing evidence writes, commit identity, format registry,
tasks, notifications, incremental views, inference protections and GraphRAG ledger.
The individual specifications link their primary sources. No domain-specific threat
intelligence feed or application ontology is required by the shared profiles.

C25 adopts separate source occurrences and unasserted claims, qualified evidence links
and immutable decisions. It rejects implicit truth from review and automatic identity
merges based on similarity. C26 adopts the existing dataset write transaction as the
authority for durable publication, checkpoints and delivery intent. It rejects a second
independent queue transaction and a promise of exactly-once external delivery.

C27 adopts explicit valid time and recorded availability, with uncertainty preserved.
It rejects invented timestamp precision and retrospective availability in backtests.
F22 adopts conservative read footprints and bounded recomputation before incremental
maintenance. It rejects citation-only invalidation and the treatment of all withdrawn
support as a false conclusion. C22b adopts stable configuration identities and shared
accounting. It rejects first-readable report selection and per-configuration budgets
that can multiply a dataset allowance.

## Development tools (not linked into Sparkles)

- **Adopted** (2026-09-30, with the pre-commit hooks in `.pre-commit-config.yaml`):
  - `prek` 0.5.4 (MIT), the hook runner, pinned in `mise.toml`;
  - `oxfmt` 0.71.0 (MIT), the UI formatter, a devDependency in `ui/package.json`
    (`ui/pnpm-lock.yaml`), with its `@oxfmt/binding-*` 0.71.0 platform packages (MIT) and
    `tinypool` 2.2.0 (MIT);
  - `shfmt` 3.14.1 (BSD-3-Clause, mvdan/sh), pinned in `mise.toml`;
  - `shellcheck` 0.11.0 (GPL-3.0), pinned in `mise.toml`. It is a development tool only.
    It runs on the shell scripts and is never linked, bundled or shipped;
  - `nixfmt` (MPL-2.0), the flake's formatter, pinned by `flake.lock`.
- **Design source:** the tools' own documentation.
- **Replaced:** Prettier and `prettier-plugin-svelte` (MIT), removed from the UI.

