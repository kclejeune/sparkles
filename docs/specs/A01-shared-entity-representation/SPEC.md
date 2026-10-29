# A01: Shared entity representation, convergence and evolution

> **Status:** implemented in part. Versioned resources, acceptance fixtures and bounded
> private offline replay are available.
>
> **Phases:** Phase 1 supplies versioned resources and independent acceptance
> fixtures. Phase 2 provides bounded offline reference replay and selected fixture mappings.
> Phase 3 adopts the shared managed runtimes after their required acceptance gates.

## 1. Purpose and scope

Several sources can describe one organization, package or event. They can disagree,
repeat a common source or change their description. A useful shared representation
preserves those descriptions while allowing a reviewed, reproducible current view.

This application profile defines a small common entity model, typed relationships,
identifier scopes, resolution inputs and evolution policies. Software, organization,
place, event and aggregate-observation examples prove that the common contract works
across domains. These examples are a bounded reference corpus, not comprehensive
ontologies, automatic entity matching or operational domain products.

The profile uses ordinary RDF, SHACL and SPARQL. It reuses [C25](../C25-evidence-and-assessments.md)
source revisions, occurrences, evidence links, review decisions and scoped versioned
resolution maps, and [C27](../C27-valid-time-profiles.md) time conventions. It does not
create another review ledger, identity service, source store or transaction history.
[F22](../F22-analysis-lifecycle.md) owns maintained assessment freshness and publication.
[C26](../C26-durable-jobs-and-delivery.md) owns durable jobs and fenced publication.

Convergence here means deterministic replay from the same exact records, decisions,
policy and selection coordinates. It does not mean a CRDT, cross-replica agreement,
last-writer-wins truth, or automatic `owl:sameAs`. F16 equality representatives, a new
RDF datatype and an ontology object API are not prerequisites. Resource authoring and
offline replay do not wait for unfinished managed runtimes.

Full feed connectors, comprehensive corporate ownership, hardware applicability,
clinical records, operational public-health workflows, forecasts and new UI screens
are outside this scope. Aggregate health examples make no clinical recommendation.
A later operational health package requires separate domain and expert validation.

## 2. Profile versions and record ownership

The namespace is `sr: <urn:x-sparkles:representation:>`. Version 1 is identified by
`urn:x-sparkles:representation:profile:1`. A versioned manifest fixes the vocabulary,
shapes, queries, mapping definitions, policies and their SHA-256 digests. Unknown
profile versions are refused without selecting a newer or older version.

C25 remains the owner of source, claim, evidence, review and authenticated identity
records. A01 adds domain descriptions and read-only resolution/projection artifacts.
Its offline examples are ordinary caller-controlled RDF. They cannot authenticate an
author, establish first availability or impersonate a C25 admitted record. Phase 3
uses C25 operations and authorization rather than application writes to protected graphs.

| Record | Required meaning |
|---|---|
| `sr:Entity` | A persistent domain identity anchor. Its IRI never changes meaning or becomes a different entity after a resolution decision. |
| `sr:Description` | One source-specific description, linked by `sr:sourceRevision` to one C25 revision and by `sr:selection` to an exact evidence reference. Its `sr:describes` entity is optional until resolution. |
| `sr:Identifier` | One identifier value under an authority, realm and scope. It identifies a designation, not a proof that its bearers are one entity. |
| `sr:Identification` | A qualified binding of an identifier to an entity, attributed through one or more C25 claim occurrences. |
| `sr:Relation` | A qualified directed relationship with subject, kind, object, domain scope, temporal record and claim occurrences. |
| `sr:ResolutionView` | One immutable interpretation of exact C25 decision inputs under a pinned application policy and scope. It specializes the application resolution map described by C25. |
| `sr:Assignment` | An entity anchor's representative in one resolution view, or an explicit unresolved result. It is not a permanent rewrite of the entity. |
| `sr:Projection` | A reproducible domain read view using exact records, resolution view, policies and temporal selection. Its current pointer is separate from its immutable content. |
| `sr:Migration` | A recorded ontology or mapping transformation over exact source revisions. It is distinct from a source correction. |

Each added record has an IRI. References to C25 records preserve their existing IDs.
Offline derived artifacts also receive explicitly supplied IRIs under a declared
fixture/application namespace; replay checks their immutability rather than minting
identity from serialization. No blank-node label, display label, mutable URL or
serialized triple string is an external durable identity. C25's retained reference and explicit skolem rules apply.

## 3. Identity, descriptions and identifiers

Entity IRIs are opaque stable anchors allocated by the supplying application or its
upstream authority. A profile fixture uses fixed `urn:example:a01:` IRIs. A production
adapter declares its minting policy and cannot present fixture IRIs as global identity.
A name change creates a new attributed description, not a new entity by default.

