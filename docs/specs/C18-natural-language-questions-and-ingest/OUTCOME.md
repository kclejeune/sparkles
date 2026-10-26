# C18: Questions and ingestion in natural language — Delivery record

Current delivery status is summarized in the [landing page](README.md).
The [design Outcome overview](SPEC.md#outcome) links the supported scope to this record.
The recorded dates and measurements below describe their original delivery stages.

## Historical opening status

> **Status:** implemented in part (Phases 1, 2, 2b, 3, 3m-a, 3m-b, 4 and 5)
>
> **Phases:** Phases 1 and 2 shipped on 2026-10-09, without the measured runs of the
> model matrix on the public sets. Phases 2b, 3, 3m-a, 3m-b, 4 and 5 shipped on 2026-10-10. Phase 1 lets an
> agent connected over MCP hand the query it wrote for a question to the web UI, where a
> person reads, edits and runs it, and adds
> a memory browser that shows each fact's source, passage and history. It also adds
> model providers to the server, for Ollama and other local models, any
> OpenAI-compatible endpoint and Anthropic's API, with an ordered list of provider and
> model pairs for each use, and an evaluation matrix that measures every pair. Phase 2
> adds an **Ask** bar on the query page that runs the whole question-to-query flow and
> escalates to a stronger model on signals the server can verify. Phase 2b explains any
> query in plain language next to its plan. Phase 3 lets an agent turn documents into
> proposed facts on a review branch, which a person accepts or rejects in the UI.
> Phase 3m imports the memory of coding agents such as Claude Code and Codex into the
> graph, and adds the `sparkles memory` commands with a brief of the facts for each
> session. Phase 4 runs ingestion inside the server, with PDF conversion by
> pdf-inspector. Phase 5 adds the maintenance of agent memory. Phase 6 suggests faster rewrites of a
> query and proves that they return the same results. Every phase builds on the tools
> of [C17](../C17-agent-memory.md).
>
> **User docs:** [API: Natural-language questions](../../API.md#natural-language-questions) ·
> [API: MCP tools](../../API.md#tools) ·
> [Usage: Asking questions with a model](../../USAGE.md#asking-questions-with-a-model) ·
> [Usage: Handing a query to the web UI](../../USAGE.md#handing-a-query-to-the-web-ui) ·
> [Usage: The Ask bar](../../USAGE.md#the-ask-bar) ·
> [Usage: Explaining a query](../../USAGE.md#explaining-a-query) ·
> [API: Explaining a query](../../API.md#explaining-a-query) ·
> [Usage: Agent memory](../../USAGE.md#agent-memory) ·
> [API: Importing agent memory](../../API.md#importing-agent-memory) ·
> [Usage: Ingesting documents and reviewing memory](../../USAGE.md#ingesting-documents-and-reviewing-memory) ·
> [API: Ingest profiles](../../API.md#ingest-profiles) ·
> [API: Review inbox](../../API.md#review-inbox) ·
> [Usage: Ingesting documents in the server](../../USAGE.md#ingesting-documents-in-the-server) ·
> [API: Ingestion](../../API.md#ingestion) ·
> [API: Memory maintenance](../../API.md#memory-maintenance) ·
> [Usage: Maintaining agent memory](../../USAGE.md#maintaining-agent-memory) ·
> [Features](../../FEATURES.md)
>
> This is the design as written before implementation. The [Outcome](SPEC.md#outcome) section at
> the end records how it lands.

## Outcome

**Amendment.** [C19 §11](../C19-layered-settings.md#11-server-wide-model-settings)
reverses the decision of §3.4 that no HTTP route creates or changes a provider. Server
administrators can now change the model configuration, endpoints included, and store
write-only keys through `/$/server/settings/models` and `/$/server/secrets/{name}`,
unless the operator's settings file locks the fields. Users with `admin` on a dataset
still only choose among the providers by name.

**Phase 1 delivered on 2026-10-09.** The matrix has not been run with real models, and
the public sets of §11.1 are not converted, so the targets of §11.3 and the measured role
lists of §11.4 are still open. Phase 2 followed on the same day, and Phases 3m-a, 3,
3m-b, 4, 2b and 5 on 2026-10-10. They are recorded below, and Phase 6 is not
built.

- **Model providers.** The server's `models` module holds the three kinds of §3.4 with
  their request and response formats, named secrets read at each request from
  `--model-secret NAME=env:VAR|file:PATH`, concurrency slots, request spacing, a daily
  token budget, estimated cost from `pricing`, and the role lists of §3.7.
  `Models::call` asks one pair for an answer that matches a schema. Under `auto` it
  tries the JSON Schema mode, then JSON mode, then plain text, remembers the first
  level that works for each pair, validates every answer against the schema subset of
  §3.6, and retries an invalid answer once with the errors. A key is scrubbed from every
  error message, and no response carries it. `GET /$/models` and
  `POST /$/models/{name}/test` describe and test the pairs. The provider clients are
  tested against mock endpoints only (A19, A26).
- **Tools and routes.** `share_query` and `why_empty` are MCP tools, and
  `POST /{ds}/check`, `POST /{ds}/recall` and `POST /{ds}/sparql/diagnose` run
  `check_query`, `recall` and `why_empty` over HTTP. Each request builds an MCP server
  for the caller, so HTTP and MCP share one code path, one view and one set of budgets.
  `check_query` gains `terms`, the list of §4.5. `recall` gains `statuses`,
  `unreviewedWeight`, a `status` on each fact and citation and `replacedBy` on
  superseded entries, read from `memory.json` (§8.8). `/$/memory/{ds}` and
  `/$/queries/{ds}/suggestions` keep the memory settings and the suggested examples.
  The `ask_graph` prompt holds the steps of §4.1 as rules (A1 to A4, A28).
- **The pipeline.** The `ask` module runs §4.1 as a library function over the MCP tools,
  as the caller, with the dataset's prefixes, and reports the events of §5.2. It
  extracts the mentions of a question from quoted strings and runs of capitalized
  words, asks about an `ambiguous` mention before drafting, checks every draft,
  refuses updates through `check_query`, adds `LIMIT 1000`, repairs at most twice from
  the check's issues, a failed run or `why_empty`, and summarizes up to 50 rows with
  row markers that it checks against the rows sent. It trims the grounding for a small
  context window and leaves out the summary below 8,192 tokens (A27). It also computes
  the complexity score of §5.5, which Phase 2 routes on and the matrix buckets by.
  `sparkles ask` runs it on a database or on files and prints text, one JSON object or
  one JSON line per event.
- **Evaluation.** `testsuite/ask/demo.json` holds 68 questions over the mock server's
  organisation graph, which `testsuite/ask/org.ttl` keeps as Turtle. There are 59
  answerable questions with gold queries in seven categories, four that need a
  clarification and five that the data cannot answer. A test runs every gold query
  and checks its stored complexity. The initial `scripts/eval-ask` report now uses
  one whole-pipeline row per pair and a cascade row computed from those runs.
  A failed Ask counts as a scored failure, so one provider error no longer ends a
  matrix. Schema 3 reports strict RDF-term row-set precision and recall,
  separate answerable, clarification and refusal denominators, and bounded child
  execution statuses. Its policy is `rdf-term-set-projected-columns-v2`, which projects
  an answer with extra columns onto the gold's columns under one mapping and counts the
  answers that needed it. `--scoring-policy` still selects the whole-row policy of the
  first paid run. A bounded first-draft query probe distinguishes an actually
  failed first draft from repair invocation. Incomplete probes stay unknown and
  outside the conditional repair denominator. Gold preflight must succeed before any model call. Reports
  require a new output directory and an exact pinned binary. The paid wrapper
  `scripts/eval-ask-anthropic` runs a loopback proxy that alone holds the API key and
  forwards each request once with the standard library, which decodes every reply
  framing. An append-only ledger reserves an upper bound before each request, settles
  the cost from the reply's usage afterwards and refuses a request that would pass the
  local cap. Error replies pass through to the client's own retries and cost nothing.
  Credential-free tests drive the CLI against a loopback fake provider. They establish
  execution and accounting controls, not real accuracy or provider behavior.
  Independent role experiments and public corpus alignment remain roadmap item 6.
- **Access.** `sparkles auth grant --template agent` prints the grants of §8.6 (A30).
  The rule of §8.8 that the `agent_memory` prompt adds is the constant
  `mcp::context::UNREVIEWED_RULE`, which `ask_graph` already includes.
- **UI.** A handoff link opens a new query tab with the question header and the terms
  list and does not run (A1, A2). The Saved menu lists the questions asked in the
  browser, **Save as example** proposes parameters for the linked constants (A5),
  **Suggest as example** posts a suggestion that an admin promotes or dismisses (A6), and
  an empty result shows the diagnosis. The Explore page has a Memory tab and the UI a
  Memory page (A20, A21).

**Deviations and additions.**

- Branch names cannot hold a slash, so the proposal branches of the agent template are
  `proposals.NAME.*` instead of `proposals/NAME/*`. The UI treats both forms as review
  branches. The template's write grants list the endpoints `query`, `update`, `gsp-r`,
  `gsp-rw`, `info` and `branches`, which leaves out merges.
- The model configuration lives only in the file of `--model-config`, which may wrap it
  in `{"models": …}` as §3.4 writes it. The schemas of `Draft` and `Summary` mark every
  member as required and use empty strings and lists for absent members, because the
  strict mode of the `openai` kind accepts no optional member. Lengths are enforced
  after validation rather than in the schema.
- A draft whose `query` is empty is the model's way of saying that the data cannot
  answer the question, and the ask ends with the outcome `unanswerable`.
- In Phase 1 `sparkles ask` uses the first pair of each role and does not move to the
  next pair on failure. `why_empty` has no `verdict` yet, so repair stops early only
  when the first element without solutions is a join with no issues and every constant
  occurs.
- The matrix runs each pair in the draft, repair and summarize roles at once. The
  cascade row is computed from those runs, moving a question on when the server sees a
  failure, rather than from separate runs with escalation, which needs Phase 2.
- `scripts/qald9plus-convert` converts the QALD-9-plus test split (item 6). Text2SPARQL'25
  is not converted. `scripts/eval-ask` takes any set in the demo set's format with its
  own data file or, with `--loc`, a database directory.
- Promotion and rejection in the review inbox, and `actedOnBehalfOf` in citations, are
  left to Phase 3.
- A stored query named `suggestions` cannot be read at `/$/queries/{ds}/suggestions`,
  which the suggestion list now answers.

**Phase 2 delivered on 2026-10-09.** Every test runs against mock providers, so the
role lists and thresholds that the server ships with are still the defaults of §5.5
rather than measured ones.

- **The endpoint.** `POST /{ds}/ask` runs the pipeline of §4.1 in a blocking task under
  the query rate-limit class and streams the events of §5.2. A client that sends
  `Accept: application/json` alone gets one object, with the status of its error code.
  A client that goes away sets a cancel flag that the pipeline reads before each step.
  The route reads `assistant.json` from the main dataset, answers `no-assistant`
  without a usable draft list, refuses a request over a daily token cap with `429` and
  `resetAt` before any model call (A15), and answers a clarification as `{id, value}`
  (A14). Every tool runs as the caller over the caller's view, so a term of a hidden
  graph never reaches a provider (A16), and the injection test of Phase 1 covers the
  same code path (A17). With `send: "schema"` no step sends rows, and no summary is
  made (A12).
- **Settings.** `GET` and `PUT /$/assistant/{ds}` keep `assistant.json` with the members
  of §3.5. A `PUT` refuses `endpoint` and `apiKey` anywhere, unknown roles, unknown
  providers and models outside `allowedModels` (A19). `sendByProvider` filters the
  summarize list to the providers that may receive rows (A42). The `GET` answer adds a
  `status` that says whether asking works and why not, which the UI reads to show the
  Ask bar.
- **Escalation.** Each role keeps a pointer into its list. A step moves to the next pair
  on `check-failed` when a repair still fails its check or run, on `empty-query` when a
  repair is still empty and `why_empty` blames the query, on `provider-failure` for a
  timeout, a refusal or invalid output after its retry, on `complexity` when a first
  draft, the best example or the last earlier turn scores at least the threshold, and
  on `try-harder`. A role moves at most twice and an ask has at most two failed calls.
  Each move is an `escalate` event and a step of the routing record (A13, A35 to A40).
- **The verdict.** `why_empty` answers `verdict`. It is `query` when a `check_query`
  issue explains the empty element, `data` for a join or filter whose constants all
  occur and for a pattern with an absent constant that has no suggestion, and `unknown`
  otherwise. The pipeline stops on `data` without another model call (A37).
- **History, feedback and usage.** The server keeps each principal's asks in
  `<db>/asks.json` for `historyDays`, prunes them every hour, and never stores rows or
  summaries. `GET` and `DELETE /$/asks/{ds}` show and remove only the caller's own
  entries (A34). Feedback counts by role, pair and complexity bucket, and
  `GET /$/models/usage` reports the counts with no question text (A41). `/$/metrics`
  carries `sparkles_ask_total`, `sparkles_ask_answers`,
  `sparkles_ask_escalations_total`, `sparkles_ask_tokens_total` and
  `sparkles_ask_estimated_cost_total`.
- **reviewedOnly.** `GraphRule` gains excluded patterns, and the principal of an ask
  with `reviewedOnly` hides the dataset's `agentGraphs` from every step (A28).
- **UI.** A dataset with an assistant gets the Ask bar above the query tabs with
  **Preview first** and **Run, then show**, remembered per principal, a step indicator
  and **Stop**. An answer opens in a new tab with the question header and the checked
  query. Its rows use the table, graph, map and plan views of any query. The summary's
  markers select the cited rows, a row under the pointer lights its markers, and the
  summary collapses. An edit clears the summary, and **Summarize again** sends the
  edited query. **Correct**, **Not correct** and **Try harder** send feedback, and the
  model's name carries the routing record in its tooltip. Clarification choices, graph
  variables, follow-up context, the failure states of §6.5, the server-side Asked list
  and the Explore page's **Ask a question…** hint are in place (A22 to A25). The mock
  server scripts the pipeline for the Playwright tests in `ui/tests/mock/ask.spec.ts`.
- **CLI and matrix.** `sparkles ask` reads `assistant.json` of a database, walks the role
  lists without `--pair`, and takes `--try-harder N`. The cascade row of
  `scripts/eval-ask` is computed from the per-pair runs, moving a question on when the
  server sees a failed Ask, and reports the share answered by each pair (A43).

**Deviations and additions of Phase 2.**

- The `results` of a `result` event use the `application/x-sparkles+json` form of
  `/{ds}/sparql`, with the plan and the timings, rather than the SPARQL 1.1 JSON
  results format, so the UI's views and the Plan tab take it unchanged.
- `send` defaults to `schema`, the most private level, because §3.5 names no default.
- `POST /$/asks/{ds}/{id}/feedback` is an addition, because §5.5 records feedback but
  names no route for it. The `query` member of an ask is an addition for **Summarize
  again**. It checks, runs and summarizes a given query without a draft.
- A clarification may be sent as `{id, value}` or as the value alone.
- The usage counters of `GET /$/models/usage` and the daily token counts live in memory,
  so a restart starts them again. The history file is the only stored record.
- An unanswerable question and a draft that still fails after its repairs end the
  stream with an `error` event that carries the last draft, and the `usage` event names
  the outcome, so a stream client learns how the ask ended.
- A16 is tested in the library with a C12 grant restricted to one graph, and A28 with
  a query that reads the default graph and every named graph.
- With history on, the UI keeps asked questions on the server only, and the Asked list
  shows the server's entries before the questions of handoff links kept in the browser.

**Phase 3m-a delivered on 2026-10-10.** A person's Claude Code and Codex memory and
instruction files become graphs of cited, unreviewed facts, a hook keeps them current,
and a session start hook prints a brief of what the graph knows about the project.

- **The adapters.** The `sparkles-memory-import` crate reads Claude Code's memory files,
  its `MEMORY.md` index and its instruction files (`CLAUDE.md`, `CLAUDE.local.md`, rules
  and the managed file), Codex's `AGENTS.md` and `AGENTS.override.md` files and its
  generated memories, and the generic set of `GEMINI.md`, Cursor rules, GitHub Copilot's
  instructions and any file given with `--path`. It parses the frontmatter itself,
  follows `@path` imports only inside the project or `~/.claude` up to five levels,
  redacts every text before it is parsed, skips files with the generated marker, and
  turns each file into structural facts with the line each one came from. Project keys
  come from the git remote, worktrees included, and entity IRIs are version 5 UUIDs of
  the dataset's id, so a link to a memory that does not exist yet already names the IRI
  that memory will get.
- **The server.** `POST /{ds}/facts` runs `assert_facts` and `POST /{ds}/memory/brief`
  runs the new `Tools::memory_brief`, both through the handler of the existing tool
  routes. The facts route needs `write` on the graphs written and counts as an update,
  and the brief needs `read`. `memory.json` gains `imports` with `base`,
  `secretPatterns`, `transcripts` and `extract`, and a `PUT` checks that `agentGraphs`
  covers the base. `sparkles auth grant --template agent --import --import-base IRI`
  adds a write grant on `<base><agent>/*` on `main`.
- **The commands.** `sparkles memory` has `init`, `import`, `sync`, `sources`,
  `status`, `brief`, `recall`, `query`, `assert`, `forget` and `setup`, with `--server`,
  `--dataset`, `--loc`, `--branch`, `--insecure-http`, `--if-reachable` and `--json` on
  each and the exit statuses of §10.1. `--loc` serves the database in the process
  through the same router and fails with the store's `locked` error when a server holds
  it. The `memory` feature, on by default, gates the commands.
- **Sync.** A sync lists the caller's import graphs with their digests in one query and
  compares each scanned file with its source. A new file gets every fact, an edited one
  is diffed against the structural facts its graph holds, a moved one changes only
  `mem:filePath`, and a deleted one is retracted. The cache in
  `$XDG_STATE_HOME/sparkles/memory/` is keyed by server, dataset and principal, and
  lets a sync skip the server when no file changed. The lock queues a second sync, and
  the holder runs once more.
- **Hooks and setup.** `sync --from-hook claude-code|codex` reads the hook's JSON, exits
  before any request for a file that is neither memory nor instructions, and syncs only
  the named file otherwise. `--detach` starts the sync as a background process.
  `brief --hook` prints the brief as plain text and nothing at all on any failure.
  `brief --write FILE` writes the generated marker first and refuses to overwrite a file
  without it. `setup claude-code|codex` prints the hooks, the skill and Codex's MCP
  entry of §10.4, and `--write` merges them once.

**Deviations and additions in Phase 3m-a.**

- No source registration exists before Phase 3m-b, so the graph IRI is also the
  source's IRI and the source is described in its graph by structural facts with
  `spk:contentDigest`. No text, rendition or chunk is stored. A60 therefore makes no
  call at all for an unchanged file, because the sync compares digests with the
  server's listing. The prose facts and `reanchorFrom` of A62 and A64 wait for 3m-b.
  A rename writes the new graph with `dcterms:replaces` the old one and treats the old
  graph as deleted.
- A deletion retracts every fact of the graph except the source's description, which
  keeps `spk:contentDigest`, `mem:filePath` and the other source facts and gains
  `prov:invalidatedAtTime`. The file's text appears in the memory browser once 3m-b
  stores it.
- The sync reads back only the predicates the import writes, so facts that an agent
  extracts from the same file are never retracted by a sync.
- An idempotency key is `import:` and 48 hex digits of the SHA-256 of the graph, the
  old and new digests, the head and the part number. The form `import:<graph>:<digest>`
  could exceed the 128 characters that `assert_facts` accepts.
- The principal in graph IRIs is the name the caller acts as. A minted token acts as
  its owner, a configured token as its name without `cfg-`, and a `--loc` run as the
  operating-system user.
- The session start hooks print the project scope of the hook's `cwd`, because a
  session that starts has no query for the session scope.
- `sync --watch` polls the harness directories once a second and syncs after 2 seconds
  without a change, instead of using the operating system's notifications, which would
  need a new dependency.
- `sources --needs-extraction` lists a source that holds no fact with a predicate the
  import does not write, which approximates the rendition check of 3m-b.
- `query QUESTION` calls `POST /{ds}/ask` of Phase 2 and fails with `unavailable` when
  the server does not have it. `query --sparql` works now.
- `init` creates a guard in `warn` mode over the union graph with the shapes graph
  `urn:x-sparkles:shapes:mem`, or adds that graph to an existing SHACL guard. A ShEx
  guard, or a caller who is not an admin, leaves the guard alone and the output says
  why. Without `--import-base` and without a current base, `init` uses
  `urn:x-sparkles:import/`.
- Redaction runs in the command line, with the built-in patterns, the dataset's
  `secretPatterns`, the `redact` entries of `memory.toml` and `--redact-patterns`. The
  server's `secret-detected` check of A66 comes with `POST /{ds}/sources` in 3m-b.
- A file given with `--path` whose name is a known instruction file lands in the generic
  harness's instruction area, so a later sync without that `--path` records it as
  deleted. Other files given with `--path` are memory files and stay until `forget`.
- The brief's base score for the entity and session scopes comes from recall's seed
  rank and hop. `matched` counts the facts that pass the review filter.
- `memory.toml` takes `server`, `dataset`, `skip-projects`, `redact`, `claude-dir` and
  `codex-dir`. `SPARKLES_MEMORY_WATCH_EVENTS` stops `sync --watch` after that many
  syncs, which the tests use.
- `--transcripts` prints why no transcript was read, and `setup --transcripts`,
  `export --sources`, `inbox`, `review`, `promote` and `reject` are left to 3m-b and
  Phase 3.

**Tests.** `crates/sparkles-server/tests/cli_memory.rs` runs the binary against a server
with authentication on fixture directories. It covers A59 to A65, A68, A69, A72, A73,
A74, the sync and assert parts of A76, and A77, and `memory_loc` covers `--loc` and the
`locked` error. `mcp/import_tests.rs` covers the facts route, the import settings, A68,
A70 and A71 at the route level. The adapters have unit tests and
`crates/sparkles-memory-import/tests/adapters.rs`, and A30 with `--import` is in the
router's auth tests. A66, A67, A75 and the promotion of A76 wait for 3m-b and Phase 3.

**What 3m-b and Phase 3 build on.** `register_source` can adopt the graph IRIs as source
IRIs, because every import graph already carries its digest, and the first 3m-b sync
can register the text of each source whose digest matches. The adapters keep the line
of every structural fact and a digest of the body after the frontmatter, which spans
and re-anchoring need. `memory_brief` and the
`Mode::Internal` route are the pattern for other `Tools` methods over HTTP, and the
`sparkles memory` dispatcher has room for `inbox`, `review`, `promote`, `reject` and
`export`. The import graphs match `agentGraphs`, so the review inbox of Phase 3 sees
every imported fact as unreviewed.

**Phase 3 delivered on 2026-10-10.** An agent turns a document into facts that cite
their passages on a proposal branch, and a person reviews ingestions, proposals and
unreviewed session facts in one inbox and promotes or rejects them. Every test runs
without a model, and the ingestion evaluation has run only with scripted agents.

- **Sources and spans.** `register_source` normalizes the text to NFC with `\n` line
  ends, splits it into chunks at headings, paragraphs and sentences, and writes the
  source, its `spk:TextRendition` and the chunks of §7.2 to the source's graph in one
  commit. The rendition IRI is a version 5 UUID of the dataset and the digest, so the
  same text writes nothing again. A changed text makes a new rendition with
  `prov:wasRevisionOf` the old one, and the result names `previousRendition` and the
  number of `staleFacts`. `read_chunks` and `list_sources` read the sources back.
  `assert_facts` takes `span` on a fact, checks that the quote is the text there after
  folding whitespace, fills in a missing quote from the text, and derives the reifier
  from the span IRI `<rendition#char=start,end>` and the source. It also takes
  `derivedFrom` and `retractStale` (A7, A8, A11).
- **Ingest profiles.** `ingest_profile` answers the classes and predicates of a named
  profile, or of the whole schema report, with a JSON Schema for an extraction whose
  class and predicate members are enumerations. A fact with a span and a predicate the
  rendition's profile does not list is `unknown-predicate` (A9). `<db>/ingest.json`
  keeps `keepText` and up to 50 profiles, and `GET /$/ingest/{ds}/profiles`,
  `PUT /$/ingest/{ds}/settings` and `GET`, `PUT` and `DELETE
  /$/ingest/{ds}/profiles/{name}` manage them. With `keepText: false` a source keeps
  only its digest and length, `read_chunks` answers `no-text`, and a fact with a span
  needs its quote.
- **The review inbox.** `GET /$/memory/{ds}/inbox` lists the facts asserted only in
  agent graphs by session graph, each with the span, link, guard and corroboration
  signals of §8.9 and `passes`, and the open review branches with the facts they
  propose and retract. Facts without a reifier, such as imported harness memory, are
  listed too. `GET /$/memory/{ds}/review/{name}` reviews one branch with its proposed
  facts, retractions, new entities with their candidates, and the text of the cited
  sources. `POST /$/memory/{ds}/promote`, `reject`, `relink` and `edit` are **Promote
  selected**, **Reject**, **Use existing** and **Edit value**, and each writes as the
  caller through `assert_facts` or a SPARQL update (A10, A29).
- **Policy and status.** With `conversationFacts: "review"`, an agent's `assert_facts`
  on `main` runs on `proposals.{agent}.inbox`, which is created when needed, and the
  result carries a `notice` (A31). `recall` on a branch other than `main` reports the
  facts that only the branch asserts as `proposed`.
- **Elicitation.** A client of revision `2026-07-28` that declares elicitation gets an
  `input_required` result from `assert_facts` when every error is
  `possible-duplicate`. The form has one enumeration per new entity with the candidates
  and "a new entity", and the retried call uses the chosen IRI or sets `distinctFrom`
  (A33).
- **Agent workflows.** The `ingest_document` prompt gives the steps of an ingestion on
  a proposal branch, and `consolidate_memory` those of a consolidation pass that
  asserts repeated session facts into the consolidated graph with `derivedFrom` and
  lists duplicates and conflicts for a person. Both stop before the merge.
- **The web UI.** The Memory page has an Inbox tab with the sessions, signals and
  branches, **Accept all that pass** with the corroboration checkbox, **Promote
  selected** into a target graph, which opens the merge page of the new review branch,
  and **Reject selected** with a reason (A32). The review page at
  `/ui/datasets/{name}/review/{branch}` shows the source text with the cited passages
  highlighted, the proposed facts with their signals, the retractions and the new
  entities, with **Use existing**, **Edit value**, **Reject** and **Merge**. An Ingest
  card on the Memory page edits `keepText` and the profiles. The mock server answers
  every new route.
- **Evaluation.** `testsuite/ingest/sample.json` holds 20 short documents about the
  organisation of `testsuite/ask/org.ttl` with 39 gold facts, their quotes and 5 new
  entities. `scripts/eval-ingest` reads the ingest and proposal branches of a server
  through the review route and reports fact precision and recall, linking accuracy,
  the duplicate rate, span failures, guard rejections and tokens per 1,000 characters
  from an agent's log. `--self-test` runs a scripted `oracle` agent, which scores 1 on
  precision, recall and linking, and a `weak` agent that misquotes a quarter of its
  facts, mints a duplicate team and changes some dates. The weak agent scores 0.97
  precision, 0.74 recall, 0.78 linking, 16.7 duplicates per 100 new entities and 23%
  span failures.

**Deviations and additions in Phase 3.**

- Re-ingestion is driven by the agent. The server does not compare two extractions
  itself, because Phase 3 has no extraction in the server. The last `assert_facts` call
  of a re-extraction names the new rendition in `retractStale`, and the server then
  retracts the facts of the graph that cite only earlier renditions and that the call
  does not assert again. A fact that the new extraction cites again stays.
- Review branches are `review.{person}.{yyyymmdd}-{n}`, because branch names cannot
  hold a slash. The person is the caller's user name, or `local` on an open server.
- `promote`, `reject`, `relink` and `edit` are HTTP routes for the UI in Phase 3, which
  pulls `POST /$/memory/{ds}/promote` forward from 3m-b. The `sparkles memory` commands
  `inbox`, `review`, `promote` and `reject` remain in 3m-b and can call these routes.
  Acceptance is the merge itself, so the review page has no accept button per fact, and
  keeping a new entity is the default, so it has no **Keep new**.
- The link signal compares labels exactly or after normalization and needs a matching
  type, as `possible-duplicate` does. The corroboration signal passes when any other
  graph asserts the triple, whatever its source.
- A fact counts as proposed on a branch when the branch asserts it with a reifier that
  `main` does not know, and as retracted when its reifier is invalidated on the branch
  and still live on `main`. Merged branches, with no commit ahead, leave the inbox.
- Facts without a reifier are promoted with a new reifier and no `derivedFrom`.
- The elicitation form titles each candidate with its IRI and the field with the new
  entity's label. Only `assert_facts` elicits, because `register_source` mints no
  entity. Clients of older revisions get `possible-duplicate`, since only `2026-07-28`
  has `input_required`.
- The Ingest section of §7.10 is a card on the Memory page. The upload that the dataset
  page's Ingest section would hold comes with Phase 4.
- The second half of A9, an extraction with structured output that never proposes
  `ex:leads`, needs the extraction of Phase 4.
- The sample holds Markdown and plain text only. HTML and PDF documents join it with
  the conversions of Phase 4, and review effort and cost need a person and a model, so
  the report leaves them out.
- `GET /$/ingest/{ds}/profiles`, `PUT /$/ingest/{ds}/settings`,
  `GET /$/memory/{ds}/review/{name}` and the routes of `reject`, `relink` and `edit` are
  additions to the table of §10.3.

**Tests.** `http/router_tests/mcp/ingest.rs` covers A7 to A11 in
`a7_to_a11_ingest_review_and_reingest`, A29 and the inbox side of A32 in
`a29_a32_inbox_promote_and_reject`, A31 in `a31_review_policy_holds_conversation_facts`,
which also checks the `proposed` status, and A33 in `a33_elicitation_for_possible_duplicates`
with the unit tests of `mcp/elicit.rs`. `ingest_settings_and_read_tools` covers the
settings routes, `keepText` and the read tools, and `ingest_and_consolidation_prompts`
the prompts. `ui/src/lib/review.test.ts` checks the selection of A32 and the span
segments, and `ui/tests/mock/review.spec.ts` runs the inbox, the review page and the
Ingest card against the mock. `ingest_sample_matches_the_demo_data` keeps the sample
consistent with `org.ttl`.

**What 3m-b, Phase 4 and Phase 5 build on.** `register_source` and `list_sources` are
the operations behind `POST` and `GET /{ds}/sources`, and the import graphs' IRIs can
become source IRIs whose digest already matches. Spans, `retractStale` and the
`previousRendition` of a changed text are what re-anchoring needs. The `sparkles memory`
commands `inbox`, `review`, `promote` and `reject` can call the routes of the review
inbox, which already treat imported facts as unreviewed. Phase 4's extraction can fill
an ingest branch through the same `assert_facts` calls as the `ingest_document`
prompt, with `ingest_profile`'s JSON Schema as its structured output, and its results
appear on the review page and in `scripts/eval-ingest` unchanged. Phase 5's maintenance
can run `consolidate_memory` on a schedule and use the inbox signals to rank what it
proposes.

**Phase 3m-b delivered on 2026-10-10.** The prose of imported memory files can become
cited facts, sessions become searchable episodes, a person reviews imported memory from
the terminal, and memory can be exported as a copy and recognized as one when it comes
back. Every test runs on fixture directories against a local server, with no model.

- **Sources over HTTP.** `POST /{ds}/sources` runs `register_source` and `GET
  /{ds}/sources` runs `list_sources`, through the same handler as the other tool routes.
  The POST needs `write` and counts as an update, and the GET needs `read`, counts as a
  query, and takes `graph`, `graphPrefix`, `needsExtraction`, `limit` and `atCommit` as
  query parameters. Both are in the OpenAPI document as `registerSource` and
  `listSources`.
- **`register_source` for the import.** `original` keeps the file's bytes as
  `spk:originalContent` when the text is their normalized form, and their SHA-256
  becomes the digest, so the CLI compares the digest of the bytes it read. `reanchor`
  gives each fact that cites the previous rendition a second reifier at the one place
  its quote occurs in the new text, under an activity of type `spk:Reanchoring`, and
  retracts the others with supersession. `reanchorFrom` copies the facts of an earlier
  source whose quotes occur in the new text, with reifiers derived from the old ones,
  and needs `write` on both graphs (A62, A64).
- **The secret check.** `register_source` runs the built-in patterns and the dataset's
  `secretPatterns` over every text registered in a graph under `imports.base` and
  refuses a match with `422 secret-detected`, the pattern's name and the offset. The
  error never holds the matched text (A66).
- **`needsExtraction`.** A rendition needs extraction until a live reifier cites a span
  of it through an activity that is neither a `spk:Reanchoring` nor associated with an
  agent named `sparkles-import/<harness>`, which is the agent the import's own writes
  name. `list_sources` reports the member for every source, and `graphPrefix` narrows
  the listing to an import base. `sparkles memory sources --needs-extraction` and
  `status` use it, and the skill of §10.4 now names `graphPrefix` and the `/sessions/`
  graphs it skips.
- **Spans on structural facts.** A sync registers each file's redacted text and gives
  every structural fact a span over the line it came from, so the memory browser shows
  the text and the passage of each imported fact. An edit registers the new text with
  `reanchor`, then writes the structural diff. A rename registers the new file with
  `reanchorFrom` the old graph before the old graph is retracted.
- **Transcripts.** With `imports.transcripts` and the person's opt-in, from
  `--transcripts` or a `transcripts` list of project keys in `memory.toml`, a sync reads
  Claude Code's and Codex's session logs for the project. Each session is rendered as
  Markdown with one heading per turn and registered in `…/sessions/<id>` with the
  format `text/markdown;profile=transcript`, so every turn starts a chunk. A line of the
  conversation that starts with `#` is escaped, so only turn headings are headings. The
  graph describes the `mem:Session` with its id, branch, model, harness version and
  times. `--transcript-content tools`, `--subagents`, `--since`,
  `--max-transcript-bytes`, the 10-minute rule for sessions in progress, the 8 parts
  and the cleanup warning of `status` work as §8.10.7 describes. `setup --transcripts`
  adds `--transcripts` to the session end hook (A67).
- **Review from the terminal.** `inbox` prints the review inbox with a 10-hex-digit id
  per fact, the SHA-256 of its graph, subject, predicate and object, and filters by
  agent, kind, harness and project. `promote` calls `POST /$/memory/{ds}/promote`,
  prints F09's merge preview of the new branch and, with `--merge`, merges with the
  preview's heads as `expect`. `reject` calls `POST /$/memory/{ds}/reject` with the
  message. `review` reads one choice per fact from standard input, `p`, `r`, `s`, `o`
  or `q`, and `o` prints the inbox's URL in the web UI (A76).
- **Export.** `export --sources` writes byte-identical copies to the same harness from
  `spk:originalContent` or the rendition, flags redacted files, converts Claude Code
  memory to one Codex `AGENTS.md` or to generic frontmatter files, renames instruction
  files for the target, and refuses unsafe stored paths, two sources that export to
  one path, and existing files without `--force` (A75).
- **`mem:copyOf`.** The adapters read leading `<!-- sparkles:copy-of <iri> exported
  DATE -->` lines, at the top of a file or right after its frontmatter, and a sync
  also records `mem:copyOf` for a new file whose digest equals a live source in another
  graph. The brief and the inbox's corroboration signal count a copy and its original
  as one source.

**Deviations and additions in Phase 3m-b.**

- An edited file costs two commits instead of one. The first registers the new text
  with `reanchor`, and the second writes the structural diff through `assert_facts`.
  `register_source` owns the source's description, so the diff leaves out
  `spk:contentDigest`, `spk:rendition`, `spk:originalContent`, the title and the format.
- Re-anchoring covers the import's structural facts too, because they now cite spans.
  The `reanchored` count of a sync therefore includes them, and the structural diff
  then supersedes the values that changed.
- A transcript longer than one source is split into sources of their own in the same
  session graph, `…/sessions/<id>` for the first part and `…/part-N` after it, because
  one source has one current rendition. Each part is at most 2 MiB less 4 KiB, which
  leaves room for the note that ends the last part.
- The link from a memory file to the session that wrote it is the triple `<memory
  graph> prov:wasGeneratedBy <session>`, written in the session's graph rather than on
  the memory's reifiers. A sync of the memory file then never has to know about the
  session, and the link survives later edits of the file.
- `promote` by a principal that may not create `review.{person}.{date}-{n}` branches,
  such as an agent under the template of §8.6, falls back to
  `proposals.{agent}.review-{date}-{n}`. The promotion then succeeds on a branch the
  agent may write, and the merge fails with `forbidden` as A30 requires.
- An export to Codex writes one `AGENTS.md` that holds the project's instruction text
  first and then the memory sections, instead of an `AGENTS.md` fragment next to an
  instruction file of the same name. A rule's `paths` frontmatter is left out, with a
  warning, for a target other than Claude Code.
- `copyOf` is a structural predicate that the structural diff never retracts, so an
  edit that removes the copy comment keeps the link to the original.
- `inbox` lists at most 500 items, the limit of the inbox route.
- `reject` calls `POST /$/memory/{ds}/reject` of Phase 3 rather than `POST /{ds}/facts`,
  so the terminal and the inbox's **Reject selected** write the same commit message.
- With `imports.extract: "server"`, the import reports the setting and starts no
  extraction. Extraction inside the server is Phase 4's. Phase 5 builds it: a sync now
  starts a server extraction for each changed memory file, as recorded below.

**Tests.** `http/router_tests/mcp/ingest.rs` covers the sources routes in
`a62_a64_a66_sources_routes`: the secret check with a built-in and a custom pattern,
redacted text passing, a graph outside the base not checked, `original` with its digest
and a bad original, `needsExtraction` before and after an extraction, re-anchoring,
`reanchorFrom`, and the grants of a reader. `crates/sparkles-server/tests/cli_memory.rs`
covers A62, A63 and A64 with prose facts asserted through `assert --file`, re-anchored
on an edit, carried by a rename and gone with the file, and
`memory_transcripts_review_and_export` covers A66, A67, A75 and A76 with the double
opt-in, the redaction of a token, the content left out, parts, `--since`, the link to a
memory file, the export to the same harness, to Codex and its re-import as a copy, and
promotion and rejection with the commit message checked. `mcp/import_tests.rs` covers
the copy rule of A70 in `brief_copy_does_not_corroborate`, and the transcript parsers
and renderer have unit tests in `memory_cmd/transcripts.rs`.

**What Phase 5 builds on.** Every imported fact cites a span, so maintenance can find a
fact's passage and its rendition history without reading files. `needsExtraction` and
the `sparkles-import/` agent name tell extraction work apart from the import's own, and
session graphs are episodes with turn chunks that `consolidate_memory` can cite.
`mem:copyOf` and the corroboration rule of the brief and the inbox keep exports from
inflating the evidence that consolidation ranks by. `inbox --json` gives stable item ids
that a scheduled job can promote or reject.

**Phase 4 delivered on 2026-10-10.** A person uploads a document in the UI, or posts it
to the server, and reviews the facts the server proposes on a branch without an agent.
Every test runs against mock providers, and Text2KGBench has run only with the script's
own small benchmark and mock models, because a real run needs the operator's keys.

- **Conversion.** The `ingest` module converts plain text and Markdown as they are,
  HTML to Markdown from its first `<main>` or `<article>` or its body without scripts,
  navigation and forms, and PDF to Markdown with pdf-inspector 1.25.2 (MIT), pinned
  exactly, behind the `pdf` feature, which is on by default. Each PDF page starts with a
  `<!-- Page N -->` marker, and `register_converted`, an internal variant of
  `register_source`, writes the page starts as `spk:pageStart`, OCR pages as
  `spk:ocrPage` and left-out pages as `spk:omittedPage` on the rendition. A page that
  pdf-inspector classifies as needing OCR fails the task with `needs-ocr` and every page
  with its reasons, unless `allowPartial` registers the other pages (A54, A55). The
  `pdf-ocr` feature, off by default, turns on pdf-inspector's OCR with the models in
  `--pdf-ocr-models`, PDFium from `--pdfium-lib` and ONNX Runtime loaded at run time
  from `--onnxruntime-lib`. It never downloads a model. `--pdf-workers` bounds the
  conversions that run at once. Conversion is deterministic, so the same PDF gives the
  same digest and rendition IRI (A56).
- **Tasks.** `POST /$/ingest/{ds}` takes a multipart upload, JSON text or a URL fetched
  through the outbound policy, and answers `202` with a task. `GET /$/ingest/{ds}` lists
  the caller's tasks and the server's capabilities, `GET /$/ingest/{ds}/{task}` with
  `?wait=` reports the status, progress, estimate, usage and result, and `DELETE`
  cancels. A task is visible to the principal that started it and to dataset admins.
  Finished tasks stay for seven days, at most 500 are kept and 32 run at once.
- **Extraction.** The task reads the ingest profile, estimates the tokens and cost of
  the extraction from the chunks and the first pair's pricing, and waits for
  `POST …/confirm` above the dataset's `confirmTokens`, 200,000 by default. Each chunk
  is one call of the `extract` role with the 400 characters before it as context and a
  strict schema whose class and predicate members are the profile's enumerations, so a
  predicate outside the profile is never proposed (A9). A chunk whose call fails moves
  to the next pair of the role's list, and the usage records the escalation. The
  model's mentions are de-duplicated by name and class and linked with
  `link_entities`. An exact match becomes the existing IRI, an ambiguous one a new
  entity with its candidates, and the rest new entities. The facts are written with
  `assert_facts` in batches with an idempotency key, and a dry run first drops the facts
  that would fail and sets `distinctFrom` on possible duplicates.
- **Review modes.** `branch` writes on `ingest.<slug>-<n>`, `preview` keeps the
  proposals in the task until `POST …/approve` writes them, and `auto` merges into
  `main` when every fact passed, no entity is ambiguous, the guard found nothing and
  every confidence reaches the dataset's `autoConfidence`, 0.8 by default. Otherwise it
  leaves the branch and says why in `autoFallback`. The ingest settings gain
  `confirmTokens` and `autoConfidence`.
- **Tables.** A CSV or TSV file gets a CSVW mapping draft from one model call with the
  header and 20 sample rows, checked by the C05 parser and converted in full for the
  row and triple counts, with the triples of the first 100 rows. Nothing is written,
  and the UI hands the mapping to `POST /{ds}/upload` with a dry run first (A18).
  Without a model the draft is the default mapping.
- **CLI.** `sparkles ingest --loc DB DATASET FILE…` runs the same pipeline on a
  database, file by file, with `--pair` instead of the dataset's list, and prints text
  or JSON.
- **UI.** The dataset page has an Ingest section that uploads a document with its mode,
  profile and options, shows the task's status and estimate, asks for the confirmation
  or the approval, lists the pages that need OCR with **Ingest the readable pages**, and
  links to the review page. A table's draft is shown with its counts and goes to the
  Upload section. The review page shows the page of each cited fact and the pages left
  out or read by OCR.
- **Evaluation.** `scripts/eval-text2kg` loads each Text2KGBench ontology as classes and
  properties with an ingest profile, ingests every test sentence through the server,
  and reports Text2KGBench's precision, recall, F1, ontology conformance and
  hallucination with tokens and cost per sentence. Its self-test scores an oracle mock
  model at 1 and a weak one at 0.5 precision and 0.25 recall on a benchmark of four
  sentences.

**Deviations and additions in Phase 4.**

- The model gives each fact's quote, and the server finds the quote in the chunk, first
  exactly and then with whitespace folded, to compute the span. A fact whose quote is
  not in the text is dropped with `span-mismatch` before any write.
- The extraction schema is a strict variant of `ingest_profile`'s, with every member
  required and empty strings for absent ones, because the strict mode of the `openai`
  kind accepts no optional member, as for `Draft` in Phase 1.
- Ingestion keeps its own task registry with confirmation and approval states rather
  than the MCP tasks extension, because a task waits for a person.
- The routes need `read` on the dataset, and every write goes through the tools as the
  caller, which enforce the write grants. `auto` needs `admin`.
- A CSV draft mints IRIs for the values of IRI columns under `base` and does not link
  them to existing entities.
- The Ingest section is on the dataset page next to Upload, and the Ingest card of the
  Memory page keeps the settings.
- An import with `imports.extract: "server"` still only reports the setting. The
  pipeline extracts from a document it registers itself and stops with
  `already-registered` for a text whose rendition exists, so the import's sources need
  an entry point that extracts from an existing rendition. Phase 5 builds that entry
  point, `source` on `POST /$/ingest/{ds}`, and the sync that calls it.
- `sparkles serve` panicked in debug builds when two flattened argument structs shared
  the name `ServeArgs`. The ingestion flags are now `IngestServeArgs`.

**Tests.** `crates/sparkles-server/src/ingest/tests.rs` runs the routes against mock
providers. `a54_pdf_pages_and_spans`, `a55_needs_ocr` and
`a56_conversion_is_deterministic` cover A54 to A56 with PDFs written by
`ingest/fixtures.rs`, and `a56_without_pdf_feature` the refusal without `pdf`.
`a9_profile_enum_and_escalation` covers the second half of A9 and escalation, and
`a18_csv_mapping_draft` covers A18 with 10,000 rows, at most two model requests and a
dry-run upload of 20,000 triples. `markdown_without_a_provider_registers_on_a_branch`,
`html_upload_keeps_the_main_content`, `confirmation_and_cancel`,
`preview_approve_and_auto` and `ocr_without_models_fails` cover the rest. The converters
and the draft have unit tests. `ui/tests/mock/ingest.spec.ts` runs the Ingest section
and the review page's pages against the mock server, and `ui/src/lib/review.test.ts`
checks the page of an offset.

**What Phase 5 builds on.** The task registry of `ingest::Runtime`, with its progress,
usage, confirmation and cancellation, can run consolidation as a server task. The
extraction's batching, idempotency keys and dry-run fixes are the pattern for writing
consolidated facts, and the `auto` checks are a model for a maintenance task that
merges only what passes.

**Phase 2b delivered on 2026-10-10.** A person sees what any query asks and why it is
slow, with each sentence and note pointing at the operator it is about, with or without
a model. Every test runs against mock providers.

**Plan fidelity (§6.6.5).** The engine's plans now match what runs.

1. `PlanInfo` and `CursorPlan` write an `id` on every node, the path of child indexes,
   computed when the plan is serialized. `/{ds}/explain` wraps an `ASK` in its `Slice`
   and gives `CONSTRUCT` and `DESCRIBE` their root, as execution does. A cache hit keeps
   the subtree it was computed from, each node `skipped`. `IndexTopK` always lists its
   fallback, skipped when the ordered index answered. `SpatialKnn` adds its batches into
   one child. A streamed `CountJoinFromRuns` lists the scans it reads. A test checks
   that the estimated and executed shapes agree for each query form.
2. A node that never ran says why in `skipped`. That covers both sides of a join whose
   other side is empty, `CountJoin`, `Union` branches after a `LIMIT` was met, the
   vector search and top-k fallbacks, and a nearest-neighbour batch no candidate needed.
3. Under `LIMIT`, `ASK` and `EXISTS`, the children add up their rounds with
   `PlanInfo::merge_run`, and the node reports `runs` and `stoppedEarly`. An `IndexJoin`
   inside a pipeline keeps its note and counters.
4. The streaming executor labels a merge join that runs as a hash join `HashJoin`, with
   a note. A fallback subtree reports the rows it emitted, with the table's size in the
   `materializedRows` counter. A node that a `LIMIT` stopped in a completed query is
   `stoppedEarly`, the plan refreshes descriptions and counters as it runs, and the
   error path of a shared pull refreshes the plan.
5. The eager `CONSTRUCT` and `DESCRIBE` stage has a timed root node. Filters pushed into
   a range scan or an index join are listed in `pushedFilters`, and `SERVICE`'s
   constant estimate carries `estimateGuessed`.
6. A query stopped by a budget or its timeout keeps its plan. `sparql::query_with_plan`
   and `execute_query_with_plan` return a `Failed` with the error and the plan as it
   stood, whose unfinished nodes have `complete: false`. Each failing node records its
   partial plan in the execution context, and its parent places it. The error body of
   the `application/x-sparkles+json` format carries `plan` and `commit` on both the eager
   and the streaming path.
7. `explain_query` already showed a hidden estimate as `est=?` and `null`. Its node facts
   now report `estimatedRows: null`, and a `hidden-estimates` note says why. `PlanView`
   no longer counts a hidden `-1` as a misestimate and reads `CursorPlan` trees.
8. The new facts cost the operators nothing measurable. They live in a boxed
   `PlanFidelity` that most nodes leave empty, so `PlanInfo` grows by one pointer, and
   the failure hook is a drop guard that only acts when an operator returns an error.
   A first version that kept them inline and wrapped every operator's result ran about
   2,500 more instructions per execution of the smallest benchmark queries, which was
   34% more in `exec::execute` for `distinct-obj` and 15 to 20% of its time. In callgrind counts against the main branch, the final
   version runs 2.2% more instructions in `exec::execute` for `distinct-obj`, 1.8% for
   `predicate-counts` and the same count for `path-plus`.

- **Notes and the description.** The server's `explain` module reads a `PlanNode` or
  `CursorPlan` tree as node facts and computes the notes of §6.6.2 with a severity, the
  budget first and then by share of time. Lint findings of X04 are placed on the node
  whose columns hold the variables they name, or carry their editor range. The template
  description writes one to four sentences from the scans, paths, joins, optional
  parts, filters, exclusions, grouping, order, distinct and limit, each citing its
  nodes.
- **The explain role.** The prompt holds the query, one line per node with its id and
  figures, the notes and the template text, and no rows. The answer's sentences and
  notes must cite nodes of the plan, and a note's numbers must be among its node's facts
  within rounding, or the note keeps the computed text. When no sentence survives, the
  template is shown with the reason. The calls are charged to the caller and the dataset
  and counted under the role `explain` in `GET /$/models/usage`.
- **The endpoint.** `POST /{ds}/sparql/explain` runs as the caller with the HTTP tools'
  limits, needs `read` and counts as a query. It streams `plan`, `notes`, `explanation`,
  `usage` and `error`, or answers one object for `Accept: application/json`.
- **MCP.** `explain_query` without the new arguments answers as before. `profile: "run"`
  runs read-only under `timeoutSeconds` and explains a stopped run from its partial
  plan. `notes: true` adds `nodes`, `notes`, `asks` and `source`, and starts each plan
  line with the node id. `useServerModel: true` needs the new `serverModels` permission,
  a grant flag `server_models` that admins of a dataset and the local principal also
  have, and fails with `server-model-not-allowed` before any call otherwise.
- **UI.** `PlanView` uses the server's ids and shows hidden estimates, skipped and
  stopped nodes, reruns, materializing streamed operators, partial counts with their
  legend, and the notes' severity marks. The **Explanation** switch opens the panel
  beside the tree for an estimated plan or a run's plan, with links that select,
  expand and scroll to a node, hover highlighting in both directions, and **Show
  template text** for model text. A failed query whose error carries a plan offers
  **Explain why**, which opens that plan with the panel.

**Deviations and additions of Phase 2b.**

- `EXISTS` bodies, the inner plans of `Lateral`, `REDUCED` and the remote side of
  `SERVICE` still have no node of their own. A filter's counters describe its `EXISTS`
  test, `REDUCED` does no work, and the remote side runs elsewhere.
- The template names terms by their local names and the dataset's prefixes rather than
  by the labels of the terms list, so it reads "?p memberOf ?team" rather than "people
  who are members of".
- Schema findings of `check_query` are not added as notes. The planner's warnings and
  the lint findings are.
- After a row or memory budget, `dominant` names the node with the most rows, because
  the plan carries no estimated memory per node.
- `POST /{ds}/sparql/explain` takes an `error` member with `profile: "given"`, the error
  body of the stopped run, so the server knows the budget. It also takes
  `timeoutSeconds` for a run.
- The MCP result writes the description's origin as a top-level `source` beside `asks`,
  with `provider`, `model` and `usage` after a model call. `useServerModel` implies
  `notes`.
- The UI asks about the plan it shows with `profile: "given"`, so the tree and the notes
  always agree. The panel's ending with **Optimize** and the C01 limits to raise is
  left to Phase 6.
- A44's dominant hash join is tested with a given plan, since the timing of a real run
  is not deterministic. Its run part checks ids, notes and the template on a real run.
  A46 uses a timeout of 0.3 seconds to keep the test short.

**Tests.** `crates/sparkles-core/src/sparql/plan_fidelity_tests.rs` covers ids, skipped
nodes, reruns, cache hits, the shapes of each query form, pushed filters, failed
queries and streamed plans. `crates/sparkles-server/src/explain/tests.rs` covers the
notes, the template, partial counts, hidden estimates, streamed plans and the model
checks. `crates/sparkles-server/src/explain/http_tests.rs` covers A44 in
`a44_a_run_is_explained_without_a_model`, A45 in
`a45_model_text_is_checked_against_the_plan`, A46 in
`a46_a_timeout_is_explained_from_its_partial_plan`, A47 in
`a47_explain_query_with_notes_and_a_run` and A57 in `a57_server_models_need_the_grant`.
`ui/tests/mock/explain.spec.ts` covers **Explain why** and A48, and
`ui/src/lib/explain.test.ts` the plan reading and marks.

**What the plan cache and Phase 6 build on.** Node ids are computed from the tree's
shape when a plan is serialized, so a cached plan needs no stored ids. The partial plan
of a failure lives in the execution context (`Ctx::failed`), which belongs to one
execution. `exec::describe` redacts for the context's graph view, so a cached
description must be redacted per view. Phase 6 can read an executed plan with
`explain::Plan::read`, pair the timings of two plans by node id when their shapes
agree, use the `dominant` and `misestimate` notes to choose rewrites, and check model
text with the same `grounded` test. `optimize_query`'s `useServerModel` can use the
`serverModels` permission as it is.

**Phase 5 delivered on 2026-10-10.** Memory that many sessions write is consolidated by
a server task, ranked by recency on request, and trimmed by an optional retention of old
session graphs. The import's `imports.extract: "server"` setting, left open by 3m-b and
Phase 4, now starts extractions in the server. Every test runs against mock providers or
none.

- **Consolidation.** `POST /$/memory/{ds}/consolidate` starts a task in the ingest
  runtime and answers `202` with a `Location` under `/$/ingest/{ds}/{task}`. The task
  reads every agent graph through the internal `memory_consolidation_scan` tool. It
  groups the facts by triple, counts distinct sources with the copy rule of the brief,
  and keeps the facts that at least `minSources` graphs assert, 2 by default, and that
  no reviewed graph asserts yet. Structural facts of the `mem:`, `spk:` and `prov:`
  vocabularies are left out. It lists duplicate entities through `link_entities` and
  conflicts through `recall`, and writes the repeated facts into the dataset's
  `consolidatedGraph` on `consolidation.{date}-{n}` with `assert_facts`. Each fact has a
  reifier whose `prov:wasDerivedFrom` lists the session reifiers, up to 20. The writes
  go in batches of 200 with an idempotency key and up to three dry runs that drop the
  facts that would fail. In `auto` mode, which needs `admin`, the branch is merged when
  every fact passed, with the same checks as ingestion's `auto`. An agent under the
  template of §8.6 writes on `proposals.{agent}.consolidate-{date}-{n}` instead. The
  inbox lists the branch with the kind `consolidation`.
- **Recency.** `recall` takes `recency`, a half-life such as `"90d"`. The found seeds,
  at most four times the limit and at least 20, are multiplied by
  `0.5 ^ (age / halfLife)` and by `1 + log2(sources)`, where `sources` counts the graphs
  that assert one of the seed's facts, with a copy counted once.
- **Retention.** `POST /$/memory/{ds}/retention` needs `admin` and deletes, with one
  `DROP GRAPH` commit each, the agent graphs that match the retention's patterns, whose
  newest recorded time is older than `after`, and, with `requireConsolidated`, whose
  facts all appear in a reviewed graph. Without patterns it covers graphs whose IRI
  contains `/sessions/`. A dry run lists each graph with the reason it stays.
- **Settings and schedule.** `memory.json` gains `consolidation` with `every`, `mode`
  and `minSources`, and `retention` with `after`, `graphs`, `requireConsolidated` and
  `every`. A schedule checks every minute, starts the passes that are due as the server
  itself, and records the last run of each in `maintenance.json` beside the dataset.
  `GET /$/memory/{ds}/maintenance` reports the settings, the last task and the next run.
  Both are off until the settings name them.
- **Extraction of a registered source.** `POST /$/ingest/{ds}` takes `source`, the IRI of
  a registered source or its graph, instead of a file, text or URL. The task reads the
  source's live rendition with `read_chunks` and runs the `extract` role on it. The new
  `memory` mode writes the facts on `main` into the source's own graph, which must be an
  agent graph, so they are unreviewed memory like the import's other facts. `sparkles
  memory sync`, when the dataset's `imports.extract` or `--extract` is `server`, starts
  one such task for each new, edited or renamed memory file that needs extraction and
  reports the tasks under `extractions`. With `--loc` it waits for each task.

**Deviations and additions in Phase 5.**

- Consolidation calls no model. Repeated facts are found by their triples, duplicates
  by `link_entities` and conflicts by `recall`, so a pass costs no tokens.
- A pass reads every agent graph and leaves out the facts a reviewed graph asserts,
  rather than reading only the session graphs written since the last pass. The result
  is the same after a merge, and a rejected proposal is proposed again by the next pass.
- A scheduled pass writes nothing while an earlier `consolidation.*` branch waits for
  review, and reports `pending-review`.
- Scheduled tasks run as the server's local principal, because no person starts them.
- The age that `recency` uses is the newest `prov:generatedAtTime` among the live
  reifiers of the seed's facts, rather than among the reifiers of the facts that
  matched, because a search hit names an entity more often than a fact. A seed with no
  time has age 0.
- Retention keeps graphs with no recorded time and graphs with more than 5,000 facts,
  and it reads only agent graphs, so a curated graph is never deleted.
- The sync waits for its extractions only with `--loc`. Against a server it reports the
  task ids and returns.
- There was no command and no UI for consolidation or retention when Phase 5 landed.
  The inbox shows the consolidation branch, and the routes and settings covered the
  rest. Both gaps were closed later on 2026-10-10 by the work on
  [C19](../C19-layered-settings.md). `sparkles memory consolidate` and
  `sparkles memory retention` start a pass, wait for its task and print its result,
  with `--dry-run`, `--no-wait` and the members of the route bodies as flags.
  `sparkles memory maintenance` prints the settings, the last scheduled run of each pass
  with the outcome of its task, and the next run. The schedules are memory settings,
  which `sparkles settings set` changes. `crates/sparkles-server/tests/cli_settings.rs`
  covers the commands in `memory_init_and_maintenance`. In the UI, the memory section of
  the dataset page's Settings tab shows each job's last run, its outcome and the next
  scheduled run, and offers Run now to dataset admins, with a dry run and a confirmation
  before retention deletes anything.

**Tests.** `crates/sparkles-server/src/ingest/tests.rs` covers A78 in
`consolidation_task`, with the dry run, the branch and its inbox kind, a scheduled pass
that waits, `auto`, an empty next pass and bad settings, A80 in `retention_task`, with
a missing setting, the kept and deleted graphs, a dry run, a scheduled pass and a bad
`after`, and the server half of A81 in `extraction_of_a_registered_source`, with
`memory` mode, `not-agent-memory`, `branch` mode on any source, `unknown-source` and the
argument checks. `mcp/c18_tests.rs` covers A79 in `recall_recency`, and
`crates/sparkles-server/tests/cli_memory.rs` covers the sync half of A81 in
`memory_loc`, where an edited memory file starts one extraction that fails with
`no-model` because the fixture has no provider.

**Review signals, added on 2026-10-10.** A forgotten review stalled maintenance without
a sign: a scheduled consolidation only reported `pending-review`, and retention with
`requireConsolidated` kept every unreviewed session graph. The server now counts what
waits in each inbox and reports it.

- **Counts.** `mcp/memory/pending.rs` counts, as the server and without the inbox's
  signals, the unreviewed facts of kinds `session` and `import` (up to 5,000 per
  dataset) and the open review branches by their inbox kind, each with the time of its
  oldest item. A fact's time is its reifier's, a branch's is its creation, and a fact
  without a time dates from the first count of its graph, kept in `<db>/review.json`.
  Each count is kept per dataset with a fingerprint of the head commit, the review
  branches and the memory settings. The maintenance tick refreshes every dataset with
  memory settings once a minute, on a read-only server too, and the tools and routes
  that change an inbox refresh their dataset on a thread of their own. A refresh with
  an unchanged fingerprint reads nothing. Refreshes of one dataset serialize their
  settings reads, counts and cache publication, so an older refresh cannot overwrite
  a completed newer one. Other datasets count independently. Workers recheck the
  current dataset instance and resolve replacements on each coalesced refresh.
  A replacement discards the old instance's cached counts before counting, including
  when the new count fails. Obsolete cache pruning rechecks live names under the same
  gate. Metrics remain an eventual cached view and do not acquire a count gate or run
  queries.
- **Metrics.** `sparkles_memory_review_pending{dataset,kind}` and
  `sparkles_memory_review_oldest_seconds{dataset,kind}` report every kind of each
  dataset with memory settings from the kept counts, with no query at scrape time. The
  JSON snapshot carries them as `memoryReview`, and OTel exports them as
  `sparkles.memory.review.pending` and `sparkles.memory.review.oldest`.
- **The maintenance answer.** `GET /$/memory/{ds}/maintenance` has a `review` member
  for a caller with `write` on the dataset, with the open items, the oldest, the kinds
  and the open branches the caller may read. `sparkles memory maintenance` prints it.
- **UI.** The sidebar's Memory entry and the dataset page show a count badge, read from
  that member, whose accessible name gives the count and the age of the oldest item.
  The dataset page's badge opens the Inbox tab.
- **Agents.** The brief has a `review` member and a `# review:` line per open review
  branch and per kind of unreviewed fact, such as `# review: 3 consolidated facts on
  branch consolidation.2026-10-10-1 await review since …`, for a caller with `write`.
  MCP lists the resource `sparkles://{ds}/memory/review` to such a caller, and a
  subscription to it gets `notifications/resources/updated` when a count or an oldest
  time changes.

The counts are not the inbox. They skip its signals, and a branch counts once whatever
it proposes, as in the inbox's `open`. The signals go to callers with `write` on the
dataset, a coarser rule than the grants on graphs and branches that the inbox actions
check.

**Tests.** `ingest/tests.rs` covers the counts in `review_signals`: no series before
memory settings, the session facts, the consolidation branch after a pass with its
fact count in the maintenance answer and the brief, and an empty inbox after the merge,
in the Prometheus text and the JSON snapshot. `ui/src/lib/review-count.test.ts` covers
the badge's words and the shared count store, and `ui/tests/e2e/pages.spec.ts` the
badges on the dataset page and the sidebar.

**Roadmap item 15 partly delivered on 2026-10-13.** Office documents, the transcript
adapters and CSV entity linking shipped, in the order of the item's bullets. Images and
audio and video remain, and so do the follow-ups listed at the end of this section.

- **Transcript adapters.** The transcript parsers moved from the server's
  `memory_cmd/transcripts.rs` into `sparkles_memory_import::transcripts`, one module for
  each harness with Claude Code and Codex among them. Gemini CLI, Cursor, GitHub
  Copilot's CLI and Aider each have a module that finds the logs of a project and parses
  them into the same sessions of turns, so redaction, the `## user` and `## assistant`
  rendering, the size limits, the `--since` and in-progress rules, the opt-in of §8.10.7
  and the graph layout apply to them unchanged. §8.10.1 documents each location and
  format. The generic adapter reads any other file given with `--transcript FILE`, which
  is no longer a hidden flag, when no specific adapter recognizes it. The adapter names
  `gemini-cli`, `cursor`, `copilot` and `aider` select transcripts only, because their
  instruction files already belong to the generic adapter. `mem:Copilot` and `mem:Aider`
  join the harnesses of the vocabulary. Gemini CLI, Cursor and Copilot do not publish the
  formats of their logs, so the adapters follow what the tools write and what other
  readers of them accept. Each skips a line or a block it does not understand. Cursor's
  SQLite chat store and Copilot Chat's editor storage are not read. Aider's log has no
  session id, so its session is named after the file and a short hash of the file's path
  below the repository, which keeps the histories of a project's directory and root
  apart. The log does not mark which lines the person wrote, so a reply with a level-4
  heading is read as a turn of the person and a reply line that starts with `> ` is read as
  a notice. Cursor's text form starts a turn at any line that begins with `A:` or `user:`.
  These are limits of the formats.
- **CSV entity linking.** A model-drafted mapping now links its IRI columns as §7.8
  describes. `sparkles::tabular` gained the lookup extension `urn:x-sparkles:lookup` on a
  column (C05 §3.7), a list of `{ "rdf:value": value or values, "@id": IRI }` entries
  that is valid CSVW and that other processors ignore, so the CLI converts with the same
  links and other tools mint the `valueUrl` IRIs. The converter's new `tally` option
  records the exact strings that the lookup is compared with. The draft's column schema
  gained `linkClass`, `Vocabulary` now reads the profile's `ranges`, and the result has a
  `links` array with, for each column, the classes, the commit, the distinct and tried
  values, how many were linked and minted, how many had only weak candidates and the
  ambiguous values with their candidates. The decision to keep the lookup in the mapping
  and not in the dataset keeps C05's rule that a conversion reads nothing from the
  target. `link_entities` gained `vectors`, which defaults to true, because the values of
  a whole column are far more text than the 20 sample rows the model sees. The review
  UI lists the ambiguous values and writes the candidate a person picks into the lookup.

- **Office documents.** The new crate `sparkles-office` reads DOCX, PPTX, XLSX and the
  OpenDocument text, presentation and spreadsheet formats from their XML parts with
  `zip` and `quick-xml`, both MIT-licensed and already in the dependency tree. The
  server uses it behind the `office` feature, which is on by default. A Word or ODT
  document becomes Markdown with headings from the paragraph styles and outline levels,
  lists with their numbering, tables, hyperlinks and footnotes. A presentation gets a
  `<!-- Page N -->` marker and a `# Slide N: Title` heading for each slide, in the order
  of the presentation, and the speaker notes follow the slide's text under `Notes:`.
  Hidden slides are left out and keep their numbers. The marker is the page marker of
  §7.1.1, so `spk:pageStart`, the review page and `factsByPage` work for slides without
  a change. A line of document text that starts a comment is escaped, so a document
  cannot forge a marker.
- **Spreadsheets.** An XLSX or ODS upload is not converted to text. The server reads
  each visible sheet as CSV and drafts a mapping for each one like an uploaded table
  named `<workbook>-<sheet>.csv`. The result has the outcome `mapping-drafts`, with one
  entry per sheet that holds the sheet's name and index, its draft and its CSV, which is
  the file to upload with the mapping. The sheet's name and index are the marker of that
  sheet. Cells with a date format become ISO 8601 text, and formulas give their cached
  values. A sheet's rows get a namespace of their own that includes its position in the
  workbook, because names that slug alike or are cut at 40 characters would otherwise
  share one. At most 20 sheets are drafted, each draft being a model call, and a note
  names how many were left out. Hidden sheets and sheets without data are left out, but hidden rows and
  columns of a visible sheet are read, because they are data. Hidden slides and Word runs
  marked as hidden text are left out as well. The comment of an ODS cell is not read into
  its value. A sheet whose draft fails is reported with its error, and the others go on.
- **Bounds and isolation.** The converter bounds the entries of the container, the
  unpacked size of each part and of all parts, the nodes and depth of an XML tree, the
  cells of a sheet and the size of the text. A ZIP bomb stops at the limit whatever its
  headers claim, and a DOCTYPE is refused. A review of the first version found small
  files that asked for gigabytes anyway, such as a Word list level of 4e10, one shared
  string used by thousands of cells, a row repeated 1,000 times, or a table row whose
  `gridSpan` pads every other row. A second review found notes and text boxes whose
  paragraphs were collected once for every level of nesting, and ODT notes inside notes
  that took exponential time. One output budget, `Limits::max_output`, which is 128 MiB
  by default, now covers everything a conversion holds besides its input and XML trees,
  which is the Markdown blocks and table slots, the sparse rows of a sheet, the shared
  strings, the text of notes and the CSV of all sheets together. The paths that can
  multiply the size of the input charge the budget before they allocate. Those are
  repeated and padded cells, table slots, shared strings and the spaces of `text:s`. Each
  paragraph of a note, a text box or a notes shape is read once however deeply its
  elements nest. A note inside a note is left out. A third review found values that are
  as long as a part and are copied, hashed or scanned once per reference to them. One
  20 MiB relationship target named by 1,000 hyperlinks became 20 GB of links, one 12 MiB
  number format named by 400,000 cell formats took hours to scan, and a style id based
  on itself was hashed nine times per paragraph. A relationship target longer than 2,048
  bytes and a style id, style name or relationship id longer than 256 bytes are now
  dropped where they are read. A number format code longer than 512 bytes is not a date,
  and each code is read once. Links are charged to the budget, and all links together
  may not exceed the text limit, because every link ends up in the text. A link inside a
  link is read as its text, the notes of a slide are found in one walk, and the cells of
  a row in descending column order are sorted once instead of inserted one by one. Text
  and CDATA children count as nodes of an XML tree. The conversion fails with
  `too-large` when the budget is gone. The tradeoff is memory for safety. A conversion
  may hold up to 128 MiB of output beside its input and trees, and the budget charges 32
  bytes for each stored value while a sparse sheet row costs the process about six times
  its charge. At most `--pdf-workers` conversions run at once. Sheets are stored
  sparsely, a repeated row is stored once, and the rendering of a table is checked
  against the text limit after every row. Each XML attribute counts as a node, and an
  element with more than 1,024 attributes is refused. The conversion also stops at the
  task's deadline. The readers read the clock every 16 steps of a walk and on every
  charge of 64 KiB or more, instead of every 1,024 calls, so the overshoot past the
  deadline is a bounded amount of work. The converter is not registered with the
  server's query memory pool, which stays part of roadmap item 15's remaining work.
- **Child processes.** These bounds come from three rounds of review and from tests,
  and a fourth review could find another path. So the server now runs each PDF and
  Office conversion on Linux in a child process, as §7.1.1 describes, under an
  address-space limit of `--convert-memory` MiB, 2,048 by default, a CPU-time limit and a
  file-size limit of zero unless OCR is configured. The server kills the child 250 ms
  after the task's deadline, so the worker slot is released and the address space is
  bounded whatever a converter does. Before, a PDF conversion that overran its deadline
  kept its slot until pdf-inspector returned, which could be never. The conversion runs
  on a thread as before in library use and the unit tests of the server crate, with
  `--convert-in-process`, and on macOS, Windows and other platforms. macOS accepts
  `RLIMIT_AS` without enforcing it, so a child there would promise a bound it does not
  keep. A review of the child found that it inherited the server's environment, with its
  provider keys, that a child unable to set its limits failed every conversion with a
  generic error and no log line, and that a binary replaced in place would have answered
  in another protocol. The child's environment is now an allowlist, the server probes the
  child at start and logs and refuses with the reason, and the child is started through
  `/proc/self/exe` with a protocol version. The NixOS VM test converts a DOCX in a child
  under the module's system call filter and a 256 MiB limit. OCR in a child has not been
  run, because no OCR models were at hand. PDFium and ONNX Runtime map their libraries
  and model weights into the address space, so a server with OCR should give
  `--convert-memory` at least 4,096 MiB until a measurement says otherwise. The child sets its limits on itself before it reads the file and refuses to
  run without them, so the server starts it with `posix_spawn` and does not copy its own
  page tables, as a `fork` would. The child is given `MIMALLOC_ARENA_RESERVE=64MiB`,
  because mimalloc otherwise reserves 1 GiB of address space before the limit is set.
  The flag takes at least 256 MiB, below which the binary cannot start its threads. A
  build without mimalloc uses glibc's allocator, which reserves 64 MiB of address space
  for each thread's arena, so a small limit leaves fewer threads room there. The
  tradeoff is latency and memory for containment. A child costs one process start per
  conversion, and the server may use up to `--pdf-workers` times `--convert-memory` of
  address space for conversions on top of its own. On a shared 16-core Linux laptop,
  converting the 5 KB DOCX fixture took a median of 41 µs in process, or 85 µs with the
  thread the in-process path starts, and a median of 8.5 ms with a 90th percentile of
  9.5 ms through a child of the release binary, measured 200 times with the ignored test
  `ingest::isolate::tests::overhead`, and BENCHMARKS.md records them. Starting
  `/bin/true` took 2 to 4 ms on the same machine, so most of the cost is starting a
  process. The cost is small next to the model calls of an ingestion and is paid once per
  document. The converter has a cargo-fuzz target in
  `crates/sparkles-office/fuzz`, started with `mise run office:fuzz`, and its normal
  tests convert 400 seeded mutations of every format's fixture. The fuzz corpus starts
  from the fixtures and from the amplification shapes of `tests/budget.rs` at the target's
  size, and the target runs with an output budget of 8 MiB. A first run of 90 seconds
  executed 435,000 inputs and found nothing. After the budget, a second run of 90 seconds
  with two jobs on a shared machine executed 296,000 inputs and found nothing.
- **Deviations.** The converter is a workspace crate rather than a module of the server,
  so that the fuzz target does not need to build the server. A picture inside a document
  contributes its alt text as `[Image: …]`. The converter can return the pictures
  themselves, through `Converted.images` and the `max_images` option, which is tested.
  Every call site passes 0, so nothing consumes the pictures until the `describe` role
  exists.
- **Follow-ups.** The converter should register its peak memory with the server's query
  memory pool. A workbook should apply the `confirmTokens` estimate to the total of its
  sheets. Entity linking should report every variant of an ambiguous group, write the
  choice for all of them, list the first exact links for review and pin `atCommit` from
  the first batch. A test should import a Copilot or Aider session into a dataset that
  was created before those harnesses joined the vocabulary. These stay under roadmap
  item 15. Lexical linking also cannot tell two entities with the same label apart, which
  is a limit of the method.

### Roadmap follow-ups (2026-10-12)

These items come from the [roadmap](../../ROADMAP.md). They extend the design above and do
not change it.

**Item 6: Ask measured on QALD and LC-QuAD, with numbers per model.** §11.1 already
plans QALD-9-plus over DBpedia and Text2SPARQL'25, and `scripts/eval-ask` already runs
the matrix of §11.4 on the demo set of `testsuite/ask/demo.json`, with `--self-test`
against a mock provider. Its offline scorer refuses SERVICE before a query child
starts, including after CR, LF and CRLF comment endings. An excluded first draft
remains unknown and does not enter the conditional failed-first denominator. The
paid wrapper is a loopback proxy and an append-only cost ledger. The proxy forwards
each request once, and the ledger holds a bound before the request and the exact cost
after the reply. Error replies reach the client's retries unchanged, and a failed Ask is a
scored failure. A complete real-model matrix and published numbers remain missing.

The initial Anthropic demo pilot on 2026-10-15 passed all 13 local gold checks and
stopped on the first Haiku request with `upstream_framing`. It ran 17 of the 68 demo
questions on four pipelines, which were three pairs and a real cascade, so it planned 68
Ask executions. It attempted and executed one, scored no cases and left 67 unattempted.
Its wallet recorded zero settled spend and retained 1002560000 USD nanodollars, or
$1.002560, as its full unresolved liability. Actual billed spend is unknown and is
probably under one cent for one Haiku call. The retained status does not identify which
framing check failed, and this pilot supplies no accuracy results.

The gate's reader raised `upstream_framing` only for a reply without exactly one
Content-Length, for a `Transfer-Encoding` header, for a malformed status or header line,
and for 204 and 205. A reply outside 2xx raised `upstream_status`. The failure came 7.3
seconds after the request left, so the model had run, and the reply was most likely a
2xx in chunks, which the reader refused before it read the usage. The bound of
$1.002560 is the gate's fixed reserve for Haiku, a million input tokens at $1 per million
and 1,024 output tokens at $2.50 per million, which are not Haiku 5.5's prices. The
request held a grounding trimmed to 8,192 tokens and asked for at most 1,024 output
tokens, which at Haiku 5.5's $0.10 and $0.50 per million costs under $0.002.

The proxy that replaced the gate decodes every framing, and a test replays a chunked
reply of the pilot's shape and checks that it settles at its exact cost. The ledger is
now version 2. Reserve and settle lines carry a time, and a reconcile line replaces a
charge held at its bound, which is the carryover, a reserve without a settle line or a
request settled at its bound, with the billed amount and the evidence for it.
`scripts/eval-ask-anthropic --held` lists those charges, and `--reconcile` writes the line
without reading a key. The pilot's carryover is reconciled this way once the Console's
cost for its key on 2026-10-15 is known. A reply that comes back compressed despite the
identity request is decoded, and Sonnet 5.5 cache reads are charged at the published
$0.20 per million tokens, where the table had half that. Opus 5.5 cache reads are also
$0.20 per million, which is a twentieth of its input price, and a test pins the whole
rate table against the pricing documentation.

A review of this work tightened the ledger. Replay refuses a negative or non-integer
amount, a request ID that is not a positive integer, a settle without its reserve, a
carryover anywhere but right after the header and a reconcile that would take the
ledger past USD 25. A reconcile can only lower a held charge unless `--allow-raise` is
given, and its evidence must be 10 to 1000 printable characters. Any fault of a request
settles at its bound, and an error inside the request keeps only its class, so a key
with a stray newline cannot reach a message.

The gate that the pilot used is replaced. The new ledger opens with the pilot's
$1.002560 as a carryover line. The run file's cap is $23.997440, so the cap and the
carryover together stay within USD 25. The ledger's spent total includes the carryover,
so new requests can spend $22.994880 under that cap. The pilot's
wallet, grant claim and report are kept as the record and are never reopened. The next run
uses a binary built from a named commit outside the private notes.

The first paid run, on 2026-10-19, asked the demo set and CK25 with Haiku 5.5, Sonnet
5.5 and Opus 5.5 at effort `medium`, through the binary of commit 23075691. It settled
$4.30 over 612 requests with no cap refusal, and its ledger replays under the stricter
reader with nothing held but the carryover. The policy of that run compared whole rows,
and it scored as wrong every answer that named each result beside its IRI, which Sonnet
and Opus do for most questions. The projected-column policy was chosen after those
results were seen. `scripts/eval-ask-rescore` scored the saved answers again under it
without calling a model, and under the whole-row policy it reproduces the run's numbers
exactly. The projected policy favors an answer with more columns, because each extra
column is another chance to match the gold, so the table gives both and counts the
correct answers that needed a projection. With 59 and 49 questions, a difference of a few
questions between models is within noise.

| Set | Haiku 5.5 | Sonnet 5.5 | Opus 5.5 |
|---|---|---|---|
| demo, whole rows | 47/59 | 14/59 | 13/59 |
| demo, projected columns | 52/59 | 48/59 | 56/59 |
| demo, correct only by projection | 5 | 34 | 43 |
| CK25, whole rows | 10/49 | 4/49 | 5/49 |
| CK25, projected columns | 20/49 | 26/49 | 27/49 |
| CK25, correct only by projection | 10 | 22 | 22 |

The prompts, the effort, the output limit and the parsing were the same for every model,
and every draft came back at the json-schema level. On the demo set the remaining
failures are wrong rows, an answer with fewer columns than the gold, a clarification
question, and drafts that came back with an empty query and an empty explanation, five
from Sonnet and two from Opus. The run kept no reply bodies, so those empty drafts
cannot be traced to a reply, and the proxy now records every reply for that reason.
CK25 stays lower for every model for three reasons besides wrong queries. Seven of its
questions name a person or product by a part that several entities share, such as "Ms.
Brant", and the pipeline asks which one is meant before any model call, which the
evaluation scores as a failure. CK25 has a class `pv:Service`, and `sparkles ask` took
that name for the SERVICE keyword and left two questions unrun for every model, a fault
now fixed. Two gold queries order on values with ties before a limit. The runs of the
two unrun questions, and of the empty drafts with replies recorded, need new requests.
The first paid run gave every model the default `contextTokens` of 8,192, so the
grounding of each question was trimmed to fit that window, and its numbers describe
Claude models treated as small ones. A run file now sets `contextTokens`, and the second
run gave each model 200,000.

The evaluator now answers a clarification as a person who means the gold query would.
It picks one of the choices the pipeline offered, by an IRI the gold names or else by the
words the choice shares with the gold, and scores a second Ask that carries the choice,
so nothing from the gold that the pipeline did not propose reaches the model.
`testsuite/ask/README.md` gives the rule. Against the harness's fake provider, which
drafts the gold query, 84 of the 107 QALD-9-plus questions and 58 of the 74 DBpedia-only
questions stopped at a clarification. Once those were answered, 105 of 107 and 74 of 74
scored correct, so clarifications no longer decide the score.

The second paid run, on 2026-10-24, asked QALD-9-plus and the 74 DBpedia questions on the
DBpedia 2022.12.01 database of 1.01 billion quads, with Haiku 5.5, Sonnet 5.5 and Opus
5.5 at effort `medium`, through the binary of commit 03aa64bb. It settled $28.80 over
989 requests with no cap refusal and nothing held. The run showed a fault in the scorer.
Ask runs a draft with the well-known prefixes of C20 §3.1, and `sparkles query --loc`,
which the scorer used to run the same query again, does not declare them. A correct
answer that wrote `rdfs:label` without declaring it therefore failed when it was scored,
which happened to 53 Opus answers on QALD-9-plus and to none after the fix. The scorer now declares those prefixes
first, and `scripts/eval-ask-rescore` scored the saved answers again without calling a
model. The table gives the corrected scores, the same scores without credit for extra
columns, and the cost of each model on each set.

| Set | Haiku 5.5 | Sonnet 5.5 | Opus 5.5 |
|---|---|---|---|
| QALD-9-plus, projected columns | 16/107 | 35/107 | 40/107 |
| QALD-9-plus, equal width | 13/107 | 15/107 | 15/107 |
| QALD-9-plus, as first scored | 13/107 | 21/107 | 21/107 |
| QALD-9-plus, cost | $0.31 | $5.61 | $11.14 |
| DBpedia-only, projected columns | 11/74 | 28/74 | 32/74 |
| DBpedia-only, equal width | 9/74 | 14/74 | 13/74 |
| DBpedia-only, as first scored | 10/74 | 18/74 | 15/74 |
| DBpedia-only, cost | $0.19 | $3.72 | $7.82 |

Most questions asked a clarification, 84 or 85 of 107 and 58 or 59 of 74 for each model,
because `link_entities` offers a category or a namesake beside the main resource, and the
evaluator answered all of them. Haiku found 71 of the QALD-9-plus questions
unanswerable. Its drafts said that no listed predicate fits, since the grounding lists
150 of DBpedia's predicates and those seldom include the one a question needs. Sonnet and Opus write a query anyway, guessing the predicate from its name, and
most of their failures are wrong rows from a guessed predicate or a different modelling
choice than the gold's. Most of their correct answers add a label column beside the IRI,
which only the projected policy credits. The cascade from Haiku to Sonnet to Opus scored
as Haiku did, because a wrong answer from Haiku is not a failure that escalates. The
median time of an Ask, counting the first Ask of a question that stopped at a
clarification, was 11.8 s for Haiku, 12.7 s for Sonnet and 18.6 s for Opus on QALD-9-plus.
With 107 and 74 questions, a difference of a few questions between Sonnet and Opus is
within noise. [ROADMAP](../../ROADMAP.md) item 263 holds the grounding that would show a
question's predicates and the ranking that would spare the clarification. What remains
is listed below.

- Run the matrix of §11.4 with real providers on the demo set, QALD-9-plus (DBpedia
  questions, gold answers recomputed on the loaded release as §11.1 says) and the
  Text2SPARQL'25 sets, for at least one model per provider kind and size class: a small
  and a large local model through Ollama, an OpenAI-compatible model, and Haiku-, Sonnet-
  and Opus-class Anthropic models. Each run fixes the prompts, the dataset's commit and
  the provider's model version, and the report records them.
- `scripts/qald9plus-convert` fetches the 150 questions of the QALD-9-plus DBpedia test
  split from the KGQA repository at a pinned commit, checks the file's SHA256 and writes
  a set in the demo set's format. The data is licensed CC BY 4.0 and is not vendored.
  With `--loc` and `--sparkles` it runs every gold query on a DBpedia database, as §11.1
  says, and drops the questions whose gold returns nothing, fails, or cannot be scored,
  and the set counts them with the release and the binary's SHA256. The gold queries
  were written for DBpedia's Virtuoso endpoint, so the converter declares the prefixes
  that endpoint predeclares and puts parentheses around a bare aggregate or function
  projection such as `SELECT DISTINCT COUNT(?x)`, and keeps the source query beside the
  rewritten one. The set also compares each recomputed answer with the answer QALD
  publishes, by value. The recompute ran on English DBpedia 2022.12.01, all 38 files of
  [BENCHMARKING](../../BENCHMARKING.md)'s billion-triple run, 1,014,683,275 distinct
  triples loaded with `sparkles load --lenient` in 449 s on atlas at a peak RSS of 8.3
  GiB, with a text index over `rdfs:label` and `foaf:name` of 23,372,583 documents built
  in 126 s. The binary came from commit 23075691 with SHA256 2ed83fff…3b960, and the set
  embeds the load manifest with each file's SHA256. Of the 150 questions, 107 remain.
  The gold of 41 returns nothing on this release, one fails because it orders by an
  aggregate without grouping, and one returns 1,693 rows, above the scorer's 1,000. 23
  gold queries were rewritten. Of the 107, the recomputed answer equals the published one
  for 53 and differs for 54, 21 of which share no value with it. A search of the text
  index for "Salt Lake City" returns the city's resources, so `link_entities` has what it
  needs.
- A dry run of the DBpedia sets against the fake provider showed that Ask cannot answer
  on this database yet. Each `sparkles ask` took about 104 s, of which the fake model
  took under 50 ms, and nine of the first ten failed although the fake drafted the gold
  query, which `sparkles query` answers in 0.04 s with 118 MiB under the same 2 GiB
  budget. Their check reported `budget-memory` against the 2 GiB query memory budget of
  `sparkles ask`, which is measured. `check_query` builds a schema report of the whole
  dataset before it checks a query, which is the likely cause but was not confirmed with
  a profile. Through `sparkles mcp` with a 30 s timeout
  the same check stops after 45,857 of 58,564 predicates of that report, while
  `describe_schema` with the summary section takes 48.6 s, and the process reached 11.1
  GiB. Each `sparkles ask` process builds the report again. The DBpedia sets therefore
  wait for `check_query` to read a stored or cached schema report, or to check terms
  without one, and for `sparkles ask` to keep the report between questions, for
  example by asking through a running server. [Roadmap](../../ROADMAP.md) item 89 tracked
  that work, and the section on asking over a billion quads below records how it was
  done.
- `scripts/text2sparql-convert` converts Text2SPARQL'25 from the challenge repository
  and the CK25 repository at pinned commits, with SHA256 checks, and vendors nothing.
  `--set db25` takes the 100 English DBpedia questions, which the challenge ran on
  DBpedia 2015-10, and recomputes their gold on a DBpedia database with the same
  rewrites as QALD-9-plus. On the 2022.12.01 database above, 74 remain. The gold of 25
  returns nothing and one returns 1,199 rows, and 11 gold queries were rewritten. The
  recomputed answer equals the challenge's published answer for 26 of the 74 and differs
  for 48, 23 of which share no value with it. `--set ck25` takes the 50
  questions over CK25, the corporate graph of 2025, which the converter writes beside the
  set so the set loads it as the demo set loads `org.ttl`. On that graph 49 remain, one
  being dropped for 1,938 rows. The recomputed answer equals the published
  one for 45. Two differ because their gold orders on a value with ties before a limit,
  and the published answers of the other two are empty while the gold returns rows on
  the graph. CK25's data and questions are licensed CC BY 4.0 and the DBpedia questions
  CC BY-SA 4.0. The strict scorer compares rows as sets, so the two CK25 questions marked
  `RESULT_ORDER_MATTERS` are scored without their order, and their scores carry
  `orderNotScored`.
- [BENCHMARKS](../../benchmarks/natural-language/RESULTS.md#natural-language-questions) gives the Anthropic numbers
  per model. Numbers per role, and §3.7's example role lists replaced in the
  documentation with measured ones as §11.4 intends, remain.
- Add LC-QuAD 2.0 and QALD-10, which §11.1 deferred until a Wikidata subset can be
  loaded. X06's Wikidata work ([X06](../X06-benchmark-coverage.md)) provides the load. The
  subset is the closure of the gold queries' answers and the entities the questions
  mention, to two hops, plus the properties' labels, so the schema grounding sees a
  realistic vocabulary. Questions whose gold query returns nothing on the subset are
  dropped and counted, as for QALD-9-plus. `scripts/lcquad2-convert` converts the 6,046
  questions of the LC-QuAD 2.0 test split at a pinned commit and keeps 5,820 that have a
  usable question or paraphrase, and `--sample N` keeps a fixed sample chosen by a hash of
  each uid. The split publishes no answers, so the subset has to be cut from a full
  Wikidata load rather than from the answers. 1,529 of the test queries read statements
  and qualifiers through `p:`, `ps:` and `pq:`, which the truthy dump of X06 §3.8 leaves
  out, so the load must come from the full dump. That needs the large host of X06, and
  until it exists the converted set cannot run.
- Report the memorization risk: for each public set, the accuracy on questions whose
  gold query appears verbatim in public repositories against the rest, and the
  corporate Text2SPARQL'25 set as the uncontaminated reference.
- Run the subset on the demo set in CI on every change to the prompts, as §11.4 says,
  with the mock provider when no key is configured.

**Ask on a billion quads (2026-10-19), roadmap item 89.** On the DBpedia 2022.12.01 database of
[BENCHMARKING](../../BENCHMARKING.md), 1.01 billion quads on atlas, each `sparkles ask`
used to take about 100 seconds and most failed on the 2 GiB query budget. Three causes
were found and fixed. First, `check_query` and the `describe_schema` summary that grounds
the question each built a full schema report, which read every quad twice. A
`check_query` alone took 52.7 seconds and reached 11.1 GiB. The default graph of the
summary and the union graph of the check are different selections, and the dataset kept
one report, so each Ask built two. Second, the terms list of `check_query` looked up the
labels and types of the query's entities with a VALUES block outside the reader's UNION
of the default graph and the named graphs. The engine joined the table after each branch
had read every quad of its pattern, which passed the budget. Third, `link_entities`
writes its lookups the same way and failed for the same reason, which the pipeline hid by
grounding without entities.

The design keeps a full report for small stores and builds a cheaper one for large ones,
rather than persisting a full report on disk. Above `--schema-statistics-above` quads,
ten million by default, the tools' reports take the counts of predicates and classes
from the statistics that the index build keeps, combined with the delta since the
build, and read only the declarations and labels from the indexes. Such a report says
`"basis": "statistics"`. It counts quads for triples and leaves the largest number of
objects per subject unmeasured. Its distinct counts and class instances are exact, because
the counts are corrected for the delta and for the graphs the selection leaves out with
the planner's exact correction of the statistics. A selection whose left-out graphs hold
more than a million base quads is scanned. The kinds of a
predicate's objects are measured when a tool needs them, in full up to 200,000 quads and
above that from exact counts of the id ranges of each kind plus a sample of 8,192 stored
literals. A literal that is an object of the predicate never gets a sampled
`language-tag` or `datatype-mismatch` warning, and a sampled warning says so. A report
lasts until the next commit, and a dataset now keeps one per selection, up to four. No
on-disk format changed. A persisted report would still have needed a full scan for each
new commit, while correcting the statistics for a commit's changes costs no more than a
scan and usually far less. The report
endpoint and `sparkles schema` keep the exact report, because they are asked for it.

The planner now moves a VALUES table that shares variables with a UNION of the same
join into each branch, and inside a BIND or FILTER there whose expression does not read
the table, so each branch looks its rows up by index. The join distributes over the
union, so answers are unchanged, which a test checks by running fifteen query shapes with
the rewrite on and off. The rewrite moves at most 10,000 rows, and at most 20,000 rows
over all the branches' copies. It keeps the table outside a BIND or FILTER that calls
`RAND`, `UUID`, `STRUUID`, `BNODE` or a custom function, and it leaves a UNION that holds
a SERVICE, a LATERAL or a property function call as written. The query that failed after 1.94 seconds at 5.5 GiB now
takes 0.04 seconds. `sparkles ask` gained `--query-memory-mb`, 2048 by default as in
`sparkles mcp`, and `--schema-statistics-above`.

Measured on atlas with a warm page cache, through `sparkles mcp`, the first
`check_query` of a process takes 0.3 seconds, or up to 1.5 seconds right after the
binary changed, and later ones take 20 to 60 milliseconds. The first `describe_schema`
summary of 200 predicates takes 1.4 seconds and a repeat 11 milliseconds. Anonymous
memory stays under 500 MiB, and the mapped index pages the sampling touches add about 2
GiB of reclaimable file-backed memory to the resident size. Six `sparkles ask` runs
against the harness's fake provider, which drafts the gold query, each took 1.9 seconds
with at most 508 MiB of anonymous memory. Now that `link_entities` works, five of the six
stop at a clarification, because DBpedia gives "Salt Lake City" to the city, its
category and a diocese, and "Berlin" to several towns. With the city chosen through
`--clarification`, the first question answers with 292 rows in 1.96 seconds. The DBpedia
sets therefore need a way to answer such clarifications, or a ranking of candidates
that prefers the main resource, before they can be scored. That work belongs to item 6.
A paid run of the DBpedia sets no longer waits for item 89.

A review of the item found that the first statistics report dropped a class whose
instance remained in a second graph after a deletion, let distinct counts drift upward
with updates, fell back to a full scan as soon as the store had one named graph, and
broke the `describe_schema` output schema by omitting `maxPerSubject`. The exact
correction and the schema change above fixed those. Two limits remain. An ingestion
profile without a predicate list gives every predicate any object on such a report,
which [API.md](../../API.md#schema-reports-from-statistics) states, and the VALUES rewrite
misses a table under `GRAPH` and a branch filter that reads the table, which
[Roadmap](../../ROADMAP.md) item 90 tracks.

**Item 15: multimodal ingestion, more transcript adapters and CSV entity linking.**
§7.1 converts text, Markdown, HTML and PDF (`ingest/convert.rs`, `Format`), the
transcript adapters of `sparkles-memory-import` read Claude Code and Codex, and a CSV
draft mints IRIs for its IRI columns without linking them (Phase 4 deviations).

- **Office documents.** DOCX, PPTX, XLSX and ODF text and presentations, converted to
  Markdown in the server by reading their XML parts: headings, lists, tables, speaker
  notes and slide titles, with a marker per slide or sheet that serves as the page
  marker of §7.1.1 for citations. Spreadsheets go through §7.8's tabular path one sheet
  at a time. Embedded images follow the image path below. The converter is a Rust
  crate under a permissive license or written in the server, behind an `office`
  feature, and is fuzzed like the PDF path.
- **Images.** PNG, JPEG and WebP, through a provider whose model accepts images, under
  a new role `describe` with the providers' capability detection of §3.6. The model
  returns a description and the text it reads, both stored as the rendition with a
  region per passage, so a span cites the image and a bounding box. OCR behind
  `pdf-ocr` serves images with text when no such model is configured. Images never go
  to a provider that `sendByProvider` limits.
- **Audio and video.** Speech to text through a local Whisper-class model behind a
  feature, or an OpenAI-compatible transcription endpoint, with timestamps per segment.
  The rendition is the transcript with a time marker per segment, and a span cites a
  time range. Speaker labels, when the model gives them, become mentions for linking.
- **Transcript adapters.** Adapters for Gemini CLI, Cursor, GitHub Copilot's and
  Aider's session logs, with their file locations and formats documented as §8.10.1
  documents Claude Code's and Codex's, the same redaction, and the generic adapter as
  the fallback. Each adapter is a module of `sparkles-memory-import` with fixtures taken
  from the harness's documented format.
- **CSV entity linking.** After the mapping draft, the values of each column that the
  mapping maps to IRIs of an existing class are linked with `link_entities` in batches.
  Exact links replace the minted IRIs, ambiguous ones are listed in the preview for a
  person to choose, and the choices are kept as a lookup table in the CSVW metadata
  (a `valueUrl` template over a lookup), so a second upload of the same file links the
  same way.

**Item 27: rewrite suggestions moved to U04.** Phase 6 (§6.7) was not built. The `optimize`
role exists in `models/config.rs`, but there is no `POST /{ds}/sparql/optimize`. Live
query profiling with per-operator row counts and the verified rewrites of §6.7 are
designed together in [U04](../U04-live-query-profiling.md), which takes over §6.7's gate,
result comparison and profile comparison. Phase 6 here is replaced by U04.

**Community summaries for global questions.** [C22](../C22-graph-rag-community-summaries.md)
adds communities from [F14](../F14-graph-algorithms.md)'s Leiden with reports written by a
new `community` role, global questions in Ask (`mode: "global"`), and community context
in `recall` and the brief.

**Item 77 (Tier 5): redaction beyond the fixed secret patterns.** The other half of item
77, ingestion controls in the UI, is in
[C19's follow-ups](../C19-layered-settings.md#roadmap-follow-ups-2026-10-12).

- **What exists and what is missing.** Redaction (§8.10.7) is regular expressions only:
  the built-in list of `builtin()` in `crates/sparkles-memory-import/src/redact.rs`
  (private keys, AWS, GitHub, Slack, Anthropic, OpenAI and Google keys, JWTs,
  authorization headers, passwords in URLs, and `key`/`secret`/`token`/`password`
  assignments), the dataset's `imports.secretPatterns` (at most 100, checked in
  `crates/sparkles-server/src/assist.rs`), and the person's `--redact-patterns` file or
  `memory.toml` entries. A secret in any other format, such as a database password in
  prose, a token of a service the list does not know, or personal data, passes. The
  server's own check, `secret_check` in `crates/sparkles-server/src/mcp/memory/ingest.rs`,
  runs only in `register_source` and only for graphs under `imports.base`. Server-side
  document ingestion (`crates/sparkles-server/src/ingest/`) converts an uploaded document
  and sends its text to the `extract` role's providers with no redaction and no check at
  all; `send: "documents"` is the only control.
- **One redaction policy.** `imports.secretPatterns` generalizes to a `redaction` object
  in the memory settings, which the CLI already reads from `GET /$/memory/{ds}`:
  `{ patterns, entropy, personal, allow, knownValues }`. `secretPatterns` stays accepted
  as `redaction.patterns`. The same policy runs in the CLI before upload, in
  `register_source` for every graph (not only under `imports.base`) when the dataset
  sets `redaction.everywhere`, and in server ingestion before any text reaches a
  provider.
- **High-entropy tokens.** `entropy: { minLength: 20, minBitsPerChar: 4.0 }` marks
  strings of base64, base64url or hexadecimal characters whose Shannon entropy per
  character passes the threshold, when they stand as a value in an assignment, a
  header, a URL query parameter or a quoted string. Hexadecimal strings of exactly 40 or
  64 characters (commit and content hashes) and canonical UUIDs are not marked, since
  sessions and notes are full of them. The replacement is `[redacted:high-entropy]`.
  Off by default, because it has false positives that patterns do not.
- **Personal data.** `personal: ["email", "phone", "payment-card", "iban", "ip"]` adds
  detectors for those categories. Payment cards must pass the Luhn check and IBANs the
  ISO 13616 check digits, which removes most false positives; phone numbers need a `+`
  and a country code. Each category is opt-in and its matches are counted per category
  in `mem:redactions`.
- **Known values.** The strongest detector needs no pattern: the exact values that must
  never leave. The server knows its runtime secrets and provider keys (C19 §11.2), and
  ingestion on the server replaces any occurrence of one of those values, matched with
  Aho-Corasick over the text, with `[redacted:server-secret]`. The CLI does the same for
  the values of its own environment variables whose names contain `KEY`, `SECRET`,
  `TOKEN` or `PASSWORD`, and for the values of a `knownValues` file the person names.
  Values shorter than 8 characters are skipped. Values are compared, never logged or
  stored.
- **Allow list.** `allow` holds patterns whose matches are never redacted, such as a
  project's public test keys, so that a false positive can be silenced without turning
  a detector off.
- **Server ingestion.** The pipeline redacts the converted text after conversion and
  before chunking, so the rendition the facts cite is the redacted text, as it is for
  imports, and the task reports `redactions` by detector. `redaction.serverIngest:
  "redact" | "refuse" | "off"` chooses whether a match is replaced, fails the task with
  `secret-detected` and the detector's name and offset, or is ignored; the default is
  `redact`. In `register_source`, a match in a source sent with `original` is refused
  rather than redacted, as today, because the kept bytes would still hold the value.
- **Tests.** Detector unit tests with positive and negative examples per category,
  including hashes and UUIDs that the entropy detector must leave alone; a server
  ingestion of a Markdown file holding a provider key from the server's own secrets,
  checked against a mock provider that records what it received; an import with an
  allow-listed test key left in place; `secretPatterns` from an existing `memory.json`
  read as `redaction.patterns`.

### Shared memory workflows

[Roadmap](../../ROADMAP.md) item 81 has a shared representation specified by
[C25](../C25-evidence-and-assessments.md), building on [C17](../C17-agent-memory.md#shared-memory-representation).
The profile is unimplemented. This follow-up defines how ingestion, recall, review and
maintenance use it without changing the shipped phases. Current asserting writes stay
asserting writes, while C25's additive proposal operation records claims without
asserting their propositions.

Ingestion must map human notes, documents and agent episodes to the same entity
identities and declared vocabulary, while preserving each source, rendition, supporting
span and assertion. Notes, decisions and tasks need explicit links to their evidence
and their dependencies. An imported instruction remains identifiable as an instruction
and does not become executable policy through ordinary recall. Existing graph grants,
redaction, review and supersession apply throughout.

Recall and the memory brief must be able to distinguish current reviewed knowledge,
unreviewed observations, hypotheses, preferences and instructions under the chosen
profile. They must expose conflicting evidence and uncertainty rather than treating
consolidation, a summary or review as proof. When a source changes, maintenance must
identify affected conclusions and decisions for review, retain independent surviving
support and explain the dependency path. Source correction, changed valid time and
expiry need separate examples from retraction or deletion.

Acceptance will use a small realistic project and personal knowledge dataset containing
documents, notes, sessions, entities, decisions and tasks. It must demonstrate cited
recall, a decision traced to its evidence, conflicting sources, temporal questions and
a revised document that marks the appropriate dependents for review. It must also
preserve unrelated facts and prove that a retrieved instruction is treated as data.
Fixtures and deterministic queries come first, followed by an end-to-end evaluation
with reported accuracy and retrieval failures before claiming practical effectiveness.
This evaluation is separate from item 6's Ask model matrix and item 15's input formats.

The shared profile can be used before the incremental core of
[F13](../F13-incremental-views-and-provenance.md). Automatic exact derivation maintenance
remains in F13 and explanations in [F18](../F18-explanations-and-entailment-diffs.md).
Community reports from [C22](../C22-graph-rag-community-summaries.md) will cite the same
assertions and evidence, while retaining their own generated and reviewed status.

### Ask grounding accuracy, item 263

The schema summary accepts a question and ranks all caller-visible schema entries by
words shared with labels and local names before applying its response limit. Ask retrieves
common and question-ranked summaries separately, deduplicates their IRIs and keeps half
its class and predicate budget for common vocabulary. Both context sizes use this rule.
The reports remain immutable and a caller with a restricted graph view cannot read terms
from an unrestricted cached report.

Ask can prefer a resource in the order of the offered clarification choices. The
preference resolves namespaces using the tool's prefix map, keeps different visible
concrete types separate, respects reported exact matches beyond the displayed labels and
handles the quote forms that mention extraction accepts. It never changes an ambiguous
verdict, suppresses a clarification or makes a heuristic link exact. Candidate, type and
alternate-label displays are bounded and do not establish uniqueness. C22 routing and the
generic link_entities contract retain their original verdicts.

Offline checks use independently retrieved DBpedia predicate counts and declared English
property labels, plus the saved public-set questions. Gold predicate constants are read
only after retrieval. A fixed half-common, half-question budget retrieves 189 of 304
existing predicate constants mentioned by the gold queries, compared with 151 from the
count-only list, across 181 questions and 58,564 predicates. This is vocabulary retrieval,
not model execution accuracy, and it omits variable predicates and labels in other
languages. The reference inventory expands the predicate shorthand `a` and masks
literals and comments before reading gold IRI references. Pure lexical replacement
retrieved 133 and was rejected because it displaced common structural terms.

The saved paid run supplies 142 distinct clarification choice sets for separate display
ranking checks. Their displayed choices omit alternate labels, and gold-word tie choices
do not establish either selection's correctness. A prototype that automatically resolved
some choices was rejected because omitted candidates, types and labels could hide a
necessary clarification. Every ambiguous mention now retains its offered choices and
stops before drafting. Those offline checks made no provider calls. Item 263 remains
open for a complete visibility-safe evidence contract, context-sensitive namesake
resolution and further model-dependent accuracy improvements.

A paid repeat on 2026-10-25 measured the reviewed grounding with immutable source
`0d4fdd8e` and binary SHA256
`7fee24e01b21f4bd3aeb6f87ae87b85f6455a2849470d55f9e213e250104ba93`.
It includes the reviewed native ASK admission correction, but not the separate contextual
consumer. All 107 gold queries passed without provider access, and their RDF-term row
sets and variables matched the retained reference. Each model then ran all 107 retained
questions with the same dataset, medium effort, 200,000-token context, 4,096 output
tokens, 300-second Ask deadline and 60-second query timeout. These questions had already
been used during diagnosis, so this is regression evidence, not unseen generalization.

| Measure | Haiku 5.5 | Sonnet 5.5 |
|---|---|---|
| Projected accuracy before and after | 16/107 to 26/107 | 35/107 to 31/107 |
| Equal-width accuracy before and after | 13/107 to 23/107 | 15/107 to 9/107 |
| Projected cases gained and lost | 14 gained and 4 lost | 11 gained and 15 lost |
| Wall median before and after | 12.27 s to 14.78 s | 12.98 s to 14.98 s |
| Wall p95 before and after | 29.42 s to 29.22 s | 34.02 s to 33.88 s |
| Fresh full-run cost including preflight | $0.2876 | $5.4574 |

Wall time includes both calls when the evaluator answers an offered clarification.
There were 84 such cases for each model. The single run improves Haiku's measured
accuracy but lowers Sonnet's under both policies. Rust, Clang and LLD versions match
the earlier binary, but its GCC comment is 15.3.0 against 16.2.0 in this build. Together
with provider variability, that prevents isolating the grounding's causal contribution.
Roadmap item 318 records the higher median latency without treating its cause as proven.

The fixed ten-question Haiku pilot scored 0/10 and cost $0.0397059. It is excluded from
both full-run denominators. Total fresh spend was $5.7847392 across 436 ledger-reserved
model requests, including both phases' paid provider preflights. The unchanged shared 50-dollar Ask ledger
has $15.4166424 remaining and no unresolved or positive held charges. No Opus call was
made. Costs use new request IDs, since raw case costs include charges from earlier runs
with the same case IDs. The [paid record](../../benchmarks/2026-10-25-ask-grounding-paid.json)
retains exact policies, paired case changes, tokens, costs and provenance.

## Ask stage diagnostics for item 318

The local Ask pipeline records bounded debug spans and completion events for total
grounding, both schema projections, stored examples, entity linking, optional community
recall, query checks and query execution. A completion records one call, inclusive
nanoseconds, a finite outcome and error category, and the tool-returned commit when
available. Every scoped completion also records a process-generated numeric pipeline
identifier and the context-window size when selected. These fields survive the
production JSON formatter without ancestor span lists. Nested synchronous asks and unwinding restore
the previous scope, while standalone tool stages remain unscoped. No added field
contains a client identifier, question, label, arbitrary IRI, prompt or error message.

Separate nested stages identify the example query and document embedding preparation
and the linker's vector search. Their times include cache lookup, admission and any
local or remote model work. The vector-search stage also includes query execution, so
it is not a measurement of remote inference alone. Parent stages include their children.
The existing `usage.elapsedMs`, `modelMs`, token counts and role-step accounting retain
their meanings. In particular, a clarification can have zero Ask role calls while its
tools request embeddings.

The provider-free regression fixture covers initial clarification and a follow-up,
exact and absent mentions, optional recall, small and large context windows, and two
principals with different graph visibility in both access orders. It compares answers,
choices, ground events and draft prompts with tracing disabled and enabled. Only timing
fields are excluded. A hidden namesake must appear only in the unrestricted choices.
The embedding fixture uses a loopback mock and requires an actual embedding request
while the initial clarification still reports zero Ask model calls and milliseconds.

This checkpoint adds diagnostics and fixture measurements. It does not optimize the
pipeline or explain the retained DBpedia campaign's multi-second residual. Item 318
remains open for representative stage profiles, a justified optimization and matched
validation of its latency and semantic effects. No paid provider was called.

### Entity evidence bounds, item 263

The optional version 1 evidence of C17 records the visible candidate pool and display
bounds and exposes retained types and labels beyond the three display entries. It makes
per-entity and global query truncation explicit. An exact-string lookup cannot claim
complete scope from sampled language inventory, RDFS-on-read expansion or a different
Info view. Normalized and
vector search remain bounded, and the contract makes no complete-universe or unique
intent claim. Ask continues to preserve every ambiguous clarification. Context-sensitive
resolution and a fresh end-to-end model evaluation remain under item 263.
