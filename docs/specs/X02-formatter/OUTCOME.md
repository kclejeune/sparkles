# X02: Opinionated formatter (`sparkles fmt`, `POST /$/format`, the UI's Format button) — Delivery record

Current delivery status is summarized in the [landing page](README.md).
The [design Outcome overview](SPEC.md#outcome) links the supported scope to this record.
The recorded dates and measurements below describe their original delivery stages.

## Historical opening status

> **Status:** implemented
>
> **Phases:** Phase 1 covers SPARQL, Turtle/TriG, N-Triples/N-Quads, `sparkles fmt`, the
> config file, `POST /$/format` and the UI Format button. Phase 2 adds JSON-LD, `--sort`,
> `--prune-prefixes`, `--canonicalize`, the external sort, `sparkles lsp`, the two optional
> keys `turtle-layout` and `align-values`, and Format in the shapes editor. Phase 3 adds the
> WebAssembly build that the UI uses, streaming Turtle/TriG and the cargo-fuzz targets
> (`mise run fmt:fuzz`).
>
> **User docs:** [API: Formatting](../../API.md#formatting) · [Editor integration](../../editors.md) · [Features](../../FEATURES.md#formatter-and-editor-support)
>
> This is the design as written before implementation; the [Outcome](SPEC.md#outcome) section at the end
> records how it landed.

## Outcome

**Phases delivered** (2026-09-30 to 2026-10-01). SPARQL landed first, end to end: the
`sparkles-fmt` crate, `sparkles fmt`, `POST /$/format`, and the UI Format button with
Shift+Alt+F, one-step undo and format on run. The next round added JSON-LD, Turtle/TriG
and N-Triples/N-Quads. The line formats came with streaming, `--sort` with LZ4-framed
spill runs, and `--canonicalize` with RDFC-1.0. That round also added `sparkles lsp` with
[editor snippets](../../editors.md), `align-values`, `prune-prefixes`, Turtle/TriG sorting
and `turtle-layout = "conventional"`, so neither optional key was dropped. The
WebAssembly build (`crates/sparkles-fmt-wasm`) and streaming Turtle/TriG came next. The
Phase 3 fuzzing targets of §9.7 landed on 2026-10-01 in `crates/sparkles-fmt/fuzz`, a
cargo workspace of its own that `mise run fmt:fuzz` runs and `mise run ci` never builds.
There is one target each for SPARQL, Turtle and TriG, N-Triples and N-Quads, and JSON-LD.
Each checks that the formatter never panics, that a refusal is a positioned syntax error,
and that the output means what the input means, keeps its comments and formats to
itself. The first campaign found stack overflows on deep nesting, which led to the
256-level limit, a panic on an undeclared prefix, three outputs that were not
fixpoints, an unterminated long string that did not lex by the longest match, and a
cursor mapped inside a character. All of them were fixed, and each is a test in
`tests/fuzz_regressions.rs`.

**Decided by the maintainer:** the §13.1 style defaults and the twelve configuration keys
were reviewed item by item before Phase 1. The WebAssembly module is optional. A
separate `mise run ui:wasm` task builds it, and the UI uses it when present and falls
back to `POST /$/format` otherwise. CI only checks the `wasm32-unknown-unknown` build,
which is now listed in `rust-toolchain.toml`. The Nix UI package includes the module by
default.

**Chosen during implementation** (open to revision): with sort off, the CLI streams
Turtle/TriG of any size, and `--max-bytes` (256 MiB) then bounds the largest statement.
The "convert to N-Triples" error remains for SPARQL, JSON-LD and sorted Turtle/TriG.
`sparkles lsp` reads only the nearest `.sparklesfmt.toml`. In Turtle and TriG,
`prune-prefixes` drops an unused declaration even when a later directive redefines its
label, because its scope ends at the redefinition. An unused redefinition stays only when
the last declaration of its label that the output keeps binds a different namespace.
Dropping it would extend that binding over the redefinition's scope, and the next run
would compact IRIs there with it. Streamed and in-memory formatting follow the same rule.
When pruning leaves a directive block with only the section comments of its dropped
declarations, those comments are spaced as a comment block before the next statement,
which is how the next run reads them. A blank line separates them from a multi-line
statement on either side. Before this rule the output was not a fixpoint, and a stream
reported `unstable-format` there instead of a syntax error later in the input.
The opt-in task `fmt:jsonld-compare`, outside `ci`, compares the JSON-LD layout with
`oxfmt`, which follows Prettier's JSON conventions and is already pinned in `ui/`. The
SHACL shapes field became a CodeMirror editor so that its Format keeps one-step undo.

**Other deviations:** the body limit flag is `--format-max-mb` (default 16), like the
server's other size flags. An ignore pragma right before the first node leads that node
instead of joining the file header (§3.3). JSON-LD nested deeper than 256 levels is
refused. The UI formats in the browser when the module is built, so
`--format-endpoint off` disables only the endpoint. The spargebra associativity bug
(§13.2 Q11) was fixed in a patched copy under `vendor/spargebra`.

**Tests at landing:** all 1,102 W3C SPARQL queries and updates format, pass the checks
and are fixpoints. The comment-injection sweeps are clean: a comment after every token
under both operator positions, and 300k property cases. The reference parsers agree with
all 1,132 W3C Turtle, TriG, N-Triples and N-Quads cases. The Turtle/TriG suites and the
SHACL test files format under the defaults and with every key flipped. N-Triples/N-Quads
output matches the canonical-form tests byte for byte, and streamed Turtle/TriG equals
the in-memory output. JSON-LD has no local W3C suite. Jena's and Oxigraph's JSON-LD files
and the W3C RDF results written as JSON-LD stand in for one, plus
`SPARKLES_JSONLD_TESTS` when set.

**Performance:** the §8 targets are not recorded in [BENCHMARKS](../../BENCHMARKS.md).
The WebAssembly module is about 250 KB with brotli, well under the 1.5 MB budget of §7.4.

**MCP tool.** The `format` tool of §13.2 Q10 landed on 2026-10-02 in `sparkles mcp`
([C11](../C11-mcp-server.md)). It formats the six languages with `sparkles_fmt::format`, the
engine of `sparkles fmt`. It takes the camelCase style options of `POST /$/format` and
shares that endpoint's parsing of them. It returns the language, the text, whether the
text changed and the warnings. The text may be up to 1 MiB, and `--mcp-max-bytes` caps
the result. A call runs within its `timeoutSeconds`. It reads no dataset and no config
file.

**Later:** `sparkles lint` landed later as its own spec,
[X04](../X04-linter.md), on the formatter's syntax tree. It adds a `[lint]` table to the
config file, which the formatter ignores, lint diagnostics and quick fixes to
`sparkles lsp`, and a `lint` function to the WebAssembly module.

### Roadmap follow-ups (2026-10-12)

**SHACLC formatting** (item 48, Tier 5, [ROADMAP](../../ROADMAP.md)). The linter's half
of item 48 is in [X04](../X04-linter.md#roadmap-follow-ups-2026-10-12).

* **What is missing today.** `sparkles_fmt::Language` has six languages: SPARQL,
  Turtle, TriG, N-Triples, N-Quads and JSON-LD (`crates/sparkles-fmt/src/lib.rs`).
  SHACLC has a reader and a writer in `sparkles-shacl/src/compact` ([G03](../G03-shaclc.md)),
  but the writer works from a shapes graph: it drops comments and the author's order,
  so it is a conversion, not a formatter. The UI's shapes editor accepts SHACLC and its
  Format button cannot format it.
* **Design.** A seventh language, `Language::Shaclc`, with a lossless syntax tree in
  `crates/sparkles-fmt/src/shaclc/` built like the Turtle one: the lexer of
  `sparkles-shacl/src/compact/lex.rs` extended to keep trivia, a tree that holds every
  token, and a printer over the shared document model of §4. Layout: directives first
  as written, one blank line between shapes, a shape's header on one line, one property
  per line indented by `indent`, and a property's constraints on its line until
  `line-width`, then one per line. Node constraints, paths and arrays break by the same
  rules as Turtle's lists. `prune-prefixes` and `align-values` apply; the sorting keys
  do not.
* **Checks.** The same as for the other languages: the output parses with
  `sparkles_shacl::compact::parse`, produces an isomorphic shapes graph, keeps every
  comment, and formats to itself. The corpus is G03's test files and the SHACLC
  documents of the W3C draft's examples.
* **Surfaces.** `sparkles fmt` by the `.shaclc` extension and `--language shaclc`,
  `POST /$/format` with `language: "shaclc"`, the MCP `format` tool, `sparkles lsp`,
  the WebAssembly module and the shapes editor, which picks the language from the
  editor's syntax switch.
