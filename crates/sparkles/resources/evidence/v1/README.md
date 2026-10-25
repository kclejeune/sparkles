# Evidence profile version 1

This resource profile describes sources, unasserted claim occurrences and qualified
evidence in ordinary RDF. It includes a vocabulary, SHACL shapes, eight competency
queries and two compatibility views. The [feature guide](../../../../../docs/ai/README.md#evidence-profile)
explains its current scope. [C25](../../../../../docs/specs/C25-evidence-and-assessments.md)
defines the managed operations that follow this resource phase.

## Selecting a version

The Rust library embeds the resources without opening or changing a dataset.

```rust
let profile = sparkles::evidence::profile(1).expect("supported evidence profile");
assert_eq!(profile.version, 1);
assert!(sparkles::evidence::profile(2).is_none());
```

An unknown version returns `None`. It never falls back to a different profile. The
vocabulary resource is `urn:x-sparkles:evidence:profile:1`, and the shapes resource is
`urn:x-sparkles:evidence:shapes:1`. Both declare `ev:profileVersion 1`. The embedded `manifest.ttl` gives
these resources and each query a named IRI and full SHA-256 content digest. Its bytes
are available through `profile.manifest`. These versions
identify RDF resources and do not change the store's disk format or reader admission.
Loading a vocabulary triple cannot enable a managed API.

## Validating and querying

From the repository root, a CLI built with `shacl` can validate the demonstration data.

```sh
sparkles shacl --data testsuite/evidence/v1/valid.ttl \
  --shapes crates/sparkles/resources/evidence/v1/shapes.ttl --format json
sparkles query --data testsuite/evidence/v1/valid.ttl \
  --query crates/sparkles/resources/evidence/v1/queries/claim-occurrences.rq \
  --results json
```

Applications may load the vocabulary and their own profile records into graphs selected
by their ordinary access policy. They may apply these shapes with the existing SHACL
validation API or guard. No evidence setting, authenticated source registry, claims
endpoint, review state machine or managed projection is enabled by these resources.
The supplied fixture's availability receipt and commit references are demonstrations.
They do not establish authenticated local availability or resolve a retained snapshot.

Shapes require IRI identities for source, claim, reference and authored records, a
nonempty source revision key, full lowercase
SHA-256 digests, exact claim kinds and polarity, qualified authorship, and one of the
three reference structures. Timestamp literals require an explicit timezone. A passage
and its reference must preserve the revision and rendition they identify. Domain scope
predicates remain permitted. Confidence nodes and text selectors may be blank nodes or
IRIs. Structural validation does not check retained bytes,
authenticate authors, enforce immutability, normalize timestamps or establish access to
an external reference. Those checks belong to the planned typed runtime operations.

## Representing evidence

Each occurrence has its own `ev:Claim` IRI. Its one `rdf:reifies` triple term describes
a proposition without asserting it. The claim retains its source revision, creation
activity, author, kind, polarity and application scope. Equal propositions do not merge
occurrences. A negative claim records a denial without adding an RDF negation rule.

An `ev:EvidenceLink` relates an exact `ev:EvidenceRef` to a claim or assessment through
`ev:Supports`, `ev:Contradicts` or `ev:Mentions`. These are distinct attributed
judgments. Derivation is not support, and a mention is not corroboration. Notes,
decisions and tasks retain their separate types and relations. An identity proposal
and a review decision can be represented as RDF, but loading them does not change
identity resolution, admission, execution authority or asserted facts.

A revision's source key identifies an importer observation within its source. Its digest
identifies the exact retained bytes. Renditions preserve that revision while recording
their own conversion activity, media type, language and digest. Translations are new
renditions of the same revision. `ev:upstreamSource` and `ev:sourceFamily` record known
lineage. Missing lineage remains unknown independence. Distinct source URLs do not
establish independent corroboration.

Text selectors use half-open Unicode code point offsets in the retained normalized
rendition. A passage records a SHA-256 digest of the selected text and an optional
permitted quote. A structured reference records an RFC 6901 JSON Pointer and a selected
value digest, with its normalization method and version. The fixture uses compact
UTF-8 JSON without ASCII escaping for the selected value. This is a fixture method,
not a universal canonicalization rule for arbitrary importers.

A stored-fact reference records dataset and branch UUIDs, a commit number, a graph kind
and an expected triple term. The default graph has no `ev:graph` value. A named graph
has one IRI, even if that IRI equals `ev:DefaultGraph`. The shapes check this structure
without checking that the commit or triple exists. Reference resolution, blank-node
anchoring and non-disclosing unavailable responses remain runtime work under item 81.

Confidence records retain their evaluator and declared scale. `ev:ZeroToOne` requires
an `xsd:decimal` in the closed interval from zero to one. The shapes validate its exact
lexical domain, including signs, leading zeros and arbitrarily long decimal fractions,
without rounding through floating point. Other scales retain uninterpreted literal
values. The profile does not combine confidence facets or turn confidence into truth.

## Competency queries

Each query selects from the authorized view supplied by its caller. Results describe
visible RDF and do not grant trust or access.

| Query | Result and interpretation |
|---|---|
| `claim-occurrences.rq` | Lists occurrence IDs, propositions, polarity, kind, authors and revisions without grouping equal propositions. |
| `assertion-status.rq` | Reports whether each proposition also appears as an ordinary triple in the selected default graph. It does not admit the proposition. |
| `evidence-relations.rq` | Retains supports, contradictions and mentions as separate qualified links. |
| `source-lineage.rq` | Lists declared families and upstream sources. Missing lineage is reported as unknown. |
| `passage-selectors.rq` | Lists exact revision, rendition, selector, digest and optional quote. |
| `stored-fact-references.rq` | Lists the recorded view and expected triple, preserving default and named graphs. It does not resolve that view. |
| `opposite-polarities.rq` | Lists syntactically equal propositions with opposite polarity. Applications must compare domain scope before calling these contradictory. |
| `unsupported-claims.rq` | Lists claims without an explicit visible supporting link. Absence of a visible link does not prove that no hidden evidence exists. |

## Compatibility views

`compatibility/legacy-assertions.rq` constructs `ev:LegacyAssertion` descriptions of
currently asserted C17 records while retaining their reifier IDs, source, activity,
legacy confidence and quote where present. It keeps activity associations distinct
from author attribution. It does not emit a new `ev:Claim` or copy the business triple
into its output. The original C17 dataset still contains that assertion. A retracted
record is absent from this current-assertion view unless an appropriate historical
snapshot is explicitly selected.

`compatibility/legacy-source-spans.rq` constructs `ev:LegacyRendition` descriptions of
C18 renditions and Web Annotation selectors from their `#char=start,end` spans. Source,
rendition and span IDs remain unchanged, including renditions with no cited span. It
does not invent immutable revision identities, availability receipts or text digests
that the old records did not retain.

Both are read-only `CONSTRUCT` queries. Their output is a compatibility description,
not a migration into authenticated records. Neither view changes existing assertions,
supersession or retraction semantics. Migration and managed writes remain explicit
future operations.

## Validation fixtures

[The fixtures](../../../../../testsuite/evidence/v1/README.md) contain actual source
bytes, a valid RDF 1.2 example, invalid overlays, compatibility records and independent
confidence-domain cases. The Rust `evidence_profile` integration test exercises the
resources through parsing, query execution, round trips and SHACL validation.
