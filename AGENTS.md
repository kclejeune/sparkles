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
- Commits are made where the contributor's signing agent is reachable, usually the
  machine the orchestrating session runs on. A terminal on another host can outlive the
  forwarded agent, so remote terminals run builds, tests, gates, benchmarks and read-only
  reviews, and do not commit.
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
- Performance claims need measurements. `mise run bench:remote --executor boat:bx_ID A B`
  gives a quick A/B on a Boat worker, and `mise run bench:nsc A B` on Namespace. Effects under about 5%, queries under 0.3 ms and codegen-sensitive changes
  need an Intel host, and published numbers come from the benchmark host. See
  [docs/BENCHMARKING.md](docs/BENCHMARKING.md).
- Run both arms of an A/B comparison on one host with the same compiler and build
  configuration and no competing heavy work. Never pool or compare timings across hosts,
  even hosts with the same hardware, and qualify a second host's results separately.
- A measured slowdown that a gate accepts still gets a follow-up item in
  [docs/ROADMAP.md](docs/ROADMAP.md), added in the same change that accepts it. This holds
  for a loosened gate and a tradeoff taken on purpose. A difference that the comparison's
  own rule calls noise needs no item. For the Intel rule that is a ratio whose 95%
  interval reaches 1.0 or a median added time of at most 0.5 µs, and for other A/Bs a
  difference that is not above 1 in every round. Small real costs of one workstream go
  into that workstream's running small-costs item as one line per query, naming the host
  and the commits compared, and are never combined across hosts.
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

Every turn of an agent re-reads its whole context, so context size and the number of
turns spent waiting set most of the cost of a lane. Four rules follow from this.

- The coordinator owns long waits. A lane does not wait inside its own session on a
  batch gate, a benchmark-host or benchmark-worker timing run, GitHub Actions or another
  job expected to run past about 45 minutes. It commits its work where the signing agent
  is reachable, or names its source tip and uncommitted changes when it runs where
  commits are not made. It records in its resume file what it is waiting on, where the
  log is and what marks completion, and returns. The
  coordinator watches the job with a background command and starts a fresh agent from
  the resume file when the result is in.
- A lane checks a shorter wait with bounded commands a few minutes apart, often enough
  that its prompt cache stays warm. A longer wait that it cannot hand back runs as a
  background command that notifies on completion, so the agent spends no turns polling.
- A lane is checkpointed and continued by a fresh agent once its context passes about
  150k to 200k tokens, at a natural boundary such as a clean review or a finished phase.
  The checkpoint is a tip committed where signing works, or a named source tip with its
  outstanding changes, and a short resume file that names the next steps, the open
  decisions and the live jobs. An agent idle for more than an hour is replaced
  by a fresh one from its resume file instead of being resumed, unless its context is
  small.
- The coordinator assembles and gates batches in its own session. Merging the ready
  lanes, premerge, the gate and the screen are mostly waiting, so they run as background
  commands, and an agent is started only to fix a failure.

For a remote agent, record sandbox, conversation and prompt IDs with its source,
environment version, branch, resource deadline and snapshot identity when applicable.
Attach the self-contained brief, and confirm subscription authentication before its
first model turn. Paid API runs still require approval and a spending cap. Independent
review uses a separate conversation and checkout. Keep native command logs and exit
results separately from conversation events.

### Models

| Role | Model |
|---|---|
| Coordination, dispatch, monitoring and handoff maintenance | GPT-6.1 Sol or Claude Opus 5.5 |
| Substantive planning, dependencies, validation gates and difficult consults | GPT-6.1 Sol or Claude Opus 5.5 |
| Implementation requiring substantial engineering judgment | GPT-6.1 Sol or Claude Opus 5.5 |
| Second-look lanes and consults at critical decisions | Use Claude Fable 5.2 or GPT-6 Astra. Prefer the family that the stalled lane or the consulting session does not use. When neither is available, use the strongest available model in a separate session. |
| Easy, nearly trivial tasks and the execution of a plan that fixes every step | GPT-6.1 Terra or Claude Sonnet 5.5 |
| Code review and finding validation | Prefer GPT-6.1 Sol. GPT-6.1 Terra may review a nearly trivial change. Claude Opus 5.5 may review when an independent perspective helps. |
| Mechanical tasks with an exact rule | A plain script. |
| Easy tasks that need a lot of context, such as exploration, broad file scans, searches across many files and log triage | Claude Haiku 5.5 or GPT-6 Luna. |

The table is the assignment policy, and it holds whatever model the orchestrator runs.
Work defaults to Sol or Opus. Terra or Sonnet takes a task only when it is nearly
trivial or its plan fixes every step, and Haiku or Luna takes easy work whose cost is
mostly the context it must read.

A coordinating session delegates substantive plans, dependency decisions and validation
strategy instead of taking them on because it controls the session. A plan names the
files and behaviour, edge cases, regression checks, acceptance gates and dependencies
before a cheaper worker executes it.

Work that needs substantial judgment includes a refactor whose shape is unclear, a bug
whose cause is unknown, an open behaviour or API decision, crash durability or WAL
ordering, concurrency beyond a parallel iterator, an on-disk format, planner or executor
cost decisions, and `unsafe` code. Its validating planner normally implements it to keep
the code and reasoning context, and may hand a portion to Terra or Sonnet only when its plan
fixes every step and names the required checks. A loader for a documented format, a
conformance phase fixed by its test suite and a binding that mirrors an existing API
need no substantial judgment, but they still go to Sol or Opus unless their plan fixes
every step.

Prefer Sol for reviews, to preserve Claude usage limits. Opus adds an independent
perspective on difficult work. Model availability or
quota pressure must not lower the required standard of reasoning or verification, and
review always uses a separate agent, even when it runs the author's model.

