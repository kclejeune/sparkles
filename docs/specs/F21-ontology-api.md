# F21: An OWL 2 ontology API

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 is the axiom model, reading and writing it through
> the OWL 2 mapping to RDF graphs, the OWL 2 EL, QL and RL profile checks, the Rust
> library API, `/$/ontology/{ds}` and `sparkles ontology`. Phase 2 adds the Python and
> Node.js surfaces, the JVM entry point that runs Jena's `jena-ontapi` over a Sparkles
> graph, entity views that include materialized inferences, and the reasoner's use of the
> profile checks. Phase 3 adds the OWL 2 DL global restrictions, the functional-style
> syntax reader, an MCP tool and ontology diffs between commits.
>
> **User docs:** none, because nothing is built.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

This design was written from the W3C recommendations *OWL 2 Web Ontology Language
Structural Specification and Functional-Style Syntax*, *OWL 2 Web Ontology Language
Mapping to RDF Graphs* and *OWL 2 Web Ontology Language Profiles* (all second edition,
2012), the W3C RDF 1.1 Concepts recommendation, the public documentation and Javadoc of
Apache Jena's `jena-ontapi` module (`OntModel`, `OntModelFactory`, `OntSpecification` and
the `Ont*` object interfaces) and of Jena's legacy `org.apache.jena.ontology` API, and the
Sparkles code. No Jena code was read for this spec, and no code from any other ontology
library was copied. It builds on materialized reasoning and its inconsistency diagnostics
([C08](C08-inference-freshness.md)), materialization at scale
([F16](F16-materialization-at-scale.md)), query-time reasoning
([F17](F17-query-time-reasoning.md)), the bindings ([P01](P01-python-bindings.md),
[P04](P04-jvm-bindings.md), [P05](P05-node-bindings.md)) and the library administration
API ([P06](P06-library-admin-api.md)), schema discovery ([C02](C02-schema-discovery.md)),
and access control ([C12](C12-graph-access-control.md),
[C12b](C12b-triple-access-control.md)).

## 1. Summary, goals, non-goals

An OWL 2 ontology is stored in RDF as triples, and the OWL 2 mapping to RDF graphs says
how those triples encode axioms. A class expression such as "something that teaches at
least one course" is a blank node with `rdf:type owl:Restriction`, `owl:onProperty` and
`owl:someValuesFrom`. An annotated axiom is an `owl:Axiom` node with
`owl:annotatedSource`, `owl:annotatedProperty` and `owl:annotatedTarget`. A program that
reads or edits an ontology through triples has to know these encodings, keep RDF lists
well formed, and avoid leaving orphaned blank nodes when it removes an axiom.

Sparkles has no API at this level. The reasoner (`crates/sparkles-reasoner`) reads OWL
vocabulary only as rule premises, and its diagnostics
(`crates/sparkles-reasoner/src/diagnostics.rs`) find inconsistencies through SPARQL
queries. The JVM binding lets Jena's own `OntModel` run over a Sparkles graph view, but
every call is a `find()` across the native boundary, which P04 §3.10 documents as much
slower than an in-memory graph. Python and Node.js programs have nothing.

This spec adds an object API over OWL 2 ontologies stored in a Sparkles dataset: entities,
class and property expressions, axioms and annotations, read from a graph and written back
to it in the RDF form the mapping defines, with checks for the OWL 2 EL, QL and RL
profiles.

**What changed since AUDIT §5.** [AUDIT.md](../AUDIT.md) §5 lists `jena-ontapi`'s object
API among the non-goals of the first version. Three things have changed. Reasoning has
grown from a forward rule engine into a set of designs that depend on knowing which
profile an ontology is in: OWL 2 RL materialization with diagnostics (C08), complete RL
rules at scale (F16), and OWL 2 QL rewriting (F17, whose §10 plans a QL profile check of
its own). The engine now has bindings in Rust, Python, the JVM and Node.js, and none of
them can read an ontology as axioms. And agents that keep knowledge in a dataset
([C17](C17-agent-memory.md)) and drafted shapes ([C02](C02-schema-discovery.md)) edit
schema triples, where an axiom-level API prevents malformed structures. The non-goal
stays in AUDIT §5 as the record of the original scope; this spec supersedes it.

**Goals**

