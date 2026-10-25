# U03: Query by example and search phrases

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 builds a SPARQL query from nodes and edges selected
> in Explore's graph, with constants turned into variables, filters and optional parts,
> a match count, and saving as a stored query with parameters. Phase 2 adds search
> phrases on stored queries and a phrase search box. Phase 3 adds the same selection in
> the query page's graph results and Cypher output once F01 exists.
>
> **User docs:** none, because nothing is built.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

This design was written from M. M. Zloof's "Query-by-Example" (1977) for the idea of
writing a query by marking an example, the public documentation of Neo4j Bloom's search
phrases (phrases with parameters that run a stored Cypher query), the public
documentation of Sparnatural (a visual SPARQL query builder driven by a configuration of
classes and properties), the W3C SPARQL 1.1 Query Recommendation, and the Sparkles code
and specs: stored queries ([C16](C16-stored-queries.md)), schema discovery and class
profiles ([C02](C02-schema-discovery.md)), the Cypher frontend
([F01](F01-cypher.md)), full-text search ([F03](F03-full-text-search/README.md)), the formatter
([X02](X02-formatter/README.md)) and the MCP server ([C11](C11-mcp-server.md)). Sparnatural is
LGPL-licensed, and none of its code was read or used.

## 1. Summary, goals, non-goals

People find interesting structure by browsing. In Explore they expand a person, see that
she works for a company located in a city, and want every person who works for a
company in that city. Today they must write that query by hand. This spec turns a
selection in the graph into a query: the selected nodes and edges are a pattern, and the
person decides which constants stay constant and which become variables.

The second half gives stored queries **search phrases**, as Neo4j Bloom does for Cypher:
a phrase such as "people working in $city" names a stored query, and typing "people
working in Lyon" runs it with `city` bound to the entity named Lyon. The phrases need no
model.

**Goals**

1. A selection of nodes and edges becomes a readable SPARQL query, with variable names
   taken from labels and classes, formatted by X02.
2. Each node can stay a constant, become a variable, become a variable of a class, or
   become a parameter. Each literal can stay, become a variable, or get a filter. Each
   edge can stay, become optional, or become a path.
3. The pattern shows how many matches it has before the person leaves Explore.
4. A pattern saves as a C16 stored query, with its parameters typed and their defaults
   taken from the example.
5. Stored queries can carry phrases, and a phrase search finds and runs them with
   parameters filled from entity names.
6. The same pattern produces Cypher once F01 exists.

**Non-goals**

- A query builder that starts from an empty canvas. Sparnatural and similar tools do
  that. This spec starts from data the person has seen.
- Aggregation, grouping, subqueries and `UNION` in the builder. The editor is one click
  away for those.
- Natural-language questions. C18's Ask bar handles free text with a model. Phrases are
  fixed templates.
- Updates. Patterns build read queries only.

## 2. User-visible behaviour

### 2.1 Selecting a pattern

Explore's graph toolbar gets **Pattern** (shortcut `P`). In pattern mode, clicking a
node or an edge adds it to the pattern, `Shift`-click removes it, and dragging a box adds
everything inside it. Selected elements get a thick outline. Selecting an edge selects
its two ends. A side panel replaces the properties panel while the mode is on.

```
┌─ Pattern ──────────────────────────────────────────── 3 nodes · 2 edges ─┐
│ ?person   ex:alice "Alice"        (•) variable of class ex:Person ▾      │
│                                   ( ) constant  ( ) variable  ( ) param   │
│ ?org      ex:acme "Acme"          (•) variable  …                         │
│ city      ex:lyon "Lyon"          (•) parameter  default ex:lyon          │
│ ── ?person ex:worksFor ?org       ☐ optional  path: once ▾               │
│ ── ?org ex:locatedIn city         ☐ optional  path: once ▾               │
│ Return: ☑ ?person ☑ ?org ☐ labels                                         │
├───────────────────────────────────────────────────────────────────────────┤
│ 214 matches (at commit 318)   first 10 ▾                                  │
│ [Open in editor]  [Save as stored query…]  [SPARQL ▾ | Cypher]            │
└───────────────────────────────────────────────────────────────────────────┘
```

### 2.2 Roles of nodes

| Role | Meaning | Default for |
|---|---|---|
| constant | The IRI as written. | Literals |
| variable | A variable, named from the label in camel case (`?acme` → `?org` when it has one class, see §4.2). | IRIs and blank nodes in the pattern |
| variable of class | A variable and `?x a C`, with `C` chosen from the node's types. | IRIs with exactly one non-builtin type |
| parameter | A variable that becomes a C16 parameter, with the example as its default. | none |

