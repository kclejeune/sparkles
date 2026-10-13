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
| Implementing lanes and fixes | Claude Sonnet 5.5 at high effort |
| Code review | Codex with gpt-6.1-sol at high effort |

A lane starts on Opus instead of Sonnet when its core is crash durability or WAL ordering,
concurrency beyond a parallel iterator, an on-disk format, planner or executor cost
decisions, or `unsafe` code.

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
3. **Sonnet implements.** The lane, or a new Sonnet agent, applies `plan.md` as written and
   reports what it did for each item.
4. **Re-review.** Codex reviews the fix commits and Opus validates them again. The lane
   merges once this comes back clean.

The user decides only scope changes and disagreements with a spec that Opus raises.

### Merging and pushing

Finished lanes merge into local `main` as they land. One full gate runs on the batch:
`scripts/ci.sh`, the conformance suites and a Namespace screen
(`mise run bench:nsc origin/main HEAD`). Push only when the tests pass and no query
regresses by 10% or more. Rerun a flagged query with more rounds, and bisect a confirmed
regression across the batch's merges before pushing.
