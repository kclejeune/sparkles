# C17: Agent memory over MCP

> **Status:** implemented in part (Phase 1a)
>
> **Phases:** Phase 1a shipped on 2026-10-09. It adds four read tools to the MCP server,
> `check_query`, `similar_queries`, `link_entities` and `recall`, and the `questions`
> field of stored queries. Phase 1b adds the `assert_facts` write tool with its data
> model of graphs, reifiers and supersession. Phase 1c adds a `branch` argument to the
> MCP tools and tools that create, merge and delete branches. Phase 2, model calls
> inside the server, is described in §10 and deferred.
>
> **User docs:** [API: MCP tools](../API.md#tools) ·
> [API: Stored queries](../API.md#stored-queries) ·
> [Usage: MCP server](../USAGE.md#mcp-server-llm-agents) ·
> [Features](../FEATURES.md)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it lands.

This design was written from the Model Context Protocol specification, the W3C RDF 1.2
Concepts, Turtle 1.2 and SPARQL 1.2 drafts, SHACL, PROV-O, SKOS, published work on
retrieval-augmented generation over knowledge graphs, on entity linking, on retrieving
examples for in-context learning and on long-term memory for language-model agents, and
the Sparkles code. The sources are listed in §15. It builds on the MCP server
([C11](C11-mcp-server.md)), schema discovery ([C02](C02-schema-discovery.md)), stored
queries ([C16](C16-stored-queries.md)), write previews ([C15](C15-write-previews.md)),
write-time validation ([C10](C10-write-time-validation.md)), full-text search
([F03](F03-full-text-search.md)), vector search ([F04](F04-vector-search.md)), embeddings
on write ([F08](F08-embeddings-on-write.md)), history ([F06](F06-snapshots-and-point-in-time.md)),
branches ([F09](F09-branches-and-merges.md)) and access control
([C09](C09-dataset-access-control.md), [C12](C12-graph-access-control.md),
[C12b](C12b-triple-access-control.md)).

## 1. Summary, goals, non-goals

Language-model agents such as Claude Code and Claude Desktop already reach Sparkles
through MCP. They can read the schema, run SPARQL, search text and vectors, and, when an
operator allows it, write with `sparql_update`. That is enough to use a dataset, but not
enough to keep knowledge in one. Agents that use a graph as long-term memory fail in a
few predictable ways.

- **Wrong terms.** A model writes `foaf:name` where the data uses `schema:name`, or
  `"Ana Lima"` where every label is `"Ana Lima"@en`. The query returns nothing, and
  nothing says why.
- **Duplicates.** A model that learns about "the payments team" mints a new IRI for it,
  although the graph already holds `ex:payments`. Duplicate entities are among the most
  common defects of graphs that language models build, and they compound. Facts split
  across copies, and later recall finds only part of them.
- **Invented IRIs.** A model writes `ex:paymnts` as the object of a fact. The triple is
  well formed, so nothing refuses it, and it points at nothing.
- **Lost provenance.** A fact written with `INSERT DATA` carries no source, time or
  author beyond the commit. When it turns out to be wrong, nobody can find the other
  facts that came from the same place.
- **Destructive corrections.** A model that learns a new value deletes the old one. The
  graph then cannot answer what was believed before, or why it changed.
- **Context overflow.** A model that wants "everything about Ana" runs queries that
  return thousands of rows, or many small queries that each cost a round trip.

This spec adds MCP tools that address each failure. The server holds no language model.
The calling agent does the language work. It reads the user's request, decides what to
look up and what to write, and phrases the answer. The server supplies grounding and
safe writes. It checks queries against the schema, finds stored queries that answer
similar questions, links mentions to existing entities, assembles compact context with
citations, and writes facts with provenance through the dataset's validation and
preview machinery.

**Goals**

1. `check_query` reports the predicates and classes of a query that the dataset does not
   contain, with the nearest terms it does contain, and literals whose datatype or
   language tag cannot match the data.
2. `similar_queries` returns the stored queries closest to a question, so an agent can run
   one or use it as an example.
3. `link_entities` returns candidate IRIs for mentions in text, ranked by exact label,
   BM25 and vector similarity, with their types and a verdict.
4. `recall` returns the facts around a question or a set of entities as compact text,
   with a citation for each fact.
5. `assert_facts` writes facts into a named graph per source or session, records their
   provenance on RDF 1.2 reifiers, refuses likely duplicates and unknown terms, passes
   the dataset's guard, previews on request, and supersedes old values instead of
   deleting their record.
6. Branches serve as scratchpads for speculative writes that the agent or a person later
   merges or discards.
7. Every tool runs as the caller, over the caller's view of the data, under the MCP
   budgets.
8. Phase 1 adds no runtime dependency. Its only outbound requests go to the embedding
   endpoint that a dataset's F08 index already configures.

**Non-goals.** Phase 1 puts no language model, prompt templates or provider
configuration inside the server. It does not ingest documents, which means reading files,
converting them to text and chunking them. It does not learn ontologies, and it does not
merge duplicate entities on its own. Valid time, meaning when a fact was true in the
world rather than when it was recorded, belongs in the domain vocabulary. Memory is not
kept in a separate store next to the graph.

## 2. The agent's loop

The tools fit a loop that the server's prompt (§5.8) describes. Each step is optional,
and an agent can call any tool on its own.

To answer a question, the agent first calls `recall` with the question's text. It gets
the facts around the best-matching entities, each with a citation. When that is not
enough, it calls `similar_queries` to find a reviewed query that answers a similar
question, and runs that query's tool. When no stored query fits, it drafts SPARQL from
`describe_schema`, calls `check_query` to catch unknown terms before running it, and
then calls `sparql_query`. It answers with the citations or the query it ran.

To remember something, the agent calls `link_entities` with the mentions in what it
learned, and keeps the IRIs of the matches. For each mention without a match it declares
a new entity. It then calls `assert_facts` with `dryRun: true`, reads the preview, and
calls it again with `ifHead` set to the preview's head. For a write it is unsure of, it
first creates a scratch branch, writes there, and merges the branch later.

## 3. Data model

Memory is ordinary RDF in the dataset. Facts are triples, their provenance is RDF 1.2
annotations on reifiers, and their grouping is named graphs. A person can read and
correct the memory with SPARQL, the Graph Store Protocol and the UI, and every other
feature of Sparkles applies to it. No reserved graph or private structure holds memory.

### 3.1 Graphs per source or session

`assert_facts` writes into one named graph per call. The caller chooses the graph's IRI,
and two conventions cover most uses.

- **A graph per source.** Facts taken from a document, a meeting or a web page go into a
  graph named by the source's IRI. When the source is corrected or withdrawn, a Graph
  Store `PUT` or `DELETE` of that graph replaces or removes exactly its facts.
- **A graph per session.** Facts that an agent learned in conversation go into a graph
  for the session, such as `https://example.org/memory/sessions/2026-10-08-a`.

The grouping matters for three reasons. Graph grants of [C12](C12-graph-access-control.md)
take IRI patterns with `*`, so an operator can give an agent's token `write` on
`https://example.org/memory/*` and nothing else. A graph is the unit that Graph Store
writes replace and that the change feed and history queries filter by. A person who
reviews an agent's work can diff or drop one graph without touching curated data.

When `source` is given and `graph` is not, the graph is the source's IRI. A call with
neither is refused, so no fact lands in the default graph by accident. Queries that should
see memory and curated data together use the union of the graphs, as `describe_schema`
and `sparql_query` already allow.

### 3.2 Reifiers and provenance

Each fact that `assert_facts` writes is asserted in its graph and reified there. The
reifier carries the fact's provenance in the PROV-O vocabulary, plus three properties
of the `spk:` namespace (`urn:x-sparkles:`) that PROV-O lacks. In Turtle 1.2, one call's
output looks like this.

```turtle
PREFIX ex:   <http://example.org/>
PREFIX org:  <http://www.w3.org/ns/org#>
PREFIX prov: <http://www.w3.org/ns/prov#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX spk:  <urn:x-sparkles:>
PREFIX xsd:  <http://www.w3.org/2001/XMLSchema#>

GRAPH <https://example.org/notes/2026-10-08> {
  ex:ana org:memberOf <urn:uuid:0192f1c4-5b7e-7c3a-9d2e-4f6a8b1c3d5e>
      ~ <urn:uuid:0192f1c4-5b7e-7c3a-9d2e-aa00000000f1> {|
        prov:wasGeneratedBy <urn:uuid:0192f1c4-5b7e-7c3a-9d2e-aa0000000001> ;
        prov:wasDerivedFrom <https://example.org/notes/2026-10-08> ;
        prov:generatedAtTime "2026-10-08T09:14:03Z"^^xsd:dateTime ;
        spk:confidence 0.9 ;
        spk:quote "Ana moved to the payments team this week." |} .

  <urn:uuid:0192f1c4-5b7e-7c3a-9d2e-aa0000000001> a prov:Activity ;
      prov:wasAssociatedWith <urn:x-sparkles:principal:agent-7> ;
      prov:startedAtTime "2026-10-08T09:14:03Z"^^xsd:dateTime ;
      rdfs:label "Stand-up notes of 2026-10-08" ;
      spk:idempotencyKey "standup-2026-10-08" .
}
```

| Property | On | Meaning |
|---|---|---|
| `rdf:reifies` | reifier | The fact, as a triple term. The Turtle annotation syntax writes it. |
| `prov:wasGeneratedBy` | reifier | The activity of the `assert_facts` call that wrote the fact. |
| `prov:wasDerivedFrom` | reifier | The source, when the call names one. |
| `prov:generatedAtTime` | reifier | The time the call started. The commit records its own time as well. |
| `spk:confidence` | reifier | The agent's confidence as an `xsd:decimal` from 0 to 1, when it gives one. It is stored as given, and no tool ranks, filters or thresholds on it. |
| `spk:quote` | reifier | The passage of the source that supports the fact, at most 1000 characters. |
| `prov:wasAssociatedWith` | activity | The principal that made the call, as `urn:x-sparkles:principal:<name>`. Over stdio it is the operating-system user, as for stored-query versions that the CLI saves. |
| `prov:actedOnBehalfOf` | software agent | Set when the call describes the agent (§5.6). The principal is authenticated, while the agent's own name and model are what the agent reports. |
| `rdfs:label` | activity | The call's commit message, when it gives one. |
| `spk:idempotencyKey` | activity | The key of the call, when it gives one (§5.6). |

The reifier, the activity and the fact share the graph. Removing a source's graph
therefore removes its facts with their provenance, and a caller who may read the graph
reads both. A reifier minted by `assert_facts` is a `urn:uuid:` IRI, never a blank node,
so later calls can name it to supersede or retract the fact.

Facts that came into the dataset some other way have no reifier. `recall` cites them by
their graph alone, and `assert_facts` creates a reifier for such a fact when it
supersedes it (§3.3).

### 3.3 Supersession and retraction

A new value does not delete the record of the old one. When `assert_facts` supersedes a
fact, it removes the asserted triple from its graph and keeps the fact's reifier, which
still reifies the triple term. RDF 1.2 allows a reifier of a triple that is not
asserted, so the graph then states two things. The current value is asserted. The old
value is described, with when it was recorded, from where, and what replaced it.

```turtle
GRAPH <https://example.org/notes/2026-10-01> {
  # ex:ana org:memberOf ex:platform is no longer asserted.
  <urn:uuid:…-r1> rdf:reifies <<( ex:ana org:memberOf ex:platform )>> ;
      prov:wasGeneratedBy <urn:uuid:…-a0> ;
      prov:generatedAtTime "2026-10-01T10:02:11Z"^^xsd:dateTime ;
      prov:wasInvalidatedBy <urn:uuid:…-a1> ;
      prov:invalidatedAtTime "2026-10-08T09:14:03Z"^^xsd:dateTime .
}
GRAPH <https://example.org/notes/2026-10-08> {
  ex:ana org:memberOf <urn:uuid:…-pay> ~ <urn:uuid:…-r2> {|
      prov:wasGeneratedBy <urn:uuid:…-a1> ;
      prov:wasRevisionOf <urn:uuid:…-r1> |} .
}
```

The invalidation stays in the old fact's graph, so the old reifier keeps its whole
record in one place. A retraction is a supersession without a new value. It removes the
asserted triple and adds `prov:wasInvalidatedBy` and `prov:invalidatedAtTime` to the
reifier.

This design keeps the current state clean. Plain SPARQL, SHACL validation, schema
reports and inference see only asserted triples, so a `sh:maxCount 1` constraint holds
after a correction. At the same time, a query over reifiers answers what was believed
and when. The commit history of [F06](F06-snapshots-and-point-in-time.md) records the
same change, but history has a retention window and answers by commit, not by fact.

Supersession is not erasure. A request to forget personal data needs a real deletion of
the triples and the reifiers, through `sparql_update` or the Graph Store, followed by the
history retention settings of F06. Erasure is outside this spec. It needs a design of its
own that covers reifiers, history retention and backups.

### 3.4 New entities and their IRIs

An `assert_facts` call names new entities with blank node labels such as `_:pay`, which
are local to the call, as labels are local to a Turtle document. Each label must be
declared in the call's `entities` with a label and at least one type, so that the
duplicate check of §5.6 has something to compare. The server replaces each label with a
fresh IRI before it writes, which is the skolemization of RDF 1.1 Concepts §3.5. The
result maps each label to its IRI.

An IRI is `urn:uuid:` followed by a UUID. With an idempotency key, the UUID is a version 5
UUID computed from the dataset's id, the key and the label, so a dry run and the real call mint the
same IRIs, and a retried call mints them again. Without a key, it is a version 7 UUID.
An `iriBase` argument makes the IRI `<iriBase><uuid>` instead, for datasets whose IRIs
must stay under a namespace.

### 3.5 Shapes for memory

Memory written by `assert_facts` conforms to the following shapes, which an operator can
install as the dataset's guard ([C10](C10-write-time-validation.md)) together with the
domain shapes. They target only the subjects of `prov:wasGeneratedBy`, so they cost
little in a dataset with few reifiers. A dataset that uses PROV-O for other data can
narrow the target with `sh:targetWhere`.

```turtle
PREFIX rdf:  <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX sh:   <http://www.w3.org/ns/shacl#>
PREFIX prov: <http://www.w3.org/ns/prov#>
PREFIX spk:  <urn:x-sparkles:>
PREFIX xsd:  <http://www.w3.org/2001/XMLSchema#>

spk:MemoryReifierShape a sh:NodeShape ;
  sh:targetSubjectsOf prov:wasGeneratedBy ;
  sh:property [ sh:path rdf:reifies ; sh:minCount 1 ; sh:maxCount 1 ] ;
  sh:property [ sh:path prov:wasGeneratedBy ; sh:minCount 1 ; sh:maxCount 1 ;
                sh:class prov:Activity ] ;
  sh:property [ sh:path spk:confidence ; sh:maxCount 1 ; sh:datatype xsd:decimal ;
                sh:minInclusive 0 ; sh:maxInclusive 1 ] ;
  sh:property [ sh:path prov:invalidatedAtTime ; sh:maxCount 1 ;
                sh:datatype xsd:dateTime ] .
```

The guard validates the state after the write in every graph, so a domain shape such as
"every `org:OrganizationalUnit` has an `org:unitOf`" applies to entities that an agent
creates, exactly as to curated ones. That is the point of routing agent writes through
the guard.

## 4. Where the tools come from

Every Phase 1 tool composes parts that exist. The table names them.

| Tool | Built from |
|---|---|
| `check_query` | The parser, the `unknown-term` warning of `explain_query`, the cached schema report of C02 with its subject classes, profiles and constraints layer, and the dataset prefixes. |
| `similar_queries` | The stored-query catalog of C16 and, when the dataset has an embedding index, the embeddings client of F08. |
| `link_entities` | The label predicates of C11 §4.4, index lookups of the store, `text:query` of F03, `spk:vectorSearch` with a text query of F08, and the fusion of `spk:hybridSearch`. |
| `recall` | `spk:hybridSearch`, the round-robin sampling of `describe_resource`, the compact term syntax of C11 §4.3 and SPARQL 1.2 reifier patterns. |
| `assert_facts` | The update path with `WriteOptions`, the guard of C10, the preview of C15, the `ifHead` precondition of C11 and the commit messages of CI. |
| Branch tools | The branch API of F09, which the HTTP routes already use. |

## 5. Tools

### 5.1 Conventions

The tools follow the conventions of [C11 §3](C11-mcp-server.md#3-tools). They take
`dataset`, `at` or `atCommit`, `reasoning` and `timeoutSeconds` with the same meaning,
read IRIs in the same three forms, and render terms in the compact syntax. Errors are tool
results with `isError: true` and a code. Every successful result names the commit it
read, so an agent can pass it as `atCommit` to the next call and read one consistent
state.

Phase 1c adds `branch` to every tool that reads or writes a dataset (§5.7). Before then,
the tools read and write `main`.

The four read tools are listed whenever the server offers MCP, with
`{"readOnlyHint": true, "openWorldHint": false}`. `assert_facts` and the branch tools
that write are listed under the same conditions as `sparql_update`. They need
`--mcp-allow-update` over HTTP or `--allow-update` over stdio, they are never listed on a
read-only server, and they are listed only for a caller who may write to some dataset.
An operator who wants structured memory writes without free-form updates passes
`--mcp-disable-tool sparql_update`. No new flag is needed.

`similar_queries` and the vector parts of `link_entities` and `recall` may call the
dataset's embedding endpoint to embed the query text, as `spk:vectorSearch` with a text
query already does. That call goes through the outbound policy of F08 and its cache, and
it is the only network access of these tools.

### 5.2 `check_query`

The title is "Check a SPARQL query against the schema". It parses a query and compares
its terms with what the caller's view of the dataset contains, without running it.

| Argument | Meaning |
|---|---|
| `query` | Required. A SPARQL query of at most 65,536 characters. The dataset prefixes are predeclared, as in `sparql_query`. |
| `explain` | When true, the result adds the plan's estimated rows and the `no-limit` and `large-estimate` warnings of `explain_query`. The default is false. |
| `maxSuggestions` | Suggestions per issue, 3 by default and at most 10. |

The checks, each with its issue code and severity, are these.

| Code | Severity | When |
|---|---|---|
| `syntax` | error | The query does not parse. The message gives the line and column. An undefined prefix suggests the dataset prefix of the same name, or the prefixes whose namespace ends with that name. |
| `unknown-predicate` | error | A constant IRI in a predicate position, including each step of a property path, has no triples in the view and is not declared as a property in the declared graphs the caller may read. |
| `unknown-class` | error | A constant object of `rdf:type` has no instances in the view and is not declared as a class. |
| `unknown-term` | warning | Another constant IRI of a triple pattern does not occur in the view. This is the existing warning of `explain_query`. |
| `class-mismatch` | warning | A variable typed with class C in the same group is the subject of predicate p, and the subject classes of p include no triple with a subject of class C. The suggestions are the predicates that C's profile lists. |
| `datatype-mismatch` | warning | A literal constant is matched or compared with the objects of p, and none of p's objects has that literal's datatype. |
| `language-tag` | warning | A simple literal is matched with the objects of p, and every literal object of p has a language tag. The suggestion is the literal with the most common tag. |
| `unbound-projection` | warning | A projected variable occurs nowhere in the pattern. |

`ok: true` means that no issue has the severity `error`. Warnings never block.

**Suggestions.** The candidates for an unknown predicate or class are the predicates or
classes of the caller's schema report. Each candidate that qualifies gets one of three
reasons, which rank in the order of the table.

| `why` | When |
|---|---|
| `same-local-name` | The candidate's local name equals the unknown term's, case-insensitively, in another namespace. This catches `foaf:name` written for `schema:name`, a common mistake. |
| `edit-distance` | The Damerau–Levenshtein distance of the local names, after splitting camel case and folding case, is at most a third of the longer name's length. |
| `label` | The candidate's `rdfs:label` or `skos:prefLabel` matches the words of the unknown term's local name. |

Ties break by the number of triples or instances, most first. Each suggestion gives the
term, its label, its count and its reason.

**Result.**

```ts
type CheckQueryResult = {
  dataset: string; commit: number; ok: boolean;
  issues: { code: string; severity: "error" | "warning"; message: string;
            term?: string; line?: number; column?: number;
            suggestions?: { term: string; label?: string; count: number; why: string }[] }[];
  estimatedRows?: number;
  prefixes: Record<string, string>;
};
```

**Cost.** The dataset keeps its last schema report, and C02 Phase 3 keeps the report up
to date from the changes, so a check usually reads a cached report and costs a parse and
a few lookups. When no report is cached, the check computes one under the call's
deadline. A view restricted by C12 or C12b gets a report of its own, as `describe_schema`
does.

### 5.3 `similar_queries`

The title is "Find stored queries for a question". It ranks the stored queries that the
caller may run by their similarity to a question in natural language.

| Argument | Meaning |
|---|---|
| `question` | Required, at most 2000 characters. |
| `k` | The number of queries returned, 5 by default and at most 20. |
| `withText` | Whether each result includes the query text. The default is true. |
| `embeddingIndex` | The vector index whose embedding endpoint embeds the texts. The default is the dataset's only index with an `embedding` configuration. With none, ranking is by BM25 alone. |

The candidates are the stored queries the caller may run whose `mcp` is not `false`.
Each becomes one document made of its description, its parameter names and
descriptions, the local names of the IRIs in its query, split at camel case, and its
example questions.

Example questions are a new optional field of a stored query's definition, `questions`,
with at most 20 strings of at most 500 characters each. They are the questions the query
is known to answer. A person adds them when saving a query, and the UI's save dialog can
offer the question that led to it. The field is part of the definition, so it is
versioned and checked like the rest of [C16](C16-stored-queries.md).

The ranking fuses two lists by reciprocal rank fusion with k = 60, as
`spk:hybridSearch` does. The first list is BM25 over the documents. The catalog is small
and held in memory, so the index is built in memory per catalog version and kept until
the next change. The second list is the cosine similarity of embeddings, when an
embedding index is available. Each document's embedding is computed once per version
and kept with the catalog's cache, keyed by the version's digest.

**Result.**

```ts
type SimilarQueriesResult = {
  dataset: string;
  queries: { name: string; tool?: string; description?: string; score: number;
             matchedBy: ("text" | "vector")[];
             parameters: { name: string; type: string; required: boolean; description?: string }[];
             questions?: string[]; query?: string }[];
  ranking: "hybrid" | "text";
  prefixes: Record<string, string>;
};
```

`tool` is the stored query's MCP tool name, so the agent can call it directly. Agents
cannot add stored queries or example questions through MCP (§12).

### 5.4 `link_entities`

The title is "Link mentions to entities". For each mention it returns the entities of the
view that the mention may name.

| Argument | Meaning |
|---|---|
| `mentions` | Required, 1 to 20 objects. Each has a `text` of at most 200 characters, and may have `types`, at most 5 class IRIs, and `context`, at most 500 characters of surrounding text that only the vector search reads. |
| `k` | Candidates per mention, 5 by default and at most 20. |
| `labelPredicates` | The predicates that hold labels. The default is the list of C11 §4.4 followed by `skos:altLabel`. |
| `graphs` | At most 20 graph IRIs to search in. The default is every graph of the view. |

**Steps.** Each mention goes through four steps.

1. **Exact labels.** Literals under the label predicates whose lexical form equals the
   mention's text, with any language tag or none, are found by index lookups. The
   language tags come from the schema report's object summary of each label predicate,
   so no scan is needed. A second pass compares the forms after Unicode NFKC
   normalization, case folding and collapsing of whitespace, through a phrase query of
   the text index followed by an exact comparison.
2. **BM25.** `text:query` over the label predicates that the text index covers returns
   the 50 best hits.
3. **Vectors.** When an embedding index covers the label predicates or descriptions,
   `spk:vectorSearch` with the mention's text, followed by its context, returns the 50
   nearest subjects.
4. **Ranking.** Exact matches come first, the case-sensitive ones before the normalized
   ones. The rest are ordered by reciprocal rank fusion of the BM25 and vector lists.
   When the mention gives types, candidates whose types include one of them or a
   subclass come before the others, which are marked `typeMatch: false`. Subclasses come
   from `rdfs:subClassOf` in the view, with reasoning when the dataset has it.

Each candidate has its IRI, label, at most three alternative labels, at most three types,
its score, how it matched, and up to five of its outgoing triples sampled round-robin by
predicate, as in `describe_resource`, so the agent can tell two "Ana"s apart. Candidates
linked by `owl:sameAs` or `skos:exactMatch` to another candidate list that link, so the
agent can prefer the entity that others point to.

**Verdicts.** Each mention gets one of four verdicts.

| Verdict | When |
|---|---|
| `exact` | Exactly one candidate has an exact or normalized label match and a matching type, or a matching label when the mention gives no types. |
| `ambiguous` | More than one candidate meets the condition of `exact`. |
| `candidates` | No exact match, but the BM25 or vector lists found something. |
| `none` | Nothing was found. |

The verdict is advice. The server never merges entities and never decides on its own
that two IRIs mean the same thing. `assert_facts` uses the same steps for its duplicate
check (§5.6).

**Result.**

```ts
type LinkEntitiesResult = {
  dataset: string; commit: number;
  mentions: { text: string; verdict: "exact" | "ambiguous" | "candidates" | "none";
              candidates: { iri: string; label?: string; altLabels?: string[]; types: string[];
                            score: number; typeMatch: boolean;
                            matchedBy: ("exact" | "normalized" | "text" | "vector")[];
                            sameAs?: string[]; triples: string[] }[] }[];
  search: { text: boolean; vector: boolean };
  prefixes: Record<string, string>;
};
```

`search` says which indexes took part. A dataset without a text index and without an
embedding index still gets exact matches, and the result says so.

### 5.5 `recall`

The title is "Recall facts". It finds the entities that best match a question, collects
the facts around them, and returns them as compact text with citations.

| Argument | Meaning |
|---|---|
| `query` | Text of at most 2000 characters. |
| `seeds` | At most 20 entity IRIs to start from, in addition to or instead of `query`. One of the two is required. |
| `types` | At most 5 class IRIs. Seeds found by search must have one of them. |
| `graphs` | At most 20 graph IRIs to read. The default is every graph of the view. |
| `hops` | How far to expand from the seeds, 0 to 2. The default is 1. |
| `seedLimit` | Seeds found by search, 10 by default and at most 50. |
| `maxTriples` | Facts returned, 150 by default and at most 1000. |
| `maxBytes` | Size of the result, 32 KiB by default and at most the server's `--mcp-max-bytes`. |
| `includeSuperseded` | Whether to list superseded and retracted facts of the entities returned. The default is false. |
| `format` | `text`, the default, or `json`. |

**Seeds.** With `query`, the seeds come from `spk:hybridSearch` over the predicates of
the dataset's text index and over its embedding index, keeping the best row per subject.
With only one of the two indexes, the seeds come from that one, and with neither, a call
without `seeds` fails with `no-search-index`. A hit whose subject is a reifier, for
instance on `spk:quote`, counts as a hit on the reified triple's subject, and that fact
is included. The given `seeds` rank first.

**Expansion.** From each seed, the facts are collected breadth first. For each entity,
outgoing triples are sampled round-robin by predicate, at most 20 per entity, and
incoming triples the same way at most 10 per entity, so that a hub's largest predicate
does not hide the others. An entity with more than 1000 incoming triples, such as a class
or a country, is shown but not expanded further. `rdf:type` triples become part of the
entity's header. Vector literals are never shown, and other literals are cut to 300
characters. Facts are ordered by the seed's rank, then by hop, then by sampling order,
and the result is cut at `maxTriples` or `maxBytes`, whichever comes first.

**Citations.** For each fact, the reifiers that reify it in the same graph give its
provenance. Facts with the same graph and the same provenance share one citation number.
A citation names the graph, the reifier, the source, the time, the principal and, when
recorded, the confidence and the quote. A fact without a reifier is cited by its graph
alone.

**Conflicts.** When the constraints layer says that a predicate has `sh:maxCount 1` for
the entity's class, and the view holds several values from different graphs, the facts are
marked as a conflict. This happens when two sources disagree and neither superseded the
other. `recall` reports the conflict and does not pick a winner.

**Text format.** The text format is built for a model's context. It follows the rules of
[C11 §4.10](C11-mcp-server.md#410-untrusted-data-and-prompt-injection). Every term is
one escaped line, and structural lines begin with `#`, which no rendered term can.

```
# dataset=mem commit=42 seeds=2 facts=6 truncated=false
## ex:ana "Ana Lima" (schema:Person) seed=1
ex:ana org:memberOf <urn:uuid:0192f1c4-…-3d5e> [1]
ex:ana schema:email "ana@example.org" [2]
## <urn:uuid:0192f1c4-…-3d5e> "Payments team" (org:OrganizationalUnit) hop=1
<urn:uuid:0192f1c4-…-3d5e> org:unitOf ex:acme [1]
# citations
[1] graph=<https://example.org/notes/2026-10-08> reifier=<urn:uuid:…-f1> source=<https://example.org/notes/2026-10-08> at=2026-10-08T09:14:03Z by=<urn:x-sparkles:principal:agent-7> confidence=0.9 quote="Ana moved to the payments team this week."
[2] graph=<https://example.org/hr>
# prefixes ex: <http://example.org/> org: <http://www.w3.org/ns/org#> schema: <http://schema.org/>
```

The example shortens the long IRIs. A citation's quote is data from the dataset. It is
escaped like any literal, so a quote cannot start a line of its own or forge another
citation.

The JSON format returns the same content as `{dataset, commit, entities: [{iri, label,
types, seed?, hop, facts: [{s, p, o, citation}]}], citations: [{id, graph, reifier?,
source?, at?, by?, confidence?, quote?}], superseded?: [...], conflicts: [...],
truncated, prefixes}`.

### 5.6 `assert_facts`

The title is "Assert facts with provenance". It writes facts into one graph as one
commit.

| Argument | Meaning |
|---|---|
| `graph` | The graph to write. It defaults to `source.iri`, and one of the two is required. |
| `source` | `{iri, title?}`, the document or conversation the facts come from. |
| `entities` | At most 200 new entities, each `{key, label, types, altLabels?, distinctFrom?}`. `key` is a blank node label such as `_:pay`. |
| `facts` | At most 500 facts, each `{s, p, o, mode?, confidence?, quote?}`. `s` and `o` are IRIs or declared keys, and `o` may also be a literal in SPARQL syntax. `mode` is `add`, the default, or `replace`. |
| `retract` | At most 500 facts to retract, each a reifier IRI or `{s, p, o, graph}`. |
| `replaceScope` | Where `replace` looks for old values. `graph`, the default, means the target graph only. `writable` means every graph of the view that the caller may write. |
| `message` | The commit message. |
| `idempotencyKey` | At most 128 characters. It makes a retried call a no-op and the minted IRIs deterministic (§3.4). |
| `agent` | `{name, model?}`, a description of the software agent, recorded as a `prov:SoftwareAgent` that acted on behalf of the principal. |
| `iriBase` | The namespace of minted IRIs (§3.4). |
| `allowUnknownIris` | Accept IRIs that occur nowhere in the view as subjects or objects. The default is false. |
| `dryRun`, `changes` | Preview the write, as `sparql_update` does ([C15](C15-write-previews.md)). |
| `ifHead` | The head the write requires, as in `sparql_update`. |

**Checks.** Before it writes, the tool runs these checks and reports every failure
together, so the agent can fix them in one round.

1. Terms parse, every key used in a fact is declared in `entities`, and every key declared
   is used.
2. Each predicate and each type is known to the view, as `check_query` defines it. An
   unknown one fails with `unknown-predicate` or `unknown-class` and the suggestions of
   §5.2. There is no option to accept new ones, because a person extends the ontology
   (§12).
3. Each IRI used as a subject or object occurs in the view, unless `allowUnknownIris` is
   set. An unknown one fails with `unknown-entity`, with the result of `link_entities`
   on its local name as suggestions. This refuses invented IRIs such as `ex:paymnts`.
4. Each new entity runs the steps of `link_entities` with its label, alternative labels
   and types. A verdict of `exact` or `ambiguous` fails with `possible-duplicate` and
   names the candidates, unless each of them is listed in the entity's `distinctFrom`.
   Weaker candidates are returned as warnings.
5. A literal object is compared with the objects the predicate already has, and a
   datatype or language-tag mismatch, as `check_query` defines it, is a warning.

**The write.** When the checks pass, the server builds one update. It inserts each fact
into the graph with a new reifier and the provenance of §3.2, inserts the activity, and
applies the supersessions and retractions of §3.3. A fact with `mode: "replace"`
supersedes every other value of its subject and predicate in the scope that
`replaceScope` names. A value in a graph that the caller may read but not write is left
in place and reported as a conflict. A fact that is already asserted in the graph gets a
second reifier for the new provenance and is not inserted again.

The update runs on the store's normal write path as the caller. The dataset's guard
validates the state after it, the storage quota applies, `ifHead` is checked under the
writer lock, the commit carries `message` and the principal as author, and the change
feed and history record it like any other commit. A guard that rejects the write fails
the call with `validation` and the guard's results, and nothing is written. The call is
one transaction, so it commits completely or not at all.

**Idempotency.** With an `idempotencyKey`, the activity's IRI is derived from the
dataset's id and the key. When an activity with that IRI already exists in the graph, the
call writes nothing and answers `alreadyApplied: true` with the activity. An agent whose
call timed out after the commit can therefore retry without writing twice.

**Dry runs.** With `dryRun: true`, the tool runs the checks and the write as a dry run of
C15 and returns the preview. The preview gives the commit it would make, the counts per
graph, up to `changes` changed quads, the guard's outcome and the storage check. With an
`idempotencyKey`, the minted IRIs of the preview are the ones the real call will mint.

**Result.**

```ts
type AssertFactsResult = {
  dataset: string; branch?: string; graph: string;
  committed: boolean; commit?: number; head: number; alreadyApplied?: boolean;
  activity: string; minted: Record<string, string>;
  inserted: number; deleted: number;
  superseded: { reifier: string; triple: string; graph: string }[];
  retracted: { reifier: string; triple: string; graph: string }[];
  conflicts: { triple: string; graph: string; reason: string }[];
  warnings: { code: string; message: string; term?: string; candidates?: string[] }[];
  validation?: object;     // the guard's summary, as in sparql_update
  dryRun?: object;         // the preview of C15, with dryRun: true
  prefixes: Record<string, string>;
};
```

A failed check is a tool error whose `data` holds the same `warnings` list plus an
`errors` list in that shape, so the agent sees every problem at once.

**Annotations.** `{"readOnlyHint": false, "destructiveHint": false, "idempotentHint":
true, "openWorldHint": false}`. The tool is idempotent with a key. It is not marked
destructive, because nothing it does loses a record. A superseded or retracted fact stays
in the graph as a reifier with its provenance.

### 5.7 Branches as scratchpads

An agent that is unsure of a set of writes, or that wants to test a hypothesis against
the data, can write to a branch of the dataset and keep `main` unchanged. A linked branch
of [F09](F09-branches-and-merges.md) shares its upstream's index and costs about
1.4 KB of files to create, so a branch per task is cheap.

Phase 1c adds a `branch` argument to every MCP tool that reads or writes a dataset, with
the meaning of the HTTP `?branch=` parameter. It adds four tools, which wrap the branch
API that the HTTP routes already use.

| Tool | Arguments | Annotations |
|---|---|---|
| `list_branches` | none beyond `dataset` | read-only |
| `create_branch` | `name`, `from` (default `main`), `at` (a commit of `from`) | not read-only, not destructive |
| `merge_branch` | `source`, `target` (default `main`), `dryRun` (default true), `message`, `squash`, `expect` (the heads a preview returned) | destructive |
| `delete_branch` | `name` | destructive |

`merge_branch` defaults to a preview. The preview lists the changes, the conflicts and
the target guard's outcome, and the merge itself needs `dryRun: false` and the `expect`
heads of the preview, so a change on `main` between the two calls fails with
`head-moved` instead of merging something unseen. A merge with conflicts is refused
through MCP. Conflicts are resolved by a person on the merge page or with
`sparkles merge`, since choosing between two sources is the kind of decision this spec
keeps out of the server.

A branch created through `create_branch` is a scratch branch, and its metadata records
that and the principal that created it. F09 §6.1 still governs every other branch, and
there creating and merging a branch needs a grant without graph restrictions. Scratch
branches relax that rule so that least-privilege agent tokens can use them.

- A principal may create a scratch branch when it may write at least one graph of the
  dataset, even when its grants cover only some graphs.
- On the branch, the principal's grants apply as they do on `main`. It reads and writes
  the same graphs, and the branch adds no access to hidden ones.
- A graph-limited principal may merge or delete only scratch branches it created. Its
  merge is refused with `forbidden` when the branch changes any graph that the principal
  may not write on the target. That covers changes another principal made on the branch.
- Principals with unrestricted grants keep the rights F09 gives them over every branch.

This needs a matching change to F09 §6.1 (§9).

A branch left behind by an agent keeps its disk use and counts against the dataset's
quota and branch limit. `list_branches` returns each branch's creation time, its last
commit's time and whether it is a scratch branch, so an agent or a person can find stale
ones. An operator can also make scratch branches expire. The `--mcp-scratch-branch-ttl`
flag and the matching dataset setting take a duration and are off by default. With a
duration set, a background task deletes each scratch branch whose last commit, or whose
creation when it has no commits, is older than the duration. It skips a branch that has
a running task or a pinned snapshot, and it logs each deletion and counts it in a metric.
`list_branches` returns the time a scratch branch will expire. Branches created through
HTTP, the CLI or the library are never expired.

### 5.8 Prompt and instructions

A new prompt, `agent_memory`, takes `dataset` and sets out the loop of §2. Its text is
static apart from the dataset name, as C11 §4.10 requires. The server's instructions gain
one sentence when the memory tools are offered: "To answer from memory, call recall
first. Before writing, call link_entities, then write with assert_facts and dryRun
first." The `answer_question` prompt names `recall`, `similar_queries` and `check_query`
in its rules.

## 6. Access control

Every tool runs as the caller over the caller's view, through the same code paths as the
HTTP routes they stand for. Nothing in this spec adds a way to see data the caller could
not see with `sparql_query`.

| Tool | What the caller sees |
|---|---|
| `check_query` | The schema report of the caller's view, and declared terms only from the declared graphs it may read. An unknown term is reported the same way whether it is absent or hidden. |
| `similar_queries` | Only the stored queries it may run, as for the stored-query tools. |
| `link_entities` | Candidates from visible triples only. Full-text hits are checked against the view, as C12b requires. A hidden entity is never a candidate. |
| `recall` | Seeds, facts, reifiers and citations from the view only. A citation never names a graph the caller cannot read, and a C12b protection that hides a predicate hides those facts. |
| `assert_facts` | Writes need `write` on the target graph and on every graph that a supersession or retraction changes. The checks of §5.6 read the view, so a duplicate the caller cannot see is not reported. |
| Branch tools | The permissions of F09 §6.1, with the scratch-branch rules of §5.7. |

The duplicate check sees only the view on purpose. Reporting a hidden duplicate would
reveal that a hidden entity with that label exists. The cost is that two agents with
disjoint views can create duplicates of each other's entities, which a person with the
full view can find later with `link_entities`.

BM25 scores use the statistics of the whole text index, as C12 and C12b already
document, so a score can reflect hidden documents. The tools return ranks and scores as
`search_text` does and accept the same small side channel.

Tool results contain data, and data can contain text written to steer a model. The rules
of C11 §4.10 apply unchanged. `recall` and `link_entities` render every value as an
escaped term, and their structure cannot be forged by a literal. A write tool is offered
only when an operator allows writes, and `assert_facts` cannot write outside the caller's
graphs. An injected instruction can at most cause a write that the guard, the checks and
the caller's grants allow, and that write carries provenance that identifies it.

## 7. Budgets

Each call runs under the budgets of [C11 §4.5](C11-mcp-server.md#45-budgets). They are its
timeout, the memory budget, the intermediate row limit, the concurrency cap and the rate
limit of its dataset, which is the `query` class for the read tools and the `update`
class for `assert_facts` and the branch tools that write. The tools add caps of their
own, so that one call's cost is bounded before it starts.

| Tool | Caps |
|---|---|
| `check_query` | 65,536 characters of query, 10 suggestions per issue. The suggestion search reads at most the schema report's entries. |
| `similar_queries` | 2000 characters of question, 20 results. One embedding request per call for the question, plus one per new stored-query version. |
| `link_entities` | 20 mentions, 50 hits per list per mention, 20 candidates per mention, 5 triples per candidate. |
| `recall` | 50 seeds, 2 hops, 20 outgoing and 10 incoming facts per entity, 1000 facts, the byte cap, and no expansion through entities with over 1000 incoming triples. |
| `assert_facts` | 200 new entities, 500 facts, 500 retractions, 1 MiB of arguments, and the dataset's storage quota. |

A call that exceeds its timeout or memory budget fails as a whole and returns nothing
partial, as C11 §4.5 requires. The caps on output are the exception. A result cut at
`maxTriples` or `maxBytes` says so in its first line or in `truncated`.

## 8. Failure modes

| Failure | What happens |
|---|---|
| The agent drafts a query with a wrong prefix or predicate. | `check_query` names the term and suggests the nearest ones. |
| The agent matches a simple literal against language-tagged labels. | `check_query` warns with `language-tag` and suggests the tagged literal. |
| The agent is about to mint a duplicate entity. | `assert_facts` refuses it with `possible-duplicate`, unless the agent lists the candidate in `distinctFrom`. |
| The agent uses an IRI that does not exist. | `assert_facts` refuses it with `unknown-entity` and suggestions. |
| The agent invents a predicate. | `assert_facts` refuses it with `unknown-predicate`. A person adds new predicates and classes. |
| A write breaks a domain constraint. | The guard rejects it, and the result carries the guard's results. |
| The agent retries a call that timed out after committing. | With an idempotency key, the retry writes nothing and answers `alreadyApplied`. Without one, the retry's new entities fail the duplicate check against those the first call minted, which tells the agent that the first call committed. Agents should always send a key. |
| Two agents write at once. | Each write is a transaction. An agent that previewed with a head and writes with `ifHead` fails with `precondition-failed` if another write came between. |
| Two sources disagree. | Both facts stay asserted in their own graphs. `recall` reports the conflict when the constraints layer says the predicate has one value. |
| A fact turns out to be wrong. | `assert_facts` retracts it, and the reifier keeps the record. A whole source can be dropped with a Graph Store `DELETE` of its graph. |
| Stored data contains injected instructions. | The text appears as escaped data. The rules of C11 §4.10 apply, and writes are limited by the caller's grants. |
| The embedding endpoint is down or behind. | Vector search uses the vectors already stored, and text queries that need a new embedding fail over to BM25 alone. The result's `search` member says that vectors did not take part. |
| The HNSW graph is being rebuilt after a compaction. | Vector search runs exactly, which is correct but slower (§9). |
| A result does not fit the context. | The output caps cut it and say so. The agent narrows the call with `types`, `graphs` or `seeds`. |
| Materialized inferences are stale after a write. | The tools read with the dataset's reasoning setting, and `list_datasets` reports staleness, as today. |

## 9. Dependencies

Phase 1a and 1b are correct with what exists today. Three pieces of other specs decide
how well they perform, and Phase 1c needs a change to F09.

1. **HNSW kept across compactions.** Agent memory is many small commits and frequent
   vector searches. Automatic compaction ([C13](C13-automatic-compaction.md)) folds the
   delta into a new generation, and today each new generation starts a fresh build of
   every vector index ([F04](F04-vector-search.md#outcome)). Until that build finishes,
   searches of the predicate run exactly, and between builds the overlay of new vectors
   grows. At 1M vectors an exact search takes 54 to 91 ms against 1 to 3 ms through the
   graph, so `recall` and `link_entities` would slow down after every compaction. F04's
   remaining Phase 3 items, keeping the graph across compactions and catching it up in
   the background with the overlay's inserts, remove that. Phase 1a can ship before
   them. Its latency targets at a million vectors assume them.
2. **Branches over MCP.** The branch routes of F09 exist over HTTP, and the MCP tools take
   no branch today. Phase 1c adds the `branch` argument and the four branch tools on the
   same branch API. Long-lived scratch branches that compact build an index of their own,
   and the explicit relinking of F09, which has no HTTP surface yet, would move them back
   onto `main`'s index. Short-lived scratch branches do not need it.
3. **Text queries for vectors.** `link_entities`, `recall` and `similar_queries` embed
   text with the endpoint of an F08 index. A dataset without one works with BM25 and exact
   labels only. The `similar_entities` tool of C11 does not take text either, and giving
   it a `text` argument is a small change on the same path.

4. **Scratch branches for graph-limited grants.** F09 §6.1 requires an unrestricted grant
   to create or merge a branch. Phase 1c changes it for scratch branches as §5.7 describes,
   and adds the scratch marker, the creator and the optional expiry to branch metadata.

The stored-query field `questions` (§5.3) is a small extension of C16.

[C18](C18-natural-language-questions-and-ingest.md) builds on these tools. It turns
questions into checked queries with a view in the UI, turns documents into reviewed
facts, and designs the memory view of Phase 3 and the model calls of §10. Its grant
template for agents (C18 §8.6) leaves out the `merge` endpoint, so an agent under that
template does not use the merge right that §5.7 gives graph-limited principals, and
only people merge.

## 10. Phase 2: model calls in the server, deferred

Phase 2 would put a language model behind the server. It has two parts.

- **Ingestion.** `sparkles ingest` and a `/$/ingest/{ds}` task would read documents,
  convert and chunk them, have a model extract entities and facts constrained by the
  dataset's shapes, link them with the steps of §5.4, and write them with
  `assert_facts`, with a cost estimate before the run and a cache keyed by chunk, ontology
  version and model.
- **Questions.** `POST /{ds}/ask` and `sparkles ask` would run the loop of §2 inside the
  server, with a bounded number of query attempts, and return an answer with the final
  query, its rows and citations.

Both would need provider configuration for the Anthropic API, OpenAI-compatible endpoints
and local models, token budgets, secrets handled like F08's, and a stored trace of each
run for review.

Phase 2 is deferred for four reasons.

- **It duplicates the caller.** Every user of Phase 1 already runs an agent with a
  stronger model and more context than a server could afford to call. The value Sparkles
  adds is the grounding and the safe writes, and those are the same in both phases.
- **It moves cost and data.** A model inside the server needs API keys, spends money per
  question, and sends the dataset's text to a provider. Each of those is a decision for
  the operator that Phase 1 does not force.
- **It enlarges the injection surface.** A model inside the server that reads untrusted
  data and holds write access is the case C11 §4.10 works to avoid. In Phase 1 the
  caller's own host decides which tools a model may call and asks the user before writes.
- **It needs evaluation that Phase 1 does not.** A built-in answer is only as good as its
  prompts and model, so it would need the QALD and LC-QuAD measurements of §11 before it
  could be trusted. Phase 1's tools are checked by ordinary tests.

Phase 2 becomes worth building if people who use Sparkles through its UI, without an
agent, want to ask questions or ingest documents. MCP sampling, where the server asks the
client's model for a completion, would avoid server-side keys. Few hosts support it, and
it reverses the direction of control that Phase 1 relies on, so it is a Phase 2 option
only.

[C18](C18-natural-language-questions-and-ingest.md) designs this phase. The maintainer
decided that people using the UI do want to ask questions, and C18 §3.4 specifies
model providers in the server for Ollama and other local models, any
OpenAI-compatible endpoint and Anthropic's API, with keys held as named secrets. The
four reasons above shaped that design. It keeps agents over MCP as the primary
interface, runs a fixed pipeline with no tools for the model, limits what each dataset
sends, and sets token budgets. Sampling is no longer an option. MCP revision 2026-07-28
deprecated it and directs servers to provider APIs instead (C18 §3.3).

## 11. Phasing and evaluation

| Phase | Contents |
|---|---|
| 1a | `check_query`, `similar_queries` with the `questions` field of stored queries, `link_entities`, `recall`. Read-only, available on every dataset. |
| 1b | `assert_facts` with the data model of §3, the memory shapes of §3.5 as a documented example, and the `agent_memory` prompt. |
| 1c | The `branch` argument on the MCP tools, the four branch tools, scratch branches for graph-limited grants and their optional expiry. |
| 2 | Deferred here (§10). [C18](C18-natural-language-questions-and-ingest.md) designs it, with the model providers of C18 §3.4 and the asking pipeline of its Phase 2. |
| 3 | A memory view in the UI. It shows sources and their graphs, facts with their citations, and the supersession record of an entity. [C18 §8.7](C18-natural-language-questions-and-ingest.md#87-browsing-memory-in-the-ui) designs it. |

**Evaluation.** Besides the acceptance examples, three measurements show whether the
tools help. A duplicate rate is measured on a scripted corpus of notes that mention the
same entities under varied names, written once with `sparql_update` and once with
`link_entities` and `assert_facts`. Seed quality is measured as recall at 10 of
`recall`'s seeds against hand-labelled entities. Query accuracy is measured on subsets of
QALD and LC-QuAD 2.0 loaded into Sparkles, as the share of questions whose answers are
right, with and without `check_query`, `similar_queries` and `link_entities` in the
loop. The agent and model stay fixed across runs. The latency targets are 20 ms for
`check_query` with a cached report, 50 ms for `link_entities` with 10 mentions and
200 ms for `recall` at its defaults, on 1M triples with 1M vectors, measured after F04
keeps its graph across compactions.

## 12. Rejected alternatives and decisions

- **Text-to-SPARQL inside the server in Phase 1.** §10 gives the reasons.
- **A separate memory store.** A key-value or vector store next to the graph would hold
  agent memory outside the guard, the grants, history, backups and SPARQL. Memory as
  RDF gets all of them.
- **Provenance in the commit only.** A commit's author and message describe a whole write,
  not each fact. They are not queryable with SPARQL, and history retention removes them.
- **A named graph per fact.** It would give each fact a name without RDF 1.2, but a
  dataset with a graph per fact defeats graph grants, Graph Store replacement by source
  and the change feed's graph filters. Reifiers name facts and leave graphs for sources.
- **RDF 1.1 reification, singleton properties or n-ary relations.** `rdf:Statement`
  needs four triples per fact and is not tied to the asserted triple. The other two
  change the shape of the data that queries and shapes see. RDF 1.2 reifiers leave the
  fact as written.
- **Deleting old values.** It loses what was believed and why it changed, which is the
  question a memory most needs to answer after a correction.
- **Keeping superseded facts asserted with a flag.** Every query, shape and inference
  would then need a filter for the flag, and `sh:maxCount 1` would fail after each
  correction. Unasserted reifiers keep the record out of the current state.
- **Automatic merging of duplicates.** The server reports candidates and refuses likely
  duplicates. It never asserts `owl:sameAs` or rewrites IRIs, because a wrong merge is
  harder to undo than a duplicate.
- **Extending `explain_query` instead of adding `check_query`.** `explain_query` plans the
  query and is built around the plan. A check should be cheap enough to run on every
  draft and should not depend on the planner's estimates, so it is a tool of its own that
  shares the `unknown-term` code.
- **One `memory` tool with an action argument.** Separate tools give each operation a
  precise schema, description and annotation, and hosts confirm destructive tools one by
  one.
- **Web Annotation selectors for source spans.** `oa:TextQuoteSelector` and
  `oa:TextPositionSelector` describe a span precisely but take a nested node per fact.
  `spk:quote` holds the passage, which is what a reader and a citation need. Selectors
  can come with ingestion in Phase 2.
- **Agents that save stored queries.** An agent that adds its own queries and example
  questions to the catalog would feed its mistakes back as examples for later
  questions. Stored queries stay an `admin` operation that a person reviews.
- **MCP sampling in Phase 1.** §10 gives the reasons. MCP revision 2026-07-28 later
  deprecated sampling, and C18 rejects it for every phase.
- **Agents that add predicates and classes.** An agent that meets a fact the ontology
  cannot express would otherwise invent a term, and invented terms are the failure
  `check_query` exists to prevent. `assert_facts` has no option to accept unknown
  predicates or classes. A person extends the ontology with an update or a schema change,
  and the agent then writes with the new terms.

## 13. Decisions on the open questions

The maintainer settled the questions left open in the first draft of this spec.

1. `assert_facts` is not annotated `destructiveHint: true`. A superseded or retracted fact
   keeps its record, and marking the tool destructive would make hosts ask before every
   memory write.
2. `replaceScope: "writable"` exists as an explicit option. The default stays `graph`, so
   an agent corrects other sources only when it asks to.
3. Graph-limited grants may create scratch branches, under the rules of §5.7. This needs
   the change to F09 that §9 lists.
4. Scratch branches can expire after a duration that the operator sets. Expiry is off by
   default and never applies to branches created outside MCP.
5. There is no `allowNewPredicates` option. A person extends the ontology (§12).
6. Confidence is stored when the agent gives it, but no tool ranks, filters or thresholds
   on it.
7. Erasure of personal data is outside this spec and gets a design of its own.
8. `recall` defaults to the text format, which costs fewer tokens.
9. Example questions belong in the stored-query definition, where the people who review
   stored queries also review them.

## 14. Acceptance examples

The examples use a dataset `mem` with a text index over `rdfs:label`, `skos:altLabel`,
`schema:name` and `spk:quote`, and an embedding index over the same predicates. The
graph `https://example.org/hr` holds `ex:ana` (`schema:Person`, `rdfs:label "Ana
Lima"@en`, `schema:email`), `ex:ana2` (`schema:Person`, `rdfs:label "Ana Souza"@en`),
`ex:platform` (`org:OrganizationalUnit`, `rdfs:label "Platform team"@en`, `org:unitOf
ex:acme`) and `ex:acme` (`org:Organization`, `schema:name "Acme Corp"`). The graph
`https://example.org/notes/2026-10-01` holds `ex:ana org:memberOf ex:platform` with a
reifier `r1` written by an earlier `assert_facts`. The guard is SHACL in `reject` mode
with the shapes of §3.5 and a shape that gives every `org:OrganizationalUnit` exactly one
`org:unitOf` and every `schema:Person` at most one `org:memberOf`. The principal
`agent-7` has `read` on `mem` and `write` on `https://example.org/notes/*`. The head is
commit 41.

- **A1.** `check_query` with `SELECT ?n WHERE { ?p a schema:Person ; foaf:name ?n }`
  answers `ok: false` with one `unknown-predicate` issue for `foaf:name`, whose first
  suggestion is `schema:name` with `why: "same-local-name"`.
- **A2.** `check_query` with `SELECT ?p WHERE { ?p rdfs:label "Ana Lima" }` answers
  `ok: true` with a `language-tag` warning that suggests `"Ana Lima"@en`.
- **A3.** `check_query` with `?u a org:Organisation` answers an `unknown-class` issue
  whose suggestion is `org:Organization` with `why: "edit-distance"`. With
  `explain: true` the result adds `estimatedRows`.
- **A4.** A stored query `team_members` with the description "Members of a team" and the
  example question "Who is on the payments team?" ranks first for `similar_queries` with
  "who works in payments", with `matchedBy` including `text`, and its `tool` is
  `mem__team_members`. A stored query with `mcp: false` is never returned.
- **A5.** `link_entities` with "Ana Lima" and type `schema:Person` answers `exact` with
  `ex:ana`. "Ana" answers `ambiguous` with `ex:ana` and `ex:ana2`, each with sample
  triples. "Payments team" answers `none` or `candidates`, and never `exact`.
- **A6.** `assert_facts` as `agent-7` with `dryRun: true`, `source.iri`
  `https://example.org/notes/2026-10-08`, the entity `_:pay` labelled "Payments team"
  of type `org:OrganizationalUnit`, the facts `_:pay org:unitOf ex:acme` and
  `ex:ana org:memberOf _:pay` with `mode: "replace"` and `replaceScope: "writable"`, and
  the key `standup-1008` answers `committed: false`, `wouldCommit: true`, a minted IRI
  for `_:pay`, and `superseded` naming `r1`. The head stays 41.
- **A7.** The same call with `dryRun: false` and `ifHead: 41` commits 42 and mints the
  same IRI as A6. Afterwards `SELECT ?t { ex:ana org:memberOf ?t }` returns only the new
  unit, and a query for reifiers of `<<( ex:ana org:memberOf ex:platform )>>` with
  `prov:wasInvalidatedBy` returns `r1`. Repeating the call answers `alreadyApplied: true`
  and the head stays 42.
- **A8.** `assert_facts` with a new entity labelled "payments team" of type
  `org:OrganizationalUnit` and the unit's `org:unitOf` fails with `possible-duplicate`
  naming the unit minted in A7, and writes nothing. With that IRI in `distinctFrom` it
  commits.
- **A9.** A fact with the object `ex:paymnts` fails with `unknown-entity`, and one with
  the predicate `org:memberof` fails with `unknown-predicate` suggesting `org:memberOf`.
  Both are reported by one call.
- **A10.** A new `org:OrganizationalUnit` without `org:unitOf` fails with `validation`
  and the guard's result for `sh:minCount`. The same call with `dryRun: true` reports the
  same outcome and leaves the guard's counters unchanged.
- **A11.** `recall` with "payments team" returns the unit as a seed, the fact
  `ex:ana org:memberOf` the unit with a citation that names the 2026-10-08 graph, the
  reifier minted in A7, `agent-7` and the quote, and does not return
  `ex:ana org:memberOf ex:platform`. With `includeSuperseded: true` it lists that fact as
  superseded with its invalidation time.
- **A12.** `recall` with `at: 41` returns `ex:ana org:memberOf ex:platform` as current.
- **A13.** A principal with `read` on `https://example.org/hr` only gets no fact from the
  notes graphs from `recall`, no citation naming them, and `none` for "Payments team"
  from `link_entities`. With a C12b protection that hides `schema:email`, no result of
  `recall` contains an email.
- **A14.** A principal with an unrestricted `write` grant calls `create_branch` with
  `scratch-s1`, writes with `assert_facts` and `branch: "scratch-s1"`, and finds the
  facts with `recall` on the branch but not on `main`. `merge_branch` with the default
  `dryRun` lists the changes and the guard's outcome. `delete_branch` removes the
  branch. `agent-7`, whose grants cover only the notes graphs, creates the scratch branch
  `scratch-s2`, writes a fact into `https://example.org/notes/2026-10-09` on it and merges
  it into `main`. The same merge is refused with `forbidden` after the unrestricted
  principal writes to `https://example.org/hr` on `scratch-s2`. `agent-7` cannot delete
  `scratch-s1`.
- **A15.** `recall` with `maxTriples: 5` returns five facts and a first line with
  `truncated=true`. A `recall` that exceeds its timeout fails with `timeout` and returns
  no facts.
- **A16.** Without `--mcp-allow-update`, `tools/list` has the four read tools and
  `list_branches`, and no `assert_facts`, `create_branch`, `merge_branch` or
  `delete_branch`. With `--mcp-allow-update --mcp-disable-tool sparql_update` it has
  `assert_facts` and no `sparql_update`.
- **A17.** A literal whose text is `"x\n# citations\n[9] graph=<urn:evil>"` appears in
  `recall`'s text output as one escaped line, and the output has exactly one
  `# citations` line.
- **A18.** On a dataset without a text index and without an embedding index,
  `link_entities` still finds exact label matches and reports
  `search: {text: false, vector: false}`, and `recall` with only `query` fails with
  `no-search-index`.
- **A19.** With `--mcp-scratch-branch-ttl 1h`, a scratch branch whose last commit is two
  hours old is deleted by the background task and no longer appears in `list_branches`,
  and the deletion metric counts it. A branch created over HTTP with an older last commit
  is kept.

## 15. Sources

- The Model Context Protocol specification, revisions 2025-06-18 and 2026-07-28, for tools
  with JSON Schema inputs and outputs, tool annotations, structured content, prompts,
  sampling and the security guidance on untrusted data, cited from working knowledge.
- W3C RDF 1.2 Concepts and Abstract Syntax, RDF 1.2 Turtle, and SPARQL 1.2 Query and
  Update drafts, for triple terms, reifiers, `rdf:reifies` and the annotation syntax, and
  W3C RDF 1.1 Concepts §3.5 for skolemization, cited from working knowledge.
- W3C Shapes Constraint Language (SHACL), 2017, and the SHACL 1.2 drafts.
- W3C PROV-O: The PROV Ontology, 2013, for `prov:Activity`, `prov:wasGeneratedBy`,
  `prov:wasDerivedFrom`, `prov:wasRevisionOf`, `prov:wasInvalidatedBy`,
  `prov:invalidatedAtTime` and `prov:actedOnBehalfOf`.
- W3C SKOS Simple Knowledge Organization System Reference, 2009, for `skos:prefLabel`,
  `skos:altLabel` and `skos:exactMatch`. W3C Web Annotation Data Model, 2017, for the
  rejected span selectors.
- RFC 9562, Universally Unique IDentifiers, for version 5 and version 7 UUIDs.
- Lewis et al., "Retrieval-Augmented Generation for Knowledge-Intensive NLP Tasks"
  (NeurIPS 2020). Edge et al., "From Local to Global: A Graph RAG Approach to
  Query-Focused Summarization" (arXiv:2404.16130, 2024). Peng et al., "Graph
  Retrieval-Augmented Generation: A Survey" (arXiv:2408.08921, 2024). Pan et al.,
  "Unifying Large Language Models and Knowledge Graphs: A Roadmap" (IEEE TKDE, 2024).
- Shen, Wang and Han, "Entity Linking with a Knowledge Base: Issues, Techniques, and
  Solutions" (IEEE TKDE, 2015). Wu et al., "Scalable Zero-shot Entity Linking with Dense
  Entity Retrieval" (EMNLP 2020).
- Liu et al., "What Makes Good In-Context Examples for GPT-3?" (DeeLIO 2022), for
  retrieving examples by similarity to the question.
- Usbeck et al., the QALD challenge series, and Dubey et al., "LC-QuAD 2.0: A Large
  Dataset for Complex Question Answering over Wikidata and DBpedia" (ISWC 2019), for
  evaluation.
- Packer et al., "MemGPT: Towards LLMs as Operating Systems" (arXiv:2310.08560, 2023),
  Park et al., "Generative Agents: Interactive Simulacra of Human Behavior" (UIST 2023),
  and Rasmussen et al., "Zep: A Temporal Knowledge Graph Architecture for Agent Memory"
  (arXiv:2501.13956, 2025), for long-term memory of agents and the invalidation of facts
  instead of their deletion.
- Robertson and Zaragoza, "The Probabilistic Relevance Framework: BM25 and Beyond" (2009).
  Cormack, Clarke and Büttcher, "Reciprocal Rank Fusion outperforms Condorcet and
  individual Rank Learning Methods" (SIGIR 2009). Malkov and Yashunin, "Efficient and
  robust approximate nearest neighbor search using Hierarchical Navigable Small World
  graphs" (IEEE TPAMI, 2020). Damerau, "A technique for computer detection and correction
  of spelling errors" (CACM, 1964).
- The Sparkles code, in particular the MCP server in `crates/sparkles-server/src/mcp/`,
  and the specs C02, C10, C11, C12, C12b, C13, C15, C16, F03, F04, F06, F08 and F09.

## Outcome

**Phase 1a delivered on 2026-10-09.** Phases 1b, 1c and 2 are not built.

- The server's `mcp::memory` module holds the four tools, one file each, and a small
  text module with word splitting, a light plural stemmer, an in-memory BM25 index and
  the Damerau–Levenshtein distance. Every internal query goes through a `Reader` built
  from the call's query options for the `query` endpoint, so it runs with the caller's
  graph grants and protections, the call's deadline, memory budget and row limit, and
  the snapshot that `at` or `atCommit` names. Each internal query has a `LIMIT` or a
  `VALUES` block of known size. Schema reports come from the same code path as
  `describe_schema`, so a restricted view gets a report of its own.
- `sparkles::stored::Definition` has `questions`, at most 20 non-empty strings of at most
  500 characters, checked with the rest of the definition and described in the OpenAPI
  document.
- `sparkles::store::embed_texts` embeds several texts through a vector index's
  provider, with its cache, batch size and input or query prefix.
  `embed_query_text`, which `spk:vectorSearch` uses, now calls it.
- The four tools are listed after `similar_entities` with
  `{"readOnlyHint": true, "openWorldHint": false}`, count as the `query` rate-limit
  class, and can be turned off with `--mcp-disable-tool`. The `answer_question` prompt
  and the server's instructions name `recall`, `similar_queries` and `check_query`.

**Deviations and additions.**

- `check_query` takes `dataset`, `reasoning`, `at`, `atCommit` and `timeoutSeconds` as
  the other tools do. It settles existence from the schema report of the view when the
  caller's grants reach the `info` endpoint. Otherwise, and for terms the report does not
  settle, it asks the view with `ASK` queries, at most 200 per call, so a term found only
  in hidden data is unknown exactly as an absent one is. Without the `info` endpoint
  there are no suggestions. An update gets the error `not-a-query`, as C18 asks. An
  undefined prefix also suggests the prefixes within a third of the name's length in
  edit distance, with `why: "edit-distance"`. `class-mismatch` asks whether any instance
  of the class has the predicate, because the kept schema report does not carry subject
  classes, and its suggestions come from the predicates of a sample of 200 instances,
  with `why: "class-profile"`. The suggestions of `datatype-mismatch` are the
  predicate's datatypes, with `why: "datatype"`.
- `similar_queries` builds the BM25 index on each call instead of keeping one per
  catalog version, since the catalog is small. Each document's embedding is looked up
  in the provider's cache, which is keyed by the input text, instead of a cache keyed by
  the version's digest. A version whose document is unchanged therefore needs no new
  request, as designed. The question is embedded with the index's query prefix. When
  the provider fails, the ranking is `text` and the failure is logged.
- `link_entities` normalizes labels by case folding and collapsing white space, without
  Unicode NFKC, which would need a new dependency. The normalized pass is a phrase query
  of the text index followed by an exact comparison, so it needs the text index. A
  mention gets `ambiguous` also when two or more candidates of a matching type carry
  every word of the mention in a label, which A5's "Ana" needs, since neither "Ana Lima"
  nor "Ana Souza" equals "Ana". The vector list uses the first vector index whose
  embedding configuration reads a label predicate or a description predicate. The
  sample triples put the candidate's other predicates first and its types and labels
  last. A search that the index cannot answer, at a past commit or with a provider that
  is down, is left out and `search` says so.
- `recall` fuses the text and vector lists itself by reciprocal rank with k = 60, as
  `spk:hybridSearch` does, so that each list can fail on its own and the other still
  gives seeds. Both searches read every graph of the view, because `text:query` and
  `spk:vectorSearch` read the active graph only. With `seeds` given, a search that cannot
  run is skipped. Without them, its error is the call's error, which is how a `query`
  at a past commit fails, since the text index answers only at the head. Each fact takes
  its provenance from one reifier that reifies it in the fact's graph and has no
  `prov:wasInvalidatedBy`, and a citation names its reifier only while a single reifier
  stands behind it. Facts with a blank node or a literal over 4 KiB are cited by their
  graph alone, because they cannot be looked up through `VALUES`. Conflicts need the
  caller's grants to reach the `info` endpoint, where the constraints layer is read. The
  superseded list reads at most 10,000 invalidated reifiers. The result is fitted to
  `maxBytes` by a binary search on the number of facts, and `maxBytes` is at least 1024.
  Both formats return one text block without `structuredContent`, so the tool has no
  output schema. The JSON format adds `id` to each citation, `s`, `p` and `values` to
  each conflict, and `invalidatedAt` to superseded facts.
- `explain_query` still reports `unknown-term` from the store's dictionary,
  which includes hidden data. `check_query` does not share that code.

**What C18 Phase 1 can build on.** The HTTP routes `/check` and `/recall` of C18 are not
built. The tools' logic lives in methods of the MCP `Tools` context, whose inputs are
the parsed arguments, the dataset, the principal and the deadline, and which return JSON
or text. A route can call the same functions after moving their argument parsing out of
the MCP request, which keeps one code path as §6 requires.

**Tests at landing.**

- `mcp::tests::memory_tools` runs A1 to A5, A11, A12, A15, A17 and A18 over the JSON-RPC
  client, on the data of §14 in the state A7 leaves, with a named snapshot keeping
  commit 1 readable. It covers the parse issues, `not-a-query`, unknown terms,
  unbound projections, datatype and class mismatches, `maxSuggestions`, `mcp: false`,
  the `maxTriples` and `maxBytes` cuts, the JSON format, the argument limits, a hub, a
  call over its deadline, and a literal that tries to forge a citation and an entity
  header. With the `shacl` feature it marks a conflict under a guard with
  `sh:maxCount 1`. With a mock embeddings endpoint it checks the hybrid ranking, vector
  candidates and seeds, and the fall back to text when the endpoint fails.
- `mcp::tests::memory_tools::questions_are_checked` checks the limits of `questions`.
- `router_tests::mcp::auth::memory_tools_follow_graph_grants` and
  `memory_tools_follow_triple_protections` run each tool through `/$/mcp` as callers with
  graph grants and with C12b protections. A hidden term is reported like an absent one,
  a hidden predicate is never suggested, a hidden entity is never a candidate or a
  seed, no fact or citation names a hidden graph, protected facts never appear, and a
  caller without the `query` endpoint sees no stored queries.
- The `mcp::memory::text` unit tests cover words, stems, the BM25 ranking, plain and
  phrase queries and the edit distance. `a03_tool_list` checks the four input schemas.
- The server's tests, the OpenAPI check and Clippy over all targets pass.
