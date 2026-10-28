# Shared entity representation fixtures

These are synthetic, offline A01 profile fixtures. The hand-authored semantic oracle
fixes A1–A15 outcomes before resource queries or replay implementation. Expected term
bindings were authored from those outcomes, without evaluating a matcher or copying
query output. They are immutable acceptance input for later reference replay.

`valid/common.ttl` retains exact C25 revision and JSON selector records. Source JSON
files encode the explicitly supplied statement tuples. Mapping version 1 expands the
`ex:`, `sr:` and `rdfs:` term spellings recorded there to the fixture namespace and
standard predicate namespaces. Source bytes, selected lexical JSON objects and hashes
are retained in `expected/source-selections.json`. The tampered copy uses the same
intended revision ID with different content, for the separate future record-conflict
and retained-digest-refusal checks. It is never loaded as an admitted source revision.

The source-specific files include descriptions and identifiers, qualified relations,
resolution views and conflicts, source lineage, mapping evolution, software boundaries
and aggregate observations. Positive controls accompany all structural invalid cases.
The A3 and A12 states retain n-ary proposal membership without an eagerly materialized
pair set. A4 retains both a merge and split, including an attempted split that still
has a transitive path. Separate geometry states keep the same place anchor. Unicode
identifier spellings and exact JSON encoding vectors remain distinct.

Resolution views and projection states are independently declared expected artifacts.
Phase 1 queries inspect them. Phase 2 will implement bounded replay against the fixed
oracle. Simulated C25 decisions retain a target digest over their explicitly supplied
fixture proposal key bytes. They do not claim a managed C25 target digest or trusted
review authority. Availability timestamps are likewise unverified fixture declarations.
Source identity, source lineage, independence and review admission are different facts.

`valid/public-common.ttl` and `valid/public-boundary.ttl` form a deliberately isolated
public example. They include their own source selection and no private resolution map.
They demonstrate an input boundary rather than live authorization or declassification.
Other examples must not be loaded into that public example to emulate filtered access.

The count examples are synthetic aggregate observations, with cumulative revisions of
10 and 12 rather than 22 incident cases. An unknown denominator is not zero. Disease,
report, event episode, count observation and geometry version remain separate records.
No health connector, operational public-health workflow or forecast is implemented.

Phase 3 requires actual shared-runtime authorization, availability, durable publication
and maintained freshness. A conforming fixture shape is not authenticated provenance.
The corpus does not implement a resolution matcher or close those runtime requirements.
