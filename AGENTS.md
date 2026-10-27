# Agent guidance

This file tells coding agents (Claude Code, Codex and others) how work is done in this
repository. `CLAUDE.md` imports it. Human contributors can read it as the house rules.

Sparkles is a Rust RDF store compatible with Apache Jena and Fuseki, with an index in the
style of QLever. [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) covers the layout, the build,
the mise tasks and the test suites. [docs/ROADMAP.md](docs/ROADMAP.md) sets the order of
work, and each item links to its design spec in [docs/specs](docs/specs/README.md).

## Building and testing

- `scripts/ci.sh` is the default local gate. Add `--exhaustive-features` for the optional
  feature lint and stripped-server compatibility matrix. The conformance suites run
  separately with `mise run test:w3c`, `test:shacl` and `test:shex`.
- Use Mr. Boxington as the Cargo drop-in on local and remote build hosts. Run
  `mbx build`, `mbx check`, `mbx test` or `mbx nextest run` directly. Run
  `mise install mr-boxington` once on each host to install the pinned tool. Mise
  tasks call `mbx` and wrap Cargo calls made by child scripts and build tools.
  Check that `mbx --version` matches the version in `mise.toml` on each host.
  Having `mbx` on PATH does not prove that plain Cargo uses it.
- Keep Boxington's cache shared across worktrees on each host. Do not set a private
  `MBX_CACHE_DIR` for each checkout or share one mutable Cargo target directory.
  Each machine has its own shared cache. For cache experiments, use explicit isolated
  controls and record wrapper, incremental and linker settings.
- Let Boxington manage the default `target` directory of each worktree. Keep
  `target.views` enabled and avoid `CARGO_TARGET_DIR` or `--target-dir` for ordinary
  worktree builds, since these overrides opt out of management. `mise run cache:adopt`
  adopts an idle checkout's existing target without deleting its contents. Do not move
  targets used by running jobs. Each worktree still has its own mutable target, so one
  build cannot replace another worktree's test binary.
- Use the mise tasks for benchmarks, browser tests and binding tests that consume
  compiled files after a build. These tasks keep Boxington's managed-target lease
  until their downstream processes exit. Ordinary builds and Rust tests call `mbx`
  directly. A binary launched separately is not protected. Keep explicit isolated
  targets for controlled performance comparisons.
- Keep validation evidence and benchmark data in `.artifacts`, outside managed
  compiler targets. CI writes `.artifacts/ci`, reviews write `.artifacts/audits`,
  and benchmark tasks default to `.artifacts/bench…` or `.artifacts/nsc…`.
  Their outputs must survive collection after a command's target lease ends.
- Prefer targeted runs (`mbx test -p <crate> --test <name>`) while iterating, and run the
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
- An optimization that is built and measured but not kept, because it measured no faster
  or slower, gets an entry in
  [docs/PERFORMANCE-EXPERIMENTS.md](docs/PERFORMANCE-EXPERIMENTS.md) in the same change.
  The entry records the measurement and when the idea could still pay off, such as at a
  larger scale. The code stays reachable as a commit and its revert on `main`. Experiment
  tags are never pushed.

## Commits and documentation

- Commit in logical chunks that each compile and pass their tests. Do not add attribution
  lines to commit messages.
