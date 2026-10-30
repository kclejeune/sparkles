# Roadmap

This roadmap prioritizes making Sparkles the best graph database by objective measures.
These are correctness, conformance, performance, robustness and the standard features.
Embedded agent memory and entity representation follow, and applications built on top of
Sparkles, such as the supply-chain pilot, come after them. New features enter the delivery
path when a concrete use case or an adopted dependency requires them. A separate analyst
workbench follows these goals.

Existing storage, query, reasoning, search, security and recovery capabilities retain
their required phases and acceptance gates. Close competitive results can be accepted
with their measurements and tradeoffs recorded. Packaging, recovery, installed-artifact
testing and the API compatibility contract advance alongside capability completion.

The [feature index](FEATURES.md) and its category guides describe what Sparkles does
and does not do today. Every known gap names the roadmap item that addresses it.
[BENCHMARKS.md](BENCHMARKS.md#where-sparkles-loses) records the measured results and the
workloads where Sparkles loses. This document holds only the work that remains.

## Contents

- [How the roadmap is organized](#how-the-roadmap-is-organized)
- [Delivery priorities](#delivery-priorities)
- [Tracks and decisions](#tracks-and-decisions)
- [Completion paths for partially complete specs](#completion-paths-for-partially-complete-specs)
- [Tier 1: now](#tier-1-now)
- [Tier 2: high-impact features and the scale foundation](#tier-2-high-impact-features-and-the-scale-foundation)
- [Tier 3: big bets](#tier-3-big-bets)
- [Tier 4: worth doing, not urgent](#tier-4-worth-doing-not-urgent)
- [Tier 5: backlog](#tier-5-backlog)
- [Tier 6: later operational completion](#tier-6-later-operational-completion)
- [Tier 7: compatibility leftovers](#tier-7-compatibility-leftovers)
- [Completed](#completed)
- [Keeping this current](#keeping-this-current)

## How the roadmap is organized

The work is grouped into seven tiers. Complete the adopted requirements of partial
capabilities and the selected release contracts before unrelated new breadth, including
requirements whose follow-ups appear in a lower tier. The tiers order eligible phases
and extensions by effort, dependencies and impact on the goals above. Independent phases
can run in parallel. A deferred adopted requirement keeps its parent capability partial.

Each item has a number that stays fixed for its lifetime. Specs, FEATURES and code
comments refer to items by these numbers, so a number is never reused or changed. The
numbers are not a ranking, and a new item takes the next free number whatever its tier.

Implementation status is recorded in the [capability matrix](FEATURES.md#capability-status).
Complete means the stated scope is available. Partially complete means a usable subset
is available with remaining work named. In progress means implementation has started
but is not available on the main branch. Planned means implementation has not started.
A written spec alone is planned. Completed scope belongs in the list below even when
an extension to the same capability remains in a tier.

Each row says what remains to be done and links to the spec that holds the design. Items
that extend an existing spec are designed in a "Roadmap follow-ups" section at the end of
that spec's Outcome, and the Outcome also records what has already landed. An item that
is partly finished stays in its tier, and its row describes only the remaining part.
Finished items are listed under [Completed](#completed).

The Size column estimates the remaining effort.

| Size | Meaning |
|---|---|
| S | Days |
| M | One to two weeks |
| L | Several weeks |
| XL | A quarter or more |

The Value column in Tiers 1 to 3 rates the impact of an item on the goal. Lower tiers are
ordered by their tier alone.

## Delivery priorities

The goal comes first. Sparkles should be the best graph database by objective measures,
which are correctness, conformance, performance, robustness and the standard features.
Embedded agent memory in C17 and C18 comes second, and shared entity representation in
A01 and C25 comes third. Applications built on top of Sparkles, such as the cyber and
public-health supply-chain pilot, come after these. The engine supplies generic
primitives, and domain adapters and matching logic stay application work rather than
engine infrastructure.

Complete named spec phases and their acceptance gates. A parent capability remains
partially complete while adopted requirements remain. Building an application on a
usable subset does not waive those requirements. Correctness, recovery, bounded
resources and source identity go ahead of unrelated feature expansion.

### Wave A: engine quality

Wave A holds the work that most improves the engine against objective measures.

* Competitive benchmark losses come first. The order is SP²Bench `q11`, DBpedia
  births-by-decade, then BSBM Explore `q7` and `q8` and BI `q3`, as
  [BENCHMARKS.md](BENCHMARKS.md#where-sparkles-loses) records them. Durable commits and
  mixed-load writes follow through items 86, 111, 112 and 171, and cold point lookups
  through item 4.
* Streaming execution and spill follow through items 7 and 93.
* The Stage A lanes of item 17 are items 413 to 417. They deliver the safety net,
  internal circuits, the provenance algebra, reasoner witnesses and the fixed-point
  operator.
* Small lanes that finish a capability run in free capacity. These are item 48, items 90,
  57, 277 and 77, and the CSVW and RML conformance gaps in items 235 and 232.

### Wave B: memory and entities

Wave B starts with item 81 Phase 3, then items 341, 396 and 57 and the remaining C18
work. Item 18 Phase 3 and item 12 follow once the item 17 lanes they need have landed.
Item 12 needs items 413 and 414, and item 18 Phase 3 needs items 415 and 416.

### Production readiness

Production readiness runs alongside both waves. It covers items 60, 59 and 58 for the
API policy, server artifacts and packages, item 61 for backups and item 65 for
encryption, together with items 402 and 400. Item 64 and the adopted phases of 65 close
startup and key workflows.

### Deferred

Items 339 and 340 for durable jobs and maintained analyses, item 100 for report
configurations, the supply-chain application, item 20 for openCypher, item 72 for JSON
query languages and item 344 for ChatGPT-plan model access wait for the work above. A
prerequisite can bring one of them forward when a concrete use case needs it.

### Sequencing

Write-path competitive work goes before the commit-time maintenance lane of item 17,
which is item 421, because both change the commit path. Item 421 then measures its cost
against the improved baseline.

### Acceptance by milestone

| Milestone | Work | Acceptance |
|---|---|---|
| Reliable baseline | Close confirmed correctness failures and retain qualified measurements. | The integration passes its required review, tests, conformance and performance gates. |
| Production readiness | Items 58–60 complete packages, server artifacts and compatibility policy. Item 402 and the adopted phases of 65 close startup and key workflows. | Installed artifacts, supported platforms, upgrade and recovery tests pass before a release is cut. |
| Shared capability completion | Close the relevant search, path, view, reasoning, explanation, validation, API and visualization phases from the completion paths below. | The declared scope passes its correctness, visibility, recovery and measured resource gates. |
| Trusted sources and resumable work | Item 81 delivers C25 Phases 1–2, and item 341 delivers C27 Phase 1. Item 339 delivers C26 Phase 1 when it is taken up. Backups and clones already carry view definitions paused, and C26 extends that to jobs and deliveries. | Source revisions and decisions retain their identity and evidence. Interrupted work resumes safely, and restored external work stays paused. |
| Shared entity representation | Bounded private offline replay and selected fixture mappings are available over the entity, identifier, resolution and evolution resources. Item 396 retains authenticated managed integration. | Independent query fixtures preserve source claims through conflicting identifiers, merge and split decisions, source correction and mapping migrations. |
| Reproducible assessments | Item 340 delivers F22 Phase 1 after C25's typed records and C26's fenced publication are available. Full recomputation is the initial execution model. | Inputs, outputs and methods are reproducible. New matching data, source withdrawals, expiry and authorization changes invalidate affected results. |
| Unattended operation | Complete C26 Phase 2 and F22 Phase 2, including temporal scheduling. Item 100 supplies guarded report configurations when distinct audiences are required. | Fenced database publication rejects stale attempts and duplicate accepted database effects. External delivery is at least once with stable event IDs, so receivers deduplicate repeated deliveries. Recovery preserves current authorization. |
| Full partial-feature completion | Finish the other adopted partial-feature phases and release contracts, including explicitly scheduled large phases. | Required feature, compatibility, packaging and operational acceptance are complete before release. |

Item 396 defines shared entities and domain mappings without duplicating C25 review
operations, C27 time semantics or F22 dependency maintenance. Its first phases use
ordinary RDF, SHACL and SPARQL with pinned evidence and expected answers. Live managed
integration follows the exact C25, C26, C27 and F22 contracts it consumes.

C25's first phase supplies vocabulary, shapes, fixtures and competency queries. It does
not supply the managed runtime APIs of later phases. C26 Phase 1 and C27's static
profile resources can proceed independently. Full C27 Phase 1 additionally requires
A4's authenticated availability and backdating refusal through C25 Phase 2 and the
shared protected-write boundary. F13-backed incremental execution and F18-supported inferred
visibility follow their explicit prerequisites. Initial analyses use full recomputation
and authorized projections, with the required freshness and access checks. Exact retained
references use item 395 for bounded history and permission preparation.

Optimize measured ingestion, dependency joins, paths, refresh and retrieval costs, and
the competitive losses that Wave A lists. Preserve durability, cancellation and
visibility. Broader competitive refreshes that do not name a recorded loss remain
tracked without driving this delivery sequence.

Existing partial UI and profiling contracts remain in the completion program and can
follow the shared knowledge and production phases. New query-by-example, notebook,
query-language, local-first and provider conveniences follow demonstrated use-case need.
A prerequisite can bring a bounded planned feature forward. Conditional integrations do
not delay independent core phases, and retain their declared dependencies.

Virtual graphs in F19 and replication in F10 are substantial later phases of partial
specs. They remain visible completion work with explicit phase boundaries. They are not
prerequisites for the first memory workflow.

## Tracks and decisions

Four tracks run through the tiers. Dependencies apply to the named remaining phases,
not to every extension in a spec or to phases that have already shipped.

| Track | Open items | Specs |
|---|---|---|
| **Performance** | 1 refreshes the evidence while 7, 86 and 4 address their remaining gates. Item 93 controls the spill default, and 111, 112 and 171 close the indexed group-commit gates. Write-path work goes before item 421. | X06, X08 Phase 2, X09 Phase 4, X07 |
| **Graph features** | 8 closes algorithm surfaces and acceptance. Item 12's view-based refresh waits for 413 and 414. | F14, C22, F13 |
| **Reasoning** | 9 and 19 complete their remaining scale and query-time phases. Item 18 Phase 3 waits for 415 and 416, and item 19 Phase 3 waits for 415 and 417, while existing explanations remain available. | F16, F17, F13, F18 |
| **Evidence and maintained analyses** | 81 and 339 can run independently, and both feed 340's shared representation and durable execution. Item 396 specializes the available evidence and temporal profiles with entity modeling and offline fixtures. Item 341 adds validity, and later incremental maintenance uses the lanes of item 17. | C25, C26, A01, F22, C27, C22b |

The following dependencies describe the remaining phase contracts. Independent work can
proceed in parallel, including full recomputation of maintained analyses before later
incremental execution exists.

```mermaid
flowchart LR
    E["81 · Shared evidence"] --> A["340 · Maintained analyses"]
    J["339 · Durable jobs and delivery"] --> A
    L0["413 · Safety net and load checks"] --> L1["414 · Internal circuits"]
    L0 --> L2["415 · Provenance algebra"]
    L2 --> L3["416 · Reasoner witnesses"]
    L2 --> L4["417 · Fixed-point operator"]
    L0 --> R["12 · View-based report refresh"]
    L1 --> R
    L2 --> V["18 · F18 Phase 3 supported visibility"]
    L3 --> V
    E --> V
    L2 --> T["19 · F17 Phase 3 maintained tables"]
    L4 --> T
    W["write-path competitive work"] --> L8["421 · Commit-time table views"]
    P222["222 · View access by grant containment"] --> L11["424 · MCP"]
    P222 --> L13["426 · UI"]
    P223["223 · Parallel view passes"] --> L14["427 · Per-branch views and keepCommits"]
    B["280 · Replication decisions"] --> D["66 · Replicas and recovery"]
    D --> S["66 · Synchronous replication and fenced failover"]
```

Item 226 goes with item 414, because the change ring is copied for every commit once an
internal circuit consumes it. Item 223 comes before item 427, and item 222 comes before
items 424 and 426. The other item 17 lanes in Stage B and Stage C depend on item 413 and
are listed in the [lane table](#lanes-of-item-17).

Item 19 Phase 2 does not wait for F13 Phase 3. Its `auto` mode must account for the
materialization states available when it lands. Item 100 Phase 1's report configurations
and item 341's validity profile can proceed independently. Item 100 Phase 2 uses 339 and
340 for durable refresh, without holding up existing GraphRAG refresh. Items 58 to 60
prepare the release, and item 65's new encrypted formats use the existing X12 format
registry and compatibility window.

Use free capacity for independent completion phases and application acceptance work.
Item 14's adaptive compaction policy uses sustained-write measurements from item 1;
shared operation progress can proceed independently. Measurements select necessary
execution improvements, while feature order follows the delivery milestones above.

## Completion paths for partially complete specs

Every partially complete entry in the [spec index](specs/README.md) has a path below.
The linked Outcome distinguishes remaining implementation from historical omissions
that later shipped. Each exit condition includes the spec's applicable acceptance,
conformance, access-control, recovery and measured performance gates. Finishing code
without its required validation does not complete that scope.

Complete means the accepted contract is delivered. Optional extensions need an explicit
scope decision, and rejected experiments retain their measured rejection rather than
becoming implementation requirements. A benchmark loss alone does not prevent completion
when the bounded work has improved the material gaps and recorded accepted close cases
and deferrals. Any accepted slowdown keeps its named follow-up under the engineering rules.

| Spec | Remaining items | Exit condition |
|---|---|---|
| [C01](specs/C01-observability-and-budgets.md#outcome) | 342, 402 | Startup reports a dataset whose derived index is unavailable as degraded, and the progress of a log replay, without losing readiness or access controls. JSON and OTLP include the Phase 2 work, memory-peak and rebuild series with the Prometheus units, labels and histogram semantics. |
| [C03](specs/C03-graphql/SPEC.md#outcome) | 37 | Complete Phase 2's seeded execution, bounded per-parent windows, nested connections, UI/schema editing and persisted-query controls with differential and visibility tests. Subscriptions are deferred until a workload asks for them, and then use F13. Mutations require the separate scope decision recorded in the Outcome. |
| [C06](specs/C06-clone-to-sandbox.md#outcome) | 45 | Remote and archive clones preserve a chosen branch/commit's data, stored blank identities, prefixes and configuration while creating correct new lineage. Interrupted, corrupt or cancelled transfers publish no destination. |
| [C17](specs/C17-agent-memory.md#outcome) | 57, 81 | Dataset-layered scratch expiry works over HTTP and stdio with the configured limits and notifications. Adopt the shared evidence profile with its source, review and independent-support acceptance queries; server model calls use the successor C18 contract. |
| [C18](specs/C18-natural-language-questions-and-ingest/SPEC.md#outcome) | 6, 15, 27, 77, 81, 90, 263, 318 | Finish the remaining evaluation matrix, multimodal ingestion, redaction and verified rewrites, and test grounding and VALUES changes against caller-visible data. Publish matching accuracy and latency evidence, preserving clarification for ambiguity and the original secret, budget and citation guarantees. |
| [C25](specs/C25-evidence-and-assessments.md#outcome) | 81, 395 | Finish Phase 2's authenticated source, reference and claim operations, protected metadata, retained-view resolution, reader admission, review, support ownership and reversible identity decisions. Phase 3 integrates recall, ingestion, reports and maintained analyses, with exact identity, authorization, retry and crash acceptance. |
| [C27](specs/C27-valid-time-profiles.md#outcome) | 341 | Complete Phase 1 with authenticated availability and backdating refusal through C25 Phase 2 and protected writes. Then add the temporal projection helper, event-time controls and expiry integration with interval, visibility and recovery acceptance. |
| [C21](specs/C21-outbound-notifications.md#outcome) | 46 | Add email, the administration UI and complete backup-failure coverage. A slow destination cannot delay another channel, and delivery tests retain retry, outbound, secret and disabled-channel guarantees. |
| [C22](specs/C22-graph-rag-community-summaries.md#outcome) | 12, 100, 260, 261, 262, 264 | Add refresh counting with an accumulator on an internal F13 circuit, finish person-checked and broader-corpus evaluation, and validate global-question routing and schema-order decisions. Multiple report configurations follow C22b under 100, retaining shared spending and disclosure controls. |
| [F03](specs/F03-full-text-search/SPEC.md#outcome) | 23, 35, 50, 250, 251 | Complete online changes, uncommitted and historical search, multi-field documents and the named analyzer parity gaps. Verify tokens against the adopted Lucene references and preserve restricted-view scoring and query correctness across rebuilds and pins. |
| [F04](specs/F04-vector-search.md#outcome) | 34, 82 | Deliver the chosen quantization modes, result caching and statistics. Measure catch-up, worker sizing and binary16 recall on all named distance distributions, and verify cancellation, version invalidation and memory ownership. |
| [F05](specs/F05-snapshot-repositories/SPEC.md#outcome) | 61, 65, 401 | Restore whole-server and multi-dataset runs with consistent, versioned definitions and paused external work. Complete repository transfer, chunking adoption, optional retained-history capture and storage-retention controls, proving restored retained commits and preserving older backup readers. |
| [F06](specs/F06-snapshots-and-point-in-time.md#outcome) | 23, 52, 395 | Complete pin rebasing, per-quad periods, graph feeds, expired-state reconstruction and settings history, with full-text reads at pins supplied by 23. Compare historical answers and diffs with fresh reconstruction through compaction and retention boundaries. Add bounded replay and permission preparation through item 395. |
| [F07](specs/F07-path-search.md#outcome) | 275, 276, 277 | Add length-bounded weighted search and bidirectional tied shortest paths with the reference path/cost tests. Resolve or explicitly disposition the measured control costs without weakening visit budgets or zero-weight semantics. |
| [F09](specs/F09-branches-and-merges.md#outcome) | 45 | Finish relink from the remote CLI and remote/archive cloning. Compare relink identities and clone contents with offline operations, including chosen commits, lineage, cancellation and corrupt-transfer refusal. |
| [F10](specs/F10-replication.md#outcome) | 74, 280, 66 | Settle the replication contract before implementing replicas, bootstrap, archives, promotion, synchronous acknowledgement and fenced failover. Test exact primary identities, stream gaps, restart and declared data-loss guarantees, and separately complete multi-transaction patch and blank-node semantics. |
| [F11](specs/F11-encryption-at-rest.md#outcome) | 61, 65, 58, 59 | Finish key workflows and recovery, Phase 2 chunking and write-only age-recipient repositories, then live-file and derived-index encryption. Pass the remaining phase acceptance for provider outages, read-authority separation, rotation, locked states and format compatibility. Standard builds include repository encryption with protected keys on Linux and macOS; later phases remain open. |
| [F13](specs/F13-incremental-views-and-provenance.md#outcome) | 17, 413–430, 221, 222, 223, 224, 225, 226, 227, 265 | Complete graph and branch views, commit-time maintenance, transports, provenance and recursion, plus the named access, worker, guard and server-limit gaps. Differential, crash and subscriber/load tests must cover inferred data and measured commit costs. |
| [F14](specs/F14-graph-algorithms.md#outcome) | 8 | Finish Graphalytics acceptance, completion, background progress and the remaining algorithm surfaces. The C23 writer is item 36 and openCypher procedures are item 20, and each belongs to its own capability. Items 79 and 80 extend weighted betweenness as optional work, and the exact refusal boundaries stand until a broader domain is defined and independently validated. |
| [F16](specs/F16-materialization-at-scale.md#outcome) | 9, 113 | Deliver the separate derived layer, canonical equality representatives and the remaining barrier-free and incremental-equality execution. Re-run the scale, differential, datatype and kept-closure memory gates, and settle the named small-change costs with matched measurements. |
| [F17](specs/F17-query-time-reasoning.md#outcome) | 19, 190, 192, 193 | Finish automatic cost selection, hybrid rules, OWL 2 QL, settings and client surfaces, then F13-maintained tables and provenance. Close Phase 1's cache accounting, DESCRIBE and per-query costs with demand/materialization equivalence tests. |
| [F18](specs/F18-explanations-and-entailment-diffs.md#outcome) | 18, 285, 286 | Add F13-backed supported visibility and reviewed-only derivations, result-cell actions and the explicitly extended unbound why-not contract. Compare filtered answers with fresh derivation after support withdrawal and policy changes, including multiple supports and truncated witnesses. |
| [F19](specs/F19-mappings-and-virtual-graphs/SPEC.md#outcome) | 43, 230, 232, 233, 234, 235, 236, 237, 238, 239, 404 | Finish the named source, UI/task, conformance, function and term-identity gaps before claiming Phase 1 complete. Phase 2 virtual graphs must match materialized SPARQL answers with pushdown, access, cancellation and source-failure behavior tested. |
| [G01](specs/G01-geosparql/SPEC.md#outcome) | 24 | Complete per-row nearest-neighbour joins, QLever's spatial syntax, metric-property rewrites, polygon approximations, datum shifts and curved GML segments as item 24 lists them. Keep intentional default divergences explicit, since item 70's Jena-compatible mode is not adopted. |
| [G02](specs/G02-shex/SPEC.md#outcome) | 54 | Complete combined guards, graph-plus-file schemas and CLI outbound controls with the adopted ShEx conformance and incremental/full equivalence tests. Unsupported reference syntax remains a stated language boundary rather than a claim of universal ShEx parity. |
| [G03](specs/G03-shaclc.md#outcome) | 48 | Add a lossless SHACLC parser and comment-preserving formatter across CLI, HTTP and editor, retaining semantic round trips and existing constraint tests. The separately listed SHACL 1.2 draft constraints are outside the adopted node-expression phase. |
| [P01](specs/P01-python-bindings.md#outcome) | 32, 33, 40, 58 | Complete release/platform and further async administration and bulk coverage, reusable plans and the remaining bounded per-call and footprint assessment. Validate package installation and the Python API suite on the claimed runtimes while preserving cancellation, term identity and snapshots. |
| [P03](specs/P03-query-extensions.md#outcome) | 25 | Complete dynamic registered dispatch, foreign callbacks, batching and the adopted aggregate arities. Verify captured-view access, duplicate invocation, error, cancellation and transaction behavior, and measure callback costs. |
| [P04](specs/P04-jvm-bindings/SPEC.md#outcome) | 25, 32, 33, 58, 76, 405 | Complete the named callback, plan, integration-harness, metrics and release gaps with the accepted Jena contract tests. Keep fallback BGP batching and optional module/allocator/transaction choices explicit, and substantiate performance claims with matched bridge measurements. |
| [P05](specs/P05-node-bindings.md#outcome) | 14, 32, 33, 44, 58, 329, 405 | Complete progress callbacks, reusable plans and publication/runtime acceptance while preserving RDF/JS terms, bounded streams and transaction hand-off. Record optional Wasm/UI-migration decisions separately, and retain full-consumption controls for the measured query/export costs. |
| [X01](specs/X01-compression-codecs.md#outcome) | 61, 343 | Use the adopted X11 vocabulary and F05 repository codecs, and record the optional portable-decoder and repository-zstd decisions under the named follow-ups. Preserve format/feature refusal and codec round-trip tests; measured rejected storage codecs are not required to ship. |
| [X05](specs/X05-streaming-query-execution.md#outcome) | 7, 84, 93, 240, 243 | Finish the named fallback-demand, aggregate, early-output, UI and buffering gaps, and settle large-plan and serialization costs. Preserve eager/cursor equivalence, cumulative budgets and disconnect cancellation, widening defaults only after measured adoption gates. |
| [X06](specs/X06-benchmark-coverage.md#outcome) | 1, 8, 91, 290, 291, 292, 293, 299, 306, 328 | Finish the later benchmark batches, 100M BSBM, BI-driver reruns, noise controls and reproducible table regeneration. Retain the named cost checks and comparison inventory, accepting documented close cases without requiring every engine comparison to be a win. |
| [X07](specs/X07-cold-sparse-reads.md#outcome) | 4, 7 | Complete optional prewarm, remaining I/O reporting and the unmeasured billion-scale dense/window and counter-cost gates. Recheck cold sparse targets with controlled eviction, while treating unachieved stretch targets and read-ahead tradeoffs explicitly. |
| [X08](specs/X08-disk-spill-and-server-memory.md#outcome) | 1, 7, 40, 88, 93, 210 | Complete the remaining eager fallbacks, spill execution costs, cache accounting and footprint work, then decide the spill default under 93. Preserve the passed full-scale plain/encrypted matrix and verify ordered-join/grouping costs, one-pass outer joins, disk cleanup, admission and aggregate correctness before broadening defaults. |
| [X09](specs/X09-group-commit-by-default.md#outcome) | 86, 111, 112, 170, 171 | Establish the outstanding spatial serial and mixed-reader gates with valid drive-state calibration, and settle fence and reader-tail costs. Retain durability, crash recovery and the documented legacy-header limits while testing all indexed dataset classes. |
| [X11](specs/X11-index-compression-and-predicate-patterns.md#outcome) | 160, 241, 343 | Add dataset-local settings for the adopted codecs and patterns with lock, compaction and format compatibility tests. Keep FSST optional until its adoption gates pass, address the named lookup costs, and preserve the measured rejection of encoded IRIs, column codecs and the count shortcut. |
| [X12](specs/X12-release-engineering.md#outcome) | 58, 59, 60 | Complete unified versions, artifact publication and platform coverage, then the HTTP/CLI/library stability policy and checks. Release validation must exercise installed artifacts and older-format fixtures, including verbose version/capability reporting and required feature flags. |
| [U02](specs/U02-shape-driven-forms/SPEC.md#outcome) | 21 | Complete atomic nested blank-node changes and the browser editor from the Rust read models, then list editing and `sh:or`/`sh:xone` choices. Complete blank-named graph contexts and admission for model sources, complex nested paths and remaining target and query producers, and adopt item 395's controlled scan/decode seam. Test previews, guards, graph-specific conflicts and branch saves against the candidate state. |
| [U04](specs/U04-live-query-profiling.md#outcome) | 27 | Deliver live registry/progress, cancellation by id, hot spots and stop/explain, then integrate verified C18 rewrites. Test redaction and running/completed state transitions while retaining the shipped static overlay. |
| [U06](specs/U06-large-graph-rendering.md#outcome) | 29 | Deliver cached server layouts and versioned binary transfer, then pass the named GPU frame-rate and large-graph limits. Verify access-filtered grouping, cancellation and bounded transfer on the supported graph sizes. |
| [U07](specs/U07-data-quality-and-schema-diagram.md#outcome) | 31, 120, 121 | Build per-commit quality records, backfill, introducing-commit search and the schema diagram. Move overlays to the retained record and verify focus-scoped fix previews against the complete candidate validation result. |
| [U08](specs/U08-visual-query-builder.md#outcome) | 101 | Add Phase 3's union, graph, values, minus and not-exists editing plus overview entry. Preserve text outside edited spans, shared undo, stable layout and round trips for locked syntax. |
| [U11](specs/U11-linked-views.md#outcome) | 140, 141, 142 | Finish the named colouring, date-facet, mobile, keyboard and branch gaps, and settle persisted/filtered projection choices. Pass the GPU frame-time and lasso gates with linked selection and caller visibility preserved. |

### Scope decisions for partially complete capabilities

These decisions fix what completes four capabilities in the [capability matrix](FEATURES.md#capability-status).

* Formatting, linting and command-line tools complete with item 48 alone. Items 55 and 73 are
  follow-ups outside that capability.
* Paths and graph algorithms complete with items 8, 275, 276 and 277. OpenCypher
  procedures belong to item 20 under additional query languages, and the C23 writer belongs
  to item 36 under batched updates and triggers. The weighted-betweenness extensions in
  items 79 and 80 are optional and outside the completion path.
* Server and Fuseki integration completes with items 76, 38, 51, 37 and 39. Item 37
  excludes subscriptions, and item 51 excludes `shortest(…)` evaluation, which ARQ does not
  evaluate either. Item 69 adds the ARQ extensions that it lists as a lower-priority
  extension outside the completion path.
* GeoSPARQL completes with item 24. Item 70's Jena-compatible mode is not adopted, and
  exact arithmetic on curves, GML solids, 3D constructions and splitting lines at crossings
  are not part of the capability.

## Tier 1: now

The first wave establishes evidence, durable execution, time profiles and recovery.
Item 340 waits for the C25 records and C26 publication contract it uses. Ask completion
continues within its caller-visible evidence and measured-latency scope.

| # | Item | Spec | Size | Value |
|---|---|---|---|---|
| 81 | **Shared evidence and review records.** Versioned resources, bounded preparation, current memory compatibility checks and resident source lifetime with safe catalog disposal are available. Phase 2 still requires authenticated source and claim writes, protected metadata, durable source holds, retained references, review-controlled projections, reversible identity decisions and safe catalog mutation from close callbacks. A close callback that calls a catalog management operation now waits forever on the management lock, and it should receive a refusal until such mutation is supported. Shared-record integration with memory, ingestion, reports and maintained analysis remains planned for Phase 3. | [C25](specs/C25-evidence-and-assessments.md), [C17](specs/C17-agent-memory.md#shared-memory-representation) | M–L | High |
| 396 | **Shared entity managed integration.** Bounded private offline replay and selected fixture mappings are available. Integrate with actual C25 managed review and protected records, C27 authenticated availability, C26 durable publication and F22 freshness while preserving exact causal inputs, conflicts, scopes and historical views. Validate through the store and the shipped shapes, and charge each input byte once, replacing the private validator and its conservative accounting. | [A01](specs/A01-shared-entity-representation/README.md) | M–L | High |
| 395 | **Bounded preparation of read views.** Capped historical snapshots preserve exact retained origins, and bounded Rust permission masks support graph, predicate and class rules with the built-in disclosure guard. Complete protection-pattern planning and retained nested-provider accounting. Validate costs under item 398 before adopting the shared preparation budget in guarded Dataset queries, exact evidence references and U02 readers. On adoption, replace the controlled term decoder with the existing key decoder for graph and predicate names, and reconsider the bounded cache-key formatter, private containers and linear mask-cache scan, which currently have no production callers. | [C29](specs/C29-bounded-view-preparation/README.md) | M | High |
| 398 | **Controlled preparation costs.** Measure and reduce allocation and latency in cooperative vocabulary lookup, mask preparation and cold disclosure indexes while preserving one cap, cancellation, deadlines and exact visibility. Delta vocabulary lookup scans every retained entry while it holds the vocabulary read lock, which delays writers. Use an indexed lookup or release the lock between chunks. Retained-state admission charges the input snapshot's existing delta and its shared copy at 2 KiB per quad, even when a call returns the input unchanged, so ordinary caps refuse stores with large uncompacted deltas. Charge only newly retained storage. Compare cold and cached release builds before broader guarded caller adoption. | [C29](specs/C29-bounded-view-preparation/SPEC.md#outcome) | S–M | High |
| 400 | **Retained bytes of interrupted deletes.** A crash after a delete saves the registry and moves the root aside leaves `databases/.kept-<name>-<token>` behind. Startup neither removes nor reports it. A recorded delete intent or a startup warning for unregistered kept directories would let the next start finish the cleanup. | [C25](specs/C25-evidence-and-assessments.md) | S | Medium |
| 339 | **Durable jobs and delivery.** Planned checkpointed jobs, fenced external workers and a transactional outbox make collection, analysis and delivery resumable after a restart. Publication and delivery intent share the dataset transaction. | [C26](specs/C26-durable-jobs-and-delivery.md) | L | High |
| 341 | **Valid time and observation history.** Versioned RDF resources, SHACL shapes and typed temporal templates are available. Phase 1 remains incomplete at A4 until C25 Phase 2 and protected writes establish server-owned availability and refuse backdating. Later phases add the projection helper, event-time UI and expiry integration. | [C27](specs/C27-valid-time-profiles.md) | M–L | High |
| 340 | **Maintained analyses and evidence dependencies.** Planned immutable run manifests, conservative read dependencies and guarded publication track when assessments need review or recomputation. Begin with bounded full recomputation and add proven incremental maintenance through item 17. | [F22](specs/F22-analysis-lifecycle.md) | L | High |
| 58 | **Packages and async coverage.** Python async queries, bounded cursors and transactions are available. Publishing to PyPI, Maven Central and npm, Windows and Intel macOS builds, a musl JVM build, PyPy and free-threaded wheels, and further Python async administration and bulk coverage remain. The workflows exist with their publish jobs switched off. | [X12](specs/X12-release-engineering.md), [P01](specs/P01-python-bindings.md#async-datasets-and-cursors), [P04](specs/P04-jvm-bindings/OUTCOME.md#roadmap-follow-ups-2026-10-12), [P05](specs/P05-node-bindings.md#roadmap-follow-ups-2026-10-12) | M–L | High |
| 59 | **Server binaries and published container images.** An unpublished GNU x86_64 archive pipeline passes packaging and installed persistence checks on pinned Debian 11 and Rocky Linux 8 runtimes. Other platforms, provenance, releases and container publication remain. The HTTP client should start without a system CA store, and the candidate workflow should run on demand, on a schedule and for packaging changes, with tests that import its preflight script. | [X12](specs/X12-release-engineering.md) | M | High |
| 60 | **API stability policy.** The policy and HTTP and standard CLI development-baseline checks are available. Library and binding comparisons, CLI JSON and exit-code coverage, MCP and metrics guards, deprecation signals and release baseline integration remain under X12 Phase 4. | [X12](specs/X12-release-engineering.md) | S–M | High |
| 61 | **Backup completion.** Whole-server and multi-dataset runs, remote CLI, repository copy and gated content-defined chunking remain. Add optional retained generations and pins, storage-class and Object Lock/WORM controls, and an explicit repository-zstd decision, with restoration and retention acceptance. | [F05](specs/F05-snapshot-repositories/OUTCOME.md#roadmap-follow-ups-2026-10-12), [X01](specs/X01-compression-codecs.md#remaining-codec-decisions) | L | High |
| 263 | **Ask on DBpedia.** Question-aware grounding has mixed measured accuracy, while bounded versioned candidate evidence preserves full choice identities and supports conservative display ordering. General contextual resolution and further accuracy work remain, with genuine ambiguity kept as a clarification. Changes need offline and end-to-end checks. | [C18](specs/C18-natural-language-questions-and-ingest/OUTCOME.md#ask-grounding-accuracy-item-263) | M | High |
| 318 | **Ask median latency.** The retained 107-question repeat has higher median wall time for Haiku and Sonnet, while p95 is nearly unchanged. Bounded debug tracing separates grounding, tool calls and nested embedding work. Collect representative stage profiles and reduce avoidable work while preserving caller visibility and required clarification. The retained paid run does not establish the cause. | [C18](specs/C18-natural-language-questions-and-ingest/OUTCOME.md#ask-grounding-accuracy-item-263), [Benchmarks](benchmarks/natural-language/RESULTS.md#ask-on-dbpedia) | M | High |

## Tier 2: high-impact features and the scale foundation

| # | Item | Spec | Size | Value |
|---|---|---|---|---|
| 17 | **Incremental computation core.** This umbrella item is split into one item per lane, items 413 to 429, so that each lane has its own size and dependencies. Item 17 is complete when all of its lanes are. The [lane table](#lanes-of-item-17) lists them. GraphQL subscriptions are deferred under item 37 until a workload asks for them. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | XL | Very high |
| 18 | **Explanations.** Provenance-aware protections with `reviewedOnly` over derived facts. Demand derivation trees, bounded why-not searches, entailment diffs and minimal justifications are available. Phase 3 needs the provenance algebra and reasoner witnesses of items 415 and 416. | [F18](specs/F18-explanations-and-entailment-diffs.md) | M–L | High |
| 23 | **Full-text parity.** Online enabling and reconfiguring, search after uncommitted writes, and historical search remain. The analyzers that follow Lucene's and the dictionary analyzers are built. | [F03](specs/F03-full-text-search/OUTCOME.md#roadmap-follow-ups-2026-10-12) | M each | Medium |
| 24 | **GeoSPARQL completion.** Add per-row nearest-neighbour joins that run at least 50 times faster than the generic plan, QLever's `spatialSearch:` syntax, metric-property rewrites and polygon approximations in a versioned index format. Add exact Helmert datum shifts and geographic CRSs on datums other than WGS 84, opt-in grid-based shifts from operator-supplied grid files that still refuse when a needed grid is missing, and curved GML segments read by densifying arcs into line segments within a stated tolerance. Exact arithmetic on curves, GML solids, 3D constructions and splitting lines at crossings are not adopted. | [G01](specs/G01-geosparql/OUTCOME.md#roadmap-follow-ups-2026-10-12) | L | Medium |
| 21 | **Editing forms generated from SHACL shapes.** Complete atomic nested changes and browser editing from the Rust library's pooled read models, plus list and `sh:or`/`sh:xone` editors. Complete blank-named graph contexts and admission for model sources, complex nested paths, remaining target and query producers and controlled reader integration through item 395. | [U02](specs/U02-shape-driven-forms/SPEC.md#outcome) | M–L | Medium–high |
| 100 | **Reports for distinct audiences.** Planned named GraphRAG configurations have separate principals, projections, guarded reports and review histories, with an explicit default and shared dataset spending limits. | [C22b](specs/C22b-report-configurations.md) | M | High |
| 65 | **Encryption.** Finish key workflows, Vault/cloud KMS and recovery export, then Phase 2 write-only age-recipient repositories and age identities. Phase 3 live-dataset and Phase 4 derived-index encryption follow; item 61 owns the shared chunking gate. A delayed initializer with the wrong key should remove its own leftover intent, and the marker-attachment branches should share one helper. | [F11](specs/F11-encryption-at-rest.md#roadmap-follow-ups-2026-10-12) | L–XL | High |
| 7 | **Batch-oriented execution and disk spill.** The full-scale spill matrix passes, while execution follow-ups and the default decision remain under item 93. Spill stays opt-in, and several cases still run eagerly. The [remaining work](#remaining-work-for-item-7) is listed below. | [X08](specs/X08-disk-spill-and-server-memory.md) Phase 2, [X05](specs/X05-streaming-query-execution.md#roadmap-follow-ups-2026-10-12) | L | Very high |
| 8 | **Graph-algorithm library.** Algorithms and graph lenses are available. The full Graphalytics harness, algorithm-name completion, richer background progress, remaining algorithm surfaces, and the remaining algorithm surfaces remain. The C23 writer is item 36 and openCypher procedures are item 20. JVM statistics inside a transaction use a binding copy of the library's statistics code, while Node.js and Python statistics read only the head, so a library method that takes the snapshot should give the bindings one implementation. | [F14](specs/F14-graph-algorithms.md#outcome) | S–M | High |
| 9 | **Materialization at scale.** Phase 1 materialization is available. The separate derived-fact layer of Phase 2, `owl:sameAs` representatives of Phase 3 and eligible execution without iteration barriers and equality maintenance in Phase 4 remain. Representatives would also let derivation on demand read equality without the `eq-rep-*` rules. Today a demand on a small view where `owl:sameAs` relates classes and properties can take much longer than materializing the whole view. | [F16](specs/F16-materialization-at-scale.md), [C08](specs/C08-inference-freshness.md#roadmap-follow-ups-2026-10-12) | L | High |
| 12 | **GraphRAG community summaries.** Community reports, global answering and change-feed refresh are available. Counting changes with an accumulator on an internal F13 circuit remains planned and waits for items 413 and 414. | [C22](specs/C22-graph-rag-community-summaries.md) | M | High |
| 14 | **Compaction and operation progress.** Delta changes located per block, new vocabulary without a full rebuild, adaptive thresholds and automatic compaction in embedded use remain. Shared controlled load and compaction progress must also reach the Node callbacks without weakening cancellation or memory budgets. | [C13](specs/C13-automatic-compaction.md#roadmap-follow-ups-2026-10-12), [P05](specs/P05-node-bindings.md#operation-progress) | M | Medium–high |
| 15 | **Ingestion of images and audio.** The remaining follow-ups are recorded in C18. | [C18](specs/C18-natural-language-questions-and-ingest/OUTCOME.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 86 | **Group commit for indexed datasets.** The p90 of spatial serial inserts at 10.5M misses its gate. The spatial reader gate at 1.05M is not established, because the ordinary writer missed the schedule lag requirement. Items 111, 112 and 171 follow up. | [X09 section 17](specs/X09-group-commit-by-default.md#17-phase-4-group-commit-for-indexed-datasets) | M–L | Medium–high |
| 113 | **Small changes and undos over a kept closure.** Changes of 100 triples at 16 threads and undos at 1 thread remain slower than before the F16 follow-up, and a profile of the full run should find or rule out a cost of about 5%. The options are an opt-in in-memory index of the input facts, charged to the memory budget, and per-thread counters in the block cache. | [F16](specs/F16-materialization-at-scale.md#roadmap-follow-ups-2026-10-18) | M | Low |

### Lanes of item 17

Stage A makes the incremental core safe and supplies what items 12, 18 and 19 need.
Items 12, 18 Phase 3 and 19 Phase 3 do not need all of Phase 2 and Phase 3. Item 12 needs
items 413 and 414, item 18 Phase 3 needs items 415 and 416, and item 19 Phase 3 needs
items 415 and 417. Stage B delivers the rest of Phase 2. Stage C delivers view
provenance. The F13 spec records the design decisions that the lanes follow. Items that
affect the write path measure with the `--views none|async:N|commit:Q|graph:N` option of
`scripts/nsc-writes.sh` that item 414 adds.

| # | Item | Spec | Size | Depends on |
|---|---|---|---|---|
| 413 | **Safety net and load checks.** Apply `ViewDef::check()` when a view loads or attaches, so a format 1 `view.json` that carries graph, commit, branch or `keepCommits` members is refused and no longer runs silently as an async table view. Add the `COMPILER` version bump test and reduce the complexity of `runtime.rs`. | [F13](specs/F13-incremental-views-and-provenance.md#84-persistence) | S–M | None |
| 414 | **Internal circuits.** Let the server attach circuits that no user defined, with an accumulator sink and a server-only `InternalAccess`. The accumulator keeps each changed fact's identity to the sink, which sums the absolute weight per key and fact and then per key, so an addition followed by a removal counts 2, a replacement within one commit counts 2 and a deletion counts 1, as the existing feed-based counter does. Item 12 counts per-community changes with one. Tests cover churn across commits, a same-commit replacement and deletion-only workloads against that counter. Item 226 goes with this lane, and the lane adds the `--views none\|async:N\|commit:Q\|graph:N` option to `scripts/nsc-writes.sh` so that write-path gates can measure views. | [F13](specs/F13-incremental-views-and-provenance.md#36-internal-circuits), [C22](specs/C22-graph-rag-community-summaries.md#5-refresh) | S–M | 413 |
| 415 | **Provenance algebra.** Add the `count`, `lineage` and `why` annotations with a 128-bit `FactKey` over vocabulary keys and bounded witness sets. | [F13](specs/F13-incremental-views-and-provenance.md#5-provenance) | S–M | 413 |
| 416 | **Reasoner witnesses.** Record witnesses in the reasoner with F13's algebra, off unless a protection needs supported visibility, and define how witness sets converge under truncation. | [F18](specs/F18-explanations-and-entailment-diffs.md#71-protections) | M | 415 |
| 417 | **Fixed-point operator.** Host one fixed-point interface in F13 with a reasoner plug-in, maintain property paths with `+` and `*` through backward and forward rederivation, and add `VIEW_STATE` format 2 for its state. | [F13](specs/F13-incremental-views-and-provenance.md#43-added-in-phase-3), [F17](specs/F17-query-time-reasoning.md#63-kept-current-phase-3) | M | 415 |
| 418 | **Aggregates and correlated EXISTS.** Maintain `MIN`, `MAX`, `GROUP_CONCAT`, `SAMPLE`, `HAVING` and correlated `EXISTS` instead of recomputing them. | [F13](specs/F13-incremental-views-and-provenance.md#42-added-in-phase-2) | M | 413 |
| 419 | **Ordered windows.** Keep `ORDER BY` with `LIMIT` and `OFFSET` as a window with a reserve, and report row positions in live diffs. | [F13](specs/F13-incremental-views-and-provenance.md#44-ordered-windows) | S–M | 413 |
| 420 | **Asynchronous graph views.** Add `graph` views with `view.json` format 3, the `View` commit kind, reserved graphs and skolem IRIs over distinct solutions. On a dataset that restricts reads, a graph view always needs `declassify: true` with an audit record naming the view, its query, the output graph and the declassifying principal. A view without it marks its output graph when it first writes it. The mark stays with the graph and its history after the view is deleted, with or without `dropGraph`, and under a read-restricting policy only administrators read a marked graph until one declassifies the graph with an audit record. Installing such a policy first suspends every undeclassified graph view, including one that has not written its graph, and applies the restriction before the policy is visible. The mark lives in a versioned dataset file that older releases refuse, and this item defines the format. Tests read a populated output graph across the policy transition, cover a C12b protection that hides an input triple, cover a view with no output yet, and delete the view with and without `dropGraph` before checking that current and historical reads are refused to non-administrators. This is the graph-view counterpart of the `view:{name}` grant of item 222. | [F13](specs/F13-incremental-views-and-provenance.md#22-reading-a-view) | M | 413 |
| 421 | **Commit-time table views.** Maintain table views inside the commit with a defined position relative to the WAL, a fall back from group commit, and suspension on failure by default. Write-path competitive work goes first. | [F13](specs/F13-incremental-views-and-provenance.md#33-maintenance-passes) | L | 413 and the write-path work of items 86, 111, 112 and 171 |
| 422 | **Commit-time graph views.** Write a graph view's triples in the commit that caused them. | [F13](specs/F13-incremental-views-and-provenance.md#33-maintenance-passes) | M | 420, 421 |
| 423 | **WebSocket transport.** Serve live queries over WebSocket with axum's `ws` feature and include item 224's server-wide limit of live queries. | [F13](specs/F13-incremental-views-and-provenance.md#12-dependencies) | M | 413 |
| 424 | **MCP views.** Add `watch_query`, `list_views` and `read_view` to the MCP server. | [F13](specs/F13-incremental-views-and-provenance.md#24-cli-library-bindings-and-mcp) | S–M | 222 |
| 425 | **Node and JVM view APIs.** Expose views and live queries in the Node and JVM bindings. | [F13](specs/F13-incremental-views-and-provenance.md#24-cli-library-bindings-and-mcp) | M | 413 |
| 426 | **Views in the UI.** Add view management and live results to the web UI. | [F13](specs/F13-incremental-views-and-provenance.md#24-cli-library-bindings-and-mcp) | M | 222 |
| 427 | **Per-branch views and `keepCommits`.** Keep state per branch with defined fork semantics, and keep recent diffs in memory only for reads at past commits. | [F13](specs/F13-incremental-views-and-provenance.md#7-branches-and-history) | M | 223 |
| 428 | **Data-dependent protections as views.** Compile protections that depend on data into views, so a live reader no longer recomputes its whole query. | [F13](specs/F13-incremental-views-and-provenance.md#62-live-queries) | L | 417 |
| 429 | **View provenance.** Add `view:witness` and `view:why`, and provenance on one-off queries, together with item 221's views over inferred data. | [F13](specs/F13-incremental-views-and-provenance.md#53-uses) | M | 415, 417 |

### Remaining work for item 7

* Spill stays opt-in while item 93 closes execution costs and the default decision,
  preserving the passed full-scale plain/encrypted acceptance matrix.
* LIMIT propagation into fallbacks, running state for FOLD and registered aggregates,
  and earlier first bytes remain from the X05 follow-ups.
* An EXISTS evaluated once per outer key, in BIND or an OPTIONAL condition, stays eager
  because the cursor was slower, and DESCRIBE does not stream.
* The UI's result table still asks for eager execution until it can show a streamed
  result's unknown total.
* The parallel cursor filters and range top-k were measured on 8 vCPUs and still need a
  12-thread measurement on an Intel host. Decoding ahead under many concurrent queries is
  unmeasured.
* The eager `regex-iri` filter keeps `regex::Regex`, and its 5% inlining sensitivity on
  forge is open.

## Tier 3: big bets

These remaining reasoning and integration phases follow the foundation and shared
capability work above. Each phase keeps its declared prerequisites and acceptance.

| # | Item | Spec | Size | Value |
|---|---|---|---|---|
| 19 | **Query-time reasoning.** Phase 2 adds `auto` with its cost model, the dataset setting, hybrid Jena rules, OWL 2 QL rewriting, reuse of tables across commits, the MCP argument, the UI and the G08 mapping. Phase 3 keeps tables current through F13 after items 415 and 417, and adds `why` provenance on tables for F18. Items 190, 192 and 193 cover what Phase 1 left. | [F17](specs/F17-query-time-reasoning.md#outcome) | L | High |
| 25 | **Query extension completion.** Dynamic dispatch, foreign callbacks, batching, additional aggregate arities and measurements remain, including native Java dispatch beside ARQ. | [P03](specs/P03-query-extensions.md#roadmap-follow-ups-2026-10-12) | M | Medium |

## Tier 4: worth doing, not urgent

This tier has three groups. The features are user-facing work, the performance follow-ups
investigate measured costs and planner losses, and the graph-interaction UI finishes a set
of canvas features.

### Features

| # | Item | Spec | Size |
|---|---|---|---|
| 6 | **Ask evaluation on public benchmarks.** Runs of local and OpenAI-compatible models and an evaluation with independent roles remain, and LC-QuAD 2.0 waits for a Wikidata load from the full dump. | [C18](specs/C18-natural-language-questions-and-ingest/OUTCOME.md#roadmap-follow-ups-2026-10-12) | S–M |
| 27 | **Live query profiling and verified rewrites.** The static plan overlay is available. The running-query registry, live operator progress and hotspot views remain planned, followed by C18 Phase 6's result-equivalent rewrite suggestions and their integration in the live plan. | [U04](specs/U04-live-query-profiling.md), [C18](specs/C18-natural-language-questions-and-ingest/README.md) Phase 6 | M |
| 28 | **Query by example** and Bloom-style search phrases. | [U03](specs/U03-query-by-example.md) | M |
| 20 | **openCypher,** with graph algorithms as procedures and LDBC SNB as acceptance. This item belongs to the additional query languages capability and is outside the completion path of paths and graph algorithms. | [F01](specs/F01-cypher.md#roadmap-follow-ups-2026-10-12) | L–XL |
| 26 | **Local models.** GPU, ONNX and GGUF, rerankers, and models shared between processes. | [F12](specs/F12-local-embeddings.md#roadmap-follow-ups-2026-10-12) | M–L |
| 344 | **ChatGPT-plan model access.** Planned Sign in with ChatGPT connections let a caller use eligible plan access through the Responses API. Account-bound credentials, refresh, usage-limit handling and explicit provider selection preserve existing data and spending controls. | [C28](specs/C28-chatgpt-model-access.md) | M–L |
| 29 | **Rendering very large graphs.** Phase 3 adds server layouts, the binary transfer format and GPU frame-rate tests. | [U06](specs/U06-large-graph-rendering.md#outcome) | M |
| 30 | **Notebooks** stored in the dataset. | [U05](specs/U05-notebooks.md) | M |
| 31 | **Data-quality timeline and schema diagram.** Validation overlays are available. Per-commit quality history, backfill and the schema diagram remain planned. | [U07](specs/U07-data-quality-and-schema-diagram.md) | M |
| 32 | **Parameterized reusable plans.** | [X10](specs/X10-reusable-query-plans.md), [P01](specs/P01-python-bindings.md#roadmap-follow-ups-2026-10-12) | L |
| 34 | **Vector quantization** and vector-result caching. | [F04](specs/F04-vector-search.md#roadmap-follow-ups-2026-10-12) | M |
| 35 | **Multi-field full-text entity documents.** | [F03](specs/F03-full-text-search/OUTCOME.md#roadmap-follow-ups-2026-10-12) | M |
| 36 | **Batched updates and commit triggers,** including the C23 writer that graph algorithms use. This item belongs to the batched updates and triggers capability. | [C23](specs/C23-batched-updates-and-triggers.md) | M |
| 37 | **GraphQL completion.** Child queries seeded with engine IDs, per-parent top-k, nested connections, the UI/schema editor and persisted queries with their MCP controls remain. Subscriptions are deferred until a workload asks for them, and then use F13 as a live query. Mutations retain their separate scope decision. | [C03](specs/C03-graphql/OUTCOME.md#roadmap-follow-ups-2026-10-12) | M |
| 38 | **SERVICE enhancer.** A slice-aware cache, cache functions, a disk cache and parallel requests. | [G10](specs/G10-service-enhancer.md#roadmap-follow-ups-2026-10-12) | M |
| 39 | **Stored updates,** multi-value parameters and private stored queries. | [C16](specs/C16-stored-queries.md#roadmap-follow-ups-2026-10-12) | S–M |
| 41 | **Embedding conveniences.** Text input in Similar, chunk offsets kept beside vectors, and NixOS embedding settings. | [F08](specs/F08-embeddings-on-write.md#roadmap-follow-ups-2026-10-12) | S |
| 42 | **MCP observability.** Progress notifications, per-tool metrics and performance measurements. | [C11](specs/C11-mcp-server.md#roadmap-follow-ups-2026-10-12) | S–M |
| 43 | **Virtual graphs** over SQL, Parquet and Iceberg. | [F19](specs/F19-mappings-and-virtual-graphs/README.md) Phase 2 | XL |
| 44 | **A local-first engine** in WebAssembly that syncs through RDF Patch and branches. | [F20](specs/F20-local-first-engine.md) | XL |
| 72 | **JSON query languages,** and a Datalog surface syntax for rules and queries. | [C24](specs/C24-json-query-languages.md), [F17](specs/F17-query-time-reasoning.md) | M–L |
| 78 | **Incremental write-time validation** of recursive SHACL shapes and of SHACL-SPARQL constraints not anchored at the focus node. | [C10](specs/C10-write-time-validation.md#roadmap-follow-ups-2026-10-12) | M–L |
| 82 | **Cheaper vector catch-up and measured half-precision recall.** Catch-up should scan only new rows, binary16 recall needs measuring on Euclidean and offset dot-product data, and a measurement should pick the worker pool. A test should cancel a carry. | [F04](specs/F04-vector-search.md#roadmap-follow-ups-2026-10-12) | S–M |
| 250 | **Chinese segmentation after SmartChineseAnalyzer,** and Kuromoji's normal and extended modes and user dictionaries for `ja-dict` and `ko-dict`. `zh-dict` segments by jieba's frequencies today. `ja-dict` and `ko-dict` should also cut a run of unknown characters inside a surrogate pair where Lucene does, which they do before the pair. | [F03](specs/F03-full-text-search/OUTCOME.md#roadmap-follow-ups-2026-10-12) | M |
| 251 | **Analyzers for the other languages of jena-text,** such as Greek, Arabic, Persian and Hindi, that reproduce Lucene's tokens as the analyzers for nine languages do. | [F03](specs/F03-full-text-search/OUTCOME.md#roadmap-follow-ups-2026-10-12) | M |
| 222 | **View access by grant containment.** A caller reads a view only with full access or the principal's exact access. Containment of grant sets, a cache per caller and view, and a `view:{name}` grant that declassifies a view with an audit record are not built. Item 222 comes before items 424 and 426. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | M |
| 342 | **Metrics parity for JSON and OTLP.** Add the Phase 2 produced-row, query-memory-peak and rebuild counters and histograms to the JSON snapshot and OTLP export. Preserve Prometheus units, labels, reset rules and dataset visibility, with cross-format tests. | [C01](specs/C01-observability-and-budgets.md#metrics-parity) | S |
| 343 | **Dataset-local codec and pattern settings.** Add layered settings for adopted vocabulary codecs and predicate patterns, used by subsequent builds and compactions. Preserve existing generations and format refusals, and record the optional portable zstd decoder decision without reopening measured rejected codecs. | [X11](specs/X11-index-compression-and-predicate-patterns.md#dataset-local-settings), [X01](specs/X01-compression-codecs.md#remaining-codec-decisions) | M |
| 407 | **Software inventory corpus review and matcher.** The `testsuite/software-inventory/v1` fixtures await independent review before a matcher consumes them, and a test checks their manifest digests. Review the adjudicated outcomes, then build the bounded matcher or remove the unused fixtures. | [A01](specs/A01-shared-entity-representation/README.md) | M |

### Performance follow-ups

The measurements behind these items are in [BENCHMARKS.md](BENCHMARKS.md) and the linked
spec sections.

| # | Item | Spec | Size |
|---|---|---|---|
| 1 | **Benchmark refresh.** Later X06 batches, BSBM at 100M, a rerun of the BSBM BI driver cells and the regeneration of the published tables remain. | [X06](specs/X06-benchmark-coverage.md), [F16](specs/F16-materialization-at-scale.md), [X08](specs/X08-disk-spill-and-server-memory.md#33-the-default-and-small-profiles) §3.3 | M |
| 4 | **Cold sparse lookups.** Phase 4 adds an optional prewarm of the pages a server used before it stopped. Cold `star-join` at 10.5M is slower with index format 3 and that cost should be removed. The Oxigraph targets need measuring again, and the 1.01B dense-read gate and its window sweep are not measured. | [X07](specs/X07-cold-sparse-reads.md) | M |
| 33 | **Per-call overhead in the bindings.** JNI calls cost 3 to 4 times Jena's in-memory store, and Node `has()` and export conversion are slower than published targets. Python value lookups trail by 0.67 µs, and some small queries by 11 to 20 µs. The scorecard in scripts/bench-bindings/scorecard.py can compare cases once the harness records the API layer, term consumption and oracle evidence and the scorecard accepts ordered and pooled fingerprints. The Python async cursor converts each row of a batch through a separate native call and reads the plan and statistics again for every batch, which is unmeasured. | [P01](specs/P01-python-bindings.md#current-source-per-call-assessment), [P04](specs/P04-jvm-bindings/OUTCOME.md#roadmap-follow-ups-2026-10-12), [P05](specs/P05-node-bindings.md#roadmap-follow-ups-2026-10-12) | M |
| 40 | **Memory footprint.** Idle server RSS, Python process RSS and full-text index size. | [X08](specs/X08-disk-spill-and-server-memory.md) Phase 3 | M |
| 84 | **Large query plans.** An n-ary join would let the 4000-level plan limit rise, and planning time still grows with the square of the number of triple patterns. A star of a variable that many patterns share remains slow at the limit. | [X05](specs/X05-streaming-query-execution.md#large-basic-graph-patterns) | M |
| 88 | **Allocator overhead of the server-wide caches.** The result cache's heap is about 1.2 times the bytes it counts, and evicted entries leave free pages on the tokio and Rayon threads. The item also covers about 3% more instructions on queries of a few microseconds. | [X08](specs/X08-disk-spill-and-server-memory.md#gates) | S–M |
| 90 | **VALUES pushdown in more shapes.** The rewrite misses a VALUES inside `GRAPH`, joins reached while flattening a group, and branches whose FILTER reads the table's variable. | [C18](specs/C18-natural-language-questions-and-ingest/OUTCOME.md#roadmap-follow-ups-2026-10-12) | S–M |
| 91 | **Large SP²Bench answers.** The `q4` answer is 1.96 GB of TSV and needs a `--max-result-mb` above the default. Profile large-result costs while preserving response budgets and bounded memory. The `q12b` comparison is inside the ten-percent tie band. | [X06](specs/X06-benchmark-coverage.md#matched-query-comparisons), [follow-up](specs/X06-benchmark-coverage.md#sp2bench-q4-and-q6-follow-up-2026-10-23) | M |
| 93 | **Spill execution costs and the default.** The full-scale acceptance matrix passes. Grouping cost above ordered spilled joins, the extra writes that restore probe order, a DISTINCT over ordered input that keeps its whole set, the [performance investigation](#spill-performance-investigation) and a decision on enabling spill by default remain. | [X08](specs/X08-disk-spill-and-server-memory.md#full-scale-spill-acceptance-item-93-2026-10-21) | M |
| 111 | **Spatial serial slowdowns of group commit at 10.5M.** Single geometry inserts and IRI inserts run a few percent slower at p90 with grouping. Find the cause, then fix it or state the tradeoff. | [X09 Outcome](specs/X09-group-commit-by-default.md#phase-4) | S–M |
| 112 | **Fence cost of group commit on indexed datasets.** A fence costs about half as much again as an ordinary commit and carries only about two commits with four writers. Find why and whether it can carry more, and measure it on a drive in its fast state too. | [X09 Outcome](specs/X09-group-commit-by-default.md#phase-4) | M |
| 160 | **FSST single-term lookups.** Looking up one term at a time with FSST costs more than decoding each earlier key in full, which slows `star-join` and cold long-literal lookups. A lookup that plans every key's need from the last key back measured 7% slower on single lookups, because the lookups wait on memory more than on decoding. Profile the cold lookups and find a path that keeps the partial decode for long literals without the cost on short keys. | [X11](specs/X11-index-compression-and-predicate-patterns.md#item-108-faster-fsst-reads) | S |
| 170 | **Reader tail of the parallel full-text checkpoint.** Since a checkpoint synchronizes its files from eight threads, the mixed full-text reader p99 at 1.05M measured about 4% higher, within the spread of the runs. Measure it with more rounds, and if the checkpoint's share holds, try fewer threads or a lower I/O priority for the synchronizing threads. | [X09 Outcome](specs/X09-group-commit-by-default.md#phase-4) | S |
| 171 | **Calibrate the spatial mixed cell for the drive's slow state.** The ordinary writers of the spatial mixed cell at 1.05M fell behind their schedule because the unpaced calibration averaged the drive's fast and slow states. Calibrate the rate against the ordinary writer in the slow state, or probe the flush time before each process, and rerun the cell to settle item 86's reader gate. | [X09 Outcome](specs/X09-group-commit-by-default.md#phase-4) | S |
| 276 | **Unweighted path searches after item 150.** Forge measured the unweighted `ps-pair`, `ps-pair-all`, `ps-pair-k5` and `ps-values10` 3 to 4% slower in every round and weighted `path:allShortest` 2.3% slower, although item 150 does not touch their searches. Compare their code generation in both builds and profile them to find what changed. | [Benchmarks](benchmarks/graph/RESULTS.md#weighted-pair-searches) | S |
| 223 | **Parallel view passes.** Each dataset runs its view passes on one worker thread, which falls behind a fast writer with 100 views. `--view-threads`, a shared worker pool and joins whose large deltas split across threads are not built. Item 223 comes before item 427. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | M |
| 430 | **Streaming view reads.** `ViewRows` and the snapshot events of live queries decode the whole result before they return it. Stream the rows in bounded chunks, so that reading a large view or opening a large live query does not hold the whole result in memory. | [F13](specs/F13-incremental-views-and-provenance.md#22-reading-a-view), [X05](specs/X05-streaming-query-execution.md#outcome) | M |
| 225 | **The F13 benchmarks and tests.** The SNB update streams with live short reads and views, BSBM's mix with views, the fan-out test of 1,000 subscribers, a grammar-driven query generator for the differential tests and injected crashes for recovery are not built. A grouped COUNT's pass is only 15 to 35 times cheaper than the query on synthetic data, short of the 100 times target. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | M |
| 226 | **The cost of the change ring on commits.** While a view or live query is subscribed, each commit copies its changes into a ring, which adds about 4% to a two-quad commit on atlas. It needs a pinned forge measurement and a cheaper copy. Item 226 goes with item 414. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | S |
| 240 | **Buffered output of streamed answers.** The cursor writers of streamed SELECT and graph answers still write a few bytes at a time through the response writer and are compiled with the server. Give them the engine-side buffer that eager answers use, with the same count of the rows the client received. | [Benchmarking](BENCHMARKING.md#release-build-settings) | S |
| 241 | **Front coding on three warm DBpedia queries.** Since item 108, `pat-film-predicates` and `pat-film-predicate-list` take about 4% longer with front coding and `top-linked` 1.4% longer, in all 16 pinned rounds on forge. Count their instructions on both builds and remove the cost, or show that it comes from code placement. | [X11](specs/X11-index-compression-and-predicate-patterns.md#item-108-faster-fsst-reads) | S |
| 243 | **Cheaper row accounting in the result buffer.** The buffer checks the row count at every write so that an answer cut partway reports the rows the client received. Serializers write one character at a time, so the check costs 7% of the instructions of `optional-chain` and 14% of its client time on forge, 150.8 ms against 132.0 ms. A row log that the serializers fill themselves measured no cheaper. Find a way to record row ends that costs nothing per write. | [Benchmarks](benchmarks/engine/RESULTS.md#the-cost-of-result-serialization) | S |
| 265 | **The commit path without views.** Forge measurements of inferred-view rounds are published, but the persistent commit comparison varies between 0.27 and 1.61 ms even within one build after SSD trimming. Syscall tracing locates large variation inside WAL and vocabulary writes in both builds. Repeat the comparison with matched durable-write latency before assigning a cost to the inference change. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | S |
| 290 | **Durable write costs.** Preserve synchronization before acknowledgement and compare unsynced commits as a distinct durability class. Revisit serial storage waits and mixed-reader tails only when a material persistent cost or simple broad improvement justifies it, keeping completed read work in the comparison. | [X06](specs/X06-benchmark-coverage.md#outcome) | M |
| 291 | **Guard costs after secondary-key probes.** Recheck the accepted small LUBM and synthetic-query increases with more pinned Intel rounds, particularly LUBM `q10` and `q13` and the microsecond `path-plus` case. Separate instruction cost from code placement and remove any confirmed cost. | [X06](specs/X06-benchmark-coverage.md#secondary-index-probe-spans) | S |
| 292 | **Small costs after pure ASK demand.** Recheck the small accepted changes in SP²Bench `q8`, `q12a`, `q12c` and false and empty controls, together with the pinned small-query checks recorded in X06. Include repeated full join builds when tight resource limits require exact demand. Keep these low priority unless a material persistent cost or a simple broad fix emerges. | [X06](specs/X06-benchmark-coverage.md#pure-ask-comparison-and-small-costs) | S |
| 293 | **Cursor BIND reuse costs.** Improve general admission for sparse reuse while preserving cheap inputs and query budgets. The 50M slice shortcut and later reuse remain deferred. Recheck small eager and dense-string costs alongside the retained all-thread flags before making a broad fix. | [X06](specs/X06-benchmark-coverage.md#outcome) | M |
| 299 | **Short-query checks after keyed inner groups.** Verify the small Namespace guard increases for `class-predicates`, `exists-join` and `two-hop-count` on a quiet Intel host with matching compilers. Profile and remove any confirmed cost while retaining the keyed ASK and singleton-binding improvements. | [X06](specs/X06-benchmark-coverage.md#short-query-checks-after-keyed-inner-groups) | S |
| 306 | **Tiny queries after the combined feature changes.** In 16 pinned forge rounds, `count-all` server time rose from 5.3 to 6.0 µs and `distinct-obj` from 3.6 to 4.2 µs, while client time stayed within 1.2%. The later build runs about 11% fewer instructions across complete HTTP requests, and intermediate builds between `142cd28f` and `8798c2db` do not isolate one responsible change. Measure execution and its plan bookkeeping separately from serialization, distinguish instruction cost from code placement, and remove the cost. | [Benchmarks](benchmarks/engine/RESULTS.md#the-cost-of-result-serialization) | S |
| 328 | **Small query costs in integration screens.** Recheck all screen queries on a quiet Intel host, including `count-all`, `star-lookup`, `values-star`, `predicate-counts`, `types-grouped`, `order-by-full`, `predicate-subjects`, `star-join`, `subquery-agg`, `distinct-obj`, `path-plus` and `range-topk`. Cover standard capabilities, bounded history, store finalization, shared runtime ownership and citations, shared entity replay, schema-copy verification, bounded demand completion, resident source disposal, controlled preparation and fallible sort decoding. Pursue further tuning only for a persistent material cost or a simple broad improvement. | [X06](specs/X06-benchmark-coverage.md#small-query-costs-in-the-integration-screen), [integration control](specs/X06-benchmark-coverage.md#short-queries-after-evidence-integration), [standard profile](specs/X06-benchmark-coverage.md#standard-capability-integration-screen) | S |

### Spill performance investigation

This work belongs to item 93 and to [X08](specs/X08-disk-spill-and-server-memory.md).
Repeat the default-path `expr-order-key` and `distinct-join` comparisons on a quiet Intel
host with matched builds and inputs. Capture the executed plans and profile shared cursor
buffer accounting, join and DISTINCT work before assigning a cause, and bisect
reproducible slowdowns that stay below the push limit as well as clear failures.

Measure ordered hash joins with a GROUP BY parent at tight and larger spill budgets,
including consolidation writes, merge passes, charged memory, result equality and
cleanup. Compare memory budgets within one build separately from comparisons between
builds, and keep coverage for partially ordered DISTINCT.

### Graph-interaction UI

These items finish a set of graph-interaction features whose first phases have landed.
Rows that need backend work name it first.

| # | Item | Spec | Size |
|---|---|---|---|
| 101 | **Visual query builder, Phase 3.** `UNION`, `GRAPH`, inline `VALUES`, `MINUS` and `FILTER NOT EXISTS` join the editable subset, and **Build a query** opens the pane from a class of the Explore overview. | [U08](specs/U08-visual-query-builder.md#outcome) | L |
| 120 | **The validation overlay on the quality record.** Once item 31 Phase 1 records the guard's results per commit, add `focus=` to `GET /$/validation/{ds}/results` and read the record at the head instead of validating. | [U07](specs/U07-data-quality-and-schema-diagram.md#outcome) §11 | S |
| 121 | **Conclusive previews of a suggested fix.** Let a dry run name focus nodes to validate in full against the candidate state, so that the overlay can say a fix resolves a result while other results remain. | [U07](specs/U07-data-quality-and-schema-diagram.md#outcome) §11, [C15](specs/C15-write-previews.md) | S |
| 140 | **Linked views remainder in the UI.** Colour the embedding map by a kept Leiden run and open it with `Shift+M`, count date facets by month for short spans, give the facets a full-screen tab on phones, and let Explore's map and facets read a branch. | [U11](specs/U11-linked-views.md#outcome) | M |
| 141 | **Frame times of the linked views on a GPU.** Run U11's A9 frame-rate and lasso targets in a Playwright performance project on a GPU runner, since they were measured only in software rendering. | [U11](specs/U11-linked-views.md#outcome) | S |
| 142 | **Projections on disk and for partial callers.** Decide whether to write projections beside the index in a versioned file, and whether to serve a projection filtered to what a caller can read instead of answering `403`. | [U11](specs/U11-linked-views.md#10-open-questions) | M |
| 275 | **Weighted shortest paths under a length limit.** A weighted shortest search with `path:maxLength` lists paths by cost until one is short enough, so a pair whose short paths are all dear runs to the visit limit, and the path finder always sends a limit. Search over pairs of node and length, keeping a label only when no other label at the node is both cheaper and shorter, and use the same search for Yen's spurs. | [F07](specs/F07-path-search.md#weighted-searches-from-both-ends-2026-10-24), [U09](specs/U09-path-finder.md#outcome) | M |
| 277 | **Weighted `path:allShortest` from both ends.** Every cheapest path of one pair without `path:maxLength`, cycles through the source and searches to several targets still run Dijkstra's algorithm from the source alone. Keep every tied parent on both ends and join the halves at every cheapest meeting. | [F07](specs/F07-path-search.md#weighted-searches-from-both-ends-2026-10-24) | S–M |

| 285 | **Explain intensional query result cells.** Offer a Why? action on a result row when the query's triple pattern was intensional, carrying the row bindings into the existing explanation panel. | [F18](specs/F18-explanations-and-entailment-diffs.md#outcome) | S–M |
| 286 | **Why-not explanations for unbound query patterns.** Extend target patterns beyond a ground triple so `why_empty` can explain a failing intensional pattern with unbound variables, without inventing constant values. | [F18](specs/F18-explanations-and-entailment-diffs.md#outcome) | M |

## Tier 5: backlog

These items are smaller or narrower. Each has a design sketch in the linked spec.

| # | Item | Spec | Size |
|---|---|---|---|
| 45 | **Branches and cross-server cloning.** Remote branch relinking from the CLI and cross-server clone and archive transfer remain. Verify chosen-commit contents, blank identities, prefixes, configuration and lineage, with no destination published on a failed transfer. | [F09](specs/F09-branches-and-merges.md#roadmap-follow-ups-2026-10-12), [C06](specs/C06-clone-to-sandbox.md#completion-path) | M |
| 46 | **Notifications.** An email channel, an administration UI, broader backup-failure coverage, and delivery that a slow channel cannot delay. | [C21](specs/C21-outbound-notifications.md#roadmap-follow-ups-2026-10-12) | M |
| 47 | **Rust client.** Typed schema reports, statistics, stored queries, backup listings, snapshot and branch metadata and mutations, and bounded JSON diffs are available. Typed change feeds, clones, merges, search, reasoning, validation and settings remain, along with proactive rate-limit pacing and environment and saved-credential integration tests. | [P02](specs/P02-rust-client.md#roadmap-follow-ups-2026-10-12) | S–M |
| 48 | **Formatter and linter.** A lossless SHACLC parser and comment-preserving formatting across CLI, HTTP and editor remain. Linting also needs N-Triples, N-Quads, JSON-LD, embedded SHACL queries, the shapes editor and an MCP tool. | [G03](specs/G03-shaclc.md#completion-path), [X02](specs/X02-formatter/OUTCOME.md#roadmap-follow-ups-2026-10-12), [X04](specs/X04-linter.md#roadmap-follow-ups-2026-10-12) | M |
| 49 | **Write previews.** Local dry-run flags, UI previews beyond the CSV import panel and merge page, `If-Match` on `/{ds}/update`, and handling of read-only servers and costly bulk previews. | [C15](specs/C15-write-previews.md#roadmap-follow-ups-2026-10-12) | S–M |
| 401 | **Views in in-memory clones.** A persistent clone carries the source's view definitions paused, but an in-memory clone carries none, as it carries no stored queries. Seed the clone's view runtime with the captured definitions, paused with the reason `cloned`, through every binding that adopts an in-memory clone. | [F05](specs/F05-snapshot-repositories/OUTCOME.md#recovery-of-managed-definitions), [C06](specs/C06-clone-to-sandbox.md#completion-path) | S |
| 50 | **Restricted views.** Token scopes limited to graphs, validation and statistics for restricted views, and text scoring local to what a caller can see. | [C12](specs/C12-graph-access-control.md#roadmap-follow-ups-2026-10-12) | M |
| 51 | **ARQ.** Parallel LATERAL, user DESCRIBE handlers, and the remaining client options and builder methods. Sparkles does not evaluate `shortest(…)`, and ARQ does not either. | [G06](specs/G06-arq-query-extensions.md#roadmap-follow-ups-2026-10-12) | M |
| 52 | **History.** Pin rebasing, periods per quad, change feeds per graph, reconstruction of expired states, and history of settings. | [F06](specs/F06-snapshots-and-point-in-time.md#roadmap-follow-ups-2026-10-12) | M |
| 53 | **Inference settings.** Input-graph and RDFS settings in the UI, the remaining RDFS-on-read surfaces, and controls for the import policy. | [C08](specs/C08-inference-freshness.md#roadmap-follow-ups-2026-10-12) | S–M |
| 54 | **ShEx.** Combined SHACL and ShEx guards, schemas from a graph plus a file, and CLI outbound controls. | [G02](specs/G02-shex/OUTCOME.md#roadmap-follow-ups-2026-10-12) | M |
| 55 | **Completions and OpenAPI.** Dynamic and Nushell completions, and an OpenAPI "try it" page with named schemas. This follow-up is outside the completion of formatting, linting and command-line tools. | [X03](specs/X03-openapi-and-completions.md#roadmap-follow-ups-2026-10-12) | S–M |
| 56 | **Legacy MCP resource subscriptions.** | [C11](specs/C11-mcp-server.md#roadmap-follow-ups-2026-10-12) | S |
| 57 | **Scratch-branch expiry** as a dataset setting and under stdio. Today only the server's `--mcp-scratch-branch-ttl` sets it. | [C17](specs/C17-agent-memory.md#roadmap-follow-ups-2026-10-12) | S |
| 71 | **An OWL 2 ontology object API,** as Jena's `jena-ontapi` provides. | [F21](specs/F21-ontology-api.md) | L |
| 73 | **Format coverage.** All formats in `load_str`, `dump_graph`, SPARQL `LOAD` and the JVM and Node bindings, and loading Thrift, Protobuf, RDF/JSON and TriX without a temporary N-Quads copy. This follow-up is outside the completion of formatting, linting and command-line tools. | [G05](specs/G05-command-line-tools.md#roadmap-follow-ups-2026-10-12) | S–M |
| 74 | **RDF Patch.** Patches with several transactions applied as several commits, blank-node identity across datasets, and patch logs. | [F10](specs/F10-replication.md#roadmap-follow-ups-2026-10-12) | M |
| 75 | **Access control.** Validation endpoints for callers whose grants cover part of a dataset, the change feed for data-dependent protections, and hiding IRIs that visible triples mention. | [C12](specs/C12-graph-access-control.md#roadmap-follow-ups-2026-10-12), [C12b](specs/C12b-triple-access-control.md#roadmap-follow-ups-2026-10-12) | M–L |
| 76 | **Jena integration completion.** Complete assembler bodies, direct Graph Store naming per dataset, JVM metrics and batched fallback graph patterns. Expand the standalone HTTP harness to its full accepted cases, and record decisions for optional JVM module, allocator and transaction extensions. | [G08](specs/G08-fuseki-configuration.md#roadmap-follow-ups-2026-10-12), [P04](specs/P04-jvm-bindings/OUTCOME.md#remaining-integration-contract) | M |
| 77 | **Assistant ingestion.** The Ingest panel should say when it stores uploads without extracting facts because server ingestion is off, and server-side document ingestion needs redaction. | [C18](specs/C18-natural-language-questions-and-ingest/OUTCOME.md#roadmap-follow-ups-2026-10-12), [C19](specs/C19-layered-settings.md#roadmap-follow-ups-2026-10-12) | S–M |
| 79 | **Nonnegative weighted betweenness.** Define zero-weight path counting and validate an algorithm before extending the current finite, strictly positive domain. This optional extension is outside the completion path of paths and graph algorithms. | [F14](specs/F14-graph-algorithms.md#nonnegative-weighted-betweenness-semantics) | M–L |
| 80 | **Positive-weight numerical robustness.** Extend the refused mixed-scale domain and define approximation guarantees for distances and path counts in weighted betweenness. This optional extension is outside the completion path of paths and graph algorithms. | [F14](specs/F14-graph-algorithms.md#positive-weight-numerical-robustness) | M–L |
| 92 | **Per-crate Nix builds.** Compare the crate2nix prototype with Crane under matched settings, measure which crates rebuild after source changes, and validate packaging and platforms before deciding whether to migrate. | [X13](specs/X13-per-crate-nix-builds.md) | S–M |
| 399 | **Build and benchmark tooling upkeep.** Check that the Nix development shell evaluates whenever `rust-toolchain.toml` or `flake.lock` changes, since the manual Nix workflow does not catch a toolchain the overlay lacks. Derive the Boxington version in workflows and scripts from the `mise.toml` pin, and build `scripts/nsc-writes.sh` revisions through `mbx` like `nsc-bench.sh`. Keep normalized records, manifests and checksums of benchmark runs in `docs/benchmarks` and move bulky raw logs, headers and responses to digest-pinned storage outside the repository. | [DEVELOPMENT](DEVELOPMENT.md#continuous-integration), [BENCHMARKING](BENCHMARKING.md) | S |
| 190 | **Per-query cost of derivation on demand.** An OWL RL demand over a 40-triple view expands about 100 specialized rules into about 35,000 generated rules in three stages, and planning these rules costs more than the joins. The two equality stages do not depend on the query but run in every session, and they hold about two thirds of the rules and most of the evaluation time. The engine also scans every rule of a stage in each iteration, although only about a tenth have new facts. In order, cache the equality stages' facts per view as the schema table is cached, index rules by the predicates of their atoms so that an iteration plans only the rules with new facts, prove more schema relations complete so that property rules lose their variable predicates, and merge duplicate keys, such as those that differ only by asking for distinct subject and object, if measurement favours it. Also reduce schema-completeness scans and workspace reservation, repeated program setup and derived-fact merging, including the loss of staged schema reuse for `makeTemp` programs and temporary generator work during implicit fact replay. Cache safe specialization and program results per view, merge derived facts as a sorted run, and measure the reasoner development profile's compilation and cache tradeoff. Complete matched Forge measurement of the bounded temporary closure for tiny positive feedback views against materialization and selective, schema, no-feedback and asserted controls, record its retained-fact memory cost, and follow up every accepted slower control before assessing F17's cost target. | [F17](specs/F17-query-time-reasoning.md#outcome) | M |
| 192 | **Budget accounting of the derived-table cache.** The cache is a process-wide 256 MiB budget that no flag sets. Charge it to the result cache, or add a `serve` flag and report its use in `/$/stats`. | [F17](specs/F17-query-time-reasoning.md#outcome) | S |
| 193 | **DESCRIBE under derivation on demand.** `reasoning=demand` refuses DESCRIBE with `501`. Turn each described resource into demands for the facts the dataset's DESCRIBE mode follows before the description runs. | [F17](specs/F17-query-time-reasoning.md#outcome) | S |
| 210 | **One pass over the unbound build rows of a spilled outer join.** OPTIONAL, semi, anti and MINUS joins read that file once per partition. A bitmap of probe rows shared between the jobs would let one job read it once, as the inner join does. | [X08](specs/X08-disk-spill-and-server-memory.md#spilled-join-amplification-item-94-2026-10-24) | S |
| 221 | **Incremental views over inferred data.** A view or live query on a dataset with RDFS on read or materialized inferences evaluates its whole query again at each commit that touches it. Incremental maintenance needs circuit scans that read the inferred graph, and the RDFS rewrite compiled into the circuit again when the schema changes. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | M |
| 224 | **A server-wide limit of live queries.** `--max-live-queries` and `--max-live-queries-per-principal` count per dataset, where the design counts over the server. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | S |
| 227 | **Disclosure guards in circuits.** Circuits treat the graphs a C22 guard protects as unreadable, because a request decides a guard at its own snapshot. A view principal or live reader whose footprints prove a reports graph does not see it. Circuits need a guard decision that a pass can update as the footprints change. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | M |
| 230 | **PostgreSQL and DuckDB sources for mappings.** The `postgres` and `duckdb` connection kinds of F19 §4.4 with secret references, the outbound policy for PostgreSQL hosts, the R2RML test cases on PostgreSQL in CI, and acceptance examples A1 and A10. | [F19](specs/F19-mappings-and-virtual-graphs/SPEC.md#outcome) | M |
| 232 | **RML join conditions written as expression maps,** and the IRI checks of RMLTC0027b. Six RML-Core test cases are excluded until then. | [F19](specs/F19-mappings-and-virtual-graphs/SPEC.md#outcome) | S–M |
| 233 | **A Mappings page in the web UI** that lists stored mappings, edits them, runs them with a dry run first and shows the last run. | [F19](specs/F19-mappings-and-virtual-graphs/SPEC.md#8-stored-mappings) | M |
| 234 | **Stored mapping runs as tasks.** Runs longer than 10 seconds become tasks of `/$/tasks` with progress and cancellation, as F19 §2.2 describes. A run is one request. | [F19](specs/F19-mappings-and-virtual-graphs/SPEC.md#outcome) | S–M |
| 235 | **The W3C CSVW test suite,** vendored as test data for the RDF tests in minimal and standard mode. | [F19](specs/F19-mappings-and-virtual-graphs/SPEC.md#131-conformance-and-equivalence) | S–M |
| 236 | **The remaining SPARQL functions in function maps,** such as the date, numeric and term functions, as `urn:x-sparkles:fn:` functions. | [F19](specs/F19-mappings-and-virtual-graphs/SPEC.md#outcome) | S |
| 237 | **Parquet sources in object stores** (`s3`, `gcs`, `azure`) through `object_store`, as F19 §4.4 lists. Parquet files are read from the local disk. | [F19](specs/F19-mappings-and-virtual-graphs/SPEC.md#outcome) | M |
| 238 | **Template lookups that keep the dataset's blank nodes.** A lookup that binds an existing blank node writes a new one, because the loader scopes blank node labels per file. | [F19](specs/F19-mappings-and-virtual-graphs/SPEC.md#outcome) | S–M |
| 239 | **JSON numbers with their source lexical form,** as F19 §4.2 asks. serde_json prints a non-integral number in its own form. | [F19](specs/F19-mappings-and-virtual-graphs/SPEC.md#outcome) | S |
| 404 | **Python mapping run controls.** `Dataset.run_mapping` and `Dataset.mappings.run` take no cancellation token or timeout and wait without Python's signal handlers, so Ctrl-C cannot stop them. Pass the token and deadline as write options, as the JVM and Node.js runs do. | [F19](specs/F19-mappings-and-virtual-graphs/SPEC.md#outcome) | S |
| 405 | **One test hook for binding control phases.** The JVM algorithm write and Node.js statistics tests each use their own file-based handshake, built into the binding under a test feature, to pause at a progress boundary. A shared library hook would replace both. The JVM probe's deadline-forwarding check compares the deadline with itself. | [P04](specs/P04-jvm-bindings/SPEC.md#outcome), [P05](specs/P05-node-bindings.md#outcome) | S |
| 260 | **Person-checked GraphRAG evaluation samples.** No person checked the 30 generated global questions or judged a sample of the Opus judgements, both of which §9 asks for. | [C22](specs/C22-graph-rag-community-summaries.md#paid-acceptance-and-evaluation-2026-10-24) | S |
| 261 | **GraphRAG evaluation on more sets.** §9 also names meeting transcripts and a DBpedia subset about one domain, and a text baseline that retrieves by vector search rather than BM25 over 20 documents. The effect of §7.3 on C18 and the refresh tokens per thousand changed facts remain unmeasured. | [C22](specs/C22-graph-rag-community-summaries.md#paid-acceptance-and-evaluation-2026-10-24) | M |
| 262 | **Classifying whole-graph questions.** `auto` mode sent 14 of 30 generated global questions to local mode, where most ended in a clarification or as unanswerable. The draft's scope decision needs examples or a rule for questions about the whole dataset. | [C22](specs/C22-graph-rag-community-summaries.md#paid-acceptance-and-evaluation-2026-10-24) | S |
| 264 | **Explicit property order in the other model schemas.** Constrained decoding writes members in the order a schema lists them, and only the global map and reduce schemas name an order. The draft, report and ingest schemas reach providers sorted by name, and a measured run should decide whether a reasoning-first order helps them. | [C22](specs/C22-graph-rag-community-summaries.md#paid-acceptance-and-evaluation-2026-10-24) | S |
| 270 | **Scope challenges inside MCP tools.** `/$/mcp` challenges a missing `sparkles:read` or `sparkles:update` before a message runs, but an operation inside a tool that needs admin, such as deleting a protected branch, is refused as a tool error without naming `sparkles:admin`. The tools need a way to return the challenge, or the message check needs to know which arguments need admin. | [C11](specs/C11-mcp-server.md#oauth-for-the-mcp-endpoint-item-67-2026-10-24) | S |
| 329 | **Node query and export controls.** Recheck the small-query process variation and the accepted 0.4% to 1.5% export costs of borrowed native term validation on a quiet Intel host. Keep complete term consumption and fresh-process pairing. | [P05](specs/P05-node-bindings.md#borrowed-native-term-validation), [Benchmarks](benchmarks/bindings/RESULTS.md#native-term-validation) | S |

<a id="tier-6-release-work"></a>

## Tier 6: later operational completion

Release artifacts, recovery, startup, audit and the adopted encryption workflows advance
in the parallel completion program above. These larger operational extensions retain
their explicit adoption, compatibility and failure-model decisions. Their later placement
does not waive a required phase or delay an independent accepted capability.

| # | Item | Spec | Size |
|---|---|---|---|
| 63 | **Authentication additions.** Mutual TLS, OCSP, opaque-token introspection, front-channel logout and Cloudflare group lookup. | [C09](specs/C09-dataset-access-control.md#roadmap-follow-ups-2026-10-12) | M |
| 280 | **Replication spec refresh.** Answer F10's open questions 15 to 23 before Phase 2 starts. They cover derived state on replicas, branches, encryption, the fencing and data-loss guarantees of lease-based failover, Kubernetes, scope, the estimate, how a peer joins and exchanges credentials, and one account of high availability with recovery targets and client routing. | [F10](specs/F10-replication.md#11-open-questions) | S |
| 402 | **Startup degraded datasets and replay progress.** A dataset whose full-text, vector or spatial index is unavailable opens without that index and reports a `degraded` state with the reason, and an opening dataset reports the progress of its log replay in `/$/ready`. The bind-before-open states, policies and access rules stay as they are. A one-dataset store measured about 8 ms (6%) slower to become ready on a loaded laptop, and a quiet Intel host should confirm or remove that cost. | [C01](specs/C01-observability-and-budgets.md#roadmap-follow-ups-2026-10-12) | M |
| 66 | **Replication.** Read replicas, preserved primary IDs, promotion, failover, synchronous replication, and a patch log or continuous archive. | [F10](specs/F10-replication.md#roadmap-follow-ups-2026-10-12) | XL |

## Tier 7: compatibility leftovers

These were left out on purpose. They stay on the list and are reconsidered when users ask
for them.

| # | Item | Spec | Size |
|---|---|---|---|
| 69 | **ARQ extras.** JavaScript functions, `afn:context`, the Leviathan library, the `JSON` query form, ARQ's `EXISTS { … }` group element, `AGG` with more than one argument, printing in `afn:print` and `afn:execTime`, and locale collation. This lower-priority extension is outside the completion path of server and Fuseki integration. | [G06](specs/G06-arq-query-extensions.md#roadmap-follow-ups-2026-10-12) | M each |
| 70 | **Jena-compatible GeoSPARQL mode.** Not adopted. Sparkles keeps the answers that [COMPARISON.md](COMPARISON.md#geosparql) describes, and the number stays listed so that it is not reused. | [G01](specs/G01-geosparql/OUTCOME.md#roadmap-follow-ups-2026-10-12) | M |

## Completed

These items are finished. [FEATURES.md](FEATURES.md) describes what they deliver, and the
linked spec Outcome records how they landed.

- **2.** Server-wide cache sizing and query memory pool. [X08](specs/X08-disk-spill-and-server-memory.md) Phase 1
- **3.** Group commit as the default. [X09](specs/X09-group-commit-by-default.md)
- **5.** Correctness fixes in path search, change logs and Fuseki endpoints. [F07](specs/F07-path-search.md#roadmap-follow-ups-2026-10-12), [F06](specs/F06-snapshots-and-point-in-time.md#roadmap-follow-ups-2026-10-12), [G08](specs/G08-fuseki-configuration.md#roadmap-follow-ups-2026-10-12)
- **10.** Vector index maintenance. [F04](specs/F04-vector-search.md#roadmap-follow-ups-2026-10-12)
- **11.** Time slider. [U01](specs/U01-time-slider.md)
- **13.** Current shape standards. [G03](specs/G03-shaclc.md#roadmap-follow-ups-2026-10-12), [G02](specs/G02-shex/OUTCOME.md#roadmap-follow-ups-2026-10-12)
- **16.** Index compression after predicate patterns. [X11](specs/X11-index-compression-and-predicate-patterns.md#outcome)
- **22.** RML, R2RML and YARRRML loading. [F19](specs/F19-mappings-and-virtual-graphs/SPEC.md#outcome) Phase 1
- **231.** JVM and Node.js stored mappings and direct mapping runs. [F19](specs/F19-mappings-and-virtual-graphs/SPEC.md#outcome)
- **64.** Bind-before-open startup with opening, failed and degraded states. [C01](specs/C01-observability-and-budgets.md#roadmap-follow-ups-2026-10-12)
- **62.** Dataset and branch administration audit events. [C09](specs/C09-dataset-access-control.md#dataset-administration-audit)
- **67.** MCP OAuth. [C11](specs/C11-mcp-server.md#oauth-for-the-mcp-endpoint-item-67-2026-10-24)
- **68.** Sealed runtime secrets. [C19](specs/C19-layered-settings.md#roadmap-follow-ups-2026-10-12)
- **83.** Verified ShEx schema copies and full validation for legacy configurations. [C10](specs/C10-write-time-validation.md#known-gap-schema-copy-digest-2026-10-16)
- **85.** Hash-join, ordered DISTINCT and encrypted spill. [X08](specs/X08-disk-spill-and-server-memory.md) §6.3 and §6.7
- **87.** Small incremental runs over a kept closure. [F16](specs/F16-materialization-at-scale.md#roadmap-follow-ups-2026-10-18)
- **89.** Ask and `check_query` on large datasets. [C18](specs/C18-natural-language-questions-and-ingest/OUTCOME.md#roadmap-follow-ups-2026-10-12)
- **94.** Spilled join amplification. [X08](specs/X08-disk-spill-and-server-memory.md#spilled-join-amplification-item-94-2026-10-24)
- **95.** Close-only locks on the database directory. [P06](specs/P06-library-admin-api.md), [F05](specs/F05-snapshot-repositories/SPEC.md#23-restore)
- **96.** Streaming NOT EXISTS over a Cartesian product. [X08](specs/X08-disk-spill-and-server-memory.md#automatic-streaming-without-spill-item-96-2026-10-23)
- **97.** Retuned `small` memory profile. [X08](specs/X08-disk-spill-and-server-memory.md#small-profile-retune-and-the-phase-1a-control-item-97-2026-10-23)
- **98.** Materialization limit and F16 acceptance at scale. [F16](specs/F16-materialization-at-scale.md#outcome)
- **99.** Truncated eager responses logged as truncated. [X06](specs/X06-benchmark-coverage.md#sp2bench-q4-and-q6-follow-up-2026-10-23)
- **102.** The plan drawn on the query's pattern. [U04](specs/U04-live-query-profiling.md#11-phase-4-the-plan-drawn-on-the-querys-pattern-2026-10-23) Phase 4
- **103.** Path finder. [U09](specs/U09-path-finder.md#outcome)
- **104.** Time-lapse and commit diffs on the canvas. [U01](specs/U01-time-slider.md#phase-4-time-lapse-and-commit-diffs-on-the-canvas-2026-10-23) Phase 4
- **105.** Canvas lenses and the derivation tree of an inferred edge. [U10](specs/U10-canvas-lenses.md#outcome)
- **106.** Validation overlay. [U07](specs/U07-data-quality-and-schema-diagram.md#11-phase-4-a-validation-overlay-on-the-graph-2026-10-23) Phase 4
- **107.** Linked views. [U11](specs/U11-linked-views.md#outcome)
- **108.** Faster FSST reads. [X11](specs/X11-index-compression-and-predicate-patterns.md#item-108-faster-fsst-reads)
- **109.** `order-by-full` with one codegen unit for sparkles-core, which runs the same instructions and keeps the setting. [Benchmarking](BENCHMARKING.md#release-build-settings)
- **110.** Bounded write stall of the locked full-text checkpoint. [X09](specs/X09-group-commit-by-default.md#phase-4)
- **135.** Partial derivation trees within search budgets. [F18](specs/F18-explanations-and-entailment-diffs.md#outcome)
- **136.** Explanations through generalized facts and default geometries. [F18](specs/F18-explanations-and-entailment-diffs.md#outcome)
- **137.** Rule bindings and passed builtins in derivation trees. [F18](specs/F18-explanations-and-entailment-diffs.md#outcome)
- **138.** Interruptible RDFS schema builds for queries and explanations. [F18](specs/F18-explanations-and-entailment-diffs.md#outcome)
- **139.** Mixed materialized and RDFS-on-read derivations. [F18](specs/F18-explanations-and-entailment-diffs.md#outcome)
- **150.** Weighted path searches from both ends. [F07](specs/F07-path-search.md#weighted-searches-from-both-ends-2026-10-24)
- **114.** Client time of large results, from eager serializers compiled with the engine and buffered. [Benchmarking](BENCHMARKING.md#release-build-settings), [Benchmarks](benchmarks/engine/RESULTS.md#the-cost-of-result-serialization)
- **161.** Front coding on warm DBpedia after item 108, which forge measured at a geometric mean of 1.004. [X11](specs/X11-index-compression-and-predicate-patterns.md#item-108-faster-fsst-reads)
- **180.** Plan pattern references only for a shown plan. [U04](specs/U04-live-query-profiling.md#phase-4-2026-10-23) Phase 4
- **181.** Compiler provenance in the Namespace query screen. [Benchmarking](BENCHMARKING.md#namespace-query-screens)
- **182.** `subquery-agg` after the U02 forms, U11 Phase 3 and item 94 merges, which forge measured unchanged. [Benchmarking](BENCHMARKING.md#namespace-query-screens)
- **191.** Schema facts from derived instances under demand, including declarations, schema-term equality and preserved fresh witnesses. [F17](specs/F17-query-time-reasoning.md#instance-derived-schema-facts)
- **242.** `two-hop-count` after the result buffer, which forge measured at 0.998 of the build before the buffer in 24 pinned rounds. [Benchmarks](benchmarks/engine/RESULTS.md#the-cost-of-result-serialization)

- **307.** Bounded competitive assessment of bindings, durable writes and grouping queries. Residual costs remain under items 33, 290 and 293. [X06](specs/X06-benchmark-coverage.md#outcome)

- **220.** Backups and persistent clones of view definitions, captured with the data and restored paused. [F05](specs/F05-snapshot-repositories/OUTCOME.md#recovery-of-managed-definitions), [F13](specs/F13-incremental-views-and-provenance.md#outcome)

- **397.** Native Rust direct mapping controls through conversion and the final atomic load. [F19](specs/F19-mappings-and-virtual-graphs/SPEC.md#outcome)
- **403.** Credential expiry for long-lived reads and fenced OIDC sign-in. [C09](specs/C09-dataset-access-control.md)
- **432.** DBpedia `births-by-decade` decimal arithmetic and inline date parts, which leave cursor BIND reuse under item 293. [X06](specs/X06-benchmark-coverage.md#decimal-arithmetic-and-inline-date-components)

## Keeping this current

Update this document when an item ships, changes tier or gets a new spec. A new known gap
in a feature guide gets a roadmap item in the same commit. When an item ships, its spec's
Outcome records what landed, [FEATURES.md](FEATURES.md), the affected guide and the
relevant [benchmark report](benchmarks/README.md) change
in the same commit, and the Known gaps bullet drops the item's number. The item's row then
moves to a one-line entry under [Completed](#completed). When part of an item ships, its
row is cut down to the part that remains.
