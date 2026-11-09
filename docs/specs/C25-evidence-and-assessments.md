# C25: Evidence, claims and assessments

> **Status:** partially implemented
>
> **Phases:** Phase 1 supplies versioned vocabulary, shapes, source and claim fixtures,
> compatibility views and competency queries. Phase 2 adds authenticated typed writes,
> retained-reference resolution, protected metadata, review decisions, accepted projections
> and reversible identity decisions. Phase 3 integrates recall, ingestion, reports and
> dependency maintenance.
>
> **User docs:** [Evidence profile](../ai/README.md#evidence-profile) and
> [resource usage](../../crates/sparkles/resources/evidence/v1/README.md).
>
> The [Outcome](#outcome) section records the delivered scope.

This specification defines roadmap item 81. It builds on the cited writes and source
records of [C17](C17-agent-memory.md) and [C18](C18-natural-language-questions-and-ingest/README.md),
RDF 1.2 triple terms, PROV-O, Web Annotation and SHACL. It reuses ordinary RDF graphs and
the existing transaction, preview, history and access-control machinery.

## 1. Goals and boundaries

A graph can contain a source's statement without accepting that statement as knowledge.
It can retain conflicting statements, several occurrences of the same statement and a
decision made from them. Those distinctions let applications answer what supports a
conclusion, which sources disagree and what needs review after a correction.

The shared profile must support documents, imported observations, notes, hypotheses,
preferences, decisions, tasks and instruction documents without making their meanings
interchangeable. A review records a person's decision under a policy. It is not proof
that the reviewed claim is true. A confidence value describes an evaluation and is not
permission to assert a proposition or execute retrieved content.

This feature supplies a reusable data contract and guarded operations. It does not
define domain ontologies, a universal confidence score, probabilistic reasoning or
automatic identity resolution. Probabilistic reasoning through external engines, as
integration only, is outside the design of this profile, and [ROADMAP](../ROADMAP.md)
item 463 plans it under [F28](F28-ml-and-probabilistic-integration/README.md).
[F22](F22-analysis-lifecycle.md) owns support-set logic,
analysis scheduling and dependent-result freshness. [C27](C27-valid-time-profiles.md)
owns valid-time intervals and temporal selection. [F13](F13-incremental-views-and-provenance.md)
and [F18](F18-explanations-and-entailment-diffs.md) own exact engine derivations and their
visibility. This profile can be used before those later phases ship.

## 2. Vocabulary and identity

The new namespace is `ev: <urn:x-sparkles:evidence:>`. Existing `mem:` and `spk:` terms
keep their current meanings. Profile version 1 is a version of this contract, not a
change to the RDF storage format. Classes specialize PROV-O entities or activities
where those meanings apply. No class membership grants access or execution authority.

| Term | Meaning |
|---|---|
| `ev:Source` | The continuing identity of an external or local source. A mutable URL is a locator, not its revision identity. |
| `ev:SourceRevision` | One immutable observed version of a source, identified by its source key, upstream version when present and content digest. |
| `ev:Rendition` | A byte or text representation of one revision, such as extracted text or a translation. It records its conversion activity and digest. |
| `ev:Passage` | A selected part of a rendition. Its selector and quoted text are checked against that rendition. |
| `ev:Claim` | One occurrence of a proposition attributed to an author or source. Two sources stating the same proposition have different claim identities. |
| `ev:EvidenceRef` | A durable reference to a source passage, structured field or exact stored fact at a recorded view. It does not assert that the referenced statement is true. |
| `ev:EvidenceLink` | A qualified relation from an evidence reference to a claim or assessment, with a relation type, author and activity. |
| `ev:Assessment` | An immutable qualified evaluation with a conclusion, evaluator, method and evidence links. It may evaluate a claim without endorsing its proposition. |
| `ev:ReviewDecision` | An immutable decision about an exact target revision under a named policy. A later decision can supersede it. |
| `ev:IdentityProposal` | A proposed correspondence or separation between identifiers or entities, with evidence and review decisions. |
| `ev:Note`, `ev:Decision`, `ev:Task` | Distinct kinds of authored record. Evidence, rationale and work dependencies have separate relations. |

An imported instruction document retains `mem:Instructions`. Its passages and extracted
claims remain data. A task can describe work, and a decision can recommend an action,
without either being a runnable job. Only the configured operations of
[C26](C26-durable-jobs-and-delivery.md) may turn an authorized request into execution.

Each source revision, rendition, claim, assessment, decision, proposal and evidence link
has an IRI. New records use UUID IRIs. Idempotent imports derive their record identities
from the dataset UUID, importer identity, source revision key and occurrence key, using
the existing deterministic minting mechanism. Equal proposition text alone never merges
claim identities. Two records with equal IDs and unequal immutable content are a conflict.

### 2.1 Claims and qualified relations

A claim has exactly one `rdf:reifies` triple term and one `ev:claimKind`. The kinds are
`ev:Reported`, `ev:Observation`, `ev:Hypothesis` and `ev:Preference`. A claim's
`ev:polarity` is `ev:Positive`, `ev:Negative` or `ev:Uncertain`. Negative polarity means
that a source denies the proposition. It does not add a logical negation rule to RDF.
Uncertain polarity means the source does not commit to either position.

The claim identifies its source revision through `prov:wasDerivedFrom`, its creation
activity through `prov:wasGeneratedBy` and its attributed author through
`prov:wasAttributedTo`. Its scope records the release, configuration or other context
that limits the proposition. Domain vocabularies supply those scope predicates. A
profile cannot erase the scope merely to match two propositions syntactically.

An evidence link has exactly one `ev:target`, one `ev:evidence` reference and one
`ev:relation`. Relations are `ev:Supports`, `ev:Contradicts` and `ev:Mentions`. The
relation is a qualified judgment by its author. A citation that mentions a product is
not automatically supporting evidence of a vulnerability. `prov:wasDerivedFrom`
describes derivation history. It does not imply support, corroboration or endorsement.
`ev:rationaleFor` connects an authored record to a decision, while `ev:dependsOnTask`
connects work items. Neither is an alias for an evidence link.

An assessment has an exact target through `ev:target`, an `ev:conclusion` RDF term or
domain result IRI, `ev:method` and `prov:wasAttributedTo`. Method version, configuration
digest and evaluation time are recorded. [F22](F22-analysis-lifecycle.md) specializes
this class for automated analysis runs and owns their changing current-result pointer.
Editing an assessment creates a successor with `prov:wasRevisionOf` rather than changing
its evidence or conclusion in place.

### 2.2 Sources, independence and confidence

A source revision names its stable source using `prov:specializationOf`. A rendition
uses `prov:wasDerivedFrom` for the revision and records its media type, language and
conversion activity. Translation is a new rendition. It cannot replace the original
passage's language or imply independent corroboration.

`ev:publishedAt` is a source's publication timestamp with provenance. `ev:firstAvailableAt`
is the earliest durable local availability recorded by the server or an audited C26
import. A client cannot backdate that field. An externally alleged earlier availability
uses the separately attributed `ev:reportedAvailableAt`. Local acquisition time does
not claim when the publisher first made the content public.

These timestamp fields use valid `xsd:dateTime` values with an explicit offset and
normalize to UTC while retaining the source lexical value. A date with unknown time
or timezone uses C27's bounded or unknown temporal representation instead of an
invented timestamp. `ev:availabilityReceipt` names the authenticated ingest record and
its dataset UUID, branch UUID and commit. A trusted restore preserves that origin or
reports that it cannot reproduce the original knowledge history.

`ev:upstreamSource` links a revision to a known underlying source. `ev:sourceFamily`
records a declared common origin. A claim that two sources are independent is itself
an assessment with evidence and a method. Absent lineage means unknown independence.
A mirrored advisory, translated article and generated summary do not each count as
independent corroboration. No default query counts source URLs as independent votes.

Confidence belongs to qualified records. `ev:sourceConfidence` records the source's
reported confidence and its scale. `ev:extractionConfidence` records the importer's
evaluation of its extraction, with its method. `ev:analystConfidence` records an
assessment's evaluation. Each uses an `ev:Confidence` node with `ev:value`, `ev:scale`
and an attributed evaluator. A finite decimal on the declared zero-to-one scale is
bounded accordingly. Other scales retain their declared units and ordered categories.
The profile defines no multiplication or conversion between these facets. The current
`spk:confidence` remains the author's uninterpreted zero-to-one value in legacy memory.

## 3. References and source spans

An evidence reference to a stored fact carries `ev:datasetId`, `ev:branchId`,
`ev:commit`, `ev:graphKind` and the expected proposition as `ev:expectedTriple`.
The identifiers are the dataset and branch UUIDs, not their mutable catalog names.
`ev:graphKind` is `ev:DefaultGraph` or `ev:NamedGraph`. A named graph also has exactly
one `ev:graph` IRI. The default graph has no `ev:graph` value. A named graph whose IRI
happens to equal a vocabulary term remains distinguishable from the default graph.

When the fact has a stable reifier IRI, `ev:reifier` names it and the resolver verifies
that it reifies the expected triple in that graph at that commit. Otherwise a ground
triple with only IRI and literal terms can be referenced directly at the exact view.
A caller-supplied blank-node label is not a durable identity. A blank-containing fact
requires a stable reifier present at the referenced commit or a source import's explicit
skolem mapping. Creating an anchor now cannot retroactively add one to an old snapshot.
Unanchored historical blank-containing facts are refused as `unanchored-evidence`.

A missing retained snapshot returns `evidence-unavailable`, not a lookup of the same
triple at the current head. Renamed datasets or branches resolve by UUID. Recreated
names cannot satisfy old references. A resolver checks access before confirming whether
a hidden reference exists and uses the same non-disclosing unavailable response for
an unreadable reference. It never searches another branch for a convenient match.

A passage reference names the exact revision and rendition. Text positions use Unicode
code points in the normalized rendition, with half-open start and end offsets, as the
existing C18 span contract does. It also stores a digest of the selected text and an
optional permitted quote. A structured reference uses an RFC 6901 JSON Pointer over the
retained parsed revision and a digest of its selected value. Normalization rules are
part of the importer version. An offset or pointer that does not match is refused.

Web Annotation's `oa:SpecificResource`, `oa:hasSource`, `oa:hasSelector` and selectors
provide the interoperable passage description. Existing C18 span records map to this
description without rewriting their IDs. A changed rendition creates new references
and explicit successor links. Re-anchoring cannot silently change existing evidence.

## 4. An example

This proposed Turtle 1.2 data records an advisory statement without asserting the
`ex:release ex:affectedBy ex:vulnerability` proposition. The source is fictional.

```turtle
PREFIX ev:   <urn:x-sparkles:evidence:>
PREFIX ex:   <https://example.org/>
PREFIX prov: <http://www.w3.org/ns/prov#>
PREFIX rdf:  <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX xsd:  <http://www.w3.org/2001/XMLSchema#>

ex:bulletin a ev:Source .
ex:bulletin-r1 a ev:SourceRevision ;
    prov:specializationOf ex:bulletin ;
    ev:contentDigest "sha256:0123456789abcdef" ;
    ev:publishedAt "2026-01-01T10:00:00Z"^^xsd:dateTime ;
    ev:firstAvailableAt "2026-01-01T10:03:00Z"^^xsd:dateTime .
ex:field-ref a ev:EvidenceRef ;
    ev:sourceRevision ex:bulletin-r1 ;
    ev:jsonPointer "/affected/0" ;
    ev:valueDigest "sha256:abcdef0123456789" .
ex:claim-1 a ev:Claim ;
    rdf:reifies <<( ex:release ex:affectedBy ex:vulnerability )>> ;
    ev:claimKind ev:Reported ; ev:polarity ev:Positive ;
    prov:wasDerivedFrom ex:bulletin-r1 ;
    prov:wasGeneratedBy ex:import-1 ;
    prov:wasAttributedTo ex:publisher .
ex:link-1 a ev:EvidenceLink ;
    ev:target ex:claim-1 ; ev:evidence ex:field-ref ;
    ev:relation ev:Supports ; prov:wasAttributedTo ex:importer ;
    prov:wasGeneratedBy ex:import-1 .
ex:import-1 a prov:Activity ;
    prov:wasAssociatedWith ex:importer .
```

The shortened digest strings illustrate the fields. Validated fixtures use full
SHA-256 digests of their actual source bytes and selected values. A later not-affected
statement has another claim IRI, its own scope and a contradiction link. It does not
overwrite `ex:claim-1` or automatically retract a different release's finding.

## 5. Writes, review and accepted projections

`POST /{ds}/claims` is an additive proposed operation over C17's existing guarded write
path. It accepts claim occurrences, source references and evidence links as one batch.
It supports `branch`, `dryRun`, `ifHead` and `idempotencyKey` with C17's semantics.
Proposal creation writes profile records only. It never inserts the reified proposition.
The authenticated caller is the creation activity's principal. Source authors and model
names remain attributed metadata, not alternate authenticated principals.

`POST /{ds}/claims/review` appends review decisions. A request names exact target IDs,
target content digests, the review policy and expected branch head. Its disposition is
`accepted`, `rejected` or `needs-evidence`. The operation does not mutate a claim or an
assessment. Reversing a review appends a decision that supersedes the exact prior
decision. A stale target digest, head or policy version gives `409` and writes nothing.
Repeated identical idempotent requests return the first receipt. Changed payloads under
the same key conflict. No caller can replace an authenticated reviewer with client data.

Acceptance writes ordinary triples only when an explicit projection policy requests it.
The policy declares its positive admitted claim kinds, scope constraints, reviewer
permissions and output graph. Negative or uncertain claims are retained as qualified
claims. They do not enter a positive assertion projection. A policy may record a
conflict while admitting both incompatible positive claims for examination, but it must
label that projection accordingly and cannot call it a consistent logical closure.

An admitted occurrence creates a distinct acceptance support linking its claim and
decision to the projected triple. Removing one support deletes the projected triple only
when no admitted occurrence still owns it. Projection graphs are dedicated managed
graphs. Existing arbitrary assertions are not mixed into them. Direct Graph Store or
SPARQL mutation of a managed graph is refused as `profile-managed-graph`; the normal
write path checks this after resolving the candidate's graphs and before publication.
An administrator can explicitly detach the graph from profile management before editing
it, which marks dependent managed outputs unavailable until reconciled.

C25 owns these accepted-claim projections. F22 writes derived assessments to separate
managed graphs. To promote one into accepted assertions, an explicit policy submits
the claim through this operation rather than competing with C25's support ownership.
Read visibility still follows graph and triple grants. An accepted projection must
be built within one authorized evidence boundary, or use the supported-visibility
machinery of F18 once available. A reviewed flag alone cannot declassify its supports.

The current `assert_facts` tool and `POST /{ds}/facts` continue to assert their supplied
facts, with existing supersession and retraction behavior. They are not silently changed
into proposals, and old clients are not treated as new-profile clients. New clients
discover the claims capability before using it and cannot fall back to an asserting
endpoint when it is absent. Existing source, span and reifier records can be read through
a compatibility mapping. Migration is explicit, previewed and preserves their identities.

## 6. Identity, validity and correction

An identity proposal names two or more exact entity identifiers and a relation of
`ev:PossibleMatch` or `ev:DistinctEntities`. A review decision selects or rejects a
proposal under its versioned matching policy. Neither proposal nor acceptance asserts
`owl:sameAs`. An application may use a reviewed resolution map with stable entity IDs,
but its version and every contributing decision remain inspectable.

A merge does not rewrite immutable source claims. The resolution map maps source IDs
to a selected entity in a particular revision. A split creates a new map revision and
invalidates assessments that consumed the old mapping through F22. Names, account IDs,
mail addresses, shared infrastructure and organization roles retain their distinct
source meanings. An account does not become a person because its display name matches.

The common predicate `vt:validDuring`, in `urn:x-sparkles:valid-time:`, attaches C27
validity records. Recorded time, first availability, source publication and valid time
remain distinct. Missing validity means unknown validity, not an interval of all time.
`vt:expiresAt` is a review or freshness deadline for current use. It does not end the
underlying claim's valid-time interval or prove that the old claim is false.

A source correction creates a new revision and new claim occurrences, with
`prov:wasRevisionOf` where correspondence is known. A withdrawal records its activity
and `prov:invalidatedAtTime`. It does not manufacture a contrary claim. A decision can
withdraw an occurrence's admission while independent surviving occurrences remain.
The operation emits changed IDs for F22 to mark dependent analyses for review or
recomputation. It does not assume every dependent conclusion must become false.

## 7. Configuration, access and retention

An opt-in C19 evidence setting declares profile version 1, source-record graphs,
proposal graphs, managed acceptance graphs and named review policies. Its persisted
settings envelope uses version 1 and the format registry. Unknown settings versions or
required profile features refuse managed operations without rewriting data. Ordinary
SPARQL can still read the RDF under its grants. No index or vocabulary migration is
required. Profile vocabulary and shape revisions are named resources with content digests.

Enabling managed projections also reserves a dataset-reader capability through the
central format registry and raises `minimumReader` before the first managed write,
using the existing admission mechanism described in C26. This prevents supported older
writers from bypassing protection of the new managed graphs. A profile triple alone
cannot make an older binary enforce the new contract. Releases predating that admission
gate are outside the downgrade boundary. Unmanaged evidence RDF remains readable under
ordinary grants, while future unknown profile versions are not writable through these
operations.

All profile-managed source, rendition, passage, claim, assessment, evidence-link,
identity and review-decision graphs have typed writers. The same core admission
check protects them from ordinary SPARQL Update, Graph Store writes, LOAD, RDF Patch,
mapping output, imports, restore overlays and merges. Update permission permits a
caller to submit an authorized typed operation, not to bypass that operation by
mutating its RDF records directly. Internal batch imports use the same invariant checks.
A typed operation appends immutable revisions and their provenance. It cannot replace
an existing proposition, passage, evidence link or decision in place.

The server authenticates availability receipts, review authors and protected creation
fields. User-supplied copies of these predicates in unmanaged RDF are ordinary claims,
not authenticated profile records. Resolvers and admission policies require a managed
record and its verified publication origin. Raw namespace or class membership is
insufficient. A matching IRI with conflicting content is refused rather than shadowing
an authenticated record.

Only an authorized retention or erasure operation can remove managed immutable content.
It records the permitted audit state, invalidates affected projections and follows the
retained-history limitations below. Detaching an accepted projection never turns its
source or decision graphs into editable authenticated evidence.

A proposal needs update rights for every graph it changes. A review needs the policy's
review permission, read access to its complete evidence and update access to the
decision and projection graphs. A caller cannot approve a decision using evidence it
cannot inspect. Export, recall and summaries apply the same support visibility. A
restricted caller cannot infer hidden support counts, labels or target existence from
errors. Explicitly configured declassification is separate from ordinary review.

Raw bytes and quoted passages follow their source's retention and redistribution terms.
The profile stores locators, hashes, rights metadata and availability state when it
cannot retain content. A digest verifies identity, not permission to fetch a source.
Readers resolve artifacts through configured collectors and outbound policy, never an
unrestricted URL fetch triggered by source text.

Withdrawal, expiry and erasure are distinct. A withdrawal leaves an inspectable historical
record where permitted. Erasure removes the required source content, passages, claims,
quotes and derived copies according to the deployment's retention policy. References
then report unavailable and dependent outputs lose admission or become stale. The
profile makes no claim of immediate physical erasure from retained commits or backups;
those require the configured F06 and F05 retention and deletion procedures.

## 8. Shapes, bounds and failure behavior

The versioned shape set covers each class's cardinalities, term kinds, enums, digest
syntax and references. Separate checks verify cross-graph identity, snapshot availability,
source span content, authenticated provenance and managed projection ownership. SHACL
validation alone is not presented as an authorization or cryptographic-integrity check.
API admission validates the complete candidate before its one ordinary commit.

Defaults bound a batch to 1,000 claim occurrences, 10,000 links and 16 MiB of decoded
input, with existing request limits allowed to impose smaller bounds. References and
quotes count toward those limits. Resolution streams referenced records with a
64 MiB per-operation memory limit, the request deadline and cancellation. An operation
that cannot verify all mandatory references returns `incomplete-evidence` and writes
nothing. It never substitutes truncation for a verified admission decision.

`immutable-record-conflict`, `span-mismatch`, `unanchored-evidence`,
`evidence-unavailable`, `unsupported-profile` and `profile-managed-graph` have stable
machine codes. Invalid shape or scope returns a field-specific `400`. A changed head,
target or policy returns `409`. Permission failures use the existing non-disclosing
access response. Budgets use the existing request error contract. No batch publishes
partial claims or accepted triples after a failed check.

## 9. Acceptance and verification

| Example | Required result |
|---|---|
| A1. Two sources repeat one proposition. | Two claims and references survive. Withdrawing one accepted occurrence leaves the other and its projected triple. |
| A2. A proposal describes an affected release. | Plain BGP and inference input see no affected-release proposition until an explicit admission policy projects it. |
| A3. A source denies the same proposition. | Its negative claim remains separate. Contradiction and scope are queryable without inventing an RDF negation rule. |
| A4. A mirror and translation repeat a bulletin. | Lineage exposes their common origin. A source-count query cannot label them independent corroboration by default. |
| A5. Review accepts a claim, then reverses. | Both immutable decisions remain. The accepted projection removes only that occurrence's support and respects other accepted occurrences. |
| A6. A source changes between preview and review. | Head or target preconditions refuse the stale action without writing a decision or projected triple. |
| A7. A dataset or branch name is reused. | A pinned reference retains the old UUID identity and cannot resolve to the replacement. Unretained evidence reports unavailable. |
| A8. Evidence contains blank nodes. | A stable reifier or explicit import mapping preserves identity. A supplied blank label alone is refused. |
| A9. A matching decision is split later. | Immutable source claims remain intact. Consumers of the old resolution map are identified for review through F22. |
| A10. A source contains tool instructions. | Recall labels them as source data. They do not change caller authority, outbound permissions or execution policy. |
| A11. A reviewer cannot read one support. | The admission fails without revealing the hidden support's existence or value. A public summary cannot bypass that boundary. |
| A12. Legacy memory is read and written. | Existing asserting semantics, reifier IDs, source spans, idempotency and supersession remain unchanged. |
| A13. Validity expires or a claim is withdrawn. | Historical evidence remains where permitted. Current admission and dependent freshness follow their explicit policies rather than changing truth implicitly. |
| A14. A request is interrupted or retried. | It publishes either the complete operation or nothing. A retry with the same key cannot duplicate occurrence supports. |

### Managed-write integrity acceptance

Attempting to change an admitted claim's proposition, forge first availability or
replace a review author through SPARQL, Graph Store, RDF Patch, import, mapping or
merge must refuse before publication and leave accepted facts unchanged. An authorized
successor operation creates a new record with separate admission. Unmanaged copies
never resolve as authenticated records.

Phase 1 ships vocabulary and shapes, valid and invalid RDF fixtures, mapping examples
and deterministic competency queries. Parser and round-trip tests cover triple terms,
directional language strings, precise literals and source selectors. Phase 2 adds
state-machine and transaction tests with independent expected accepted sets, including
duplicate support ownership and attempted direct edits. Phase 3 adds C17/C18 recall,
F22 invalidation and C22 report integration tests under changed access and source state.

Before performance claims, measure proposal, review, withdrawal and bounded reference
resolution at increasing claim and support counts. Report memory and retained artifact
cost separately from graph storage. Correctness requires exact evidence and occurrence
identity, not only matching counts. No speed target is inferred from engine benchmarks.

## 10. Alternatives and outcome

Using plain asserted triples for every extraction would let hypotheses enter reasoning
before review. One reifier per proposition would lose source-specific occurrences.
One generic dependency relation would confuse mentions, support, derivation and work.
Automatic owl:sameAs links would make reversible matching into global logical identity.
A separate evidence database would complicate transactional review and graph access.
This design uses ordinary RDF with explicit profile operations instead.

## Sources

- [RDF 1.2 Concepts, Candidate Recommendation Snapshot](https://www.w3.org/TR/2026/CR-rdf12-concepts-20260407/)
  supplies triple terms and the distinction between describing and asserting a triple.
- [PROV-O](https://www.w3.org/TR/prov-o/) supplies entities, activities, attribution,
  derivation, revision and invalidation relations.
- [Web Annotation](https://www.w3.org/TR/annotation-model/) supplies source selections
  and passage targeting. C18's existing normalized-text offsets remain authoritative.
- [SHACL](https://www.w3.org/TR/shacl/) supplies the shape language, and
  [RFC 6901](https://www.rfc-editor.org/rfc/rfc6901) supplies JSON Pointer syntax.
- The design also uses the existing contracts of C17, C18, C19, CI, F05, F06, F09,
  C12 and C12b. No external implementation is adopted and no runtime dependency is added
  by this specification.

## Provenance

This design shares the [evidence and analysis provenance record](shared/PROVENANCE.md#evidence-durable-work-and-maintained-analysis)
with C25, C26, C27, F22 and C22b. The record preserves their common standards
and each profile's adopted and rejected decisions. It does not imply shared
implementation status. This spec's status and Outcome describe its delivery.

## Outcome

Phase 1 delivers the version 1 RDF resource profile under
[`crates/sparkles/resources/evidence/v1`](../../crates/sparkles/resources/evidence/v1/README.md).
`sparkles::evidence::profile(1)` exposes the vocabulary, shapes, eight competency queries
and two compatibility views without opening or changing a dataset. Unknown versions
return `None`. An embedded manifest names the resources and records their full SHA-256
content digests. The resource version does not alter the on-disk format or enable a
managed-write capability.

The shapes cover source revisions and renditions, unasserted occurrences, three exact
reference structures, qualified evidence, assessments, confidence, authored records,
review decisions and identity proposals. They preserve domain scope and distinguish
source lineage from independent corroboration. The zero-to-one confidence domain is
checked exactly from decimal lexical forms, including long fractions, without floating
point rounding. Other scales remain uninterpreted.

The [fixtures](../../testsuite/evidence/v1/README.md) retain actual source bytes and full
SHA-256 digests. They cover Unicode selectors, RFC 6901 escaping, graph-kind identity,
RDF 1.2 triple terms, directional language strings and precise literal round trips.
Independent expected query results distinguish occurrence identities, assertion status,
opposite-polarity candidates, mentions, missing visible support and unknown lineage.
Invalid overlays exercise structural refusals and inconsistent passage identities.
Compatibility views retain C17 and C18 identifiers and their original assertion
semantics without inventing authenticated revisions or availability. Invalidated reifiers
stay historical even after a new occurrence reasserts the same business triple. Rendition
metadata and citations join across independently authorized graphs. Evidence query results
retain each qualified link and creation activity. Invalid fixtures assert their intended
constraint component, with mutation controls separating enum and digest failures from
incidental cardinality failures.

These resources are ordinary RDF under the caller's access policy. A conforming shape
report does not authenticate a source, author, receipt or timestamp, resolve a retained
snapshot, verify source bytes or enforce immutability. The existing C17 assertions,
supersession and C18 source records retain their behavior. No evidence setting, claims
endpoint, protected profile registry or accepted projection ships in Phase 1.

The engine's `sparkles_core::evidence` module supplies typed input records and bounded
local preparation for Phase 2. It verifies retained content digests, normalized-text
passages and explicitly versioned lexical JSON selections. JSON selection preserves
exact retained UTF-8 bytes and rejects duplicate decoded keys. References sharing a
revision are resolved together, and literal components are budgeted before validation
can copy them. Independent framing goldens, selector oracles, allocation checks and
cancellation controls cover this preparation contract.

The same module decodes complete versioned evidence settings and produces canonical
bytes, values and digests. Source and proposal roles select unordered graph sets and
may share a graph. The union is bounded to 1024 IRIs, each at most 4096 UTF-8 bytes.
Raw JSON strings are bounded before decoding can allocate escaped-string scratch.
Canonical settings are data for the managed configuration factory, not an authenticated
revision, graph reservation or administration capability.

`prepare_input` returns explicit requirements for references that need retained-view,
identity or method resolution. A runtime must discharge those requirements through
the caller's authorized view before publication. These helpers do not open a dataset,
authenticate source availability, register managed graphs or authorize a write. The
authenticated proposal writer described below discharges them.

The runtime ownership prerequisite gives each opening one shared family identity.
Persistent families retain a lease on the root process lock. Persistent and in-memory
members inherit their family's identity.
Writer permits span acquisition through grouped publication or refusal, including
when a submitting caller unwinds. Branch creation, deletion, renaming and table
settings enter the same gate before their branch locks. Store resources and branch
locks are destroyed before the final root lease. A retained persistent member or
branch set can therefore outlive main without releasing process exclusion early.

An earlier resident source ownership prerequisite supplied lifetime leases of exact
catalog openings for typed writers. Neither the evidence runtime nor C26 adopted them,
because resolution reads retained commits by identity through the catalog and holds
instead of keeping openings alive, so they were removed with the retained-reference
step below.

Catalog deletion, persistent rename and backup replacement now use a final reversible
runtime close proof before filesystem publication. Nonblocking admission on the existing
family gate refuses active writers and Pending group publication. Built-in state and set
links freeze reversibly. Actual Store, State and Set allocations must satisfy exclusive
ownership, with raw external Strong and Weak aliases conservatively busy. Refusal restores
all original slots, caches and weak links before resuming workers, without changing files.
Removed and retired members remain inventoried until their final resource drop completes.
Delete and rename refuse at once. Backup replacement retries a refused proof within its
drain time, because a built-in owner such as a views worker round can hold a short-lived
strong reference after the drain observed none.

The public Own(Store) and Branch(Arc<Store>) variants retain their payloads and behavior.
A private bounded empty in-memory Store temporarily parks inaccessible Branch slots during
proof. It has zero cache budgets, grants no authority and is destroyed before directory
publication. Main stays in its original Own payload. Close uses storage proportional to
the resident state and opening inventory. Terminal links wake workers before destructors,
and catalog lookup exclusion ends before callbacks or checkpoints. Catalog mutation remains
serialized. A management call from a close callback on the disposing thread is refused at
once with a `catalog-disposing` conflict instead of waiting on the lock its own disposal
holds. Callbacks queue such calls with `Catalog::defer`, which runs them on the same thread
after the delete, rename or restore releases the lock and before it returns. Each deferred
call runs under its own panic boundary, so a panic is logged and later calls still run.
Deferred calls live only in memory and are lost if the disposing call unwinds or the
process stops, so callbacks must be idempotent. Management calls from other threads still
wait for the lock. Members closed during unwinding retain the existing behavior that skips
clean-close checkpoints. Family admission and the real root process lease still span
required resource shutdown, including a captured callback panic.
A precommit panic restores the same graph and catalog entry before propagating its
original payload. A delete or rename keeps the dataset registered as closed while its
runtime closes, so a callback panic after the close commits leaves it in `config.json` for
the next catalog open. Live unmatched detached openings refuse without being upgraded into
close-owned Arcs, and a final drop witness keeps a moved-out Store value busy.
Later I/O failure recovers through a fresh existing-only opening rather than reviving
closed links. Recovery checks a known original UUID before and under process exclusion
and cannot create a missing root or empty generation. A failed delete move is synchronized
back before reopen, or remains unavailable with its original bytes and reported path.
Uncertain moved data uses the existing retained `.kept-<name>-<token>` convention.
Catalog startup refuses only a registered root that lacks its directory or CURRENT,
names the expected path and serves the other datasets. Restore publication
checks the restored identity separately from original-identity rollback. Existing roots
without a known expected UUID retain the supported legacy metadata upgrade only when
CURRENT already exists.

These checks establish runtime closure only. Operational observation, privacy
visibility, typed writes and C25/C26 consumer composition remain separate dependencies.
Durable source holds are described below.

The managed-write boundary and its reader admission are available as the first Phase 2
step. `sparkles_core::managed` defines owner kinds, a `ManagedGraphs` registry and a
`ManagedWrite` capability that only `Store::managed_writer` makes, for a registered
owner and the registry installed at that moment. `Store::enable_managed` raises the
dataset reader to 6 durably, writes the owner's settings file, writes `managed.json`
format 1 as the commit point and installs the registry. One registry belongs to the
dataset's runtime family, so main and every branch admit against it, including branches
created before enablement, and branch directories do not copy it. Each owner entry
records its profile, settings digest and graphs, and a `visibility` and `merge` member
whose defaults `public` and `refuse` are the only values this build enforces.

Every commit point admits the candidate before its dry-run return. These are ordinary
transactions, bulk loads and replacements, whose source quads the builder checks as it
reads them, RDF Patch, merges, replays and cherry-picks, including their previews.
`bypass_validation` does not skip admission and merges get no exemption. Each distinct
graph of a candidate is decoded once, and a store without a registry pays one atomic
load and a `None` check. A managed graph changes only in a transaction that carries its
owner's capability. Such a transaction starts from the durable head, is never grouped,
and `WriteTxn::managed_stamp` fixes its sequence number and timestamp before publication.
A newer `managed.json`, an unknown owner or profile, a reserved member value or a
mismatched settings digest keeps reads and refuses every write as `managed-unavailable`.
An owner settings file whose `format` or `profile` this build does not read is
unavailable in the same way, even when its digest matches. Enabling the owner again
repairs a mismatched digest. A change of the registry that fails or unwinds after its
first file is written leaves the running dataset refusing every write until the owner
is enabled again. Enablement returns only after every writer that entered the family
before the new registry was installed has published or refused, including writers of
deleted branches whose handles are retained. Later writers, including nested family
operations of those writers, are not held back. `Store::detach_managed`
returns a graph to ordinary writes. Backups, restores and clones carry the registry. A
registry that names an owner this build does not know, or whose format it does not read,
is not copied, and the backup or clone is refused. The server
answers `profile-managed-graph` with `409` and `managed-unavailable` with `503`. In
library mode the capability guards against mistakes, not a hostile embedder.

Failing-first tests cover SPARQL Update, Graph Store writes, uploads, LOAD into an empty
store, bulk replacement, RDF Patch, merges, squash merges, replays, cherry-picks,
validation bypass, dry runs, branches created before enablement, foreign, stale and
wrong-owner capabilities, unenforceable registries, the reader refusal of a reader 5
build, crashes between each enablement step, exact stamps under group commit, and
backup, restore and clone protection. Replay refuses at the first replayed commit that
changes a managed graph, so the commits before it stay replayed, as with other replay
refusals.

All 14 ownership tests pass without optional features and with `text,geo`. The
control for retained members also passes in a separate exact run. Another 131 tests
cover grouped commits, process locks, memory branches, branch operations and shutdown.
Strict core lint checks pass for all targets in both feature configurations. The
original implementation fails the control's assertion that the root stays locked
while a member outlives main.

Evidence settings administration is the second Phase 2 step. `evidence.json` format 1
and profile 1 hold six roles and named review policies. `sourceRecords` and
`proposals` keep their meaning, and `decisions`, `supports`, `identity` and
`projections` hold review decisions, acceptance supports, identity records and accepted
projections. A projection graph has no other role. Each policy has a `version`, its
`admittedKinds`, its `requiredScope` predicates, a `conflicts` mode of `refuse` or
`admitLabelled`, an evidence `boundary` of source-record and proposal graphs, and the
`decisions`, `supports` and `output` graphs it uses. Each projection graph is the output
of one policy. Canonical bytes sort every set and key and name every member, and the
four later roles and `policies` default to empty when an object leaves them out.

`sparkles::evidence::configure` and `PUT /$/evidence/{ds}/settings` validate complete
settings and hand every graph of every enabled role to the evidence owner through the
managed-graph registry, with the canonical bytes as `evidence.json`. A configuration
never stops protecting a graph. A graph leaves protection only through
`sparkles::evidence::detach` or `POST /$/evidence/{ds}/detach`, which keeps it in its
role, and the next configuration protects it again. The registry change uses
`Store::enable_managed_at`, which refuses a revision other than the one the
configuration read, and a detach uses `Store::detach_managed_at` in the same way, so
concurrent configurations and detaches never undo each other. Status and mutation
answers come from one `Store::managed_snapshot` or from the registry the mutation
committed. A detach first checks that this build reads the recorded settings and
otherwise changes nothing. A configuration interrupted between `evidence.json` and
`managed.json` refuses every write until the next configuration, which is checked
against the protection `managed.json` records and repairs the registry. After an
interrupted first configuration there is no `managed.json`, and the next configuration
starts from no protection at revision 0, on the live dataset or after a reopen. An
in-memory clone of such a dataset has no recorded registry to repair from, so it keeps
refusing every write rather than starting from no protection.
All three routes need `admin`. A `format` or `profile` other than 1 answers
`409 unsupported-profile`.

Tests cover canonical goldens computed independently, policy reference and dedication
rules, duplicate policy names, refusal of weakening, detach and protection again,
stale revisions, persistent reopen, a later settings profile that refuses every write,
unreadable recorded settings, repair after an interrupted configuration, concurrent
configuration and detach, coherent status reads during configuration, and the HTTP
routes.

#### Settings decisions

The open questions of this step were decided as follows.

1. **Additive amendment.** No release carries `evidence.json`, so the new roles and
   `policies` amend format 1 and profile 1 instead of starting a profile 2. Objects
   written with only the first two roles still read.
2. **Policy references.** A policy names exactly one decisions graph, one supports graph
   and one output graph of the matching roles. Review permission then reduces to update
   grants on those graphs and read access to the evidence, as §7 requires, without a
   separate reviewer list. The boundary is limited to source-record and proposal
   graphs and is checked statically here. Per-review containment belongs to the review
   operation.
3. **Scope constraints.** `requiredScope` lists the scope predicates an admitted claim
   must carry. Value constraints on those predicates are domain policy and stay with
   the application.
4. **No weakening.** Configuration only adds protection. Removing a protected graph
   from every role or disabling the profile while it protects a graph is a conflict,
   so a client that sends stale settings cannot silently drop protection. Detaching is
   explicit and per graph.
5. **Protection is not authentication.** Placing a graph under management, first or
   again after a detach, does not vouch for what it holds. Typed operations will trust a
   record only through its verified publication origin, as §7 requires, so records
   written while a graph was ordinary never become authenticated by protecting it.
6. **Disabled settings.** A dataset that was never configured stores no disabled
   settings. Defaults write nothing and anything else is refused, so a configuration
   never raises the reader for data that protects nothing.
7. **Disclosure.** Reading the settings needs `admin`, because they name protected
   graphs and policies that graph-limited readers may not see. Typed operations will
   advertise their capability separately.
8. **C19 layering.** `evidence.json` holds the complete canonical object whose digest
   `managed.json` records, so the settings are not a C19 layered kind in this step.
   Declared settings and drift refusal are roadmap item 508.
10. **Bindings.** The settings operations are HTTP and Rust only in this step. The
   Python, JVM and Node.js bindings receive them with the typed evidence operations,
   as roadmap item 509 records.
9. **Vocabulary.** Terms for supports, withdrawals, resolution maps, operations and
   receipts are added with the operations that write those records, so that their
   shapes and fixtures are tested against real output.

Durable source holds are the third Phase 2 step. `sparkles_core::holds` records in
`holds.json` format 1 at the dataset root the commits that owners keep readable on
`main` and every branch. A hold has an owner-chosen UUID, its owner, its creation time
and a sorted set of commits, each named by the branch identity the caller gave and the
branch store whose history serves it. A commit at or below a branch's starting point is
served by its upstream. A read of the starting commit on the branch falls back to the
upstream once the branch no longer keeps its own copy, so the hold keeps it readable
under both names. Together the holds of a dataset name at most 65,536 commits. The
file is refused above 32 MiB, and the format check at open reads no more of it than
that. `Store::acquire_hold`, `Store::release_holds`,
`Store::source_holds` and `Store::reconcile_holds` change and list them through the main
store, and `sparkles::evidence` and `/$/evidence/{ds}/holds` expose the evidence owner's
holds to administrators. Acquiring the same hold again returns it, and other commits
under its id are a `hold-conflict`. The first hold raises the dataset reader to 7, a capability
of its own, so a release that honors managed graphs but not holds refuses the dataset
instead of trimming held history.

Every collector keeps a held commit. Each store's history state reads the family's held
commits from one lock-free index when it decides which generations it needs, so the
retention window and its generation and byte limits, compaction, retention changes,
backup lease releases, branch deletions and the history tick keep every generation that
serves a held commit. `sparkles check` counts holds among the generations it reports as
retained. An in-memory store keeps the held states beside its window. A branch that a
hold names is refused deletion as `branch-held`, and `DeleteOptions::release_holds`,
`?releaseHolds=true` and `sparkles branch delete --release-holds` release those holds
first. The hold routes and a branch deletion that releases holds need `admin` on every
branch and graph of the dataset, through a grant without a branch list or with the
branch pattern `*`. Holds of a retired upstream stay, since only a branch that started
from it can name them.

The ordering argument is as follows. An acquisition resolves the commits under the
branch table, opens the stores that serve them, takes the holds' lock and checks again
that every named branch is listed. A branch deletion takes the holds' lock before its
commit point, so no named branch is deleted meanwhile. It publishes the new index first and
then checks, under each serving store's history lock, that every commit is still
readable. A collection decides what to remove and removes it from the generation table
under the same history lock, reading the index there. Either the collection decided
before the index was published, and the check then finds the commit gone and the
acquisition fails with `history-gone` and restores the index, or the collection reads
the new index and keeps the commit. The acquisition then raises the reader and replaces
`holds.json` atomically, and returns only after the file and its directory are synced.
It installs the hold as soon as the file may have been replaced, including when the
rename reports an error, so the running store never protects less than the file
records. Such a hold answers an error and stays until a release or reconciliation
removes it. A write that fails after it may have replaced the file marks the holds
uncertain, and the next change or repeated acquisition writes the
installed holds again before it answers. A crash before the replacement leaves no hold, and
a crash after it leaves the complete hold. A release replaces `holds.json` without the
hold before it narrows the index, so a crash leaves either the hold in force or the
hold gone. A branch deletion that releases holds makes the release durable before the
branch table's commit point, so a crash in between leaves the branch without its holds.
The holds' lock comes before a branch table and a store's history, because a
collection takes the table under its history lock. Acquisitions never take a writer
lock, and collectors never take the holds' lock.

An owner acquires its holds before the commit that refers to them. When that commit
never publishes, the hold is an orphan. `Store::reconcile_holds` asks the owner about
each of its holds last acquired more than a grace period ago, one hour by default, outside
every lock and releases those it does not keep, unless the hold was acquired again
meanwhile. Each acquisition of an existing hold starts its grace period again, and a
reopened store counts it from the hold's creation. The
typed evidence operations supply that check when they record references. A
`holds.json` that cannot be read or names another dataset keeps every past state and
refuses hold changes and branch deletions with `holds-unavailable`, while reads and
writes continue. A newer format refuses the open. Backups and clones carry no holds,
because they carry no past states, and deleting a dataset deletes its holds. The file
is rewritten whole for each change, which costs up to a few megabytes of writing at the
bound. That keeps one atomic commit point per change.

Tests cover reopening, compaction, the retention window with its byte limit and the
tick, idempotent and conflicting acquisitions, refusals that record nothing, branch
commits held in their upstream, branch deletion with and without released holds, an
in-memory dataset and its branches, reconciliation with its grace period and failing
checks, the exact 65,536-commit bound and its reopening, unreadable, foreign and newer
files, offline retention, and crashes at every acquisition step, during a release and
between a released hold and its branch deletion. A concurrency test races acquisitions
and releases with commits and compactions and finds no held commit unreadable. HTTP
tests cover acquisition, idempotence, paging, branch filters, deletion of a held
branch, release and each refusal.

Retained-reference and record resolution is the fourth Phase 2 step and the first
production caller of [C29](C29-bounded-view-preparation/README.md).
`sparkles_core::evidence::resolve::Resolver` verifies the references of one operation
under one `Preparation` of 64 MiB with the request deadline and cancellation, and
`sparkles::evidence::resolve::Resolution` adds the catalog. A stored fact names its
dataset and branch by UUID. The dataset is found among the catalog's registered datasets
with `Catalog::try_get_by_id`, and the branch among the dataset's listed branches. The
caller decides access on their current names before anything is read, so a renamed
dataset or branch keeps resolving and a recreated name never does. An anchor that the reference alone refuses is refused
before the dataset is looked up, so its answer does not depend on whether the dataset
exists.
`Store::snapshot_of_branch_limited` then reads exactly the referenced commit within the
remaining allowance, without opening a cold branch store, and never another branch or the
head. A commit at or below a branch's starting point, including a held starting commit
that the branch no longer keeps, is read from the upstream that serves it, and the result
names that serving branch. The snapshot's conservative delta charge stays reserved while
its view is kept, and `GraphAccess::masked_controlled` prepares the caller's graph
policy and triple mask under the same allowance. Each commit is prepared once per
operation and shared by its references.

A guard decided at the snapshot, such as the GraphRAG disclosure guard, is bound by
that preparation before the graph is checked. A ground fact is found by controlled term
lookups and one quad probe in the prepared view,
and a named reifier must reify that triple in the same graph. A fact with blank nodes
needs a reifier. The one triple term it reifies in the graph must match the expected
triple up to a consistent renaming of blank nodes, and that stored triple must be
asserted there. A blank-containing fact without a reifier, a reifier of more than one
triple and an import-mapping anchor are `unanchored-evidence`, and the first two of these
are decided from the reference before anything is read. A missing dataset, branch, commit,
graph, term, fact or reifier and one the caller cannot read all give the same
`evidence-unavailable` error and message. The resolution's memory limit and an access
policy that this bounded entry point cannot prepare, such as a protection pattern, give
`incomplete-evidence` and never read as absence. The request deadline and cancellation
keep the request error contract.

A record reference names an IRI, its allowed kinds and an optional content digest, and
resolves at the current state of the requesting branch. The record must have exactly one
profile class in exactly one graph that the evidence settings give a role other than
`projections` and that the evidence owner protects now. A copy in an ordinary or detached
graph is not a profile record. The record carries exactly one `ev:availabilityReceipt`
whose `ev:datasetId`, `ev:branchId` and `ev:commit` name this dataset, a branch it lists,
retired or recorded as deleted, and the publishing commit. That is the receipt layout the
typed writers record. A missing receipt, or one naming another dataset or an unknown
branch, is `origin-not-reproduced`, as after a restore into another identity. Another
kind, another content digest, two digests or a second copy of the record is
`immutable-record-conflict`. A passage's single selector start and end are Unicode code
point offsets into its rendition's single `ev:normalizedText`. That rendition must itself
resolve as an authenticated record, and the digest of the selected text must equal the
passage's `ev:textDigest`, or the answer is `span-mismatch`. The text is read as its
stored bytes under the allowance rather than decoded as a term. The registry is read
from the running state without a copy, and its settings decoding is charged before it
allocates. Each record reads the running registry again, so a detach or another
management change made during one resolution applies to its later records.

A graph that the caller's grants refuse is refused before any history is read. A
GraphRAG disclosure guard is decided only once the view is bound to its snapshot, so the
check made before the read treats each undecided guard as passing and applies every
other rule.

`POST /{ds}/claims/resolve` exposes both reference forms. It needs `read` on `{ds}`
through the query endpoint and builds each referenced dataset's view with the server's
caller context, once per dataset and branch for the request, and a client that
disconnects cancels it. It answers every verified reference in order, or the first one that
does not verify with its code and key. The six outcomes have the stable codes above, with
`422` for all but `immutable-record-conflict`, which is `409`. Malformed references are
field-specific `400` errors.

Tests cover exact commits against later heads, the default and named graphs, literal
datatypes and languages, triple terms, renamed, deleted and recreated branches and
datasets, starting commits served by the upstream, a held starting commit after its
history was trimmed, graph grants, guards decided at the snapshot, static graph
denials under an undecided guard with an allowance too small for the view, triple protections,
blank nodes with and without reifiers, the allowance and cancellation, branch lookups
while the branch table is held, view reuse, the record kinds, digests, receipts, detached
and ordinary copies, a detach between two records of one resolution, passages with code point selectors over a 4 MiB rendition, and the
HTTP route with and without authentication. The unauthorized and missing answers are
compared byte for byte, including for references refused from their anchor alone.

#### Resolution decisions

1. **Receipt verification.** The receipt check is a lineage check that does not read
   history, so a record stays verifiable after retention trims its publishing commit.
   It cannot tell a record that a typed writer published from one written with a
   correct-looking receipt while its graph was detached. The authenticated typed writer
   of the next step must record an operation marker that makes that distinction.
2. **Delta charge.** The 64 MiB allowance uses C29's conservative 2 KiB charge for
   each quad of a state's unindexed delta, so a state with tens of thousands of writes
   since its last compaction can answer `incomplete-evidence`. Roadmap item 542
   tracks a tighter charge for shared live states.
3. **Source leases.** The resident source lease family was removed rather than adopted.
   [C26](C26-durable-jobs-and-delivery.md) does not use it.
4. **Bindings and MCP.** The route is HTTP, OpenAPI and Rust only. MCP waits for Phase 3
   and the bindings for roadmap item 509.

Authenticated claim proposals are the fifth Phase 2 step. `sparkles::evidence::propose`
and `POST /{ds}/claims` publish one batch of source revisions, renditions, passages,
evidence references, claim occurrences, evidence links and assessments as one commit
of the evidence owner. A claim describes its proposition with RDF 1.2 reification and
never asserts it. Each record IRI is minted by the identity namespace of the dataset and
the authenticated principal from the record's scope and occurrence keys, so an equal
record from another occurrence key is another record and an equal payload gives the
same IRIs. A record goes to the settings' single source or proposal graph, or to the
one the request names when the settings have several. The server writes every
server-owned field. Each record gets its `ev:availabilityReceipt`, which is the
operation IRI, its `prov:wasGeneratedBy`, its graph's `ev:graphProtection` nonce and an
`ev:recordDigest` over its immutable content in a canonical triple order. The digest
counts retained bytes and normalized text by their presence only, so a metadata-only
rendition and one with text never share a digest. Each source revision gets its
`ev:firstAvailableAt`, the commit's time. Each written graph receives the operation as an
`ev:Operation` and `prov:Activity` with its agent, end time, dataset, branch, commit,
payload digest, optional idempotency key and source hold, and the graph's protection
nonce.

The protection nonce is the operation marker that resolution decision 1 required.
`managed.json` format 2 adds a `protections` map from each protected graph to a random
UUID. Protecting a graph keeps its nonce while the owner keeps protecting it, detaching
drops the nonce, and protecting it again chooses a new one. This build reads formats 1
and 2 and writes format 2, and an older build refuses format 2 as written by a newer
release. A format 1 registry has no nonces, so its graphs take no typed writes until
the owner protects them again. Resolution now requires the receipt to be an
`ev:Operation` in the record's graph, and both the record and the receipt to carry that
graph's current nonce as `ev:graphProtection`, otherwise the answer is
`origin-not-reproduced`. A record written while its graph was detached cannot carry a
nonce chosen after it. The nonce on the record itself matters because an operation IRI
derived from an idempotency key is predictable, so a planted record could otherwise
name an operation that a later typed write describes with the current nonce.
Resolution also reports each record's `ev:recordDigest`.

A proposal runs in this order.

1. The input is prepared, its records are minted and placed, and the caller's update
   grants on every written graph are checked from the grants alone.
2. The managed-write capability is taken, so any later change of the registry, such as
   a detach, refuses the commit. The settings the plan used must equal the settings
   of the capability's registry state, or the proposal answers a retryable conflict.
3. An idempotency key derives the operation IRI as a name-based UUID of the dataset,
   the principal and the key. When any record graph of the branch describes that
   operation, a payload
   with the same digest returns its first receipt without writing, and another payload
   is `409 idempotency-conflict`.
4. The references to existing records and stored facts are resolved under the caller's
   read access with the L4 resolver, so a reference the caller cannot read answers as a
   missing one.
5. The stored facts of this dataset are held under a hold whose id is the operation
   UUID, and the hold is durable before step 6 starts.
6. Under the branch writer, `ifHead` is checked as `409 head-changed`, the key is checked
   again, every record identity is checked, the commit is stamped and the record and
   operation quads are built. An existing identity in the same graph whose authenticated
   record has an equal digest is reported as `existing` and not written again, and any
   other existing identity is `409 immutable-record-conflict`. A publish hook then runs
   on the committing thread inside the commit. It takes the holds' publication lock
   shared, refuses when the hold is gone or holds other commits, and enters the host's
   authority fence. The thread keeps both guards until the commit has published or been
   refused.

The hold comes before the commit because a reference recorded without a durable hold
could lose its commit to retention before any hold existed. With this order a crash at
any point leaves either nothing, a hold without a published operation or a published
operation with its hold. A hold without an operation is an orphan. A retry with the same
key acquires the same hold again, because the hold id is the operation UUID and the
held commits are equal. `reconcile_operation_holds` releases each evidence hold that is
older than its grace period and whose operation no graph of `main` or a branch
describes as an `ev:Operation`, whatever its nonce. A changed protection does not show
that an operation never published, so such a hold stays. Every published proposal runs
the reconciliation with an hour's grace, after its commit, and its failure never fails
the proposal. Every release takes the publication lock exclusively, and a
reconciliation asks again under it which holds to keep, reading only the current
snapshots of the stores it found and taking no writer. A commit's hold check also
renews the hold, so a reconciliation that chose its candidates before the check does
not release it. A release therefore either
comes before a commit's hold check, which then refuses the commit, or waits until the
commit has published, and a reconciliation never releases the hold of an operation
that published after it first looked. A published operation always names a hold that
protects it.

The server's authority fence binds the commit to the caller's credential.
`AuthorityRegistry::begin_hold` takes a shared guard on the authority generation, and
`begin_revocation` takes it exclusively without changing the generation. Removing a
token, removing a session, ending an OIDC session and a login that evicts sessions over
their caps now take the exclusive guard, so a
revocation waits for a publication that already passed the fence, and a publication
after the revocation sees the credential gone and answers `503 authority-changed`. The
fence also refuses a changed generation and an expired credential. The guard is not
`Send`, so it lives in a thread-local slot that a drop guard clears, also on unwind.

`POST /{ds}/claims` needs `write` on the dataset or branch through the update endpoint,
an update grant on each written graph and read access to every referenced record and
fact. Its principal is the caller's authenticated log name as a
`urn:x-sparkles:principal:` IRI. A dry run checks everything except the hold and the
fence and returns a preview of the inserted and deleted quads. The response gives the
operation, whether it committed or replayed, the commit, branch, first availability,
hold and each record's key, kind, IRI, graph, digest and `existing` flag. The new codes
are `head-changed`, `idempotency-conflict` and `authority-changed`, and `GET /$/server`
advertises `capabilities.claims` with profile 1 and both routes. The vocabulary adds
`ev:Operation`, `ev:graphProtection`, `ev:payloadDigest`, `ev:idempotencyKey`,
`ev:sourceHold`, `ev:recordDigest`, `ev:retainedBytes`, `ev:locator` and
`ev:reportedRights`.

Tests cover deterministic identities, graph choice, the server fields on every written
graph, nonce persistence and renewal, each refusal code over Rust and HTTP, an equal
payload replayed, another payload under the same key and the same key of another
principal, a key reused for records in other graphs, a replay of an operation that
reused records, a stale `ifHead`, a head moved by a concurrent write after the hold, an
existing equal record and a conflicting one, records written while a graph was detached,
a planted record that names a later operation, a batch key spelled like a referenced
record IRI, one existing record used as two kinds, a metadata-only rendition against one
with text, a release from another thread after the hold check, a reconciliation after a
detach and a new protection, a login that evicts a session during a publication, a
format 1 registry,
a proposal on a branch, a dry run, missing update grants on one written graph, a
referenced record the caller cannot read compared byte for byte with a missing one, an
authority revoked between preparation and the commit, a token revoked after the hold
over HTTP, and failpoints after the hold and after the commit followed by
reconciliation and a retry.

#### Proposal decisions

1. **Unverifiable selections.** A passage of an existing rendition, a reference to an
   existing passage and a structured value outside a retained JSON revision of the
   same batch answer `incomplete-evidence`, because this step cannot yet verify them
   against retained content. Roadmap item 551 tracks their verification.
2. **Holds in other datasets.** A stored fact of another dataset is verified but not held.
   Roadmap item 550 tracks holding it under the operation.
3. **Records of an earlier protection.** A detach and a new protection invalidate the
   records written before them, which then answer `origin-not-reproduced`. Roadmap item
   552 tracks an administered re-authentication.
4. **Blank nodes.** Record content refuses blank nodes, except in a stored fact's
   expected triple, so that the record digest and the conflict check compare ground
   content.
5. **Bindings and MCP.** The route is HTTP, OpenAPI and Rust only. MCP waits for Phase 3
   and the bindings for roadmap item 509.

Review decisions, accepted projections and withdrawals are the sixth Phase 2 step.
`sparkles::evidence::review` and `POST /{ds}/claims/review` append one immutable
`ev:ReviewDecision` per target under a named policy of the evidence settings, in the
policy's decisions graph. A decision names its target, the target's `ev:recordDigest`,
the policy as `urn:x-sparkles:evidence:policy:{name}`, the policy version, its
disposition and the authenticated reviewer. When the target already has a current
decision under the policy, the request must name it as `supersedes`, and the new
decision then replaces it without editing it. A target answers `target-changed` for
another digest, `decision-superseded` for a stale or missing `supersedes`, and the
request answers `policy-changed` for another policy version. Decision identities are
minted from the target, the policy and the operation, so a replayed key names the same
decisions and a keyless retry finds the first decision current and is stale. Before
publication every minted decision, support, withdrawal and projection identity must be
absent from the record graphs, so content written under a predictable identity while a
graph was detached answers `immutable-record-conflict` instead of being authenticated.

An acceptance of a claim projects it when the claim is positive, its kind is admitted
and it carries every required scope predicate. Other acceptances record their reason
as the decision's `ev:projectionNote` and project nothing, and a replay reads that note
and a support's `ev:conflictsWith` to answer as the first request did. The decoded
propositions an operation keeps are charged four times their size against a 64 MiB
allowance, and exhaustion answers `incomplete-evidence`. A projecting acceptance records an `ev:AcceptanceSupport` in
the policy's supports graph that names the claim, the decision, the policy, the output
graph and the `ev:supportedTriple`, and inserts that triple into the output graph. No
support is ever edited. A support is live while its decision is authenticated and no
authenticated decision supersedes it, and while its claim has no authenticated
`ev:Withdrawal`. After each operation, every triple it touched is in the output graph
exactly when a live support owns it there, so duplicate occurrences each own one support
and reversing or withdrawing one leaves the triple to the others.

Contrary evidence is an authenticated `ev:Contradicts` link to the claim, or an
authenticated negative claim of the same proposition in the policy's boundary that is
not withdrawn and whose current decision is not a rejection, decided against the
request's final decisions. A `refuse` policy answers `evidence-conflict`, and an
`admitLabelled` policy records the contrary records as the support's
`ev:conflictsWith`. Conflicts are decided only at admission. The first projecting
review of a policy version writes an `ev:Projection` record, minted from the policy and
version, with the output graph, the conflict mode and the protection nonces of the
decisions, supports and output graphs. When a projection record of the policy carries
other nonces than the current ones, because one of those graphs was detached and
protected again, reviews and withdrawals under the policy answer
`projection-unavailable`, since the output still holds triples whose supports no longer
authenticate. An output graph that holds triples while no projection record of the
policy exists, as after its supports graph was detached and cleared, answers the same
way, and so does a recorded projection whose output or conflict mode differs at the
same version.

A review needs update and read grants on the policy's decisions, supports and output
graphs, decided from the grants, and read access to each target, which must be in the
policy's boundary. An acceptance also needs read access to every authenticated evidence
link of the target in the boundary, the link's reference, the stored fact, revision,
rendition and passage the reference names, and every negative claim of the same
proposition there. Each is resolved through the L4 resolver under the caller's view in
stream order, and when the caller's access hides triples, every triple of the target
and of each such record must also be in its controlled masked view of the head. Any
refusal answers as the `evidence-unavailable` answer of the decision's target, byte for
byte. Candidates are resolved before a bound is reported, so more than 10,000 readable
records answer `incomplete-evidence` and a hidden one never counts toward it. `ifHead` is required. The review reads the head only when it is
`ifHead`, and the branch writer refuses the commit as `head-changed` unless its base is
still that commit, so the reads and the commit see one state. The commit is made as a
proposal's is, with the authority fence. A review records no new stored-fact reference
and takes no source hold.

`sparkles::evidence::withdraw` and `POST /{ds}/claims/withdraw` record an
`ev:Withdrawal` that `ev:withdraws` each named claim, with its digest, the caller and
`prov:invalidatedAtTime`, the commit's time, in the claim's own graph. A withdrawal makes
no contrary claim. Each projected triple left without a live support leaves its output
graph. An already withdrawn claim is answered as `existing`, and the operation's
description is still written to the claim's graph so that its key can be replayed. A
withdrawal needs update grants on each claim's graph and on the output graph, and read
grants on the decisions, supports and output graphs, of every review policy, because a
support admitted under an earlier version ends with its claim whatever the boundary is
now. Liveness looks for withdrawals in every proposal graph for the same reason. Its `ifHead` is optional, and the head it read must still be the
writer's base. Both operations answer `changed`, the supports added and ended and the
triples inserted into and removed from output graphs, so that F22 can later find the
dependent results. The vocabulary and shapes add `ev:supersedes`,
`ev:AcceptanceSupport`, `ev:claim`, `ev:decision`, `ev:output`, `ev:supportedTriple`,
`ev:conflictsWith`, `ev:Withdrawal`, `ev:Projection`, `ev:conflictMode`, `ev:Refuse`,
`ev:AdmitLabelled` and the three protection properties. These are RDF records, so no
file format or reader version changes.

Tests state their expected accepted sets independently and cover duplicate occurrences
that each own a support, reversal and withdrawal of one of them, a negative claim that
is recorded and conflicts until it is rejected in either order of one request, a
labelled policy, every stale precondition writing nothing, replays and keyless retries
that add no support, hidden evidence compared with an unreadable target, direct SPARQL
edits of the decisions, supports and output graphs, a changed protection, missing
grants, kind and scope admission, dry runs, a review on a branch, a repeated withdrawal,
conformance of the written records to the shapes, and the HTTP routes.

#### Review decisions

1. **Derived liveness.** Supports and decisions are never edited. A support's end is
   derived from a superseding decision or a withdrawal, which each carry their own
   receipt, so no unauthenticated triple can end or revive a support.
2. **Conflicts.** The profile has no domain incompatibility rules, so only explicit
   contradiction links and negative occurrences of the same proposition count. Domain
   conflicts, such as two values of a functional property, remain application policy.
3. **Policy versions.** Supports admitted under an earlier policy version stay live. A
   version change records a new projection record and applies to later decisions only.
4. **Withdrawal rights.** Any caller with the grants may withdraw a claim, as for every
   other typed operation. A withdrawal is final, and a new occurrence must be proposed.
5. **Reconciliation.** A projection whose protections changed is refused rather than
   repaired. Roadmap item 581 tracks an administered reconciliation.
6. **Identity decisions.** Identity proposals need their own record input, a matching
   policy and resolution-map semantics, so they are a later step of this phase. The
   current review policy is specific to claims.
7. **Bindings and MCP.** The routes are HTTP, OpenAPI and Rust only. MCP waits for Phase 3
   and the bindings for roadmap item 509.
8. **Output visibility.** A review checks the reviewer's access, and the output graph is
   read under each reader's grants. The administrator must give the output's readers
   the policy's boundary. Roadmap item 582 tracks support-aware visibility of
   projections with F18.
9. **Bounds.** One review reads at most 10,000 evidence records, and one triple's
   supports, one target's decisions and one decision's successors are each read up to
   10,000 records, beyond which the operation answers `incomplete-evidence`.

Identity proposals with reviewed, reversible resolution maps are the seventh and last
Phase 2 step. A claims batch takes `identities`, each naming 2 to 64 distinct entity
IRIs, the relation `ev:PossibleMatch` or `ev:DistinctEntities`, an optional
`ev:preferredEntity` of a match among its entities, the attributed proposer and an
optional matching method. They are minted from the set of entities and the occurrence
key, so the order of the entities changes neither the identity nor the digest, and go
to the settings' identity graph or the one `identityGraph` names. They carry the same
receipts, nonces, digests, idempotency and authority fence as every other record.
Evidence links and assessments may target them.

`evidence.json` format 1 and profile 1 gain `matchingPolicies`. Each has a `version`, a
`boundary` of identity, source-record and proposal graphs that holds at least one
identity graph, a `decisions` graph, a `maps` graph of the identity role and an
`output` graph of the projections role. Names and the bound of 64 are shared with
review policies, so `urn:x-sparkles:evidence:policy:{name}` names one policy, and a
projection graph belongs to exactly one policy of either kind. A build that predates
the member refuses it through its unknown-member check, so such settings read as
`managed-unavailable` there.

`sparkles::evidence::review` and `POST /{ds}/claims/review` dispatch on the policy's
name, and under a matching policy the targets are identity proposals in an identity
graph of the boundary. Decision records, supersession, the stale-target, policy-version
and head preconditions, replay, dry runs and the commit are those of a claim review.
The policy's effective matches are its authenticated `PossibleMatch` proposals whose
current decision under the policy accepts them, and they join entities into clusters,
the connected components of the entities they name. The output graph holds the current
map, `<member> ev:resolvesTo <representative>` for every member of every cluster of
two or more entities, the representative included. The representative is the smallest
IRI among the preferred entities of the cluster's effective matches, or the smallest
member IRI when none names one, so the map does not depend on the order of decisions.
An effective `DistinctEntities` proposal is a constraint, and an operation whose final
decisions put two of its entities in one cluster answers `409 identity-conflict` and
writes nothing, in either order.

An operation reads the clusters of its targets' entities before and after its
decisions, and rewrites exactly those entities' output triples. When it adds an
acceptance or supersedes one, it writes an immutable `ev:ResolutionMap` record in the
maps graph, minted in the policy's namespace from its number. The record names the
policy, its version, `ev:mapRevision`, `prov:wasRevisionOf` the revision before it,
the output graph, `ev:adds` for each acceptance that became effective, `ev:ends` for
each accepted decision it superseded, the protection nonces of the decisions, maps
and output graphs, and an `ev:boundaryProtection` literal of nonce and IRI for each
protected identity graph of the boundary. The latest revision is found by galloping and
binary search over the minted identities. A revision that does not authenticate, a
latest revision under other nonces than the current ones, a latest revision built from
an identity graph that has left the boundary or carries another nonce, and an output
that holds triples while no revision exists answer `projection-unavailable`. The
clusters before an operation are recomputed from the boundary, so a map whose matches
are no longer read would otherwise keep stale triples for entities the operation does
not reach. A graph added to the boundary is accepted, since no proposal in it was
accepted under the policy while the map was built. Rebuilding a map after a boundary
change is roadmap item 643. A split is a superseding rejection, and accepting the
match again restores the earlier map. Proposals, decisions and earlier revisions are
never edited, and the output graph's history serves the map of any retained commit.
`Changed::map` and the answer's `changed.map` name the policy, the new revision and its
predecessor, the decisions added and ended, and the entities whose selected entity
changed, and `mapRevision` is answered on a replay as well. These are the changed IDs
that F22 needs to find the consumers of the earlier revision.

A review under a matching policy needs update and read grants on its decisions, maps
and output graphs, read grants on every identity graph of its boundary and read access
to each target. An acceptance also needs read access to the target's evidence links in
the boundary, as for claims, and every review that changes the map needs read access to
each effective proposal of the clusters it reads in either state. One whose triples the
caller's view hides answers as the decision's target being unavailable, byte for byte.
Candidates are resolved in order before a bound is reported, and a hidden proposal that
no decision accepts is left out without being counted. More than 10,000 proposals naming
one entity answer `incomplete-evidence` when the caller's view shows that many, and
otherwise answer as the target being unavailable, so a refusal never reports a count
that includes a record the caller cannot read. One operation visits at most 10,000
entities and proposals in each state and retains at most 64 MiB of the identifiers of
its targets and clusters, beyond which it answers `incomplete-evidence`. A resolution
read checks the latest revision at the unmasked head and refuses a caller whose view
hides part of it, so a mask cannot hide the boundary that would refuse the map. It
traverses each component the caller's view reaches once and answers each requested
entity with the decisions of its own component, whatever else the request names.
Authentication reads only a record's receipt and protection, so the read takes its
candidates from the caller's view and leaves out a proposal, or a proposal whose current
decision is a record, that the view hides any triple of. Such a proposal neither joins a
cluster nor reveals its decision, and the output graph is read in the caller's view.

`sparkles::evidence::resolve_entities` and `POST /{ds}/claims/identities` read the map
at the head of a branch. They need read grants on the policy's decisions, maps and
output graphs and on the identity graphs of its boundary, and the route needs `read`
through the query endpoint. Each of 1 to 100 entities answers its representative, up to
1,000 sorted members with a completeness flag, and the current accepted decisions that
join its cluster. An entity the map does not name resolves to itself. Queries join the
output graph explicitly and are never rewritten through a map, so the query path is
unchanged. The vocabulary and shapes add `ev:preferredEntity`, `ev:ResolutionMap`,
`ev:mapRevision`, `ev:adds`, `ev:ends`, `ev:resolvesTo`, `ev:mapsProtection` and
`ev:boundaryProtection`, and `GET /$/server` lists the route as `capabilities.claims.identities`.

Tests state the expected maps independently and cover merges, transitive merges, a
split and the exact restoration of the earlier map, the revision chain, representatives
under preferred entities and request order, decisions that change no acceptance and
write no revision, separations in both orders and within one request, a split that
accepts a separation in the same request, replays, keyless retries, stale heads,
digests and policy versions, dry runs, an authority fence that refuses at publication
and a cancelled request writing nothing, missing grants, a hidden proposal of a
cluster compared with an unreadable target, direct SPARQL edits of the identity, maps,
output and decisions graphs, changed protections of the maps and output graphs, earlier
policy versions, the resolution read and its refusals, a review on a branch, the
written records against the shapes, settings goldens and validation, and the HTTP
routes.

#### Identity decisions

1. **Proposals in the claims batch.** Identity proposals reuse the proposal writer
   instead of adding a parallel path, so their receipts and holds are the same.
2. **One review route.** A matching policy is reviewed through `claims/review`, and the
   policy's kind decides which records it reviews. A claim review of an identity
   proposal answers as for any record of another kind.
3. **Deterministic representatives.** The smallest preferred entity, or the smallest
   member, keeps a cluster's selected entity stable as members join, without a minted
   identity that would change with membership.
4. **Deltas, not snapshots.** A revision records the decisions it adds and ends rather
   than the whole map, so a review writes in proportion to its change. The map at a
   revision is the fold of its chain, and the output graph's history serves it at any
   retained commit.
5. **Separations refuse.** A separation that meets a cluster refuses rather than
   splitting it, so a split is always an explicit reversal of the match that joined the
   entities.
6. **No withdrawal of proposals.** Identity proposals are reversed by decisions.
   `claims/withdraw` stays specific to claims.
7. **Bindings and MCP.** The routes are HTTP, OpenAPI and Rust only. MCP waits for Phase
   3 and the bindings for roadmap item 509.

Phase 2 is complete. The shared managed-write and admission boundary described above is
the one C26 reuses for its control graphs.

Phase 3 begins with the shared-record hook that F22 needs to find dependent analyses
(A9 and A13). An assessment's `inputs` names up to 64 records it consumed, batch-local
or existing: claims, assessments, identity proposals, source revisions, review decisions,
acceptance supports and resolution map revisions. They are written as `prov:used` and
verified as authenticated records of those kinds before anything is published. The
field is framed in the payload and record digests only when it is non-empty, so every
earlier assessment keeps its digests, and inputs and targets of one batch's assessments
may not form a cycle. The assessment shape allows up to 64 `prov:used` IRIs.

`sparkles::evidence::find_dependents` and `POST /{ds}/claims/dependents` take 1 to 1,000
changed record identities and answer the authenticated assessments in the proposal
graphs the caller can read whose `ev:target` or `prov:used` names one of them, each once
at its smallest depth with how it depends on its record. With `transitive` the
assessments that depend on a dependent are found too. A graph the caller cannot read is
not searched. Authentication reads only a record's receipt and protection, so an
assessment any of whose triples the caller's view hides is neither reported nor
followed, and nothing hidden is reported. More than 10,000 target and input triples in
the caller's view answer `incomplete-evidence`. `Changed::identities` gathers the changed identities of an
operation's `changed`: the ended supports, and for a new map revision the revision it
follows and the decisions whose effect ended. Withdrawn claims and superseded decisions
are in the operation's own result. The answer marks nothing stale and decides nothing
about the assessments. F22's maintained analyses, roadmap item 340, consume it.

#### Phase 3 decisions

1. **Inputs as `prov:used`.** The standard PROV property records what an evaluation
   consumed, so dependents need no new vocabulary and ordinary SPARQL can follow them.
2. **Optional framing.** Framing the inputs only when present keeps every earlier
   assessment's digests, which avoids a profile version for an additive field.
3. **Find, not mark.** Dependents are an answer computed from authenticated records, not
   stored staleness, so a reversal that restores an earlier state needs no cleanup.
   Whether a dependent is stale belongs to the F22 analysis that consumes it.
4. **Caller-scoped search.** Only readable proposal graphs are searched, and a masked
   assessment is left out, which follows the claims rule that a hidden record answers
   as a missing one.

The rest of Phase 3 is planned as separate roadmap items. Recall and Ask citations of
reviewed claims under their review state, identity resolution and access are
650. Ingestion of extracted facts as claim proposals with their sources is
651. Reports and community summaries that draw on accepted evidence are
652. The bindings remain item 509.

### Historical configuration for evidence views

Item 761 is the storage and restore prerequisite of item 650. A selected evidence
view must pair its retained RDF revision with a complete authenticated managed
configuration, including owner settings and graph protection nonces. The current
policy version, a projection or a caller-provided digest cannot establish that
historical configuration. This work remains planned and does not change the
available evidence operations described above.

Head selects the configuration actually installed when its RDF snapshot is
captured. An explicit commit selects the configuration that admitted that data
publication. These can differ at the same numeric sequence when an administrator
changes settings without writing RDF. A time selector uses its resolved RDF
commit's admitted configuration. An opaque captured-view token or a durable bound
named snapshot can replay an exact Head pair. An ordinary same-sequence named pin
retains its existing binding rather than acquiring today's configuration.

Immutable complete configuration generations are reachable from the managed
registry's durable publication root. Each ordinary, grouped and bulk data
publication retains its own admission generation through validation and final
publication. A configuration installed while an older admitted writer finishes
does not relabel that writer's commit. Configuration history initially retains
every published generation, with bounded encoding, decoding and predecessor
walks. Retained configuration cannot restore missing RDF or erased sources.

Persistent activation uses the closed-dataset migration and recovery path. It
records the actual branch heads and the first future own sequence that requires a
binding. It does not certify earlier commits or legacy pins from current settings.
In-memory activation has its own exclusive boundary and makes no disk durability
claim. Genuinely unmarked stores preserve their existing RDF behavior, while an
anchored history or a covered commit cannot fall back to legacy absence when a
reference, flag, generation or coverage file is missing. Complete accepted seals
are authenticated before recovery can truncate a log or expose recovered data.

The newer file versions are written only after activation, so a store that never
activates stays readable by older releases. Such a store writes version 1 logs,
generation `commit.json` format 1 and `managed.json` format 2, and its dataset
records reader 5, or the managed-graph reader when it has managed graphs.
Activation first raises the dataset reader of every branch to 8, the
configuration-history reader. It then upgrades each current log header in place
and publishes the registry root. From then on the family writes version 2 logs,
`commit.json` format 2, `managed.json` format 3, `history.json` format 2 and
`branch.json` format 2. Branches and captures of an activated family record reader 8
before they write any of these. An older release refuses an activated dataset by its
reader before it reads anything else. A dataset's recorded reader also bounds the
versions this build accepts in it. Open and reidentification check every generation's
log and `commit.json` and the dataset's settings and registry against it, before they
read or change them. One of these versions in a dataset that records an older reader
is refused as a newer release's file. Bounded history readers do not repeat the check,
so it adds no read to their allowance.

A family root records activation when its dataset reader is at least 8 or when it
holds an `evidence-history` directory. Activation runs on a migration copy that
replaces the root only once it is complete, so a live root is never half activated.
An activated root whose `managed.json` is missing, or names no history root, refuses
to open with `evidence-unavailable` and is not read as a legacy store. The refusal
writes nothing.

The prepared native capture is private and has no public caller. Before anything
calls it, it must charge its copied metadata to the caller's preparation before
allocating it, or stream that metadata from the retained descriptors. This covers the
cloned prefix map, its serialized form and whole-file reads of optional metadata such
as `annotations.bin`. As written, a store with a large annotation history can allocate
more than a small receiving allowance admits.

Backups retain one coherent family configuration around their data capture,
including a branch capture racing main configuration. Their immutable source
descriptors and complete published closure remain valid after later settings
changes. The required backup feature identifies versioned configuration-history
files through a closed path grammar. Missing, corrupt or unsupported dependencies
refuse before destination publication, including restores without an optional
integrity check and encrypted restores.

Keeping a dataset identity preserves native bindings. A new identity preserves
old generation bytes and original small identity and base documents as source
provenance. Its versioned origin descriptor identifies the source family and
logical branch and authenticates disjoint exact imported WAL intervals. Only those
verified intervals permit imported RDF replay. Foreign references outside them
refuse. Later own publications use a genuine destination generation, while old
explicit revisions remain unavailable as native historical evidence of the new
identity. Repeated identity changes retain earlier imported origins and add only
new imported suffixes, without duplicating large WAL files. Rebuild clones have no
old WAL history and establish their own destination root explicitly.

Historical configuration supplies semantics, never current access. Source
retention, current graph protections, caller grants and provider admission remain
independent restrictions through Recall, Ask, queued responses and publication.
Read, query and output owners retain the actual selected view through their last
physical consumer. A borrowed cloneable snapshot or a separate metadata token
does not establish that lifetime, and newly copied output needs its receiving
budget. Item 650 additionally owns the named policy and matching-map consumer
integration, reviewed claim-occurrence citations and A10 treatment of source
instructions as untrusted data.

### Evidence operations over MCP

Roadmap item 653 exposes `propose_claims`, `review_claims`, `withdraw_claims`,
`resolve_evidence`, `resolve_identities` and `find_evidence_dependents`. They use the
same typed payload parsers, JSON results and evidence operations as the HTTP routes.
Proposal batches include identity proposals and assessment inputs, and reviews under
matching policies retain map revisions and reversible decisions.

Each tool runs as its caller on the selected dataset and branch. Its endpoint and
graph grants, hidden-record refusals, protected writer, revision checks and credential
fence are those of HTTP. The shared execution context carries the selected branch,
absolute MCP deadline, cancellation and admitted grant into the blocking operation.
HTTP captures its branch before that boundary too. The evidence resolver retains its
shared preparation allowance. Write tools follow MCP's update switch and the server's
read-only setting, while tool listing and disabling use the existing MCP controls.

The three evidence writes advertise `idempotentHint: false`. Their explicit
`idempotencyKey` provides receipt replay. Keyless proposals and withdrawals retain
HTTP's behavior of publishing new operation metadata even when their records are
already present. Reviews require the head they read, so an exact successful keyless
review retry refuses the moved head. Error codes use owned boxed strings to preserve
arbitrary HTTP refusal codes while keeping the shared tool error below Clippy's
128-byte large-error boundary. This adds a small code allocation on static-code error
paths in exchange for a smaller inline error value.

The advertised input and output schemas reuse the evidence OpenAPI definitions, with
MCP's dataset, branch and timeout arguments. A refused operation is an MCP tool result
with `isError: true` and its HTTP code, status and body in error metadata. JSON-RPC
success alone does not report successful publication. The HTTP authority-change
response retains its authentication challenge. The adapter never adds evidence
settings, administrative holds, job execution or domain entity semantics.

Router tests exercise all six tools through actual JSON-RPC dispatch, HTTP parity,
per-caller and graph grants, hidden records and stored facts, protected writes,
previews, replay, head, target and policy refusals, withdrawal, matching-policy reversal,
assessment dependents, Unicode passages, selected branches and credential revocation
before publication. Tool advertisement tests cover payload schemas and update,
read-only and disable controls.

Retained-reference resolution uses C29's bounded history and controlled permission
preparation. Guarded Dataset queries and U02 readers still adopt them under item 395.

### Scope changes

**Probabilistic reasoning through external engines.** The scope statement in §1 excluded probabilistic reasoning. Probabilistic reasoning through external engines, as integration only, is now planned under [ROADMAP](../ROADMAP.md) item 463 and [F28](F28-ml-and-probabilistic-integration/README.md). Sparkles still defines no probabilistic reasoner of its own.