1. A Rust model of OWL 2's structural specification: entities, class expressions,
   object and data property expressions, data ranges, axioms, annotations and the
   ontology header (§3).
2. Reading that model from a named graph, the default graph or a set of graphs with their
   `owl:imports` closure, through the reverse mapping of the RDF mapping's §3, with a
   report of the triples that do not map to any axiom (§4).
3. Writing axioms back through the forward mapping of the RDF mapping's §2, in one
   transaction, so that adding then removing an axiom leaves the graph as it was (§5).
4. Profile checks for OWL 2 EL, QL and RL that name each axiom outside the profile and
   the rule it breaks (§6).
5. The same model and checks from Rust, Python and Node.js, and on the JVM through Jena's
   `jena-ontapi` interfaces (§2).
6. A defined relation to the reasoner: which profile a materialization or a query-time
   mode is complete for, and entity views that include materialized inferences (§7).
7. Reads and writes under the caller's view, budgets and write-time validation (§8).

**Non-goals.** An OWL 2 DL reasoner, OWL 2 EL classification or any reasoning beyond what
C08, F16 and F17 provide. Manchester syntax and OWL/XML. Re-implementing the whole
`jena-ontapi` interface set in native code. SWRL rules. Ontology modularization,
alignment or versioning beyond diffs between commits. Storing axioms anywhere other than
as RDF triples in the dataset.

## 2. User-visible behaviour

### 2.1 Rust

The library adds an `ontology` handle to `Dataset`, beside `reasoning()` and `schema()`
(P06):

```rust
let onto = ds.ontology().graph("http://ex.org/onto").imports(true).open()?;
for c in onto.classes() {
    println!("{} {:?}", c.iri(), onto.labels(c.iri()));
}
let prof = onto.class("http://ex.org/Professor")?;
for sup in onto.super_classes(&prof) {            // ClassExpression values
    println!("{sup}");                             // functional-style syntax
}
let report = onto.check(Profile::El);
ds.ontology().graph("http://ex.org/onto").edit(|o| {
    o.add(Axiom::sub_class_of(
        Class::new("http://ex.org/Professor"),
        ClassExpression::some(ObjectProperty::new("http://ex.org/teaches"),
                              Class::new("http://ex.org/Course")),
    ))?;
    o.remove(&old_axiom)?;
    Ok(())
})?;
```

`open()` reads the axioms of the graph at the handle's snapshot. `edit` opens a write
transaction, applies the additions and removals as triples, and commits once. The
returned `Ontology` is immutable and keeps its snapshot, as a `Snapshot` does.

### 2.2 Python and Node.js (Phase 2)

Python has `ds.ontology("http://ex.org/onto", imports=True)` for reads and
`with ds.ontology_edit(graph) as o: o.add(SubClassOf(...))` for edits. Axioms and
expressions are frozen objects with structural equality and hashing, their `str()` is
functional-style syntax, and IRIs may be given as strings or the package's terms (P01).
Node.js has `dataset.ontology({ graph, imports })`, which returns a promise of an
`Ontology` with the same methods. Its axioms are plain objects with a `type` member
(`"SubClassOf"`, …) and RDF/JS terms (P05), and long listings are async iterables in
batches, as query results are.

### 2.3 JVM (Phase 2)

`SparklesOnt.model(dataset, graphIri, spec)` returns an
`org.apache.jena.ontapi.model.OntModel` built by Jena's `OntModelFactory` over a graph
that Sparkles supplies. The graph is a read-through copy of the ontology graph in a Jena
in-memory graph, filled with one bulk read across the boundary, with writes sent to the
Sparkles graph in the caller's transaction. Programs therefore use `jena-ontapi`'s own
interfaces (`OntClass`, `OntObjectProperty`, `OntIndividual` and the others), and get
`jena-ontapi`'s profile behaviour from the `OntSpecification` they pass.
`SparklesOnt.check` runs the native profile checks of §6 on the graph. §13 explains why
the binding does not implement `OntModel` natively.

### 2.4 Server and CLI

