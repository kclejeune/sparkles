# Evidence profile fixtures

These fixtures exercise [evidence profile version 1](../../../crates/sparkles/resources/evidence/v1/README.md).
They are ordinary demonstration RDF. Their receipt, timestamps and retained-commit
references are fictional metadata, not records authenticated by an evidence runtime.

`valid.ttl` contains three occurrences of the same proposition. Two are positive and
one is negative. Domain configuration scope differs for the third occurrence. None
asserts the proposition. Separate evidence links record support, contradiction and a
mention. A mirror shares declared lineage with the first source, while the other
source's independence remains unknown. Notes, decisions and tasks retain distinct types.

The full SHA-256 digests match `source.json`, `source.txt` and `other-source.txt`.
The text selector spans Unicode code points 10 through 14, selecting `Café`. Its four
code points occupy five UTF-8 bytes. The JSON Pointer `/a~1b/m~0n` selects the same
string from keys containing `/` and `~`. Its selected value digest covers the compact
UTF-8 JSON encoding `"Café"`, including the JSON quotation marks. Importer method
`https://example.org/json-value-utf8` version `1` names this fixture convention.
The stored-fact examples distinguish the default graph from a named graph whose IRI
is `ev:DefaultGraph`. They do not claim that commit 7 exists.

Every file under `invalid/` is an additive overlay on `valid.ttl`. The expected result
is a non-conforming SHACL report with its specific intended constraint component.
The bad digest, claim kind and relation each use an otherwise valid fresh record, so
a cardinality failure cannot substitute for the intended check. Mutation controls remove
only those rules and require the isolated records to conform. Cases cover duplicate or
non-triple propositions, invalid enums, malformed digests, a missing source key, a
timestamp without timezone, empty selectors, inconsistent passage revisions and
references, malformed JSON Pointers, incomplete stored references, forbidden graph
fields, blank record identities, extraction confidence without a method and out-of-domain
confidence.

`confidence-cases.json` records exact expected membership in the zero-to-one decimal
domain. The cases include signs, leading zeros, negative zero, missing integer or
fraction parts, long fractions, boundary overshoot and invalid lexical or datatype
forms. Their expected results were checked independently with Python's `decimal.Decimal`
and the XSD decimal lexical grammar. Other declared confidence scales retain their
literal values without adopting this range.

`legacy.trig` contains an asserted C17 fact and C18 source/rendition/span records in a
named graph. The compatibility queries preserve their identifiers without creating
new claims or authenticated availability. An uncited rendition prevents an optional
span match from accidentally dropping sources without spans.

The `regressions/` fixtures preserve distinctions that compatibility and competency
queries must retain. `reasserted.trig` keeps invalidated and current occurrences after
reasserting a business triple in default and named graphs. `split-citations.trig` places
renditions and citations independently across default and named graphs, with an uncited
rendition as a control. Access-filtered tests check both sides of that join.
`duplicate-judgments.ttl` adds a link with the same target, relation, reference and author
as an existing link, but a different IRI and creation activity. Both identities survive.

Run the resource integration tests with:

```sh
cargo test -p sparkles --features shacl --test evidence_profile
cargo test -p sparkles --no-default-features --test evidence_profile
```

The first run includes structural validation and confidence cases. Both runs check
query results, non-asserting occurrences, compatibility, exact selectors and RDF 1.2
round trips. Tests compare exact record identities and RDF terms rather than counts
alone. Runtime authentication, immutability, review, accepted-support ownership and
reference-resolution acceptance follow in C25 Phase 2.
