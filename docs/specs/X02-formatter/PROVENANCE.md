# Formatter

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

See the [spec landing page](README.md), [design](SPEC.md) and
[delivery record](OUTCOME.md) for their respective contracts.

- **Spec:** [`X02-formatter.md`](README.md), written on 2026-09-30 independently
  as internal engineering. Its sources are:
  - the W3C and IETF specifications: SPARQL 1.1 and 1.2 Query, SPARQL 1.1 Update and
    Results JSON, RDF 1.1 and 1.2 Turtle, TriG, N-Triples, N-Quads and Concepts,
    RDFC-1.0, JSON-LD 1.1, RFC 8259 and BCP 47;
  - the pretty-printing papers of Wadler, Lindig and Oppen, and the Prettier docs (MIT);
  - the Oxc/oxfmt docs, `Cargo.toml` files and npm and crates.io metadata (MIT);
  - prior art through public documentation and licenses only: Apache Jena, rdflib,
    turtlefmt, atextor/turtle-formatter, sparqling/sparql-formatter, Qlue-ls, TopBraid's
    published Turtle files, and the tree-sitter SPARQL and Turtle grammars (checked for
    RDF 1.2 constructs only);
  - the Sparkles code, spargebra 0.4.7, oxttl 0.2.4 and oxrdf 0.3.4.

  No code was read or copied from any GPL or LGPL project.
- **Implementation, slice 1** (2026-09-30): SPARQL queries and updates, `sparkles fmt`,
  `POST /$/format` and the UI's Format button. It was built from the spec, the slice plan
  and the Sparkles code only.
  - **Design sources:**
    - Philip Wadler, "A prettier printer" (1998/2003), for the document algebra (text,
      line, group, nest) and the fits-then-break rule;
    - Prettier's printer algorithm as its public docs and `commands.md` describe it (MIT):
      `group`, `indent`, `line`/`softline`/`hardline`, `ifBreak` with a group id,
      `lineSuffix`, `breakParent`, `cursorOffset`, `--check`/`--list-different`/`--write`
      and their exit codes, ignore files and `prettier-ignore`;
    - rust-analyzer's syntax-tree approach, from its public architecture and design docs
      (MIT OR Apache-2.0). It uses a lossless token stream and a hand-written
      recursive-descent parser with markers (`start`/`complete`/`precede`), and keeps
      trivia by token adjacency;
    - the W3C SPARQL 1.1 and 1.2 grammars (EBNF, terminals, keyword spellings), with
      spargebra 0.4.7 (MIT OR Apache-2.0, already a dependency) as the reference parser and
      the source of the algebra the safety check compares.
  - **New dependencies** (all permissive; `Cargo.lock` and `THIRD_PARTY_LICENSES.md`
    regenerated):
    - `unicode-width` 0.2.2 (MIT OR Apache-2.0): display widths in the printer
      (`sparkles-fmt`);
    - `ignore` 0.4.33 (Unlicense OR MIT): directory walks with gitignore rules and
      `.sparklesfmtignore` (server, feature `fmt`);
    - `similar` 2.7.0 (Apache-2.0): `--diff` unified diffs (server, feature `fmt`). The
      spec named version 3, but the lock had 2;
    - `proptest` 1.11.0 (MIT OR Apache-2.0): property tests, dev only (`sparkles-fmt`);
    - `toml` 1.1.6 (MIT OR Apache-2.0, already an optional dependency for auth and backup):
      `.sparklesfmt.toml`, now also enabled by feature `fmt`;
    - reused: `thiserror` 2.0.21 (MIT OR Apache-2.0), `rayon` and `tempfile` (parallel
      files and atomic `--write`), `sha2` (the input hash in refusal logs).
  - **Test-only switch:** the `fault-injection` feature of `sparkles-fmt`. Only
    dev-dependencies enable it, in its own tests and in `sparkles-server`'s. With
    `SPARKLES_FMT_FAULT=drop-token`, the printer drops a token, so the tests can show
    that the CLI and the endpoint refuse such output.
- **Later slices** (2026-10-01): Turtle, TriG, N-Triples, N-Quads and JSON-LD,
  `sparkles lsp`, the optional WebAssembly build for the UI, and streaming Turtle and
  TriG. They were built from the spec, the plan for the remaining slices and the
  Sparkles code only.
  - **New dependencies** (all permissive; versions from `Cargo.lock`):
    - `json-event-parser` 0.2.3 (MIT OR Apache-2.0, from the Oxigraph developers): the JSON
      tokenizer of the JSON-LD formatter (`sparkles-fmt`);
    - `lsp-server` 0.10.0 (MIT OR Apache-2.0) and `lsp-types` 0.97.0 (MIT): `sparkles lsp`
      (server, feature `fmt`);
    - `wasm-bindgen` 0.2.129 (MIT OR Apache-2.0) and its macro and support crates: the
      `sparkles-fmt-wasm` crate, built only by its own task, never linked into the server.
- **Rejected** (spec §12):
  - re-serializing through spargebra `Display` or the `oxttl` serializers;
  - tree-sitter grammars;
  - embedding oxfmt, or shelling out to oxfmt or Prettier, for JSON-LD;
  - the `pretty` crate;
  - aligning predicate columns, prefix IRIs or `VALUES` columns;
  - many style options;
  - formatting RDF/XML;
  - formatting SPARQL inside SHACL literals;
  - canonicalizing literal lexical forms.