| Route | Answer |
|---|---|
| `GET /$/ontology/{ds}?graph=G&imports=true` | The ontology header (IRI, version IRI, imports, annotations), counts of entities by kind and of axioms by type, the graphs read, and the count of unmapped triples. |
| `GET /$/ontology/{ds}/axioms?graph=G&about=IRI&type=SubClassOf&format=json\|ofn` | The axioms that mention an entity or have a type, paginated, as JSON or functional-style syntax. |
| `POST /$/ontology/{ds}/check` with `{"graph", "imports", "profiles": ["el","ql","rl"]}` | One report per profile (§6.5). |
| `POST /$/ontology/{ds}/axioms` with `{"graph", "add": [...], "remove": [...]}` | Applies axioms in JSON form as one commit and answers with the receipt. Takes `dryRun` (C15) and `If-Match`. |

`sparkles ontology summary|axioms|check|edit` runs the same operations against `--loc` or
`--server`, and `sparkles ontology check FILE` checks a file offline. `check` exits with
status 1 when an ontology is outside a requested profile.

### 2.5 Errors

An IRI that is not declared or used as the asked entity kind is `NotFound`. An axiom that
the forward mapping cannot write, such as a `SubClassOf` whose superclass is a data range,
is rejected by the type system in Rust and with `invalid-axiom` elsewhere. Removing an
axiom that the graph does not hold is not an error; the answer counts removed axioms.

## 3. The model

The model follows the structural specification's UML classes, with Rust enums for the
abstract classes. It lives in a new crate, `sparkles-owl`, which depends on `oxrdf` and
not on the store, so that it can be checked against files without a dataset.

### 3.1 Entities and individuals

`Class`, `Datatype`, `ObjectProperty`, `DataProperty`, `AnnotationProperty` and
`NamedIndividual` wrap an IRI. `AnonymousIndividual` wraps a blank node label that is
local to the ontology read (§4.4). `Individual` is either. `Literal` is `oxrdf::Literal`.
The built-in entities that the structural specification reserves (`owl:Thing`,
`owl:Nothing`, `owl:topObjectProperty`, `rdfs:Literal` and the others) are constants.

### 3.2 Expressions

- `ObjectPropertyExpression`: a property or `ObjectInverseOf`.
- `DataRange`: a datatype, `DataIntersectionOf`, `DataUnionOf`, `DataComplementOf`,
  `DataOneOf` or `DatatypeRestriction` with facet-literal pairs.
- `ClassExpression`: a class, the Boolean connectives (`ObjectIntersectionOf`,
  `ObjectUnionOf`, `ObjectComplementOf`, `ObjectOneOf`), the object restrictions
  (`ObjectSomeValuesFrom`, `ObjectAllValuesFrom`, `ObjectHasValue`, `ObjectHasSelf`,
  `ObjectMinCardinality`, `ObjectMaxCardinality`, `ObjectExactCardinality`, qualified or
  not) and the data restrictions with the same shapes.

Operands of the n-ary connectives are kept as sets, as the structural specification
defines them, with a stable order (by functional-style rendering) for output.

### 3.3 Axioms

Every axiom type of the structural specification's §9: declarations; class axioms
(`SubClassOf`, `EquivalentClasses`, `DisjointClasses`, `DisjointUnion`); object property
axioms (sub-properties with chains, equivalence, disjointness, inverses, domain, range,
and the characteristics functional, inverse functional, reflexive, irreflexive,
symmetric, asymmetric and transitive); data property axioms; `DatatypeDefinition`;
`HasKey`; assertions (`SameIndividual`, `DifferentIndividuals`, `ClassAssertion`,
object and data property assertions and their negative forms); and annotation axioms
(`AnnotationAssertion`, `SubAnnotationPropertyOf`, annotation domain and range). Each
axiom carries its set of annotations. Two axioms are equal when their structure and their
annotations are equal.

### 3.4 Annotations

An annotation is a property and a value (an IRI, a literal or an anonymous individual),
and may itself be annotated. Annotations on the ontology are part of the header.

### 3.5 The ontology header

The ontology IRI, the version IRI, the direct imports and the ontology annotations, read
from the one `owl:Ontology` node of the graph. A graph with no such node has an anonymous
ontology. A graph with several is read with the first by term order and a warning.

## 4. Reading from RDF

### 4.1 The reverse mapping

