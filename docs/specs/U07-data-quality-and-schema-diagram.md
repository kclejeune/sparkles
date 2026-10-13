# U07: A data-quality timeline and a schema diagram

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 records a validation summary for every commit a
> write-time guard validates, and draws it as a timeline on the dataset page with links
> to the commits that introduced each result. Phase 2 backfills the timeline for past
> commits and finds the introducing commit of results older than the record. Phase 3
> adds the schema diagram with observed and declared cardinalities, and its export.
>
> **User docs:** none, because nothing is built.
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

## Outcome

Not built. See [ROADMAP](../ROADMAP.md) item 31.
