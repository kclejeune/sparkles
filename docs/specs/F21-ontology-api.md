# F21: An OWL 2 ontology API

> **Status:** implemented
>
> **Phases:** Phases 1 to 3 are delivered. Phase 1 provides the axiom model, reading and
> writing it through the OWL 2 mapping to RDF graphs, the OWL 2 EL, QL and RL profile
> checks and the OWL 2 DL structural restrictions, the Rust library API,
> `/$/ontology/{ds}` and `sparkles ontology`. Phase 2 provides the Python and Node.js
> surfaces, the JVM entry point that runs Jena's `jena-ontapi` over a Sparkles graph,
> reads that include materialized inferences, the OWL 2 RL warning of reasoning runs and
> the profile partition that F17's OWL 2 QL rewriting is to use. Phase 3 provides the
> functional-style syntax reader, an MCP tool and ontology diffs between commits. Roadmap
> items 540 and 541 added entity accessors, an edit builder, typed axiom constructors and
> literal checks, item 580 added OWL 2 EL classification, and items 539 and 559 extend
> the delivered phases.
>
> **User docs:** [OWL 2 ontologies](../reasoning/README.md#owl-2-ontologies) and
> [API: Ontology](../API.md#ontology).
>
> The design below is the one written before implementation. The [Outcome](#outcome)
> section records how the phases landed and where they depart from the design.

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
[P04](P04-jvm-bindings/README.md), [P05](P05-node-bindings.md)) and the library administration
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

**Non-goals.** An OWL 2 DL reasoner, or any reasoning beyond what C08, F16 and F17
provide. OWL 2 EL classification and a bridge to external OWL 2 DL reasoners are outside
the design of this spec. [ROADMAP](../ROADMAP.md) item 457 plans them under
[F24](F24-owl-classification/README.md). Manchester syntax and OWL/XML are likewise
outside this spec's design, and item 457 plans them in the syntaxes phase of this spec's
Roadmap follow-ups. Re-implementing the whole `jena-ontapi` interface set in native code.
SWRL rules are outside this spec's design. Item 456 plans DL-safe SWRL import under
[F23](F23-reasoner-core/README.md), which runs SHACL-AF natively and translates SWRL, SPIN
and N3 into its rule engine. Ontology modularization and alignment are outside this
spec's design, and item 457 plans them in the module extraction and ontology matcher
phases of this spec's Roadmap follow-ups. Versioning beyond diffs between commits.
Storing axioms anywhere other than as RDF triples in the dataset.

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
- **Strict**: an undeclared entity or a shared list cell is an unmapped triple and an
  error in the report, as an OWL 2 DL parser would treat it. A class expression, data
  range or property expression used more than once is read once, shared and listed as a
  warning, because OWL 2 DL ontologies such as the W3C test case Restriction-002 share
  them.

Punning (one IRI used as a class and an individual) is accepted in both modes, as OWL 2
allows it, and listed by the DL check of §6.4.

### 4.3 Which graphs

The handle reads one graph, the default graph, or a list of graphs. With `imports`, it
adds the graphs that `owl:imports` leads to, resolved as the reasoner resolves them
(`crates/sparkles-reasoner/src/inputs.rs`: graphs of the dataset by IRI, a location
mapping, and fetching only when the caller asks for it). Each axiom remembers the graph
it came from, so a write can target the graph that holds an axiom and a report can name
it.

The import resolution of [U12](U12-ontology-workbench/SPEC.md#import-resolution) adds two
sources of dataset graphs before and after the graph named by an import's IRI. The
dataset's import map comes first, and a C30 vocabulary resource whose namespace matches
the import comes last. Reads still fetch nothing.

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
(snapshot, graphs, mode, view) in a cache bounded by axiom count, as the reasoner bounds
the closure it keeps between runs (`sparkles_reasoner::Cache`), and is dropped at the
next commit that touches one of its graphs.

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
restriction blank node reads in both modes as two axioms and one warning. A graph where
two expressions share one list cell reads in strict mode as unmapped triples. Removing
one of the two axioms keeps the restriction and lists it as kept.

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

## Provenance

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

- **Spec:** [`F21-ontology-api.md`](F21-ontology-api.md), written on 2026-10-12 independently from:
  - the W3C OWL 2 Structural Specification and Functional-Style Syntax, OWL 2 Mapping to
    RDF Graphs and OWL 2 Profiles recommendations (second edition, 2012), and RDF 1.1
    Concepts, cited from working knowledge and not re-fetched;
  - the public documentation and Javadoc of Apache Jena's `jena-ontapi` module
    (`OntModel`, `OntModelFactory`, `OntSpecification`, the `Ont*` interfaces) and of the
    legacy `org.apache.jena.ontology` API, cited from working knowledge; no Jena code was
    read for this spec;
  - the Sparkles code (`crates/sparkles-reasoner/src/{lib.rs,diagnostics.rs,inputs.rs}`,
    `crates/sparkles/src/handles/`, `crates/sparkles/src/dataset.rs`,
    `jvm/gradle/libs.versions.toml`) and specs C02, C08, C12, C12b, C17, F16, F17, F18, P01,
    P04, P05, P06 and AUDIT §5.
- **XML literal namespace checks:** written from [OWL 2 Structural Syntax §4.8](https://www.w3.org/TR/owl2-syntax/#XML_Literals),
  its normative reference to [RDF Concepts 2004 §5.1](https://www.w3.org/TR/2004/REC-rdf-concepts-20040210/#section-XMLLiteral),
  [Namespaces in XML 1.0](https://www.w3.org/TR/REC-xml-names/),
  [XML 1.0 §§2.5, 3.3.3 and 4.1](https://www.w3.org/TR/xml/) and
  [RFC 3986](https://www.rfc-editor.org/rfc/rfc3986#appendix-A).
  RDF 1.1 Concepts §5.3 is non-normative and does not replace OWL 2's normative
  linkage here. Namespace conformance is a necessary condition. Full canonical
  lexical and value semantics remain in item 663. The existing `oxiri` dependency
  validates borrowed URI references after an ASCII check, without resolving or
  rewriting namespace values. No ontology library implementation source was read
  or copied.
- **Implementation (Phase 1):** written from the W3C OWL 2 Structural Specification,
  Mapping to RDF Graphs and Profiles recommendations (second edition), re-read for the
  implementation, and checked against the W3C OWL 2 test cases, the approved export
  `all.rdf` of the OWL Working Group's test repository (W3C Test Suite licence), which
  `scripts/fetch-test-suites.sh` fetches by checksum and which is not vendored. No code of
  Jena's `jena-ontapi` or of any other ontology library was read or copied, and no new
  third-party dependency was added.
- **Implementation (Phase 2):** written from this spec, the W3C OWL 2 Profiles
  recommendation (§4.3 and the OWL 2 RL/RDF rule tables) and the public Javadoc of
  Apache Jena's `jena-ontapi` module (`OntModelFactory.createModel`, `OntSpecification`,
  `OntModel` and `OntClass`). No Jena code was read. `jena-ontapi` 5.6.0, under the
  Apache License 2.0 as the rest of Jena, is a compile-only dependency of
  `sparkles-jena`. The jar does not bundle it, and a program that calls
  `SparklesOnt.model` adds it. It is recorded in `jvm/gradle/libs.versions.toml`, the
  Gradle lock and verification files and `nix/jvm-deps.json`.
- **Rejected** (spec §13):
  - a native implementation of `jena-ontapi`'s interfaces in the JVM binding;
  - an ontology store separate from the triples;
  - using an existing Rust OWL library;
  - profile checks as SPARQL queries.

## Outcome

Phase 1 is delivered as roadmap item 71, Phase 2 as roadmap item 537 and Phase 3 as
roadmap item 538.

### Strict materialized browser reads

The hierarchy, individual and JSON term reads accept an exact commit and optional source
pin for a fresh recorded RDFS or OWL RL materialization. They require the caller's complete
input view to match the producer's graph closure, refuse masked or synthetic views before
revealing producer metadata, and verify input and inferred-graph freshness through bounded
history comparison. A changed or cleared producer refuses a continuation. Their source
metadata binds branch, family, revision, scope and producer, with bounded input/warning
samples and unknown completeness and consistency. GET never classifies or materializes.

Materialized hierarchy pages collapse mutual-subsumption groups, remove transitive edges
and stream bounded lexical pages for classes and object/data properties. Named individual
types include the materialized indirect types, with the existing mapper and admission
limits. Term statements and axioms retain inferred-graph provenance. These reads do not
turn F18 fact witnesses into arbitrary-axiom explanations or answer from the class-only
kept classifier. General classifier-backed entity/axiom reads remain under item 588.
The exact selectors, source metadata and refusal behavior are documented in the
[ontology API reference](../API.md#ontology).

### Paged named individuals

Item 607 adds `Ontologies::individuals` and `GET /$/ontology/{ds}/individuals` for
U12's Individuals tab. The shared wire dispatcher exposes `individuals`, and the Python,
Node.js and JVM ontology handles provide the same read through their native adapters.
The read pages asserted named-type buckets, their named members
and an Untyped bucket. Direct lookup returns one individual's memberships without
walking earlier pages. Lexical IRI continuations retain the response commit through
`at=commit:N`. Every stage uses the caller's authorized graph and triple view, imports,
branch, retained revision, cancellation and deadline. No classification runs.

Responses hold at most 1,000 buckets or individuals, with 100 by default. Root discovery
and Untyped reads scan the authorized scope, and typed reads scan that type's membership.
The route uses F21's existing terminology mapper to preserve declarations, complex
assertions and signature semantics. It does not bound server work by the response size.
Item 749 covers repeated discovery, lexical-page selection and exact-total scans,
independently of item 736's cold mapping.
Root discovery refuses more than 100,000 type buckets. A typed member page reads complete
type lists only for returned rows and refuses a returned individual with more than
1,000 named types. An individual before the cursor or outside that page does not trigger
this type-list cap. Root discovery and Untyped reads apply the cap to every
individual they examine, and direct lookup applies it to its individual. These limits
fail explicitly and never return truncated type lists.
Anonymous individuals stay outside this named-entity editor tree. The exact HTTP
contract is in [the API reference](../API.md#ontology).

### Phase 1

The `sparkles-owl` crate holds the structural model of §3, the reverse mapping of §4
with lenient and strict reads, the forward mapping of §5, a functional-style writer, a
JSON form of axioms and the profile checks of §6. The library's `ontology` cargo feature
adds the handle `ds.ontology()`, which opens a snapshot's ontology, checks it, edits it
and removes entities. The server builds it by default. `GET /$/ontology/{ds}`,
`GET /$/ontology/{ds}/axioms`, `POST /$/ontology/{ds}/check` and
`POST /$/ontology/{ds}/axioms` serve it with the caller's grants through the server's
`CallerContext`. `sparkles ontology summary|axioms|check|edit` works on a database
directory or a server, and `sparkles ontology check FILE` checks a file without a
database.

The tests cover acceptance examples A1 to A5, the QL half of A6 and the graph-view half
of A8. `mise run test:owl` reads the 335 W3C OWL 2 test cases that have an RDF/XML
ontology. The DL species agree for 326 cases, the 983 EL, QL and RL memberships the cases
state agree apart from the listed differences, and the 246 ontologies in OWL 2 DL
round-trip. The 31 known differences in `crates/sparkles-owl/tests/w3c_owl_known.txt`
are cases that type entities the OWL 1 way, use anonymous individuals in EL or QL, which
their grammars do not allow, or omit the ontology header that the mapping requires.
Property tests write random axioms to triples and read them back.

### Phase 2

**One JSON surface.** `sparkles::handles::ontology::wire` holds the JSON forms of the
routes' answers, which the server, the MCP tool and every binding share. Its `call`
function reads a request in the same form and answers it. The operations are `summary`,
`axioms`, `check`, `edit`, `removeEntity` and `diff`, with the members of the routes. An
axiom argument is its JSON form or a string in functional-style syntax with the dataset's
prefixes, as the MCP tool takes it. The bindings therefore answer what the HTTP routes
answer, and a new member reaches all of them at once.

**Python.** `Dataset.ontology` is an `Ontologies` handle with `summary`, `axioms`,
`check`, `edit`, `remove_entity` and `diff`. Each takes the graph as its first argument
and the other members as keyword arguments, with `cancel` and `timeout`. `Axiom` is a
frozen axiom with structural equality and hashing. `Axiom(text)` and `Axiom.parse` read
functional-style syntax, `Axiom.from_json` reads the JSON form, `str()` writes
functional-style syntax with the standard prefixes, and `json()` and `ofn()` give the
other forms. Listings and diffs answer dicts whose entries carry an `Axiom`, and edits
take `Axiom` objects, strings or dicts. The wheel's `ontology` feature is on by default.

**Node.js.** `dataset.ontology` has `summary`, `axiomPage`, `axioms`, `check`, `edit`,
`removeEntity` and `diff`, each returning a promise except `axioms`. `axioms` is an async
iterable that reads pages of `batch` axioms (1,000 by default) from the commit of the
first page, whatever state `at` names, so that a commit made during the listing does not
mix into it. Its `timeout` bounds the whole listing. Edits and diffs pass `at` and
`inferences` on to the native call, which refuses them. Axioms are
plain objects in the JSON form, with a `type` member, and the 64-bit counts are `bigint`,
as in the other administration answers.

**JVM.** `SparklesOnt.model(dataset, graphIri, spec)` returns the `OntModel` that Jena's
`OntModelFactory` builds over a Sparkles graph with the given `OntSpecification`. The
model reads an in-memory copy of the graph that one native call, `find_all`, fills from
the thread's transaction or the head, so the per-`find` cost that P04 §3.10 describes
does not apply. A write to the model goes to the copy and to the Sparkles graph, in the
thread's write transaction when there is one, so a transaction's edits make one commit.
The copy notes the triples a transaction changes and, once the transaction has ended,
sets them to what the dataset holds, so an aborted transaction leaves the model as the
dataset is. This settling waits for nothing, and a read whose own transaction reads an
older state leaves it for later. Like Jena's in-memory models, the copy needs the
caller's locking when threads use it at once, and another thread sees a transaction's
writes before it commits. `SparklesOnt` also runs the native operations over one graph, and
`DatasetGraphSparkles.ontology()` is the full handle with `summary`, `axioms`, `check`,
`edit`, `removeEntity` and `diff`.

**Reads with inferences.** A read with `inferences` adds the graph
`urn:x-sparkles:inferred` to the graphs read while the dataset records a
materialization. The flag is `OntologyRequest::inferences` in Rust, `inferences=true` on
the summary, axioms and check routes, and an argument of that name in the bindings. An
axiom listing about an entity then includes the materialized closure, such as the
transitive superclasses of a class. `Opened::inferences` and the answers' `inferences`
member give the materialization's profile and the freshness at the state read, which is
the staleness that the `Sparkles-Inferences` header gives a query. Without a
materialization the read covers the asserted axioms and says that it included no
inferences. It never runs the reasoner. An axiom that the inferred graph holds names that
graph among its origins. Edits and diffs refuse the flag, since they change and compare
the asserted axioms.

**The OWL 2 RL warning.** A materialization under `owl-rl` reads the terminology of its
input graphs and their dataset imports in the state it reads, once it holds the writer
lock and before it derives anything. The reasoner's `ReasonOptions::inspect` hook makes
this check, so a commit that lands after the run starts and before it locks is checked
too. The check holds the lock for the time it takes, after a progress report, so a run
that yields to writers can still be superseded during it. This holds for the library, the
server's `/$/reason/{ds}` and `sparkles infer`. The run adds a warning with the count of
axioms outside the OWL 2 RL grammar and the first of them with its rule, since the rules
are complete only for ontologies in the profile (OWL 2 Profiles §4.3, Theorem PR1). A
second warning counts the axioms in OWL 2 RL whose rule the rule subset covers only up to
a size, from `crates/sparkles-reasoner/rules/owl-rl.gaps` beside the rules file. These
are property chains of more than three properties (`prp-spo2`) and keys of more than two
(`prp-key`), and a test checks the table against the rule names. The warnings are in the
run's report and in the reasoning record.

**The profile partition for F17.** `sparkles_owl::profiles::partition` splits an
ontology's axioms by one profile's grammar into those inside it and the violations of the
others. Unlike a check, it leaves out the OWL 2 DL global restrictions. It is the QL check
that F17 §7 plans in `qlrewrite.rs`, so the rewriting reads the axioms inside OWL 2 QL and
reports those set aside. The RL warning uses the same partition.

**Testing.** Library tests cover reads with and without inferences, a view that hides the
inferred graph, the RL warning of A10 with a commit that lands before the run locks, the
gap table, and every JSON operation and its
refusals. The CLI test runs A10 through `sparkles infer`. The binding tests cover each
operation in Python, Node.js and the JVM, `Axiom` equality and hashing, the batched
Node.js listing with its commit and its deadline, the refusals of inferred and historical
edits, and A9 with an aborted transaction. In A9, `OntModel.classes()` lists the classes of A1, a superclass
added inside a write transaction makes one commit, and a model over 10,000 classes and
20,000 triples is filled by one native read. The OWL 2 tests cover the partition of A6's
ontology.

### Phase 3

**The functional-style reader.** `sparkles_owl::ofn_read` reads a functional-style
document by the grammar of the structural specification. Its `Reader` reads the prefix
declarations and the header first and then yields one axiom at a time, and
`parse_axiom` reads one axiom with given prefixes. The prefixes `owl:`, `rdf:`, `rdfs:`
and `xsd:` are predefined, a literal typed `rdf:PlainLiteral` is read as the literal it
stands for, and every axiom is checked as an edit's axioms are. Expressions nest at most
128 levels, as in the reverse mapping, and errors name a line and column.

**Loading.** The loader's syntaxes gain `RdfSyntax::OwlFunctional`, selected by the
`.ofn` extension, the name `ofn` and the media type `text/owl-functional`. A document is
mapped to triples one axiom at a time by the forward mapping, so `sparkles load`,
`sparkles ontology check FILE`, the library's and the bindings' file loaders, SPARQL
`LOAD`, Graph Store writes and uploads read it. The decoded document is held in memory
while it is read, as for the other whole-document syntaxes, and the triples count
against the decompression limit as N-Quads lines, since a short axiom can map to many
triples. No writer produces the syntax, and a dump in it is refused as unsupported.

**The MCP tool.** `ontology_axioms` lists axioms and, on a server that allows updates,
adds and removes them through the handle's edit, with `dryRun`, `changes`, `ifHead` and
the commit message as `sparql_update` takes them. Provenance departs from C17 §3.2,
whose reifiers are RDF 1.2 triple terms of single triples. An axiom can be many triples,
so provenance is written the OWL 2 way, as annotations of each added axiom:
`prov:wasGeneratedBy` a `urn:uuid:` activity, `prov:generatedAtTime` and
`prov:wasDerivedFrom` the source. Its reifier is the blank `owl:Axiom` node of the
mapping, and a later call removes the axiom by its annotated form, as listings show it.
Annotation assertions on the activity give the principal, the start time, the message
and the software agent. The activity is not typed `prov:Activity`, which would be a
class assertion that reasoners materialize. The PROV-O properties are declared as
annotation properties, and an ontology that types one of them as an object or data
property, as PROV-O does, or an edit that declares one so, refuses provenance with
`provenance-conflict` rather than leave OWL 2 DL. The check reads the head before the
edit takes the writer lock, so a declaration committed in between is not seen, and
`ifHead` closes that gap. An edit is charged and scoped as a write when MCP admits it,
and a listing stops at the result byte budget with `next` naming where to resume. A removal is recorded only by the activity and the commit. Without
`--allow-update` the tool is read-only and its schema lists no edit arguments.

**Diffs.** `Ontologies::diff` and `GET /$/ontology/{ds}/diff` combine the two readings
§10 allows. The terminology of each state is read as `open` reads it, through the
caller's view and the cache, and the axiom sets are compared, since an axiom's identity
is structural and blank node labels of expressions do not matter. The assertions, which
are never read eagerly, come from F06's diff of triples: each changed triple outside the
terminology in a graph read is mapped as the assertion stream maps it. An annotated
assertion that gains or loses its annotations keeps its triple, so the diff adds the
plain assertion on the other side when the store still holds it. Both selectors are
resolved once, and the triple diff compares those two commits. The graphs each state
covers are resolved first, without reading their terminology, and when no triple of
them changed neither state is opened. The store's diff is bounded at ten million
changed quads, counted in the one graph read when a diff reads one graph and in every
readable graph otherwise. An in-memory dataset keeps only the states its snapshots and
retention hold, and a diff of a state it no longer keeps fails as `at` does.

**Testing.** The 78 functional-style ontologies of the W3C OWL 2 test cases all parse
and read back from the writer's output as the same document. Mapped to triples and read
strictly, 52 give the axioms of the case's RDF/XML form of the same role, and five
differ because of the cases' data, as `w3c_owl_known.txt` lists. The 20 premises that
have no RDF/XML form meet the DL species their cases state. Four profile memberships
differ where the cases disagree with the Profiles grammars, also listed. Property tests
write random axioms with the writer and read them back.

### Entity accessors and the edit builder

Roadmap item 540 adds the entity accessors and the edit builder of §2.1. The accessors
are methods of `sparkles_owl::read::Ontology`, so they read files as well as datasets.
`entities`, `classes`, `object_properties` and the other kinds list what the ontology
declares or a lenient read types by use, and `class(iri)` and the other lookups find one
entity. `labels`, `annotations`, `super_classes`, `sub_classes`, `equivalent_classes`,
`disjoint_classes`, `keys`, the domains, ranges, hierarchy, inverses and characteristics
of object and data properties, the hierarchy, domains and ranges of annotation
properties, datatype definitions, and the types, equalities and property assertions of
individuals answer from the axioms the read holds, sorted and each once. Each accessor
goes over the axioms once per call, without an index by entity. With an inferred read,
the answers include the materialized closure.

The store keeps assertions out of the read (§4.5), so the open ontology adds streams for
them. `Opened::instances` finds the class assertions of a class through the store's
predicate-object index, and `Opened::types_of` and `Opened::property_assertions` find an
individual's assertions by subject in each graph read. Each first answers the
assertions the read holds, such as annotated ones, and then those the store holds,
with the filter that `Opened::assertions` applies. `Opened::class` and the other
lookups answer `NotFound` for an IRI that the ontology does not have as that kind, as
§2.5 asks. An entity that only the stored assertions use is found by a probe of the
graphs read that stops at its first match. A class is found as the class of a class
assertion, a property as the property of a property or annotation assertion, and a named
individual as the subject of an assertion, the object of a mapped object-property
assertion or the object of an `owl:sameAs` or
`owl:differentFrom` assertion. The probes keep the request's deadline and cancellation.

`Ontologies::graph` and `Ontologies::graphs` start an `OntologyScope`, a builder over
`OntologyRequest` and `QueryOptions` with `imports`, `mode`, `at`, `inferences` and
`options`. It ends in `open`, `check`, `check_with`, `diff`, `remove_entity` or `edit`.
`edit` takes a closure that names additions and removals on an `Editor`, and then makes
one edit of the handle under the writer lock. The closure runs before the lock is taken,
so it cannot read the head the edit writes on.

The JSON operations add `entities` and `entity`, which `GET /$/ontology/{ds}/entities`
and `GET /$/ontology/{ds}/entity` serve. `entity` answers an IRI's kinds, labels and
annotations and, for each kind, the accessors of that kind, with each expression in
functional-style syntax and in its JSON form. With `assertions`, it streams the
instances of a class and the types, equalities, inequalities and property assertions of
an individual from the store, at most `limit` of each. Each stream reports an assertion
once for every graph that holds it. Python's `Dataset.ontology`, Node.js's `dataset.ontology`
and the JVM's `SparklesOnt` and `DatasetGraphSparkles.ontology()` have both operations.
The JVM's `SparklesOnt` also gains `removeEntity`, which the bindings table now lists.
The CLI and the MCP tool do not have the two operations, roadmap item 578.

Python's `sparkles.owl` and Node.js's `owl` namespace hold typed constructors named
after the functional-style forms, one for each of the 37 axiom types and each class
expression, data range and property expression. They build the JSON form, so Python's
answer an `Axiom` and Node.js's answer plain objects that an edit takes. Entities are
typed by their constructor, and a plain string is an IRI except where an annotation
value or a literal is expected. The library tests cover the accessors, the streams, the
builder and both operations. The binding tests cover both operations and the
constructors, and a Python test checks that every axiom type has a constructor.

### Literal checks

Roadmap item 541 adds the literal rules of the structural specification to the OWL 2 DL
check. The lexical form of each literal whose datatype is in the OWL 2 datatype map must
be in that datatype's lexical space (§5.7). The lexical spaces follow XML Schema 1.1
Part 2 for the `xsd:` datatypes, without trimming white space, with the ranges of the
integer datatypes and the `xsd:language` pattern. `owl:rational` takes the forms of §4.1,
`owl:real` has none, `rdf:PlainLiteral` is a string, `@` and an empty or well-formed
language tag. The `rdf:XMLLiteral` check requires balanced XML content, QName
components, scoped namespace declarations, the reserved `xml` and `xmlns` bindings
and unique expanded attribute names. Namespace values undergo XML attribute-value
normalization, with character references and the five predefined entities. A nonempty
normalized namespace name must be an RFC 3986 URI reference. Relative and
same-document references remain accepted, and names retain their case and percent
spelling for comparison. The check retains noncanonical fragment spellings. It does not establish the full canonical
lexical space required by OWL 2 §4.8 through RDF Concepts 2004 §5.1, or compare XML
canonical values, which remains part of item 663. Each pair of a datatype restriction
must be in the facet space of its datatype (§7.5), so the facet must be one of the datatype's normative facets and its
value must be in the value space the facet ranges over. The value is judged by its data
value, so `"3.0"^^xsd:decimal` is a valid length and an `xsd:token` is a valid pattern.
A pattern must parse as an XML Schema regular expression and a language range must
follow the grammar of RFC 4647. A pattern whose groups or subtracted classes nest more
than 250 deep is refused too, which bounds the check's recursion. A datatype that a datatype definition introduces
occurs in no literal and no restriction (§9.4). The check covers the literals of
annotations and of the headers of the ontology and its imports too, since §5.7 applies to every literal of the
ontology, and it covers streamed assertions, as the other rules of one axiom do.

The rules are part of OWL 2 DL, so every EL, QL and RL report lists their violations with
the other DL violations. The profiles' own datatype lists are subsets of the OWL 2
datatype map with the same lexical spaces, and their checks of which datatypes a profile
allows are unchanged. A literal of a datatype outside the map has no known lexical space,
and the datatype check reports the datatype instead. Edits still write ill-typed
literals, since the forward mapping can write them, and a check reports them. The W3C
OWL 2 test cases agree as before, and profile tests cover each datatype and facet rule.

### Classification

Roadmap item 580 adds OWL 2 EL classification to the handle as F24's first phase.
`OntologyScope::classify` classifies the ontology that `open` reads, and the wire
operation `classify` and `POST /$/ontology/{ds}/classify` answer it in the same JSON form
for every surface. The handle keeps one classification per dataset and reuses it
incrementally when the ontology changes. [F24's Outcome](F24-owl-classification/SPEC.md#outcome)
describes the classifier.

A classification can write its direct taxonomy into a named graph through the handle's
edit, which gained a replacement predicate and a hook that runs inside the write
transaction. The predicate removes the axioms of the previous taxonomy, and the hook
refuses the write when a graph the classification read changed after it ran.
Provenance departs from the MCP tool's form. The taxonomy is many axioms of one
activity, so the provenance is written once as annotations of the target graph's
ontology header rather than on each axiom. The header carries `prov:wasGeneratedBy` a
`urn:uuid:` activity, `prov:generatedAtTime`, `prov:wasDerivedFrom` each graph read and
an `rdfs:comment` that names the commit read, and a later write replaces them. The
message goes to the commit. A target that
declares a PROV-O property as an object or data property refuses the write with a
conflict, as the MCP tool refuses provenance, so the header stays in OWL 2 DL.

### Routes for the ontology editor

The ontology editor of [U12](U12-ontology-workbench/SPEC.md#outcome) added four things to
the HTTP surface. The `/$/ontology/{ds}` routes take `?branch=` and the `/{ds}@{branch}`
form, as the other admin routes on the branch list do, so reads and edits work on a
branch with its own `ETag`. `DELETE /$/ontology/{ds}/entity?iri=` removes an entity with
its declarations, axioms and assertions, with `dryRun`, `If-Match`, a commit message and
`receipt`, and needs write access to the graphs it reads. An edit's `add` and `remove`
take strings in functional-style syntax with the dataset's prefixes beside the JSON form,
as the MCP tool and the bindings already did. An edit's answer, and so the answer of every
binding, lists the axioms it added and removed as `addedAxioms` and `removedAxioms`, up to
10,000 of each with `axiomsTruncated`. An added n-ary axiom of which part was present is
listed whole, so the lists are not an exact inverse, and the editor undoes a commit by
reverting it instead.

### Decisions and departures from the design

**Shared expressions in strict mode.** The first design made a blank node structure used
more than once unmapped in a strict read. The W3C test case Restriction-002 shares a
restriction and is in OWL 2 DL, so §4.2 and A4 now accept a shared class expression, data
range or property expression in a strict read with a warning, which keeps the reader
compatible with the W3C suite. A shared list cell is still unmapped in a strict read.
Removing one of two axioms that share a structure keeps the structure, as A4 asks.

**The ontology header.** The Mapping §3.1.2 requires exactly one ontology header, so an
ontology without one is outside OWL 2 DL. When several nodes are typed `owl:Ontology`,
the header is the one no other ontology node points to, such as the object of
`owl:priorVersion`.

**The DL check in Phase 1.** §6.4 placed the OWL 2 DL global restrictions in Phase 3.
Phase 1 checks them, since the profile checks are subsets of OWL 2 DL and the test cases
state DL membership. The DL check covers the typing of entities, reserved vocabulary,
simple properties in cardinalities and other restrictions, regular property chains,
datatype definitions, anonymous individuals and unmapped triples. An EL, QL or RL report
also lists the DL violations.

**Profile grammars.** The QL check follows the QL grammar, which has no
`IrreflexiveObjectProperty`, although the test case New-Feature-IrreflexiveProperty-001
lists QL. EL and QL refuse anonymous individuals.

**Equivalences of one operand.** An `EquivalentClasses` or `SameIndividual` of one
operand, as `owl:equivalentClass` from a class to itself reads, is accepted. A
disjointness axiom still needs two operands, and an edit that adds one with fewer is
refused with code `invalid-axiom`.

**The Rust API.** The handle takes request structures, `OntologyRequest`,
`CheckRequest` and `EditRequest`, with `QueryOptions` for the caller's grants and
deadline. Roadmap item 540 adds the builder, the closure and the entity accessors of
§2.1 over them, as the section [Entity accessors and the edit builder](#entity-accessors-and-the-edit-builder)
describes.

**Imports.** A read follows `owl:imports` only to graphs of the dataset that the caller
can read. The server's reads add the dataset's import map and the resource catalog's
vocabularies as sources of those graphs, as U12's
[import resolution](U12-ontology-workbench/SPEC.md#import-resolution) describes, and the
summary lists the closure. Fetching is refused, because a read route would otherwise make
outbound requests. The editor's explicit fetch stores a remote import as a graph, and
roadmap item 559 adds fetching during a read through the outbound policy.

**Caching.** The handle keeps the last four reads of up to two million triples each. An
entry is keyed by commit, graphs, imports and mode, so any commit replaces it, and only
callers who can read every graph share it. The size budget of §4.5 and its `507` answer
are not enforced, and reads are bounded by the query deadline. Roadmap item 539
adds the budget and the A7 measurement on LUBM.

Retained-state preparation still uses the legacy resolution, cache, generation-open and
replay paths. It polls primary cancellation and deadlines, while an additional composed
cancellation source is checked around preparation. The strict materialized freshness
query and subsequent index/hash work poll that composition themselves. A field forwarded
only into one helper would leave legacy cold generation decoding unchecked. Item 539
also owns complete control propagation through preparation and request/server admission
for that work, preserving exact retained revisions and access semantics. These reads do
not promise fully interruptible cold historical preparation.

**Assertions.** A check covers all the assertions the store holds by default, not only
the restricted forms, and `assertions: false` restricts it to the terminology. The
assertions stream from the store and are never held by the ontology.

**Removal scope.** A removal edits the graphs the request names and the graph the
edit names as its target. An axiom read only from an imported graph is refused with
`invalid` until the edit names that graph, as §5.4 asks. A structure stays when a triple
of the graphs read refers to it, and the main triple of a removed axiom goes even when
its subject is an expression another axiom shares.

**Nesting.** A read follows class expressions and data ranges 128 levels deep. A deeper
structure stays unmapped with the error diagnostic `nesting`, so that a long chain of
blank nodes cannot exhaust the stack.

**Removal.** A removal deletes only the declarations it names. `remove_entity` names the
entity's declarations, the axioms that mention it and the assertions about it, which
answers open question 3 for punned IRIs by removing both uses.

**Commits.** The routes accept `at=` to read an earlier commit, which answers open
question 5. An edit reads the ontology from the head it writes on, under the writer lock.

**Unchecked.** A8 is tested with graph views, not with C12b triple protections.

The parity test of the library administration API (P06) maps each route to its library
call and to its Python, Node.js and JVM names.

**Binding surfaces.** §2.2 sketched `ds.ontology(graph, imports=True)` returning an
ontology and `ds.ontology_edit(graph)` as a context manager. The bindings mirror the Rust
handle instead, as the routes do. A handle on the dataset takes the graph and the other
members with each operation and answers in the routes' JSON forms. Python has one frozen
`Axiom` class, read from functional-style syntax or the JSON form, rather than a class
per axiom type, so that an axiom is written as the recommendation writes it. Node.js
axioms keep IRIs as strings in the JSON form rather than RDF/JS terms, so that they are
the objects the HTTP client sends and receives. Roadmap item 540 adds typed axiom
constructors that build these forms.

**The JVM model's copy.** The copy that `SparklesOnt.model` reads is filled once, so a
commit made through another handle after the model is built is not seen by it, except
for the triples a transaction of the model wrote. Those are read back from the dataset
once that transaction ends, so that an abort leaves the model as the dataset is, and the
read then shows later commits to them too. A program builds a new model to read later
commits.

**Inferences of other graphs.** The inferred graph holds the inferences of every input of
the materialization, so a general ontology read with inferences of one graph also reads
what the run derived from the other graphs it read. Strict materialized browser reads
refuse that ambiguous scope instead of displaying cross-input consequences.

**F17's QL check.** F17's `owl-ql` mode is not built, roadmap item 19. Phase 2 provides
the partition that its rewriting reads and the QL half of A6, and the fallback half of A6
is tested with F17's mode.

### Scope changes

**OWL 2 EL classification.** The non-goal in §1 excluded OWL 2 EL classification. Roadmap item 580 delivers it as [F24](F24-owl-classification/README.md)'s first phase, and the [Classification](#classification) section above describes how it reaches this spec's surfaces.

**External OWL DL reasoner bridge.** The non-goal in §1 excluded an OWL 2 DL reasoner and anything beyond C08, F16 and F17. A bridge to external DL reasoners is now planned under item 457 and [F24](F24-owl-classification/README.md). A native DL reasoner stays a non-goal.

**Manchester and OWL/XML syntaxes.** The non-goal in §1 excluded Manchester syntax and OWL/XML. Readers and writers for these and for functional syntax and OBO are now planned under item 457 as the syntaxes phase of this spec's Roadmap follow-ups.

**Module extraction and alignment.** The non-goal in §1 excluded ontology modularization and alignment. Module extraction and an ontology matcher with SSSOM output and merge review are now planned under item 457 in this spec's Roadmap follow-ups.

**SWRL import and rule frontends.** The non-goal in §1 excluded SWRL rules. Item 456 and [F23](F23-reasoner-core/README.md) now plan DL-safe SWRL import, SPIN and N3 translation into the rule engine, and native SHACL-AF rules.

### Roadmap follow-ups

#### Ontology workbench workstream

These phases belong to [ROADMAP](../ROADMAP.md) item 457, the ontology workbench workstream. Each phase gets its own numbered item when it is ready to start.

**OWL and SHACL translation.** Generate SHACL from domains, ranges, restrictions and cardinalities under a closed-world reading, summarize shapes as OWL, translate ShEx schemas to SHACL where the constructs correspond, and install the result as a guard like drafted shapes. It bridges the ontology and the shapes and forms stack that already exists. It depends on item 71.

**Ontology refactoring operations.** Rename or change IRIs everywhere including RDF 1.2 triple terms, merge entities, move axioms between ontologies, deprecate with replacement and delete with usage check, each as a previewed commit. These are daily ontology maintenance tasks. Rename can ship first over SPARQL. It depends on item 71.

**The F21 axiom model as the workbench base.** Item 71 built the F21 Phase 1 axiom model, the reverse and forward mappings, the profile checks and `/$/ontology/{ds}` in the `sparkles-owl` crate. Almost every ontology editing, refactoring, diff, documentation and classification phase depends on it, so it is the root of this workstream.

**Functional, Manchester, OWL/XML, OBO and OBO Graphs JSON syntaxes.** Add readers and writers for OFN, Manchester, OWL/XML, OBO 1.4 and OBO Graphs JSON in sparkles-owl, registered with the loader, formatter, linter and LSP, with a Manchester class-expression editor mode. Ontologies are distributed in these formats and Protégé users write Manchester. `sparkles_owl::manchester` reads Manchester class expressions and one-line axioms against an ontology's names and labels for the ontology editor and `POST /$/ontology/{ds}/parse`, and whole Manchester documents remain. It depends on item 71.

**SSSOM mapping sets.** Import and export SSSOM TSV as RDF and review mapping sets in the UI. Crosswalks between vocabularies have no support.

**Axiom-level and hierarchy-level diffs.** Diff axioms between commits or branches rendered in Manchester or OFN with labels, and classify changes as effectual or ineffectual from entailment diffs. Ontology review needs semantic diffs, not triple diffs. It depends on item 71.

**Module extraction and import refresh.** Extract syntactic locality modules (top, bot, star) and MIREOT term lists into import graphs and refresh them against a new source version. It is the standard OBO way to manage imports. It depends on item 71.

**Ontology release pipeline.** Run checks, reason, produce ODK base, full and simple artifacts in chosen syntaxes, bump the version IRI, take a named snapshot and publish files as a durable job from a branch head. It follows ODK and OBO practice and makes Sparkles the ontology build system. It depends on item 339.

**ROBOT templates and DOS-DP patterns.** Read ROBOT template strings and DOS-DP YAML patterns over CSV or TSV to generate axioms, using the Manchester parser. CSVW, Tarql and RML produce triples, not axioms. It depends on item 71.

**Ontology matcher and mapping review.** Combine lexical matching over labels and synonyms, embedding similarity and structural propagation, output SSSOM, and repair incoherent mappings with the classifier. Matches are exchanged as SSSOM and enter the ontology through merge review. Alignment is a common task with no store-integrated tool. It depends on items 71 and 396.

**LinkML import and export.** Read LinkML YAML into OWL and SHACL graphs and export classes and shapes as LinkML. It bridges to the LinkML ecosystem and its generators.

**Annotator endpoint.** Return text spans and term IRIs from labels and synonyms of selected ontologies at POST `/$/annotate/{ds}`. It matches the BioPortal Annotator over the existing entity linking.
