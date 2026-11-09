# Agent guidance

This file tells coding agents (Claude Code, Codex and others) how work is done in this
repository. `CLAUDE.md` imports it. Human contributors can read it as the house rules.

Sparkles is a Rust RDF store compatible with Apache Jena and Fuseki, with an index in the
style of QLever. [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) covers the layout, the build,
the mise tasks and the test suites. [docs/ROADMAP.md](docs/ROADMAP.md) sets the order of
work, and each item links to its design spec in [docs/specs](docs/specs/README.md).

A contributor can keep personal instructions in `AGENTS.local.md` at the repository root,
which git ignores, for their own hosts, accounts and tools. Read it when it exists. It
adds to this file and does not override it.

## Building and testing

- Build with Mr. Boxington by running `mbx build`, `mbx check`, `mbx test` or
  `mbx nextest run` in place of Cargo. Check the installed version and ensure that
  child Cargo calls use the matching Boxington shim. `mise.toml` selects the default
  version, but other compatible releases may be used.
- Keep Boxington's one shared cache per host and its managed `target` in each worktree.
  Do not set a private `MBX_CACHE_DIR`, `CARGO_TARGET_DIR` or `--target-dir` for
  ordinary builds, since these opt out of the shared cache or the managed target.
- Use the mise tasks for benchmarks, browser tests and binding tests that consume a
  build, since they hold the target's lease until their processes exit.