Codex runs GPT-6.1 Sol and Terra at high effort for every task, and GPT-6 Astra at high
effort when it takes a second-look lane or a consult at a critical decision. Claude Opus
runs at medium effort by default, and at high effort for the judgment-sensitive work
above and for validating findings and planning fixes on that work.

A plain script takes a mechanical task with an exact rule, such as an exact renumbering
or fixtures from a fixed format. Haiku or Luna takes a task only when it is easy and its
cost is mostly the context it must read. Examples are exploration that locates code for
another agent, broad scans and searches that find and flag patterns across many files,
triaging long logs and sweeping a large report. A mechanical edit that is awkward to
script but needs little context goes to a script or to Sol or Opus. Each task states its rule, its output
format and how the result will be checked before it starts. If the check fails twice,
move the task to Sol or Opus. Haiku and Luna do not rewrite documentation prose,
touch engine, durability, concurrency, planner or performance code, or review and
validate findings.

A Terra or Sonnet worker consults Sol or Opus through a read-only agent with its
question, relevant files and attempted fixes. It must consult when the same failure
survives two different fixes, the spec leaves behaviour, an API or a format open, the
work enters one of the judgment-sensitive areas above, a performance gate misses for
no clear reason, or before stopping a phase early or declaring a gate unmeetable. The
consult decides whether to provide a bounded plan or transfer implementation to Sol
or Opus. The final report lists each consult.

A Sol or Opus session consults a second strong model at critical decision points
before it commits to a path, and chooses that model from the second-look row of the
table. The critical decision points are an on-disk format or durability ordering, a
planner or executor cost decision, an open API or behaviour question, a change of
approach after a failed design, closing or deferring an item, and declaring a gate
unmeetable. The consult goes to an advisor tool when the session has one, or otherwise
to a read-only agent with the question, the relevant files and what has been tried. A
Terra or Sonnet worker still consults Sol or Opus as described above, and that Sol or
Opus consult applies this rule in turn. A consult shares the session's framing, so it
never replaces an independent review or a second-look lane. The final report lists
each consult and whether its advice was followed.

When the implementing lane has made no significant progress on a hard problem, the
coordinator starts an independent second-look lane on a model chosen from the
second-look row of the table. Typical signs are designs that measure as failures,
review rounds that keep finding new defects of the same kind, a recommendation
to close or defer an item for lack of a path, or a long-open item whose attempts have
not moved it. The second-look lane gets its own worktree and branch, the full record of
what was tried, and a brief that asks it to test the earlier conclusions rather than
adopt them. It reports a missed path, an improvement or a confirmation, and the first
lane's closing step waits for that report. Its prototypes stay behind switches, pass the
same correctness gates and get the same independent review.

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
   judgment-sensitive repairs itself, and Terra or Sonnet can execute a plan only
   when it fixes every step. The worker reports what it did for each item.
4. **Re-review.** An independent reviewer checks the fix commits and the validating
   agent validates the findings again. The lane merges once this comes back clean.

A finding blocks the merge and gets a re-review of its fix when it is non-trivial: a
correctness, durability, concurrency, security or data-loss defect, a broken API, format
or spec contract, or a fix that changes behaviour beyond a few lines. Other findings, such
as more precise accounting, documentation wording or a cleanup, are either fixed in the
same round without a further review or recorded as roadmap follow-ups, and the plan says
which. A re-review covers only the fix commits since the previous review, and its
reviewer may keep the earlier round's context instead of reading the branch again. A
lane may keep one reviewer session open for this, such as a Codex session in a named
terminal, and send each re-review to it as a new turn that names the fix commits. Each
round still writes its `review.md` under `.artifacts/audits`.

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
batch. One gate runs on the batch, with a query screen on an admitted
Boat worker (`mise run bench:remote --executor boat:bx_ID --build-host boat:bx_ID
origin/main HEAD`). The Namespace screen (`mise run bench:nsc origin/main HEAD`) remains
the fallback when no Boat worker can take the screen, with the same acceptance criteria. `scripts/gate/gate.sh <rev>` runs the gate on the
configured gate hosts and selects its checks from the batch's diff against `origin/main`.
It runs the `scripts/ci.sh` tasks, conformance suites and spill matrices that the changed
files call for, with Clippy and the Rust tests for the packages they can affect, as the
[batch gate section](docs/DEVELOPMENT.md#batch-gate) describes. Build configuration, the CI
scripts and files that no rule covers run everything. Push when the tests pass and the
screen shows no query 10% or more slower after the rerun of the flagged queries with more
rounds, which the screen runs on the same worker or instance and reports beside the first pass.
`scripts/premerge.sh` checks the generated clients, the binding lock files, the Python
parity list, the doc paths and the script tests in minutes, and runs on the merged batch
before the gate starts.

The nightly gate (`scripts/gate/nightly.sh`) runs every check with `--force` on
`origin/main`. A nightly failure is bisected across the merges since the last nightly pass
and fixed forward in a follow-up commit, and a check that the batch selection should have
chosen gets a rule in `scripts/gate/select-checks.py`.

Benchmark-host measurements do not hold up a feature lane or a push. A lane whose goal
is performance measures on a designated benchmark worker with the benchmark host's
hardware before it reports, and every other lane reports once its review and tests pass.
The benchmark host itself runs published measurements and the pinned checks of the
screen's flags, including queries under 0.3 ms, after the push. A regression they confirm is
bisected across the batch's merges and fixed forward in a follow-up commit. The forge
matrix runs after every push that changes the query path, ahead of lane comparisons on
forge. Confirmation cells that a lane's goal does not need, such as 12-thread cells and
reverse-order checks, wait for that matrix. Compile-time comparisons run on a quiet build
host, since forge builds nothing.