A description identity includes source revision identity and a stable source record
key or exact selector. Equal text in two revisions does not collapse descriptions or
C25 claim occurrences. A description can mention several entities through separate
qualified identification claims. Descriptions of the same entity can remain
contradictory without making one record invalid.

An identifier has exactly one `sr:authority`, `sr:realm`, `sr:identifierScope` and
`sr:lexicalValue`. The first three are IRIs. The value is an `xsd:string`, preserving
case, Unicode and leading zeroes. Absent scope is invalid. An explicit declared
unscoped realm is permitted only when the authority policy defines its semantics.
The logical designation key is the ordered tuple of those four RDF values. No default
case folding, Unicode normalization or URL rewriting is allowed. A versioned adapter
may supply a separate normalized designation while retaining the original.

An identification has one `sr:identifier`, one `sr:entity`, at least one `sr:claim`,
one `sr:domainScope` and an optional C27 validity record. Identifier reassignment and
changing identifier rules are represented by separate scoped claims and policy
versions. Equal designation keys are matching evidence under their authority policy,
not an unconditional entity merge. Different keys do not prove distinct entities.
Aliases are identification claims or source labels, never free global equality.

Every entity has exactly one `sr:entityKind`. Version 1 uses `sr:Person`, `sr:Account`,
`sr:Organization`, `sr:LegalEntity`, `sr:Place`, `sr:EventEpisode`, `sr:PackageFamily`,
`sr:PackageVersion`, `sr:SoftwareRelease`, `sr:Build`, `sr:Artifact`, `sr:Deployment`,
`sr:DeviceModel`, `sr:DeviceInstance`, `sr:Disease` and `sr:CountObservation`. These are
application kinds rather than an exhaustive upper ontology. A legal entity can relate
to an organization through `sr:RealizesOrganization`; it is not merged into it.
A human controlling an account is represented by `sr:ControlsAccount`, not identity.

A version 1 resolution policy must treat different entity kinds as incompatible for
co-resolution. A future profile can adopt a reviewed compatibility rule with a new
version. A broad RDF superclass does not silently loosen this constraint.

## 4. Qualified relations and domain boundaries

A relation has exactly one `sr:subject`, `sr:relationKind`, `sr:object` and
`sr:domainScope`, and at least one `sr:claim` naming immutable C25 occurrences. The
subject and object are entity IRIs. Relations use C27 `vt:validDuring` for applicability
or `vt:occurredDuring` for reported occurrence, preserving their different meanings.
Missing time remains unknown. A relation with participant role qualifiers links through `sr:roleAssignment` to
`sr:RoleAssignment` records, each with one `sr:participant` and one `sr:role` IRI.
There is no generic affiliation, related-to or risk edge in the initial profile.

| Relation kind | Subject and object kinds | Meaning |
|---|---|---|
| `sr:ControlsAccount` | Person and Account | A source claims control within the given scope and period. |
| `sr:RealizesOrganization` | LegalEntity and Organization | A legal entity realizes the named organizational identity within scope. |
| `sr:AccountingParentOf` | LegalEntity and LegalEntity | A source claims the declared accounting parent relationship. It does not imply beneficial ownership or control. |
| `sr:LocatedAt` | Organization, LegalEntity, Deployment or DeviceInstance and Place | A scoped location claim with its own role and dates. It does not imply citizenship, incorporation or sponsorship. |
| `sr:VersionOf` | PackageVersion and PackageFamily | A version belongs to a package family under a pinned ecosystem identity policy. |
| `sr:ReleaseContains` | SoftwareRelease and PackageVersion | A release description includes a version. It does not establish a build or deployed artifact. |
| `sr:BuildProduces` | Build and Artifact | The attributed build produced the artifact identified by a declared digest policy. |
| `sr:DeploymentUses` | Deployment and Artifact | An attributed observation describes deployment use, with environment and dates. |
| `sr:InstanceOfModel` | DeviceInstance and DeviceModel | A particular device is associated with a model. A model advisory alone cannot establish device applicability. |
| `sr:ReportAboutEpisode` | A source Description and EventEpisode | A reviewed grouping associates a report with a local episode. This row is the sole exception to the entity-only subject rule. |
| `sr:ObservationAboutEpisode` | CountObservation and EventEpisode | An observation belongs to a reviewed episode grouping. It does not identify the observation with the episode. |

The `ReportAboutEpisode` exception requires the subject to have `sr:Description` type
and an exact C25 source revision and selector. All other subject constraints remain
closed to description records. Relation-kind shapes implement the table rather than
inferring relationship meanings from an arbitrary predicate name.

An artifact digest has algorithm and lexical digest fields. A supplied digest is an
identity assertion until its bytes are verified by the declared verification method.
Build parameters, features, platform and dependency occurrence are separate scoped
records. Cargo lock membership cannot establish selected-build inclusion, deployment,
reachability or exploitability. A source advisory and a vulnerability record retain
distinct IDs even when an advisory names a vulnerability.

