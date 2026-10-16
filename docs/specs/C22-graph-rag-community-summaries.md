# C22: Community summaries for global questions (GraphRAG)

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 is the community build from F14's Leiden, the
> summaries written by the server's model providers with citations to facts, their
> storage as triples with provenance and review status, the build task and the settings.
> Phase 2 adds global questions in Ask, community context in `recall` and the brief, and
> the MCP tool. Phase 3 refreshes communities and summaries incrementally after commits.
>
> **User docs:** none, because nothing is built.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

This design was written from Edge et al., "From Local to Global: A Graph RAG Approach to
Query-Focused Summarization" (arXiv:2404.16130, 2024), Microsoft's public GraphRAG
documentation of its indexing pipeline, community reports and global search, Traag,
Waltman and van Eck, "From Louvain to Leiden: guaranteeing well-connected communities"
(Scientific Reports, 2019), the W3C PROV-O and SKOS recommendations, and the Sparkles
code. It builds on the graph algorithms of [F14](F14-graph-algorithms.md), the model
providers, roles, budgets, review inbox and Ask pipeline of
[C18](C18-natural-language-questions-and-ingest.md), the memory tools of
[C17](C17-agent-memory.md), embeddings on write ([F08](F08-embeddings-on-write.md)),
access control ([C12](C12-graph-access-control.md),
[C12b](C12b-triple-access-control.md)), and, for incremental refresh, the change feed of
[F06](F06-snapshots-and-point-in-time.md) and the incremental core of
[F13](F13-incremental-views-and-provenance.md).

## 1. Summary, goals, non-goals

C18's Ask turns a question into a SPARQL query and summarizes the rows. That answers
questions about named things: who is on the payments team, which projects use Postgres.
It does not answer questions about the whole dataset, such as "what are the main themes
in these meeting notes" or "which areas of the organisation have the most open risks",
because no single query returns the answer and the rows that bear on it do not fit in a
model's context. C17's `recall` has the same limit: it starts from seeds that match the
question's words.

Edge et al. answer such questions by partitioning the graph into a hierarchy of
communities with Leiden, writing a summary of each community ahead of time, and
answering a global question by map-reduce over the summaries. This spec adds that to
Sparkles over data that is already a graph, with the summaries kept as triples that
cite the facts they come from and carry a review status.

**Goals**

1. A hierarchy of communities over a dataset's entities, computed by F14's Leiden on a
   configurable projection, with identities that stay stable across rebuilds.
2. A report per community: a title, a summary and findings, each finding citing the
   facts that support it, written by the server's model providers through a new
   `community` role with C18's budgets and escalation.
3. Reports stored as triples in a reports graph, with PROV-O provenance (model, prompt
   version, source commit) and the review statuses of C18 §8.8.
4. Global questions in Ask, answered from the reports with citations that resolve to
   facts.
5. Community context in `recall` and in the memory brief.
6. Refresh after commits that regenerates only the reports whose facts changed enough,
   within a token budget.
7. An evaluation that compares global answers with C18's Ask and with chunk retrieval.

**Non-goals**

- Extracting the entity graph from text. C18's ingestion does that, and this spec reads
  the graph it produces together with everything else.
- Claims or covariates as Microsoft's pipeline extracts them. Facts with reifiers already
  carry their sources.
- Summaries without a model. A dataset without a `community` role has no reports.
- Summaries that differ per caller. Reports are built for one principal's view (§6).

## 2. User-visible behaviour

### 2.1 Settings

A dataset's GraphRAG settings are a settings kind of C19, `graphrag`, stored in
`<db>/graphrag.json` and changed through `/$/settings` by dataset admins.

```json
{
  "enabled": true,
  "principal": "reports",
  "projection": { "graphs": ["https://example.org/memory/*", "urn:graph:org"],
                  "exclude": ["rdf:type", "prov:wasGeneratedBy"],
                  "weight": null },
  "levels": 3,
  "resolution": 1.0,
  "minCommunitySize": 3,
  "reportsGraph": "urn:x-sparkles:graphrag:reports",
  "language": "en",
  "maxFactsPerCommunity": 400,
  "refresh": { "mode": "auto", "debounceSeconds": 300, "minChangedFraction": 0.1 },
  "budget": { "tokensPerBuild": 2000000, "tokensPerDay": 500000 }
}
```

