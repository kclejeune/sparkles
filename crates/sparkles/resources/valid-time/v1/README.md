# Valid-time profile version 1

This installable resource profile represents reported validity independently of recorded
history. It contains an RDF vocabulary, SHACL shapes and two typed stored-query
definitions. The [feature guide](../../../../../docs/ai/README.md#valid-time-profile)
describes the current capability. [C27](../../../../../docs/specs/C27-valid-time-profiles.md)
records the full design and its remaining acceptance requirements.

## Selecting and validating resources

`sparkles::valid_time::profile(1)` returns the embedded resources. Any unknown version
returns `None`, with no fallback. The profile IRI is
`urn:x-sparkles:valid-time:profile:1`. Its manifest identifies the vocabulary, shapes,
queries and stored-query definitions by resource IRI and SHA-256 content digest.
These versions identify resources, without changing the database disk format.

Apply `shapes.ttl` using the existing SHACL API or write guard before querying normalized
records. The shapes require one start and end, distinguish known, unknown and unbounded
endpoints, and reject reversed envelopes, empty intervals and overlapping endpoint
uncertainties. Intervals have IRI identities; endpoint nodes may be IRIs or blank nodes.
Known endpoints use `xsd:dateTime` with an explicit offset. An exact
`vt:occurredAt` event has equal endpoint values. A `vt:occurredDuring` window reports
where an event occurred; it does not assert that the event lasted for the whole window.

The shapes validate RDF structure and comparisons. They do not normalize source dates,
verify an external timezone database, authenticate a normalization activity or establish
trust in an author. C25 evidence-profile validation applies separately to the claims
and source revisions. The supplied temporal fixtures isolate C27 constraints and are
not complete C25 source records.

## Normalizing imported time

An importer retains `vt:sourceValue` and `vt:precision` and records its
`vt:Normalization` activity, method, method version and calendar. For the supported
Gregorian calendar with explicit offsets, it writes UTC bounds without rounding away
source precision. A date, month or year becomes a bounded window only under an explicit
calendar and timezone policy. A date without such a policy remains unknown.

`vt:Normalized` means the declared representation is usable under this profile. Its
endpoints can still be explicitly unknown. `vt:Unresolved`, `vt:UnsupportedCalendar`,
`vt:UnsupportedPrecision`, `vt:IndeterminateOrder` and `vt:NonexistentLocalTime` retain
source evidence and yield an unknown verdict. An importer that cannot preserve a
fractional second reports unsupported precision or a conservative outward envelope.
It must not emit an exact rounded time. Unsupported calendar and leap-second
expressions likewise remain unresolved.

Named-zone resolution is an importer responsibility. It records `vt:timezonePolicy`,
`vt:tzdbVersion` and, for an ambiguous local time, `vt:foldChoice` as `earlier` or `later`.
It quarantines a nonexistent local time and does not assume every local day lasts
24 hours. The named-zone fixtures declare expected importer output; they do not
implement or authenticate that resolution.

An unknown endpoint is not infinity. Explicitly unbounded start and end mean negative
and positive infinity. For finite endpoints, the latest start must precede the earliest
end. If those uncertainty envelopes overlap, retain the source expressions with unknown
boundaries and `vt:IndeterminateOrder` instead of inventing their dependency.

These conventions add application rules to the temporal values defined by
[XML Schema Datatypes](https://www.w3.org/TR/xmlschema11-2/#dateTime).
They do not assert additional [OWL-Time](https://www.w3.org/TR/owl-time/) entailments.

## Installing and running templates

An administrator can install each JSON definition through C16. For example, from the
repository root with an authorized local server and dataset `records`:

```sh
curl -X PUT 'http://localhost:3030/$/queries/records/temporal-active-claims' \
  -H 'Content-Type: application/json' \
  --data-binary @crates/sparkles/resources/valid-time/v1/queries/temporal-active-claims.json
curl --get 'http://localhost:3030/records/queries/temporal-active-claims' \
  --data-urlencode 'claimGraph=urn:example:claims' \
  --data-urlencode 'validAt=2026-01-01T00:00:00Z' \
  --data-urlencode 'policy=all' \
  --data-urlencode 'at=commit:42'
```

The example commit must exist in the selected dataset. The caller supplies its ordinary
authorization. Installation changes stored-query configuration, not the dataset or
its acceptance policy. The definitions disable automatic MCP exposure. An administrator
can choose different query names or MCP settings explicitly.

| Definition | Typed parameters |
|---|---|
| `temporal-active-claims.json` | `claimGraph` is an IRI. `validAt` is an exact `dateTime` instant. `policy` is an enumerated string. |
| `temporal-overlapping-claims.json` | `claimGraph` is an IRI. `validFrom` and `validTo` are exact `dateTime` bounds of a nonempty half-open interval. `policy` is an enumerated string. |

Every temporal selector requires an explicit offset. Offset-free selectors, and query
intervals whose start does not precede their end, return no rows. C16 refuses values
that do not parse as the declared type and refuses unknown policies. Its initial
bindings supply RDF terms without inserting user text into the query. There is no new
HTTP temporal selector or validation response in this resource delivery.

Both queries return the claim IRI, optional interval IRI, exact source revision,
`verdict` and `normalizationStatus`. Missing interval or normalization metadata produces
`unknown` with `vt:Missing` status. The graph contains both claim and temporal metadata;
the caller selects one authorized recorded view. Cross-graph joins and authorization
remain ordinary SPARQL concerns.

`policy=all` preserves every verdict. The default `possibleOrDefinite` selects those
two verdicts; `definiteOnly`, `outsideOnly` and `unknownOnly` select their named result.
`outsideOnly` excludes unknowns. A known opposite boundary can prove a row outside even
when its other boundary is unknown. Explicit unbounded endpoints participate in the
membership rules. The templates require validated profile records; they do not replace
the shapes with a second structural validator.

A temporal match describes a claim occurrence. It does not assert its `rdf:reifies`
proposition or establish a review decision. These SELECT templates never construct an
accepted fact graph. Applications must keep temporal status visible and apply their
independent current review and acceptance policy before publishing facts.

## Selecting recorded state

The existing `at` parameter selects recorded history. It is independent of `validAt`
and `validFrom`/`validTo`, and its interpretation is unchanged. Resolve head once to an
actual retained commit before a reproducible multi-step read. Record the dataset and
branch UUIDs, exact commit, profile version, claim graph and normalized selector values.
A removed retained state must fail under the existing history contract; it must never
fall back to current data.

The supplied retained-commit test imports an older claim after pinning a prior state.
Even a client-written `ev:firstAvailableAt` value cannot put that claim into the prior
state. This tests commit selection, not availability authentication. Ordinary RDF still
accepts that timestamp. No template treats `firstAvailableAt`, publication dates or an
arbitrary receipt triple as permission to enter a knowledge cutoff.

C27 Phase 1 remains incomplete because A4 additionally requires server-owned availability
receipts and an actual refusal of client backdating. That depends on C25 Phase 2 and the
shared protected-write boundary, or a separately reviewed runtime helper. The future
service must assign dataset, branch, accepting commit and server timestamp atomically
with ingestion, preserve readable receipts under normal grants and deny ordinary RDF
writes to those managed records. This resource profile does not simulate that authority.
The projection helper, event-time UI and expiry scheduling remain later C27 phases.
