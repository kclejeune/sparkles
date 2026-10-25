# ShEx validation

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

See the [spec landing page](README.md), [design](SPEC.md) and
[delivery record](OUTCOME.md) for their respective contracts.

- **Spec:** [`G02-shex.md`](README.md), written on 2026-09-30 independently from:
  - the Sparkles code and spec C10;
  - the ShEx 2.1 Final Community Group Report and the ShapeMap draft from shex.io, both
    fetched. Both are under the W3C Software and Document License, and the shape-map
    repository is MIT;
  - the shexTest suite (W3C Software and Document License), upstream and as vendored in
    Apache Jena;
  - Apache Jena's `jena-shex`, `jena-cmds` and `jena-fuseki2` sources (Apache-2.0), read
    for behaviour and CLI surface. No code was copied;
  - rudof 0.3.24 (MIT OR Apache-2.0), inspected for licensing, dependency weight, API
    and conformance. It is not a dependency;
  - papers by Staworko et al. (ICDT 2015), Boneva, Labra Gayo and Prud'hommeaux (ISWC
    2017) and Labra Gayo et al. (2015, RBE derivatives).
- **Implementation, Phase 1** (2026-10-01): from the spec, the plan and the Sparkles
  code.
  - The conformance harness (`crates/sparkles-shex/tests/shextest.rs`) reads the
    shexTest manifests itself, with oxttl for Turtle and serde_json for JSON-LD. Jena's
    `ShexTests.java` and `ShexValidationTest.java` (Apache-2.0) were read to learn how
    the suite is run: base IRIs, the start-focus and map forms, extension results and
    its exclusions. No code was copied.
  - The server endpoint (`http/shex.rs`) and `sparkles shex` (`shex_cmd.rs`) follow the
    `/{ds}/shacl` handler and `sparkles shacl` in the Sparkles code.
  - The ShExC lexer and parser (`shexc/`) are hand-written from the ShEx 2.1 grammar,
    without a parser generator. The suite's ShExJ files were the reference for the
    parsed form.
  - The ShExJ reader (`shexj.rs`) follows the ShExJ section of the ShEx 2.1 report.
  - Schema checks and compilation follow the report's well-formedness and stratification
    rules (Boneva et al. 2017 for stratified negation).
  - The matcher decides a neighbourhood partition by iteration-count intervals per
    group, not by RBE derivatives. The brute-force partition enumerator in its tests
    follows the report's partition definition literally. Typing is a per-stratum
    greatest fixed point (Staworko et al. 2015; Boneva et al. 2017).
  - Node constraints reuse `sparkles::xsd` for lexical forms and value comparison.
    Facet semantics follow the report and its errata.
  - Shape maps follow the ShapeMap draft. Jena's compact-map extensions (BASE/PREFIX,
    optional commas, trailing `.`) were learned from reading `jena-shex`. No code was
    copied.
  - Imports and EXTERNAL resolution follow the report. http(s) imports go through
    `sparkles::outbound`.
  - **Dependencies:** none new. `sparkles-shex` uses workspace crates only, and
    `smallvec` and `oxilangtag` were already in the lock.
  - **Test data:** shexTest is read from the Jena checkout or `SPARKLES_SHEX_TESTS`,
    not vendored.
  - **Development-only:** the rudof CLI as a differential oracle (`bench:shex
    --compare-rudof`), never linked.
- **Rejected** (spec §12):
  - depending on rudof, or vendoring its parser crate;
  - porting Jena's partition-enumeration validator;
  - goal-directed recursive validation with hypothesis stacks;
  - translating ShEx to SHACL or SPARQL;
  - RBE derivatives as the matcher. The spec preferred them, but the implementation
    adopted the interval method because it is exact and linear, and an oracle checks
    it;
  - ShEx 2.2 in Phase 1;
  - executing semantic actions other than the Test extension;
  - an RDF report format;
  - ShExC literals in named graphs;
  - SHACL and ShEx guards together in Phase 2;
  - a `peg` grammar;
  - treating budget overruns as nonconformant.