Place descriptions retain administrative level, a geometry resource and its geometry
version, plus spatial uncertainty. A geometry revision does not rename the place by
default. A changing border or jurisdiction must retain its applicable interval and
mapping policy, rather than moving historical observations to the newest geometry.

An aggregate count observation has one `sr:measure`, `sr:caseDefinition`, `sr:countMode`,
`sr:population`, `sr:place`, `sr:geometryVersion`, `sr:reportingAuthority` and
`sr:observedDuring`, whose value is a C27 interval for the observation window.
It records an integer `sr:count` or an explicit `sr:valueStatus`
of `sr:UnknownValue`, and never both. Counts are nonnegative. Count mode is
`sr:Incident` or `sr:Cumulative`. A denominator is a separate qualified record with
value, unit, population and interval, or explicit `sr:UnknownDenominator` status.
Missing denominator is not zero. Case definition, measure, units, geometry and
uncertainty policies are versioned IRIs. Version 1 represents uncertainty through a
retained source expression and an optional nonnegative ordered lower/upper count
interval. It does not assign a universal probability. Suspected and confirmed case
definitions remain different. Cumulative revisions cannot be summed as incidence.

Disease, pathogen, a report, an episode and its observations are distinct resources.
Official event identifiers and local episode grouping remain separate. Two reports
about one episode can carry revised counts. Retrospective episode boundaries can
change through reviewed resolution/grouping inputs. Scenario outputs are assessments,
not observations. The health fixtures validate these representation distinctions;
they do not validate epidemiological inference or operational use.

## 5. Evidence and acceptance

Every description, identification and relation points to exact source-backed C25
occurrences. Equivalent propositions preserve every occurrence identity. Extraction,
source confidence, analyst confidence, review status and truth are different fields
under C25. A mention cannot become supporting evidence through a domain mapping.

`prov:wasDerivedFrom`, mirrors, translations and shared source families expose lineage.
They do not establish independent corroboration. Missing lineage yields unknown
independence. No competency answer treats URL count, matching descriptions or number
of occurrences as a count of independent votes. Independence is a separate evidenced
assessment, using C25 rather than a new A01 confidence formula.

A proposed or rejected relation remains a described proposition. It is excluded from
ordinary accepted business triples until C25 admission. Offline fixture projections
are clearly labeled simulated selections and do not create accepted C25 support
ownership records. Withdrawing one occurrence removes only its support. A surviving
independent admitted occurrence can retain the projection. Withdrawal is not a claim
of the opposite proposition, and absent evidence remains unknown.

## 6. Scoped resolution and deterministic replay

A resolution view records one exact scope IRI, profile/matching-policy versions,
source and claim IDs, C25 identity proposal and decision IDs, the decision selection
coordinate, and a content digest of its input manifest. It also records the ontology,
mapping and temporal-normalization versions. Only entities in that scope participate.
An assignment in another scope is not an implicit input.

The input contract provides a complete selected set of admitted C25 decisions and
explicit unavailable/excluded inputs under the pinned policy. In Phase 2 these are
fixture declarations. In Phase 3 C25 resolves their authenticity, review authority,
current admission and supersession. The replay code does not independently authorize
reviewers or manufacture an admission decision. Missing mandatory inputs refuse a
complete resolution view. A partial diagnostic can retain unknown rows but cannot
publish a complete or current view.

Version 1 interprets an admitted `ev:PossibleMatch` proposal as a resolve-together
constraint only when the selected matching policy explicitly admits that decision as
a match. An undecided proposal remains possible and contributes no equality edge.
An accepted `ev:DistinctEntities` proposal means pairwise separation of every distinct
unordered pair of its members under A01 version 1. This is an A01 matching-policy rule
over C25 records, not a change to the C25 record contract. Each logical separation
retains the original proposal and accepted decision IDs. A rejected or needs-evidence
terminal outcome alone establishes no match or separation. Rejection of a possible
match does not imply a keep-separate decision. Decisions and constraints retain their
contributing proposal IDs and do not assert `owl:sameAs`.

A separation proposal with conflicting accepted and rejected terminal outcomes creates
a decision conflict in each prospective component containing any of its members. It
does not connect those components. This also applies to any other disagreement between
terminal dispositions. Thus a contradicted separation over three otherwise singleton
entities makes each singleton unresolved, with three separate component diagnostics.
The same accepted separation without contradictory terminal outcomes leaves all three
self-assigned when no positive match connects them.

Replay applies the following rules to a set, not to arrival order.

1. Validate all immutable IDs and records, scope, entity kinds, references and selected
   policy versions. Repeated identical records are idempotent. An equal ID with unequal
   content is `immutable-record-conflict`; neither value silently wins.