- New specs use `docs/specs/<ID-and-name>/README.md` and `SPEC.md`, including small specs.
  Keep a concise Outcome overview in `SPEC.md` and link to an optional `OUTCOME.md` for
  a substantial delivery record. Follow the [spec layout](docs/specs/README.md#spec-layout)
  when creating or migrating a spec.
- Keep short provenance in the spec's Provenance section. Link to a bundle-local
  `PROVENANCE.md` for substantial records, and keep the
  [provenance index](docs/specs/PROVENANCE.md) current. Preserve historical source and
  license records without treating old delivery statements as current feature status.
- Current capability details and limitations belong in the feature-category guides
  linked from [FEATURES.md](docs/FEATURES.md). Keep configuration, usage and roadmap
  sections discoverable there. FEATURES owns the capability matrix, and ROADMAP owns
  item priorities and dependencies. Preserve existing links when moving content.
- Benchmark methodology and results belong in `docs/benchmarks/<workload>/README.md`
  and `RESULTS.md`, linked from the benchmark index. Keep raw run records immutable
  and identify their host, build, configuration and correctness evidence. Distinguish
  manual reports from generated reports, and check generated output against its
  explicit source selection with `python3 scripts/bench-docs.py --check`.
- When a roadmap item or spec phase ships, update its spec's Outcome section,
  [docs/FEATURES.md](docs/FEATURES.md), the relevant
  [benchmark report](docs/benchmarks/README.md) when there are numbers, and the item's
  row in [docs/ROADMAP.md](docs/ROADMAP.md), all in the same
  change. Update the affected feature guide too. A new known gap in feature documentation
  gets a roadmap item in the same commit.
- Write documentation as plain technical prose in full sentences. Avoid fragment openers,
  label-colon openers, chains of colons, parentheses or semicolons, "(yet)", marketing words
  and filler. Keep table cells to one to three short sentences.
  [docs/COMPARISON.md](docs/COMPARISON.md) is the reference voice.
- Public docs describe the project as it is now. README, FEATURES, BENCHMARKS, ROADMAP
  and the guides carry no progress dates, "Done (date)" notes, superseded measurements,
  review history or mentions of agents and lanes. History belongs in CHANGELOG.md and in
  the specs' Outcome sections. Finished roadmap items go to the roadmap's Completed list.
- `docs/plans/` holds private working notes. Never link to it from tracked files.

## Multi-agent workflow

Large work is split into lanes, one agent per roadmap item or spec phase, each in its own
git worktree and port range. An orchestrating session starts the lanes, merges them and
keeps the handoff notes current.

### Models

| Role | Model |
|---|---|
| Coordination, dispatch, monitoring and handoff maintenance | GPT-6.1 Terra or Claude Sonnet 5.5 |
| Substantive planning, dependencies, validation gates and difficult consults | GPT-6.1 Sol or Claude Opus 5.5 |
| Implementation requiring substantial engineering judgment | GPT-6.1 Sol or Claude Opus 5.5 |
| Implementation of a well-defined plan and routine fixes | GPT-6.1 Terra or Claude Sonnet 5.5 |
| Code review and finding validation | Prefer GPT-6.1 Sol for difficult work and GPT-6.1 Terra for straightforward work. Claude Opus 5.5 or Sonnet 5.5 may review when appropriate. |
| Mechanical tasks with a deterministic check | A script first, then Terra, Sonnet or Claude Haiku 5.5 when language work is needed. |

Choose the model for each responsibility, independently of the orchestrator's model.
An orchestrator running Terra or Sonnet can coordinate the workflow, but delegates
substantive plans, dependency decisions and validation strategy to Sol or Opus. A plan
names the files and behaviour, edge cases, regression checks, acceptance gates and
dependencies before a cheaper worker executes it. Do not assign difficult planning to
the coordination model merely because it controls the session.

Sol or Opus owns work whose shape requires substantial judgment. Examples include a
refactor whose shape is unclear, a bug whose cause is unknown, an open behaviour or API
decision, crash durability or WAL ordering, concurrency beyond a parallel iterator,
an on-disk format, planner or executor cost decisions, and `unsafe` code. Its validating
planner normally implements that work itself to retain the code and reasoning context.
It may hand a clearly specified routine portion to Terra or Sonnet with the required
checks. A loader for a documented format, a conformance phase fixed by its test suite,
and a binding that mirrors an existing API can usually go directly to Terra or Sonnet.

Apply the same judgment to reviews and finding validation. Prefer Sol and Terra when
they are suitable to preserve Claude usage limits. Sol at high effort is the default
for substantial code review, difficult findings and remediation plans. Terra can review
a small, straightforward change with deterministic checks. Opus remains available for
difficult work or an additional independent perspective, and Sonnet can review clear-cut
work. Model availability or quota pressure must not lower the required standard of
reasoning or verification. Use a separate agent for independent review even when the
author and reviewer use the same model.

Mechanical language tasks state an exact rule, fixed output format and deterministic
check before starting. Examples include listing the first error in logs, applying an
exact renumbering rule, or generating fixtures from a fixed format. Haiku does not
rewrite documentation prose, touch engine, durability, concurrency, planner or
performance code, or review and validate findings. If its check fails twice, move the
task to Terra or Sonnet. Use a plain script whenever it suffices.

A Terra or Sonnet worker consults Sol or Opus through a read-only agent with its
question, relevant files and attempted fixes. It must consult when the same failure
survives two different fixes, the spec leaves behaviour, an API or a format open, the
work enters one of the judgment-sensitive areas above, a performance gate misses for
no clear reason, or before stopping a phase early or declaring a gate unmeetable. The
consult decides whether to provide a bounded plan or transfer implementation to Sol
or Opus. The final report lists each consult.

### Review before landing

No lane lands without review:

1. **An independent agent reviews.** Prefer Codex with Sol or Terra according to the
   model policy above. `scripts/codex-review.sh NAME TIP BASE "CONTEXT"` runs
   `codex exec review` on `BASE..TIP` in a scratch worktree and writes `review.md` under
   `$AUDIT_DIR/NAME` (default `.artifacts/audits`). CONTEXT names the roadmap item, the specs
   and what the work was meant to do. The default is Sol at high effort; set
   `CODEX_REVIEW_MODEL` and optionally `CODEX_REVIEW_REASONING_EFFORT` for another choice.
2. **A capable agent validates and plans.** Sol or Opus handles substantive findings
   and remediation plans; Terra or Sonnet may validate straightforward findings under
   the same model policy. The validating agent checks every finding independently and
   reproduces it where it can. It marks each one confirmed, refuted or plausible, decides
   against the specs whether to fix it now, fix it later under a named roadmap item, or
   document it as a limitation, and writes `plan.md` beside the review. For every fix-now
   item the plan gives the files and functions to change, the behaviour to implement, the
   edge cases, the regression test that must fail before the fix, the docs to update and the
   commands that verify it. It also records serious problems the review missed.
3. **The assigned worker implements.** The validating planner normally implements
   judgment-sensitive repairs itself. Terra or Sonnet can execute a well-defined
   routine repair plan. The worker reports what it did for each item.
4. **Re-review.** An independent reviewer checks the fix commits and the validating
   agent validates the findings again, with the same model-selection policy. The lane
   merges once this comes back clean.

The user decides only scope changes and disagreements with a spec that the validating
agent raises.

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
