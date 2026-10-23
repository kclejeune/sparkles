# U08: A two-way visual query builder

> **Status:** partly implemented
>
> **Phases:** Phase 1 shipped on 2026-10-23, with the splice core of Phase 2. Phase 1
> draws the pattern graph of any query, from the editor's text or from a query that ran.
> Phase 2 makes the canvas an editor of the same text: variables, IRIs, literals and
> predicates picked from the schema, filters, optional parts and simple paths, with every
> change written back to the text as a small splice and every text change redrawn. Its
> splices for adding and deleting a triple and for optional on an edge shipped with Phase
> 1, and the rest of it is not built. Phase 3 adds `UNION`, `GRAPH`, inline `VALUES`,
> `MINUS` and `FILTER NOT EXISTS` to the part of SPARQL the canvas edits, and starting a
> pattern from a class in the Explore overview. It is not built.
>
> **User docs:** [Usage: The Pattern pane](../USAGE.md#the-pattern-pane) ·
> [Features](../FEATURES.md#web-ui)
>
> This is the design as written before implementation, with the answers to open
> questions 1 and 2 of §10. The [Outcome](#outcome) section at the end records how
> Phase 1 landed.

This design was written from the W3C SPARQL 1.1 Query Recommendation and the SPARQL 1.2
working drafts, the public documentation of Sparnatural and of Neo4j Bloom as examples of
visual query tools, the WAI-ARIA Authoring Practices for the listbox, combobox and grid
patterns, the CodeMirror 6 documentation of transactions and history, and the Sparkles
code and specs: query by example ([U03](U03-query-by-example.md)), the formatter and its
lossless SPARQL parser ([X02](X02-formatter.md)), schema discovery and class profiles
([C02](C02-schema-discovery.md)), declared prefixes ([C20](C20-declared-prefixes.md)),
stored queries ([C16](C16-stored-queries.md)), the Explore overview
([U06](U06-large-graph-rendering.md)) and graph access control
([C12](C12-graph-access-control.md)). Sparnatural is LGPL-licensed, and none of its code
was read or used.

## 1. Summary, goals, non-goals

U03 builds a query from nodes and edges a person selected in Explore. It starts from data
and ends in the editor, and the text it writes is not drawn again. This spec closes the
loop. The query page gets a canvas beside the editor that shows the query's graph pattern,
and the canvas and the text are two views of one query. Drawing a node or an edge changes
the text, and typing in the editor redraws the pattern. Any query that ran, including one
from the history or a stored query, can be shown as its pattern graph.

The hard part is the round trip. SPARQL has far more syntax than a canvas can edit, and a
person who writes a `BIND`, a subquery or a comment must not lose it because they dragged
an edge. The canvas therefore edits a defined subset (§4.1). Everything outside it is
drawn as a **locked block** that keeps its text byte for byte and can be edited only in
the editor. A canvas edit never regenerates the query. It is a splice of the text at the
ranges the parser reports, so every byte it does not touch stays as it was.

**Goals**

1. The pattern graph of any query that parses, drawn within 100 ms of a keystroke for
   queries of up to 200 triple patterns.
2. A canvas that edits basic graph patterns, filters, optional parts and simple property
   paths, with predicates and classes picked from the dataset's schema under the caller's
   view.
3. A round trip that loses nothing. A canvas edit changes only the text it must change.
   Text outside the editable subset, comments and formatting outside the edited range
   survive every edit.
4. A stable layout. A node keeps its position while the text around it changes, and a new
   node appears next to the nodes it connects to.
5. One undo history for both views, so `Mod+Z` undoes a canvas edit and a typed edit
   alike.
6. Keyboard and 390 px phone use for every edit, not only for viewing.

**Non-goals**

- Editing aggregates, grouping, subqueries, `MINUS`, `EXISTS`, `SERVICE`, `BIND` or
  update requests on the canvas. They are locked blocks, which the editor edits.
- A layout format in the query text. Positions are kept beside the text (§4.4), so the
  text a person copies is plain SPARQL.
- Replacing U03. Explore's pattern selection stays the way to start from data, and it
  hands its query to this canvas.
- Cypher. F01 and U03 Phase 3 cover a Cypher view of a pattern.

## 2. User-visible behaviour

### 2.1 The canvas

The query page's editor gets a **Pattern** pane, opened from the editor toolbar or with
`Alt+P`. On a wide screen it sits to the right of the editor, and the divider between them
can be dragged. The pane is a Cytoscape canvas drawn with the same styles as Explore's
graph.

```
┌─ Query ─────────────────────────────┬─ Pattern ─────────────── [+ Node] [+ Edge] ─┐
│ PREFIX ex: <http://example.org/>    │                                              │
│ SELECT ?person ?org WHERE {         │   (?person:Person) ──ex:worksFor──▶ (?org)   │
│   ?person a ex:Person ;             │        │                            ╎        │
│     ex:worksFor ?org .              │   ex:name                     ╎ OPTIONAL ╎   │
│   ?person ex:name ?name .           │        ▼                    ╎ ex:locatedIn╎  │
│   FILTER(STRSTARTS(?name, "A"))     │   (?name) [starts with "A"]   ▼            │
│   OPTIONAL { ?org ex:locatedIn ?c } │                            (?c)              │
│   BIND(YEAR(NOW()) AS ?y)           │   ┌ locked · BIND ───────────────┐           │
│ }                                   │   │ BIND(YEAR(NOW()) AS ?y)      │           │
│ LIMIT 100                           │   └──────────────────────────────┘           │
├─────────────────────────────────────┴──────────────────────────────────────────────┤
│ SELECT ☑ ?person ☑ ?org ☐ ?name ☐ ?c   DISTINCT ☐   LIMIT 100   [Count matches]    │
└────────────────────────────────────────────────────────────────────────────────────┘
```

- **Nodes.** A variable is a round node named `?person`, with its classes as a subtitle
  when the pattern says `?person a ex:Person`. An IRI is a rectangle with its prefixed
  name and its label when the label is cached. A literal is a small rounded rectangle.
  A labelled blank node is drawn as a variable with a dotted outline, because it behaves
  as one.
- **Edges.** A triple pattern is an edge labelled with its predicate. A predicate that is a
  variable is drawn as a dashed edge labelled `?p`. A simple path is an edge labelled with
  the path's text, such as `ex:partOf+`.
- **Filters.** A filter that the canvas understands is a chip on the node of its
  variable, such as `starts with "A"` or `> 2020-01-01`. Any other filter is a locked chip
  that shows its text and attaches to every variable it mentions.
- **Optional parts.** An `OPTIONAL` group is a dashed hull around its edges and the nodes
  only it binds, labelled OPTIONAL.
- **Locked blocks.** A part outside the subset is a box with its kind and its text. Thin
  dotted lines join it to the variable nodes it shares, so the reader can see that `?y`
  comes from a `BIND`.
- **Projection and modifiers.** The strip under the canvas lists the projected variables
  as checkboxes, `DISTINCT`, `LIMIT` and `OFFSET`. `ORDER BY`, `GROUP BY` and `HAVING`
  appear in the strip as locked text.

### 2.2 Editing on the canvas (Phase 2)

| Action | Result in the text |
|---|---|
| **+ Node** with a variable name | Nothing until an edge uses it, because SPARQL has no free variables. The node waits, dimmed, in the canvas. |
| **+ Node** with an IRI or a class | The same, and a class node becomes `?x a C` when linked. |
| **+ Edge** from a node to a node, with a predicate | A new triple pattern at the end of the group that holds the source node's patterns. |
| Changing an edge's predicate | The predicate token is replaced. |
| **Optional** on an edge | The triple moves into an `OPTIONAL { … }` at the end of its group, with the edges that only it connects. |
| **Path** on an edge | The predicate becomes `p+`, `p*`, `p?`, `^p`, or a sequence or alternative of IRIs. |
| A filter on a node | A `FILTER` is appended after the last triple that binds the variable. |
| Renaming a variable | Every occurrence in the editable subset is renamed, and a locked block that uses it is renamed too only after the person confirms. |
| Deleting an edge or a node | The triple patterns go, along with a filter chip that no longer has a bound variable. A locked block is never deleted from the canvas without a confirmation that shows its text. |
| Toggling a projection checkbox | The `SELECT` clause changes. `SELECT *` becomes an explicit list on the first change. |

The predicate picker is a combobox fed from the schema (§3.1). When the source node has a
class, it lists first the predicates that instances of that class use, with their counts,
then the rest. When the target is a class node, it ranks predicates whose values are
instances of that class. Typing filters the list, and typing a full IRI or a prefixed
name that the list lacks is accepted. A term whose namespace has a declared prefix (C20)
that the query does not declare adds the `PREFIX` line at the top.

Filters are built from a form chosen by the variable's observed datatypes, as U03 §2.2
does: equality, ranges for numbers and dates, `CONTAINS`, `STRSTARTS` and `REGEX` for
strings, `LANGMATCHES` for language-tagged strings, `BOUND`, `isIRI` and `isLiteral`.

**Count matches** runs `SELECT (COUNT(*) AS ?n)` over the pattern under a five-second
timeout, as U03 does, and shows the count or "more than the count could finish".

### 2.3 Editing the text

The canvas redraws 150 ms after typing stops. Text that does not parse leaves the last
pattern on screen, dimmed, with the error's line and column, and the canvas refuses edits
until the text parses again, so a canvas edit never lands in broken text. The redraw
keeps every node whose identity survived (§4.4) where it was.

### 2.4 Patterns of queries that ran

The result area gets a **Pattern** tab next to Table, Graph, Map and Plan. It draws the
pattern of the query that produced the result, which may differ from the editor's text
after the person kept typing. A history entry and a stored query have **Show pattern**,
which opens the same view read-only. **Edit** in that view copies the text into the
editor and opens the pane. U04 Phase 4 draws the plan on this view.

### 2.5 Keyboard

The canvas is one focus stop. Inside it, focus moves through elements in text order.

| Key | Action |
|---|---|
| `Alt+P` | Open or close the Pattern pane, and move focus between it and the editor. |
| `Tab`, `Shift+Tab` | Next or previous element in text order, then out of the canvas. |
| Arrow keys | Focus the nearest element in that direction. With `Shift`, move the focused node by 16 px. |
| `Enter` | Open the focused element's panel, where every action of §2.2 is a control. |
| `N` | New node. A combobox asks for a variable, an IRI or a class. |
| `E` | New edge from the focused node. The predicate picker opens, then a target picker. |
| `O` | Optional on or off for the focused edge. |
| `F` | New filter on the focused variable. |
| `Delete` | Delete the focused element, with the confirmation of §2.2 for a locked block. |
| `Mod+Z`, `Mod+Shift+Z` | Undo and redo, shared with the editor. |
| `Escape` | Close a panel or picker, or return focus to the editor. |

A polite live region announces each edit, such as "Added ?person ex:worksFor ?org, 3
nodes, 2 edges", and the element in focus is described with its role, such as "edge
ex:worksFor from ?person to ?org, optional". A locked block is announced with its kind
and its first line. Every mark is a symbol or text as well as a colour.

### 2.6 The 390 px layout

On a phone the editor and the canvas are two tabs, **Text** and **Pattern**, over the
same query. The modifier strip folds into a **Query options** sheet. Nothing needs a
drag. A tap selects an element and shows a bottom sheet with its actions. **+ Edge** is a
two-tap mode: tap the source, tap the target or **New node**, and pick the predicate from a
full-screen picker. Pinch zooms and one finger pans. Nodes keep 44 px touch targets at
the default zoom, and labels shorten to the local name with the full name in the sheet.

### 2.7 Empty and error states

| Case | What the canvas shows |
|---|---|
| An empty editor | "Add a node to start" with **+ Node** and the five classes with the most instances. |
| An update request | "Updates are not drawn. The builder edits queries." The pane stays read-only. |
| A query beyond the drawing caps (§4.6) | "This pattern has N triple patterns, more than the canvas draws", with a list of the patterns. |
| A part that does not connect | The parts are drawn apart with "These parts are not connected, so the query matches every combination", as U03 §2.6 says. |
| A schema request that fails or times out | The picker accepts typed IRIs only and says why. |

## 3. Server and API changes

Phases 1 and 2 need no new server route. Phase 3 needs none either.

### 3.1 Routes the builder reads

- **`GET /$/schema/{ds}`** gives the classes and predicates for the picker, first page
  only, at most 1,000 of each. Later pages are fetched when the person scrolls the
  picker, and typed text filters the pages already fetched. A dataset with more
  predicates than that still accepts any typed IRI.
- **`GET /$/schema/{ds}/profiles?class=…`** gives, for one class at a time, the
  predicates its instances use, the classes of their values and the datatypes of literal
  values. The builder asks for a class when a node first gets it and caches the answer
  per commit.
- **`/{ds}/sparql`** runs **Count matches** and the query itself, as today.
- **The declared prefixes** come from the same source that the editor's prefix
  completion already reads (C20).

All of these read under the caller's view, so a reader limited by graph grants (C12) or
protections (C12b) sees only the classes and predicates the schema routes already show
them.

### 3.2 The parser in the browser

The builder needs the query's syntax with byte ranges. The formatter's SPARQL parser in
`sparkles-fmt` is lossless: its tree keeps every token, its trivia and its range. Phase 1
adds a third export to the browser module in `crates/sparkles-fmt-wasm`, beside `format`
and `lint`:

```ts
// pattern(JSON.stringify({ text })) returns JSON
type PatternAnswer =
  | { ok: true; form: "select" | "ask" | "construct" | "describe";
      prologue: { prefixes: { prefix: string; iri: string; range: Range }[] };
      root: Group;                           // the WHERE group
      projection: { vars: string[]; star: boolean; distinct: boolean; range: Range };
      limit?: { value: number; range: Range }; offset?: { value: number; range: Range };
      modifiers: Locked[];                   // ORDER BY, GROUP BY, HAVING, a trailing VALUES
      locked: Locked[] }
  | { ok: false; error: string; line: number; column: number };

type Range = { from: number; to: number };   // UTF-16 offsets, as `lint` returns
type Group = { kind: "group" | "optional" | "union-branch" | "graph"; range: Range;
               graph?: Term; items: (Triple | Filter | Group | Locked)[] };
type Triple = { s: Term; p: Pred; o: Term; range: Range;   // the subject's statement
                sRange: Range; pRange: Range; oRange: Range };
type Pred = { path: PathExpr; text: string };
type Filter = { expr: FilterForm | null; vars: string[]; text: string; range: Range };
type Locked = { kind: string; vars: string[]; text: string; range: Range };
```

A triple written with `;` or `,` has its own entry and the ranges of its own predicate and
object, so a canvas edit can change one object of a list without rewriting the
statement. The module is the same one the editor already loads for **Format**, so the
builder adds no download. **Format** falls back to `POST /$/format` when the module does
not load, and the builder has no such fallback in this spec. Without the module, the pane
says that the builder needs it and stays closed. A server route for the same answer is
open question 4.

## 4. Design

### 4.1 The editable subset

The canvas edits these constructs, and only these.

| Construct | Edited on the canvas | Notes |
|---|---|---|
| Query form | `SELECT` and `ASK` | A `CONSTRUCT` or `DESCRIBE` query's `WHERE` group is edited, and its template or terms are a locked block. |
| Projection | Plain variables, `*`, `DISTINCT` | A projection item with an expression locks the projection. The canvas can still add plain variables to it. |
| Triple pattern | Subject and object as a variable, IRI, prefixed name or labelled blank node. Object also as a literal. | `[ … ]` property lists, collections and RDF 1.2 triple terms, reifiers and annotations are locked. |
| Predicate | An IRI, `a`, a variable, or a simple path | A simple path is a sequence or alternative of IRIs, `^p`, `p+`, `p*`, `p?` and `p{n,m}`. Negated sets and nested groups lock the triple. |
| `FILTER` | The forms of §2.2 over one variable and constants | Any other expression is a locked chip. It stays where it is in the text. |
| `OPTIONAL` | One level, holding triples and filters of the subset | An `OPTIONAL` inside an `OPTIONAL`, or one holding a locked block, is locked as a whole. |
| `UNION` (Phase 3) | Branches of groups in the subset | Each branch is a tinted lane. A union nested in a union is locked. |
| `GRAPH` (Phase 3) | A graph IRI or variable around groups in the subset | Drawn as a labelled hull. |
| `VALUES` inline (Phase 3) | One variable with up to 1,000 terms | U11's lasso writes this form. Several variables lock the block. |
| `LIMIT`, `OFFSET` | Numbers | |

Everything else is locked. That covers `BIND`, `MINUS`, `EXISTS` and `NOT EXISTS`,
subqueries, `SERVICE`, `LATERAL`, aggregates, `GROUP BY`, `HAVING`, `ORDER BY`, a
trailing `VALUES`, `FROM` clauses and every construct of a newer SPARQL draft that the
parser accepts and this table does not list.

The classification is done in the wasm module, from the syntax tree, so that the browser
and the Rust tests agree on it.

### 4.2 Locked blocks

A locked block is a range of the text that the canvas never writes into. Its rules are
these.

1. Its text, including comments and whitespace inside its range, is never changed by a
   canvas edit, with one exception. A variable rename the person confirms for that block
   replaces the variable's tokens inside it, and nothing else.
2. A canvas edit never moves a locked block, because order matters in SPARQL. A `BIND`
   after a triple and before it mean different queries. New triples go after the last
   editable item of their group and before any locked block that follows it only when
   the locked block does not use a variable the new triple binds. Otherwise they go after
   the block.
3. Deleting a locked block from the canvas deletes its range after a confirmation that
   shows its text.
4. A canvas edit that would need to change a locked block, such as removing the last
   triple that binds a variable the block reads, is allowed. The block gets a warning mark
   "uses ?y, which nothing binds now".

### 4.3 Edits as splices

Every canvas action is a function from the current `PatternAnswer` and the action to a
list of text changes `{from, to, insert}`. It is applied as one CodeMirror transaction
with the user event `input.pattern`, so the editor's own history undoes it in one step,
and the canvas redraws from the new text like any other change. The canvas keeps no
model of its own beyond the layout of §4.4 and the waiting nodes of §2.2, and the text is
the only source of truth.

Inserted text is formatted with X02's rules for the fragment alone, at the indentation
of the line it joins. The rest of the query is not reformatted, so a person's own layout
survives. **Format** stays a separate action.

The round trip has three properties, which the tests check (§7).

- **R1, totality.** Every text that parses as a query has a pattern, even if all of it is
  locked.
- **R2, locality.** An edit changes only the bytes inside the ranges its action names,
  plus the inserted fragment and at most one added `PREFIX` line.
- **R3, agreement.** Parsing the edited text gives the pattern the action describes. For
  a triple added to the canvas, the new pattern has exactly one more triple, with the
  chosen terms, and every other element is unchanged.

### 4.4 Identity and stable layout

Positions live in a layout map from an element's key to its coordinates. The keys are
these.

| Element | Key |
|---|---|
| A variable | Its name. |
| An IRI | The full IRI. One IRI used twice is one node. |
| A literal | The key of its subject, its predicate and its occurrence index, because two equal literals in different triples are different nodes. |
| A blank node label | The label. |
| A locked block | Its kind, a hash of its text with whitespace normalized, and its index among blocks with that hash. |
| A filter chip, a hull | Derived from the elements they belong to, so they need no position. |

After each parse, a node whose key is in the map keeps its position. A renamed variable
keeps its position in two ways. A rename made on the canvas moves the map entry. A rename
typed in the editor is matched when exactly one variable key disappeared and one
appeared and the two have the same incident edges, compared by predicate and the keys of
the other ends. New nodes are placed by an fcose run with every known node fixed, seeded
next to their first placed neighbour, as Explore's incremental layout already does, so a
known node never moves on its own. **Tidy** in the pane's menu runs a full layout and
replaces the map.

The map is kept in memory for the editor's tab and in `localStorage` keyed by a hash of
the query's text with whitespace removed, at most 200 entries, oldest dropped first. A
stored query's layout is not saved on the server in this spec (§10).

### 4.5 Patterns of queries that ran

The **Pattern** tab of §2.4 parses the text of the request that produced the result,
which the query page already keeps with the result for the time slider. It uses the same
layout map, so a pattern drawn from the editor and from its result look the same.

### 4.6 Caps and budgets

- **Drawing.** The canvas draws at most 300 triple patterns, 100 locked blocks and 400
  nodes. Above that it offers the list of §2.7, which shows the same elements as rows.
- **Parse.** Text up to 256 KiB is parsed on every change after the debounce. The parse
  runs in the main thread, because the formatter's parse of a 200-pattern query takes
  well under the 100 ms goal. If a measurement in Phase 1 shows otherwise, the parse
  moves to a worker.
- **Schema.** The picker holds at most 10,000 classes and 10,000 predicates in memory.
  Profiles are fetched one class at a time, at most 50 cached per commit.
- **Count.** One count in flight, cancelled by the next, with the five-second timeout and
  the caller's ordinary budgets.

Nothing in the builder reads data beyond the schema routes and the queries a person runs.

### 4.7 Branches and past states

The pane reads the schema at the query page's `branch` and `at`, the state the query
will run against. Moving the time slider changes the picker's counts and the profile
suggestions. The pattern itself does not depend on the state, so the layout stays.
**Count matches** runs at the same `at` and branch as the editor's query.

## 5. Architecture

| Part | Where |
|---|---|
| `pattern` export, subset classification | `crates/sparkles-fmt-wasm`, with the classification in `sparkles-fmt` so Rust tests reach it |
| Pattern answer types, splices, keys | `ui/src/lib/pattern.ts` (new) |
| Layout map | `ui/src/lib/pattern-layout.ts` (new) |
| Canvas | `ui/src/lib/components/PatternCanvas.svelte` (new), on Cytoscape as `GraphView.svelte` is |
| Element panels and pickers | `ui/src/lib/components/PatternPanel.svelte`, shared with U03 when U03 Phase 1 lands |
| Editor wiring | `SparqlEditor.svelte` exposes its view so the pane can dispatch transactions |
| Result tab | `routes/query/+page.svelte` |
| Hand-off from Explore | U03's **Open in editor** opens the pane with its pattern |

U03's `lib/qbe.ts` builds text from a selection. Whichever of U03 Phase 1 and this spec's
Phase 2 lands first defines the shared filter forms and variable naming, and the other
reuses them.

## 6. Phases

1. **Phase 1.** The `pattern` export, the read-only pane and result tab, locked blocks,
   the layout map, the keyboard and list alternative, and the phone layout for viewing.
2. **Phase 2.** Canvas edits as splices, the schema picker, filters, optional parts,
   paths, projection and modifiers, the shared undo history, and the phone layout for
   editing.
3. **Phase 3.** `UNION`, `GRAPH` and inline `VALUES` in the subset, and **Build a query**
   on a class of the Explore overview, which opens the pane with `?x a C`. Phase 3 also
   adds `MINUS` and `FILTER NOT EXISTS` over a basic graph pattern to the subset, drawn as
   red-tinted hulls, as open question 2 decided.

## 7. Acceptance examples

The data is U03 §7's.

- **A1. Draw.** The query of U03 A3 draws three nodes, `?person`, `?org` and `ex:lyon`,
  and three edges, with classes as subtitles. Its list alternative has the same rows.
- **A2. Locked block.** A query with `BIND(YEAR(NOW()) AS ?y) # year` draws a locked
  block with that text, comment included, joined to `?y`.
- **A3. Byte-identical round trip.** For every positive syntax test of the W3C SPARQL 1.1
  and 1.2 query suites, `pattern` answers `ok: true`, and applying the empty edit leaves
  the text byte-identical.
- **A4. Locality.** Starting from A2's query, twenty canvas edits drawn from a seeded
  generator leave the locked block's text identical, and every edit's change lies inside
  the ranges R2 allows.
- **A5. Agreement.** For each kind of edit in §2.2, parsing the result gives the expected
  pattern, over the queries of A3 that are entirely in the subset.
- **A6. Optional.** Marking `ex:locatedIn` optional in A1 produces exactly this change,
  with the rest of the text unchanged.

  ```sparql
  OPTIONAL { ?org ex:locatedIn ex:lyon . }
  ```
- **A7. Layout under typing.** Typing a new triple `?person ex:age ?age .` in the editor
  leaves every existing node within 1 px of its position and places `?age` next to
  `?person`.
- **A8. Rename.** Renaming `?org` to `?company` in the editor keeps the node's position.
- **A9. Undo.** After A6, `Mod+Z` in the editor restores the text before the edit, and the
  canvas redraws without the hull.
- **A10. Speed.** For a generated query of 200 triple patterns, the time from a keystroke
  to a settled redraw, after the debounce, is under 100 ms at the median of 50 runs in the
  Playwright performance project, on the reference laptop of U06 A2.
- **A11. Keyboard.** A1's query can be built from an empty editor with the keyboard alone,
  and the live region announces each edit.
- **A12. Phone.** At 390 px, A1's query can be built with taps alone, and no element needs
  horizontal page scrolling.
- **A13. Limited reader.** For a reader whose grants hide the graph that holds every
  `ex:worksFor` triple, the predicate picker does not list `ex:worksFor`, and a typed
  `ex:worksFor` is accepted without a count.

## 8. Testing

- **Rust.** In `sparkles-fmt`, the subset classification for each construct of §4.1, the
  ranges of `;` and `,` abbreviations, and A3 over the W3C suites already vendored for
  the formatter's tests.
- **Vitest.** `lib/pattern.ts`, for each splice of §2.2, R2 and R3 on seeded random edit
  sequences, the placement rule of §4.2 for new triples beside locked blocks, prefix
  insertion, and the keys and rename matching of §4.4. `lib/pattern-layout.ts`, for
  storage bounds and restore.
- **Router.** No new route. The existing schema route tests already cover limited views,
  and A13 adds a case that the picker's request is one they cover.
- **Playwright against the mock server.** A1, A2, A6 to A9, A11 and A12, and the error
  states of §2.7. The mock serves schema and profile answers for the fixture dataset.
- **Playwright against a real server.** A1, A6 and A13, and a count.
- **Performance.** A10 in the separate performance project of U06, recorded rather than
  gating ordinary CI.

## 9. Rejected alternatives

- **Regenerating the query from a model on every canvas edit.** It is simpler, and it
  loses comments, formatting and any construct the model lacks. Splices keep them.
- **A JavaScript SPARQL parser such as sparqljs.** It builds an algebra-like object
  without trivia or full ranges, and a second parser would disagree with the server's in
  corner cases. The formatter's parser is already in the browser and is lossless.
- **Positions in a comment inside the query.** Copying the query would carry layout noise
  into other tools, and a person editing the comment could break it.
- **Editing locked constructs through forms.** Each would need its own editor and its own
  round-trip rules. The text editor already edits them, and the canvas shows where they
  sit.

## 10. Open questions

1. Whether a stored query should keep its pattern layout on the server, as a `layout`
   field of the C16 definition, so a shared query opens with the author's picture. U03
   asks the same question about keeping the pattern. **Decided on 2026-10-23:** layouts
   stay in the browser for now, as §4.4 describes, and C16 gets no `layout` field. This
   also answers U03's question 2.
2. Whether `MINUS` and `FILTER NOT EXISTS` over a basic graph pattern should join the
   subset as red-tinted hulls, since they are common in hand-written queries.
   **Decided on 2026-10-23:** they join the subset in Phase 3, not in Phases 1 or 2.
   Until then they are locked blocks and locked chips.
3. Whether the canvas should show estimated counts on edges before a run, from the schema
   report's predicate counts, or leave counts to U04 Phase 4.
4. Whether to add `POST /$/pattern`, the same answer from the server, for builds where
   the browser module is switched off with `VITE_FMT_WASM=off`.

## Outcome

Phase 1 shipped on 2026-10-23 under [ROADMAP](../ROADMAP.md) item 101. Phase 2's splice
core shipped with it, ahead of the rest of Phase 2, so that the round-trip properties and
the shared undo history could be tested on real edits. That core adds a triple, deletes a
triple and turns optional on or off for an edge. The rest of Phase 2 shipped later on
2026-10-23, and its section below describes it. Phase 3 is not built and stays under
item 101.

**The pattern export.** The classification is `sparkles_fmt::pattern`, which reads the
lossless tree of X02 and answers a `Pattern` with byte ranges. The browser module's third
export, `pattern`, writes it as JSON with UTF-16 ranges, converted through one table per
text. The answer follows §3.2 with these differences. Each item has a `type` (`triple`,
`filter`, `group` or `locked`). A group also has `inner`, the range between its braces.
A triple also has `stmt`, `entry` and `object`, which place it in its statement, and
`entryRange` and `objectRange`, the ranges of its property list entry with the `;` after
it and of its object with the `,` after it, which the splices of a `;` or `,` list need.
Terms carry `kind`, `text` and `value`, where `value` is a variable's name, a full IRI with
prefixed names expanded, a blank node label or a literal as written. `root` is null for
a `DESCRIBE` without a `WHERE` group, `projection` is null for the other forms and has a
`locked` flag, and an error answer has a `code`, which is `syntax`, `update` for an
update request, or `bad-request`. A locked block's range runs to the end of a comment on
the line of its last token, so A2's block keeps `# year`, and its `end` is the end of
that last token. A filter, a locked block and an `OPTIONAL` have a `dot`, the range of the
`.` that follows them in their group wherever it stands, even after a comment or a line
break. A triples statement holds its own `.`. The reader honours `BASE` in source order,
resolving IRIs and the namespaces of `PREFIX` lines against it, and the answer gives the
last one as `prologue.base`. A request with `strict: true` also runs the server's parser
on the text and answers `code: "syntax"` when it refuses, which catches what the lossless
parser accepts, such as a `BIND` to a variable already in scope.

**Classification.** It follows §4.1, with these choices where the table left room. A
statement with any part outside the subset is one locked block of kind `triples`, so a
statement mixes no editable and locked parts. An anonymous `[]` and `()` lock their
statement like other blank node lists and collections. A path is simple when it is an
IRI, `a`, one modifier on an IRI, an inverse of either, or a sequence or alternative of
plain IRIs. A sequence that holds an inverse locks the statement. A `FILTER` the canvas
does not understand is a locked chip but not a locked block, so an `OPTIONAL` holding one
stays editable, while an `OPTIONAL` holding any other element is locked whole. `REDUCED`
locks the projection, and a `LIMIT` or `OFFSET` beyond 2⁶⁴ is a locked modifier. A nested
group `{ … }` is a locked block of kind `group`, since `UNION` is Phase 3. A subquery
shares its projected variables, and the other locked blocks share every variable they
mention.

**The pane.** `ui/src/lib/pattern.ts` holds the answer's types, the drawing, the keys of
§4.4, the rename match and the splices. `pattern-layout.ts` holds the layout map.
`PatternCanvas.svelte` draws on Cytoscape, `PatternPanel.svelte` is an element's panel,
and `PatternPane.svelte` parses, draws and edits. The editor exposes `applyPattern`,
which dispatches an edit as one transaction with the user event `input.pattern` and an
isolated history entry, and `undo` and `redo`, which the pane calls for `Mod+Z` and
`Mod+Shift+Z`. The pane handles those keys and `Escape` anywhere inside it apart from the
fields of the add form, so they work in the list and in an element's panel as on the
canvas. An edit names its triple by the id of its edge, which is resolved again against
the text as it is when the edit is made, so typing between a selection and its edit never
moves the edit to another triple. An id that typing made ambiguous, because another copy
of the same triple changed and the copies were numbered again, does not count as found.
When the edge is gone or ambiguous the pane refuses with "The pattern changed, select the
edge again". Before a canvas edit is applied, the pane parses its result with
`strict: true` and checks R3 on that parse, comparing triples by their terms as written.
An added triple must be the only new one and carry the terms that were typed. A deleted
triple must be the only one gone, and an optional toggle must move just that triple in or
out of an `OPTIONAL`. A locked statement that ends in a comment and lacks its `.` gets the
`.` at the start of the new triple's line, since no byte of a locked range is written.
Typed IRIs are resolved against `BASE` by RFC 3986 without normalization, as the reader
does, and a `BASE` that is not absolute resolves nothing. The group of a new triple is
that of the first triple whose subject or object is written like the new subject or
resolves to the same term, and R3 also checks that the new triple landed on that group's
side of an `OPTIONAL`. A
prefixed name whose prefix neither the query nor the dataset declares is refused before
that, and a typed term is resolved under the query's prefixes and `BASE` to find the
group that holds its patterns. New nodes are placed by an fcose run with every known node
fixed, after which the known nodes are put back exactly where they were. The dotted link
from a locked block or a chip to a variable has half the ideal length of a triple's edge,
so a block sits next to the variables it uses. `docs/images/pattern.png`, which
`mise run docs:screenshots:mock` takes, shows the pane beside a query. A variable that
only a locked block mentions gets a node beside it, except for a `MINUS`, which binds
nothing outside itself, and such a node does not count as a disconnected part. `FROM`
clauses, a `CONSTRUCT` template and the terms of `DESCRIBE` are locked nodes too, joined
to the variables they use. An `OPTIONAL` whose nodes are all bound outside it has no hull,
and its edges stay dashed. A filter keeps its scope. It is a chip on its variable only
when the variable sits in the filter's own hull, or in none for a filter of the root, and
is otherwise a chip node inside the hull. The query page keeps a pane per editor tab, so
each tab keeps its own drawing and layout. The mock UI tests format through the mock's endpoint with `VITE_FMT_WASM=off`, so
the pane loads the module on its own glob, `VITE_PATTERN_WASM=off` leaves it out, and
`mise run ui:e2e:mock` builds it first.

**Differences from the acceptance examples.** A1 draws three nodes and two edges, not
three edges, because the two `a` triples are the subtitles of §2.1. A history entry and a
stored query have no **Show pattern**, because the query page has no list of past runs
and a stored query opens in an editor tab, where the pane draws it. Phase 2 met A11, A12
and A13, as its section says.

**Phase 2.** The rest of Phase 2 shipped on 2026-10-23. `sparkles-fmt` gives the edits
the ranges they need. The projection has its keyword, the range of `DISTINCT` and of `*`,
and an item per variable or expression. Filters and locked blocks have the ranges of
their variable references, a locked block lists the variables it binds, and the answer
gives where the query form starts, after the prologue. `ui/src/lib/pattern-edits.ts`
holds the edits of §2.2 beyond the splice core, each a list of changes checked against
R3 on the strict parse of its result, as the core's are. A predicate change replaces
the predicate token. When the triple shares its predicate with others in a `,` list, it
moves out into a new `;` entry, so the others keep theirs. A filter is appended after the
last triple of its group that binds the variable, as one `FILTER(…)` item. A rename
replaces the variable's tokens in triples, understood filters and the projection, and in
the locked blocks, modifiers and projection expressions only after the person confirms
in a dialog that shows those blocks. A rename to a name already in the query is refused,
since it would merge two variables. Deleting a node deletes its triples one at a time,
parsing again between them, and composes the steps with `ChangeSet.compose` into one
change. It also deletes each understood filter whose variables the deleted triples bound
and that nothing binds any more. A locked block of the `WHERE` group goes after a
confirmation that shows its text. Blocks outside it, such as `FROM` or a `CONSTRUCT`
template, are not offered for deletion. The first projection change writes the
variables of `SELECT *` out in the order they first appear, removing the last projected
variable is refused because a query projects at least one, and a new `LIMIT` or `OFFSET`
goes after the group and the other modifiers.

`pattern-schema.ts` reads the pickers' terms. It reads the first page of
`/$/schema/{ds}`, with 1,000 classes and 1,000 predicates, once per dataset, branch and
**At** state, and further pages when the person asks for more from the schema, keeping
at most 10,000 of each. Profiles are read one class at a time and kept per commit, at
most 50. Predicates are ranked by the source class's profile, then by the target
class's `incoming`, then by their triples. Variable names follow U03 §4.2, so
`ex:Person` gives `?person`, a clash gets a number and the names stored queries reserve
are skipped. `pattern-filters.ts` holds the filter forms. The forms a variable offers
come from the datatypes the profiles of its subjects' classes saw for the predicate, and
every form writes an expression the reader understands, so its chip opens again in the
same form, which a test checks for each form. `PatternPicker.svelte` is the combobox of
the pickers, in the WAI-ARIA pattern. A schema read that fails leaves the pickers empty
with a note that says why, and typed names still work.

A waiting node lives in the pane only, since it is not text. It is not kept across a
reload, and a pane per editor tab means each tab has its own. An edge whose end is a
waiting class node writes `?x a C` and the edge in one change, and an empty editor is
edited as `SELECT * WHERE { }`, so the first edge writes the whole query. A node with no
edges is placed beside the drawing rather than where fcose's gravity would put it, and
the view fits when a new node lands outside it. Cytoscape now measures its container
again when the container's box changes, because a bar or form opening above the canvas
moved it and taps landed beside their nodes. The warning mark of §4.2 rule 4 is a red
border and a subtitle on the block, which the screen reader's description of the block
includes. **Count matches** sends the query of §2.2 with the prologue and `FROM` clauses
of the text, `timeout=5` and the page's **At** state, and a `408` shows "More than the
count could finish". A count belongs to the text it ran on and clears when the text
changes. On a phone the strip folds into a **Query options** sheet, **+ Edge** is the
two-tap mode of §2.6, and the node and edge forms fill the screen.

A11 builds A1's shape from an empty editor with `N`, `E`, `Tab` and `F`, with each edit
announced. A12 builds a query at 390 px with taps on the empty state's classes, the node
form and two canvas taps, with no sideways scrolling. A13 runs against a real server
with a reader whose view hides `ex:worksFor`. Graph grants can hide only whole graphs,
and the queries read the default graph, so the test hides the predicate with a protection
of triples (C12b), which every schema read applies as a graph grant does. The picker
does not list `ex:worksFor`, a typed `ex:worksFor` is accepted, and the count under the
reader's view is 0. The screenshot pipeline takes `docs/images/pattern-edit.png`, which
shows **+ Edge** with the ranked predicate picker, and `pattern.png` now shows the strip's
checkboxes and a count.

Vitest checks every Phase 2 edit's exact text, the refusals, and an A5 run over seeded
edits of every kind, asserting R3 on each applied edit. It also checks the schema source's
paging and caching, predicate ranking, variable names, the filter forms and their
constants. `ui/tests/mock/pattern-edit.spec.ts` covers A11, A12, filters by datatype with
chip editing, predicate and path changes, the rename and locked-block confirmations, the
warning mark, node deletion with its filters, the strip, the count and its timeout, and
the schema failure note. `ui/tests/e2e/pattern.spec.ts` covers A1, A6, a count and A13
on a real server. The mock server's class profiles now count literal datatypes and
incoming predicates, as the real route does.

**Tests.** In Rust, `crates/sparkles-fmt/tests/pattern.rs` checks the classification of
each construct, the ranges of `;` and `,` lists, paths, modifiers, the other query forms,
errors and byte ranges after multi-byte characters. Its corpus test runs A3 over the
golden inputs and every positive W3C SPARQL 1.0, 1.1 and 1.2 query the formatter's tests
read. All 993 queries have a pattern, with 893 editable triples and 448 locked blocks
between them, every locked block's text is the source at its range, and the items of
every group cover each of its significant tokens apart from the `.` between items. The
module's own tests check the JSON and the UTF-16 ranges, and
`tests/pattern_fixtures.rs` keeps `ui/src/lib/pattern-fixtures.json` equal to the
module's answers. Vitest checks the drawing, the keys, the rename match, every splice
with its exact text, the placement rule of §4.2 beside locked blocks, prefix insertion,
CRLF text, and that each result parses with the `oxigraph` package. With the built
module present, it runs A4 as twenty seeded edits from each of five seeds on A2's query,
checking R2, that the locked blocks stay byte-identical and that the only deletion is the
range the edit names, and R3, that the parse agrees with the edit. The layout map's tests
cover storage bounds, restore and renames. `ui/tests/mock/pattern.spec.ts` covers A1, A2,
A6 to A9, A11, locked blocks through a sequence of canvas edits, the error states of
§2.7, the result's Pattern tab, and the 390 px layout. The three code reviews found 14,
6 and 2 problems, and each fix has a regression test in one of these suites.

**Measurements.** `ui/tests/mock/pattern-perf.spec.ts` runs A10 when `PATTERN_PERF=1`
is set. On the development laptop, under a load average of about 20 from other builds
and with the UI served by `vite dev`, a query of 201 triple patterns redrew after a
keystroke in a median of 24 ms after the debounce over 50 runs when the keystroke changed
a literal, and 47 ms when it changed a variable, so that a node went and a new one was
placed by fcose. The slowest run took 120 ms. The parse in the module took a median of
9.4 ms and building the drawing 1.3 ms, so the parse stays on the main thread as §4.6
expected. These are not the reference laptop of U06 A2, and the U06 performance project
does not exist yet, so A10 is recorded here rather than run in CI. The `pattern` export
adds 40 KB to the browser module, from 1,119 KB to 1,159 KB, and 10 KB once compressed
with brotli, from 293 KB to 303 KB. The editor already loads the module for **Format**,
so the pane adds no download of its own.
