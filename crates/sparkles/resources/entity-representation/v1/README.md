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

The explicit `positiveMatchRule` is `AcceptedPossibleMatch`. Selected accepted terminal
PossibleMatch decisions contribute prospective resolve-together connectivity. Rejected,
needs-evidence-only and undecided proposals contribute none. Conflicting terminal
outcomes retain positive prospective connectivity and make the affected component
unresolved. This application policy does not supply reviewer authority or logical
equality. Missing and unknown positive rules are invalid policy records.

Each InputManifest has exactly one `contentDigest`, a `sha256:` prefix followed by
64 lowercase hexadecimal characters. Its retained payload is compact UTF-8 JSON with
fields `profileVersion`, `inputIds`, `requiredExtensions` and `extensionIds` in that
order. Version is the integer 1. The other fields are sorted unique arrays of IRI
strings using the A01 scalar encoding rules. No whitespace, byte-order mark or trailing
newline occurs. The record identifier and digest metadata are not payload fields.
An input-ID set must not contain the manifest's own identifier. The recognized
semantic manifest fields are all represented, including required extension profiles
and linked extension record IDs. This is a small declared payload convention rather
than general RDF canonicalization. Shapes check digest syntax and structure; reference
replay separately verifies the digest against retained bytes.

An artifact digest carries its algorithm and value together. A Verified declaration
also names its versioned `verificationMethod` IRI. An unknown verification can remain
unknown without inventing a digest. Structural completeness does not authenticate the
verifier or demonstrate that a production artifact was checked.

The aggregate-observation query returns selected source membership, recorded
coordinate, input manifest and offline selection profile alongside each expected
observation. The A10 fixture envelope fixes a unique terminal revision within one
explicit synthetic series. Its earlier and revised states have distinct inputs.
This example does not define a general health revision or admission policy.
