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
  scratch branches, and a dataset's `memory.scratchBranches.ttl` overrides it for that
  dataset. `sparkles mcp --loc` applies the dataset setting as well.
* `link_entities` can include versioned evidence about the named types and literal
  labels retained for each candidate, with separate flags for display, per-entity and
  query limits. It reports values and counts from the caller's query view. Bounded text
  and vector search do not establish a complete candidate universe or unique intent. Full
  evidence fields are counted before formatting and share the configured result-byte
  allowance across mentions. The final check also includes branch and policy metadata.
  Query memory limits govern lookup and ranking state.
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
  matching the question against every visible class and predicate. Recognized candidate
  evidence preserves full choice identities and can order explicitly named kinds using
  known ontology types. Full retained types guard resource preferences against disambiguation
  types beyond the display limit. An ambiguous initial request still asks for a clarification, because
  bounded search does not prove unique intent. The generic `link_entities` verdict is
  unchanged. If rich evidence exceeds the result-byte allowance, Ask retries ordinary
  linking once under the same deadline and permissions and keeps its display rule.
* The extra kind preference recognizes the English words film, movie, book, novel,
  song, album, city, town, country, river and person through supported DBpedia,
  schema.org and FOAF class IRIs. Unknown class namespaces and incomplete or competing
  type evidence retain conservative clarification choices.
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

### Ask stage diagnostics

Set `RUST_LOG=warn,sparkles::ask::profile=debug` to record bounded stage timings
through the existing log and tracing configuration. Local Ask records grounding,
common and question-ranked schema retrieval, examples, entity linking, optional
community recall, query checking and execution. Each completion records one call,
inclusive nanoseconds, a finite outcome and error category, and the selected commit
when the tool returns one. Each scoped completion also records a process-generated numeric pipeline identifier
and the context-window size when selected. These remain in JSON logs without
ancestor span lists. Standalone tool stages have no Ask identifier. These are diagnostic fields, not metric labels.
The added fields contain no questions, labels, entity IRIs, prompts or error messages.

Nested stages identify example-query and example-document embedding preparation and
linker vector search. These include caches, admission and any local or remote model
work. Vector search also includes query execution. Parent timings include their child
stages and must not be summed together. These diagnostics do not change the API's
`elapsedMs` or `modelMs`. The latter remains the inclusive Ask role-call time and does
not include embeddings performed inside tools.

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

Recall citations identify a registered passage's document source through visible
rendition metadata. A source IRI may differ from the fact's graph IRI. When several
documents apply, the singular source field prefers relationships in the fact's graph,
then elsewhere in the caller's view, with lexical IRI ordering within each group.
Opaque legacy sources follow those documents, while recognized passages and visible
derivation reifiers are excluded from that fallback. This is a bounded provenance
representative and does not rank evidence or establish complete provenance. Historical
and branch recall use their own metadata snapshots, and all metadata reads respect
the caller's view without loading source text or chunks.

The deterministic [project handover corpus](../../testsuite/memory/acceptance/README.md)
checks current MCP and HTTP memory workflows with independently specified source
occurrences, correction, withdrawal, retained-commit recall, graph visibility, branch
separation and citation fidelity. It uses explicit entity seeds and does not establish
retrieval quality, model accuracy or latency. Shared-profile independence, accepted
support ownership and dependent freshness remain under items 81 and 340.

Idle scratch branches expire after the dataset's `memory.scratchBranches.ttl`, over HTTP
and in a `sparkles mcp --loc --allow-update` session. A limit per principal, warnings
before a branch expires and a way to keep one branch remain under item 431.

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
  `pdf-ocr` feature is included by default. Scanned pages use OCR models and native
  libraries that the operator provides. Office
  files are read by `sparkles-office` under the `office` feature, on by default. On Linux
  each conversion runs in a child process with an address-space limit, which limits
  resources but is not a sandbox.