| Field | Meaning |
|---|---|
| `principal` | Whose view of the dataset the projection and the reports read (§6). Required. |
| `projection` | The graphs read, the predicates left out, and an optional weight property on reifiers. Edges are triples between resources in those graphs. The default leaves out `rdf:type` and the provenance predicates of C17 §3.2, so that classes and activities do not join every community. |
| `levels` | The deepest level of the Leiden hierarchy kept, from 0 (coarsest). |
| `minCommunitySize` | Smaller communities get no report and are merged into their parent's report. |
| `maxFactsPerCommunity` | The facts sent for a leaf community's report, chosen as §3.2 says. |
| `refresh` | `manual`, or `auto` with a debounce and the share of a community's facts that must change before its report is rewritten (§5). |
| `budget` | Tokens a full build and a day of refreshes may use. The provider's own budget of C19 applies as well. |

### 2.2 Building

`POST /$/graphrag/{ds}/build` starts a task in `/$/tasks` that runs the steps of §3.
Before it calls a model, it reports an estimate of the communities, the facts and the
tokens, and with `"confirm": false` (the default from the UI) waits for confirmation, as
C18's ingestion does for its cost estimates. `GET /$/graphrag/{ds}` reports the state:
the commit of the last build, the number of communities per level, the reports by
status, the tokens used, and the communities waiting for a refresh.
`sparkles graphrag --loc DB build|status|refresh|reports` does the same from the
command line.

### 2.3 Reading reports

Reports are triples in the reports graph (§4), so SPARQL reads them. The UI's dataset
page gains a **Communities** tab with the hierarchy as a tree, each community's members,
its report with findings and citations, and the review actions. The explorer can colour
nodes by community, from the same triples. The review inbox of C18 §8.9 gains the kind
`community`.

### 2.4 Asking global questions

Ask classifies each question as `local` or `global` (§7.1). A global question is
answered from the reports, and the answer shows the reports it used, each finding it
relies on, and the facts each finding cites. A person can switch a question to the other
mode with **Answer from the data** or **Answer from summaries**. `POST /{ds}/ask` takes
`"mode": "auto" | "local" | "global"`, and the response gains a `reports` member.

### 2.5 Recall, brief and MCP

`recall` takes `communities: true`, which adds, after the facts, the report of each
seed's community at the requested `communityLevel` (the finest by default), as header
lines and summary lines in the text format of C17 §5.5. `sparkles memory brief` adds the
reports of the coarsest level for the project scope. The MCP read tool
`community_reports` lists the reports that match a text, by vector and text search over
the summaries, with their members' counts and findings, so an external agent can run its
own global answering.

## 3. The build

### 3.1 Communities

1. **Projection.** The settings' projection, as an F14 named projection
   `graphrag`, built undirected from the principal's view at the head commit.
2. **Leiden.** `algo:leiden` with the settings' resolution and levels and a fixed seed,
   which yields a community per node and level. Communities smaller than
   `minCommunitySize` are folded into their parent.
3. **Stable identities.** Each new community is matched with the previous build's
   community at the same level that shares the most members, when their Jaccard
   similarity is at least 0.5, and inherits its IRI. Others get a new IRI
   `urn:x-sparkles:graphrag:{ds}:{level}:{uuid}`. Unchanged members and a stable IRI let
   §5 reuse reports.

### 3.2 The input of a report

A leaf community's input is its members' facts in the principal's view, as text in the
format of C17's `recall`: each entity with its label and types, then its facts, with a
numbered citation per fact. When the facts exceed `maxFactsPerCommunity`, they are
chosen by the degree of their subject within the community, the internal edges first,
then each member's literal properties up to five per member, then edges to other
communities. The facts sent are recorded with the report, so a citation can be checked.

A higher-level community's input is its child communities' reports, each with its
title, summary and findings, plus the edges between children, as Edge et al. do when the
facts do not fit. Reports are therefore written bottom-up.

### 3.3 The model call

The new role `community` of C18 §3.7 writes the report. It has its own ordered list of
provider and model pairs, falls back to `summarize`'s list when it has none, and uses the
structured-output levels of C18 §3.6. The output is a `CommunityReport`:

```json
{ "title": "Payments platform team",
  "summary": "Three to five sentences.",
  "findings": [ { "text": "The team owns the ledger service and its two databases.",
                  "cites": [3, 7, 12] } ],
  "rating": 6 }
```

