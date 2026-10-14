# U01: A time slider over graphs, results, entities and the schema

> **Status:** implemented
>
> **Phases:** All three shipped. Phase 1 is the slider component, the query page's table
> and graph results at any commit with row diffs, and an entity's properties in Explore
> over time. Phase 2 adds the Explore graph with animated additions and removals, the
> schema tab, a bucketed commit timeline and a term filter on diffs. Phase 3 adds
> comparison across branches, result diffs computed on the server and the map view.
>
> **User docs:** [USAGE: The time slider](../USAGE.md#the-time-slider), and in
> [API](../API.md) the sections on [the commit timeline](../API.md#the-commit-timeline),
> the `term` parameter of diffs and
> [comparing a query between two states](../API.md#comparing-a-query-between-two-states).
>
> The sections before the Outcome are the design as written before implementation. The
> [Outcome](#outcome) records where the build differs from it.

This design was written from the Sparkles code and specs: point-in-time reads, diffs, the
change feed and history queries ([F06](F06-snapshots-and-point-in-time.md)), branches and
the commit graph ([F09](F09-branches-and-merges.md)), commit identity
([CI](CI-commit-identity.md)), schema reports and schema diffs
([C02](C02-schema-discovery.md)) and the result comparison of
[C18 §6.7.5](C18-natural-language-questions-and-ingest.md#675-comparing-results). It also
uses RFC 7089 (Memento) for the meaning of a state at a time, the WAI-ARIA Authoring
Practices' slider pattern for keyboard and screen-reader behaviour, the Gephi
documentation of its timeline for dynamic graphs as an example of a scrubbed graph view,
and the Cytoscape.js documentation of element animation, which the UI already uses for
graph drawing.

## 1. Summary, goals, non-goals

Sparkles keeps every commit readable within its retention window and records the net
changes of every commit in the change log (F06). The UI already reads past states in
several places, and this spec builds on them rather than adding point-in-time viewing:

- the query page's **at** field (`app.queryAt`), which sends `at=` with a query and
  offers named snapshots, and shows the state a result read (`Sparkles-At`);
- the dataset page's History panel (`HistoryPanel.svelte`), which lists commits with
  their diffs from `/{ds}/diff`, runs history lookups, and draws every branch as a commit
  graph (`CommitGraph.svelte`, `lib/commit-graph.ts`);
- the Memory tab's **as of** selector (`MemoryTab.svelte`), which reads an entity's
  memory facts at a chosen commit;
- the schema tab's **Compare** dialog (`SchemaDiffDialog.svelte`), which shows a schema
  diff between two states.

Each of these picks one state from a field or a list. What is missing is a way to move
through time continuously and see what changed in the view one is looking at, instead
of reading commit lists and diff lines. The slider is a new control over the same
selectors: it writes the existing `at` field, and it replaces the Memory tab's select
with the same component, keeping its behaviour.

This spec adds one slider component, `TimeSlider`, and puts it over four views: query
results, the Explore graph, an entity's properties and the schema tab. Moving the handle
reads the view at that commit. A second handle compares two commits and marks what was
added and removed. **Play** steps through the commits and animates the changes.

**Goals**

1. One control with the same keyboard, URL and accessibility behaviour on every view.
2. Each view reads exactly the state that the slider names, through the existing `at`
   selector, and shows the commit it read.
3. Additions and removals between two states are marked in the view by shape and text as
   well as colour.
4. The slider only offers states that can be read, and says why the others cannot.
5. A link reproduces the view, the commit and the comparison.
6. The cost of a scrub is bounded: one request in flight per view, cancelled when the
   handle moves on.

**Non-goals**

- Editing at a past commit. Writes with `at` stay refused (`at-on-write`).
- Bitemporal or valid-time data. The slider moves through transaction time only, the
  commit sequence. Data that records its own validity periods is queried as data.
- Reconstructing states that retention dropped. The change log's entries outlive states,
  and the slider shows them as marks, but it never claims to show a state it cannot read.
- A new history store. Everything reads F06's states, diffs and change log.

## 2. User-visible behaviour

### 2.1 The slider

The slider is a bar under the view's toolbar, opened by a **History** toggle (shortcut
`H`). It reads the commits of the branch the view targets.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ⏮ ◀ ▶ ⏭  ▷ Play 1×   main ▾   [Compare]                commit 42 · 2026-10-01 │
│ ├──┼─┼┼──────●─┼───┼┼┼───────────────◆──────────┼──────────┼───────────┤ head │
│ ░░░░ gone   │  load   update ×3         snapshot:release-3   merge dev        │
│ 42 · update by ana · "fix titles" · +3 −1                                     │
└──────────────────────────────────────────────────────────────────────────────┘
```

- **Track.** Ticks are commits, placed by sequence number by default or by time (a switch
  in the bar's menu). Named snapshots are diamonds with their names. Merge commits and the
  branch's fork point are marked. A stretch that can no longer be read is hatched and
  labelled **gone**, with a tooltip that gives the oldest readable commit from
  `oldestReconstructable`.
- **Density.** When the branch has more commits than the track has pixels, ticks are
  bucketed, and each bucket shows its commit count and its added and removed quads as a
  small bar. Zooming the track (`+`, `−`, or the wheel with `Ctrl`) narrows the range.
- **Handle.** Dragging the handle reads the view at that commit when the drag pauses for
  250 ms or ends. The caption shows the commit, its time, kind, author, message and net
  change.
- **Compare.** **Compare** adds a second handle. The view then shows the state at the
  right handle and marks what changed since the left one (§2.3). Dragging the band between
  the handles moves both.
- **Play.** **Play** steps the right handle one commit at a time, or one tick of the
  current zoom, at 1×, 2× or 4×. A step waits until the view has rendered the previous
  one, and never starts sooner than 1.5 times the last read took, so playing never queues
  requests. **Play** stops at the head, at a gone stretch, or on any error.
- **Head.** The rightmost position is **head**, the live state. A view at head follows new
  commits only when the person presses **Refresh** or the slider is at head with
  **Follow** on, which listens to the change feed's event stream (`GET /{ds}/changes` with
  `Accept: text/event-stream`).

### 2.2 Keyboard

| Key | Action |
|---|---|
| `←`, `→` | Previous or next commit (the focused handle). |
| `PageUp`, `PageDown` | Ten commits back or forward, or one bucket when bucketed. |
| `Home`, `End` | The oldest readable commit, or head. |
| `S`, `Shift+S` | The next or previous named snapshot. |
| `Space` | Play or pause. |
| `C` | Compare on or off. |
| `Tab` | Moves between the two handles, then out of the slider. |
| `Escape` | Closes the slider and returns the view to head. |

### 2.3 What each view shows

**Query results (Phase 1).** The query page's **at** field and the slider are the same
state: moving the slider writes the field, and typing in the field moves the slider. The
query runs again at the new commit. In compare mode the query runs at both commits, and
the table gets a leading change column: `+` for rows only in the right state, `−` for
rows only in the left. Removed rows are shown struck through below the kept rows, and a
filter chip offers **all**, **added**, **removed** and **unchanged**. Rows are compared
as bags of their terms, as in C18 §6.7.5. The graph view of a result marks added nodes and
edges with a solid outline and a `+` badge and removed ones with a dashed outline and a
`−` badge. When either result has more than 10,000 rows, the client compares counts only
and offers **Compare on the server** (Phase 3, §4.4).

**Agent memory in Explore (Phase 1).** The Memory tab's **as of** select becomes a
`TimeSlider` over the same commit list. Choosing a commit reads the same facts as the
select does today, and compare mode marks facts added and superseded between two commits.

**An entity in Explore (Phase 1).** The properties panel of a focused resource gets a
**History** strip: a slider whose ticks are only the commits that changed the resource's
triples as subject or object, read with a history query (§4.2). Moving it shows the
properties at that commit, and compare mode lists the property values added and removed,
each with the commit, author and message that changed it. A value whose change predates
the readable range is listed from the change log with the note "state not readable".

**The Explore graph (Phase 2).** The drawn neighbourhood is re-read at the slider's
commit. Nodes and edges that appear fade in, those that disappear fade out and stay as
dashed ghosts in compare mode, and nodes that stay keep their positions, so the layout
does not jump. The person's expansions are replayed at the new commit: a node expanded at
head is expanded at the past commit too, if it existed.

**The schema tab (Phase 2).** The class tree and property list are read with `at=`
(`/$/schema/{ds}?at=`). In compare mode each class and predicate shows the change in its
counts, as `+120` or `−4`, and added or removed classes are marked. The existing
**Compare** dialog stays, and gets a button that opens the slider with its two states.

**Maps (Phase 3).** The map view of a result marks added and removed features the same
way.

### 2.4 URL

The query page and Explore keep the slider in the URL so a link reproduces the view:
`at=commit:42` for the state, `from=commit:40` for the left handle in compare mode, and
`branch=` as today. Explore already keeps `ds`, `iri` and `tab` in its URL, and the query
page keeps none today, so the query page adds `at`, `from` and `branch` to its URL only
while the slider is open.

### 2.5 Empty and error states

| Case | What the view shows |
|---|---|
| A dataset with one commit | The slider shows one tick and disables Play and Compare, with "No earlier commits". |
| An in-memory dataset with no retention window | "Past states of this in-memory dataset are not kept. Set a retention window or pin snapshots." with a link to the History panel. |
| A gone commit (`410 history-gone`) | The view keeps its last state, the handle snaps back, and a toast gives the readable ranges from the error body. |
| `507` from the history cache | "Reading this state needs more history cache than the server allows", with the limit. |
| A caller limited to some graphs | Commit counts are absent (they cover every graph), so density bars show commit counts only. |
| A read that times out | The handle stays, the view shows the timeout, and **Retry** reads again. |

### 2.6 Accessibility

Each handle is a `role="slider"` with `aria-valuemin`, `aria-valuemax` and `aria-valuenow`
as commit numbers and `aria-valuetext` such as "commit 42, 1 October 2026 14:03, update
by ana, 3 added, 1 removed". A polite live region announces the result of each read,
such as "Commit 42: 3 rows added, 1 removed". Changes are marked by `+` and `−` badges,
outline style and the change column as well as colour. With `prefers-reduced-motion`,
nothing fades or moves: changes appear at once with their badges. Play is paused while
focus is in the view, so a screen-reader user is not interrupted.

## 3. Server and API changes

Phase 1 needs no server change. It uses `/{ds}/sparql?at=`, `/$/commits/{ds}`,
`/$/snapshots/{ds}`, `/{ds}/diff` and history queries (`SERVICE hist:changes`), which all
exist.

Phase 2 adds two things.

1. **`GET /$/commits/{ds}/timeline`.** The commits of a branch in at most `buckets`
   buckets (default 200, at most 2,000) between `from` and `to`. Each bucket is
   `{firstSeq, lastSeq, firstTime, lastTime, commits, inserted, deleted, kinds,
   snapshots, merges}`, plus `readable` ranges. It reads the commit catalog, which keeps
   the counts, so it costs one pass over the catalog's entries in range. It needs `read`
   and takes `branch=`. Counts are left out for a caller limited to some graphs, as
   `/$/commits` does.
2. **A term filter on diffs.** `GET /{ds}/diff` takes `term=` (repeatable, up to 1,000)
   and keeps the changes whose subject or object is one of the terms. The `log` method
   filters while it reads the log records, and the `compare` method filters both sides of
   its merge. This lets the Explore graph diff a drawn neighbourhood without moving the
   whole diff to the browser.

Phase 3 adds **`POST /{ds}/sparql/diff`**, which runs one read-only query at two
selectors and returns the rows only one side has, with counts. It reuses the streaming,
hashed comparison of C18 §6.7.5, keeps hashes and not rows during the comparison, then
runs the query once more to fetch up to `limit` differing rows of each side (default 100,
at most 10,000). It needs `read`, counts twice against the `query` rate-limit class, and
takes `from`, `to`, `branch`, `reasoning` and `limit`. Phase 3 also lets diff selectors
name a branch (`from=main:commit:40`), so the slider can compare a branch with `main`;
whether that is cheap enough is open (§9).

All of these are added to `docs/API.md`, the route table and the OpenAPI description,
which the X03 test keeps equal.

## 4. Design

### 4.1 The slider's model

`lib/timeline.ts` holds the pure parts: the track model built from a commit page or a
timeline answer, bucketing on the client for short histories, the mapping between a pixel
and a commit (by sequence or by time), the readable ranges from `readable()` in
`lib/history.ts` and from `history-gone` bodies, snapshot and merge marks, and the step
of **Play**. It has no Svelte and no fetches, so Vitest covers it.

`TimeSlider.svelte` draws the track as SVG, like `CommitGraph.svelte`, and exposes
`at: string | null`, `from: string | null`, `branch` and `onchange`. It reuses
`atLabel`, `normalizeAt` and `diffSummary` from `lib/history.ts` and the commit formatting
of `lib/commits.ts`.

### 4.2 Reading each view

Every view implements one function, `readAt(selector, signal)`, and the slider calls it
with an `AbortController` that is aborted when the handle moves on. One request per view
is in flight at a time.

- **Results.** `api.query` with `at`. Compare mode reads both selectors in parallel and
  diffs on the client with `lib/rowdiff.ts`: each row is keyed by its terms in order,
  with `termKey` from `lib/rdf.ts`, and the two results are compared as bags. The diff of
  a graph result keys triples the same way.
- **Entity.** One history query lists the commits that touched the resource:

  ```sparql
  PREFIX hist: <urn:x-sparkles:history#>
  SELECT ?commit ?time ?author ?message ?op ?p ?o WHERE {
    { SERVICE hist:changes { << <IRI> ?p ?o >> hist:op ?op ; hist:commit ?commit ;
        hist:time ?time ; hist:author ?author ; hist:message ?message } }
    UNION
    { SERVICE hist:changes { << ?o ?p <IRI> >> hist:op ?op ; hist:commit ?commit ;
        hist:time ?time ; hist:author ?author ; hist:message ?message } }
  } ORDER BY ?commit LIMIT 5000
  ```

  F06 says constants in the triple use the change log's index, so this reads only the
  commits that touched the resource. The properties at a commit come from the same
  `explore.details` call with `at` added.
- **Explore graph.** The drawn nodes are re-read with `neighbours` and `at`, limited to
  the expanded nodes. In compare mode, Phase 2 asks `/{ds}/diff` with `term=` set to the
  drawn IRIs and applies the changes to the graph instead of re-reading every node.
- **Schema.** `api.schema` and `/$/schema/{ds}/diff` with the two selectors.

`lib/explore.ts` functions gain an optional `at` and pass it through `api.select`.

### 4.3 Animation

`GraphView.svelte` gains a `changes` prop, a map from element id to `added`, `removed` or
`kept`. Added elements start at opacity 0 and animate to 1 over 300 ms. Removed elements
animate to 0.35 with a dashed border and stay as ghosts while compare mode is on, or are
removed after the animation otherwise. New nodes are seeded next to an existing
neighbour, as incremental layouts already do, and the layout runs without randomizing, so
kept nodes do not move. The styles use the existing CSS variables, with two new tokens,
`--added` and `--removed`, defined for light and dark themes.

### 4.4 Cost and limits

Each step is a full query at a past state. F06 bounds materializing a past state by
`--history-cache-mb` and runs one materialization at a time, so the slider steps in
order and never in parallel beyond the two reads of compare mode. Reads near a cached
state are cheap, because F06 replays the log from the nearest known state. The client
diff compares at most 10,000 rows per side. The server diff of Phase 3 handles larger
results within the caller's row and memory budgets and fails with the budget's `507`
otherwise.

## 5. Architecture

| Part | Where |
|---|---|
| Track model, bucketing, play steps | `ui/src/lib/timeline.ts` (new) |
| Row and triple bag diffs | `ui/src/lib/rowdiff.ts` (new) |
| Slider | `ui/src/lib/components/TimeSlider.svelte` (new) |
| Change marks in graphs | `GraphView.svelte`, `lib/graph.ts` (`changes` on `GNode` and `GEdge`) |
| Change column and filter | `ResultTable.svelte` |
| Query page wiring, URL | `routes/query/+page.svelte` (the existing `app.queryAt` field stays, and the slider writes it) |
| Memory tab | `MemoryTab.svelte`, whose **as of** select the slider replaces |
| Explore wiring | `routes/explore/+page.svelte`, `lib/explore.ts` (`at` on its reads) |
| Schema tab | `routes/explore/+page.svelte`, `SchemaDiffDialog.svelte` |
| Timeline route | `sparkles-server` `http` commits handlers, reading the commit catalog |
| Diff term filter | the diff handler and the change-log reader of `sparkles-core` history |
| Result diff | `sparkles-server`, sharing the comparison of C18 §6.7.5 when Phase 6 lands |
| Mock server | `ui/mock/` gains commits with states for Playwright |

## 6. Phases

1. **Phase 1.** `TimeSlider`, `lib/timeline.ts`, `lib/rowdiff.ts`, the query page's table
   and graph views with compare mode, the Memory tab's **as of** control, and an entity's
   History strip in Explore. No server change.
2. **Phase 2.** The Explore graph with animation, the schema tab, the timeline route and
   the term filter on diffs.
3. **Phase 3.** Comparison across branches, `POST /{ds}/sparql/diff`, and the map view.

## 7. Acceptance examples

The examples assume a dataset `ds` whose commits are: 1 loads `ex:alice ex:name "Alice"`
and `ex:alice ex:knows ex:bob`; 2 adds `ex:bob ex:name "Bob"`; 3 removes
`ex:alice ex:knows ex:bob` and adds `ex:alice ex:knows ex:carol`; a snapshot `s2` pins
commit 2.

- **A1. Scrub results.** On the query page, `SELECT ?s ?o WHERE { ?s ex:knows ?o }` runs
  at head. Opening the slider and pressing `←` twice runs it at commit 1. The table shows
  one row (`ex:alice`, `ex:bob`), and the caption reads "commit 1".
- **A2. Compare results.** With compare on, the left handle at commit 2 and the right at
  head, the table shows `+ ex:alice ex:carol` and, struck through, `− ex:alice ex:bob`.
  The live region says "Commit 3: 1 row added, 1 removed". The **added** chip shows one
  row.
- **A3. URL.** Reloading the URL of A2 restores compare mode, both handles and the
  same table.
- **A4. Snapshots.** `S` from commit 1 moves the handle to commit 2 and the caption names
  `snapshot:s2`.
- **A5. Entity history.** In Explore, focusing `ex:alice` and opening History shows ticks
  at commits 1 and 3 only. Compare between them lists `ex:knows ex:carol` as added and
  `ex:knows ex:bob` as removed, each with commit 3's author and message.
- **A6. Gone state.** When the server answers `410 history-gone` for commit 1, the handle
  returns to its previous commit, a toast names the readable range, and the track hatches
  commit 1.
- **A7. Graph animation (Phase 2).** In Explore with `ex:alice` expanded at head, moving
  to commit 1 shows `ex:bob` and its edge, and `ex:carol` disappears. In compare mode
  `ex:carol` has a `+` badge and `ex:bob` a dashed outline and `−` badge. With reduced
  motion emulated, no CSS transition or Cytoscape animation runs.
- **A8. Play.** Play from commit 1 visits commits 2 and 3 in order, never has two queries
  in flight, and stops at head.
- **A9. Schema (Phase 2).** The schema tab at commit 1 shows `ex:name` with 1 triple. In
  compare mode from commit 1 to head, `ex:name` shows `+1`.
- **A10. Keyboard only.** Every action of A1–A5 can be done with the keyboard, and each
  handle's `aria-valuetext` names its commit.

## 8. Testing

- **Vitest.** `lib/timeline.ts`: bucketing, pixel and commit mapping by sequence and by
  time, gone ranges, play steps that skip gone stretches. `lib/rowdiff.ts`: bags with
  duplicates, unbound cells, blank nodes, literals that differ only by datatype.
- **Playwright against the mock server.** A1–A6, A8 and A10, with the mock serving
  `at=` reads from fixed states and a `410` for a gone commit.
- **Playwright against a real server.** A1, A2 and A5 on a dataset built by the test from
  three updates, and A7 and A9 once Phase 2 lands. The smoke suite gains one history
  scrub.
- **Server tests (Phase 2 and 3).** The timeline route's buckets against `/$/commits`, the
  diff term filter for `log` and `compare` methods against an unfiltered diff filtered in
  the test, and `/sparql/diff` against two runs compared in the test.

## 9. Rejected alternatives

- **Downloading the change feed and rebuilding states in the browser.** It moves every
  change of the dataset to the client and duplicates the server's state logic. Reading
  states with `at` costs one query per step and stays within the caller's grants.
- **A separate history page.** Time is a property of the view, so the slider sits over
  the views people already use. The History panel stays for commit administration.
- **Animating every intermediate commit when jumping.** A jump of many commits reads only
  the target, and compare mode shows the net change. Play is the way to see each step.
- **Diffing results on the server from Phase 1.** Client diffs of up to 10,000 rows need
  no server change and cover the common case. The server diff waits for C18's comparison
  code.

## 10. Open questions

1. Whether a diff between branches can use the merge base and two log ranges, or needs a
   state comparison. This decides whether Phase 3's branch-qualified selectors are cheap.
2. Whether **Follow** at head should be on by default on the query page, where an
   unexpected re-run would cost a query per commit on a busy dataset.
3. Whether the timeline route should also bucket by time with calendar boundaries, for a
   track by time over years of commits.

## Outcome

All three phases are built. The slider is `ui/src/lib/components/TimeSlider.svelte`, its
pure model is `ui/src/lib/timeline.ts`, and the row and triple diffs are
`ui/src/lib/rowdiff.ts`. The server gained `GET /$/commits/{ds}/timeline`
(`http/timeline.rs`), the `term` filter on `GET /{ds}/diff` (`DiffOptions::only_terms` in
`sparkles-core`, applied where the diff nets the changes) and `POST /{ds}/sparql/diff`
(`http/sparql_diff.rs`). The route table, the OpenAPI description, `docs/API.md` and the
client parity table list all three.

### What was checked

- **Phase 1.** Vitest covers the track model and the diffs. The mock-server Playwright
  suite covers A1 to A6, A8 and A10, and the real-server suite covers A1, A2 and A5 and a
  scrub of the History view.
- **Phase 2.** Rust tests cover the timeline's buckets against the commit list and the
  term filter against an unfiltered diff for both diff methods. Playwright covers A7 and
  A9 and the bucketed track against the mock server, and A7 and A9 against a real
  server.
- **Phase 3.** Rust tests cover `/sparql/diff` against two runs compared in the test and
  its charge of two against the `query` rate-limit class. Playwright covers marks on the
  map, a comparison with another branch on the query page and in the Explore graph, and
  **Compare on the server**, against the mock server and against a real one.

### Where the build differs from the design

- **Change marks.** Graph elements carry a `change` field on `GNode` and `GEdge` in
  `lib/graph.ts` instead of `GraphView` taking a map of ids to changes. A change is a
  property of the element, and this keeps the marks through the layout's rebuilds. The
  Map view takes a `change` on each `MapFeature` the same way.
- **URL.** The query page's address also carries `ds` while the slider is open, so that a
  link names the dataset. Explore's and the query page's address add `against` for the
  branch of a comparison.
- **Branch comparison.** The design proposed selectors such as `from=main:commit:40`.
  `GET /{ds}/diff` already takes `fromBranch` and `toBranch`, so the slider compares the
  head of another branch with the state it is at, and the selector syntax was not added.
  The query page reads the query on both branches and compares the results in the
  browser. `POST /{ds}/sparql/diff` compares two states of one branch and is not offered
  across branches. The merge-base question of open question 1 is therefore not needed,
  because a diff across branches compares the two states with a sorted merge.
- **Follow.** It listens to `GET /{ds}/changes` as server-sent events, not by long poll.
  It is off by default on every view, which answers open question 2.
- **Timeline buckets.** The route buckets by commit number only. A track by time over
  years of commits still places the buckets by their commit times, and calendar
  boundaries (open question 3) remain unbuilt. The client switches to the timeline route
  when a branch has more than 2,000 commits.
- **The `507` of a history read.** The slider reports the server's `507` with the limit it
  names, as designed. For a past state the budget it names is `memory`.
- **Term filter cost.** The `log` method still reads every record of the commit range and
  keeps only the matching changes, so memory stays proportional to the matches and not to
  the range. The filter cannot be combined with RDF Patch output, and the server answers
  `400` for that pair.
- **Graph rebuilds.** The Explore graph is rebuilt at a commit from the expansions the
  person made, so a node added by hand with the graph's link action is not kept when the
  slider moves.
- **Test hooks.** The graph view exposes `data-graph`, `data-motion` and `data-animations`
  on its wrapper, and the map exposes `data-added` and `data-removed`, so that Playwright
  can read what was drawn.
- **Mock server.** Its seeded `foaf` dataset has history that cannot be read, and serves as
  the gone case of A6.

### Not built

A calendar-aware track by time, a branch-qualified selector and a server comparison
across branches. The first needs a decision on the buckets of a track that spans years,
and the other two have a workable alternative in the slider's comparison with the head of
another branch.

See [ROADMAP](../ROADMAP.md) item 11.
