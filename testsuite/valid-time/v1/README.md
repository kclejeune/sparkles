# Valid-time fixtures

These fixtures exercise C27 profile resources using ordinary RDF, SHACL and SPARQL.
They isolate temporal structure and are not complete C25 evidence records.

`exact.ttl` represents a one-nanosecond half-open interval, retains its source offset
and reifies a proposition without asserting it. `unresolved.ttl` retains a date without
policy, an unsupported calendar, unsupported precision and indeterminate endpoint order.
It also includes a claim with no interval. `timezone-policy.ttl` declares importer
outputs for the two choices of a named-zone fold and a quarantined gap, retaining the
zone database version. The declared New York outputs follow the
[IANA 2025b North America rules](https://data.iana.org/time-zones/tzdb-2025b/northamerica).
They are expected import results, without a runtime timezone resolver.

The Rust integration test `valid_time` compares both stored-query templates with an
independent model that enumerates all concrete intervals in bounded endpoint envelopes.
It covers exact and uncertain endpoints, explicit infinities, unknown endpoints,
half-open boundaries, equivalent offsets and touching intervals. Invalid overlays must
fail SHACL for reversed or empty intervals, missing timezone offsets, finite unknown
bounds, duplicate endpoints, reversed envelopes and an uncertain exact event.

The history test uses actual retained commits. It verifies that late imports and later
corrections remain absent from earlier states, retains the original source and assessment,
and checks that temporal matching does not assert a proposed triple. It also demonstrates
that ordinary RDF can contain an arbitrary availability timestamp. That is an explicit
unmet A4 control, not evidence of authenticated receipt or backdating refusal.

```sh
CARGO_TARGET_DIR=target/valid-time cargo test -p sparkles --test valid_time --features shacl
```
