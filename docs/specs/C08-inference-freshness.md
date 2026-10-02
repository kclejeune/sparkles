# C08: Inference freshness and inconsistency diagnostics

> **Status:** implemented in part
>
> **Phases:** Phase 1 shipped. It covers commit-based freshness, `GET /$/reason/{ds}`,
> the `Sparkles-Inferences` header, re-runs, opt-in automatic re-materialization, the
> seven diagnostics checks, `sparkles infer --status/--check` and the UI panel. It is
> built on durable commit identity, so the §5.1 stopgap was never needed. Phase 2
> shipped as well: staleness limited to default-graph commits, the remaining OWL 2 RL
> checks, the Turtle report, a per-dataset auto setting and superseded automatic runs.
> Phase 3 is not built.
>
> **User docs:** [API: Reasoning status and diagnostics](../API.md#reasoning-status-and-diagnostics) ·
> [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This is a clean-room spec. It builds on the durable commit sequence `seq` from the commit
identity spec ([CI](CI-commit-identity.md)). It uses `Snapshot::commit`,
`Store::head_commit()` and the `Receipt` that `WriteTxn::commit()` returns. In case C08 is
implemented first, §5.1 defines a stopgap counter.

## 1. Summary

Sparkles materializes entailments into the graph `urn:x-sparkles:inferred`. The library
call is `sparkles_reasoner::materialize`, the endpoint is `POST /$/reason/{ds}` and the
command is `sparkles infer`. A run records `{profile, inferred, at}` in
`<db>/reasoning.json` and in the registry. Queries include the inferred graph by default.
Two things are missing:

1. **Freshness is invisible.** After an update, the inferences can lack new entailments
   or keep entailments of deleted triples. Nothing tells the user, the UI or the CLI. The
   README only says "must be re-run after updates".
2. **Inconsistencies go undetected.** The OWL 2 RL rule set leaves inconsistency
   detection out. The header of `rules/owl-rl.rules` says "Not covered: validation /
   inconsistency detection". A contradiction such as an individual in two disjoint
   classes goes unnoticed.

**Goals**

- Record the store position at which the inferences were generated.
- Report `stale` and `commitsSince` in a new status endpoint, in `DatasetInfo`, in
  `/$/stats`, in the CLI, and as a response header on queries that use inferences.
- Offer debounced automatic re-materialization as an opt-in. It is **off** by default.
- Offer an explicit diagnostics report (`GET /$/reason/{ds}/diagnostics`,
  `sparkles infer --check`). It checks a documented, sound subset of the OWL 2 RL
  inconsistency rules with SPARQL over data ∪ inferences.

**Non-goals**

- Claiming OWL consistency. The report can say "violations found" or "none of these
  checks found a violation", never "consistent".
- Incremental maintenance with DRed or similar counting algorithms.
- Explanations beyond the matched evidence.
- Datatype reasoning (`dt-not-type`).
- Checking named graphs other than the default graph.

## 2. User-visible behavior

### 2.1 Reasoning status

The new route `GET /$/reason/{ds}` returns the `ReasoningStatus` below. When nothing is
materialized, it returns `{"reasoning": null, "head": 17}`.

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

The status also appears in three other places:

- `DatasetInfo.reasoning` gains `commit`, `stale` and `commitsSince`. The existing
  `profile`, `inferred` and `at` fields do not change.
- `DatasetStats` gains `reasoning: ReasoningStatus | null`.
- **A query response header.** When a SPARQL query or a SHACL validation includes the
  inferred graph and the status is stale, the response carries
  `Sparkles-Inferences: stale; commits-since=3`. When `stale` is `null`, the value is
  `unknown`. Fresh inferences send no header. The header is informational and the body
  does not change.

`POST /$/reason/{ds}` keeps its current contract and gains a re-run form. The body
`{"rerun": true}`, or the parameter `?rerun=true`, re-runs the recorded profile with the
stored custom rules text. With no recorded status, it returns 409
`"no recorded reasoning to re-run"`.

### 2.2 Automatic re-materialization (opt-in)

```
sparkles serve --auto-reason 5 [--auto-reason-max-delay 60]
```

With the flag set, a background loop checks every dataset that has a reasoning status
once per second. It starts a `reason` task with the recorded profile when all of these
hold:

- `stale == true`;
- the dataset's `head` has not changed for `--auto-reason` seconds (the debounce);
- no `reason` task for that dataset is running.

The task message starts with `auto:`.

`--auto-reason-max-delay` forces a run when writes never pause for the debounce period.
It defaults to 12 × the debounce. After a failed automatic run, the loop waits for the
next change of `head` before it tries again.

Automatic runs never happen on a `--read-only` server, or for a dataset whose status has
`stale == null`. The loop does not guess.

The default is **off** because each run is costly. `materialize` holds the store's writer
lock for the whole run (`sparkles-reasoner/src/lib.rs`), so every update stalls until it
finishes. Each run recomputes everything, costs O(default graph) and can rebuild a
generation. An operator has to opt into that latency. The UI shows whether auto mode is
on.

### 2.3 Diagnostics

`GET /$/reason/{ds}/diagnostics` takes these parameters:

| Param | Default | Meaning |
|---|---|---|
| `checks` | all Phase 1 checks | Comma-separated check ids (§4.3). |
| `limit` | 100 (max 10000) | Findings reported per check. |
| `reasoning` | `true` if inferences exist | Whether to include `urn:x-sparkles:inferred`. |
| `closure` | `subclass` | With `subclass`, type tests follow `rdfs:subClassOf*`, which is sound under RDFS and OWL 2 RL `cax-sco`. With `none`, only stated types count. |
| `timeout` | server query timeout | Deadline for the whole report. |

The response is `200` whatever the findings:

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

The overall `status` is the first of these that applies:

1. `violations-found` if any check with `severity: "inconsistency"` has findings;
2. `incomplete` if any check timed out or failed;
3. `none-found` otherwise.

Warnings never make the status `violations-found`.

Errors:

- 400: an unknown check id, or a bad `limit` or `closure`;
- 404: an unknown dataset;
- 501: a build without the `reasoning` feature.

There is no `408`. A timeout marks the remaining checks `timeout`, and the report says
`incomplete`.

### 2.4 CLI

```
sparkles infer --loc DB --status                    # prints the ReasoningStatus as text
sparkles infer --loc DB --check [--checks a,b] [--limit N] [--no-inferences]
               [--closure subclass|none] [--format text|json]
sparkles infer --loc DB --profile owl-rl --check    # materialize, then check
```

`sparkles stats` prints a line such as
`reasoning       owl-rl, 1234 inferred at commit 40 (STALE: 3 commits since)`.

`--check` exits with:

- 0 for `none-found`;
- 1 for `violations-found`, like `sparkles shacl`;
- 2 for `incomplete` or an error.

### 2.5 UI (dataset page, Reasoning panel)

- **Status line and badge.**
  - **Up to date** (green): "owl-rl · 1,234 inferred · at commit 40 · 2 h ago".
  - **Stale · 3 commits since** (amber), with a primary **Re-run reasoning** button
    that calls `POST /$/reason/{ds}` with `{"rerun": true}`.
  - **Freshness unknown** (grey) for a legacy status. It also shows the re-run button.
- **Auto mode.** When auto mode is on, the panel shows "Auto re-run: on (5 s after the
  last update)". When a run is scheduled, it adds "next run in 3 s".
- **Diagnostics sub-panel.**
  - A "Run checks" button, check toggles and a closure toggle.
  - A permanent scope banner with the `note` text. When the inferences are stale, the
    banner adds "inferences are stale: findings marked *uses inferences* may be
    outdated".
  - A findings table with the check, the rule id, the focus, the evidence terms, a basis
    badge and the message. The rule id links to the OWL 2 RL rule table in the docs.
    Clicking the focus opens the explorer.
  - A truncated check shows "first 100 of more" and a "Load 1000" action, which repeats
    the request with a higher `limit`.
- **Query page.** When the response carries `Sparkles-Inferences: stale…`, a thin
  notice appears above the results: "Includes inferences that are 3 commits out of
  date." It links to the dataset page.

## 3. Standards basis

- **OWL 2 Web Ontology Language Profiles, §4.3** (W3C Rec). These rules have `false` in
  the head: `eq-diff1`, `eq-diff2`, `eq-diff3`, `prp-irp`, `prp-asyp`, `prp-pdw`,
  `prp-adp`, `prp-npa1`, `prp-npa2`, `cls-nothing2`, `cls-com`, `cls-maxc1`,
  `cls-maxqc1`, `cls-maxqc2`, `cax-dw`, `cax-adc`, `dt-not-type`. Theorem PR1 states
  that the OWL 2 RL/RDF rules are sound. Every Phase 1 check is one of these rules, or
  combines one with rules from the same tables (`eq-ref`, `eq-sym`, `eq-trans`,
  `cax-sco`, `prp-fp`, `dt-diff`). A finding is therefore a genuine inconsistency of the
  input graph. The converse does not hold, so the report never claims consistency.
- **RDF 1.1 Semantics, RDFS entailment.** The rules `rdfs9` and `rdfs11` justify
  `closure=subclass`.
- **SPARQL 1.1 Query.** The checks are SPARQL SELECTs, and literal inequality uses
  SPARQL `!=`. A type error counts as "not different", so the functional-literal check
  can only under-report.
- **SHACL** (W3C Rec). The Phase 2 Turtle rendering reuses its result vocabulary:
  `sh:focusNode`, `sh:resultSeverity`, `sh:resultMessage` and
  `sh:sourceConstraintComponent`.

## 4. Semantics

### 4.1 Store position and staleness

- **Position.** The store position is `seq` from the CI spec. It is gap-free and
  strictly increasing, with one value per data-changing commit. It survives restart and
  compaction, and compaction does not advance it. `head` is `Store::head_commit().seq`.
- **Recording.** A materialization records the dataset id and the `seq` of **its own
  commit** (`Receipt.commit.seq`, of kind `reason`). A run that changed nothing
  (`Receipt.committed == false`) records the unchanged head, which is where it read the
  data.
- **Deriving the status.** The status follows from `head` and the recorded `commit`:

  | Recorded `commit` | Condition | `stale` | `commitsSince` |
  |---|---|---|---|
  | Absent, or recorded under another dataset id or another position source (§5.1) | any | `null` | `null` |
  | Present | `head == commit` | `false` | 0 |
  | Present | `head > commit` | `true` | `head − commit` |
  | Present | `head < commit` (a restored or replaced database) | `true` | `null`, with `staleReason: "store position moved backwards"` |
  | Any, with `inheritedStale: true`, which a [C06](C06-clone-to-sandbox.md) clone writes | any | `true` | `null`, with `staleReason: "inherited from source at clone time"` |

- **Phase 1 is conservative.** Any commit makes the inferences stale, including a
  commit that only touches named graphs the reasoner does not read. Phase 2 narrows this
  to commits that touch the default graph or the inferred graph (§6).
- **Clearing.** `DELETE /$/reason/{ds}` removes the status, as it does today.

### 4.2 Diagnostics scope and soundness

- **Data.** The checks run on one snapshot, over the query default graph. With
  `reasoning=true`, the default graph also includes `urn:x-sparkles:inferred` through
  `QueryOptions::default_graph_extra`, the same way queries include it.
- **Stale inferences.** Stale inferences can contain entailments of deleted triples. So
  every finding is checked again with the same bindings against asserted data only, as
  an ASK with `reasoning=false`. `basis` is `asserted` when that ASK holds and
  `uses-inferences` otherwise. With fresh inferences, both kinds of finding are sound.
  With stale inferences, only `asserted` findings are guaranteed to hold for the current
  data, and the UI and the CLI say so.
- **No unique name assumption.** Two different IRIs are never reported as different
  individuals unless `owl:differentFrom` or `owl:AllDifferent` says so.

### 4.3 Phase 1 checks

`T(x, C)` means `?x rdf:type ?C`, or `?x rdf:type/rdfs:subClassOf* ?C` with
`closure=subclass`. `S(x, y)` means `?x (owl:sameAs|^owl:sameAs)* ?y`, which covers
`eq-ref`, `eq-sym` and `eq-trans`.

| id | Rules | Severity | Condition | Evidence |
|---|---|---|---|---|
| `nothing-member` | `cls-nothing2` (+`cax-sco`) | inconsistency | `T(x, owl:Nothing)` | `type`, the stated type |
| `disjoint-classes` | `cax-dw` | inconsistency | `c1 owl:disjointWith c2` in either direction, with `T(x,c1)` and `T(x,c2)`. Reported once per `x` and unordered `{c1,c2}`. | `classes: [c1, c2]` |
| `all-disjoint-classes` | `cax-adc` | inconsistency | `d a owl:AllDisjointClasses ; owl:members/rdf:rest*/rdf:first c1, c2`, with `c1 ≠ c2`, `T(x,c1)` and `T(x,c2)` | `axiom: d`, `classes` |
| `same-different` | `eq-diff1` (+`eq-ref`, `eq-sym`, `eq-trans`) | inconsistency | `x owl:differentFrom y` and `S(x, y)`. This includes `x owl:differentFrom x`. | `other: y` |
| `functional-literal-conflict` | `prp-fp` + `dt-diff` + `eq-diff1` | inconsistency | `p a owl:FunctionalProperty`, `x p v1` and `x p v2`, where both values are literals and `v1 != v2` under SPARQL value comparison. Reported once per unordered pair. | `property: p`, `values: [v1, v2]` |
| `thing-empty` | OWL semantics (the domain is non-empty) | inconsistency | `owl:Thing rdfs:subClassOf owl:Nothing`, or `owl:Thing owl:equivalentClass owl:Nothing` in either direction | `axiom` |
| `unsatisfiable-class` | lint | **warning** | `c rdfs:subClassOf+ owl:Nothing`, `c ≠ owl:Nothing`, and `c` has no members. A class with members is reported by `nothing-member`. | `path` |

Phase 1 leaves out the following rules. They are candidates for Phase 2 and use the same
machinery:

- `prp-irp`, `prp-asyp`, `prp-pdw`, `prp-adp`;
- `cls-com`, `cls-maxc1`, `cls-maxqc1`/`cls-maxqc2`;
- `eq-diff2`/`eq-diff3` (`owl:AllDifferent`);
- `prp-npa1`/`prp-npa2`.

Two things are never checked: `dt-not-type`, and anything that needs
`owl:equivalentClass` or `owl:intersectionOf` reasoning beyond what the materialized
graph contains.

Two details of the functional-property check:

- **Literal value comparison.** `30` and `"30"^^xsd:decimal` are the same value and give
  no finding. `30` and `31` are different values and give a finding. An incomparable
  pair, such as `"1"^^xsd:integer` and `"1"`, raises a SPARQL type error and is not
  reported. Such a pair is inconsistent, but reporting it would need datatype reasoning.
  Staying silent keeps the check sound.
- **IRI values.** Two IRI values of a functional property only entail `owl:sameAs`.
  They are reported only by `same-different`, when they are also `owl:differentFrom`.

### 4.4 Limits and errors

- Each check is one SPARQL SELECT with `max_rows = 2·limit + 2`. Rust code deduplicates
  the rows for unordered pairs. A check is `truncated` when more than `limit` findings
  remain.
- The checks share the report's deadline. A check that would start after the deadline
  is marked `timeout` and does not run.
- A SPARQL error in a check sets the check's status to `error` and records the message.
  Such an error is a bug, so the report shows it instead of hiding it.
- Diagnostics only read, so they run on `--read-only` servers and take no writer lock.
- Configuration: `--auto-reason SECS` (default off) and `--auto-reason-max-delay SECS`.
  Phase 1 has no per-dataset auto setting.

## 5. Design sketch

### 5.1 Store position

**Preferred: implement after CI Phase 1.** C08 then uses:

- `Store::head_commit().seq` for `head`;
- the `Receipt` from `WriteTxn::commit()` for the materialization's own commit. Reading
  the head after committing would race with other writers. The reasoner writes with
  `write_as(Reason)` / `write_as(ReasonClear)`, as the CI spec defines;
- `Store::dataset_id()`, stored next to the position, so that a clone or a re-created
  dataset never compares positions from another lineage.

**Stopgap, only if C08 must ship before CI.** `Snapshot::version` cannot serve as the
position. It counts the WAL commits replayed since the current generation, so it resets
at restart, and pure compactions bump it in memory. The stopgap adds a durable counter
instead:

- **Format.** `IndexMeta.data_version: u64` with `#[serde(default)]`.
- **`Store::open`.** `data_version = meta.data_version + committed WAL transactions
  replayed`.
- **`publish_log`.** Adds 1 when the log is non-empty.
- **`rebuild_locked`.** Takes `data_changed: bool`, which is false for `compact()`, and
  writes `snap.data_version + data_changed` into the new `meta.json`. That file is
  durable before `CURRENT` switches. After a crash, either the old generation and its
  WAL survive or the new generation with an empty WAL does, and both give the right
  counter.
- **Commit.** `WriteTxn::commit_data_version()` returns the value it published.

A stopgap status records `"positionSource": "data-version"` instead of `"commit"`. When
CI lands, its baseline commit restarts the numbering, so a status recorded under the
stopgap reads as `stale: null`. The UI then shows "Freshness unknown" and offers a
re-run. CI does not reuse the stopgap field, and the stopgap uses none of the WAL spare
bytes that CI claims.

### 5.2 Reasoning status file

`reasoning.json` gains new fields. All of them are `#[serde(default)]`, so older files
still parse:

```json
{ "reasoningFormat": 2, "profile": "owl-rl", "inferred": 1234, "at": "2026-09-30T10:00:00Z",
  "commit": 40, "positionSource": "commit", "datasetId": "3f1c9a2e-…",
  "rules": null, "warnings": [], "millis": 812 }
```

`positionSource` is `"commit"` under CI and `"data-version"` under the stopgap. The
stopgap writes no `datasetId`.

- `rules` holds the custom rules text for the `rules` profile, so that a re-run or auto
  mode can repeat it.
- `ReasoningInfo` in `state.rs` gets the same fields, and the registry entry still
  embeds it.
- `write_reasoning_file` becomes atomic, with the same pattern as
  `save_registry_locked`: write a temporary file, `sync_all`, rename, then sync the
  directory. Today it calls a plain `std::fs::write`, and a torn file silently loses the
  status.
- **Ordering.** The data commit comes first, then the status file. A crash in between
  leaves the old status with an older `commit`, which reads as *stale*. That errs on the
  safe side. `clear` uses the same order: it removes the data first and the file second.
  A crash in between leaves a status whose `commit` is behind, which again reads as
  stale.

### 5.3 Reasoner and server changes

**`sparkles-reasoner`**

- With CI, `materialize` returns its `Receipt` in `ReasonReport.receipt`. Under the
  stopgap it returns the data version instead.
- A new module, `diagnostics.rs`, exposes
  `pub fn diagnose(snap: Arc<Snapshot>, opts: &DiagnoseOptions) -> Result<DiagnosticsReport>`.
  It runs the check queries through `sparkles::sparql::query` and sets
  `default_graph_extra = [INFERRED_GRAPH]` when inferences are included.
- The query texts live in `crates/sparkles-reasoner/diagnostics/*.rq`, one file per
  check, loaded with `include_str!`. The `closure` setting is substituted as `rdf:type`
  or `rdf:type/rdfs:subClassOf*`. `docs/API.md` links the files so that anyone can audit
  the checks.
- The basis re-check runs one ASK per finding, with `initial_bindings` set and
  `default_graph_extra` empty.

**`sparkles-server`**

- `http.rs` adds the routes `get(reason_status)` on `/$/reason/{ds}` and
  `/$/reason/{ds}/diagnostics`.
- `dataset_info` and `stats` add the status fields.
- `query_endpoint` and `shacl` add the `Sparkles-Inferences` header when
  `default_graph_extra` includes the inferred graph and the status is not fresh.
- `reason` accepts `rerun`.
- `state.rs` adds `auto_reason: Option<AutoReason { debounce, max_delay }>` to
  `AppState`, and `serve` starts one thread for it.
  - The thread keeps `HashMap<ds, (last_head, first_seen: Instant, stale_since: Instant)>`.
  - It skips datasets that have a running `reason` task, which it finds by scanning
    `tasks`.
  - It calls `start_task` with the same closure as `reason`, factored out into
    `fn run_reason(ds, profile, trigger)`.

**`main.rs`:** the `infer` command becomes
`Infer { status, check, checks, limit, no_inferences, closure, format }`.

**UI:**

- `api.ts` adds `reasonStatus`, `diagnostics`, `rerunReasoning` and their types.
- The Reasoning panel works as §2.5 describes.
- The `query` page reads the response header.

## 6. Phasing

**Phase 1 (MVP, about one day):**

- the store position from CI, or the §5.1 stopgap if CI is not there;
- `ReasonReport.receipt`, and `reasoning.json` v2 written atomically;
- `GET /$/reason/{ds}`, plus the status fields in `DatasetInfo` and `/$/stats`;
- the `Sparkles-Inferences` header;
- `rerun`;
- `--auto-reason` with debounce and max delay;
- diagnostics with the seven checks of §4.3, JSON only, with the basis re-check;
- `sparkles infer --status/--check`;
- the UI badge, re-run button and diagnostics panel;
- tests for §7.

**Phase 2:**

- **Precise staleness.** Each commit records one bit that says whether it touched the
  default graph or the inferred graph. The reasoner's only input is the default graph.
  The status then compares the last such commit (`inputCommit`) with the recorded
  `commit`, while `commitsSince` still counts all commits. This needs an extension to
  CI, coordinated with that spec. CI's WAL commit record keeps bytes `27..29` reserved,
  and its catalog record has free flag bits in byte 45.
- **More checks:** `prp-irp`, `prp-asyp`, `prp-pdw`, `prp-adp`, `cls-com`,
  `cls-maxc1`, `cls-maxqc1`/`2`, `eq-diff2`/`3`, `prp-npa1`/`2`.
- **Turtle rendering** as `spx:DiagnosticsReport` with SHACL result properties. It
  omits `sh:conforms`, which would overclaim.
- **A per-dataset auto setting**, stored in `reasoning.json`.
- **Task cancellation**, so that a newer commit can supersede a running automatic run.

**Phase 3:** incremental, DRed-style materialization to make auto mode cheap, and
diagnostics over chosen named graphs.

## 7. Acceptance examples

The examples use dataset `t`, with `ex:` = `http://ex.org/`.

**A. Freshness lifecycle:**

1. Bulk-load `ex:C rdfs:subClassOf ex:B . ex:x a ex:C .`, then `POST /$/reason/t`
   `{"profile":"rdfs"}` and wait for the task. Then `GET /$/reason/t` →
   `stale:false, commitsSince:0, commit:S, head:S`.
2. `INSERT DATA { ex:y a ex:C }` → `stale:true, commitsSince:1, head:S+1`.
   `SELECT * { ?s a ex:B }` responds with `Sparkles-Inferences: stale; commits-since=1`.
3. `POST /$/compact/t` → still `commitsSince:1`.
4. Restart the server → still `commitsSince:1`.
5. `POST /$/reason/t {"rerun":true}` → `stale:false` and `commit:S+2`, a commit of kind
   `reason`. The query returns `ex:x` and `ex:y` with no header.

**B. Legacy or foreign status.** Write `reasoning.json` without `commit` and restart. The
status shows `stale:null` and `commitsSince:null`, and the UI shows "Freshness unknown".
Auto mode never triggers on it. A status whose `datasetId` differs from
`Store::dataset_id()` behaves the same way.

**C. Auto mode.** Run `serve --auto-reason 1 --auto-reason-max-delay 3`.

- One insert: within about 2 s, a task `kind:"reason"` with message `auto: …` completes
  and the status is fresh.
- Inserts every 200 ms for 6 s: at least one automatic run starts within 3 s of the
  first insert, and the status becomes fresh after the writes stop.
- With `--read-only`, no automatic run happens.

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

With `closure=none`, the result is `status:"none-found"`. This shows that the scope is
explicit.

**E. `owl:Nothing`:**

- `ex:x a owl:Nothing .` → `nothing-member`, rule `cls-nothing2`.
- `ex:E rdfs:subClassOf owl:Nothing .` with no members → an `unsatisfiable-class`
  warning and `status:"none-found"`.
- Adding `ex:e a ex:E` → a `nothing-member` finding with evidence `type: ex:E`, and
  `violations-found`.

**F. sameAs / differentFrom:**

- `ex:a owl:sameAs ex:b . ex:b owl:differentFrom ex:a .` → one `same-different`
  finding.
- `ex:c owl:differentFrom ex:c .` → one finding.
- `ex:d owl:sameAs ex:e . ex:e owl:sameAs ex:f . ex:d owl:differentFrom ex:f .` → one
  finding, through the transitive path.

**G. Functional literals:**

```turtle
ex:age a owl:FunctionalProperty .
ex:p ex:age 30, "30"^^xsd:decimal, 31 .
ex:q ex:age ex:v1, ex:v2 .
```

There are two findings, both with focus `ex:p`, for the value pairs `{30, 31}` and
`{"30"^^xsd:decimal, 31}`. The pair `30` and `"30"^^xsd:decimal` gives no finding. `ex:q` gives none
either, because its values are IRIs and there is no unique name assumption.

**H. AllDisjointClasses:**
`[] a owl:AllDisjointClasses ; owl:members (ex:A ex:B ex:C) . ex:z a ex:A, ex:C .` →
one finding with `classes [A, C]`.

**I. Stale basis:**

- Load `ex:K rdfs:subClassOf ex:Cat . ex:Cat owl:disjointWith ex:Dog . ex:k a ex:K, ex:Dog .`
  and materialize rdfs.
- Run `DELETE DATA { ex:k a ex:K }`.
- Run diagnostics with `closure=none`. The finding for `ex:k` is still reported, through
  the stale inferred triple `ex:k a ex:Cat`, with `basis:"uses-inferences"`. The scope
  shows `stale:true`, and the UI banner warns.
- After `rerun`, there is no finding.

**J. Truncation.** 150 individuals are each typed `ex:Cat` and `ex:Dog`. With
`limit=100`, the check has `status:"truncated"` and `findings:100`, and the overall
status is `violations-found`. With `limit=1000`, all 150 are reported.

**K. `thing-empty`:** `owl:Thing rdfs:subClassOf owl:Nothing .` → one inconsistency
finding.

**L. CLI:**

- `sparkles infer --loc db --check` on data D → exit 1, and the text output lists
  `cax-dw ex:tom …`.
- On clean data → exit 0, with the output `none-found (7 checks; this is not a full OWL
  consistency check)`.
- `--checks bogus` → exit 2.
- `sparkles infer --loc db --status` after an update prints `STALE (1 commit since)`.

## 8. Rejected alternatives

- **Timestamp-based freshness**, comparing `at` with the last write time. Clocks move,
  and write times are not recorded durably. A commit counter is exact.
- **Using `Snapshot::version`.** It is not durable, and compaction changes it (§5.1).
- **Auto re-materialization on by default.** It holds the writer lock and recomputes
  everything after every burst of writes. Users who did not ask for the feature would
  see surprising latency.
- **Re-materializing synchronously inside each update request.** It has the same cost,
  and it makes update latency unbounded.
- **Inconsistency rules inside the forward rule set**, as derived facts in the style of
  Jena's `rb:violation`. Diagnostics would then sit in the inferred graph and go stale
  with it, and the data would need a violation vocabulary. Separate SPARQL checks stay
  explicit, auditable and on demand.
- **Full OWL consistency checking** with a DL tableau reasoner. It is out of scope, and
  it would contradict the rule that a subset checker never claims consistency.
- **Reusing `sh:ValidationReport` with `sh:conforms`.** `sh:conforms true` would read
  as a guarantee.

## 9. Open questions

1. Should `closure` default to `none` when fresh materialized inferences exist, since
   RDFS already provides the closure? The spec always keeps `subclass`, which is sound
   and makes the checks useful without reasoning.
2. Should the `Sparkles-Inferences` header also be sent on fresh results
   (`fresh; commit=40`)? The spec sends it only when the inferences are not fresh, so
   responses stay unchanged in the common case.
3. `unsatisfiable-class` is a warning, not an inconsistency. Should warnings be off by
   default?
4. Should auto mode be allowed per dataset in Phase 1? The spec makes it server-wide to
   keep the MVP small.
5. Should the §5.1 stopgap be skipped entirely, so that C08 waits for CI Phase 1? The
   spec prefers that order. The stopgap exists only so that C08 is not blocked.

## 10. Sources

- Sparkles repository:
  - [CI-commit-identity.md](CI-commit-identity.md), a sibling clean-room spec: `seq`,
    `head`, `Receipt`, `write_as(Reason)`, the dataset id, and the layouts of the WAL
    commit record and the catalog.
  - `crates/sparkles-reasoner/src/lib.rs`: `materialize`, which holds the writer lock
    for the whole run, reads the default graph and commits in bulk. Also `clear` and
    `ReasonReport`.
  - `crates/sparkles-reasoner/rules/owl-rl.rules`: the covered and uncovered rules.
  - `crates/sparkles/src/store.rs`: `Snapshot::version`, the WAL record layout
    (`WAL_REC`, `WAL_COMMIT`, spare bytes), the replay in `Store::open`,
    `rebuild_locked`, and `WriteTxn::commit`/`publish_log`.
  - `crates/sparkles/src/builder.rs`: `IndexMeta`, `FORMAT_VERSION`.
  - `crates/sparkles/src/sparql/mod.rs`: `QueryOptions::{default_graph_extra,
    initial_bindings, max_rows}`, `query`.
  - `crates/sparkles-server/src/state.rs`: `ReasoningInfo`, `read_reasoning_file`/
    `write_reasoning_file`, the registry, tasks.
  - `crates/sparkles-server/src/http.rs`: `reason`/`unreason`, `query_options`,
    `dataset_info`, `stats`.
  - `crates/sparkles-server/src/main.rs`: `infer`, `stats`.
  - `ui/src/routes/datasets/[name]/+page.svelte`: the Reasoning panel.
  - `README.md`, whose known gaps said "must be re-run after updates; no inconsistency
    detection", and `docs/API.md`.
- W3C, *OWL 2 Web Ontology Language Profiles (Second Edition)*, §4.3, rule tables and
  Theorem PR1: https://www.w3.org/TR/owl2-profiles/. Fetched for this spec to confirm
  the list of rules with a `false` conclusion.
- W3C RDF 1.1 Semantics (RDFS entailment rules), SPARQL 1.1 Query (operator mapping
  and `!=` type errors) and SHACL (result vocabulary). Cited from working knowledge and
  not re-fetched.
- Apache Jena documentation on `rb:violation`-style validation in rule reasoners. Cited
  from working knowledge, only as a rejected alternative, and not re-fetched.
- Fluree was not consulted: no code, documentation or product pages.

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30, after durable commit identity. Freshness
was therefore based on commit `seq` and the dataset id from the start, and the §5.1
data-version stopgap was never built (open question 5). Phase 1 shipped:

- a versioned `reasoning.json`, written atomically, that records the dataset id and the
  run's own commit, or the unchanged head the run read;
- `GET /$/reason/{ds}`, with the same status in dataset info and `/$/stats`;
- the `Sparkles-Inferences` header on queries and SHACL validations that include stale
  or unknown inferences;
- `POST /$/reason/{ds}` with `{"rerun": true}`, custom rules included;
- `serve --auto-reason SECS [--auto-reason-max-delay SECS]`. It is off by default and
  never runs for unknown freshness or on read-only servers;
- `sparkles_reasoner::diagnostics`, with the seven checks of §4.3 and the basis re-check
  against asserted data. Each check is one SPARQL file under
  `crates/sparkles-reasoner/diagnostics/`, linked from the API reference;
- `sparkles infer --status` / `--check`, a reasoning line in `sparkles stats`, and the
  UI's reasoning panel with a freshness badge, a re-run button, the auto-mode line and a
  diagnostics sub-panel.

**Decisions.** The maintainer decided that automatic re-reasoning stays off by default
and that the status keeps the canonical profile name. The other open questions kept the
spec's proposals: `closure=subclass` by default, the header only when inferences are not
fresh, the `unsatisfiable-class` warning in the default run, and auto mode as a
server-wide setting.

**Later additions.** Clones rebase the status ([C06](C06-clone-to-sandbox.md)).
GeoSPARQL added the optional `vocabularies` and `geoDefaultGeometry` fields to the status
([G01](G01-geosparql.md)). Write-time validation can refuse a reasoning run's commit and
puts the reason in the task's error ([C10](C10-write-time-validation.md)).

**Phase 2.** Phase 2 landed on 2026-10-02 with these parts:

- **Precise staleness.** Each commit records one bit that says whether it may have
  changed the default graph. The catalog keeps it in flag bit 3 of byte 45, set when
  the default graph is known to be unchanged, so older records read as changed. A bulk
  commit keeps it in `commit.json`, and replay recomputes it from the WAL's data
  records. The WAL commit record therefore stays as CI defines it. The status is fresh
  while no commit after the recorded one has the bit, and `commitsSince` still counts
  every commit. Automatic runs follow the same rule.
- **More checks.** Nine checks cover the twelve rules of §4.3's candidate list. Rules
  that differ only in their vocabulary share a check: `all-different` reports `eq-diff2`
  or `eq-diff3`, `max-qualified-cardinality-zero` reports `cls-maxqc1` or `cls-maxqc2`,
  and `negative-property-assertion` reports `prp-npa1` or `prp-npa2`. List members are
  matched by position, so a member listed twice counts as two members, as the rules
  say. Cardinalities match the value 0 of any numeric datatype.
- **Turtle rendering.** `format=turtle`, or an `Accept` header that prefers Turtle,
  returns a `spk:DiagnosticsReport`. The prefix is `spk:` instead of `spx:`, because
  `urn:x-sparkles:` already has that prefix for the vector terms ([F04](F04-vector-search.md)).
  Findings use the SHACL result properties, and
  `sh:sourceConstraintComponent` names the check. There is no `sh:conforms`.
  `sparkles infer --check --format turtle` prints the same.
- **Per-dataset auto setting.** `PUT /$/reason/{ds}/auto` stores
  `{enabled, debounceSeconds?, maxDelaySeconds?}` in `reasoning.json`, and `DELETE`
  removes it. It takes precedence over `--auto-reason` and also works without it, so
  `serve` always runs the loop unless the server is read-only. Re-runs and
  `sparkles infer` keep the setting.
- **Superseding automatic runs.** A run holds the writer lock for its whole duration,
  so a newer commit cannot arrive while it runs. A write that waits for the lock
  supersedes it instead. The store counts the write transactions waiting for the lock,
  and the task cancels the run when one waits. Only runs started after the debounce
  yield, and a run forced by the maximum delay always finishes. Reasoning tasks also
  accept `DELETE /$/tasks/{id}`.

**Phase 2 decisions.** §6 counted commits to the inferred graph as input changes. The
reasoner never reads that graph, and every run rewrites it, so changes to it leave the
inferences fresh. The status does not expose an `inputCommit` field. The UI gained a
button that turns the dataset's own automatic re-runs on or off.

**Not built.** Phase 3, incremental DRed-style materialization and diagnostics over
chosen named graphs, is not built. `dt-not-type` is still never checked.