`rating` is the model's estimate of the community's importance from 0 to 10, as Edge et
al. ask for, and serves only to order reports within a level. The prompt tells the
model that the facts are data, as C11 §4.10 requires.

### 3.4 Checks

The server checks each report before it stores it, as C18 checks summaries that cite
rows:

- Every citation number names a fact that was sent. A finding without a valid citation
  is dropped.
- A finding that names an entity must name a member or a fact's object. Unknown names
  are counted as `unsupported` and the finding is dropped.
- The text passes the `secret-detected` check of C18's transcripts.
- A report whose findings were all dropped is retried once with the escalation of C18
  §5.5, then stored as `failed` with the reason.

### 3.5 Cost

A build makes one model call per community with a report. With the settings' sizes, a
dataset of 100,000 entities at three levels produces some thousands of calls. The
estimate before the build counts the communities and the input tokens exactly and the
output tokens from `maxOutputTokens`. A build stops when it reaches
`budget.tokensPerBuild`, keeps the reports it wrote, and reports the remaining
communities as `pending`.

## 4. Storage

Reports are triples in the reports graph, written by the build task's principal
`urn:x-sparkles:principal:graphrag` in commits of kind `graphrag`, one per level.

```turtle
PREFIX gr:   <urn:x-sparkles:graphrag#>
PREFIX prov: <http://www.w3.org/ns/prov#>
PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
PREFIX spk:  <urn:x-sparkles:>

<urn:x-sparkles:graphrag:org:2:7f…> a gr:Community ;
    gr:level 2 ; skos:broader <urn:x-sparkles:graphrag:org:1:3a…> ;
    gr:member ex:ana, ex:ledger, ex:payments ;
    gr:memberCount 41 ; gr:factCount 388 ;
    gr:report <urn:uuid:…r1> .

<urn:uuid:…r1> a gr:Report ;
    rdfs:label "Payments platform team" ;
    gr:summary "…"@en ;
    gr:rating 6 ;
    gr:status "unreviewed" ;
    gr:finding <urn:uuid:…f1> ;
    gr:sourceCommit 412 ;
    prov:wasGeneratedBy <urn:uuid:…a1> .

<urn:uuid:…f1> gr:text "The team owns the ledger service and its two databases."@en ;
    gr:cites <<( ex:payments ex:owns ex:ledger )>>, <urn:uuid:…reifier-of-a-fact> .

<urn:uuid:…a1> a prov:Activity ;
    prov:wasAssociatedWith <urn:x-sparkles:principal:graphrag> ;
    spk:model "anthropic/claude-sonnet-5-5" ; spk:promptVersion "community-1" ;
    spk:inputTokens 9120 ; spk:outputTokens 610 ;
    prov:startedAtTime "…"^^xsd:dateTime .
```

A cited fact is a triple term, and also the fact's reifier when it has one, so the
citation reaches the fact's own sources through C17's provenance. A report for a higher
level cites the child reports it used with `gr:citesReport`.

**Statuses.** `unreviewed` until a person reviews it in the inbox, then `reviewed` or
`rejected`. A report whose community changed enough after its build (§5) becomes
`stale` until it is rewritten. `failed` keeps the reason. Unreviewed reports are usable,
as C18 §8.8 decided for agent facts, and Ask's `reviewedOnly` uses only reviewed ones.
A person can edit a report's text in the UI. The edit is a new report that supersedes the
old one with `prov:wasRevisionOf`, written by the person, with status `reviewed`.

**Search.** The summaries and findings are text, so the dataset's full-text index can
cover `gr:summary` and `gr:text`, and an F08 embedding index on `gr:summary` gives vector
search. `graphrag.json` creates neither, and the build reports when they are missing,
because global answering ranks reports by them (§7.2).

## 5. Refresh

`refresh.mode: "auto"` follows the change feed. After `debounceSeconds` without commits,
or at most every ten debounce periods while writes continue, the refresh task:

1. reads the commits since the last build or refresh through `Store::changes` with the
   principal's graphs, and counts the changed facts per community, by the community of
   each change's subject and object;
2. marks a leaf community `stale` when its changed facts reach `minChangedFraction` of
   its facts, or when an entity joined or left it;
3. re-runs Leiden when the changed facts across the projection reach 5% of its edges,
   and matches identities as §3.1 does, so most communities keep their IRI and members;
4. rewrites the reports of stale leaf communities, then of their ancestors whose stale
   children reach `minChangedFraction` of their children, within `tokensPerDay`.

