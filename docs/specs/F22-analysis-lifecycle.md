# F22: Maintained analyses and evidence dependencies

> **Status:** specified.
>
> **Phases:** None shipped. Phase 1 defines analysis runs, results and dependency
> tracking with bounded recomputation. Phase 2 adds automatic invalidation, replacement
> and durable delivery. Phase 3 uses incremental maintenance where equivalence is proven.
>
> **User docs:** none, because this capability is not built.
>
> The [Outcome](#outcome) records implementation when it ships.

This design builds on [C25](C25-evidence-and-assessments.md),
[C26](C26-durable-jobs-and-delivery.md), [C27](C27-valid-time-profiles.md),
[F06](F06-snapshots-and-point-in-time.md), [F13](F13-incremental-views-and-provenance.md),
[F18](F18-explanations-and-entailment-diffs.md), [C08](C08-inference-freshness.md),
[C12b](C12b-triple-access-control.md) and [C22](C22-graph-rag-community-summaries.md).
It uses RDF 1.2 and PROV-O for records. It introduces no domain ontology or prediction
algorithm.

## 1. Purpose and scope

An application can already query facts, run a graph algorithm and store its answer.
Keeping that answer useful needs more than a citation. A corrected source, an updated
inventory, a changed query, a new contradiction or an expired validity interval may
require another run. The result needs to say what it used, whether it remains current
and why it changed.

This capability maintains analysis results as versioned evidence-backed assessments.
A cybersecurity assessment that a release contains an affected component is one
example. The same lifecycle applies to a supplier concentration calculation, an entity
match proposal, a data-quality finding or a generated report.

The goals are reproducible inputs, honest freshness, bounded invalidation, safe
publication, reviewable replacements and durable notifications. A first implementation
recomputes affected analyses rather than requiring a general incremental engine.

This design does not add a probabilistic reasoner, a forecast model, a universal risk
score or an automatic truth arbiter. It does not require application algorithms to run
inside the database process. It does not replace C08's maintenance of rule closures or
F18's proof search. A hypothesis is not a logical consequence merely because its run
has complete provenance.

## 2. Definitions and records

The namespace is `an: <urn:x-sparkles:analysis:>`. The managed RDF profile is version 1.
Records reuse the evidence namespace `ev: <urn:x-sparkles:evidence:>` from C25.

| Record | Meaning |
|---|---|
| `an:Definition` | A stable analysis identity with immutable revisions. A revision names its executor kind, parameters, input selection, output schema, principal and policy. |
| `an:Run` | An execution attempt against pinned inputs. It records the definition revision, job identity, input manifest, clock and algorithm versions. |
| `an:InputManifest` | The exact dataset, branch, commit and reference identities read, the selection footprint, access fingerprint and the external artifact digests. |
| `an:Assessment` | An immutable subclass of `ev:Assessment` produced by a run. It contains a qualified conclusion, evidence links, unresolved conditions and limitations. |
| `an:SupportSet` | One declared alternative collection of supporting evidence references. All members of one set are required. Several sets express alternatives. |
| `an:ResultKey` | A definition-scoped identity for a finding across runs. Its canonical key contains explicit domain identity and scope, not a generated summary. |
| `an:Invalidation` | An event saying an input or policy change requires a freshness decision. It does not declare the conclusion false. |
| `an:Publication` | An atomic installation of an assessment revision as the current result for a result key, with the checked input frontier. |

An `an:Assessment` uses `ev:target` to name its stable result key and
`ev:conclusion` to name its qualified conclusion or domain result resource. Its
`prov:wasGeneratedBy` identifies the run. Each reference to evidence is an `ev:EvidenceRef` with dataset UUID, branch UUID,
commit, graph and claim or reifier identity. Process-local vocabulary IDs and exported
blank-node labels are not persistent references. C25 defines a resolvable immutable
reference and its handling of blank nodes. A reference that cannot be resolved does
not count as confirmed support.

An external artifact includes its content digest, permitted location, producer and
format version. The input manifest records a missing artifact as unavailable rather
than substituting current content from the same URL. A reproduced result may have a
different serialization digest while agreeing under its declared comparison policy.
The policy must identify which comparison it uses.

The definition distinguishes `query`, `rules`, `graphAlgorithm` and `external` executor
kinds. The first three name registered operations and their versions. An external
worker uses C26's registered worker protocol. A definition cannot name an arbitrary
shell command or acquire network permission by adding a URL to its input.

## 3. Independent state dimensions

Execution, freshness, review and epistemic status are separate.

| Dimension | Values and meaning |
|---|---|
| Execution | `queued`, `running`, `succeeded`, `failed` or `cancelled`. These describe a run. |
| Freshness | `current`, `stale`, `unknown` or `superseded`. These describe whether an assessment remains usable under its definition. |
| Review | C25's review decisions apply to an exact assessment revision. Acceptance is a policy decision, not proof of truth. |
| Conclusion kind | `observation`, `logicalConsequence`, `assessment`, `hypothesis` or `scenario`. The executor must declare the kind it can produce. |

A successfully executed run may be stale before it publishes. A reviewed assessment
may become stale after a correction. A failed replacement leaves the previous result
inspectable and stale. A scenario is never promoted to an observation by changing its
review status.

An assessment has a stable result key and immutable revision identity. A publication
points to the current revision. Disappearance from a run is not automatically a
negative conclusion. Only a complete authoritative run over the declared result scope
may publish a resolution for an absent prior key. A partial run retains unresolved
prior results and reports its incomplete coverage.

New revisions do not inherit review by default. A policy may carry a decision forward
only when the conclusion, scope and complete evidence-and-method digest are identical.
The carry-forward is itself a recorded decision that cites the previous one.

## 4. Definitions and inputs

An example definition identifies a registered version matcher. The component and
advisory vocabularies in its query are application choices.

```json
{
  "profileVersion": 1,
  "executor": {"kind": "external", "type": "release-applicability", "version": "1"},
  "principal": "inventory-analysis",
  "inputs": {
    "graphs": ["urn:inventory:accepted", "urn:advisories:accepted"],
    "includeSchema": true,
    "includeInferences": false
  },
  "resultScope": "watched-releases",
  "conclusionKind": "assessment",
  "refresh": {"mode": "automatic", "debounceSeconds": 30},
  "publication": {"reviewRequired": true},
  "limits": {"memoryBytes": 268435456, "deadlineSeconds": 120, "maxResults": 10000}
}
```

An input manifest captures the dataset and branch UUID, exact input commit, immutable
definition revision, schema and rule digests, the effective settings and model or
algorithm version. It records parameters, random seeds where applicable, a fixed
evaluation clock and C27 temporal selectors. A model request records its prompt and
provider configuration identities subject to the caller's access and retention policy.
Reproducibility does not promise deterministic output from a remote model.

Phase 1 accepts one input dataset branch per definition, initially `main`. External
source revisions must already be imported or named by immutable artifact references.
Several datasets can be represented as independent pinned inputs in a later phase,
with an explicit vector of commits. No API calls that vector an atomic global snapshot.

Result graphs and the definition's control graphs are excluded from its inputs by
default. A dependency on another definition is explicit and uses that definition's
published result revision. The definition graph is acyclic in Phase 1. Cycles are
refused with `analysis-cycle`, not approximated by uncontrolled trigger recursion.

## 5. Read dependencies and support

Supporting citations and read dependencies answer different questions. Citations
explain an assessment. Read dependencies identify changes that might alter it.

A result such as "no affected dependency was found" reads an inventory scope and an
advisory scope even if it cites no matching advisory. A new advisory may invalidate
it without deleting a cited fact. Counts, rankings, top-k results, negation and
community membership also depend on absent or unselected rows.

Every executor therefore declares a conservative read footprint over selected graphs,
predicates, resources and schema. A query executor may obtain one from planning. An
external worker supplies a declared upper bound at definition time and may narrow it
only through a registered, tested dependency proof. A citation list alone is never
accepted as the footprint. Unknown or data-dependent reads use the whole selected
visible input scope.

An ordinary input change intersecting the footprint makes the assessment stale. A
change proved disjoint may advance the checked frontier without recomputation. A gap
in retained changes makes freshness unknown and schedules full recomputation. A
checkpoint never advances across an unexamined gap.

A support set is an explanation supplied by the executor, not a proof unless a
registered logical executor verifies it. Each alternative support is evaluated under
the requesting principal's view. Truncated alternatives are marked incomplete. An
unavailable set does not become visible by replacing its restricted members with a
count or a statement that hidden evidence exists.

A changed supporting source schedules re-evaluation. Independent surviving support can
keep a conclusion justified, but the system must still consider changed contradictions,
scope and policy before marking the new assessment current. It does not retract all
dependent statements as false merely because one support was withdrawn.

## 6. Scheduling and atomic publication

C26 owns the durable queue, leases, fences, retries and outbox. F22 owns the assessment
semantics. Each definition has at most one active publication generation. Several input
changes may coalesce into one pending run, but the pending watermark is never lost.
Every run uses its own immutable input manifest.

A worker computes outside the write transaction. It returns a bounded result artifact,
supports and coverage metadata. Publication uses C26's typed publish operation on the
same dataset branch. That transaction verifies the lease fence, definition revision,
current principal and input frontier, validates the output, installs result pointers
and enqueues delivery records atomically. No transaction remains open during a model
request, an artifact download or external computation.

Queue and lease updates themselves advance commits. Publication does not require the
head to equal the input commit. It verifies that every intervening change relevant to
the declared footprint has been examined and that none invalidates the candidate.
Excluded control graphs may be ignored only because they were never input. The
verification and pointer change share the writer's transaction boundary. If this check
cannot finish within its budget or history is missing, publication returns
`analysis-input-unverified` and leaves the candidate unpublished. A new run may then
read a current pin.

A source change racing publication either precedes that atomic check and prevents a
current publication, or follows it and is recorded as a new invalidation. A durable
consumer frontier and reconciliation scan ensure the second case is eventually found.
On restart, current results whose inputs have not yet been reconciled are unknown,
unless the available checked frontier proves their validity. A status response returns
the checked input commit and current head so callers can see any lag.

The initial implementation does not expose an assessment as current merely because
its queued refresh has not started. While consumers lag, reads must either prove the
intervening footprint disjoint within a bounded check, or downgrade freshness to
unknown. A durable subscriber cannot offer synchronous freshness by assumption.

C27 expiry and scheduled observation windows create durable invalidation timers.
Timers refer to a definition revision and interval identity, so an old timer cannot
invalidate a replacement revision accidentally. A changed clock policy or uncertain
expiry yields an explicit re-evaluation rather than deleting historical evidence.

## 7. Authorization and review

Definition management requires dataset administration and write access to its managed
output scope. Execution uses a configured principal with only the required query,
artifact and output privileges. Worker credentials authorize a particular job and
registered operation, not arbitrary access to the dataset.

Input access is checked at execution and again before publication and delivery. A
changed access fingerprint prevents reuse of results from the old view. Restricted
data must not leak through conclusions, absence reports, aggregate counts, caches,
projection membership or failure details. Status APIs omit records the caller cannot
read rather than disclosing the existence of a hidden run.

Until F18's provenance-aware visibility is implemented, an analysis publishes only
into a compatible trust boundary whose readers may see its required inputs, or remains
private to the defining principal. Creating an output graph grant is not sufficient to
declassify its dependencies. Cross-boundary publication requires a separate explicit
declassification policy and audit and is outside Phase 1.

C25 owns the accepted-claim projection. F22 owns separate managed assessment and result
graphs. A reviewed assessment may propose a C25 claim or trigger an explicitly approved
admission policy, but it cannot silently insert its conclusion into the accepted graph.
Direct SPARQL or Graph Store mutations of managed lifecycle pointers are refused.

## 8. Routes and surfaces

The following routes are proposed additions, not existing endpoints.

| Route | Contract |
|---|---|
| `PUT /$/analyses/{ds}/{name}` | Validate and save an immutable definition revision under `If-Match`. Return its identity and dependencies. |
| `POST /$/analyses/{ds}/{name}/run` | Pin inputs, create a C26 job and return `202` with run and job identities. A repeated idempotency key with another payload is refused. |
| `GET /$/analyses/{ds}/{name}` | Return the visible definition and execution, coverage and freshness summaries. |
| `GET /$/analyses/{ds}/{name}/runs/{run}` | Return the visible manifest, limits, state and result references. |
| `GET /$/analyses/{ds}/{name}/results` | Page through result keys at an exact retained commit, with assessment and freshness states. |
| `POST /$/analyses/{ds}/{name}/reconcile` | Queue a bounded reconciliation or full recomputation with a returned task identity. |

Cancellation and worker publication use C26. Review uses C25. An initial CLI exposes
`sparkles analysis define|run|status|results|reconcile`. Rust exposes typed definitions,
run manifests and result references. HTTP clients and MCP follow the same contracts
in the second phase. The UI shows evidence, contradictions, current input commit,
checked frontier, unresolved conditions and review actions beside a focused graph.

Application ranking and notification thresholds are versioned policies. A delivery is
identified by definition, result key, material revision transition and channel policy.
Identical recomputation does not create a new notification. Corrections and resolutions
are explicit transitions, with cooldown and repeat policies managed by the application
over C26's durable deliveries. The engine does not invent an urgency score.

## 9. Persistence, recovery and resource bounds

Managed records and publication pointers use ordinary dataset commits in a reserved
namespace. Their typed profile version is checked before operations. Any auxiliary
reverse dependency index is derived, versioned and rebuilt from records after corruption
or an unsupported cache version. It never becomes the only record of support.

Definitions, manifests, result keys, reference counts, dependency depth and retained
history each have quotas. A run pins its input only for a bounded retention period.
When that expires, the run fails as unreproducible or is explicitly restarted against
new inputs. It does not quietly reread the head under the old run identity.

Backups must preserve managed data and the definitions needed to interpret it. A
restore leaves recurring jobs and external deliveries paused under C26, revalidates
principal mappings and resolves source references before any automatic recomputation.
Clones have new dataset and branch identities. Old input references stay historical
references until an explicit remapping operation proves the cloned support exists.

A format or feature version used for durable control must follow C26's reader-admission
rules. Merely adding a profile triple is not a mechanism for making old binaries refuse
unsafe operations. No unversioned auxiliary persistence is allowed.

## 10. Acceptance examples and verification

| Example | Required result |
|---|---|
| A1. An inventory and an advisory yield a finding. | The assessment names exact input commits, application matcher version and resolvable evidence references. It is not an asserted fact before admission. |
| A2. A new advisory affects a prior negative result. | The scope footprint invalidates that result even though no previously cited statement changed. |
| A3. One of two independent supports is withdrawn. | Re-evaluation preserves the remaining support and records the changed assessment. It does not declare the conclusion false solely because of the withdrawal. |
| A4. A source changes during external computation. | Publication either refuses the old candidate or publishes it as historical and stale. No current pointer is installed from unverified inputs. |
| A5. Lease commits advance the head without input changes. | The guarded publication may succeed after proving its input footprint unchanged. |
| A6. A change-feed gap or dropped pin prevents verification. | Freshness becomes unknown and full recomputation is queued. The old result remains inspectable with its limitation. |
| A7. A limited run returns only some results. | Missing prior keys are unresolved, not automatically resolved. |
| A8. A reader loses access to one supporting graph. | Neither results nor delivery payloads disclose restricted evidence or derived counts. Existing caches cannot preserve the old visibility. |
| A9. The worker crashes after publication but before acknowledgement. | C26 deduplication returns the same committed publication and delivery identities. |
| A10. A validity boundary passes during downtime. | Recovery queues its durable invalidation before treating the assessment as current. |
| A11. A model proposes an actor connection. | The result remains a hypothesis with its method and sources. Review alone cannot turn it into an observation. |
| A12. A report cites a generated report. | Its lineage reaches original evidence. It does not count the generated report as independent corroboration. |

Tests use a deterministic clock, fault injection at publication and queue boundaries,
and separate principals with hidden supports. Differential tests compare incremental
refresh with full recomputation after insertions, deletions, source corrections,
contradictions and schema changes. Randomized tests include negation, aggregate inputs,
alternative supports and incomplete footprints.

Measurements report change-to-invalidation lag, recomputation work, recovery time,
publication contention, reverse-index memory and the ordinary commit-path cost when no
definition exists. Phase 1 needs no unmeasured speed claim. An optimization is accepted
only with unchanged results and the repository's performance measurement rules.

## 11. Phases and dependencies

Phase 1 needs C25's reference and assessment records and C26's atomic publication
contract. It ships explicit runs, conservative footprint checks, read-only manifests
and manual reconciliation with all failure states above. It may use full recomputation.

Phase 2 adds durable change consumers, C27 timers, automatic refresh, result transition
delivery and the CLI, UI and MCP workflow. Recovery and backup tests are release gates.

Phase 3 uses F13 graph views and per-fact provenance for eligible computations. A
definition states whether it is incremental or recomputed and why. Unsupported
negation, external algorithms or model calls retain conservative recomputation.
F18 handles logical justifications; external workers continue to supply qualified
method and evidence records. No phase silently weakens visibility or freshness to
increase throughput.

## 12. Alternatives and sources

A citation-only invalidation index was rejected because it misses insertions, negative
results and aggregates. Unrestricted arbitrary worker code was rejected because it
cannot establish read footprints or privileges. Treating every withdrawal as false was
rejected because alternative support can survive. Requiring F13 Phase 3 before any
analysis was rejected because bounded recomputation already provides the required
semantics.

The primary sources are [PROV-O](https://www.w3.org/TR/prov-o/),
[RDF 1.2 Concepts](https://www.w3.org/TR/rdf12-concepts/) and the Sparkles designs named
above. Provenance annotations describe origin, not a statistical confidence calculus.
No new runtime dependency is selected by this specification.

## Outcome

This capability is specified and unimplemented. Existing query, reasoner, algorithm and
GraphRAG operations retain their current behavior. The analysis records, guarded
publication lifecycle and automatic dependency maintenance described here have not
shipped.