2. Apply explicit C25 decision supersession within the selected input set. A superseded
   decision remains historical. A supersession cycle is invalid. Concurrent terminal
   decisions on the same proposal that disagree produce a conflict, regardless of
   timestamp or ID order. Equal terminal outcomes retain all supporting decisions.
3. Build the undirected prospective graph of resolve-together constraints. A positive
   terminal outcome contributes an edge even when a conflicting terminal outcome also
   exists; the conflict check below prevents admission of that component. A proposal that
   names more than two entities contributes one jointly constrained component. It may
   use a star anchored at its least member to avoid materializing all positive pairs.
   Preserve each original proposal's membership and contributing terminal decision IDs
   in the diagnostic graph. N-ary separation remains one compact membership constraint;
   the diagnostic graph need not materialize its quadratic logical pair expansion.
4. For each prospective connected component, check kind compatibility, conflicting
   terminal decisions and every admitted keep-separate constraint. If a keep-separate
   pair is connected, or an entity has a conflicting terminal decision affecting that
   component, mark the whole prospective component unresolved. Do not choose a maximal
   consistent subset, drop a nonmatch or select the newest edge. Conflicts outside it
   do not contaminate an unrelated component. For each accepted n-ary separation,
   group its members by prospective component and test whether a group contains at
   least two distinct members. Such a group creates a `distinct-conflict` for that
   component. Use component counts and original proposal membership rather than an
   eagerly allocated pair set. A singleton touched by accepted separation alone has
   no conflict. Conflicting terminal dispositions create `decision-conflict` for every
   component touched by that proposal. Kind incompatibility creates `kind-conflict`.
5. For a consistent component, assign its members the lexicographically least entity
   IRI by UTF-8 byte order as its view representative. A singleton is assigned to itself.
   This tie-break chooses a display/join representative, not a privileged real-world
   identity. Original entity IDs and claim terms are never rewritten. All possible-only
   links and scope exclusions remain separately queryable with explicit status.
6. Emit assignments, unresolved components and evidence coordinates in deterministic
   term order. Equivalent RDF graph content is the semantic comparison, independent
   of Turtle prefix, triple order or whitespace. The replay manifest also defines a
   deterministic JSON comparison form for exact fixture checks.

The full diagnostic graph retains each original proposal, terminal decision, compact
constraint and its affected prospective components. The JSON form normalizes that
graph to exactly one conflict row for each pair of prospective component and conflict
code. Its `entities` contains every entity in that component, sorted uniquely, even
when a particular cause names only some of them. Its `decisions` is the sorted unique
union of all terminal decisions causing that code there and all terminal decisions
contributing positive connectivity in that component. Multiple independent causes of
the same code therefore combine into one row, without losing their individual proposal
provenance from the full graph. An accepted separation's decision contributes only
where its members collide; a contradictory separation's terminal decisions contribute
to every touched component. A decision conflict on singleton A and one on singleton B
produce separate rows with `[A]` and `[B]`, not a fictitious joined `[A,B]` component.

The JSON comparison form is UTF-8 JSON with an object containing `profileVersion`,
`scope`, `inputIds`, `assignments`, `conflicts` and `unknowns` in that field order.
`profileVersion` is the integer `1`. Input IDs are sorted unique strings, including
the manifest, policy and mapping version IRIs as well as entity, claim, proposal and
decision IDs. Assignment objects have `entity`, `status`, `representative` and
`decisions` in that order. Status is `resolved` or `unresolved`; an unresolved
representative is null. Every assignment in a consistent component lists all terminal
decisions contributing its positive connectivity. Every assignment in an unresolved
component lists the union of that component's normalized conflict decision sets.
A self-assigned singleton with no such decisions lists an empty array. A path witness
must not replace either complete decision set.

Each conflict object has `code`, `entities` and `decisions` in that order. Conflict
codes are `distinct-conflict`, `kind-conflict` and `decision-conflict`. Each unknown
object has `record`, `code` in that order. Unknown codes include `possible-match` and
`unavailable-input`. Repeated identical unknown `(record, code)` pairs collapse to
one row. All string sets are sorted uniquely by UTF-8 bytes. Assignments sort by
entity; normalized conflicts sort by their entity tuple then code; unknowns sort by
record then code. Arrays use commas and objects use commas and colons without
whitespace. Only the declared fields appear. The null token is lowercase `null`.

String encoding preserves Unicode scalar values without normalization. Surround each
string with ASCII quotation marks. Encode U+0022 as the two bytes `5c 22` and U+005C
as `5c 5c`. Encode each U+0000 through U+001F as the six ASCII bytes for `\u00xx`,
with two lowercase hexadecimal digits. Do not use short escapes such as `\n` or `\t`.
Encode every other scalar directly as UTF-8, including U+002F, U+007F, U+2028,
U+2029 and non-ASCII characters. Do not escape a solidus or emit a byte-order mark.
An unpaired surrogate or invalid UTF-8 refuses as `invalid-domain-record`. This small
string-only rule needs no external canonicalization library.

