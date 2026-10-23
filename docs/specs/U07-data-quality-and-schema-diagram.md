# U07: A data-quality timeline and a schema diagram

> **Status:** Phase 4 shipped on 2026-10-23
>
> **Phases:** Phase 4 shipped on 2026-10-23, and Phases 1 to 3 are not built. Phase 1 records a validation summary for every commit a
> write-time guard validates, and draws it as a timeline on the dataset page with links
> to the commits that introduced each result. Phase 2 backfills the timeline for past
> commits and finds the introducing commit of results older than the record. Phase 3
> adds the schema diagram with observed and declared cardinalities, and its export. Phase
> 4, added on 2026-10-23 in §11, badges SHACL and ShEx results on the nodes of the Explore
> graph, shows the failing shape and triples, and suggests a fix query.
>
> **User docs:** [USAGE.md](../USAGE.md#the-validation-overlay) for the overlay, and
> [API.md](../API.md#shacl-validation) for the routes' focus sets and stored shapes.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

This design was written from the W3C SHACL Recommendation (validation reports and
results), the ShEx 2.1 specification and the ShapeMap specification (result shape maps),
the UML 2.5 specification's notation for multiplicities, the Mermaid documentation of
class diagrams (for the text export), and the Sparkles code and specs: write-time
validation and its grandfather baseline ([C10](C10-write-time-validation.md)), ShEx
([G02](G02-shex.md)), schema discovery, class profiles, the constraints layer and schema
diffs ([C02](C02-schema-discovery.md)), point-in-time reads and history
([F06](F06-snapshots-and-point-in-time.md)), branches ([F09](F09-branches-and-merges.md))
and the time slider ([U01](U01-time-slider.md)).

## 1. Summary, goals, non-goals

The dataset page's **Write-time validation** panel shows the result counts of the head
(the guard's `baseline`, by severity, at its commit), the last validated write and the
last ten rejected writes. The **Validate** panel runs SHACL or ShEx on demand, and
`/{ds}/shacl` accepts `at=` for a past state. Nothing records how the
results changed over time, so a person cannot see when warnings started to grow, or which
commit introduced a given violation.

The schema browser lists classes and predicates with observed counts, and the class panel
shows each class's profile, including how many values each instance has. There is no
picture of how classes connect, and the observed cardinalities are not compared with
the declared ones in one place.

This spec adds both: a timeline of validation results per commit with links to the
commits that introduced each current result, and a schema diagram drawn from class
profiles with observed and declared cardinalities.

**Goals**

1. Every commit a guard validates records a small summary: counts by severity, by shape
   and by constraint component, and the result keys it introduced and resolved.
2. The timeline shows those counts over commits, per branch, and each point opens the
   commit in the History panel and the state in U01's slider.
3. Each current result names the commit that introduced it, from the record or, for
   older results, by a search over readable states.
4. Past commits can be validated after the fact, so a guard set up today shows history.
5. A schema diagram of classes and the predicates between them, with observed
   multiplicities next to declared ones and mismatches marked.

**Non-goals**

- Changing how the guard decides. C10's decision rule and baselines stay as they are.
- Storing full reports for every commit. Summaries and result keys are kept, and a full
  report of any readable state is one `at=` validation away.
- Data-quality metrics beyond SHACL and ShEx results, such as completeness scores. They
  can be expressed as shapes.
- An ontology editor. The diagram is read-only.

## 2. User-visible behaviour

### 2.1 The quality timeline

The dataset page gets a **Quality** card beside **Write-time validation**, shown when the
dataset has a guard or a backfilled record.

```
┌─ Quality · main ▾ ─────────────────────────────────── SHACL · warn mode ──────────┐
│ violations ▁▁▁▁▂▂▂▂▂▂▅▅▅▅▅▅▅▃▃▃  32 now                                           │
│ warnings   ▃▃▃▄▄▄▄▄▅▅▅▅▅▅▅▆▆▆▆▆  210 now                                          │
│            commit 120 ───────────────────────────────────── 318                  │
│ ▲ commit 241 · upload by etl · +27 violations (ex:PersonShape, sh:minCount)       │
├───────────────────────────────────────────────────────────────────────────────────┤
│ Current results by shape            introduced in                                │
│ ex:PersonShape  sh:minCount   ex:name   19   commit 241 (upload, etl)  [diff]    │
│ ex:OrgShape     sh:class      ex:parent  8   commit 288 (update, ana)  [diff]    │
│                                              5 older than the record [Find]       │
└───────────────────────────────────────────────────────────────────────────────────┘
```

- **Series.** One step series per severity (SHACL) or one for nonconformant associations
  (ShEx), over commit numbers, with a time axis on request. Hovering or focusing a point
  shows the commit's kind, author, message and change in counts. Gaps mark commits that
  were not validated, such as bypassed writes (`baseline` unknown).
- **Jumps.** Commits whose introduced results exceed 10 or 10% of the total are marked
  `▲` and listed under the chart.
- **Current results.** Grouped by shape, constraint component and path, with the count
  and the commit that introduced them. **diff** opens the History panel at that commit.
  A group opens into its results with focus nodes, which open in Explore.
- **Older results.** Results already present when the record began are counted as
  "older than the record". **Find** (Phase 2) searches for their introducing commits
  (§4.3).
- **Range.** The card shows the last 500 validated commits, and a range control widens
  it. U01's slider, when open on the dataset page, moves a cursor on the chart.

### 2.2 The schema diagram

The schema tab in Explore gets a **List | Diagram** switch (Phase 3).

```
┌───────────────────────┐   ex:worksFor  1..1 ▸ (observed 1..1 · 98%)  ┌──────────────┐
│ ex:Person    12,402   │──────────────────────────────────────────────▶│ ex:Org   840 │
│ ex:name   1..1 ✓      │   declared sh:minCount 1, sh:maxCount 1       │ ex:name 1..1 │
│ ex:email  0..3 ⚠ max 1│                                               └──────────────┘
└───────────────────────┘
```

- **Nodes.** Classes with instances, sized by count, with their literal-valued
  predicates listed inside (the eight used by the most instances, then "and N more").
- **Edges.** A predicate whose values include instances of another class is an edge
  between the classes, labelled with the predicate and its multiplicity.
- **Multiplicities.** The observed multiplicity is `min..max` values per instance, where
  `min` is 0 when some instances have no value, from class profiles
  (`minPerInstance`, `maxPerInstance`, `instances` with a value against the class's
  instances). The share of instances with a value is shown as a percentage. The declared
  multiplicity comes from the constraints layer (`sh:minCount`, `sh:maxCount`) and from
  `owl:FunctionalProperty`. Both are shown, labelled "observed" and "declared", as the
  schema browser already keeps them apart (C02 §2.4).
- **Mismatches.** Where observed data breaks a declared bound, the label gets `⚠` and the
  text "max 1 declared, 3 observed". Where they agree, `✓`.
- **Filters.** Hide built-in classes, hide classes under N instances, show one class and
  its neighbours to a depth of one or two.
- **Export.** PNG of the drawing (Cytoscape's image export), and Mermaid class diagram
  text, which renders in Markdown viewers that support Mermaid.
- **Time.** The diagram reads the profile at the schema tab's state, so U01's slider moves
  it through commits.

### 2.3 Keyboard and accessibility

The timeline is an SVG with a `role="img"` summary ("violations rose from 3 to 32
between commits 120 and 318, the largest jump at commit 241") and a **Table** toggle that
lists the points. Points are focusable in order with arrow keys and announce their
details. The diagram has a list alternative, which is the existing schema list, and its
elements are reachable with `Tab` in size order, with `Enter` opening the class panel.
Mismatch marks are text and symbols as well as colour.

### 2.4 Empty and error states

| Case | What the card or diagram shows |
|---|---|
| No guard and no record | The Quality card is absent. The Write-time validation panel offers **Backfill** (Phase 2) next to its setup. |
| A guard set up recently | The timeline starts at the guard's first commit, with "older commits not validated" and **Backfill**. |
| A branch | The record is per branch. A branch shows its own commits after its fork and its parent's record before it. |
| A caller limited to some graphs | The card is shown only to callers who can read the guard's data graphs in full, because result keys and counts would reveal hidden data. |
| A class profile over the budget | The diagram shows the `408` or `413` message of the schema routes. |

## 3. Server and API changes

### 3.1 The validation record (Phase 1)

After a guard validates a write that commits, it appends one entry to the branch's
validation record, `validation-log.jsonl` in the database directory, next to C10's
`validation-status.json`:

```ts
type QualityEntry = {
  commit: number; language: "shacl" | "shex"; strategy: "incremental" | "full";
  conforms: boolean; bySeverity: { violation: number; warning: number; info: number };
  byGroup: { shape: string; component: string; path: string | null; count: number }[];  // top 200
  introduced: { key: string; group: number }[];   // up to 1,000 result keys
  resolved: { key: string; group: number }[];     // up to 1,000
  truncated: boolean;
};
```

A result key is a 128-bit hash of the fields C10's grandfather mode already matches on:
for SHACL the focus node, path, value, source shape, component and constraint; for ShEx
the node and shape. Computing `introduced` and `resolved` is the comparison the
grandfather baseline performs, and in strict mode the guard runs the same comparison
when the record is on. The record is kept with the branch, copied by clones and backups
as the other validation files are, and trimmed with the retention window, keeping at
least the entries of the last 10,000 commits. A bypassed write (`validate=false`) records
a gap entry.

### 3.2 Routes

| Method | Path | Needs | Description |
|---|---|---|---|
| GET | `/$/validation/{ds}/timeline` | `read` on the guard's data graphs | Entries between `from` and `to` (selectors), at most `limit` (default 500, at most 10,000), without `introduced` and `resolved` unless `keys=true`. `branch=` applies. |
| GET | `/$/validation/{ds}/results` | the same | The current results grouped as in §2.1, each group with `introducedIn` counts by commit and `older` for results present before the record. `at=` reads at a recorded commit. |
| POST | `/$/validation/{ds}/backfill` | `admin` | Phase 2. Validates past readable commits with the current configuration and writes entries marked `backfilled`. Body `{from?, to?, every?: number}`, where `every` validates one commit in N. Runs as a cancellable task with progress. |
| POST | `/$/validation/{ds}/introduced` | `read` | Phase 2. Finds the introducing commit of up to 100 result keys or of one group, by the search of §4.3. Runs as a task when it needs more than 10 validations. |

All routes are added to `docs/API.md`, the route table and the OpenAPI description.
Schema diagram data needs no new route: `GET /$/schema/{ds}/profiles` and the
constraints layer of `GET /$/schema/{ds}` already return everything §2.2 draws.

## 4. Design

### 4.1 Cost of recording

The guard already holds the results of each write it validates. Recording adds the
grouping, the hashing of introduced and resolved results, and one appended line,
written before the commit is acknowledged and fsynced with the commit's own sync. In
strict mode the comparison of introduced results costs what the grandfather mode costs
today, and the record can be switched off per dataset (`record: false` in the validation
configuration) where that matters.

### 4.2 Backfill

A backfill validates past states read with `at=`, oldest first, so each entry's
`introduced` and `resolved` compare with the previous validated state. It uses the full
validator, not the incremental one, because the guard's incremental state belongs to the
head. Each validation counts against the dataset's task limits and the history cache.
With `every: N`, intermediate commits are not validated, and the entries say which range
each comparison covers.

### 4.3 Finding introducing commits

For a result key present at head and absent from the record's start, the server
validates only the focus node of the result (`sh:targetNode` style, or a fixed ShEx
shape map entry) at readable commits, by binary search over the range where the result
appears. Each step is one point-in-time validation of one focus node. The search assumes
a result appears once and stays; when a step finds it absent after a commit where it was
present, the server falls back to a linear scan of that stretch, so a result that came
and went is attributed to its last appearance. Commits outside the readable range are
reported as "before commit N".

### 4.4 The diagram's layout

The diagram uses Cytoscape and fcose, as `GraphView` does, with class nodes as rounded
rectangles whose label lists their properties, and edges labelled with multiplicities.
`lib/schema-diagram.ts` turns `ClassProfiles` and the constraints layer into nodes,
edges and labels, and computes the mismatches. Self-links (a class whose instances point
to instances of the same class) are drawn as loops.

## 5. Architecture

| Part | Where |
|---|---|
| Record, grouping, key hashing | the write-time guard in `sparkles-server/src/write_validation.rs` and `validation_common.rs`, sharing the grandfather comparison |
| Routes, backfill and search tasks | `sparkles-server` validation handlers and the task system |
| Quality card | `ui/src/lib/components/QualityCard.svelte` (new), `ui/src/lib/quality.ts` (new) |
| Dataset page | `routes/datasets/[name]/+page.svelte`; a `commit=` URL parameter that opens the History panel at a commit (new) |
| Diagram | `ui/src/lib/components/SchemaDiagram.svelte` (new), `ui/src/lib/schema-diagram.ts` (new), using `api.schemaProfiles` and the existing `constraintLines` and `cardinalityChips` of `lib/explore.ts` |
| Schema tab switch | `routes/explore/+page.svelte` |

## 6. Phases

1. **Phase 1.** The record for SHACL and ShEx guards, the timeline and results routes,
   the Quality card, introducing commits from the record, and the `commit=` link.
2. **Phase 2.** Backfill and the search for introducing commits of older results.
3. **Phase 3.** The schema diagram, its filters and its exports.
4. **Phase 4.** Focus sets and stored shapes in the validation routes, then the validation
   overlay on the graph, with suggested fixes (§11). It needs U10 Phase 1. It does not need
   Phases 1 to 3 here, and uses Phase 1's record when it exists.

## 7. Acceptance examples

A dataset `q` has a SHACL guard in `warn` mode with
`ex:PersonShape sh:property [ sh:path ex:name ; sh:minCount 1 ]`.

- **A1. Record.** Commit 2 adds three people without names. `GET
  /$/validation/q/timeline` has an entry for commit 2 with 3 violations and 3 introduced
  keys in the group `(ex:PersonShape, sh:MinCountConstraintComponent, ex:name)`.
- **A2. Resolution.** Commit 3 adds a name to one of them. Its entry has 2 violations and
  1 resolved key.
- **A3. Introduced in.** `GET /$/validation/q/results` lists the group with 2 results
  introduced in commit 2. In the UI, **diff** on the group opens the History panel with
  commit 2 selected and its diff shown.
- **A4. Timeline.** The Quality card draws violations 0, 3, 2 for commits 1–3 and marks
  commit 2 with `▲`. The table alternative lists the same values.
- **A5. Bypass.** A write with `validate=false` (where allowed) appears as a gap, and the
  next validated write's entry compares with the last validated state.
- **A6. Backfill (Phase 2).** On a dataset whose guard was installed at commit 10, a
  backfill of commits 1–9 adds entries marked `backfilled`, and the timeline starts at
  commit 1.
- **A7. Find (Phase 2).** For a result present at commit 10 and introduced at commit 4 by
  the data, **Find** reports commit 4 after at most five focus-node validations.
- **A8. Diagram (Phase 3).** With 3 people, 2 of them with one `ex:worksFor` an `ex:Org`
  and one with two, and a shape declaring `sh:maxCount 1` on `ex:worksFor`, the diagram
  draws `ex:Person` → `ex:Org` labelled `ex:worksFor`, observed `0..2` with 67% having a
  value, declared `0..1`, and a `⚠` with "max 1 declared, 2 observed".
- **A9. Mermaid.** A8's export contains `Person --> Org : worksFor` and a note of both
  multiplicities, and renders in the Mermaid live editor without errors.

## 8. Testing

- **Rust.** Record entries for incremental and full validation, strict and grandfather
  baselines, SHACL and ShEx; key stability across restarts; trimming with retention;
  branch records; backfill against `at=` validations; the search for introducing commits
  on constructed histories, including a result that disappears and returns.
- **Vitest.** `lib/quality.ts`: series with gaps, jump detection, grouping.
  `lib/schema-diagram.ts`: multiplicities from profiles, mismatches against the
  constraints layer, Mermaid text.
- **Playwright.** A3, A4 and A8 against a real server, building the history in the test
  as `write-validation.spec.ts` does, and the error states against the mock server.

## 9. Rejected alternatives

- **Storing full reports per commit.** Reports can hold thousands of results per commit,
  and any readable state can be validated again on demand. Keys and counts answer
  "when did this start" at a fraction of the size.
- **Validating every commit after the fact instead of recording.** It costs a full
  validation per commit, and states beyond retention cannot be read. Recording reuses the
  work the guard already does.
- **A dedicated diagram library with a layered layout.** Class graphs are small, the UI
  already ships Cytoscape and fcose, and one graph library is easier to theme and test.
- **Mixing observed and declared multiplicities into one figure.** C02 keeps
  measurements and constraints apart, so a reader never mistakes the data's current shape
  for a rule. The diagram keeps that rule.

## 10. Open questions

1. Whether the record should also be written when no guard is configured, by running
   validation in the background after commits (a "watch" mode), which would cost a
   validation per commit outside the write path.
2. Whether result keys should include the value for `sh:minCount` results, where the value
   is absent and the key would collapse to focus node and path.
3. Whether the diagram should show subclass edges from the declared hierarchy, which the
   class tree already shows.

## 11. Phase 4: a validation overlay on the graph (2026-10-23)

This section was added on 2026-10-23, when the user approved a set of graph-interaction
features. It was written from the same sources as the rest of this spec, together with
the SHACL and ShEx routes as built, the write previews of
[C15](C15-write-previews.md), and the lens model of [U10](U10-canvas-lenses.md).

### 11.1 Goals and non-goals

The timeline of Phase 1 says when results appeared, and the Validate panel lists them as
rows. Neither shows where in the graph the problems are. Phase 4 badges SHACL and ShEx
results on the nodes Explore draws, shows the failing shape and the triples involved, and
offers a query that would fix a result, for the person to review and run.

**Goals**

1. Every drawn node that is a focus node of a result gets a badge with its count and worst
   severity.
2. A badge opens the node's results with the shape, the constraint, the path, the value and
   the message, and the triples involved are highlighted on the canvas.
3. For the common constraint components, a suggested SPARQL Update that would resolve the
   result, opened in the editor and previewed with C15 before anything commits.
4. The overlay validates only the drawn nodes, so its cost follows the picture and not the
   dataset.

**Non-goals**

- Applying fixes automatically. A suggestion is text in the editor, and the person runs it.
- Fixes for every constraint. A component without a safe suggestion shows none.
- Validation for callers whose access covers part of a dataset. The validation routes
  refuse them today, and §11.5 keeps that rule.

### 11.2 User-visible behaviour

The badge channel of U10's lens menu offers **Validation**, with the source chosen
beside it: the dataset's write-time guard, a shapes graph of the dataset, or the shapes in
the Validate panel's editor. With a guard configured, the overlay is on by default in
Explore for callers who may use it.

```
 (ex:p17 ⚠2) ──ex:worksFor──▶ (ex:acme)        ┌─ ex:p17 · 2 violations ───────────────┐
      │                                          │ ex:PersonShape · sh:minCount 1        │
   ex:email ━━━▶ ("a@x" ✕)                       │   path ex:name · no value              │
                                                 │   [Suggest fix]                        │
                                                 │ ex:PersonShape · sh:pattern           │
                                                 │   path ex:email · value "a@x"          │
                                                 │   message "not an email address"       │
                                                 │   [Show triples]                       │
                                                 └───────────────────────────────────────┘
```

- **Badges.** A violation is `✕`, a warning `⚠` and an info `ⓘ`, with the count. The badge
  shows the worst severity. A literal node that is the value of a result gets the mark of
  its result, so a bad email is marked where it is drawn.
- **Results panel.** Clicking a badge, or pressing `V` on a focused node, opens the node's
  results grouped by shape and constraint. Each result names its source shape, its
  component, its path, its value and its messages, as `ShexResults` and the Validate panel
  already show them. A ShEx result shows its shape and its failures by kind.
- **Show triples.** The result's focus node, path and value are highlighted, and edges along
  the path that are not drawn are added as an overlay.
- **Suggest fix.** For the components of §11.4, the editor opens with a suggested update
  and a comment that names the result. **Preview** runs it as a C15 dry run, which reports
  the changes and whether the result would be resolved by the guard. The overlay never
  commits.
- **After a write.** When a commit changes the drawn nodes, the overlay validates them again,
  so a fix's badge disappears.

The badges are part of the node's accessible description, such as "ex:p17, 2 violations",
and the list view gains a **Validation** column. `V` opens the results, `Shift+V` moves
focus to the next node with a result. At 390 px the results panel is a bottom sheet, and
**Suggest fix** opens the query page in its text tab.

| Case | What the overlay shows |
|---|---|
| A caller whose access covers part of the dataset | **Validation** is absent from the lens menu, with "Validation needs access to every graph". |
| No guard and no shapes graph | The source picker offers the Validate panel's shapes only. |
| Shapes with SHACL-SPARQL constraints over the whole graph | The overlay validates the drawn nodes as focus nodes, which can differ from a full validation for constraints that are not anchored at the focus node, and the panel says so. |
| A validation over the budget | The route's `507`, and badges stay off. |

### 11.3 Server and API changes

The overlay needs to validate a set of nodes against the guard's shapes without sending
them. `POST /{ds}/shacl` validates one `target` node against a shapes graph in the body,
and `POST /{ds}/shex` takes a shape map. Phase 4 therefore starts with one addition to
each.

**Prerequisite, focus sets and stored shapes.**

1. `POST /{ds}/shacl` takes `target` up to 1,000 times, and validates each node against the
   shapes whose targets select it, as one `target` does today. It also takes
   `shapes=guard` or `shapes=<graph IRI>` with an empty body, which validates against the
   write-time guard's shapes or a shapes graph of the dataset. A request that names a source
   and also has a body is a `400`.
2. `POST /{ds}/shex` takes `schema=guard` with an empty body, and its compact shape map may
   hold up to 1,000 node associations, each node with its shape from the guard's shape map
   when the map names `START` or a class target.

Both answers keep their current shapes. The overlay sends the drawn nodes in batches of up
to 500, one batch in flight. When U07 Phase 1's record exists for the head and the canvas is
at the head, the overlay reads `GET /$/validation/{ds}/results` filtered to the drawn focus
nodes instead, which costs no validation. Phase 1's route gains `focus=` for that, up to
1,000 terms.

These parameters are added to `docs/API.md`, the route table and the OpenAPI description.

### 11.4 Suggested fixes

A suggestion is generated in the browser from the result alone, in `lib/fixes.ts`. It is a
template with the result's terms filled in and the unknowns left as clearly marked
placeholders, so it never guesses data.

| Component | Suggestion |
|---|---|
| `sh:minCount` | `INSERT DATA { <focus> <path> ?value }` with `?value` as a placeholder to fill in. |
| `sh:maxCount` | A `DELETE` of the values beyond the first, listed in a comment for the person to choose. |
| `sh:datatype` | `DELETE` and `INSERT` that casts the value when the lexical form is valid for the datatype, and none otherwise. |
| `sh:class` | `INSERT DATA { <value> a <class> }`, with a comment that the other choice is to change the value. |
| `sh:in`, `sh:hasValue` | A `DELETE` and `INSERT` with the allowed values listed in a comment. |
| `sh:closed` | A `DELETE` of the triple with the unexpected predicate. |
| ShEx `cardinality`, `datatype`, `valueSet` | The same forms as the SHACL rows above. |

Paths other than a single predicate, `sh:sparql`, `sh:pattern`, `sh:node`, qualified shapes
and other components get no suggestion, and **Show triples** remains. Every suggestion runs
through C15's preview before the person can commit it, under their own grants and the guard.

### 11.5 Access, branches and past states

The SHACL and ShEx routes answer `403` to a caller whose grants cover part of a dataset,
because a result can reveal hidden triples, and C12 §5.5 keeps validation over a view for
later. The overlay follows that rule and is not offered to such callers. U07 §2.4 already
shows the Quality card only to callers who can read the guard's data graphs in full, and the
`focus=` filter of the results route keeps that check. Validation with `at=` already exists,
so the overlay validates at the canvas's state and branch, and its header names them. Moving
U01's slider validates the drawn nodes again at the new state, one batch at a time, and
**Play** pauses the overlay as U10 §4.3 pauses projection lenses.

### 11.6 Limits

At most 1,000 focus nodes per request and 1,500 drawn nodes per canvas, with batches of 500
and one in flight. Each request runs under the validation pool, the request timeout and the
result budgets that the routes already apply. The results panel lists up to 100 results for
a node and counts the rest.

### 11.7 Testing

- **Rust.** Repeated `target`, `shapes=guard` and `shapes=<graph>`, the `400` for a source
  with a body, ShEx `schema=guard` with a map of many nodes, the `403` for restricted callers
  on the new forms, and `focus=` on the results route.
- **Router.** The new parameters under the validation pool, the request timeout and the
  result budgets, and OpenAPI equality.
- **Vitest.** `lib/fixes.ts` for every row of §11.4, placeholders, and the absence of a
  suggestion for the other components. The mapping from results to badges, including values
  drawn as literal nodes.
- **Playwright against the mock server.** A10 to A12 below and the 390 px sheet.
- **Playwright against a real server.** A10, A11 and A13, building the history as
  `write-validation.spec.ts` does.

### 11.8 Acceptance examples

The data is §7's dataset `q`.

- **A10. Badges.** With the guard's shape and three people without names drawn, each person
  has `✕1`, and the list view's column says "1 violation" for each.
- **A11. Fix.** **Suggest fix** on one of them opens an `INSERT DATA` with the person and
  `ex:name` and a placeholder value. Filling it with `"Ana"` and previewing reports one
  resolved result, and after running it the badge disappears.
- **A12. Show triples.** For a `sh:pattern` result on `ex:email`, **Show triples** highlights
  the person, the `ex:email` edge and the literal.
- **A13. Restricted caller.** A caller whose grants hide one graph sees no **Validation**
  lens and the reason, and the new request forms answer `403` for them.
- **A14. Cost.** On a dataset of 10 million triples with 1,500 drawn nodes, badges appear
  within two seconds of the drawing settling, through at most three requests.

### 11.9 Open questions

1. Whether the overlay should be offered to callers with partial access once C12's
   validation over a view lands, and with which shapes.
2. Whether suggested fixes should be recorded as stored updates under roadmap item 39, so a
   team can reuse a fix across results.

## Outcome

Phases 1 to 3 are not built, and [ROADMAP](../ROADMAP.md) item 31 covers them. Phase 4
shipped on 2026-10-23 under item 106, and item 120 holds what it left for later.

### Phase 4 (2026-10-23)

The prerequisite of §11.3 is in the routes. `POST /{ds}/shacl` takes `target` up to 1,000
times and validates the nodes in one engine run, and it takes `shapes=guard` or
`shapes=<graph IRI>`. `POST /{ds}/shex` takes `schema=guard` with a shape map of up to
1,000 nodes, and it now takes `at=` as `/{ds}/shacl` does. The library gained
`sparkles_shacl::validate_nodes`, `sparkles_shex::shapemap::focus_map`, and accessors on
the two guards for their data graph and, for ShEx, their schema and shape map. The routes
keep the validation pool, the request timeout, the result budgets and the `403` for a
caller whose grants cover some graphs, and router tests check each of these on the new
forms.

Explore has the overlay of §11.2. Badges are buttons drawn over the canvas at their
nodes, whose names give the node and its counts. The results panel groups a node's
results by shape and constraint, **Show triples** draws and marks the focus node, the path
edge and the value, and **Suggest fix** builds an update in `ui/src/lib/fixes.ts` for the
components of §11.4. A preview is a dry run of `/{ds}/update` with `changes=20`, and the
panel reads the dry run's guard summary to say whether the result would be resolved.
GraphView gained one optional `validation` prop with the badges, the marked elements and
the click handler, and TimeSlider an `onplaying` callback.

Nothing in Phase 4 changes an on-disk format. The overlay sends one small probe when a
graph opens and then one request per 500 drawn nodes, so no benchmark covers it. A14 at
10 million triples was not measured. Per node, a validation reads the node's own triples
through the indexes, so its cost does not grow with the dataset.

**Decisions taken where the spec left them open.**

1. **A toolbar toggle instead of the lens menu.** U10's badge channel is not built, so
   **Validation** is a toggle beside the canvas. With no choice made, it is on when the
   dataset has a guard the caller may use. The choice is remembered in the browser. Item
   120 moves it into the lens menu.
2. **No read of the Phase 1 record.** Phase 1 is not built, so the overlay always
   validates, and `focus=` on `GET /$/validation/{ds}/results` waits for item 120.
3. **A named source takes a form body.** §11.3 asks for an empty body with `shapes` or
   `schema`. A form body is accepted too, and its fields are parameters, because 500 IRIs
   in a query string can pass the URL limits of proxies. Any other body is still a `400`.
4. **The guard's shapes as they are now.** `shapes=guard` validates the guard's data graph
   with the guard's current shapes, also at a past state, so `graph` is a `400` with it.
   `shapes=<graph IRI>` reads the shapes graph at the state read and leaves it out of the
   data graph.
5. **START under `schema=guard`.** Each association of the request's map must name a node.
   `START` takes the shapes of the guard's associations that select the node, by naming it
   or by a `{FOCUS p o}` or `{s p FOCUS}` pattern checked for that node alone. A guard's map
   cannot hold SPARQL selectors, so none are left out. An association with a label keeps
   it.
6. **Targets name blank nodes.** `target` also takes an IRI in angle brackets and a stored
   blank node label, so the overlay can send every drawn IRI and blank node as it is.
7. **Finding the guard.** The page asks `shapes=guard` about a node that is not in the
   data. A `200` means a SHACL guard, a `400` naming ShEx a ShEx guard, a `404` no guard
   and a `403` a restricted caller. This works for callers who may not read the guard's
   configuration.
8. **The Validate panel's shapes.** The editor source is the SHACL shapes the dataset
   page's Validate panel last validated with, read from the browser's storage, sent in
   batches of 100 nodes because the targets then travel in the query string. A ShEx
   editor source is not offered.
9. **Parameters of a suggestion.** A SHACL result does not carry the constraint's value, so
   the datatype, the class and the allowed values are read from the default messages. When
   a shape's own `sh:message` hides them, a class becomes the placeholder `?class`, allowed
   values become `?allowed`, and a cast is not suggested. A focus node that is a blank node
   gets no suggestion, because an update cannot name a stored blank node. `sh:hasValue`
   inserts the required value. `sh:maxCount` lists the current values, read with a query at
   the canvas's state. A ShEx datatype or value-set failure does not name its predicate,
   so its update matches the value under any predicate `?p`.
10. **The fix stays in the panel at 390 px.** §11.2 opens the query page there, but that
    page has no C15 preview, and a suggestion must be previewed before it runs. The sheet
    keeps the editor and the preview, and **Open in Query** takes the text to the query
    page at any width.
11. **When Run is enabled.** Run needs a preview of the same text whose outcome is
    `commit`, and a canvas at the head. A placeholder left in the text, comments aside,
    disables Preview.
12. **Resolved or not.** The preview's guard summary decides. The result is resolved when
    the summary is complete and does not list it, still reported when it lists it, and
    unknown without a summary of the result's language or when the list was truncated.
    Without a guard source, the overlay validates again after the update runs.
13. **The list view.** Explore's node-link canvas has no list view, so the overlay has its
    own **Nodes with results** table with a Validation column.
14. **Pausing.** The overlay waits while the graph's history plays, through a callback that
    TimeSlider gained, and it validates the drawn nodes 150 ms after they stop changing.

