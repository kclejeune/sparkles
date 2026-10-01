# C08: Inference freshness and inconsistency diagnostics

> **Status:** implemented in part
>
> **Phases:** Phase 1 (commit-based freshness, `GET /$/reason/{ds}`, the
> `Sparkles-Inferences` header, re-runs, opt-in automatic re-materialization, the seven
> diagnostics checks, `sparkles infer --status/--check`, the UI panel) shipped, built on
> durable commit identity, so the §5.1 stopgap was never needed. Phase 2 and Phase 3 are
> not built.
>
> **User docs:** [API: Reasoning status and diagnostics](../API.md#reasoning-status-and-diagnostics) ·
> [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation; the [Outcome](#outcome) section at the end
> records how it landed.

This is a clean-room spec. It uses the durable commit sequence `seq` from the commit
identity spec ([CI](CI-commit-identity.md)): `Snapshot::commit`, `Store::head_commit()`,
and the `Receipt` returned by `WriteTxn::commit()`. If C08 is implemented first, §5.1
defines a stopgap counter.

## 1. Summary

Sparkles materializes entailments into `urn:x-sparkles:inferred`
(`sparkles_reasoner::materialize`, `POST /$/reason/{ds}`, `sparkles infer`). It records
`{profile, inferred, at}` in `<db>/reasoning.json` and in the registry. Queries include
the inferred graph by default. There are two gaps:

1. **Freshness is invisible.** After any update the inferences may be outdated: missing
   new entailments, or keeping entailments of deleted triples. Nothing tells the user,
   the UI or the CLI. The README only says "must be re-run after updates".
2. **No inconsistency detection.** The OWL 2 RL rule set explicitly excludes it
   (`rules/owl-rl.rules` header: "Not covered: validation / inconsistency detection").
   Contradictions such as an individual in two disjoint classes go unnoticed.

**Goals**

- Record the store position at which inferences were generated.
- Report `stale` and `commitsSince` in a new status endpoint, in `DatasetInfo`, in
  `/$/stats`, in the CLI, and as a response header on queries that use inferences.
- An opt-in, debounced automatic re-materialization. The default is **off**.
- An explicit diagnostics report (`GET /$/reason/{ds}/diagnostics`,
  `sparkles infer --check`) for a documented, sound subset of the OWL 2 RL
  inconsistency rules, computed with SPARQL over data ∪ inferences.

**Non-goals**

- Claiming OWL consistency. The report can say "violations found" or "none of these
  checks found a violation", never "consistent".
- Incremental maintenance: DRed and similar counting algorithms.
- Explanations beyond the matched evidence.
- Datatype reasoning (`dt-not-type`).
- Checking named graphs other than the default graph.

## 2. User-visible behavior

### 2.1 Reasoning status

New route: `GET /$/reason/{ds}` returns the `ReasoningStatus` below, or
`{"reasoning": null, "head": 17}` when nothing is materialized.

```ts
type ReasoningStatus = {
  profile: string;            // "rdfs" | "rdfs-simple" | "owl-rl" | "rules"
  inferred: number;
  at: string;                 // RFC 3339, when the run committed
  commit: number | null;      // seq at which the inferences were materialized; null = unknown (older Sparkles, or other lineage)
  head: number;               // current head seq (same value as DatasetInfo.head in the CI spec)
  stale: boolean | null;      // null = unknown (commit missing or not comparable)
  commitsSince: number | null;
  staleReason?: string;       // "3 commits since materialization", "inherited from source at clone time", …
  auto: { enabled: boolean; debounceSeconds?: number; scheduledAt?: string /* next planned run */ };
  warnings: string[];         // the last run's warnings (e.g. generalized triples dropped)
};
```

Where the status appears:

- `DatasetInfo.reasoning` gains `commit`, `stale` and `commitsSince`. The existing
  `profile`, `inferred` and `at` fields are unchanged.
- `DatasetStats` gains `reasoning: ReasoningStatus | null`.
- A **query response header**: when a SPARQL query or a SHACL validation includes the
  inferred graph and the status is stale, the response carries
  `Sparkles-Inferences: stale; commits-since=3`, or `unknown` when `stale` is `null`.
  Fresh inferences send no header. The header is informational; the body is unchanged.

`POST /$/reason/{ds}` keeps its current contract and adds a re-run form.
`{"rerun": true}` (or `?rerun=true`) re-runs the recorded profile, including the stored
custom rules text. It returns 409 `"no recorded reasoning to re-run"` when there is no
status.

### 2.2 Automatic re-materialization (opt-in)

```
sparkles serve --auto-reason 5 [--auto-reason-max-delay 60]
```

With the flag, a background loop checks each dataset that has a reasoning status once
per second. When `stale == true`, the dataset's `head` has not changed for
`--auto-reason` seconds (the debounce), and no `reason` task for that dataset is
running, the loop starts a `reason` task with the recorded profile. The task message is
prefixed `auto:`.

`--auto-reason-max-delay` (default 12 × the debounce) forces a run when writes never
pause that long. After a failed automatic run, the loop waits for the next change of
`head` before trying again.

Automatic runs are disabled when:

- the server is `--read-only`;
- the dataset's status has `stale == null`, since the loop never guesses.

The default is **off**. `materialize` holds the store's writer lock for the whole run
(`sparkles-reasoner/src/lib.rs`), so every update stalls while it runs. Each run is a
full recomputation, O(default graph), and can rebuild a generation. An operator must
opt into that latency. The UI shows whether auto mode is on.

### 2.3 Diagnostics

`GET /$/reason/{ds}/diagnostics`. Parameters:

| Param | Default | Meaning |
|---|---|---|
| `checks` | all Phase 1 checks | comma list of check ids (§4.3) |
| `limit` | 100 (max 10000) | findings per check |
| `reasoning` | `true` if inferences exist | include `urn:x-sparkles:inferred` |
| `closure` | `subclass` | `subclass`: type tests follow `rdfs:subClassOf*` (sound under RDFS/OWL 2 RL `cax-sco`); `none`: only stated types |
| `timeout` | server query timeout | whole report |

Response `200` (whatever the findings):

```ts
type DiagnosticsReport = {
  diagnosticsFormat: 1;
  dataset: string; commit: number /* snapshot seq */; computedAt: string;
  scope: { graph: "default";            // the query default graph, as queries see it
           inferences: { included: boolean; profile?: string; stale?: boolean | null; commitsSince?: number | null };
           closure: "subclass" | "none" };
  status: "violations-found" | "none-found" | "incomplete";
  note: "Checks a fixed subset of OWL 2 RL inconsistency rules; 'none-found' does not establish OWL consistency.";
  checks: { id: string; rules: string[]; severity: "inconsistency" | "warning";
            status: "violations" | "none" | "truncated" | "timeout" | "error";
            findings: number; millis: number; error?: string }[];
  findings: {
    check: string; rule: string; severity: "inconsistency" | "warning";
    focus: Term;                        // Term as in docs/API.md
    evidence: Record<string, Term | Term[]>;
    basis: "asserted" | "uses-inferences";
    message: string;
  }[];
};
```

The overall `status` is decided in this order:

1. `violations-found` if any check with `severity: "inconsistency"` has findings;
2. `incomplete` if any check timed out or errored;
3. `none-found` otherwise.

Warnings never make the status `violations-found`.

Errors:

- 400: an unknown check id, or a bad `limit`/`closure`;
- 404: the dataset is unknown;
- 501: built without the `reasoning` feature.

`408` is **not** used: a timeout marks the remaining checks `timeout`, and the report
says `incomplete`.

### 2.4 CLI

```
sparkles infer --loc DB --status                    # prints the ReasoningStatus as text
sparkles infer --loc DB --check [--checks a,b] [--limit N] [--no-inferences]
               [--closure subclass|none] [--format text|json]
sparkles infer --loc DB --profile owl-rl --check    # materialize, then check
```

`sparkles stats` prints a line like
`reasoning       owl-rl, 1234 inferred at commit 40 (STALE: 3 commits since)`.

`--check` exits with:

- 0: `none-found`;
- 1: `violations-found`, like `sparkles shacl`;
- 2: `incomplete` or an error.

### 2.5 UI (dataset page, Reasoning panel)

- **Status line and badge.**
  - **Up to date** (green): "owl-rl · 1,234 inferred · at commit 40 · 2 h ago".
  - **Stale · 3 commits since** (amber), with a primary **Re-run reasoning** button
    that calls `POST /$/reason/{ds}` with `{"rerun": true}`.
  - **Freshness unknown** (grey) for legacy status. The re-run button is shown there
    too.
- **Auto mode.** When it is on: "Auto re-run: on (5 s after the last update)" and, when
  scheduled, "next run in 3 s".
- **Diagnostics sub-panel.**
  - A "Run checks" button, check toggles and a closure toggle.
  - A permanent scope banner with the `note` text. When inferences are stale, the
    banner adds "inferences are stale: findings marked *uses inferences* may be
    outdated".
  - Findings table: check, rule id (linked to the OWL 2 RL rule table in the docs),
    focus (click opens the explorer), evidence terms, a basis badge, and the message.
  - Truncated checks show "first 100 of more" with a "Load 1000" action (same request
    with a higher `limit`).
- **Query page.** When the response carries `Sparkles-Inferences: stale…`, a thin
  notice appears above the results: "Includes inferences that are 3 commits out of
  date." It links to the dataset page.

## 3. Standards basis

- **OWL 2 Web Ontology Language Profiles, §4.3** (W3C Rec). These rules have `false` in
  the head: `eq-diff1`, `eq-diff2`, `eq-diff3`, `prp-irp`, `prp-asyp`, `prp-pdw`,
  `prp-adp`, `prp-npa1`, `prp-npa2`, `cls-nothing2`, `cls-com`, `cls-maxc1`,
  `cls-maxqc1`, `cls-maxqc2`, `cax-dw`, `cax-adc`, `dt-not-type`. Theorem PR1 states
  that the OWL 2 RL/RDF rules are sound. Every Phase 1 check is one of these rules, or
  a composition with rules the same table lists (`eq-ref`, `eq-sym`, `eq-trans`,
  `cax-sco`, `prp-fp`, `dt-diff`). A finding is therefore a genuine inconsistency of the
  input graph. The converse does not hold, which is why the report never claims
  consistency.
- **RDF 1.1 Semantics / RDFS entailment** (`rdfs9`, `rdfs11`): justify
  `closure=subclass`.
- **SPARQL 1.1 Query**: the checks are SPARQL SELECTs, and literal inequality uses
  SPARQL `!=`. A type error means "not different", so the functional-literal check can
  only under-report.
- **SHACL** (W3C Rec): the result vocabulary (`sh:focusNode`, `sh:resultSeverity`,
  `sh:resultMessage`, `sh:sourceConstraintComponent`) is reused for the Phase 2 Turtle
  rendering.

## 4. Semantics

### 4.1 Store position and staleness

- **Position.** The store position is the CI spec's `seq`. It is gap-free and strictly
  increasing, one per data-changing commit. It survives restart and compaction, and
  compaction does not advance it. `head` is `Store::head_commit().seq`.
- **Recording.** A materialization records the `seq` of **its own commit**
  (`Receipt.commit.seq`, kind `reason`), together with the dataset id. If the run
  changed nothing (`Receipt.committed == false`), it records the unchanged head, at
  which it read the data.
- **Deriving the status** from `head` and the recorded `commit`:

  | Recorded `commit` | Condition | `stale` | `commitsSince` |
  |---|---|---|---|
  | absent, or recorded under another dataset id or another position source (§5.1) | any | `null` | `null` |
  | present | `head == commit` | `false` | 0 |
  | present | `head > commit` | `true` | `head − commit` |
  | present | `head < commit` (restored or replaced database) | `true` | `null`; `staleReason: "store position moved backwards"` |
  | any, with `inheritedStale: true` (written by a [C06](C06-clone-to-sandbox.md) clone) | any | `true` | `null`; `staleReason: "inherited from source at clone time"` |

- **Phase 1 is conservative.** Any commit makes the inferences stale, including
  commits that only touch named graphs the reasoner does not read. Phase 2 narrows
  this to commits touching the default graph or the inferred graph (§6).
- **Clear.** `DELETE /$/reason/{ds}` removes the status, as today.

### 4.2 Diagnostics scope and soundness

- **Data.** The checks run on one snapshot, over the query default graph. With
  `reasoning=true` the default graph is extended by `urn:x-sparkles:inferred`
  (`QueryOptions::default_graph_extra`, as queries do).
- **Stale inferences.** They may contain entailments of deleted triples, so
  every finding is re-checked with the same bindings over asserted data only
  (an ASK with `reasoning=false`). `basis` is `asserted` when that ASK holds, and
  `uses-inferences` otherwise. With fresh inferences, both kinds are sound findings.
  With stale ones, only `asserted` findings are guaranteed for the current data. The
  UI and the CLI say so.
- **No unique name assumption.** Two different IRIs are never reported as different
  individuals unless `owl:differentFrom` or `owl:AllDifferent` says so.

### 4.3 Phase 1 checks

`T(x, C)` means `?x rdf:type ?C`, or `?x rdf:type/rdfs:subClassOf* ?C` with
`closure=subclass`. `S(x, y)` means `?x (owl:sameAs|^owl:sameAs)* ?y`, which covers
`eq-ref`, `eq-sym` and `eq-trans`.

| id | Rules | Severity | Condition | Evidence |
|---|---|---|---|---|
| `nothing-member` | `cls-nothing2` (+`cax-sco`) | inconsistency | `T(x, owl:Nothing)` | `type`: the stated type |
| `disjoint-classes` | `cax-dw` | inconsistency | `c1 owl:disjointWith c2` (either direction), `T(x,c1)`, `T(x,c2)`, reported once per `x` and unordered `{c1,c2}` | `classes: [c1, c2]` |
| `all-disjoint-classes` | `cax-adc` | inconsistency | `d a owl:AllDisjointClasses ; owl:members/rdf:rest*/rdf:first c1, c2`, `c1 ≠ c2`, `T(x,c1)`, `T(x,c2)` | `axiom: d`, `classes` |
| `same-different` | `eq-diff1` (+`eq-ref`, `eq-sym`, `eq-trans`) | inconsistency | `x owl:differentFrom y` and `S(x, y)`. This includes `x owl:differentFrom x` | `other: y` |
| `functional-literal-conflict` | `prp-fp` + `dt-diff` + `eq-diff1` | inconsistency | `p a owl:FunctionalProperty`, `x p v1`, `x p v2`, both literals, `v1 != v2` under SPARQL value comparison, reported once per unordered pair | `property: p`, `values: [v1, v2]` |
| `thing-empty` | OWL semantics: the domain is non-empty | inconsistency | `owl:Thing rdfs:subClassOf owl:Nothing` or `owl:Thing owl:equivalentClass owl:Nothing` (either direction) | `axiom` |
| `unsatisfiable-class` | lint | **warning** | `c rdfs:subClassOf+ owl:Nothing`, `c ≠ owl:Nothing`, and no member (members are reported by `nothing-member`) | `path` |

Not included in Phase 1 (candidates for Phase 2, same machinery):

- `prp-irp`, `prp-asyp`, `prp-pdw`, `prp-adp`;
- `cls-com`, `cls-maxc1`, `cls-maxqc1`/`cls-maxqc2`;
- `eq-diff2`/`eq-diff3` (`owl:AllDifferent`);
- `prp-npa1`/`prp-npa2`.

The following are never checked: `dt-not-type`, and anything needing
`owl:equivalentClass` or `owl:intersectionOf` reasoning beyond what the materialized
graph contains.

Two notes:

- **Literal value comparison.** `30` and `"30"^^xsd:decimal` are the same value, so no
  finding. `30` and `31` are different values, so a finding. An incomparable pair, for
  example `"1"^^xsd:integer` against `"1"`, raises a SPARQL type error, and the pair is
  not reported. Such a pair is inconsistent, but reporting it would need datatype
  reasoning, and staying silent keeps the check sound.
- **IRI values of a functional property** only entail `owl:sameAs`. They are reported
  only through `same-different` when they are also `owl:differentFrom`.

### 4.4 Limits and errors

- Each check is one SPARQL SELECT with `max_rows = 2·limit + 2`. Rows are deduplicated
  (for unordered pairs) in Rust. A check is `truncated` when more than `limit`
  findings remain.
- The checks share the report deadline. When a check starts after the deadline has
  passed, it is marked `timeout` without running.
- A SPARQL error in a check sets its status to `error` with the message. Such an error
  is a bug, and the report surfaces it rather than hiding it.
- Diagnostics are read-only, so they are allowed on `--read-only` servers. They take
  no writer lock.
- Configuration: `--auto-reason SECS` (default off) and `--auto-reason-max-delay SECS`.
  There is no per-dataset auto setting in Phase 1.

## 5. Design sketch

### 5.1 Store position

**Preferred: implement after CI Phase 1.** C08 then uses:

- `Store::head_commit().seq` for `head`;
- the `Receipt` from `WriteTxn::commit()` for the materialization's own commit. The
  reasoner uses `write_as(Reason)` / `write_as(ReasonClear)`, per the CI spec.
  Reading the head after committing would race with other writers;
- `Store::dataset_id()`, stored next to the position so that a clone or a re-created
  dataset never compares positions from another lineage.

**Stopgap, only if C08 must ship before CI.** `Snapshot::version` is not usable: it
counts WAL commits replayed since the current generation (so it resets at restart), and
pure compactions bump it in memory. The stopgap adds a durable counter:

- **Format.** `IndexMeta.data_version: u64` with `#[serde(default)]`.
- **`Store::open`.** `data_version = meta.data_version + committed WAL transactions
  replayed`.
- **`publish_log`.** `+1` when the log is non-empty.
- **`rebuild_locked`.** Takes `data_changed: bool`, which is false for `compact()`, and
  writes `snap.data_version + data_changed` into the new `meta.json`. That file is
  durable before `CURRENT` switches, so either the old generation and its WAL survive a
  crash, or the new generation with an empty WAL does. Both give the right counter.
- **Commit.** `WriteTxn::commit_data_version()` returns the value it published.

The status records `"positionSource": "data-version"` rather than `"commit"`. When CI
lands, its baseline commit restarts numbering, so statuses with the other source read
as `stale: null`. The UI shows "Freshness unknown" and offers a re-run. CI does not
reuse the stopgap field, and the stopgap uses none of the WAL spare bytes that CI
claims.

### 5.2 Reasoning status file

`reasoning.json` gains new fields. Every new field is `#[serde(default)]`, so older
files still parse:

```json
{ "reasoningFormat": 2, "profile": "owl-rl", "inferred": 1234, "at": "2026-09-30T10:00:00Z",
  "commit": 40, "positionSource": "commit", "datasetId": "3f1c9a2e-…",
  "rules": null, "warnings": [], "millis": 812 }
```

`positionSource` is `"commit"` (CI) or `"data-version"` (the stopgap). `datasetId` is
absent under the stopgap.

- `rules` holds the custom rules text for profile `rules`, so a re-run and auto mode
  can repeat it.
- `ReasoningInfo` in `state.rs` gets the same fields. The registry entry keeps
  embedding it.
- `write_reasoning_file` becomes atomic: temporary file, `sync_all`, rename, directory
  sync, the same pattern as `save_registry_locked`. Today it uses a plain
  `std::fs::write`, and a torn file silently loses the status.
- **Ordering.** The data commit comes first, then the status file. A crash in between
  leaves the old status with an older `commit`, which reads as *stale*: conservative.
  For `clear`, the data is removed first, then the file. A crash leaves a status whose
  `commit` is behind, so it also reads as stale.

### 5.3 Reasoner and server changes

**`sparkles-reasoner`**

- `materialize` returns its `Receipt` in `ReasonReport.receipt` (CI). Under the
  stopgap it returns the data version instead.
- New module `diagnostics.rs` with
  `pub fn diagnose(snap: Arc<Snapshot>, opts: &DiagnoseOptions) -> Result<DiagnosticsReport>`.
  It runs the check queries through `sparkles::sparql::query`, with
  `default_graph_extra = [INFERRED_GRAPH]` when inferences are included.
- The query texts live in `crates/sparkles-reasoner/diagnostics/*.rq`
  (`include_str!`), one file per check, with `closure` substituted as `rdf:type` or
  `rdf:type/rdfs:subClassOf*`. `docs/API.md` links them so the checks are auditable.
- The basis re-check is an ASK per finding with `initial_bindings`, and
  `default_graph_extra` empty.

**`sparkles-server`**

- `http.rs` routes: `get(reason_status)` on `/$/reason/{ds}`, and
  `/$/reason/{ds}/diagnostics`.
- `dataset_info` and `stats` add the status fields.
- `query_endpoint`/`shacl` add the `Sparkles-Inferences` header when
  `default_graph_extra` includes the inferred graph and the status is not fresh.
- `reason` accepts `rerun`.
- `state.rs`: an `auto_reason: Option<AutoReason { debounce, max_delay }>` on
  `AppState`, and one thread started by `serve`.
  - The thread keeps `HashMap<ds, (last_head, first_seen: Instant, stale_since: Instant)>`.
  - It skips datasets that have a running `reason` task (scan of `tasks`).
  - It uses `start_task` with the same closure as `reason`, factored into
    `fn run_reason(ds, profile, trigger)`.

**`main.rs`:** `Infer { status, check, checks, limit, no_inferences, closure, format }`.

**UI:**

- `api.ts`: `reasonStatus`, `diagnostics`, `rerunReasoning`, and the types.
- The Reasoning panel as in §2.5.
- `query` reads the response header.

## 6. Phasing

**Phase 1 (MVP, about one day):**

- the store position from CI (or the §5.1 stopgap if CI is not there);
- `ReasonReport.receipt`, `reasoning.json` v2 written atomically;
- `GET /$/reason/{ds}`, plus the status fields in `DatasetInfo` and `/$/stats`;
- the `Sparkles-Inferences` header;
- `rerun`;
- `--auto-reason` with debounce and max delay;
- diagnostics with the seven checks of §4.3, JSON only, with basis re-check;
- `sparkles infer --status/--check`;
- the UI badge, re-run button and diagnostics panel;
- tests for §7.

**Phase 2:**

- **Precise staleness.** This needs a CI extension, to be coordinated with that spec.
  CI's WAL commit record keeps bytes `27..29` reserved, and its catalog record has
  free flag bits in byte 45. Record one bit per commit: "touched the default graph or
  the inferred graph". The reasoner's input is only the default graph. The status then
  compares the last such commit (`inputCommit`) with the recorded `commit`, while
  `commitsSince` still counts all commits.
- **More checks:** `prp-irp`, `prp-asyp`, `prp-pdw`, `prp-adp`, `cls-com`,
  `cls-maxc1`, `cls-maxqc1`/`2`, `eq-diff2`/`3`, `prp-npa1`/`2`.
- **Turtle rendering** as `spx:DiagnosticsReport` with SHACL result properties.
  `sh:conforms` is omitted because it would overclaim.
- **Per-dataset auto setting**, stored in `reasoning.json`.
- **Task cancellation**, so a newer commit can supersede a running automatic run.

**Phase 3:** incremental materialization (DRed-style) to make auto mode cheap, and
diagnostics over chosen named graphs.

## 7. Acceptance examples

Dataset `t`; `ex:` = `http://ex.org/`.

**A. Freshness lifecycle:**

1. Bulk-load `ex:C rdfs:subClassOf ex:B . ex:x a ex:C .`, then `POST /$/reason/t`
   `{"profile":"rdfs"}` and wait for the task. Then `GET /$/reason/t` →
   `stale:false, commitsSince:0, commit:S, head:S`.
2. `INSERT DATA { ex:y a ex:C }` → `stale:true, commitsSince:1, head:S+1`.
   `SELECT * { ?s a ex:B }` responds with `Sparkles-Inferences: stale; commits-since=1`.
3. `POST /$/compact/t` → still `commitsSince:1`.
4. Restart the server → still `commitsSince:1`.
5. `POST /$/reason/t {"rerun":true}` → `stale:false`, `commit:S+2` (a commit of kind
   `reason`), and the query returns `ex:x` and `ex:y` with no header.

**B. Legacy or foreign status.** Write `reasoning.json` without `commit`, then restart →
`stale:null`, `commitsSince:null`, and the UI shows "Freshness unknown". Auto mode
never triggers on it. The same holds for a status whose `datasetId` differs from
`Store::dataset_id()`.

**C. Auto mode.** Run `serve --auto-reason 1 --auto-reason-max-delay 3`.

- One insert: within about 2 s a task `kind:"reason"` with message `auto: …` completes,
  and the status is fresh.
- Inserts every 200 ms for 6 s: at least one automatic run starts within 3 s of the
  first insert, and after the writes stop the status becomes fresh.
- With `--read-only`: no automatic run.

**D. Disjointness with closure:**

```turtle
ex:Cat owl:disjointWith ex:Dog . ex:Kitten rdfs:subClassOf ex:Cat .
ex:tom a ex:Kitten, ex:Dog .
```

With no reasoning, `GET /$/reason/t/diagnostics` gives `status:"violations-found"` and
one finding:

```json
{"check":"disjoint-classes","rule":"cax-dw","severity":"inconsistency",
 "focus":{"type":"uri","value":"http://ex.org/tom"},
 "evidence":{"classes":[{"type":"uri","value":"http://ex.org/Cat"},{"type":"uri","value":"http://ex.org/Dog"}]},
 "basis":"asserted","message":"ex:tom is an instance of the disjoint classes ex:Cat and ex:Dog"}
```

With `closure=none`: `status:"none-found"`, which shows that the scope is explicit.

**E. `owl:Nothing`:**

- `ex:x a owl:Nothing .` → `nothing-member`, rule `cls-nothing2`.
- `ex:E rdfs:subClassOf owl:Nothing .` with no members → an `unsatisfiable-class`
  warning, and `status:"none-found"`.
- Adding `ex:e a ex:E` → a `nothing-member` finding with evidence `type: ex:E`, and
  `violations-found`.

**F. sameAs / differentFrom:**

- `ex:a owl:sameAs ex:b . ex:b owl:differentFrom ex:a .` → one `same-different`
  finding.
- `ex:c owl:differentFrom ex:c .` → one finding.
- `ex:d owl:sameAs ex:e . ex:e owl:sameAs ex:f . ex:d owl:differentFrom ex:f .` → one
  finding (transitive path).

**G. Functional literals:**

```turtle
ex:age a owl:FunctionalProperty .
ex:p ex:age 30, "30"^^xsd:decimal, 31 .
ex:q ex:age ex:v1, ex:v2 .
```

Two findings, both focused on `ex:p`: the value pairs `{30, 31}` and
`{"30"^^xsd:decimal, 31}`. There is no finding for the pair `30` and `30.0`, and none
for `ex:q` (IRIs, no unique name assumption).

**H. AllDisjointClasses:**
`[] a owl:AllDisjointClasses ; owl:members (ex:A ex:B ex:C) . ex:z a ex:A, ex:C .` →
one finding with `classes [A, C]`.

**I. Stale basis:**

- Load `ex:K rdfs:subClassOf ex:Cat . ex:Cat owl:disjointWith ex:Dog . ex:k a ex:K, ex:Dog .`
  and materialize rdfs.
- Run `DELETE DATA { ex:k a ex:K }`.
- Diagnostics with `closure=none`: the finding for `ex:k` is still reported through
  the stale inferred `ex:k a ex:Cat`, with `basis:"uses-inferences"`. The scope shows
  `stale:true`, and the UI banner warns.
- After `rerun` there is no finding.

**J. Truncation.** 150 individuals, each typed `ex:Cat` and `ex:Dog`. With `limit=100`,
the check has `status:"truncated"` and `findings:100`, and the overall status is
`violations-found`. With `limit=1000`, all 150 are reported.

**K. `thing-empty`:** `owl:Thing rdfs:subClassOf owl:Nothing .` → one inconsistency
finding.

**L. CLI:**

- `sparkles infer --loc db --check` on data D → exit 1, and the text output lists
  `cax-dw ex:tom …`.
- On clean data → exit 0 and prints `none-found (7 checks; this is not a full OWL
  consistency check)`.
- `--checks bogus` → exit 2.
- `sparkles infer --loc db --status` after an update prints `STALE (1 commit since)`.

## 8. Rejected alternatives

- **Timestamp-based freshness** (compare `at` with the last write time). Clocks move,
  and write times are not recorded durably. A commit counter is exact.
- **Using `Snapshot::version`.** It is not durable, and compaction changes it (§5.1).
- **Auto re-materialization on by default.** It holds the writer lock and does a full
  recomputation after every burst of writes. That is surprising latency for a feature
  users did not ask for.
- **Re-materializing synchronously inside each update request.** Same cost, and it
  makes update latency unbounded.
- **Inconsistency rules inside the forward rule set** (Jena `rb:violation`-style
  derived facts). That would mix diagnostics into the inferred graph, and they would
  go stale with it. It would also need a violation vocabulary in the data. Separate
  SPARQL checks keep them explicit, auditable and on demand.
- **Full OWL consistency checking** with a DL tableau reasoner. Out of scope, and it
  would contradict the "never claim consistency" rule of a subset checker.
- **Reusing `sh:ValidationReport` with `sh:conforms`.** `sh:conforms true` would read
  as a guarantee.

## 9. Open questions

1. Should `closure` default to `none` when fresh materialized inferences exist, since
   RDFS already provides the closure? The spec always keeps `subclass`, which is sound
   and makes the checks useful without reasoning.
2. Should the `Sparkles-Inferences` header also be sent on fresh results
   (`fresh; commit=40`)? The spec sends it only when not fresh, to keep responses
   unchanged in the common case.
3. `unsatisfiable-class` is a warning, not an inconsistency. Should warnings be off by
   default?
4. Should auto mode be allowed per dataset in Phase 1? The spec makes it server-wide to
   keep the MVP small.
5. Should the §5.1 stopgap be skipped entirely, making C08 wait for CI Phase 1? The
   spec prefers that order. The stopgap exists only so that C08 is not blocked.

## 10. Sources

- Sparkles repository:
  - [CI-commit-identity.md](CI-commit-identity.md) (a sibling clean-room spec): `seq`, `head`,
    `Receipt`, `write_as(Reason)`, the dataset id, the WAL commit-record and catalog
    layouts.
  - `crates/sparkles-reasoner/src/lib.rs`: `materialize` (writer lock for the whole
    run, default-graph input, bulk commit), `clear`, `ReasonReport`.
  - `crates/sparkles-reasoner/rules/owl-rl.rules`: the covered and uncovered rules.
  - `crates/sparkles/src/store.rs`: `Snapshot::version`, WAL record layout
    (`WAL_REC`, `WAL_COMMIT`, spare bytes), `Store::open` replay, `rebuild_locked`,
    `WriteTxn::commit`/`publish_log`.
  - `crates/sparkles/src/builder.rs`: `IndexMeta`, `FORMAT_VERSION`.
  - `crates/sparkles/src/sparql/mod.rs`: `QueryOptions::{default_graph_extra,
    initial_bindings, max_rows}`, `query`.
  - `crates/sparkles-server/src/state.rs`: `ReasoningInfo`, `read_reasoning_file`/
    `write_reasoning_file`, registry, tasks.
  - `crates/sparkles-server/src/http.rs`: `reason`/`unreason`, `query_options`,
    `dataset_info`, `stats`.
  - `crates/sparkles-server/src/main.rs`: `infer`, `stats`.
  - `ui/src/routes/datasets/[name]/+page.svelte`: the Reasoning panel.
  - `README.md` (known gaps: "must be re-run after updates; no inconsistency
    detection"), `docs/API.md`.
- W3C, *OWL 2 Web Ontology Language Profiles (Second Edition)*, §4.3, rule tables and
  Theorem PR1: https://www.w3.org/TR/owl2-profiles/. Fetched for this spec to confirm
  the list of rules with a `false` conclusion.
- W3C RDF 1.1 Semantics (RDFS entailment rules), SPARQL 1.1 Query (operator mapping
  and `!=` type errors), SHACL (result vocabulary). Cited from working knowledge; not
  re-fetched.
- Apache Jena documentation on `rb:violation`-style validation in rule reasoners. Cited
  from working knowledge as a rejected alternative only; not re-fetched.
- Fluree was **not** consulted: no code, documentation, or product pages.

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30, after durable commit identity, so freshness
is based on commit `seq` and the dataset id from the start; the §5.1 data-version
stopgap was never built (open question 5). What shipped:

- a versioned `reasoning.json`, written atomically, recording the run's own commit (or
  the unchanged head it read) and the dataset id;
- `GET /$/reason/{ds}`, with the same status in dataset info and `/$/stats`;
- the `Sparkles-Inferences` header on queries and SHACL validations that include stale
  or unknown inferences;
- `POST /$/reason/{ds}` with `{"rerun": true}`, custom rules included;
- `serve --auto-reason SECS [--auto-reason-max-delay SECS]`, off by default, never for
  unknown freshness or on read-only servers;
- `sparkles_reasoner::diagnostics` with the seven checks of §4.3, one SPARQL file per
  check under `crates/sparkles-reasoner/diagnostics/` (linked from the API reference),
  and the basis re-check against asserted data;
- `sparkles infer --status` / `--check`, a reasoning line in `sparkles stats`, and the
  UI's reasoning panel with a freshness badge, a re-run button, the auto-mode line and a
  diagnostics sub-panel.

**Decisions.** Automatic re-reasoning stays off by default, and the status keeps the
canonical profile name (both decided by the maintainer). The other open questions kept
the spec's proposals: `closure=subclass` by default, the header only when inferences are
not fresh, the `unsatisfiable-class` warning included in the default run, and auto mode
as a server-wide setting.

**Later additions.** Clones rebase the status ([C06](C06-clone-to-sandbox.md)).
GeoSPARQL added optional `vocabularies` and `geoDefaultGeometry` fields to the status
([G01](G01-geosparql.md)), and write-time validation can refuse a reasoning run's
commit, with the reason in the task's error ([C10](C10-write-time-validation.md)).

**Not built.** Phase 2: precise staleness from the graphs a commit touched (any commit
still makes inferences stale), the further OWL 2 RL checks (`prp-irp`, `prp-asyp`,
`prp-pdw`, `prp-adp`, `cls-com`, `cls-maxc1`, `cls-maxqc1`/`2`, `eq-diff2`/`3`,
`prp-npa1`/`2`), the Turtle rendering of the report, a per-dataset auto setting, and
superseding a running automatic run. Phase 3: incremental (DRed-style) materialization
and diagnostics over chosen named graphs.