These fixed string vectors include the surrounding JSON quotation bytes. The input
column lists Unicode code points and the output column lists the exact UTF-8 bytes
in hexadecimal, with spaces only for readability.

| Input code points | Encoded bytes |
|---|---|
| The ASCII string `https://example.org/A` | `22 68 74 74 70 73 3a 2f 2f 65 78 61 6d 70 6c 65 2e 6f 72 67 2f 41 22` |
| `0022 005C 002F` | `22 5c 22 5c 5c 2f 22` |
| `0000 0008 0009 000A 000C 000D 001F` | `22 5c 75 30 30 30 30 5c 75 30 30 30 38 5c 75 30 30 30 39 5c 75 30 30 30 61 5c 75 30 30 30 63 5c 75 30 30 30 64 5c 75 30 30 31 66 22` |
| `00E9 0065 0301 1F600 2028` | `22 c3 a9 65 cc 81 f0 9f 98 80 e2 80 a8 22` |

Two contradictory proposals on the same A–B prospective component, with terminal
decisions d1/d2 and d3/d4, yield the following single conflict object. Its exact JSON
bytes have no trailing newline. Reversed proposal order or repeated identical records
must yield the same object. Other codes in that component receive their own row.

```json
{"code":"decision-conflict","entities":["https://example.org/A","https://example.org/B"],"decisions":["urn:example:d1","urn:example:d2","urn:example:d3","urn:example:d4"]}
```

A SHA-256 over the complete comparison object bytes identifies the comparison
artifact, not the entity or an authenticated C25 record.

A merge is a successor view in which previously separate anchors share a representative.
A split is a successor view in which they no longer do, following superseding decisions,
withdrawal of admission or a changed policy. A split must remove every path that would
still connect a declared keep-separate pair, or yield an unresolved component. A removed
edge cannot silently override another transitive path. Historic views, IDs, decisions
and methods remain inspectable. F22 consumes changed map/view identities to review or
recompute dependent assessments; A01 does not build a competing invalidation ledger.

The same exact input set and versions must give the same comparison artifact under
permutation, batching and duplicate delivery. Different visibility, missing records,
policy versions or source snapshots are different inputs and need not converge.
There is no claim of convergence across independently selected review policies.

## 7. Evolution and time

| Change | Required representation |
|---|---|
| A real relationship changes in the world. | New attributed relation occurrences describe their own valid intervals. Recorded history retains the earlier relationship. |
| A source corrects or withdraws an earlier report. | New C25 source revision and occurrences link to the earlier revision where correspondence is known. A withdrawal records invalidation without creating a contrary fact. |
| An identity decision changes. | A successor C25 decision produces a successor resolution view. Original source terms and historical assignments remain unchanged. |
| An ontology or mapping changes over unchanged source bytes. | A new mapping/ontology version and `sr:Migration` activity produce new derived descriptions or interpretations with the same raw source revision. This is not represented as a source correction. |
| An assessment changes after new inputs or methods. | F22 creates a successor immutable assessment and explicitly changes the current publication. Neither earlier evidence nor the old assessment is edited. |

A migration records exactly one `sr:fromVersion`, `sr:toVersion`, `sr:method` and
`sr:inputManifest`, and its generated record IDs. Version values are immutable IRIs.
Source digests retain their original byte identity. Unsupported mappings quarantine
the affected records and retain their locators and diagnostics; they do not partially
rewrite admitted records. A metadata-only mapping change still receives a new version.

C27 supplies event/valid intervals, precision and unknown or unbounded boundaries.
C25 supplies source publication and authenticated first local availability. Dataset,
branch and retained commit identity supply recorded time. An earlier event or source
publication date cannot backdate local availability. A date without a timezone cannot
silently become an exact UTC instant. Missing validity is unknown, not all time.

Every projection records the exact retained view, resolution view, ontology/mapping
versions, temporal selector and selected temporal verdicts. `head` resolves once before
reading. An as-known answer requires retained evidence and authenticated availability
at the cutoff. Offline source timestamps can demonstrate a proposed expected exclusion,
but cannot claim historical server knowledge. Unavailable retained state refuses exact
replay rather than substituting current records or a later mapping version.

An immutable projection artifact can be superseded while remaining inspectable. Its
current pointer is policy-dependent and cannot silently advance after an incomplete,
conflicted, unauthorized or stale run. A domain consumer must surface unresolved
identity, temporal unknowns, stale assessment and incomplete coverage separately.

## 8. Validation, extensions and bounds