A literal node offers **constant**, **variable**, or **filter**. A filter is chosen by the
literal's datatype: equality, a range for numbers and dates, `CONTAINS` or `STRSTARTS`
for strings, and a language match for language-tagged strings. Blank nodes are always
variables, since their labels are not stable names.

### 2.3 Edges

An edge is a triple pattern with its predicate. **optional** wraps the edge, and the
nodes reachable only through it, in `OPTIONAL`. **path** chooses `once` (the predicate),
`one or more` (`p+`) or `zero or more` (`p*`). **predicate variable** replaces the
predicate with a variable, which finds any link between the two ends.

### 2.4 Preview, editor and saving

The match count runs `SELECT (COUNT(*) AS ?n)` over the pattern with a five-second
timeout. A timeout shows "more than the count could finish" instead of a number. The
first ten rows show under it on request. **Open in editor** hands the query to the query
page through `app.pendingQuery`, as Explore's describe action
(`describeInQuery`) already does.
**Save as stored query…** opens C16's save dialog filled in: the name from the classes
(`person-by-city`), the parameters with types (`iri` for IRI nodes, the XSD type for
literals), their defaults, and a description written from the pattern ("People who work
for an organization located in a city"). Saving needs `admin`, as C16 does.

### 2.5 Search phrases (Phase 2)

A stored query may list phrases. A phrase is text with parameter slots:

```json
{ "phrases": ["people working in $city", "who works in $city"],
  "parameters": { "city": { "type": "iri", "class": "http://example.org/City" } } }
```

Explore's search box and the query page's Ask bar both match phrases as the person
types. A suggestion lists the phrase with its slots filled by candidates, such as
"people working in **Lyon** (ex:lyon)". Choosing it runs the stored query by name with
the parameters, through `/{ds}/queries/{name}`, and shows the results in the query
page's views. A phrase whose slots are not yet filled shows the slot's description as a
hint. When the Ask bar has a model, phrase suggestions appear above the **Ask** action, so
a phrase that fits runs without a model call.

### 2.6 Empty and error states

| Case | What the person sees |
|---|---|
| A selection with two parts that do not connect | "These parts are not connected, so the query matches every combination", with the count of each part. **Save** asks for confirmation. |
| A pattern of only constants | "This pattern has no variables. It checks whether these triples exist" and the query is an `ASK`. |
| No matches | "No matches at commit N" and the pattern stays for editing. |
| A caller without `admin` | **Save as stored query** is replaced by **Copy query**. |
| A phrase whose slot finds no entity | The suggestion shows "no City named 'Lyn'" and nothing runs. |

### 2.7 Keyboard and accessibility

Pattern mode is reachable without a mouse. With the graph focused, `Tab` moves between
nodes and edges in the order the explorer added them, `Space` toggles one in the pattern,
and the panel's controls are ordinary radio groups and checkboxes. The graph announces
"ex:acme added to the pattern, 3 nodes, 2 edges" in a polite live region. Selection is
shown by outline width and a check mark in the panel as well as colour. Phrase
suggestions are a combobox listbox in the WAI-ARIA pattern.

## 3. Server and API changes

Phase 1 needs no server change. It uses `/{ds}/sparql` and C16's `PUT
/$/queries/{ds}/{name}`.

Phase 2 changes C16's definition:

- **`phrases`**: up to 20 strings of at most 200 characters. Every `$name` slot must be a
  parameter of the query, and every required parameter without a default must appear in
  each phrase. A definition that breaks this is `400`.
- **`class`** on an `iri` parameter: an IRI. It limits phrase suggestions to instances of
  the class. It does not change how a run binds the value.
- **`GET /$/queries/{ds}/phrases`**: every phrase of the stored queries the caller may
  read, as `{name, phrase, slots: [{name, type, class?, description?}]}`, with an
  entity tag that changes with any definition. The client matches locally, so typing
  costs no request beyond entity lookups.
- **MCP.** `similar_queries` (C17) ranks stored queries by their `questions` today, and
  ranks phrases with them, with slots shown as `{city}`.

Phase 3 adds nothing for SPARQL. Cypher output needs F01's compiler and name resolution,
and stored Cypher queries need F01 Phase 3.

## 4. Design

### 4.1 The pattern model

`lib/qbe.ts` holds a pure model:

```ts
type PNode = { id: string; term: Term; role: 'constant' | 'variable' | 'class' | 'param' | 'filter';
               var?: string; class?: string; filter?: Filter };
type PEdge = { id: string; s: string; p: string; o: string; optional: boolean;
               path: 'once' | 'plus' | 'star'; predicateVar?: string };
type Pattern = { nodes: PNode[]; edges: PEdge[]; project: string[]; labels: boolean };
```

`toSparql(pattern, prefixes)` writes the query text: the prefixes the pattern uses, the
projection, triple patterns in the order of a walk from the most constrained node,
`OPTIONAL` groups, `FILTER`s and a `LIMIT 1000` that the person can change in the editor.
Parameters are plain variables in the text, as C16 requires: C16 binds them as terms
when the query runs by name. The text goes through the formatter (`fmt-wasm`), so it
matches what **Format** produces.

`components(pattern)` finds connected parts for the warning of §2.6.

### 4.2 Variable names

A variable is named from the node's single non-builtin class in lower camel case
(`?person`), else from its label (`?acme`), else from the predicate that reaches it
(`?worksFor`), else `?n1`. Clashes get a number (`?person2`). Names are valid SPARQL
`VARNAME`s and are not the request parameter names that C16 reserves (`RESERVED` in
`lib/stored-queries.ts`).

### 4.3 Matching phrases

`lib/phrases.ts` tokenizes phrases and input in lower case on Unicode word boundaries.
A phrase matches when its literal tokens match the input's in order, allowing the last
input token to be a prefix while typing. Each slot takes one to six input tokens. A
slot's candidates come from the label search Explore already uses (`explore.search`),
restricted to instances of the slot's `class`, or from `text:query` when F03 is enabled.
Typed slots (`integer`, `date` and the rest) take the token's text when it has the
type's lexical form. Suggestions rank by the number of literal tokens matched, then by
the candidate's label match, then by phrase order. Matching is a few hundred phrases at
most per dataset, so it runs in the browser on every key with no index.

### 4.4 Cypher output (Phase 3)

`toCypher(pattern)` writes `MATCH` with node labels from classes and relationship types
from predicates, named by F01 §3's rules, so the names resolve back to the same IRIs.
Optional edges become `OPTIONAL MATCH`, `plus` paths become `*1..` and `star` paths
`*0..`. A literal filter becomes a `WHERE` clause. Patterns that F01's subset cannot
express (predicate variables) disable the Cypher tab with the reason.

## 5. Architecture

| Part | Where |
|---|---|
| Pattern model, SPARQL and Cypher text | `ui/src/lib/qbe.ts` (new) |
| Phrase matching | `ui/src/lib/phrases.ts` (new) |
| Pattern panel | `ui/src/lib/components/PatternPanel.svelte` (new) |
| Selection in the graph | `GraphView.svelte` gains a `multiSelect` mode and a box selection (Cytoscape's `boxSelectionEnabled`) |
| Explore wiring | `routes/explore/+page.svelte` |
| Save dialog | the query page's existing save form, factored into a component that both pages use |
| Phrase suggestions | `AskBar.svelte` and Explore's search box |
| Definition checks, phrases route | C16's handlers in `sparkles-server` and the stored-query store in the `sparkles` crate |
| MCP | the `similar_queries` tool of C17 |

## 6. Phases

1. **Phase 1.** Pattern mode in Explore, roles, filters, optional edges and paths, the
   count and preview, **Open in editor** and **Save as stored query**.
2. **Phase 2.** `phrases` and `class` in C16, the phrases route, phrase suggestions in
   Explore and the Ask bar, and phrases in `similar_queries`.
3. **Phase 3.** Pattern mode on the query page's graph results, and Cypher output after
   F01 Phase 1.

## 7. Acceptance examples

Data: `ex:alice a ex:Person ; ex:worksFor ex:acme .`, `ex:bob a ex:Person ; ex:worksFor
ex:beta .`, `ex:acme a ex:Org ; ex:locatedIn ex:lyon .`, `ex:beta a ex:Org ; ex:locatedIn
ex:lyon .`, `ex:lyon a ex:City ; rdfs:label "Lyon" .`, `ex:paris a ex:City ; rdfs:label
"Paris" .`

- **A1. Default roles.** In Explore, focus `ex:alice`, expand `ex:acme`, turn on Pattern
  and select the edges `worksFor` and `locatedIn`. The panel shows `?person` (class
  `ex:Person`), `?org` (class `ex:Org`) and `?city` (class `ex:City`). The count reads
  "2 matches".
- **A2. Constant.** Setting `ex:lyon` to constant gives a query that contains
  `?org ex:locatedIn ex:lyon`, and the count stays 2.
- **A3. Generated text.** With A2's choices, **Open in editor** opens the query page with
  this query, formatted:

  ```sparql
  PREFIX ex: <http://example.org/>
  SELECT ?person ?org WHERE {
    ?person a ex:Person ; ex:worksFor ?org .
    ?org a ex:Org ; ex:locatedIn ex:lyon .
  }
  LIMIT 1000
  ```
- **A4. Save with a parameter.** Setting `ex:lyon` to parameter and saving as
  `person-by-city` stores a C16 definition with parameter `city` of type `iri` and default
  `http://example.org/lyon`. Running it by name with `city=ex:paris` returns no rows.
- **A5. Optional.** Making `locatedIn` optional and removing `ex:lyon`'s class wraps that
  edge in `OPTIONAL`, and the count stays 2.
- **A6. Disconnected.** Selecting `ex:alice` and `ex:paris` alone shows the warning and
  "1 × 1 combinations".
- **A7. Phrase (Phase 2).** After adding `"phrases": ["people working in $city"]` and
  `class: ex:City` to `person-by-city`, typing "people working in ly" in Explore's search
  offers "people working in Lyon (ex:lyon)". Choosing it opens the query page with the
  stored query run with `city=ex:lyon`, showing two rows.
- **A8. Bad phrase.** A definition with phrase "people in $town", where `town` is not a
  parameter, is refused with `400`.
- **A9. Keyboard.** A1 can be done with the keyboard alone, and the live region announces
  each addition.

## 8. Testing

- **Vitest.** `lib/qbe.ts`: SPARQL text for each role, filter, optional and path, variable
  naming and clashes, connected parts, and that every generated query parses (with the
  `oxigraph` package, a dev dependency the mock server already uses). `lib/phrases.ts`: tokenization, prefix
  matching, slot spans, ranking.
- **Rust.** C16 definition checks for phrases and `class`, and the phrases route's
  filtering by the caller's grants.
- **Playwright.** A1–A6 and A9 against a real server; A7 and A8 once Phase 2 lands.

## 9. Rejected alternatives

- **Inferring the pattern automatically from two selected nodes.** F07's path search
  already finds connections. Query by example starts from the edges the person chose, so
  the query says what they meant.
- **Generalizing constants by default.** Turning every IRI into a variable makes the
  first count huge and slow. IRIs become variables of their class, which keeps the count
  close to the example's meaning.
- **Phrases matched by a model.** Phrases are meant to work without a model and to run the
  same query every time. The Ask bar remains for free text.
- **A separate phrase store.** Phrases belong to the query they run, so they live in
  C16's definition, are versioned with it and are removed with it.

## 10. Open questions

1. Whether phrases should have a language tag, so a dataset can offer phrases in more
   than one language.
2. Whether the pattern should be kept with the stored query, so a saved query can be
   reopened as a pattern rather than as text.
3. Whether box selection conflicts with panning on touch screens, where the explorer is
   already used.

## 11. Relation to the visual query builder (2026-10-23)

This section was added on 2026-10-23, when the user approved a two-way visual query
builder as [U08](U08-visual-query-builder.md). U08 lifts the first non-goal of §1 for the
query page. It edits a query on a canvas that starts empty or from any query text, and it
keeps the editor's text as the only source of truth. This spec keeps its own scope, which
is starting from data the person selected in Explore.

The two specs share three things. The filter forms of §2.2 and the variable naming of §4.2
are defined once, by whichever of this spec's Phase 1 and U08's Phase 2 lands first. **Open
in editor** opens U08's Pattern pane with the generated query when U08 Phase 1 exists, so
the person sees the same pattern they selected. Open question 2 of §10 is the same question
as U08's open question 1 and is decided there.

`UNION`, which §1 leaves out of this spec, is in U08's Phase 3 subset.

## Provenance

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

- **Spec:** [`U03-query-by-example.md`](U03-query-by-example.md), written on 2026-10-12 independently from:
  - M. M. Zloof, "Query-by-Example: a data base language", IBM Systems Journal 16(4), 1977;
  - the public documentation of Neo4j Bloom's search phrases;
  - the public documentation of Sparnatural (LGPL-3.0), whose code was not read or used;
  - the W3C SPARQL 1.1 Query Recommendation and the WAI-ARIA combobox pattern;
  - the Sparkles code and specs C02, C11, C16, C17, F01, F03 and X02.
- **Implementation:** not built. No new runtime dependency is planned. The tests use the
  `oxigraph` npm package (MIT OR Apache-2.0), already a dev dependency, to parse
  generated queries.
- **Rejected** (spec §9):
  - inferring the pattern from two selected nodes;
  - generalizing constants by default;
  - phrases matched by a model;
  - a phrase store separate from stored queries.

## Outcome

Not built. See [ROADMAP](../ROADMAP.md) item 28.
