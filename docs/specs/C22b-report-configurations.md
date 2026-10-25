# C22b: Report configurations for distinct audiences

> **Status:** specified.
>
> **Phases:** None shipped. Phase 1 adds named configurations and an explicit default
> with migration of the existing single configuration. Phase 2 connects automatic
> selection surfaces, durable refresh and configuration-aware report review.
>
> **User docs:** none for multiple configurations, because that extension is not built.
>
> The [Outcome](#outcome) records implementation when it ships.

This extends [C22](C22-graph-rag-community-summaries.md) for roadmap item 100. It reuses
the disclosure footprint and guard of C22, settings of [C19](C19-layered-settings.md),
access control of [C12](C12-graph-access-control.md) and
[C12b](C12b-triple-access-control.md), and the maintenance contracts of
[F22](F22-analysis-lifecycle.md) and [C26](C26-durable-jobs-and-delivery.md).

## 1. Goals and boundaries

Readers with different views need reports built from the facts each audience may use.
A report built from all facts cannot be made safe by removing citations afterward.
Different projections and languages also need distinct report identities and review
histories. A cybersecurity team might summarize internal inventories while a supplier
receives reports built only from the shared advisory and product graphs.

This design adds independently configured report builds within one dataset. Each has
an immutable identity, its own principal, input projection, reports graph, language,
state and review history. Dataset and provider spending caps still apply across them.

The extension does not let a reader pick another principal, weaken a disclosure guard,
or declassify a report. It does not create a probabilistic interpretation of a generated
summary. It does not require one model request per user when several users share an
authorized configuration.

## 2. Configuration identity and selection

Settings format 2 accepts a map of named configurations and one explicit default.
Each entry uses C22's existing configuration fields. Names are unique within the
dataset. A server-assigned configuration UUID is stable across renaming, while every
material edit creates an immutable revision. The settings API returns both identities.

```json
{
  "format": 2,
  "defaultConfiguration": "shared",
  "budget": {"tokensPerDay": 500000},
  "configurations": {
    "shared": {
      "principal": "shared-reports",
      "projection": {"graphs": ["urn:advisories:accepted"]},
      "reportsGraph": "urn:reports:shared",
      "language": "en",
      "budget": {"tokensPerBuild": 100000, "tokensPerDay": 200000}
    },
    "internal": {
      "principal": "internal-reports",
      "projection": {"graphs": ["urn:advisories:accepted", "urn:inventory:accepted"]},
      "reportsGraph": "urn:reports:internal",
      "language": "en",
      "budget": {"tokensPerBuild": 150000, "tokensPerDay": 300000}
    }
  }
}
```

The default names an existing enabled configuration. Omitting `config` selects that
default, not the first configuration the caller can access. If the default is
unavailable to the caller, report retrieval returns the same unavailable response as
an absent configuration, and Ask follows C22's ordinary query fallback. It never
silently switches to another audience's reports.

An explicit `config` selects one authorized named configuration. Names are not bearer
credentials. The settings API is administrator-only. Reader-facing pickers list only
configurations whose current reports the reader may see, without counts or labels for
hidden configurations. An explicit invisible name does not produce a distinguishable
existence error.

Two configurations must have distinct reports graphs and managed identity prefixes.
They may read overlapping facts. A direct write or reconfiguration that aliases their
managed output scopes is refused with `report-graph-conflict`.

## 3. Disclosure and caches

A build captures configuration UUID and revision, dataset and branch UUID, source
commit, principal access fingerprint, projection digest, model configuration and
language. It computes communities and report inputs from that principal's visible
snapshot. Global hierarchy metadata is not computed first and filtered afterward.

C22's footprint includes every fact sent to the model and the transitive footprints
of child reports. It applies separately to each configuration and revision. The guard
is checked when listing communities, reading reports, searching report text or vectors,
answering Ask, recalling memory, exporting reports and opening a citation. Shared graph
grants alone do not make a disclosure footprint valid.

Cache keys include configuration identity and revision, branch and commit, access
fingerprint, model and language. A stored conversation or continuation cannot cross
configuration boundaries. Cross-configuration reuse is initially disabled, even for
matching report text. A later optimization needs an explicit proof that both visible
inputs and disclosure requirements are identical.

After access revocation or a changed guard, existing reports become unavailable to
affected readers immediately. Rebuilding may later produce reports over a reduced
view. Returning a stale broad-view report while that rebuild runs is forbidden. Hidden
configuration names, member counts and cache hits must not be observable through public
status or error details.

## 4. Runs, review and budgets

Each configuration has an independent run frontier, hierarchy and immutable report
revisions. A configuration edit invalidates pending estimates and confirmations. Every
provider attempt rechecks the current definition, principal, provider send level and
remaining budget, as C22 already requires for one configuration.

A source commit can schedule several refreshes. F22 tracks the read dependencies and
freshness of each independently. C26 supplies durable jobs when automatic refresh is
enabled. A failure in one configuration cannot advance another one's frontier.

Review targets a report revision within a configuration UUID. A report copied into
another configuration has new provenance and starts unreviewed. Renaming a configuration
does not invalidate review, while a changed report or supporting input must follow
F22's explicit carry-forward policy. Deleting a configuration does not delete historical
source evidence or authorize publication of its previous reports.

Budget admission reserves against the build, configuration and dataset caps in one
atomic dataset-ledger operation. Provider-wide admission still uses the provider's
existing accounting protocol. No request is sent until both reservations are held.
A refusal releases the other reservation through an idempotent accounting transition,
and an uncertain reservation remains charged until reconciled. No cross-dataset
transaction is implied. Configuration caps subdivide the dataset allowance; adding
or renaming configurations cannot multiply or reset it.
Retries and outstanding reservations remain charged under their original configuration
UUID. An unresolved charge blocks new work under C22's existing conservative policy.

The ledger remains one dataset-wide accounting authority, with configuration dimensions
on entries. Per-configuration summaries are derived views, not independent ledgers
whose races could overspend the shared cap. A fair scheduler limits active requests per
configuration and per dataset without reserving unbounded work in memory.

## 5. Routes, migration and compatibility

C22 routes accept `?config=NAME`, including status, build, refresh, confirmation,
communities and review. An estimate or task records the UUID and revision so a rename
cannot redirect confirmation. CLI calls accept `--config`. Ask, `community_reports`,
recall and the memory brief accept an optional configuration selector. Each response
records the actual selected configuration when it is visible to the caller.

Existing single-object settings are read as one configuration named `default`, with
the same principal, output graph, report identities and budget. An explicit migration
assigns its stable configuration UUID and writes settings format 2 atomically. No
existing report is rewritten or reviewed during migration, and migration cannot trigger
new model calls. Unnamed routes retain their behavior through that explicit default.

The current format registry records GraphRAG state version 2 and ledger version 1.
This extension registers state version 3 and ledger version 2, with configuration
identity in every new record. Migration stages and verifies the new files before
publishing the settings and state pointer under the dataset's management lock. A
versioned migration marker makes crash recovery complete the same conversion or retain
the old consistent set. Readers refuse unknown versions without truncation. The
dataset minimum-reader gate is advanced before the new durable capability is exposed,
using the existing compatibility mechanism rather than assuming old settings readers
will reject unfamiliar fields.

Ledger migration preserves all settled and unresolved charges. Renaming files or
starting a second configuration cannot create a new daily allowance. An operator
downgrade requires an explicit export to the older single-configuration format and
refuses any unresolved reservation. No automatic downgrade discards configurations.

Phase 1 retains C22's current main-branch build scope. It does not imply that GraphRAG
is available on arbitrary historical branches. Backup captures settings, state,
configuration identities, guards and the ledger consistently. Restore and clone leave
automatic work paused, revalidate principals and references, and preserve outstanding
budget liabilities before any approved resumption.

## 6. Acceptance and delivery

| Example | Required result |
|---|---|
| A1. Two audiences read different source graphs. | Their communities and reports are built independently. The restricted reader learns nothing from the other configuration. |
| A2. The configured default is invisible. | An omitted selector follows the documented unavailable or query-fallback behavior. It does not choose another configuration. |
| A3. A user guesses a hidden configuration name. | The response does not reveal its existence, principal, counts or language. |
| A4. Two builds race for the dataset's remaining budget. | Atomic reservations prevent the sum from exceeding the dataset or provider cap. |
| A5. An administrator renames a configuration with an outstanding request. | Its confirmation, task, review and accounting still address the same UUID and revision. |
| A6. Access is revoked after generation. | Search, cached answers, report text and citations all stop exposing the old report. |
| A7. A shared child report cites restricted evidence. | The transitive footprint prevents disclosure through a higher-level report. |
| A8. Migration crashes between state and settings writes. | Recovery exposes one consistent version and preserves all charges. Unsupported readers refuse the capability. |
| A9. A configuration is added or restored under another name. | Shared spending is not reset and deliveries remain paused until authorized resumption. |
| A10. Reports are byte-identical across configurations. | Their identities, access checks and review histories remain separate. |

Tests cover migration fixtures, every crash boundary, concurrent reservations, access
revocation and inference-dependent guards. Noninterference tests compare public
responses with and without an invisible configuration, including counts, errors and
cache behavior. Budget tests use fake providers and exact ledgers. No paid evaluation
is required to verify these invariants.

Phase 1 ships identity, settings migration, separate guarded states, budget dimensions
and explicit HTTP and CLI selection. Phase 2 adds all retrieval surfaces, UI selection
and durable refresh through F22 and C26. A phase that leaves a surface on a single
configuration must refuse its explicit unsupported selector rather than ignore it.

The first-readable selection proposed in C22's open questions is rejected because
settings order and permission changes could silently change an answer's evidence.
Independent daily ledgers are rejected because they do not enforce one dataset cap.
Filtering broad reports after generation is rejected because it cannot remove their
implicit disclosure.

## Sources

The sources are the current Sparkles settings, GraphRAG implementation, format registry
and C22's published design sources. This spec adopts no external code, ontology or
runtime dependency.

## Outcome

Multiple report configurations remain planned under roadmap item 100. The currently
implemented single configuration retains its existing behavior. None of the migration,
selection or accounting changes described here has shipped.