The profile declares SHACL Core and SHACL-SPARQL constraints over selected graphs.
Shapes check cardinalities, node kinds, enums, qualified relation endpoints, time links,
count fields and identifier scope. Cross-record replay checks resolve IDs, decision
supersession, transitive conflicts and manifest integrity. Neither shapes nor a source
digest establish authenticity, permission or independent corroboration.

Each A01 control record declares `sr:profileVersion`. Its recognized properties form
a closed shape, allowing `rdf:type`, labels and the declared C25/PROV/C27 properties.
An extension is a separate linked `sr:Extension` record with one `sr:extensionProfile`
IRI and a retained payload digest. The manifest lists required extension profiles.
Unknown required extensions, unknown control fields and enum values refuse semantic
replay. An unknown optional extension is retained, marked uninterpreted and excluded
from matching, projections and coverage claims. A domain source's unknown fields are
retained as raw evidence; an adapter must refuse or mark affected semantics unknown
rather than silently treating them as understood. A new interpretation requires a
new mapping/profile version and fixtures.

The offline replay initially supports at most 10,000 entities, 20,000 descriptions,
50,000 claim references, 20,000 selected decisions, 50,000 relations and 64 MiB decoded
input per invocation, with a 256 MiB operation-memory cap. Retained source files are
stream-hashed and individually capped at 16 MiB. These are reference-tool limits, not
engine capacity claims. Counts and bytes are charged before allocation. Check deadlines
and cancellation between records and graph traversal steps. An over-limit operation
writes no result or current pointer. The implementation accepts lower caller caps and
a deadline; it does not silently increase limits or truncate a connected component.

Stable failure codes are `unsupported-profile`, `unsupported-extension`,
`unknown-control-field`, `immutable-record-conflict`, `source-digest-mismatch`,
`unresolved-reference`, `scope-mismatch`, `decision-cycle`, `invalid-domain-record`,
`incomplete-input`, `replay-limit` and `replay-cancelled`. A conflicting identity
component is a successful diagnostic with `identity-conflict` status, not a parsing
failure or an accepted mapping. Unknown evidence produces an explicit diagnostic;
it does not become a negative identity decision. Diagnostics use visible record IDs
and field paths without leaking hidden references or cardinalities.

## 9. Authorization and runtime dependencies

Resource fixtures use synthetic public inputs. Live reads use current graph/triple
authorization even when reading retained snapshots. A projection manifest records its
authorized input boundary and policy fingerprint. Filtering a hidden decision from an
existing complete map does not produce a valid public map: resolution must be evaluated
under a separately declared visible input boundary, with coverage honestly represented.
No representative, conflict, count or error may reveal hidden entities or supports.

| Capability | Required gate |
|---|---|
| Vocabulary, shapes, pinned fixtures and SPARQL competency answers | Existing RDF, SHACL and query support plus the shipped C25/C27 resource contracts. No pending engine phase is required. |
| Bounded offline reference replay | Phase 1's independently fixed oracle and manifest contract. Synthetic decisions remain declarations rather than authenticated review. |
| Live source/claim/review and identity-map integration | C25 Phase 2 authenticated typed operations, managed metadata, retained references, support ownership and protected writes. Item 395 supplies bounded retained-view preparation where needed. |
| Exact as-known temporal answers | C25 authenticated availability and C27 A4 backdating refusal, with exact retained commit origins. |
| Restart-safe recurring acquisition and publication | C26's actual registered operations, fences, checkpoints and atomic publication, followed by later scheduling/delivery phases when required. |
| Maintained assessments and current result freshness | F22 Phase 1 over landed C25/C26 contracts, initially full recomputation with absent-match and policy dependencies. Later automatic invalidation/timers follow their adopted phases. |
| Mixed-access derived inference | F13 per-fact provenance and F18 supported visibility, or separately authorized isolated projections while those remain unavailable. A01 support lists do not replace verified derivations. |

Phase 3 must not substitute an application writer, ledger or unauthenticated timestamp
for a missing shared runtime contract. Failure to meet a dependency keeps that live
integration incomplete. It does not block Phase 1 resources or Phase 2 offline replay.

## 10. Acceptance corpus and competency answers

Every positive fixture has independently authored expected RDF term bindings and every
negative fixture names its exact failure component with a supported sibling. The
reference implementation cannot generate its own expected results. Source bytes and
digests, exact selected passages/fields, versioned policies and record IDs are frozen
before replay implementation. No network or model is required to evaluate the corpus.

