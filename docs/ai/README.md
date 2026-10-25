# AI and agents capabilities

This page describes the available capabilities and their current limits. The
[capability matrix](../FEATURES.md#capability-status) records delivery status within
each stated scope.

## AI and agents

An MCP server gives agents bounded, grant-checked access to the data. The server and the
web UI turn questions into checked queries, keep cited agent memory, ingest documents and
build community reports.

### MCP server

* `sparkles mcp` serves MCP over stdio, and `serve --mcp` serves it over Streamable HTTP
  at `/$/mcp`, with the `mcp` cargo feature, which is on by default
  ([USAGE.md](../USAGE.md#mcp-server-llm-agents)). It supports MCP revisions `2026-07-28`,
  `2025-11-25` and `2025-06-18`, with completions, `subscriptions/listen` and the tasks
  extension. `sparkles mcp --url` bridges stdio to a running server with the saved login.
* Its tools list datasets, describe the schema, run bounded SPARQL, explain queries,
  describe resources, find paths, list commits and changes, run full-text and vector
  search, validate with SHACL and ShEx, draft shapes, compare schemas, format SPARQL and
  RDF, run graph algorithms, and run GraphQL. Each stored query is a tool of its own.
* `check_query` checks a query against the schema without running it,
  `similar_queries` finds stored queries for a question, `link_entities` maps names to
  entities, and `recall` returns the facts around a question with a citation for each.
* Results come as a compact table or JSON, and truncation reports the exact total.
  `atCommit` keeps several calls on one snapshot, and `at` reads a past state. Every call
  runs under the engine's budgets with SERVICE off.
* Over HTTP each call runs as the request's caller, so tools, resources and prompts see
  only the datasets that caller may read, and the server's rate limits apply.
* Standard MCP hosts connect over HTTP with OAuth and no hand-made token
  ([USAGE.md](../USAGE.md#connecting-with-oauth)). The host finds the OIDC provider through
  the endpoint's protected-resource metadata, signs the user in there and asks for the
  `sparkles:update` scope only when a write tool needs it. `--mcp-require-auth` makes
  anonymous requests start the sign-in even when anonymous callers may read a dataset.
* The `sparql_update` tool is off unless the operator enables it, and is listed only for
  callers that may write. Writes pass write-time validation and carry a commit message,
  `LOAD` is refused, `dryRun` previews, `ifHead` makes a write conditional, and `patch`
  applies an RDF Patch.
* Every tool that reads or writes takes `branch`, and `list_branches`, `create_branch`,
  `merge_branch` and `delete_branch` manage scratch branches. Merges preview by default,
  need the previewed heads and refuse conflicts. `--mcp-scratch-branch-ttl` deletes idle
  scratch branches.
* Resources give each dataset's schema summary and prefixes and each stored query's
  definition. The prompts `explore_dataset`, `answer_question`, `run_stored_query`,
  `explain_term`, `agent_memory` and `ask_graph` start common workflows.

### Questions in plain language

* `sparkles ask`, `POST /{ds}/ask` and the query page's Ask bar turn a question into a
  checked SPARQL query ([C18](../specs/C18-natural-language-questions-and-ingest/README.md),
  [USAGE.md](../USAGE.md#asking-questions-with-a-model),
  [API.md](../API.md#natural-language-questions)). On DBpedia's billion quads, an Ask takes
  about two seconds with under 512 MiB of anonymous memory.
* The server knows model providers of three kinds, Ollama, the OpenAI protocol and
  Anthropic, with keys from named secrets and an ordered list of models for each role.
  `--model-config`, `GET /$/models` and `POST /$/models/{name}/test` configure and test
  them. The server detects structured output per model and falls back to JSON mode and
  plain text.
* Ask keeps half of its schema budget for common vocabulary and retrieves the rest by
  matching the question against every visible class and predicate. A guarded resource
  preference can reorder the choices offered for an ambiguous mention. Every ambiguous
  mention still asks for a clarification, because displayed candidates, types and labels
  can be truncated. The generic `link_entities` verdict is unchanged.
* `POST /{ds}/ask` runs as the caller and streams each step as a server-sent event, or
  answers one JSON object ([API.md](../API.md#asking-in-the-server)). `assistant.json`,
  through `/$/assistant/{ds}`, turns the assistant on for a dataset and sets what may leave
  the server per provider, with token budgets, a deadline and history retention.
* Each role moves from a cheaper model pair to a stronger one only on signals the server
  checks, such as a repair that still fails, an empty result that `why_empty` blames on
  the query, a failed call, a complex query, or **Try harder**.
* `why_empty` and `POST /{ds}/sparql/diagnose` name the first pattern, join or filter of a
  query without solutions, with a verdict of `query`, `data` or `unknown`.
  `POST /{ds}/check` and `POST /{ds}/recall` run `check_query` and `recall` over HTTP.
* An agent hands a query it wrote to the web UI with the `share_query` tool, and the
  query page opens it with the question, the explanation and the terms it uses, without
  running it.
* The server keeps each person's questions for `historyDays` in `/$/asks/{ds}`, records
  feedback, and counts answers, escalations, tokens and cost in `GET /$/models/usage` and
  `/$/metrics`. `scripts/eval-ask` measures each model on the 68 questions of
  `testsuite/ask/demo.json`.

### Agent memory

* `assert_facts`, over MCP or `POST /{ds}/facts`, writes facts into one named graph per
  source or session, each with an RDF 1.2 reifier that records PROV-O provenance, the
  source, time, caller and an optional confidence and quote
  ([API.md](../API.md#memory-writes)). It mints IRIs for new entities, refuses unknown
  predicates and classes, invented IRIs and likely duplicates with suggestions,
  supersedes old values while keeping their record, and makes retries with an idempotency
  key write nothing. Each call is one commit on the normal write path.
* `memory.json` names a dataset's memory graphs, and `recall` marks facts found only there
  as unreviewed. `reviewedOnly` hides memory graphs from an Ask.
  `sparkles auth grant --template agent` prints grants for an agent that writes its own
  graphs and proposes changes to curated ones on branches.
* `sparkles memory import` and `sync` read the memory files of coding agents
  ([USAGE.md](../USAGE.md#agent-memory), [API.md](../API.md#importing-agent-memory)). They cover
  Claude Code's memory files, `MEMORY.md`, `CLAUDE.md` and rules, Codex's `AGENTS.md` and
  generated memories, `GEMINI.md`, Cursor rules and GitHub Copilot's instructions. Secrets
  are redacted, each file becomes a source, and its facts cite their line span and stay
  unreviewed until a person promotes them.
* A sync writes only what changed. An edit re-anchors facts to where their quotes now
  occur and retracts the rest, and a rename carries them to the new file. With
  `imports.extract: "server"`, a sync starts a server extraction of each changed file.
* Session transcripts of Claude Code, Codex, Gemini CLI, Cursor, the GitHub Copilot CLI
  and Aider, and any file the generic adapter can read, become searchable episodes when
  both the dataset and the person opt in.
* `sparkles memory brief` and `POST /{ds}/memory/brief` print a ranked, bounded and cited
  brief, and `sparkles memory setup claude-code|codex` installs hooks that sync on writes
  and print the brief when a session starts. The `memory` subcommands also cover `init`,
  `sources`, `status`, `recall`, `query`, `assert`, `forget`, `inbox`, `review`,
  `promote`, `reject` and `export --sources`.
* A consolidation task asserts once, in the consolidated graph, the facts that several
  agent graphs repeat and no reviewed graph asserts, and lists duplicates and conflicts
  ([USAGE.md](../USAGE.md#maintaining-agent-memory), [API.md](../API.md#memory-maintenance)). A
  retention task deletes old session graphs whose facts all appear in reviewed graphs.
  Both run on demand or on a schedule and support dry runs.
* The server counts what waits in each review inbox and reports it in metrics, the
  maintenance answer, a badge in the UI, the brief and the MCP resource
  `sparkles://{ds}/memory/review`. `recall` takes `recency`, a half-life that ranks recent
  and corroborated facts first.

### Document ingestion

* An agent registers a document's text as a source with `register_source`, reads it back
  with `read_chunks`, and writes facts with `assert_facts` and a `span` whose quote the
  server checks against the text
  ([USAGE.md](../USAGE.md#ingesting-documents-and-reviewing-memory),
  [API.md](../API.md#review-inbox)). A changed text makes a new rendition, and `retractStale`
  retracts facts it no longer supports. Ingest profiles live in `/$/ingest/{ds}`.
* The review inbox, `GET /$/memory/{ds}/inbox`, lists unreviewed agent facts with span,
  link, guard and corroboration signals and the open review branches. `promote`,
  `reject`, `relink` and `edit` act on them as the caller.
* `POST /$/ingest/{ds}` and `sparkles ingest` ingest a Markdown, HTML, plain-text, PDF,
  Word, PowerPoint or OpenDocument document as a task
  ([USAGE.md](../USAGE.md#ingesting-documents-in-the-server), [API.md](../API.md#ingestion)).
  The server converts it to Markdown, asks the `extract` model role for facts in the
  profile's vocabulary, finds each quote to compute its span, links names to existing
  entities, and writes the facts on a review branch. `mode: "preview"` holds them for
  approval, and `mode: "auto"` merges them when every check passes.
* PDF conversion uses pdf-inspector with the `pdf` cargo feature, on by default, and the
  `pdf-ocr` feature reads scanned pages with OCR models that the operator provides. Office
  files are read by `sparkles-office` under the `office` feature, on by default. On Linux
  each conversion runs in a child process with an address-space limit, which limits
  resources but is not a sandbox.
* A CSV, TSV, Excel or OpenDocument spreadsheet gets a CSVW mapping draft for
  `sparkles csv` per sheet, written by a model from the header and 20 sample rows, with
  the triples of a dry run. IRI columns are linked to existing entities through a lookup.
* `scripts/eval-ingest` scores an ingestion of `testsuite/ingest/sample.json`, and
  `scripts/eval-text2kg` runs Text2KGBench.

### Community reports (GraphRAG)

* A GraphRAG build groups a projection's resources into the Leiden hierarchy and has the
  `community` model role write one report per community, with findings that cite their
  facts ([C22](../specs/C22-graph-rag-community-summaries.md),
  [USAGE.md](../USAGE.md#community-reports-graphrag), [API.md](../API.md#graphrag)).
* Reports are triples with provenance and review status in a reports graph, which a reader
  sees only when its access covers everything the build read.
* A build reports an upper bound of its tokens and waits for confirmation, and reserves
  every request against per-build and daily budgets. A refresh rewrites only the reports
  of communities that changed, and `refresh.mode: auto` starts one when enough facts
  change.
* Ask answers questions about the whole dataset from the reports by map-reduce, and falls
  back to a query for callers who may not read them. The MCP tool `community_reports`,
  `recall` with `communities`, the brief, `sparkles graphrag` and the UI's Communities tab
  expose them.

## Known gaps

These limits name the roadmap items that address them. Current priorities and
dependencies are recorded in the [central roadmap](../ROADMAP.md).

### AI and agents

* ChatGPT-plan model access is planned in [C28](../specs/C28-chatgpt-model-access.md),
  roadmap item 344. The existing OpenAI-compatible provider uses Chat Completions with
  configured API credentials. It does not accept a ChatGPT subscription as model access.
* Ask has been evaluated with Anthropic models only, on the demo set, CK25, QALD-9-plus
  and 74 DBpedia-only questions
  ([C18](../specs/C18-natural-language-questions-and-ingest/OUTCOME.md#roadmap-follow-ups-2026-10-12),
  [BENCHMARKS.md](../BENCHMARKS.md#natural-language-questions)). On the retained 107-question
  DBpedia repeat, Sonnet answers 31 under projected-column scoring and 9 with equal-width
  scoring. Haiku answers 26 and 23 respectively. These
  repeated regression questions do not measure unseen generalization. Model-dependent
  accuracy and contextual namesake resolution remain roadmap item 263, and the higher
  median end-to-end latency is item 318. Local and OpenAI-compatible models, LC-QuAD 2.0,
  which needs a Wikidata load from the full dump, and an evaluation of independent roles
  have not run. Roadmap item 6.
* The UI's ingestion controls do not turn on assistant ingestion. Roadmap item 77.
* An MCP tool operation that needs admin, such as deleting a protected branch, refuses an
  OAuth token without `sparkles:admin` as a tool error instead of a scope challenge.
  Roadmap item 270.
* Agent memory does not import from Cursor's SQLite store or the editors' Copilot Chat
  storage, and redaction finds only the secrets its patterns describe. Roadmap item 15.
* The server does not convert images or audio. Scanned PDF pages need a build with
  `pdf-ocr` and OCR models from the operator. The Office converter is not registered with
  the query memory pool, and a workbook's token estimate for `confirmTokens` does not
  cover all its sheets. Roadmap items 15 and 77.
* A CSV mapping draft links an IRI column's values only to entities whose labels match
  exactly or after folding case and spacing. A value with several matches keeps a minted
  IRI until a person chooses, and the lookup reports only the first variant of an
  ambiguous group. Roadmap item 15.
* A shared profile for source revisions, unasserted claims, evidence, review and identity
  decisions is planned in [C25](../specs/C25-evidence-and-assessments.md), roadmap item 81.
  Current memory provenance and supersession do not provide that complete profile.
* Durable jobs, worker leases, collection checkpoints and a transactional delivery outbox
  are planned in [C26](../specs/C26-durable-jobs-and-delivery.md), roadmap item 339. Current
  server tasks and operator notifications do not provide the proposed recovery contract.
* Maintained analysis records with reproducible inputs, read dependencies and guarded
  result publication are planned in [F22](../specs/F22-analysis-lifecycle.md), roadmap item
  340. Existing reasoner and view maintenance does not provide that application lifecycle.
* Valid-time profiles, uncertain interval matching, availability cutoffs and an event-time
  control are planned in [C27](../specs/C27-valid-time-profiles.md), roadmap item 341.
  Current time travel selects retained commits, with domain validity queried as data.
* A dataset has one GraphRAG configuration, so readers with different views cannot each
  have reports built from their own view, roadmap item 100. The evaluation of global
  answering covers one generated question set on the demo graph, compares it with BM25
  rather than vector retrieval and has no samples checked by a person, items 260 and 261.
  `auto` mode answers about half of the questions about the whole graph locally, item
  262. An automatic refresh counts changes from the change feed rather than from an
  incremental view, item 12.

## Configuration and usage

The following guides describe configuration and supported UI, command-line, HTTP
or library workflows. Exact HTTP contracts remain in the API reference.

* Read [MCP server (LLM agents)](../USAGE.md#mcp-server-llm-agents).
* Read [Connecting with OAuth](../USAGE.md#connecting-with-oauth).
* Read [Asking questions with a model](../USAGE.md#asking-questions-with-a-model).
* Read [Agent memory](../USAGE.md#agent-memory).
* Read [Maintaining agent memory](../USAGE.md#maintaining-agent-memory).
* Read [Ingesting documents and reviewing memory](../USAGE.md#ingesting-documents-and-reviewing-memory).
* Read [Ingesting documents in the server](../USAGE.md#ingesting-documents-in-the-server).
* Read [Community reports (GraphRAG)](../USAGE.md#community-reports-graphrag).
* Read [Natural-language questions](../API.md#natural-language-questions).
* Read [Asking in the server](../API.md#asking-in-the-server).

## Roadmap

The remaining work described above refers to [item 6](../ROADMAP.md), [item 12](../ROADMAP.md), [item 15](../ROADMAP.md), [item 77](../ROADMAP.md), [item 81](../ROADMAP.md), [item 100](../ROADMAP.md), [item 260](../ROADMAP.md), [item 261](../ROADMAP.md), [item 262](../ROADMAP.md), [item 263](../ROADMAP.md), [item 270](../ROADMAP.md), [item 318](../ROADMAP.md), [item 339](../ROADMAP.md), [item 340](../ROADMAP.md), [item 341](../ROADMAP.md), [item 344](../ROADMAP.md).
The central roadmap owns priority and dependencies. Its
[completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs)
link partially complete specs to their remaining work and acceptance criteria.
