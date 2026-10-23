# C22: Community summaries for global questions (GraphRAG)

> **Status:** Phase 1 implemented; its paid acceptance runs wait for approval
>
> **Phases:** Phase 1 is implemented. Phase 1 is the community build from F14's Leiden, the
> summaries written by the server's model providers with citations to facts, their
> storage as triples with provenance and review status, the build task and the settings.
> Phase 2 adds global questions in Ask, community context in `recall` and the brief, and
> the MCP tool. Phase 3 refreshes communities and summaries incrementally after commits.
>
> **User docs:** [Model settings](../USAGE.md#changing-models-and-keys-at-runtime)
> cover configuring the role, and [Community reports](../USAGE.md#community-reports-graphrag)
> cover building and reviewing them.
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
| `budget` | Tokens a full build and a day of refreshes may use. `providerCounting` allows the provider's token counting endpoint and defaults to `true`. `byteBound` sets the byte bound's allowances per provider kind (§3.5). The provider's own budget of C19 applies as well. |

### 2.2 Building

`POST /$/graphrag/{ds}/build` starts a task in `/$/tasks` that runs the steps of §3.
Before it calls a model, it reports the communities, the facts, the model requests and an
upper bound on the tokens that is labelled as an estimate (§3.5), and with
`"confirm": false` (the default from the UI) waits for confirmation, as C18's ingestion
does for its cost estimates. `GET /$/graphrag/{ds}` reports the state:
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
2. **Leiden.** `algo:leiden` with the settings' resolution and a fixed seed, run until
   it converges, which yields a community per node and level. The levels are numbered
   from the coarsest, and the build keeps levels 0 to `levels`. A run that reaches the
   algorithm's level cap without converging is refused, since its coarsest level is not
   known, and a run stopped by its deadline or memory limit fails as a whole. Leiden's
   levels need not nest, so each level below the coarsest is intersected with the level
   above and split into its connected pieces, which makes every community a connected
   part of its parent. Communities smaller than `minCommunitySize` are folded into their
   nearest kept ancestor, whose report input then includes their members.
3. **Stable identities.** A new community inherits the IRI of a previous build's
   community at the same level when their Jaccard similarity is at least 0.5, and each
   IRI is inherited at most once. The build takes every qualifying pair of a level in
   order of decreasing similarity, compared exactly as fractions, then by the old IRI,
   then by a versioned digest of the new community's sorted members, then by those
   members themselves, and accepts a pair when neither side has matched yet. Others get
   a new IRI `urn:x-sparkles:graphrag:{ds}:{level}:{uuid}`. Unchanged members and a
   stable IRI let §5 reuse reports.

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

The new role `community` extends C18 §3.7 and writes the report. It has its own ordered
list of provider and model pairs. When neither the server nor the dataset configures
the role, it inherits the `summarize` list after dataset overrides. An explicit empty
`community` list turns the role off. Report calls use the structured-output levels of
C18 §3.6. The output is a `CommunityReport`:

```json
{ "title": "Payments platform team",
  "summary": "Three to five sentences.",
  "findings": [ { "text": "The team owns the ledger service and its two databases.",
                  "entities": [1, 4],
                  "cites": [3, 7, 12] } ],
  "rating": 6 }
```

The input numbers its entities as it numbers its facts. A leaf community's entities are
its members and the IRI objects of the facts sent. A higher-level community's entities
are those its child findings name and the ends of the edges between children that are
sent. Each finding lists the numbers of the entities it is about in `entities`, and the
server stores them as the entities' IRIs (§4).

`rating` is the model's estimate of the community's importance from 0 to 10, as Edge et
al. ask for, and serves only to order reports within a level. The prompt tells the
model that the facts are data, as C11 §4.10 requires.

### 3.4 Checks

The server checks each report before it stores it, as C18 checks summaries that cite
rows. The checks on citations and entity numbers are exact, and the check on names in
the text is a heuristic.

- Every citation number names a fact that was sent. A finding without a valid citation
  is dropped.
- Every entity number names an entity of the input, and that entity is the subject or
  the object of at least one fact the same finding cites. For a higher-level report, the
  entity must be named by a cited child finding or be an end of a cited edge between
  children. A finding with no entity, or with a number that fails this check, is dropped
  and counted as `unsupported`. Both checks use the numbering that rendered the prompt,
  which is recorded with the report.
- The text passes the `secret-detected` check of C18's transcripts.
- A report whose findings were all dropped is retried once with the escalation of C18
  §5.5, then stored as `failed` with the reason.

After those checks, a heuristic looks for names in each kept finding's text that match
no entity of the input. It takes as candidate names the runs of capitalised words, the
quoted phrases and the words with inner capitals or digits. A single capitalised word
that starts a sentence is not a candidate. It compares each candidate, without regard to
case, with the labels and IRI local names of the input's entities, and a candidate
matches when it equals one of them, is part of one, or contains one. A candidate that
matches none is recorded on the finding as an unmatched name, and the
report's review shows it. The heuristic does not drop the finding. It misses names
written in lower case, pronouns and paraphrases, and it flags capitalised words that are
not names, so a report with no unmatched names is not proof that its text names only
supported entities. The exact checks establish that each finding cites sent facts and is
about entities those facts mention. The text itself is covered by the heuristic and by
review, which is why reports start as `unreviewed` (§4).

### 3.5 Cost

A build makes one model call per community with a report. With the settings' sizes, a
dataset of 100,000 entities at three levels produces some thousands of calls.

The budget is strict and its counts are conservative. Every model request reserves an
upper bound on its tokens before it is sent, so the build never sends a request that
could take it past its budget. The counts are exact only where an exact counter exists
for the model.

**Input bounds.** Before each HTTP attempt, the build bounds the input tokens of the
complete request as the provider receives it. That covers the system text, the
messages, the output schema or tool definition of the request's structured-output
level, and the framing the protocol adds. The bound comes from the first of these
sources that is available for the pair:

1. An exact local tokenizer for the pair's model, which gives the exact count.
2. The provider's own token counting endpoint, such as Anthropic's `count_tokens`. It
   is used only when the same provider receives the request itself, under the same
   principal and the same `sendByProvider` policy as the inference call, so counting
   discloses nothing that the call does not. `budget.providerCounting: false` turns it
   off.
3. The byte bound: the UTF-8 length in bytes of the serialized request times a
   multiplier, plus an allowance per message for the protocol's framing tokens and an
   allowance per request for what the provider adds around a schema or a tool
   definition. A token of a byte-level BPE tokenizer covers at least one byte, so the
   length in bytes bounds the tokens of the text. The multiplier and the allowances are
   settings per provider kind, `budget.byteBound`, whose defaults are a multiplier of 1,
   16 tokens per message and 1024 tokens per request. Nothing has measured these
   defaults against a provider. A provider known to break the one-byte assumption needs
   a larger multiplier.

A missing exact counter is never a reason to refuse a build. Each bound records its
source, the counter and its version where there is one, the model and a digest of the
request, so it describes that request and no other.

**Reservations.** The build reserves the input bound plus the output limit the request
sends to the provider, which the provider enforces. The attempt is sent only when the
reservation fits in what remains of `budget.tokensPerBuild` and of
`budget.tokensPerDay`. Every attempt passes this check. That includes the client's
retries of a `429` or `5xx` answer, the retry with validation errors of C18 §3.6, the
move to another structured-output level, and escalation to another pair. A retry with a
changed request is bounded again. When the provider answers with its usage, the
reservation is replaced by the reported input and output tokens. When the attempt fails
in a way that leaves its usage unknown, such as a timeout or a connection lost after the
request was sent, or when the answer reports no usage, the whole reservation stays
counted. Concurrent builds and refreshes of a dataset share one ledger for
`tokensPerDay`, kept with the dataset's GraphRAG state so that a restart does not reset
it, and its days are UTC days. The provider's own budget of C19 still applies.

**Accounting anomalies.** A reported usage above its reservation means that a bound
was wrong. The ledger then counts the reported usage, the build records an accounting
anomaly with the pair, the counting source and both figures, and it sends no further
request. Its remaining communities stay `pending`, the anomaly shows in the status of
§2.2, and it raises a notice in the review inbox and counts in
`sparkles_graphrag_accounting_anomalies_total{dataset}`, so that an administrator sees
it. A later build reserves against the ledger as it then stands, without an
acknowledgement.

**The estimate.** The input of a higher-level report depends on the reports of its
children, so the input of a whole build cannot be known before its first call. The
confirmation step of §2.2 therefore shows an upper bound labelled as an estimate,
together with `tokensPerBuild`, and says which counting source gave each part of it.
The estimate uses only the local sources, the exact tokenizer and the byte bound, so
nothing is sent before the person confirms. Each leaf request is bounded from its
prompt. Each higher-level request is bounded from its prompt and the edges between
children, with each child report at its output limit. Every request adds its output
limit. The bound assumes one attempt per request, and it is an estimate because a
child's text, counted again inside its parent's prompt, can take a few more tokens than
the child's output did. Retries can use more than the estimate, but never more than
`tokensPerBuild`, since every attempt is reserved first.

A build stops before an attempt whose reservation does not fit, keeps the reports it
wrote, and reports the remaining communities as `pending`.

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
    gr:about ex:payments, ex:ledger ;
    gr:cites <<( ex:payments ex:owns ex:ledger )>>, <urn:uuid:…reifier-of-a-fact> .

<urn:uuid:…a1> a prov:Activity ;
    prov:wasAssociatedWith <urn:x-sparkles:principal:graphrag> ;
    spk:model "anthropic/claude-sonnet-5-5" ; spk:promptVersion "community-1" ;
    spk:inputTokens 9120 ; spk:outputTokens 610 ;
    prov:startedAtTime "…"^^xsd:dateTime .
```

A cited fact is a triple term, and also the fact's reifier when it has one, so the
citation reaches the fact's own sources through C17's provenance. `gr:about` holds the
entities of the finding's `entities` member. A name that the heuristic of §3.4 found
unmatched is a `gr:unmatchedName` literal on the finding. A report for a higher
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

A report summarizes facts as the settings' `principal` read them at the build's commit.
A caller may read a report only when the server can prove that the caller may read
everything the build read, which is the rule of F13 §6.1 applied to the inputs the
build actually used rather than to the principal's current grants. The proof works from
a disclosure footprint that the build records, and a common guard applies it to every
read path. No report is published before both exist. Ask and `recall` use reports only
for callers who may read them, and otherwise answer locally and say so. A dataset whose
readers have different views can define several GraphRAG configurations, each with its
own principal and reports graph, as `graphrag.json` takes a list in Phase 2.

The model sees the facts of the principal's view. C18 §3.5's `sendByProvider` applies:
a provider allowed only the schema cannot serve the `community` role, and the build
refuses to start.

### 6.1 Guarded graphs

Reports graphs live under `urn:x-sparkles:graphrag:`, and `reportsGraph` must name a
graph there. Every graph under that prefix is a guarded graph, whether or not a
configuration names it and whether or not a footprint exists for it, so deleting
`graphrag.json` or restoring triples without their footprint never exposes a report. A
projection may not read a guarded graph, so that reports are not built from reports.

### 6.2 The disclosure footprint

A build captures a footprint of what it read, at its start and again before it
publishes. The footprint holds:

- the format version, the dataset, the branch and the source commit;
- the principal's name, and the caller attributes its triple protections can bind:
  `?user`, each `?role` and each `?group`;
- the endpoint whose grants resolved the principal's view;
- the concrete graphs the projection and the report inputs read at the source commit,
  including the inferred graph when it was read;
- for each of those graphs, the protections in force for the principal there, each with
  its full definition: predicates, classes, `subclasses`, graphs, pattern, prefixes and
  `hide_inferences`;
- the identities of the reports and child reports it covers.

The footprint describes the whole principal-filtered projection, because the
membership, counts, labels, fact selection and child reports of a report all depend on
it, and not only the facts one prompt cited. A footprint whose protections cannot be
written in this form, such as one that would need a graph named by a blank node, makes
the build refuse to start.

Footprints are stored in the dataset, in the guarded graph
`urn:x-sparkles:graphrag:disclosure`. A footprint is a resource
`urn:x-sparkles:graphrag:footprint:{sha256}`, named by the SHA-256 of its canonical
JSON, with `gr:footprint` holding that JSON as a literal and `gr:covers` naming the
reports graph it covers. Every report links to the footprint of the build that wrote it
with `gr:disclosure`. No footprint proves the disclosure graph itself, so only
unrestricted readers see it, because protection definitions are not shown to callers.
A footprint whose version is unknown, whose JSON does not parse or fails its checks, or
whose name does not match its digest counts as invalid.

**Installation.** The commit that adds a build's first report triple to a reports graph
also adds the build's footprint, so no snapshot holds a report without the footprint
that covers it. Before that commit the build compares the principal's resolved view,
the settings and the policy with the footprint captured at its start. Any difference
stops the build before it publishes, and a new build captures a new footprint.

**Retention.** A footprint stays in the snapshot while any report in its graph links to
it. The commit that removes the last such report removes the footprint too, so a
footprint lives exactly as long as the content it covers. Older commits, branches and
backups keep the footprints of the reports they hold, because the footprints are part of
the same data. Removing a footprint that a report still links to makes the graph
unproven, so only an unrestricted writer can weaken the guard, and only by removing the
reports as well.

### 6.3 Coverage

The guard answers `proven`, `denied` or `unproven` for a reader and a guarded graph, and
only `proven` admits the graph. A guarded graph with no valid footprint is proven only
for an unrestricted reader. A graph with several footprints is proven only when every
one is. These rules are admitted initially:

1. **Unrestricted reader.** A reader whose view, before the guard, reads every graph with
   no protection in force may read everything the build read. Dataset administrators,
   server administrators and callers of a server without authentication are of this
   kind.
2. **Identical resolved view.** A reader whose resolved view, at its endpoint, equals the
   footprint's: the same graphs, the same protections in force on each with identical
   definitions, and the same values of every caller variable a pattern of those
   protections uses.
3. **Structural inclusion.** A reader whose read graphs include every graph of the
   footprint, and for whom, on each of those graphs, every protection in force is also in
   force in the footprint with an identical definition, and every caller variable its
   pattern uses has the footprint's values. Every quad the principal saw is then visible
   to the reader under the same snapshot semantics. Each protection that applies to the
   reader there either was also applied to the principal and its pattern gives the same
   answer for both, or does not apply to the reader at all.

Every other case is `unproven` and fails closed. This includes a reader whose grants
name the same roles but bind different users or groups, coverage that would have to be
inferred from the graphs present at some commit, and coverage that depends on class
membership or pattern solutions changing after the build. The rules compare definitions
and caller attributes, never data, so a decision does not depend on the snapshot read.
Rule 1 holds at any commit because such a reader may read the build's commit itself.
Rules 2 and 3 hold because the protections the reader shares with the footprint give
the same answers as they gave the principal at the build's commit.

A decision is made against the footprints in the snapshot the request reads, never
against a view decided earlier. A request resolves its view from the policy when it is
authenticated, and the guarded graphs of that view stay undecided until the request
binds the view to its snapshot. A path that reads a view it has not bound sees no
guarded graph. A guarded graph is proven for a snapshot only when the snapshot holds at
least one footprint that covers it, every such footprint is valid and proven, and every
footprint that a report in the graph links to is among them. A policy reload applies to
the next request, and a long-lived read such as the change feed stream ends when the
policy changes.

*Design refinement, 2026-10-23.* The first form of this section decided coverage when
the request resolved its view, from a registry file of footprints. A request takes its
snapshot after that, so a build could install a footprint and commit a report between
the two. The request then read the new report under a decision that had not seen its
footprint. Three repairs were weighed. A recheck of the decision in every read path fails
open the first time a path is added without it. A grace period, in which the build waits
for older requests to finish before it commits, has no bound while streaming responses
are open. Footprints stored in the snapshot itself make every point-in-time read, branch,
backup and restore consistent by construction, and an unbound view fails closed, so this
form was chosen.

### 6.4 The guard

The guard is applied where a request's view is resolved, in the server's principal
view, before any path reads data. A guarded graph that is not proven for the reader is
removed from the reader's read graphs, in the way C12 hides a graph, and every path
treats the graph as one that does not exist, through the machinery C12 and C12b already
use for a restricted view. Only a reader whose view is already restricted can lose a
graph this way, since a reader without a view is unrestricted and proven by rule 1. A
route-local check is not enough, and the GraphRAG routes use the same decision as every
other path.

Report-derived output outside the reports graph follows the same decision.
`GET /$/graphrag/{ds}`, the build task in `/$/tasks`, the review inbox and the
Communities tab show titles, members, counts, statuses, citations and token figures of a
reports graph only to callers for whom that graph is proven. Commits of kind `graphrag`
carry a message without report content, since commit messages reach restricted callers.
The metrics of §8 carry counts per dataset and status, never text, under the existing
permission of the metrics endpoint.

### 6.5 History, branches and restores

Footprints are data in the dataset, so a point-in-time read, a snapshot or a read on a
branch decides against the footprints of the commit it reads. A branch created from a
commit with reports holds their footprints with nothing to copy, and a build on a branch
records its branch and commit in its footprint. A backup, a restore, a clone and a
replica carry the footprints with the reports, and one that dropped the disclosure graph
leaves its reports readable only by unrestricted readers.

The change feed, `/{ds}/diff`, `/{ds}/history` and the timeline show changes between
commits, which no single snapshot's footprints cover. These paths never bind the guard,
so a restricted reader sees no change to a guarded graph there, and reads reports at a
commit through SPARQL or the Graph Store instead. Offline access to the database files
stays under the operator's authority, as for every other graph.

### 6.6 Read paths

The guard must cover every path that can return report-derived data:

- SPARQL `SELECT`, `ASK`, `CONSTRUCT` and `DESCRIBE`, stored queries, explain, the
  `WHERE` of updates and transactions, and SPARQL diffs;
- the Graph Store Protocol's `GET` and `HEAD` of a graph, of the default graph and of
  the whole dataset, which is also the export path of restricted callers;
- GraphQL queries and the GraphQL schema draft;
- full-text search, vector search and `spk:hybridSearch`, and the search and similarity
  tools of MCP;
- the schema routes, shapes and constraints, the dataset descriptions and listings with
  their counts, the service description's graph list, `/$/stats/{ds}` and every route
  that refuses restricted callers today;
- the change feed and its stream, `/{ds}/diff`, `/{ds}/history`, the commit listings and
  the timeline;
- point-in-time reads through `?at=` and Memento, snapshots, and reads on every branch;
- the MCP data, memory, schema and history tools and the MCP resources, the REST forms
  of the memory tools, Ask and `recall`;
- graph algorithm runs, geographic feature reads, and `/$/tasks`;
- the GraphRAG status, task, inbox and Communities responses of §6.4.

Ask keeps a history of answers for the person who asked. An answer built from reports
in Phase 2 keeps the report identities it used, and a history entry shows its text only
while those reports' graph is proven for its reader.

Before reports are published, local tests must show for direct and indirect paths that
an unproven reader gets no report triple and no metadata, together with a positive case
for each admitted rule. They cover the principal losing a graph after a build, callers
with the same roles and different `?user` or `?group` bindings, a hidden input graph,
a lifted protection, a policy change during a build, a branch, an old snapshot, a
missing or unknown footprint in the snapshot, a report committed while a request's view
was already resolved, and the schema and count paths.

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
  `sparkles_graphrag_tokens_total{dataset,phase}`,
  `sparkles_graphrag_stale_communities{dataset}` and
  `sparkles_graphrag_accounting_anomalies_total{dataset}`.

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
- **A8.** A build whose estimate passes `tokensPerBuild` stops before the first attempt
  whose reservation does not fit, sends no further request, and lists the remaining
  communities as `pending`.
- **A9.** `recall` with `communities: true` for `ex:ana` adds the report of her team's
  community after her facts.
- **A10.** A dataset whose `sendByProvider` limits the `community` role's provider to the
  schema refuses to build, naming the setting.
- **A11.** Three pairs, one with an exact local tokenizer, one whose provider offers a
  counting endpoint and one with neither, each reserve before every attempt, from the
  tokenizer, the endpoint and the byte bound in turn. With `providerCounting: false`,
  the second pair uses the byte bound and its counting endpoint receives no request.
- **A12.** A provider that answers `503` and then `200` costs two reservations. The first
  stays counted at its full size, because the failed attempt reported no usage, and the
  second is replaced by the reported usage. A provider that reports more usage than its
  reservation causes an accounting anomaly, and the build sends no further request.
- **A13.** A finding that cites a valid fact but lists entity 9 when the input has eight
  entities is dropped and counted as `unsupported`.
- **A14.** A kept finding whose text names "Globex" when no input entity has that label
  carries the unmatched name `Globex`, and the review shows it.
- **A15.** After a build whose principal reads graphs A and B, the principal loses B. A
  reader whose grants cover A only still cannot read the reports graph, at the head or
  at the build's commit.
- **A16.** Two callers hold the principal's grants, and a protection in force for all
  three has a pattern that binds `?group`. The caller whose groups equal the principal's
  reads the reports, and the other caller's
  queries, Graph Store reads, schema routes and counts behave as if the reports graph
  did not exist.

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

## 14. Revisions

**2026-10-23, token counts and entity checks.** The user decided two questions that the
first draft left open, and §2.2, §3.3, §3.4, §3.5, §4 and §11 changed to match.

The first draft promised an exact count of a build's input tokens before the build. A
higher-level report's input contains its children's reports, which do not exist until
the build writes them, so no exact total can be known before the first call. The draft
also did not say how a budget holds when requests are retried. The decision makes the
budget strict and the counts conservative. Every HTTP attempt reserves an upper bound on
its input plus its output limit before it is sent, actual usage settles the
reservation, and unknown usage stays reserved. The bound is exact where a local
tokenizer exists, comes from the provider's counting endpoint where that provider
receives the request anyway and the setting allows it, and is otherwise the byte length
of the request with fixed framing allowances. A missing exact counter is never a reason
to refuse. A usage above its reservation is an accounting anomaly that stops the build.
The confirmation shows a whole-build upper bound labelled as an estimate and with its
counting sources. The user first chose exact local counts with a refusal for models
without a counter, and replaced that choice with this contract on the same day.

The first draft required that a finding naming an entity name a member or a fact's
object. The schema gave findings prose only, and no check can enumerate every name in
free text, so the requirement could not be enforced as written. The decision splits it.
Each finding carries entity numbers and citations, and both are checked exactly against
the facts sent, with the drop and retry policy of §3.4. A heuristic over the prose and
the review status cover the free text, and the spec describes the heuristic as one and
not as proof.

**2026-10-23, footprints in the snapshot.** The orchestrator replaced the registry
file of §6.2 with footprints stored in the dataset, after a review found that a request
could read a report committed after its view was decided. §6.2, §6.3 and §6.5 changed,
and §6.3 records the race and the reason for the choice.

**2026-10-23, the disclosure guard.** §6 gained the design of the disclosure footprint
and the common read guard that reports need before they become visible. The first draft
compared readers with the build principal's current grants through a C12 graph rule,
which exposes old reports when the principal later loses access to an input, and which
cannot tell apart callers whose triple protections bind different `?user`, `?role` or
`?group` values.

## Outcome

The `community` model role is implemented in the server configuration, dataset role
overrides, CLI role guidance and the model settings UI. It preserves provider/model
order and inherits the resolved `summarize` list when neither layer explicitly
configures `community`. An explicit empty list disables the role. Configuration,
server/dataset precedence and UI guidance have focused regression tests.

Phase 1 is implemented, and its acceptance runs against paid providers wait for the
user's approval. A build reads the projection as the settings' principal, which must be
allowed to query the dataset on the run's branch. It computes the Leiden hierarchy with
stable identities, writes one report per community through the `community` role with
the checks of §3.4, and stores communities, reports, findings, citations and their
provenance as triples in commits of kind `graphrag`. A cited fact carries its reifier
when it has one. Every commit adds the run's footprint, and every community and report
links it, so the guard of §6 decides each snapshot from its own data. A report is
removed once no community links it and no linked report cites it, so a kept parent's
citations of a replaced child stay resolvable.

`POST /$/graphrag/{ds}/build` and `/refresh` run as tasks of kind `graphrag`. The task
reports the estimate and waits for `POST /$/graphrag/{ds}/confirm` unless the caller
confirmed beforehand. The run checks its settings, its providers, their send levels and
the principal's view again after the confirmation, before each model request and before
each commit, and it fails without committing when a review changed a report link while
it ran. All branches of a dataset share its daily ledger. An accounting anomaly stops
the run like the budget does, the remaining communities stay pending, and a later
refresh writes them. `GET /$/graphrag/{ds}`, `/communities` and `/community` read
through the caller's view and say only that the reports are unavailable to a caller for
whom the graph is not proven. `POST /$/graphrag/{ds}/review` sets a status or saves an
edit as a reviewed revision. The review inbox lists unreviewed reports and the
anomalies, the metrics of §8 count reports, tokens, pending communities and anomalies,
and `sparkles graphrag` builds, refreshes and reads reports on a server or with `--loc`.
The UI's dataset page has a Communities tab with the tree, a community's members and
report, the estimate to confirm and the review actions, and its Settings tab has the
`graphrag` fields. Tests with stub providers cover A1 to A4, A8 and A10 to A16.

These choices differ from or refine the text above. Manual refresh compares the fact
digests each report was written from with the facts it reads now, rather than reading
the change feed, which Phase 3 adds. The baseline of a kept report stays until the
report is rewritten, so small changes add up. A folded member's facts go into its
parent's input and its parent's change measure. Only an unrestricted writer can review
a report, since §6.2 lets only such a writer write a guarded graph. The metrics hold the
figures of the runs and reviews since the server started. The byte bound's default
allowances are unmeasured, and measuring them needs paid provider runs. Global Ask and
memory integration remain Phase 2, and automatic refresh remains Phase 3.
[ROADMAP](../ROADMAP.md) item 12 tracks this work.

The contracts that the first draft left open are settled in §14. Token budgets use
conservative reservations before every HTTP attempt, and findings carry entity numbers
that are checked exactly, with a labelled heuristic for the prose. §6 now holds the
design of the disclosure footprint and the common read guard. Reports stay unpublished
until that guard has passed review.

### Shared memory representation follow-up

[Roadmap](../ROADMAP.md) item 81 plans a shared profile for agent memory and personal
knowledge in [C17](C17-agent-memory.md#shared-memory-representation) and
[C18](C18-natural-language-questions-and-ingest.md#shared-memory-workflows). It is
deferred and does not block the existing community-summary phases. When adopted, the
profile must let a report cite the same assertion and source identities used by recall
and personal notes. A generated report retains its provenance and review status, and
does not itself establish the truth or independent corroboration of its inputs.