| Case | Required fixed answer |
|---|---|
| A1. Two aliases and two same-name organizations | Aliases can identify one entity under a scoped decision. The distinct same-name entities remain separate. A missing authority or realm fails identifier shape validation. |
| A2. Account and person | A control relationship is queryable. A proposed identity merge conflicts on kind and yields unresolved assignments. |
| A3. Binary and n-ary nonmatch | A–B and B–C match with A–C nonmatch makes all three unresolved. Accepted n-ary separation of A/B/C with only A–B matched makes A/B unresolved and C self-assigned. With no positive edges all three self-assign, and a completely unrelated component resolves in every case. |
| A4. Merge followed by split | Both views remain queryable. A split with another surviving transitive path stays conflicted. A valid split separates the anchors without rewriting earlier claims. |
| A5. Withdraw one of two admitted occurrences | The surviving occurrence retains simulated projection support. Withdrawing the final support removes current admission while preserving history. |
| A6. Bulletin, mirror and translation | Three occurrences retain common lineage. Independent corroboration is unknown unless separately assessed, never three votes. |
| A7. Late report about an earlier event | An as-known cutoff excludes the late locally available report despite its earlier event/publication time. The offline fixture labels availability as simulated. |
| A8. Mapping migration over unchanged bytes | The source digest and revision ID remain identical. Mapping version, derived interpretation and projection change explicitly. An old-version query retains the old result. |
| A9. Software family, version, release, build, artifact and deployment | All retain separate IDs and kinds. A lockfile version alone returns unknown deployed exposure. Unverified digest claims remain unverified. |
| A10. Duplicate health reports and cumulative revision | Reports can group to one episode, with distinct observation IDs and 10 then 12 cumulative cases. The current revision is 12 under the declared selection; no query sums them to 22 incident cases. |
| A11. Episode split and geography revision | A successor grouping and geometry version change their projections without altering old reports. Unknown denominator and coarse location remain explicit. |
| A12. Conflicting terminal decisions | Accepted/rejected n-ary separation marks every touched component unresolved without connecting them. Two contradictory proposals on the same A–B component normalize to one decision-conflict row containing all causal decision IDs. Reversed order, identical duplicates, HTTPS IRIs and fixed escaping vectors preserve exact bytes while unrelated components resolve, and a supersession cycle refuses replay. |
| A13. Source ID or digest conflict | Equal IDs with unequal content or tampered source bytes refuse with their exact codes. Quarantine retains diagnostic references without overwriting admitted history. |
| A14. Unknown fields, extensions and limits | Unknown required semantics refuse. Optional uninterpreted extensions stay excluded from semantic answers. Limit-plus-one and cancellation produce no partial output. |
| A15. Access boundary | Hidden supports, entity labels and conflicts do not appear in a restricted result or error. A public projection cannot reuse a private complete resolution view. |

Eight SELECT competency queries are required. They expose descriptions and identity
anchors; scoped identifiers; qualified relations and roles; resolution assignments and
conflicts; occurrence support and source lineage; evolution and policy versions;
software identity boundaries; and episode/count/geography boundaries. Each result
preserves contributing record IDs and explicit unknown/conflict status. A separate
ASK control checks default and named graphs for automatically asserted `owl:sameAs`
and proposed business triples, and returns false. Query results are exact term sets
with declared ordering only where the query specifies it.

Phase 2 adds metamorphic checks for input permutations, duplicate delivery, stable
record IDs, loss of a mandatory input, changes to matching policy and supersession.
Expected semantic outcomes remain independently authored. Phase 3 adds real-auth
admission, correction, withdrawal, restore, retry, stale publication, backdating refusal
and access-revocation tests against the actual shared runtimes.

## 11. Delivery files and validation gates

Phase 1 provides the `../../../crates/sparkles/resources/entity-representation/v1/` directory
with `README.md`, `vocabulary.ttl`, `shapes.ttl`, `manifest.ttl`, `policies/default.ttl`
and eight `queries/*.rq` resources. It adds `../../../testsuite/entity-representation/v1/`
with `README.md`, `manifest.json`, `cases.json`, `raw/`, `valid/`, `invalid/` and
`expected/` directories. The manifest records supported profile versions and digests.

The `../../../crates/sparkles/src/shared_entity.rs` resource accessor follows
existing explicit profile selection without opening or modifying a dataset. It is
exposed from `../../../crates/sparkles/src/lib.rs`. The
`../../../crates/sparkles/tests/shared_entity_profile.rs` integration test validates
manifest digests, unknown-version refusal, shapes, query bindings and nonassertion.
Adding an accessor requires the ordinary additive API policy checks.

Phase 2 provides a private example under the
`../../../crates/sparkles/examples/shared_entity_replay/` directory, with `model.rs`,
`resolution.rs`, `evolution.rs`, `output.rs` and a `mod.rs`, invoked by
`../../../crates/sparkles/examples/shared_entity_replay.rs`. Keep reusable helper code
private until an API requirement is adopted. Integration tests in
`../../../crates/sparkles/tests/shared_entity_replay.rs` consume the independent oracle.
Reuse the existing software-inventory fixture's immutable supplied source bytes and
identity boundaries through an explicit versioned mapping. Do not copy its offline
fixture IDs into C25 admitted identities or expand matcher scope implicitly.