* A CSV, TSV, Excel or OpenDocument spreadsheet gets a CSVW mapping draft for
  `sparkles csv` per sheet, written by a model from the header and 20 sample rows, with
  the triples of a dry run. IRI columns are linked to existing entities through a lookup.
* `scripts/eval-ingest` scores an ingestion of `testsuite/ingest/sample.json`, and
  `scripts/eval-text2kg` runs Text2KGBench.

### Evidence profile

The version 1 [evidence resources](../../crates/sparkles/resources/evidence/v1/README.md)
provide an RDF vocabulary, SHACL shapes, competency queries and read-only views of
existing memory records. Rust applications select them with
`sparkles::evidence::profile(1)`. Unknown versions return `None`. The CLI can validate
profile RDF with `sparkles shacl` and run the supplied queries with `sparkles query`.

Rust configuration adapters can decode `sparkles_core::evidence::Settings` and obtain
canonical bytes and a digest. These helpers enforce version and graph-set bounds but
do not select an authoritative configuration or authorize managed writes.

Rust importers can prepare typed batches with `sparkles_core::evidence::prepare_input`.
It checks local content digests and retained text or JSON selectors within its bounds.
The result keeps explicit requirements for references needing authorized runtime
resolution. JSON selection uses the named lexical UTF-8 method and preserves exact
retained value bytes. These local helpers provide no authenticated write or receipt.