In Phase 3, the per-community counts of step 1 are an F13 view grouped by community over
the projection's predicates, so a refresh reads the counts instead of the feed. A commit
that the feed lists with counts only marks every community stale and schedules a full
rebuild under the budget, since its changes are not known per community.

## 6. Access control

A report summarizes facts as the settings' `principal` reads them. A caller may read the
reports graph only when its view covers the principal's, the rule of F13 §6.1, which the
server enforces by refusing reads of the reports graph to other callers with a C12
graph rule that the settings install. Ask and `recall` use reports only for callers who
may read them, and otherwise answer locally and say so. A dataset whose readers have
different views can define several GraphRAG configurations, each with its own principal
and reports graph, as `graphrag.json` takes a list in Phase 2.

The model sees the facts of the principal's view. C18 §3.5's `sendByProvider` applies:
a provider allowed only the schema cannot serve the `community` role, and the build
refuses to start.

## 7. Global answering

### 7.1 Classifying a question

The classification is a deterministic check first: a question that names an entity that
`link_entities` links `exact` and asks about it is `local`. Otherwise the `draft` role
returns its usual draft together with a `scope` field of `local` or `global`, which the
draft schema of C18 §5.3 gains. A `global` answer skips the SPARQL steps. The person can
override the mode, and the evaluation of §9 measures the classification.

### 7.2 Map and reduce

1. **Select.** The reports at the chosen level, the finest level whose report count is
   at most `maxReports` (100 by default), ranked by vector and text similarity to the
   question with `spk:hybridSearch` over the summaries, and by `gr:rating` without those
   indexes. Reports the caller may not use are left out.
2. **Map.** The reports are sent in batches that fill the `summarize` role's context, and
   for each batch the model returns points that bear on the question, each with a score
   from 0 to 100 and the reports and findings it uses. Batches run in parallel within the
   provider's concurrency.
3. **Reduce.** Points with a score of 0 are dropped, the rest are ordered by score and
   sent, up to the context, to `summarize`, which writes the answer citing points.
4. **Check.** Every citation must resolve through a point to a finding and its facts.
   Uncited sentences are flagged as in C18's summaries.

The answer counts its model calls against the per-question limit of C18 §11.3, extended
to `4 + ceil(reports / batch)` calls for global answers, and against the provider's
budget. When the reports are missing, stale beyond `maxStale` or unreadable to the
caller, Ask answers locally.

### 7.3 Local questions with community context

A local question's grounding step gains the titles and summaries of the linked entities'
communities, at most three, so that the draft knows the neighbourhood. The measured
effect decides whether this stays on by default (§9).

## 8. Architecture

- `crates/sparkles-server/src/graphrag/`: `settings.rs` (the C19 kind), `build.rs`
  (projection, Leiden through `sparkles_core::algo`, identity matching, inputs),
  `report.rs` (the prompt, the `CommunityReport` schema and the checks), `store.rs`
  (triples and statuses), `refresh.rs`, and `global.rs` (map and reduce for Ask).
- `crates/sparkles-server/src/models/config.rs`: the `Role` enum gains `Community`, with
  its fallback to `Summarize`, and the settings and UI role lists gain it.
- `crates/sparkles-server/src/ask/`: the `scope` field, the global path beside the SPARQL
  path, and the `reports` member of the response.
- `crates/sparkles-server/src/mcp/`: `community_reports`, and `communities` on `recall`.
- Build and refresh run as tasks in the registry that `ingest::Runtime` keeps for
  ingestion and memory maintenance, with progress per level and cancellation between
  model calls, and show in `GET /$/memory/{ds}/maintenance`.
- Metrics: `sparkles_graphrag_reports{dataset,status}`,
  `sparkles_graphrag_tokens_total{dataset,phase}` and
  `sparkles_graphrag_stale_communities{dataset}`.

## 9. Evaluation

**Sets.** The demo organisation graph of C18 §11.1 after ingesting its labelled local
documents, a set of meeting transcripts imported by C18's adapters, and a public graph
of moderate size, such as a subset of DBpedia about one domain.

**Questions.** As Edge et al. do, a model generates personas and tasks from a
description of each dataset and writes global questions for them. A sample is checked
by hand and kept in the repository. The sets of C18 §11.1 serve as local questions.

