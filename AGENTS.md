# Agent guidance

This file tells coding agents (Claude Code, Codex and others) how work is done in this
repository. `CLAUDE.md` imports it. Human contributors can read it as the house rules.

Sparkles is a Rust RDF store compatible with Apache Jena and Fuseki, with an index in the
style of QLever. [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) covers the layout, the build,
the mise tasks and the test suites. [docs/ROADMAP.md](docs/ROADMAP.md) sets the order of
work, and each item links to its design spec in [docs/specs](docs/specs/README.md).

## Building and testing

- `scripts/ci.sh` is the full local gate. The conformance suites run separately with
  `mise run test:w3c`, `test:shacl` and `test:shex`.
- Build and test each worktree in its own target directory. A shared `CARGO_TARGET_DIR`
  lets one build replace another's `target/debug/sparkles` in the middle of a test run.
- Prefer targeted runs (`cargo test -p <crate> --test <name>`) while iterating, and run the
  full gate before reporting work as done.
- When the local machine is busy, build and test on another host (atlas or forge), or on an
  ephemeral Namespace instance created with `nsc create --bare --duration …` and always
  destroyed afterwards. Before a benchmark runs on forge or atlas, move builds off that host
  so the engine under test runs alone.

## Engineering rules

- Prefer streaming, non-materialized execution. Add eager materialization only when a
  measurement shows it is faster.
- Higher memory use is acceptable when it buys speed, but state the tradeoff in the docs
  and in the commit or report.
- Every on-disk format change needs a version and a clean refusal of versions the reader
  does not know.
- Features inspired by Fluree are built clean-room. Work from the W3C standards and
  Fluree's public documentation only, and never read Fluree's source code.
- Performance claims need measurements. `mise run bench:nsc A B` gives a quick A/B on
  Namespace. Effects under about 5%, queries under 0.3 ms and codegen-sensitive changes
  need an Intel host, and published numbers come from forge. See
  [docs/BENCHMARKING.md](docs/BENCHMARKING.md).
- A measured slowdown that a gate accepts still gets a follow-up item in
  [docs/ROADMAP.md](docs/ROADMAP.md), added in the same change that accepts it. This holds
  for a loosened gate, a result judged to be noise and a tradeoff taken on purpose.

## Commits and documentation

- Commit in logical chunks that each compile and pass their tests. Do not add attribution
  lines to commit messages.
- When a roadmap item or spec phase ships, update its spec's Outcome section,
  [docs/FEATURES.md](docs/FEATURES.md), [docs/BENCHMARKS.md](docs/BENCHMARKS.md) when there
  are numbers, and the item's row in [docs/ROADMAP.md](docs/ROADMAP.md), all in the same
  change. A new known gap in FEATURES gets a roadmap item in the same commit.
- Write documentation as plain technical prose in full sentences. Avoid fragment openers,
  label-colon openers, chains of colons, parentheses or semicolons, "(yet)", marketing words
  and filler. Keep table cells to one to three short sentences.
  [docs/COMPARISON.md](docs/COMPARISON.md) is the reference voice.
- `docs/plans/` holds private working notes. Never link to it from tracked files.

## Multi-agent workflow

Large work is split into lanes, one agent per roadmap item or spec phase, each in its own
git worktree and port range. An orchestrating session starts the lanes, merges them and
keeps the handoff notes current.

### Models

| Role | Model |
|---|---|
| Orchestration, validating review findings, remediation plans, consults | Claude Opus 5.5 |
| Implementing engineering work, including remediation plans | Claude Opus 5.5 |
| Implementing clear-cut work that needs no judgment | Claude Sonnet 5.5 at high effort |
| Code review | Codex with gpt-6.1-sol at high effort |
| Mechanical tasks with a deterministic check | Claude Haiku 5.5 |

Opus implements by default. The Opus agent that validates a review and writes its
`plan.md` goes on to implement that plan itself, because it already holds the code and the
reasoning in its context and its prompt cache, and this holds for large plans as well as
small ones. Opus also starts every lane whose work needs engineering judgment, such as a
refactor whose shape is unclear, a bug whose cause is unknown, a spec that leaves behaviour
or an API open, and any lane whose core is crash durability or WAL ordering, concurrency
beyond a parallel iterator, an on-disk format, planner or executor cost decisions, or
`unsafe` code.

A lane goes to Sonnet only when the work is clear-cut and needs no judgment at all. Examples
are a loader or benchmark harness for a documented format, a conformance phase whose
behaviour the test suite fixes, and a client binding that mirrors an existing API. When in
doubt, use Opus.

Haiku takes tasks that need a model to read or write language but little judgment, and
that end in a check a script can run. Examples are triaging test and benchmark logs into
failures with their first error, edits across many files under an exact rule (renumbering
roadmap items, renaming a flag in the docs, adding a field to every fixture), fixtures
generated from a documented format, and sweeps that list problems without fixing them. Each
Haiku task states the rule, a fixed output format such as JSON rows of file, change and check
result, and the check (`cargo check`, a test, a grep or a validator) before it starts. A task
whose check fails twice moves to Sonnet. Haiku does not rewrite documentation prose, touch
engine, durability, concurrency, planner or performance code, or review and validate
findings. If a plain script can do the job, use the script.

A Sonnet lane consults Opus by starting a read-only agent on Opus with its question, the
relevant files and what it has tried, then applies the advice itself. It must consult when
the same failure survives two different fixes, when the spec leaves a decision about
behaviour, an API or a format open, when the work turns out to touch one of the Opus areas
above, when a performance gate misses for no clear reason, and before it stops a phase early
or declares a gate unmeetable. Its final report lists each consult.

### Review before landing

No lane lands without review:

1. **Codex reviews.** `scripts/codex-review.sh NAME TIP BASE "CONTEXT"` runs
   `codex exec review` on `BASE..TIP` in a scratch worktree and writes `review.md` under
   `$AUDIT_DIR/NAME` (default `target/audits`). CONTEXT names the roadmap item, the specs
   and what the work was meant to do.
2. **Opus validates and plans.** An Opus agent checks every finding independently and
   reproduces it where it can. It marks each one confirmed, refuted or plausible, decides
   against the specs whether to fix it now, fix it later under a named roadmap item, or
   document it as a limitation, and writes `plan.md` beside the review. For every fix-now
   item the plan gives the files and functions to change, the behaviour to implement, the
   edge cases, the regression test that must fail before the fix, the docs to update and the
   commands that verify it. It also records serious problems the review missed.
3. **Opus implements.** The validating Opus agent applies its own `plan.md` and reports
   what it did for each item.
4. **Re-review.** Codex reviews the fix commits and Opus validates them again. The lane
   merges once this comes back clean.

The user decides only scope changes and disagreements with a spec that Opus raises.

### Merging and pushing

Finished lanes merge into local `main` as they land, and several lanes may share one
batch. One full gate runs on the batch: `scripts/ci.sh`, the conformance suites and a
Namespace screen (`mise run bench:nsc origin/main HEAD`). Push when the tests pass and the
screen shows no query 10% or more slower after a rerun of the flagged queries with more
rounds.

Forge measurements do not hold up a feature lane or a push. A lane whose goal is
performance measures on forge before it reports, and every other lane reports once its
review and tests pass. Pinned forge checks of the screen's flags, including queries under
0.3 ms, run after the push. A regression they confirm is bisected across the batch's merges
and fixed forward in a follow-up commit.
