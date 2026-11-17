# Resume: lane ref-eval-512 (test-only reference evaluator)

Checkpointed because the session was not on Opus. Read brief.md first. A fresh Opus agent at high effort continues.

## State at checkpoint
Branch lane/ref-eval-512, one WIP commit on top of origin/main (7e272f4d). The last edit round was NOT compiled
(the commit before it compiled and ran). First step: `mbx test -p sparkles-core --test w3c --no-run` and fix errors.
Use SPARKLES_W3C_DIR=/home/kclejeune/repositories/github.com/apache/jena/jena-arq/testing/rdf-tests-cg/sparql
(the default path resolves wrongly from this worktree and the suites would skip silently). The test binary
runs in under a second, so run it directly from target/debug/deps/w3c-* when heavy-run slots are busy.

## Design (done)
- tests/support/reference/{mod.rs,expr.rs,path.rs,value.rs}: engine-free evaluator (oxrdf, spargebra, oxsdatatypes,
  regex, md-5/sha1/sha2, uuid, oxiri only). `Reference::new(quads).run(text, base) -> Answer{Select,Ask,Graph}`
  or Error::{Parse,Unsupported}. Nested-loop algebra, hidden vars `:bnode:label` for pattern bnodes, `:row` for
  BNODE(label) scoping, env solution for LATERAL/EXISTS substitution (leaf patterns substitute env, expressions
  fall back to env). `value::spec_order` returns None for pairs ORDER BY leaves unordered; `total_order` extends it.
  Unsupported: SERVICE, DESCRIBE, graph templates, Jena algebra, custom functions/aggregates, other xsd casts.
- tests/w3c.rs: `#[path]` mod reference, Mode.reference, reference_answer (parses data files itself, renames bnodes
  per file), engine_answer split out, tests reference_sparql10_and_11 and reference_sparql12, known file
  tests/w3c-reference-known.txt (currently only the header).
- First run: 492/508 passed on 1.0/1.1 and 58/63 on 1.2. The 21 failures were analysed and fixed in the uncompiled edit:
  `=` value spaces (open-world: lang-tagged vs other literal is false, simple vs unknown-typed is an error, date support),
  bnode renaming per data file (graph/dataset tests), BNODE per-solution scope, triple terms sort after literals and
  by component, ill-formed boolean has no EBV (not-not), hasLANG/hasLANGDIR of a non-literal is false,
  GROUP_CONCAT returns a plain string.

## Next steps
1. Compile, rerun both reference_ tests to zero unexpected failures. List anything genuinely unsupported or a real
   engine/spec disagreement in tests/w3c-reference-known.txt with reason (engine disagreements need a roadmap
   row number from the coordinator). Check the "skipped" counts do not hide evaluation tests; also confirm engine
   w3c tests (sparql10, sparql11_query, sparql12) still pass after the refactor of run_query_test.
2. Differential tests: in tests/query_cursor.rs add the reference as oracle for the seeded `differential`
   (dataset from snapshot quads via snapshot.for_each_quad/quad_to_terms; for the masked view derive visible quads
   from the protection rule or state the limitation). Compare reference vs eager and vs cursor at batch sizes
   1,2,3,4096. Use a tolerant comparator for spec nondeterminism (ties under LIMIT/OFFSET, ORDER BY across kinds
   `value::spec_order` leaves unordered, SAMPLE, GROUP_CONCAT order): validate the engine order as a permutation of
   the reference bag violating no pair spec_order defines. Do not tune tie-breaks to match the engine.
3. Move `same_answer`/`same_rows` in src/sparql/opt_tests.rs (and the NONE sites listed by
   `grep -rn Optimizations::NONE crates`) to the reference where the query is plain SPARQL over plain data
   (include via `#[cfg(test)] #[path = "../../tests/support/reference/mod.rs"] mod reference;`), compare ALL and
   NONE both to the reference; sites needing access views, text, geo, RDFS or extensions keep their oracle and go in
   the report. Time the larger fixtures before converting.
4. Triage every disagreement: reference wrong -> fix; engine wrong vs spec/W3C -> ask the coordinator for a row,
   park in a listed file; spec undefined -> relax comparator citing the section.
5. Docs: X14 SPEC.md toggles paragraph (line ~393) and Outcome, PLAN.md lines 561 and 684, ROADMAP row 512 (move to
   Completed), docs/DEVELOPMENT.md test section, CHANGELOG, select-checks.py w3c lists (add new files).
   Plain prose per AGENTS.md. Gate: reference agrees with the engine on every W3C query test and on the
   seeded queries of query_cursor.rs at both modes.
6. `mbx fmt`, `scripts/ci.sh --fast --changed origin/main`, Sol review in hc (separate Codex session) to CLEAN,
   advisor consult before reporting. Lane CI on the Boat pool first.

## Open decisions
- Which NONE/eager oracle sites move (classify per advisor advice: plain SPARQL moves, others stay).
- Whether the W3C comparison helper term_eq (uses engine Value) should be replaced by an engine-free one for
  reference mode.

## Consults so far
- Advisor (before substantive work): followed. Key traps: vacuous W3C pass via wrong dir, spec nondeterminism,
  masked-view circularity, nested-loop cost, docs list.

## Live processes
None started by this lane remain running (the heavy-run test jobs finished).