**Comparisons.** For each global question, four answers: C18's Ask in local mode,
retrieval of text chunks by vector search over the ingested sources with the same
`summarize` model, global answering at the coarsest level, and at the finest level kept.
A judge model compares answers pairwise on comprehensiveness, diversity, empowerment and
directness, the criteria of Edge et al., with both orders of each pair, and the report
gives win rates with confidence intervals. A sample is judged by people to check the
judge.

**Other measures.** Citation validity (the share of cited findings and facts that exist
and support the sentence, on a judged sample), the share of findings dropped by the
checks, classification accuracy of §7.1 against labels, the effect of §7.3 on C18's
execution accuracy, build tokens and time per thousand entities, refresh tokens per
thousand changed facts, and Ask latency in global mode.

## 10. Phases

**Phase 1.** The settings kind, the build with Leiden, stable identities, the
`community` role, reports with checks and statuses, storage, the build and status routes
and CLI, the Communities tab, the inbox kind, and manual refresh.

**Phase 2.** Global answering in Ask with classification and the mode switch, community
context in `recall` and the brief, `community_reports`, several configurations, and the
evaluation.

**Phase 3.** Automatic refresh from the change feed, then from an F13 view.

## 11. Acceptance examples

- **A1.** On a graph of two dense clusters joined by one edge, a build makes two
  communities at the finest level, each with a report whose findings cite only facts of
  its cluster.
- **A2.** A report whose model output cites fact 99 when 40 were sent loses that
  finding, and the report records one dropped citation.
- **A3.** A second build after adding one fact to a cluster of 50 facts keeps both
  community IRIs and, with `minChangedFraction` 0.1, writes no new report.
- **A4.** Adding six facts to that cluster marks its report `stale` and the refresh
  rewrites it, superseding the old report.
- **A5.** A caller whose grants exclude one of the projection's graphs gets local answers
  only and a note that summaries are unavailable to it.
- **A6.** "What are the main areas of work in this organisation?" in Ask on the demo set
  is classified `global`, and the answer cites reports whose findings cite facts.
- **A7.** With `reviewedOnly`, the same question uses only reviewed reports, and with
  none it answers locally.
- **A8.** A build whose estimate passes `tokensPerBuild` stops at the budget and lists
  the remaining communities as `pending`.
- **A9.** `recall` with `communities: true` for `ex:ana` adds the report of her team's
  community after her facts.
- **A10.** A dataset whose `sendByProvider` limits the `community` role's provider to the
  schema refuses to build, naming the setting.

## 12. Rejected alternatives

- **Summaries in a side store.** Triples keep reports under the dataset's history,
  backups, access control and review, and SPARQL and the explorer read them without new
  code.
- **Microsoft's pipeline as a dependency.** It is a Python package that extracts its own
  graph from text and stores parquet files. Sparkles has the graph already and needs the
  reports in the dataset.
- **Summaries computed at question time only**, the approach Microsoft Research described
  as LazyGraphRAG. It avoids the build cost but pays a large model cost per global
  question and has nothing to review. It remains a fallback for datasets without
  reports through §7.2's map over communities with facts in place of reports, which is
  open question 2.
- **Communities by label propagation or connected components.** Components are too
  coarse on connected data, and label propagation gives no hierarchy. Leiden guarantees
  connected communities and gives levels.
- **A report per entity.** It multiplies calls and repeats what `recall` already shows.
- **Rewriting every report on any change.** The threshold and the budget keep refresh
  cost proportional to change.

## 13. Open questions

1. **Level choice for Ask.** Edge et al. found intermediate levels a good balance. The
   default chosen is the finest level with at most 100 reports, and the evaluation sets
   it.
2. **Answers without reports.** Global answering from raw community facts at question
   time would serve datasets that never build. The default chosen is a local answer.
3. **Multilingual reports.** One `language` per configuration. Reports in several
   languages would multiply cost.

## Outcome

Not built. See [ROADMAP](../ROADMAP.md) item 12.

### Shared memory representation follow-up

[Roadmap](../ROADMAP.md) item 81 plans a shared profile for agent memory and personal
knowledge in [C17](C17-agent-memory.md#shared-memory-representation) and
[C18](C18-natural-language-questions-and-ingest.md#shared-memory-workflows). It is
deferred and does not block the existing community-summary phases. When adopted, the
profile must let a report cite the same assertion and source identities used by recall
and personal notes. A generated report retains its provenance and review status, and
does not itself establish the truth or independent corroboration of its inputs.
