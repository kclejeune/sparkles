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
automatic identity resolution. [F22](F22-analysis-lifecycle.md) owns support-set logic,
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
Phase 2 runtime remains incomplete.

The runtime ownership prerequisite gives each opening one shared family identity.
Persistent families retain a lease on the root process lock. Persistent and in-memory
members inherit their family's identity.
Writer permits span acquisition through grouped publication or refusal, including
when a submitting caller unwinds. Branch creation, deletion, renaming and table
settings enter the same gate before their branch locks. Store resources and branch
locks are destroyed before the final root lease. A retained persistent member or
branch set can therefore outlive main without releasing process exclusion early.

The resident source ownership prerequisite supplies core SourceOwner and ArcSourceOwner
and the facade ResidentSourceOwner. A facade lease retains the catalog's original Dataset,
including its Own(Store) payload, and an exact already resident branch Arc. It opens no
cold branch and constructs no branch Dataset or Runtime. Getters borrow the retained
values once before operation exclusion. The exact opening and private family identity
are checked separately from stored UUIDs. This is a lifetime bridge, not authorization,
routing authenticity, protected publication or durable retention.

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
serialized, so callbacks must defer management operations until disposal returns. Members
closed during unwinding retain the existing behavior that skips clean-close checkpoints.
Family admission and the real root process lease still span required resource shutdown,
including a captured callback panic. Allowing catalog mutation from close callbacks remains
an item 81 lifecycle gap.
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

These checks establish runtime closure only. Durable source holds and consumer release,
operational observation, privacy visibility, typed writes and C25/C26 consumer
composition remain separate dependencies.

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

Roadmap item 81 retains the rest of this contract. Phase 2 adds typed source, reference
and claim creation with authenticated availability and guarded metadata, exact retained
reference resolution, reader admission, idempotency, review, support ownership,
projections and reversible identity decisions. The shared managed-write and admission
boundary described above is the one C26 reuses for its control graphs. Phase 3 integrates the new records with C17/C18, F22 and C22.
The state-machine, authorization, direct-write, crash and retry acceptance cases remain
required before those runtime phases are complete.

Exact retained-reference resolution additionally uses item 395 and
[C29](C29-bounded-view-preparation/README.md) for bounded history and permission
preparation. A point-only reader does not complete the general guarded-view contract.