Reading follows the RDF mapping's §3 in its order: find the ontology header, collect
declarations (§3.2.1, including the typing triples of the table "Parsing of
declarations"), parse annotations, then build class expressions, property expressions and
data ranges from their blank node structures, then axioms, then axioms with annotations
through `owl:Axiom` and `owl:AllDisjointClasses`-style nodes. The parser consumes the
triples it maps and keeps the rest as **unmapped triples**. A triple can be unmapped
because it uses a structure the mapping does not define (for example a restriction with
two `owl:onProperty` values), because it uses vocabulary outside OWL 2, or because an
entity is used without a declaration in a position the mapping cannot type.

### 4.2 Strict and lenient reads

The mapping assumes that each blank node of a class expression is used once and that
every entity is declared. Many ontologies in the wild break both. The reader has two
modes:

- **Lenient** (the default): an undeclared IRI is typed from its use where the use
  decides it (an object of `rdfs:subClassOf` is a class; an `owl:onProperty` whose
  restriction has `owl:someValuesFrom` with a class is an object property). A blank node
  structure used more than once is parsed once and shared. Every such repair is listed
  as a warning with the triples involved.
- **Strict**: an undeclared entity or a shared blank node is an unmapped triple and an
  error in the report, as an OWL 2 DL parser would treat it.

Punning (one IRI used as a class and an individual) is accepted in both modes, as OWL 2
allows it, and listed by the DL check of §6.4.

### 4.3 Which graphs

The handle reads one graph, the default graph, or a list of graphs. With `imports`, it
adds the graphs that `owl:imports` leads to, resolved as the reasoner resolves them
(`crates/sparkles-reasoner/src/inputs.rs`: graphs of the dataset by IRI, a location
mapping, and fetching only when the caller asks for it). Each axiom remembers the graph
it came from, so a write can target the graph that holds an axiom and a report can name
it.

### 4.4 Blank nodes

Blank nodes are read with their store labels (`_:b<hex>`, see
[API: Blank nodes](../API.md#blank-nodes)). An anonymous individual keeps that label, so
an axiom read from a snapshot can be removed later by its blank node. Blank nodes of
class expressions are not exposed; an expression is a value, compared structurally.

### 4.5 Size and caching

The terminological part (declarations, class, property and datatype axioms, annotation
axioms on entities) is read eagerly. It is found through the predicate and type
permutations of the store, so the read touches only triples with OWL and RDFS vocabulary
and `rdf:type` triples whose object is an OWL built-in class. Assertions are not read
eagerly: `class_assertions(c)`, `instances(c)`, `property_assertions(i)` and the
listing of all assertions are iterators over the store, so an ontology with a billion
assertions opens in the time its terminology takes. A parsed ontology is cached per
(snapshot, graphs, mode, view), with the same bounded cache the reasoner keeps for its
inputs (`sparkles_reasoner::Cache`), and is dropped at the next commit that touches one
of its graphs.

## 5. Writing to RDF

### 5.1 The forward mapping

Adding an axiom writes the triples of the RDF mapping's §2: the tables for
declarations, expressions, axioms without annotations and axioms with annotations. New
blank nodes are allocated by the transaction, as `INSERT DATA` allocates them. RDF lists
are written with fresh nodes for each cell. An axiom with annotations is written in the
reified form with `owl:Axiom`, and its main triple too, as the mapping requires.

### 5.2 Removal

Removing an axiom removes its main triples and the blank node structures that encode its
expressions and annotations, when no other triple of the graph refers to those blank
nodes. A structure that another triple still references is left in place and listed in
the answer as kept. This makes `add` followed by `remove` restore the graph exactly, and
it never leaves a list cell or a restriction node that nothing reaches. Removing an
axiom that was read in lenient mode from a shared structure removes only the triples
that belong to that axiom.

### 5.3 Declarations

`add` writes a declaration for each entity of the axiom that the graph does not declare,
unless the edit sets `declare(false)`. `remove` never removes declarations; removing an
entity is `remove_entity(iri)`, which removes its declaration, the axioms that mention it
and its annotation assertions, and lists them.

### 5.4 Transactions and receipts

Every edit is one `Dataset::transaction` and one commit of kind `update`. Write-time
validation (C10), quotas, protections (C12b) and dry runs (C15) apply as they do to a
SPARQL update. An edit that would write into an imported graph names that graph
explicitly; the default target is the graph the handle opened.

## 6. Profile checks

### 6.1 What is checked

A check takes the parsed ontology, including its imports closure when the handle has
`imports`, and returns, per profile, whether the ontology is in it and the list of axioms
that are not, each with the rule broken. The rules are those of the OWL 2 Profiles
recommendation, written as Rust predicates over the model. Each rule carries the section
it comes from, so a report reads "OWL 2 Profiles §2.2 (OWL 2 EL): ObjectUnionOf is not
allowed".

### 6.2 OWL 2 EL

Class expressions limited to classes, intersections, existential restrictions on
properties and data properties, `ObjectHasValue`, `ObjectHasSelf`, `DataHasValue` and
enumerations of one individual or literal. No universal restrictions, cardinalities,
unions, complements, inverse properties, or disjoint, functional object, symmetric,
asymmetric or irreflexive object property axioms. Datatypes from the EL datatype list
(so not `xsd:double` or `xsd:float`), and the restriction on ranges of property chains.

### 6.3 OWL 2 QL and RL

**QL:** the separate grammars for subclass and superclass expressions (unqualified
existentials on the left; intersections, complements of subclass expressions and
qualified existentials on the right), no individuals in class expressions, no property
chains, transitivity, functionality, keys or `SameIndividual`, and the QL datatype list.

**RL:** the separate grammars for subclass, superclass and equivalent-class expressions,
maximum cardinalities of 0 or 1 only on the right, no `DisjointUnion` or reflexive
properties, and the RL datatype list.

### 6.4 OWL 2 DL (Phase 3)

The global restrictions of the structural specification's §11: simple properties in
cardinality restrictions, `ObjectHasSelf` and the characteristics that require them; the
regularity of the property hierarchy with chains; typing constraints, such as one IRI
used both as an object and a data property; reserved vocabulary used as entities; and
anonymous individuals in assertions forming no cycle. Strict-mode parse errors are DL
violations too.

### 6.5 Report

```json
{ "profile": "el", "conforms": false, "axioms": 412, "checked": 412,
  "violations": [
    { "axiom": "SubClassOf(ex:A ObjectAllValuesFrom(ex:p ex:B))",
      "graph": "http://ex.org/onto", "rule": "§2.2",
      "message": "ObjectAllValuesFrom is not allowed in OWL 2 EL" } ],
  "truncated": false }
```

A report lists at most 1,000 violations by default and says it was truncated. A check
over assertions scans only the assertion forms that a profile restricts (class
assertions with a complex class, `owl:sameAs`, negative assertions), through the store.

## 7. Relation to reasoning

- **Materialization (C08, F16).** The OWL 2 RL/RDF rules are complete for ground atom
  entailment only for ontologies that satisfy the RL profile (OWL 2 Profiles §4.3,
  Theorem PR1). `sparkles infer --profile owl-rl` and `/$/reason/{ds}` run the RL check
  on the input graphs (Phase 2) and add a warning with the count of axioms outside RL,
  so a user knows which answers may be incomplete. The current rule set is an RL subset
  (`OWL_RL_RULES` in `crates/sparkles-reasoner/src/lib.rs`); the warning also names RL
  axioms whose rules the subset lacks, from a table kept beside the rules file.
- **Query time (F17).** F17 §7 rewrites queries over OWL 2 QL ontologies and plans a QL
  profile check in `qlrewrite.rs`. That check is this spec's §6.3, and `qlrewrite.rs`
  calls `sparkles-owl` instead of writing its own. F17's PerfectRef reads the parsed QL
  axioms from the model rather than from triples.
- **Inferred views (Phase 2).** `open_with_inferences()` reads `urn:x-sparkles:inferred`
  as part of the ontology, so `super_classes(c)` returns the materialized closure when
  the materialization is fresh. When C08 reports it stale, the answer carries the same
  staleness flag a query gets. Without a materialization the method returns asserted
  axioms only; it never runs the reasoner on its own.
- **Diagnostics.** C08's inconsistency findings name triples. With the model, a finding
  can also name the axioms whose triples it used, which F18's explanations display.

## 8. Access control and budgets

A read uses the caller's view (C12, C12b). Triples the view hides are absent, so an
axiom some of whose triples are hidden is either absent or, when the main triple is
visible and the structure is not, appears among the unmapped triples. The routes refuse
a caller whose grants limit endpoints to exclude `query`. Writes need `write` on the
target graph, and protections check each triple as for any write. Parsing and checks run
under the request's deadline and memory budget (C01); an ontology whose terminology
exceeds 5 million triples fails with `507` and `budget: "ontology-size"` unless the
operator raises the limit.

## 9. Architecture

| Crate / file | Change |
|---|---|
| `crates/sparkles-owl/` (new) | `model.rs` (entities, expressions, axioms, annotations), `read.rs` (the reverse mapping over a triple source trait), `write.rs` (the forward mapping and owned-structure removal), `profiles/{el,ql,rl,dl}.rs`, `ofn.rs` (functional-style writer; reader in Phase 3), `json.rs` (the JSON form of axioms). Depends on `oxrdf` only. |
| `crates/sparkles/src/handles/ontology.rs` (new) | The `Ontology` handle of P06: graph selection, imports through `sparkles_reasoner::inputs`, the triple source over a `Snapshot` with the caller's view, the cache, `edit` over `Dataset::transaction`. |
| `crates/sparkles/src/handles/mod.rs` | `Dataset::ontology()`. |
| `crates/sparkles-reasoner/src/lib.rs`, `run.rs` in `crates/sparkles/src/reasoning/` | The RL warning of §7 (Phase 2). |
| `crates/sparkles-server/src/http/ontology.rs` (new) | The routes of §2.4, the OpenAPI entries and their library mappings (P06's test that maps operations to library calls). |
| `crates/sparkles-server/src/ontology_cmd.rs` (new) | `sparkles ontology`. |
| `crates/sparkles-py/src/ontology.rs` (new) | The Python classes (Phase 2). |
| `crates/sparkles-node/src/ontology.rs` (new), `js/engine` | The Node.js methods and TypeScript types (Phase 2). |
| `crates/sparkles-ffi/src/ontology.rs` (new), `jvm/sparkles-jena/.../SparklesOnt.kt` (new) | The bulk read of an ontology graph, the native check, and the `OntModel` factory (Phase 2). `jena-ontapi` becomes an optional dependency of `sparkles-jena`. |

## 10. Phases

**Phase 1 (M–L).** `sparkles-owl` with the model, both mappings, lenient and strict
reads, the EL, QL and RL checks and the functional-style writer; the Rust handle with
reads, edits, imports and caching; the server routes; the CLI.

**Phase 2 (M).** Python and Node.js surfaces; the JVM entry point; inferred views; the RL
warning in reasoning runs; F17's use of the QL check.

**Phase 3 (M).** The DL global restrictions; the functional-style syntax reader, so
`sparkles ontology check` and `sparkles load` read `.ofn` files; an MCP tool
`ontology_axioms` that lists and edits axioms with C17's preview and provenance; diffs
of the axioms between two commits (`GET /$/ontology/{ds}/diff?from=&to=`), built on F06's
diff of triples.

## 11. Acceptance examples

The examples use `ex:` = `http://ex.org/`.

**A1. Read a restriction.** A graph holding
`ex:Professor rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:teaches ;
owl:someValuesFrom ex:Course ]` with the three declarations reads as one axiom
`SubClassOf(ex:Professor ObjectSomeValuesFrom(ex:teaches ex:Course))` and no unmapped
triples.

**A2. Round trip.** For each ontology of the OWL 2 test cases that is in OWL 2 DL, reading
its RDF/XML form into a graph, writing the parsed axioms into an empty graph and reading
that graph again gives an equal axiom set.

**A3. Removal restores the graph.** Adding
`SubClassOf(ex:A ObjectIntersectionOf(ex:B ObjectAllValuesFrom(ex:p ex:C)))` with an
`rdfs:comment` annotation to a graph, then removing it, leaves the graph's triples equal
to what they were before, and the diff between the two commits is empty.

**A4. Shared structures.** A graph where two `rdfs:subClassOf` triples share one
restriction blank node reads in lenient mode as two axioms and one warning, and in
strict mode as unmapped triples. Removing one of the two axioms keeps the restriction
and lists it as kept.

**A5. EL check.** An ontology with `SubClassOf(ex:A ObjectAllValuesFrom(ex:p ex:B))` and
`FunctionalObjectProperty(ex:p)` fails the EL check with two violations naming those
axioms, and passes the RL check.

**A6. QL check shared with F17.** The ontology of F17's example A8 passes the QL check,
and adding `TransitiveObjectProperty(ex:teaches)` makes it fail with one violation;
F17's `owl-ql` mode then falls back as its §7 describes.

**A7. Assertions stay lazy.** On LUBM at 1,000 universities loaded with its ontology in
a named graph, `open()` reads the ontology in under 100 ms, and `instances(ub:Professor)`
streams its results without the ontology holding them.

**A8. Views.** A caller whose C12b protection hides `rdfs:comment` triples reads the
axioms of A3 without the annotation, and its edit that adds an `rdfs:comment` is refused
as the equivalent SPARQL update would be.

**A9. JVM.** `SparklesOnt.model(ds, "http://ex.org/onto", spec)` with an OWL 2 DL
`OntSpecification` lists the classes of A1 through `OntModel.classes()`. Adding a
superclass to an `OntClass` inside a write transaction makes one Sparkles commit, and
building the model over a 10,000-axiom ontology takes one native read.

**A10. Reasoning warning.** `sparkles infer --profile owl-rl` on a graph with a
`DisjointUnion` axiom completes and warns that one axiom is outside OWL 2 RL.

## 12. Testing

- **Mapping tests.** The OWL 2 test cases published by the W3C OWL Working Group include
  ontologies in RDF/XML and functional-style syntax. Each case whose RDF form parses is
  read, and its axiom count and profile membership are compared with the expectations
  the cases record (each case lists the profiles it belongs to). Cases that use features
  outside OWL 2 DL are read in lenient mode and checked for no panic.
- **Property tests.** Random axioms from a generator over the model are written to an
  empty graph and read back equal (A2's property). Random edit sequences of adds and
  removes leave the graph equal to writing the final axiom set into an empty graph.
- **Profile tests.** One positive and one negative example per rule of each profile,
  written from the recommendation's grammar, plus the profile expectations of the test
  cases.
- **Differential against `jena-ontapi`.** A JVM fixture reads a set of public ontologies
  with `OntModelFactory` in each profile specification, lists classes, properties and
  individuals, and compares the counts with Sparkles' reads. Differences are recorded
  as known when they come from `jena-ontapi`'s profile personalities, which hide objects
  outside the profile rather than reporting them.
- **Bindings and benchmarks.** P06's mapping test covers every new route and library
  call in Python, Node.js and the JVM. X06 records reading and checking the LUBM,
  OWL2Bench and a large public ontology's terminology with the reasoning runs.

## 13. Rejected alternatives

- **A native implementation of `jena-ontapi`'s interfaces in the JVM binding.** The
  module is about 35,000 lines (AUDIT §1) and its behaviour is defined by its code more
  than by a specification. Running it over a Sparkles-filled in-memory graph gives Jena
  programs its exact behaviour, and the cost P04 §3.10 describes goes away with the bulk
  read.
- **An ontology store separate from the triples.** Axioms would need their own commits,
  history, access control and backups. Reading from the triples keeps one source of
  truth and makes every existing feature apply.
- **Using an existing Rust OWL library.** AUDIT §3 lists `horned-owl` as a reference.
  The model here must read through the store's indexes with views and budgets, cache per
  snapshot, and remove owned structures, which an external model would need adapters for
  at every point; its license and maintenance would also become dependencies. The model
  is small enough to own. The open question 1 keeps this under review.
- **Profile checks as SPARQL queries.** The profile grammars are recursive over class
  expressions, and the reports must name axioms, not triples.

## 14. Open questions

1. Should `sparkles-owl` adopt `horned-owl`'s model types (it is under the LGPL-3.0, which
   the license policy would have to allow) or stay independent? This spec assumes
   independent.
2. Should lenient mode be the default for writes too, or should an edit refuse to touch
   a graph that a strict read rejects?
3. Should `remove_entity` also remove assertions about the entity as an individual when
   it is punned?
4. Should the RL warning of §7 become an error for `reasoning=materialized` queries
   whose patterns touch axioms outside RL?
5. Should the ontology routes accept an `at=` commit, as queries do (F06)? The handle
   already reads a snapshot, so the cost is small.

## Outcome

Not built. See [ROADMAP](../ROADMAP.md) item 71.