- Keep validation evidence and benchmark data in `.artifacts`, outside compiler targets.
- [Building from source](docs/DEVELOPMENT.md#building-from-source) covers installation,
  adoption of existing targets, leases and cache experiments.
- Prefer targeted runs such as `mbx test -p <crate> --test <name>` while iterating.
  `scripts/ci.sh --fast` checks formatting, shell scripts, doc paths, the API and the
  workspace's Clippy and Rust tests. `scripts/ci.sh` runs every check, and the
  conformance suites run with `mise run test:w3c`, `test:shacl` and `test:shex`.
  [Merging and pushing](#merging-and-pushing) says which of these a lane and a batch run.
- Prefer available local implementation and build hosts. Use Boat sandboxes for overflow,
  isolation or additional parallel capacity, after checking the worker's tools, memory,
  disk and competing work. Reuse a warm worker for ordinary fix and test iterations.
  Namespace remains a fallback for a measured resource need or a workflow without a
  qualified Boat replacement. Provider-neutral commands accept an admitted `boat:bx_ID`
  worker and never create or stop it. The tracked `boat:qualify`, `boat:job-env` and
  `boat:finish` commands preserve explicit profile/release and verified closure. See [remote executors](docs/REMOTE-EXECUTORS.md).
- The benchmark host, which `SPARKLES_BENCH_HOST` names, runs no builds or gates. Keep
  other designated benchmark workers available for comparisons, and start a benchmark
  only after other heavy work has left its host, so the engine under test runs alone.

## Long-running and remote work

- Run a long or remote command where it can be listed, read and stopped by name, such as
  a persistent named terminal or a systemd unit. Wait for it with a command that returns
  when it finishes or prints a ready line, not with a sleep loop.
- Stop only processes you started, by their own identifier. Never use `pkill -f` or a
  pattern that can match someone else's work. Before reporting, check that nothing you
  started is still running unless the report says what keeps running and why.
- Run heavy builds and tests in a memory-capped scope where systemd is available, such
  as `systemd-run --user --scope -p MemoryMax=12G -p MemorySwapMax=2G -p OOMPolicy=continue`,
  so that an out-of-memory kill ends that command and not the session or terminal around it.
- Never run two query screens of the same commit at once, since their build cache entries
  share source boundaries. A Boat screen holds one worker lock across both arms and reruns.
- Record remote worker and conversation IDs, source identity and resource deadlines in
  the lane's notes. Reconcile an uncertain launch before retrying it. Retain results and
  release owned jobs and Boxington leases before stopping or deleting a worker. Keep
  development environments and snapshot seeds separate from CI, and exclude credentials
  and live leases from reusable seeds.
- Runs that call paid model APIs need the maintainer's approval and a spending cap first.
  Inject their keys through the secret tool the environment provides, and never print or
  write a key.

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
  need an Intel host, and published numbers come from the benchmark host. See
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
- Follow the [spec layout](docs/specs/README.md#spec-layout) when creating or migrating a
  spec, and keep the [provenance index](docs/specs/PROVENANCE.md) current. Preserve
  historical source and license records without treating old delivery statements as
  current feature status.
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

A delegated agent sees only its checkout, the tracked instructions and its host's own
configuration. Its brief is therefore self-contained and written to a file in its
worktree, such as `.artifacts/lanes/<name>/brief.md`. The agent reports results to its
parent, writes long results to a file, and starts no agents of its own unless the brief
assigns that.

For a remote agent, record sandbox, conversation and prompt IDs with its source,
environment version, branch, resource deadline and snapshot identity when applicable.
Attach the self-contained brief, and confirm subscription authentication before its
first model turn. Paid API runs still require approval and a spending cap. Independent
review uses a separate conversation and checkout. Keep native command logs and exit
results separately from conversation events.

### Models

| Role | Model |
|---|---|
| Coordination, dispatch, monitoring and handoff maintenance | GPT-6.1 Terra or Claude Sonnet 5.5 |
| Substantive planning, dependencies, validation gates and difficult consults | GPT-6.1 Sol or Claude Opus 5.5 |
| Implementation requiring substantial engineering judgment | GPT-6.1 Sol or Claude Opus 5.5 |
| Implementation of a well-defined plan and routine fixes | GPT-6.1 Terra or Claude Sonnet 5.5 |
| Code review and finding validation | Prefer GPT-6.1 Sol for difficult work and GPT-6.1 Terra for straightforward work. Claude Opus 5.5 or Sonnet 5.5 may review when appropriate. |
| Mechanical tasks with an exact rule | A plain script. |
| Broader mechanical tasks, file scans and exploration | Claude Haiku 5.5 or GPT-6 Luna. |

The table is the assignment policy, and it holds whatever model the orchestrator runs.
A coordinating session delegates substantive plans, dependency decisions and validation
strategy instead of taking them on because it controls the session. A plan names the
files and behaviour, edge cases, regression checks, acceptance gates and dependencies
before a cheaper worker executes it.

Work that needs substantial judgment includes a refactor whose shape is unclear, a bug
whose cause is unknown, an open behaviour or API decision, crash durability or WAL
ordering, concurrency beyond a parallel iterator, an on-disk format, planner or executor
cost decisions, and `unsafe` code. Its validating planner normally implements it to keep
the code and reasoning context, and may hand a clearly specified routine portion to a
cheaper worker with the required checks. A loader for a documented format, a conformance
phase fixed by its test suite and a binding that mirrors an existing API are usually
routine.

Prefer Sol and Terra for reviews where they suit the work, to preserve Claude usage
limits. Opus adds an independent perspective on difficult work. Model availability or
quota pressure must not lower the required standard of reasoning or verification, and
review always uses a separate agent, even when it runs the author's model.

Codex runs GPT-6.1 Sol and Terra at high effort for every task. Claude Opus runs at
medium effort by default, and at high effort for the judgment-sensitive work above and
for validating findings and planning fixes on that work.

A plain script takes a mechanical task with an exact rule, such as an exact renumbering
or fixtures from a fixed format. Haiku or Luna takes a task that is clearly mechanical
but too loosely defined to script without the script becoming a task of its own, such as
triaging logs, applying a consistent edit across files in varying contexts or sweeping a
report. They also suit broad scans that find and flag patterns across files, and
exploration that locates code for another agent. Each task states its rule, its output
format and how the result will be checked before it starts. If the check fails twice,
move the task to Terra or Sonnet. Haiku and Luna do not rewrite documentation prose,
touch engine, durability, concurrency, planner or performance code, or review and
validate findings.

A Terra or Sonnet worker consults Sol or Opus through a read-only agent with its
question, relevant files and attempted fixes. It must consult when the same failure
survives two different fixes, the spec leaves behaviour, an API or a format open, the
work enters one of the judgment-sensitive areas above, a performance gate misses for
no clear reason, or before stopping a phase early or declaring a gate unmeetable. The
consult decides whether to provide a bounded plan or transfer implementation to Sol
or Opus. The final report lists each consult.

### Review before landing

No lane lands without review, and each step picks its model from the table above.

1. **An independent agent reviews.** `scripts/codex-review.sh NAME TIP BASE "CONTEXT"`
   runs `codex exec review` on `BASE..TIP` in a scratch worktree and writes `review.md`
   under `$AUDIT_DIR/NAME` (default `.artifacts/audits`). CONTEXT names the roadmap item,
   the specs and what the work was meant to do. The script defaults to Sol at high effort,
   and `CODEX_REVIEW_MODEL` and `CODEX_REVIEW_REASONING_EFFORT` select another choice.
2. **A capable agent validates and plans.** The validating agent checks every finding
   independently and reproduces it where it can. It marks each one confirmed, refuted or
   plausible, decides against the specs whether to fix it now, fix it later under a named
   roadmap item, or document it as a limitation, and writes `plan.md` beside the review.
   For every fix-now item the plan gives the files and functions to change, the behaviour
   to implement, the edge cases, the regression test that must fail before the fix, the
   docs to update and the commands that verify it. It also records serious problems the
   review missed.
3. **The assigned worker implements.** The validating planner normally implements
   judgment-sensitive repairs itself, and a cheaper worker can execute a well-defined
   routine plan. The worker reports what it did for each item.
4. **Re-review.** An independent reviewer checks the fix commits and the validating
   agent validates the findings again. The lane merges once this comes back clean.

A finding blocks the merge and gets a re-review of its fix when it is non-trivial: a
correctness, durability, concurrency, security or data-loss defect, a broken API, format
or spec contract, or a fix that changes behaviour beyond a few lines. Other findings, such
as more precise accounting, documentation wording or a cleanup, are either fixed in the
same round without a further review or recorded as roadmap follow-ups, and the plan says
which. A re-review covers only the fix commits since the previous review, and its
reviewer may keep the earlier round's context instead of reading the branch again.

The user decides only scope changes and disagreements with a spec that the validating
agent raises.

### Merging and pushing

A lane reports once its review is clean and its targeted tests and `scripts/ci.sh --fast`
pass, together with the mise test task of each binding, UI or JavaScript package it
touched, such as `py:test`, `jvm:test`, `node:test`, `node:client:check` or `ui:test`,
since `--fast` runs none of them. The lane may run `scripts/ci.sh --fast --changed <base>`
instead, which runs Clippy and the tests only for the workspace packages its change can
affect and falls back to the whole workspace when a shared root changes. Lanes do not run
full gates of their own.

Finished lanes merge into local `main` as they land, and several lanes may share one
batch. One gate runs on the batch, with a Namespace screen
(`mise run bench:nsc origin/main HEAD`). This provider-specific screen remains the
required fallback until a replacement is qualified; the Boat placement policy does not
change its acceptance criteria. `scripts/gate/gate.sh <rev>` runs the gate on the
configured gate hosts and selects its checks from the batch's diff against `origin/main`.
It runs the `scripts/ci.sh` tasks, conformance suites and spill matrices that the changed
files call for, with Clippy and the Rust tests for the packages they can affect, as the
[batch gate section](docs/DEVELOPMENT.md#batch-gate) describes. Build configuration, the CI
scripts and files that no rule covers run everything. Push when the tests pass and the
screen shows no query 10% or more slower after the rerun of the flagged queries with more
rounds, which `bench:nsc` runs on the same instance and reports beside the first pass.
`scripts/premerge.sh` checks the generated clients, the binding lock files, the Python
parity list, the doc paths and the script tests in minutes, and runs on the merged batch
before the gate starts.

The nightly gate (`scripts/gate/nightly.sh`) runs every check with `--force` on
`origin/main`. A nightly failure is bisected across the merges since the last nightly pass
and fixed forward in a follow-up commit, and a check that the batch selection should have
chosen gets a rule in `scripts/gate/select-checks.py`.

Benchmark-host measurements do not hold up a feature lane or a push. A lane whose goal
is performance measures on the benchmark host before it reports, and every other lane
reports once its review and tests pass. Pinned benchmark-host checks of the screen's
flags, including queries under 0.3 ms, run after the push. A regression they confirm is
bisected across the batch's merges and fixed forward in a follow-up commit.