Trusted Rust adapters can retain actual catalog openings with
`sparkles::managed_source::ResidentSourceOwner`. The lease keeps the original Dataset
and resident branch alive without resolving a fresh name or opening a cold branch.
Catalog disposal checks shared runtime ownership and preserves original owners on refusal.
The [integration guide](../integrations/README.md#libraries-and-bindings) describes raw
Strong and Weak aliases. These lifetime checks grant no read permission or durable retention.

The Rust controlled permission helper can prepare graph, predicate and class masks
and the built-in disclosure guard under one memory cap and `Control`. It explicitly
refuses unsupported providers, unresolved providers nested in retained rules and
protection-pattern planning. C25 exact-reference
resolution and guarded Dataset queries still require caller adoption under
[item 395](../ROADMAP.md). A prepared visibility view provides no source authenticity,
managed-write authority or proof that an unavailable reference is absent.

Source revisions, renditions and passages retain exact identities and digests. Claim
occurrences describe a proposition with an RDF 1.2 triple term without asserting it.
Evidence links distinguish support, contradiction and mention. Declared lineage stays
separate from an assessment of independence. Qualified confidence retains its evaluator
and scale. Domain scope remains application RDF.

These resources are ordinary caller-visible data under the existing access policy.
Their shapes check structure, while the fixtures independently check source bytes and
selectors. Loading a review decision or receipt does not authenticate it, admit facts
or enable execution. Existing `assert_facts` and source registration keep their current
behavior. [C25](../specs/C25-evidence-and-assessments.md#outcome) records the exact phase
boundary and remaining managed operations.

### Shared entity representation

[A01](../specs/A01-shared-entity-representation/README.md) provides versioned RDF
resources for shared entities, source descriptions, scoped identifiers and reviewed
resolution maps. Source revisions, world changes, identity decisions and ontology or
mapping migrations retain separate identities and history. The profile reuses C25
claims and C27 temporal records.

In Rust, `sparkles::shared_entity::profile(1)` returns the vocabulary, structural
shapes, matching policy, digest manifest and eight competency queries. Unknown
versions return `None`. Selecting these static resources opens no dataset. The
[resource package](../../crates/sparkles/resources/entity-representation/v1/README.md)
and [fixed corpus](../../testsuite/entity-representation/v1/README.md) provide software,
organization, place, event and aggregate-observation examples with exact query
answers, retained source bytes and explicit unknowns. They can be loaded and queried
with the existing RDF, SHACL and SPARQL interfaces.

The private [offline example](../../crates/sparkles/examples/shared_entity_replay.rs)
computes resolution from explicit view manifests, entities, proposals and selected
review declarations. It preserves historical decision inputs, reports conflicts across
entire connected components and leaves undecided matches unknown. Authored assignment and conflict rows
are excluded from matching. Representatives belong to the selected view and do not
assert logical equality.

Run the comparison CLI from the repository root with explicit local deliveries.

```sh
mbx run --locked -p sparkles --example shared_entity_replay -- \
  --view urn:example:a01:A12:pair --scope urn:example:a01:A12 \
  --rdf ttl=testsuite/entity-representation/v1/valid/common.ttl \
  --rdf ttl=testsuite/entity-representation/v1/valid/resolution.ttl \
  --rdf ttl=crates/sparkles/resources/entity-representation/v1/policies/default.ttl \
  --source urn:example:a01:r1=testsuite/entity-representation/v1/raw/r1.json \
  --deadline-ms 10000
```

The CLI accepts Turtle, N-Triples, TriG and N-Quads through `ttl`, `nt`, `trig` and
`nq` format names. It checks retained source digests and mandatory evidence selectors
reached through selected records and their domain dependencies. `--reference` adds
an explicit selector check. Replay refusals produce a stable code before comparison
bytes are emitted. The tool performs no network fetch, file discovery, dataset write
or current-pointer update.

Default limits are 10,000 entities, 20,000 descriptions, 50,000 claims, 20,000 decisions
and 50,000 relations. Decoded deliveries share a 64 MiB invocation cap, retained source
files have a 16 MiB cap each and operation memory is capped at 256 MiB. The
`--limit-entities`, `--limit-descriptions`, `--limit-claims`, `--limit-decisions`,
`--limit-relations`, `--limit-decoded`, `--limit-source` and `--limit-memory` flags
accept lower caps. Supply byte limits as integer byte counts. The memory accounting
charges each delivery about seven times its size before its statements, so the memory
cap refuses deliveries above about 36 MiB before the decoded cap applies. Replay builds
only complete views and refuses a missing input with `incomplete-input`, so its only
unknown code is `possible-match`. `--deadline-ms` sets a
relative deadline, and private helpers also honor the shared cancellation control. A
refusal emits no
partial comparison.

Private helpers compute the fixed rule for occurrence withdrawal, source lineage groups,
simulated availability cutoffs, mapping migrations and terminal observation revisions.
The availability rule accepts canonical fixed-width UTC timestamps. The observation
adapter supports reported positive occurrences in the declared synthetic series and
selects its unique terminal source revision. Unsupported headers or rules refuse or
remain explicit unknowns. The separate Cargo.lock version 4 adapter checks supplied
family and version bindings against the existing software-inventory bytes, whose
manifest digests a test recomputes. Roadmap item 407 owns the review of that corpus. These
bindings do not establish release, build, artifact or deployment evidence. The CLI
emits resolution comparisons, while these evolution and mapping helpers are exercised
through the [replay tests](../../crates/sparkles/tests/shared_entity_replay.rs).

All review and availability declarations in this reference remain simulated. A
separately supplied public scope demonstrates isolation without implementing real
authorization. Roadmap item 396 retains managed integration with actual C25 review and
protected records, C27 authenticated availability, C26 durable publication and F22
freshness. Synthetic count examples define no operational health policy or connector.

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

### Valid-time profile

The version 1 [valid-time resources](../../crates/sparkles/resources/valid-time/v1/README.md)
provide an RDF vocabulary, SHACL shapes, an import normalization contract and typed
stored-query definitions. The library exposes them through
`sparkles::valid_time::profile(1)`. Unknown versions return `None`. Applications can
install the active-claim and overlapping-claim templates through the existing stored
query API and apply the shapes using ordinary SHACL validation.

Intervals are half-open and preserve uncertain, unknown and explicitly unbounded
endpoints. Query rows retain the claim, source revision, interval, temporal verdict
and normalization status. A temporal match does not assert the claim's proposition
or establish its acceptance. Recorded history still selects an actual retained commit;
valid-time parameters do not change the meaning of `at`.

C27 Phase 1 remains incomplete. The resources cannot authenticate local availability
or refuse client backdating of an ordinary RDF timestamp. That A4 acceptance requirement
needs C25's planned server-owned receipts and the shared protected-write boundary, or
a separately reviewed runtime helper. Publication dates and arbitrary receipt triples
do not establish a knowledge cutoff. The projection helper, event-time UI and expiry
integration remain planned under item 341.

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
  [BENCHMARKS.md](../benchmarks/natural-language/RESULTS.md#natural-language-questions)). On the retained 107-question
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
* The server does not convert images or audio. Scanned PDF pages need configured
  OCR models, PDFium and ONNX Runtime. The standard binary includes the OCR code. The Office converter is not registered with
  the query memory pool, and a workbook's token estimate for `confirmTokens` does not
  cover all its sheets. Roadmap items 15 and 77.
* A CSV mapping draft links an IRI column's values only to entities whose labels match
  exactly or after folding case and spacing. A value with several matches keeps a minted
  IRI until a person chooses, and the lookup reports only the first variant of an
  ambiguous group. Roadmap item 15.
* The evidence resource profile does not supply authenticated typed writes, protected
  availability metadata, retained-reference resolution, review state machines, accepted
  projections or reversible identity operations. These remain in
  [C25](../specs/C25-evidence-and-assessments.md), roadmap item 81, followed by recall,
  ingestion, reporting and maintained-analysis integration. Ordinary RDF shape validation
  does not establish these runtime guarantees.
* Durable jobs, worker leases, collection checkpoints and a transactional delivery outbox
  are planned in [C26](../specs/C26-durable-jobs-and-delivery.md), roadmap item 339. Current
  server tasks and operator notifications do not provide the proposed recovery contract.
* Maintained analysis records with reproducible inputs, read dependencies and guarded
  result publication are planned in [F22](../specs/F22-analysis-lifecycle.md), roadmap item
  340. Existing reasoner and view maintenance does not provide that application lifecycle.
* The [valid-time profile](#valid-time-profile) supplies static resources and temporal
  queries. C27 Phase 1 remains incomplete until server-owned availability and backdating
  refusal pass A4, dependent on C25 Phase 2 and protected writes. The runtime projection
  helper, event-time control and expiry integration remain roadmap item 341.
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
* Read [Evidence profile resource usage](../../crates/sparkles/resources/evidence/v1/README.md).
* Read [Maintaining agent memory](../USAGE.md#maintaining-agent-memory).
* Read [Ingesting documents and reviewing memory](../USAGE.md#ingesting-documents-and-reviewing-memory).
* Read [Ingesting documents in the server](../USAGE.md#ingesting-documents-in-the-server).
* Read [Community reports (GraphRAG)](../USAGE.md#community-reports-graphrag).
* Read [Natural-language questions](../API.md#natural-language-questions).
* Read [Asking in the server](../API.md#asking-in-the-server).

## Roadmap

The remaining work described above refers to [item 6](../ROADMAP.md), [item 12](../ROADMAP.md), [item 15](../ROADMAP.md), [item 77](../ROADMAP.md), [item 81](../ROADMAP.md), [item 100](../ROADMAP.md), [item 260](../ROADMAP.md), [item 261](../ROADMAP.md), [item 262](../ROADMAP.md), [item 263](../ROADMAP.md), [item 270](../ROADMAP.md), [item 318](../ROADMAP.md), [item 339](../ROADMAP.md), [item 340](../ROADMAP.md), [item 341](../ROADMAP.md), [item 344](../ROADMAP.md), [item 396](../ROADMAP.md).
The central roadmap owns priority and dependencies. Its
[completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs)
link partially complete specs to their remaining work and acceptance criteria.