During iteration run `mbx test -p sparkles --test shared_entity_profile` and its
`--features shacl` variant. Phase 2 adds `mbx test -p sparkles --test shared_entity_replay`.
Validate the pinned Boxington version first. Resource-only work needs independent
resource/oracle review, `python3 scripts/check-doc-paths.py --code`, formatting and the
relevant tests. Before reporting an implementation phase complete, run `scripts/ci.sh`
and the repository's independent review and finding-validation sequence. Batch landing
retains conformance and the required performance screen. This specification makes no
performance claim and adopts no specialized index.

Phase 3's concrete adapter files must be planned against the landed C25/C26/C27/F22
APIs before implementation. The implementation plan names real runtime entry points,
crash/authorization regressions and retained-reference verification. A guessed future
managed writer is not an implementable integration contract.

## Provenance

This profile adopts the distinction between describing and asserting a proposition
from [RDF 1.2 Concepts](https://www.w3.org/TR/2026/CR-rdf12-concepts-20260407/), through
[C25](../C25-evidence-and-assessments.md). It specializes entity/activity, derivation,
revision and invalidation concepts from [PROV-O](https://www.w3.org/TR/prov-o/).
[SHACL](https://www.w3.org/TR/shacl/) supplies structural validation and
[SPARQL 1.1](https://www.w3.org/TR/sparql11-query/) supplies the competency queries.
[C27](../C27-valid-time-profiles.md) supplies the temporal profile informed by
[OWL-Time](https://www.w3.org/TR/owl-time/). A01's identifier scopes, kind compatibility,
conflict policy and deterministic representative choice are application rules, not
claims of W3C entailment or automatic identity truth.

C25, C27 and [F22](../F22-analysis-lifecycle.md) remain authoritative for their shared
records, review, time and maintained-result lifecycle. No external implementation,
proprietary ontology, runtime library or source dataset is adopted by this specification.
The reference corpus uses synthetic examples and separately declared supplied fixtures.

## Outcome

Phase 1 provides the version 1 vocabulary, closed structural shapes, matching policy,
digest manifest and eight SPARQL competency queries. The Rust
`shared_entity::profile(1)` accessor selects these static resources and refuses unknown
versions. It opens no dataset. The fixed corpus supplies independent expectations
for A1–A15, with retained source bytes, exact selectors, input-manifest payloads,
scalar encoding vectors and manually
specified query answers. Acceptance checks include 28 conforming graphs and 26 exact
structural refusals, nonassertion and explicit unknown values.

Synthetic successor decisions explain support withdrawal and merge/split histories.
A separate offline observation-series profile and distinct input states explain the
cumulative revision from 10 to 12 without summing the reports. These Phase 1 declarations
do not authenticate review, prove source independence or implement managed operations.

Phase 2 provides the private local comparison CLI and bounded RDF record registry.
Complete deliveries preserve graph coordinates and immutable record identities.
Selected manifests and decision histories drive component resolution, including
n-ary constraints, conflicts with every causal decision, explicit unknowns and retained
older views.
The matcher does not consume authored assignment or conflict rows. Its canonical
comparison encoder preserves the independently fixed bytes.

Retained source and mandatory selector verification reuse C25 digest and lexical JSON
helpers. Record caps are checked during ingestion, decoded bytes accumulate across
deliveries and operation memory, source bytes, cancellation and deadlines remain
bounded. Refusals produce no comparison artifact or current-pointer update.

Private evolution helpers compute the fixed rules for occurrence withdrawal, lineage,
simulated availability, mapping migration and terminal observation selection. The
observation adapter supports reported positive occurrences in its declared synthetic
series. Availability uses canonical fixed-width UTC timestamps. An explicit Cargo.lock
version 4 adapter reuses software-inventory source bytes with supplied offline family
and version bindings. It establishes no release, build, artifact or deployment facts.
The CLI emits resolution comparisons, while evolution helpers remain private and are
exercised by integration tests.

The replay suite covers 51 cases against independent input and semantic oracles,
including source tampering, mandatory dependencies, malformed roles, permutations,
duplicate delivery, caps, cancellation and isolated public input. The original
source and expected-answer fixtures remain unchanged. The separate Phase 1 query and
SHACL suites retain their resource and nonassertion coverage.

Roadmap item 396 retains Phase 3 integration with actual C25 managed review and
protected records, C27 authenticated availability, C26 durable publication and F22
freshness. Supplied declarations and availability remain simulated, and separately
supplied public scopes do not implement real authorization. Scoped visibility and
later mixed-access inference use their adopted access contracts. General domain
mappings, health connectors and operational public-health workflows remain outside
the delivered reference scope.
