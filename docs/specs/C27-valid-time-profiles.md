# C27: Valid-time profiles and temporal analysis

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 defines the RDF profile, shapes and stored-query
> templates. Phase 2 adds bounded temporal projections and a separate event-time
> control in the UI. Phase 3 integrates scheduled expiry with durable jobs and analysis
> invalidation. Temporal indexes require a measured workload and are outside these phases.
>
> **User docs:** none, because nothing is built.
>
> The [Outcome](#outcome) section will record implementation and deviations.

This design builds on [F06](F06-snapshots-and-point-in-time.md),
[U01](U01-time-slider.md), [C25](C25-evidence-and-assessments.md),
[C26](C26-durable-jobs-and-delivery.md) and [F22](F22-analysis-lifecycle.md).
It uses OWL-Time concepts and XML Schema temporal values. It adds a profile over
ordinary RDF rather than another transaction history or a new literal datatype.

## 1. Goals and scope

A retained commit describes what the dataset recorded. It does not say when a claim
applies in the world. A security advisory imported today can describe a release that
was affected last month, and a later correction can change that interval. Replacing
one date with the other loses either historical knowledge or the event's timing.

This feature defines both coordinates without changing F06's commit selector.
Applications can ask which claims applied during an interval and what evidence was
available at a chosen retained commit. Results preserve temporal uncertainty and the
source revision that supplied the dates. A reusable interface gives graph, map and
table views the same selection semantics.

The profile covers validity intervals, event occurrence windows and assessment expiry.
It does not make a claim true because its interval matches. Review state, asserted
truth, applicability and temporal matching remain separate decisions.

The first phases do not add temporal SQL syntax, change SPARQL comparison rules,
infer a person's travel, or treat a shortest path as a sequence of feasible timed
movements. Time-respecting path algorithms and specialized indexes need separate
semantics and measurements. All examples below use invented advisory identifiers.

## 2. Vocabulary and coordinates

The profile version is the IRI `urn:x-sparkles:valid-time:profile:1`. The prefix `vt:`
is `urn:x-sparkles:valid-time:`. The evidence prefix `ev:` is
`urn:x-sparkles:evidence:`. Existing imported date properties remain available as
source data, but temporal selectors read the declared normalized profile.

| Term | Meaning |
|---|---|
| `vt:validDuring` | Connects a claim occurrence to the interval over which its proposition is asserted to apply. It never asserts the proposition itself. |
| `vt:occurredDuring` | Connects a described event to a window in which its occurrence is reported to lie. This does not assert that the event lasted for the whole window. |
| `vt:occurredAt` | Connects an event to an exact known boundary. Its earliest and latest values must be equal. |
| `vt:Interval` | A half-open interval whose beginning is included and end is excluded. |
| `vt:Boundary` | A known, unknown or explicitly unbounded endpoint. |
| `vt:start` and `vt:end` | The interval's one beginning and one end boundary. |
| `vt:kind` | One of `vt:Known`, `vt:Unknown` or `vt:Unbounded`. |
| `vt:earliest` and `vt:latest` | Inclusive bounds on a known endpoint's possible instant. Equal values represent an exact endpoint. |
| `vt:precision` | The source's precision, such as `vt:Instant`, `vt:Day` or `vt:Month`. It does not substitute for normalized endpoint bounds. |
| `vt:sourceValue` | The original date expression or structured value retained for inspection. |
| `vt:normalization` | The run that interpreted the source time, with its rules and any calendar or timezone data version. |
| `vt:expiresAt` | A review or freshness deadline. Reaching it invalidates an assessment's freshness, not the underlying claim's historical validity. |

Source publication uses C25's `ev:publishedAt`. The first time a revision became locally
available uses `ev:firstAvailableAt`, established by the service and its ingestion
receipt. Clients cannot backdate it. An external report of earlier availability is a
separate evidenced claim and is not the local availability timestamp. Commit identity
remains the authoritative coordinate for selecting what was recorded.

Publication can precede local availability. Event time can precede or follow both.
These relationships are not errors by themselves. Availability imported by a trusted
restore must retain its original dataset and commit identities or state that it cannot
reproduce the original knowledge history.

## 3. Normalization and uncertainty

### 3.1 Endpoint representation

Known normalized endpoints are valid `xsd:dateTime` values with an explicit offset.
The normalization writes UTC values and retains the source lexical expression. A
comparison uses value equality rather than string ordering. It must not discard source
precision or silently round a supported fractional second into an exact lower precision.
An implementation that cannot represent the requested precision returns an explicit
unsupported-precision result or an outward-bounded uncertainty interval.

For a known endpoint, `earliest` must not exceed `latest`. Each interval has exactly
one start and one end boundary. Unknown and unbounded boundaries carry neither finite
bound. The distinction is intentional: an unknown end does not assert that validity
continues forever. Unbounded start means negative infinity and unbounded end means
positive infinity within this profile, not a representable XML Schema timestamp.

For finite known endpoints the latest start must precede the earliest end. This keeps
every represented interval nonempty and avoids inventing a dependency between uncertain
boundaries. Overlapping endpoint envelopes remain unnormalized with the error
`temporal-indeterminate-order`. Their source values remain evidence and their temporal
verdict is unknown until a more expressive profile or better evidence resolves them.
An exact instant uses an event description rather than a zero-length validity interval.

### 3.2 Dates, local time and precision

A source value with no timezone cannot silently become midnight UTC. A calendar date
retains its calendar precision. If an explicitly declared geographic or source-timezone
policy can normalize it, the normalization records that policy and produces a bounded
window. A date known only as an occurrence day becomes `occurredDuring`, not a claim
that the event persisted all day.

Phase 1 accepts the Gregorian calendar and explicit UTC offsets. A source importer may
resolve named timezones, but must pin the timezone database version and record its
choice for an ambiguous local time. A nonexistent local time is quarantined. It must
not assume that every local day lasts 24 hours. Other calendars and unsupported leap
second expressions retain their source value and an unknown normalized verdict.

A month or year is an explicitly bounded calendar window only when its calendar and
timezone policy are known. Unknown precision does not justify exact coordinates. The
UI labels a coarse window with its source precision rather than formatting its beginning
as the event's precise occurrence time.

### 3.3 Matching

For a known interval let `s0` and `s1` be its earliest and latest start, and `e0` and
`e1` its earliest and latest end. Explicitly unbounded endpoints use their corresponding
infinity. For a selected instant `t`, possible membership is `s0 <= t < e1`, and
definite membership is `s1 <= t < e0`. If membership is possible but not definite, the
verdict is `possible`. Otherwise it is `definite` or `outside`.

For an exact nonempty query interval `[q0,q1)`, possible overlap is
`s0 < q1 && q0 < e1`. Definite overlap is `s1 < q1 && q0 < e0`. Touching endpoints
alone do not overlap. A fully known interval is wholly within the query interval only
when `q0 <= s0 && e1 <= q1`. These relations apply to the reported window of an event,
not to a proof that the event was active throughout it.

An unknown boundary yields `unknown` unless the known opposite boundary already proves
`outside`. A missing interval also yields `unknown`, not unbounded. The first phases
permit only exact query instants or exact query intervals. Comparing two uncertain
intervals needs a later operator with separately specified semantics.

`outsideOnly` selects rows proved outside a chosen instant or interval. It does
not select possible or unknown rows as if those were false. A caller that wants to
retain unknowns explicitly asks for them. No selector's absence is evidence of safety.

## 4. Queries, snapshots and publication

### 4.1 Dual selection

Every temporal projection records a dataset UUID, branch UUID and exact commit, the
profile version, selected claim graphs, its valid-time condition and the normalized
query parameters. A request for `head` resolves once before any source reads. All
lookups and result rows use that snapshot. F22 records these coordinates with an analysis.

The recorded-state selector retains F06's `at` meaning. `validAt` and `validBetween`
are application parameters and never reinterpret `at=time:T` as an event-time lookup.
Exactly one of these valid-time parameters can be selected. Interval endpoints and
the choice of `definite`, `possible`, `unknown` or `outside` are explicit.

An as-known analysis reads the retained commit selected by the cutoff. It also checks
that its referenced source revisions and local availability receipts belong to that
snapshot. A later import describing an earlier event cannot enter the earlier result.
A malformed client-supplied `firstAvailableAt` never overrides this check.

If the requested state is not reconstructable, the operation returns F06's `410`.
A change log or a document carrying an earlier publication date is not a substitute
for that state. An approximate historical reconstruction is a different analysis
with an explicit coverage statement, not an exact as-known result.

### 4.2 Stored queries and bounded projection

Phase 1 ships versioned SPARQL templates and shapes as installable resources. An
administrator installs the templates through [C16](C16-stored-queries.md), with typed
parameters bound as RDF terms. Names are examples, not new reserved query names.
`temporal-active-claims` selects one instant, and `temporal-overlapping-claims` selects
one exact query interval. Both return claim IRI, interval IRI, source revision,
temporal verdict and normalization status. A raw graph-only result cannot silently
discard that status when projected into accepted facts.

Phase 2 adds a server helper at `POST /{ds}/temporal/select`. Its request names an
installed candidate SELECT version with a required `?claim` column, its typed parameters,
the profile, the recorded selector and one valid-time selector. The helper accepts
only claim IRIs visible to the caller, streams the candidate rows and performs bounded
temporal metadata lookups at the same snapshot. It has no remote fetch or write action.
Its response streams the original rows with `temporalVerdict` and source coordinates.

The helper is an explicit join with the profile's metadata. It does not rewrite every
arbitrary SPARQL query. A candidate term that is not an IRI or has conflicting interval
definitions produces a row with `unknown` and a visible validation reason. Invisible
metadata produces only `unknown`, without revealing whether hidden metadata exists.
Malformed selectors return `400 temporal-selector-invalid` before the candidate runs.

The helper initially evaluates `validDuring` only. Occurrence templates separately
query `occurredDuring` or `occurredAt` and preserve their different roles. The UI never
uses a validity helper result as an event-duration assertion.

Results include the selected commit, profile digest and candidate-query digest. Row,
byte, scan, memory and timeout controls bound both the candidate and metadata reads.
Cancellation checks run during both stages. There is one admission to the query memory
pool, and an optional metadata cache is keyed by snapshot and caller view.

Publication into a named graph is an F22 operation. This helper never asserts a
proposition from a C25 proposed claim. Accepted projections must additionally enforce
review and acceptance policy, source visibility and the selected temporal verdict.

## 5. Example

An imported advisory revision says a product relationship was valid throughout an
exact period. It is a claim about an interval, independent of the import date.

```turtle
@prefix ev: <urn:x-sparkles:evidence:> .
@prefix vt: <urn:x-sparkles:valid-time:> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix ex: <https://example.org/> .

ex:claim-1 rdf:reifies <<( ex:release-7 ex:affectedBy ex:issue-1 )>> ;
    vt:validDuring ex:period-1 .
ex:period-1 a vt:Interval ; vt:start ex:start-1 ; vt:end ex:end-1 .
ex:start-1 a vt:Boundary ; vt:kind vt:Known ;
    vt:earliest "2026-01-01T00:00:00Z"^^xsd:dateTime ;
    vt:latest "2026-01-01T00:00:00Z"^^xsd:dateTime .
ex:end-1 a vt:Boundary ; vt:kind vt:Known ;
    vt:earliest "2026-02-01T00:00:00Z"^^xsd:dateTime ;
    vt:latest "2026-02-01T00:00:00Z"^^xsd:dateTime .
```

The triple term is not itself an assertion that the release is affected. C25 supplies
the claim's source revision, provenance and acceptance record. The full ingestion
record adds publication and server-controlled local availability. If it arrives in
March, an as-known February snapshot excludes it even though January matches its
reported validity. A later correction creates a new claim occurrence and revision
rather than changing the meaning of the earlier retained snapshot.

## 6. Access, review and lifecycle

Current authorization governs every historical read. A retained snapshot is not an
old permission grant. Apply graph and triple protections before matching, aggregation,
binning or geography lookup. Hidden claims contribute neither rows nor counts.
Restricting publication can change a projection's coverage and must not be described
as a temporal fact about the world.

A proposed or rejected claim can be inspected through an evidence view when permitted.
It never enters an accepted-fact projection solely because its dates match. Review
approval also does not establish truth, precision or independence of evidence. Derived
projections depend on the supported-visibility work of F18 or are computed separately
inside an authorized boundary.

Source correction, identity change, acceptance change and normalization-rule change
schedule F22 invalidation. A new temporal interpretation has a new normalization run
and supersedes the previous projection. It preserves the source expression and prior
assessments. Expiry changes freshness, not a past event or an assertion's validity.

C26 records expiry jobs with dataset and branch identity, the assessment generation
and expected expiry. The publication checks these again. An old job cannot expire a
newer assessment. Recovery reschedules missed deadlines once, and idempotent processing
means repeated delivery does not create repeated state transitions. A job failure
leaves the freshness state unknown or stale rather than silently fresh.

## 7. UI and configuration

The UI labels two independent controls as **Recorded state** and **Valid time**.
Recorded state uses U01's retained commit slider. Valid time selects an instant or
interval and a verdict policy. It does not move the recorded state unless the reader
asks to do so. A link encodes both coordinates and the profile and query versions.

Tables expose unknown and possible rows explicitly. Graph edges and map items use
shape or text as well as colour to show uncertainty. A coarse location or time window
does not become an exact marker or timestamp. Scrubbing has one request in flight and
cancels the prior request. Playback reports recorded time and valid time separately.

Phase 2 configuration is a C19 settings kind named `temporal`, initially disabled.
It declares allowed profile digests, candidate stored-query versions, claim graphs and
maximum selection span. Dataset administration can register configuration but cannot
turn a helper into an unrestricted principal. Definitions follow normal settings
locks and backup policy. Unknown settings versions are refused without rewriting them.
The configuration needs format version 1, registered before implementation. RDF data
and index formats do not change, and an unknown profile version is not interpreted
using the closest known profile.

## 8. Phases and acceptance

| Phase | Deliverable | Acceptance |
|---|---|---|
| 1 | Profile vocabulary, SHACL shapes, import normalization contract and stored-query templates. | A1 to A6 and A9 pass using ordinary RDF and SPARQL. Existing F06 queries are unchanged. |
| 2 | Bounded projection helper, configuration and event-time UI. | A7 to A11 pass with current permissions, cancellation and retained-state controls. |
| 3 | C26 expiry and F22 invalidation integration. | A12 to A14 pass under restart and concurrent correction. |

- **A1.** Exact validity includes its beginning and excludes its end. Adjacent intervals
  do not overlap, and a reversed or zero-length interval fails shape validation.
- **A2.** Bounded uncertain endpoints produce the specified possible and definite rows.
  Missing and explicitly unbounded endpoints differ, and an unknown opposite endpoint
  can never manufacture a definite match.
- **A3.** Offsets denoting the same instant match equally. A date without a normalization
  policy remains unknown. Named-zone import fixtures cover a daylight-saving gap and
  fold with an explicitly pinned rule set.
- **A4.** A March import about January is absent from an as-known February snapshot.
  A client trying to backdate local availability is refused. Publication alone does
  not admit an unavailable revision.
- **A5.** A backdated correction appears only in states containing its ingestion commit.
  Comparing those states preserves both source revisions and earlier assessments.
- **A6.** An unknown interval stays unknown under a negative selector. An unsupported
  calendar or indeterminate endpoint order cannot produce an exact temporal assertion.
- **A7.** A removed retained state yields `410`, without substituting current facts.
  Identical numeric commits on different branch or dataset identities cannot alias.
- **A8.** A row that remains visible after permission revocation reveals no hidden source
  dates through counts, verdict reasons, caches, graph layout or map bins.
- **A9.** A temporal match on an unreviewed proposal does not assert its target triple.
  Independent current acceptance is required for an accepted projection.
- **A10.** Cancelling the candidate scan or metadata phase stops bounded work, releases
  its query memory grant and publishes no graph. Budget exhaustion is explicit.
- **A11.** A reproducible link selects the same available snapshot and valid-time inputs.
  The UI distinguishes occurrence windows from duration and preserves accessible labels.
- **A12.** An expiry queued for assessment generation 1 cannot modify generation 2.
  Duplicate delivery and recovery after the deadline produce one effective transition.
- **A13.** Changing a normalization policy marks dependent assessments stale until a new
  run completes. It does not alter retained source expressions or older run records.
- **A14.** A run resolves its clock once and records it. Replaying its parameters does
  not reevaluate `NOW()` independently in each row or deadline test.

## 9. Verification, tradeoffs and alternatives

Unit tests compare endpoint evaluation with a small independent interval oracle.
Generated bounded intervals cover offsets, equal instants, uncertainty, boundary touching
and every verdict. Table fixtures compare template and helper outputs with that oracle.
The acceptance suite adds source revision, retention, access and restart scenarios.
An implementation must demonstrate a failing control for each fixed boundary or cutoff
regression. No source feed or paid model is needed for these checks.

Measure one selective and one broad temporal filter over retained snapshots with sparse
and dense intervals. Charge metadata and result memory and report total scanned rows.
Ordinary RDF joins may be expensive, but a temporal index is justified only when the
measurements show that cost. Any proposed index must preserve uncertainty, graph scope,
snapshots and exact authorization before it can replace the baseline.

An implicit global valid-time filter was rejected because many triples have no interval
or have different temporal roles. Treating unknown bounds as infinity was rejected
because missing evidence would become an assertion. Reusing commit time as validity was
rejected because it cannot answer historical event questions after delayed publication.
Destructively expiring source facts was rejected because expiry is an assessment policy.

## 10. Sources

- [OWL-Time](https://www.w3.org/TR/owl-time/) provides temporal entities and reference
  systems. This profile's half-open interval and uncertainty rules are additional
  application semantics, not claims about OWL-Time entailment.
- [XML Schema Datatypes](https://www.w3.org/TR/xmlschema11-2/) defines temporal lexical
  and value spaces. The profile requires an explicit offset for normalized endpoints.
- [F06](F06-snapshots-and-point-in-time.md), [CI](CI-commit-identity.md) and
  [U01](U01-time-slider.md) define the recorded-state and retention contracts.
- [C25](C25-evidence-and-assessments.md), [C26](C26-durable-jobs-and-delivery.md) and
  [F22](F22-analysis-lifecycle.md) define evidence, scheduling and assessment lifecycle.

## Outcome

No phase is implemented. The profile, helper, configuration and event-time control are
specified here. Existing commit-time reads and the U01 slider retain their current
semantics. No dependency or on-disk format change has been made.
