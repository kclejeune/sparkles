# Shared entity representation profile resources

`sparkles::shared_entity::profile(1)` exposes the A01 vocabulary, structural shapes,
default matching policy and eight competency queries. Unknown versions return `None`.
Selecting a resource does not open or change a dataset. The manifest fixes the exact
SHA-256 digest of every exposed vocabulary, shape, policy and query file.

The profile separates source descriptions, designation scopes and entity anchors.
Relationships retain direction, domain scope, occurrence identity and temporal
qualifiers. Resolution artifacts retain source terms and historic view IDs. The default
policy uses pairwise n-ary separation, whole-component conflicts and the least UTF-8
IRI as a view representative, without asserting `owl:sameAs`.

The shapes validate A01 record structure with SHACL Core and SPARQL constraints.
Validate C25 and C27 records with their own version 1 shape sets as well. Count
observations retain incident or cumulative mode, case definition, population,
denominator state, geography version and uncertainty. A valid shape report does not
authenticate a source, reviewer, receipt, timestamp or digest assertion.

Every query is read-only and assumes a validated explicitly selected authorized view.
Queries preserve record IDs and explicit unknown or unresolved values. The resolution
query inspects declared map artifacts. It does not execute matching or replay. The
support-lineage query returns unknown independence rather than counting repeated URLs
as votes. The evolution query exposes first-availability declarations, without treating
them as proof of historical server knowledge. The software query reports a use
relationship only when the selected data contains one and otherwise preserves unknown
coverage. It does not infer deployed exposure from lock membership.

The [synthetic corpus](../../../../../testsuite/entity-representation/v1/README.md)
provides fixed expected records and exact query bindings. Its projection states,
review decisions and availability coordinates are offline declarations. They do not
install C25 protected metadata, accepted support ownership or authenticated operations.
Required unknown extensions refuse semantic replay. Optional extensions remain
uninterpreted and are excluded from query semantics. A01 reference replay and managed
runtime integration remain separate phases in the
[specification](../../../../../docs/specs/A01-shared-entity-representation/SPEC.md).
