# G03: SHACL Compact Syntax and SHACL 1.2 list constraints

> **Status:** implemented in part (Phase 1 and two of the later items)
>
> **Phases:** Phase 1 is the SHACLC reader and writer in `sparkles-shacl`, SHACLC wherever
> shapes are read or shown, and the four SHACL 1.2 list constraint components. Of the
> later items of §9, `sparkles shacl parse` converts shapes between syntaxes and the MCP
> tool `draft_shapes` returns SHACLC. A comment-preserving SHACLC formatter is not built.
>
> **User docs:** [API: SHACL Compact Syntax](../API.md#shacl-compact-syntax-shaclc) ·
> [API: SHACL validation](../API.md#shacl-validation) ·
> [Usage: drafting shapes](../USAGE.md#drafting-shapes-from-the-data) ·
> [Features](../FEATURES.md#reasoning-validation-and-schema)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This spec depends on the SHACL validator in `crates/sparkles-shacl`,
on write-time validation ([C10](C10-write-time-validation.md)), on the shapes drafted
from the data ([C02 §11](C02-schema-discovery.md)) and on the MCP server
([C11](C11-mcp-server.md)).

## 1. Summary, goals, non-goals

Apache Jena's `jena-shacl` registers the SHACL Compact Syntax (SHACLC) as an RDF syntax.
Jena reads `text/shaclc` documents into shapes graphs, writes shapes graphs back as
SHACLC, and its `shacl parse --out=compact` command prints a shapes file in it. Sparkles
reads shapes only as RDF. This feature adds SHACLC as a second way to write a shapes
graph, everywhere Sparkles accepts or shows one.

The current SHACL 1.2 Core editor's draft also adds four list constraint components:
`sh:memberShape`, `sh:minListLength`, `sh:maxListLength` and `sh:uniqueMembers`. Jena
implements them. Sparkles already implements `sh:targetWhere` from the same draft, so the
list components are added to the validator in the same change.

**Goals**

* **A faithful reader.** Parsing a SHACLC document produces the graph the production
  rules of the SHACL Compact Syntax describe. Every test pair of the Working Group's test
  suite parses to a graph isomorphic to its Turtle.
* **A faithful writer.** Writing a shapes graph as SHACLC either produces a document that
  parses back to an isomorphic graph, or fails and names the triples SHACLC cannot
  express. The writer never drops a triple silently.
* **SHACLC everywhere shapes go in.** `POST /{ds}/shacl`, the write-time validation
  configuration, `sparkles shacl --shapes`, `sparkles validation --shapes`, the MCP tool
  `validate_shacl` and the UI's validation panel accept SHACLC. Files are recognized by
  the `.shaclc` and `.shc` extensions, and request bodies by the `text/shaclc` media type.
* **SHACLC where shapes come out.** The drafted shapes of C02 can be had as SHACLC from
  `GET /$/schema/{ds}/shapes` and `sparkles schema --draft-shapes`, and the UI's draft
  dialog shows them.
* **List constraints as the draft defines them**, with the results the SHACL 1.2 test
  suite expects, and incremental write-time validation that stays exact for them.

**Non-goals**

* A comment-preserving SHACLC formatter in `sparkles fmt`. The formatter handles SPARQL
  and RDF syntaxes, and shapes written in Turtle are formatted as Turtle. It has no other
  shape syntax, ShExC included. Formatting SHACLC is left for a later phase (§9).
* SHACLC as a general RDF syntax for data endpoints. Graph Store uploads and `LOAD` stay
  limited to the RDF syntaxes, because a SHACLC document can only express shapes.
* Extending SHACLC beyond what Jena reads, except for the list parameters (§3.3).

## 2. User-visible behaviour

### 2.1 Reading

| Place | How SHACLC is chosen |
|---|---|
| `POST /{ds}/shacl` | `Content-Type: text/shaclc` |
| `PUT /$/validation/{ds}` | `"shapes": { "inline": "…", "format": "text/shaclc" }` |
| `sparkles shacl --shapes F` | `F` ends in `.shaclc` or `.shc`, optionally with a compression suffix such as `.gz` |
| `sparkles validation --shapes F` | as for `sparkles shacl` |
| MCP `validate_shacl` | a new optional argument `shapesFormat`, `turtle` (the default) or `shaclc` |
| UI validation panel | a syntax selector next to the shapes editor, Turtle or SHACLC |

A SHACLC syntax error is reported like an RDF syntax error in the same place. That is a
`400` over HTTP, a `syntax` tool error over MCP, and exit status 2 with a message on the
CLI. The message gives the line and column.

Inline shapes of a write-time validation configuration are stored in the database as
`validation-shapes.ttl`. When they were given in another syntax, SHACLC included, the
stored file is the parsed graph written as Turtle with the document's prefixes, so the
file stays Turtle whatever the input was. The configuration does not remember the input
syntax.

### 2.2 Writing

| Place | How SHACLC is chosen |
|---|---|
| `GET /$/schema/{ds}/shapes` | `format=shaclc`, or `Accept: text/shaclc` |
| `sparkles schema --draft-shapes` | `--format shaclc` |
| UI draft dialog | a SHACLC tab next to SHACL and ShEx |

The drafted SHACLC carries the same counts as comments as the Turtle draft does. The
JSON draft gains a `shaclc` field next to `shacl` and `shex`.

### 2.3 Library API

```rust
/// The syntax of a shapes document: an RDF syntax, or SHACLC.
pub enum ShapesSyntax { Rdf(RdfFormat), Compact }

impl ShapesSyntax {
    pub fn from_media_type(mt: &str) -> Option<ShapesSyntax>;
    pub fn from_path(path: &Path) -> Option<(ShapesSyntax, Option<Codec>)>;
    pub fn media_type(self) -> &'static str;
}
impl From<RdfFormat> for ShapesSyntax { … }

// existing functions take any syntax
Shapes::parse(text, impl Into<ShapesSyntax>, base) -> Result<Shapes>;
Shapes::read_graph(text, impl Into<ShapesSyntax>, base) -> Result<Graph>;

pub mod compact {
    pub struct Document { pub graph: Graph, pub prefixes: Vec<(String, String)>,
                          pub base: Option<String> }
    pub fn parse(text: &str, base: Option<&str>) -> Result<Document, SyntaxError>;
    pub fn write(graph: &Graph, prefixes: &[(String, String)]) -> Result<String, NotCompact>;
}
```

Existing callers that pass an `RdfFormat` compile unchanged.

### 2.4 List constraints

A property shape or a node shape may use the four parameters below. Each value node must
be a SHACL list, and a value node that is not one gets a result with the value node as
`sh:value`, whichever parameter applies.

| Parameter | Value | A value node that is a list gets a result when |
|---|---|---|
| `sh:memberShape` | a shape | a member does not conform to the shape |
| `sh:minListLength` | a non-negative `xsd:integer` | it has fewer members |
| `sh:maxListLength` | a non-negative `xsd:integer` | it has more members |
| `sh:uniqueMembers` | an `xsd:boolean` | the value is `true` and a member occurs twice |

A `sh:memberShape` result carries one `sh:detail` per member that does not conform. The
details are the results of validating that member against the member shape. A
`sh:uniqueMembers` result carries one `sh:detail` per duplicated member, with the
member as `sh:value`. The validation report gains `sh:detail` in Turtle and a `details`
array in JSON, present only when a result has details.

## 3. Standards basis

### 3.1 SHACL 1.2 Core (editor's draft)

The editor's draft at the Data Shapes Working Group's GitHub pages, fetched on
2026-10-02, defines a *SHACL list* in its terminology section. A SHACL list in a graph G
is an IRI or a blank node. It is either `rdf:nil`, provided `rdf:nil` has no value for
`rdf:first` or `rdf:rest`, or it has exactly one value for `rdf:first` and exactly one
value for `rdf:rest` that is itself a SHACL list, and it does not reach itself over
`rdf:rest+`. Its members are its `rdf:first` value followed by the members of its
`rdf:rest` value.

The section "List Constraint Components" defines the four components of §2.4, with the
component IRIs `sh:MemberShapeConstraintComponent`, `sh:MinListLengthConstraintComponent`,
`sh:MaxListLengthConstraintComponent` and `sh:UniqueMembersConstraintComponent`. The
textual definitions say that every value node must be a SHACL list, so a value node
that is not one is a result even for `sh:uniqueMembers false`. The non-normative text
asks for `sh:detail` per failing member of `sh:memberShape` and per duplicate of
`sh:uniqueMembers`. It also says that an ill-formed list is reported at the top level,
without validating its members.

The SHACL 1.2 test suite in the same repository has eight tests for these components,
`core/node/*-001.ttl` and `core/property/*-001.ttl`. They are the acceptance tests of
§7. Two of their cases settle readings the text leaves open. A list node may have
properties besides `rdf:first` and `rdf:rest`, and a list is still well formed. An IRI
with no triples at all is not a SHACL list.

### 3.2 SHACL Compact Syntax

The SHACL 1.2 Compact Syntax editor's draft and the SHACL 1.0 Compact Syntax Working
Group Note give the same grammar and the same production rules. The 1.2 draft adds the
IANA registration of `text/shaclc` with the file extensions `.shaclc` and `.shc`. The
grammar is an ANTLR grammar of directives (`BASE`, `IMPORTS`, `PREFIX`) followed by
`shape` and `shapeClass` blocks. A block holds constraints, each ended by `.`. A
constraint is either one or more node parameters (`closed=true`), optionally negated
with `!` and joined into `sh:or` with `|`, or a property shape: a SPARQL-like path,
then cardinalities (`[1..*]`), types, node kinds, shape references (`@ex:S`), nested
bodies and parameters.

The production rules map each construct to triples. These points matter for the
implementation:

* The parser starts with the prefixes `rdf`, `rdfs`, `sh` and `xsd` bound.
* After the document, the parser produces `?baseURI rdf:type owl:Ontology` and one
  `owl:imports` triple per `IMPORTS`. It reports an error when there are imports but no
  base.
* A property type is `sh:datatype` when the IRI is "one of the RDF datatypes supported by
  SPARQL 1.1", and `sh:class` otherwise.
* `[0..*]` produces no triples. Any other minimum or maximum produces `sh:minCount` or
  `sh:maxCount`.
* Arrays become RDF lists, and alternative and sequence paths become the SHACL path
  lists.
* Each test of the suite is a `.shaclc` file and a `.ttl` file. Parsing the first must
  produce a graph isomorphic to the second.

### 3.3 Apache Jena (Apache-2.0)

`jena-shacl` reads SHACLC with a JavaCC grammar (`shaclc/shaclc.jj`) and writes it from
its parsed shapes model (`compact/writer`). Its tests run the Working Group's suite from
`src/test/files/shaclc-valid` with the base `urn:x-base:default`, and three syntax
tests from `src/test/files/local/shaclc-syntax`. Reading these sources settled the
following.

* Jena treats the keywords as case-insensitive.
* It decides "RDF datatype" as any IRI in the `xsd:` namespace, plus `rdf:langString`,
  `rdf:HTML`, `rdf:JSON` and `rdf:XMLLiteral`. Sparkles adopts the same rule, so the two
  produce the same graphs.
* It accepts three extensions. A shape reference may stand alone in a node shape body
  (`@ex:S .`, producing `sh:node`). `targetClass` is a node parameter. `group`,
  `order`, `name`, `description` and `defaultValue` are property parameters. Sparkles
  reads all three, so files Jena reads are read by Sparkles too.
* Its writer prints `memberShape=…`, `minListLength=…`, `maxListLength=…` and
  `uniqueMembers=…` for the list constraints, but its grammar does not read them back.
  Sparkles accepts the four as node and property parameters, so that every shapes graph
  that uses them can be written and read again. This is the one extension Sparkles adds
  to what Jena reads.
* Jena's writer works from the parsed shapes, so triples outside its model are lost.
  Sparkles writes from the graph and refuses graphs it cannot express (§5).

## 4. Reader

The reader is a hand-written lexer and recursive-descent parser in
`crates/sparkles-shacl/src/compact/`. It follows the grammar of §3.2 with the Jena
extensions of §3.3, and builds triples directly by the production rules.

* **Tokens.** IRIs, prefixed names, `@`-prefixed names, strings in the four Turtle
  forms with their escapes, language tags, the three numeric forms, booleans, and the
  punctuation of the grammar. Comments run from `#` to the end of the line. A byte order
  mark is skipped. Keywords are matched without regard to case.
* **IRIs.** Relative IRIs resolve against the current base with `oxiri`. A prefixed name
  with an undeclared prefix is an error. Local names follow Turtle's `PN_LOCAL`,
  including `%` escapes and backslash escapes.
* **Literals.** Strings follow Turtle's escapes, with language tags or `^^datatype`.
  Numbers keep their lexical form with `xsd:integer`, `xsd:decimal` or `xsd:double`, and
  booleans become `xsd:boolean`, as Turtle does.
* **Base and ontology.** The base starts as the base given to the parser. `BASE` replaces
  it. At the end, when a base is known, the parser adds `<base> rdf:type owl:Ontology`
  and the `owl:imports` triples. With imports and no base it fails, as the Note says.
  When no base is known and there are no imports, no ontology triple is produced. Jena
  instead makes one up with the base `urn:x-base:default`. The test harness passes that
  base, as Jena's tests do, so the suite's graphs match.
* **Parameters.** The node parameters and the property parameters are the lists of the
  grammar plus the extensions of §3.3. An unknown name is an error that lists the
  parameter names, rather than a triple with an unknown predicate.
* **Counts.** `[m..n]` requires integers. A maximum below the minimum is not an error,
  because it is a valid if unsatisfiable shapes graph.
* **Errors** give the line and column of the offending token and what was expected.

Every blank node the reader produces is fresh, so reading the same document twice gives
graphs that share no blank nodes. The reader also returns the prefixes and the final
base, for writing the graph out again.

## 5. Writer

The writer works on a graph and a prefix list. It walks the graph from its shapes and
claims each triple a SHACLC construct produces. When it is done, every triple must have
been claimed. Otherwise it fails with `NotCompact`, which lists up to ten unclaimed
triples. Callers that only show shapes (the draft) never hit this, because their graphs
are built to be expressible.

1. **Ontology.** An IRI typed `owl:Ontology` whose other triples are all `owl:imports`
   of IRIs becomes `BASE <iri>` and the `IMPORTS` lines. At most one such IRI is allowed.
2. **Top-level shapes.** Every IRI typed `sh:NodeShape` becomes a block. With
   `rdf:type rdfs:Class` as well it is a `shapeClass`, otherwise a `shape`. The
   `sh:targetClass` values of a `shape` go after `->`. Those of a `shapeClass` become
   `targetClass=` parameters.
3. **Node shape bodies.** In a body, each triple whose predicate is a node parameter
   becomes `param=value .`, where the value is an IRI, a literal or an array. An
   `sh:property` to a blank node becomes a property line. An `sh:node` to an IRI
   becomes `@iri .`. An `sh:not` to a blank node with a single parameter becomes
   `!param=value .`. An `sh:or` whose list members are blank nodes with a single
   parameter each becomes `a=1|b=2 .`.
4. **Property shapes.** A blank node becomes `path` followed by its atoms. `sh:minCount`
   and `sh:maxCount` become one `[m..n]`. `sh:datatype` and `sh:class` become the bare
   IRI when the reader would map the IRI back to the same predicate, and `datatype=` or
   `class=` otherwise. `sh:nodeKind` becomes the bare kind. `sh:node` becomes `@iri` for
   an IRI and a nested `{ … }` body for a blank node. `sh:not` and `sh:or` become `!`
   and `|` over single atoms. Every other parameter becomes `param=value`.
5. **Paths** are written with the fewest parentheses that keep their structure.
   Alternatives bind loosest, then sequences, then `^` and the modifiers.
6. **Sharing.** A blank node a construct claims must have exactly one incoming triple,
   and the claim must not revisit it. Shared or cyclic blank nodes cannot be written,
   because the reader makes a fresh one for every occurrence.
7. **Exact forms.** A count must be an `xsd:integer` whose lexical form is a decimal
   integer, because the reader produces exactly that. `sh:minCount 0` cannot be written,
   because `[0..n]` produces no `sh:minCount`. Array members must be IRIs or literals.
   A list node must have exactly `rdf:first` and `rdf:rest`.
8. **Terms.** IRIs are written as prefixed names when a prefix fits and the local name
   needs no escapes, and in full otherwise. They are never written relative to the
   base. Literals are written as Turtle would write them. Integers, decimals, doubles and
   booleans are bare when their lexical form is a valid token of that kind.
9. **Prefixes.** The writer prints a `PREFIX` line for each given prefix that it uses,
   except `rdf`, `rdfs`, `sh` and `xsd` when they have their standard namespaces.

## 6. List constraints in the validator

* **Parsing.** `sh:memberShape` takes a shape, as `sh:node` does. The lengths must be
  non-negative integers and `sh:uniqueMembers` a boolean, or the shapes graph is
  rejected, as for the other parameters.
* **Lists.** One function walks a value node as a SHACL list over the data graph, with
  index scans for `rdf:first` and `rdf:rest`. It returns the members, or nothing when
  the node is a literal, has zero or several `rdf:first` or `rdf:rest` values, ends
  anywhere but `rdf:nil`, or loops. It stops after 2²⁴ members and treats a longer list
  as ill-formed, so a hostile list cannot exhaust memory.
* **Results.** Each value node that is not a list gets a result. Otherwise each component
  checks its condition and reports one result for the value node. `sh:memberShape`
  checks every member with the conformance probe used by `sh:node`. When results are
  collected, it validates each nonconforming member again in collecting mode for the
  details.
* **Incremental validation (C10 §6.2).** A list component reads the `rdf:first` and
  `rdf:rest` edges of every node that `path/rdf:rest*` reaches from the focus node. Those
  are two dependencies, with the prefix `path/rdf:rest*`. `sh:memberShape` also nests
  the member shape's dependencies under the prefix `path/rdf:rest*/rdf:first`.
* **Candidates.** The list components do not narrow the nodes that may conform to a
  shape, so a `sh:targetWhere` shape with only list constraints considers every node.

## 7. Testing

* **The SHACLC suite.** A harness reads every `.shaclc` and `.ttl` pair from Jena's
  copy of the Working Group's suite, as `w3c_shacl.rs` reads the SHACL suite from the
  Jena checkout. It skips the suite when the checkout is absent. Each pair must parse to
  isomorphic graphs with the base `urn:x-base:default`. The same pairs also go through
  the writer, by parsing the Turtle, writing SHACLC, parsing that, and comparing.
* **Jena's syntax tests.** `nodeParams.shc` and `propertyParams.shc` must round-trip,
  and `nodeParam-bad-01.shc` must fail.
* **Unit tests** cover each token form, the error positions, the ontology rules, every
  writer refusal of §5, and the list-parameter extension.
* **The SHACL 1.2 list tests.** The eight test files are vendored into
  `crates/sparkles-shacl/tests/shacl12/` with their license and source, and
  `w3c_shacl.rs` runs them as a second suite that is never skipped. The harness compares
  results as it does for SHACL 1.0, without `sh:detail`. A unit test checks the details
  separately.
* **Incremental validation.** The guard's differential test, which checks incremental
  results against full validation after random writes, gets list shapes.
* **Draft.** A test checks that the drafted SHACLC parses to a graph isomorphic to the
  drafted Turtle.
* **Server and UI.** A server test posts SHACLC to `/{ds}/shacl` and sets a SHACLC
  write-time configuration. A UI unit test covers the syntax switch.

## 8. Acceptance examples

**A1.** This document parses to the graph of the complex example of the Note.

```
BASE <http://example.com/ns>
IMPORTS <http://example.com/person-ontology>
PREFIX ex: <http://example.com/ns#>

shape ex:PersonShape -> ex:Person {
    closed=true ignoredProperties=[rdf:type] .
    ex:ssn       xsd:string [0..1] pattern="^\\d{3}-\\d{2}-\\d{4}$" .
    ex:worksFor  IRI ex:Company [0..*] .
    ex:address   BlankNode [0..1] {
        ex:city xsd:string [1..1] .
        ex:postalCode xsd:integer|xsd:string [1..1] maxLength=5 .
    } .
}
```

**A2.** `curl -H 'Content-Type: text/shaclc' --data-binary @shapes.shaclc
localhost:3030/ds/shacl` validates the dataset `ds` against the shapes and answers the
same report as the Turtle form of the shapes would.

**A3.** `sparkles validation --loc db --mode reject --shapes person.shaclc` installs
write-time validation, and `validation-shapes.ttl` in the database holds the shapes as
Turtle.

**A4.** Writing a graph with `ex:S sh:property [ sh:path ex:p ; sh:minCount 0 ]` fails
with `NotCompact` naming the `sh:minCount 0` triple.

**A5.** With `ex:S a sh:NodeShape ; sh:targetNode ex:l ; sh:uniqueMembers true` and
`ex:l rdf:first 1 ; rdf:rest (2 1)`, validation reports one result with
`sh:focusNode ex:l`, `sh:value ex:l` and one `sh:detail` with `sh:value 1`.

**A6.** In SHACLC, `ex:speakers IRI [1..1] memberShape=ex:Speaker maxListLength=10 .`
parses to a property shape with `sh:memberShape ex:Speaker` and `sh:maxListLength 10`,
and the graph writes back to the same line.

## 9. Phasing

Phase 1 is everything above. A later phase may add a lossless SHACLC formatter to
`sparkles fmt`, `POST /$/format` and the editor, if users ask for it. It would follow
the X02 design and keep comments. Converting between SHACLC and Turtle in the CLI is
also left for later, unless a generic `shacl parse` command appears first.

## 10. Rejected alternatives

* **Writing from the parsed `Shapes` model, as Jena does.** The model drops triples it
  does not interpret, such as labels on shapes, so a write could lose data without
  telling. Writing from the graph keeps the guarantee of §1.
* **Writing what can be written and skipping the rest with a comment.** Jena has this as
  an unused option. A shapes file that silently differs from its graph is worse than an
  error, and the callers that show shapes build expressible graphs anyway.
* **Storing inline SHACLC as written.** The stored shapes file would then need a format
  field and a second file name. Converting to Turtle keeps one file format and also fixes
  inline JSON-LD and RDF/XML, which were stored under a `.ttl` name before.
* **A parser generator.** The grammar is small. A hand-written parser gives better
  errors and needs no new dependency, as the ShExC parser of G02 showed.
* **Treating `text/shaclc` as an RDF syntax in `sparkles::io`.** The RDF format type is
  `oxrdfio::RdfFormat`, which cannot gain a variant, and data endpoints should not accept
  shapes documents.

## 11. Sources

* SHACL 1.2 Core, W3C Editor's Draft, Data Shapes Working Group, fetched 2026-10-02.
  W3C Software and Document License.
* SHACL 1.2 Compact Syntax, W3C Editor's Draft, fetched 2026-10-02, and its test
  directory.
* SHACL Compact Syntax, W3C Working Group Note draft (SHACL 1.0), and its test suite.
* The SHACL 1.2 test suite of the Data Shapes Working Group, the list constraint tests.
  W3C Software and Document License.
* Apache Jena `jena-shacl` (Apache-2.0): `shaclc/shaclc.jj`, `compact/reader`,
  `compact/writer`, the `List*` constraints and the SHACLC tests. Read for behaviour.
  No code was copied.

## Provenance

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

- **Spec:** [`G03-shaclc.md`](G03-shaclc.md), written on 2026-10-02 independently from:
  - the Sparkles code and specs C02, C10 and C11;
  - the SHACL 1.2 Core and SHACL 1.2 Compact Syntax editor's drafts and the SHACL 1.0
    Compact Syntax Working Group Note, fetched from the Working Group's pages. They are
    under the W3C Software and Document License;
  - the SHACL Compact Syntax test pairs, as copied into Apache Jena, and the SHACL 1.2
    list constraint tests of the Working Group's test suite;
  - Apache Jena's `jena-shacl` sources (Apache-2.0): the SHACLC grammar, reader, writer,
    list constraints and tests, read for behaviour. No code was copied.
- **Implementation** (2026-10-02): from the spec and the Sparkles code.
  - The lexer and parser (`compact/lex.rs`, `compact/read.rs`) are hand-written from the
    grammar and production rules of the Compact Syntax drafts. Jena's JavaCC grammar and
    `ShaclCompactParser.java` were read for the extensions and the datatype rule. No
    code was copied.
  - The writer (`compact/write.rs`) is written from spec §5. It does not follow Jena's
    writer, which works from the parsed shapes.
  - The list constraints follow the SHACL 1.2 Core draft's textual definitions and its
    definition of a SHACL list. Jena's `List*` constraint classes were read for behaviour.
  - **Dependencies:** none new. `sparkles-shacl` now uses `oxiri`, already in the
    workspace.
  - **Test data:** the eight SHACL 1.2 list tests are vendored in
    `crates/sparkles-shacl/tests/shacl12/` under the W3C Software and Document License,
    with a README. The SHACLC test pairs and Jena's syntax tests are read from the Jena
    checkout, not vendored.
- **Rejected** (spec §10):
  - writing SHACLC from the parsed shapes model;
  - skipping what cannot be written;
  - storing inline SHACLC as written;
  - a parser generator;
  - SHACLC as a general RDF syntax.

## Outcome

**Delivered.** The spec was written on 2026-10-02, and Phase 1 landed the same day as
designed in §2–§7. The reader and the writer are in `sparkles_shacl::compact`, with
`ShapesSyntax` and the conversion helpers in `sparkles_shacl::syntax`. `Shapes::parse` and
`Shapes::read_graph` take any syntax, so their callers compile unchanged. The list
components are in the validator, the report has `sh:detail`, and incremental write-time
validation reads the lists. The only new dependency is `oxiri` for `sparkles-shacl`. It
was already a workspace dependency.

**Deviations from the spec.**
* **The Python bindings** gained `Dataset.validate_shacl(..., format="shaclc")`, which §2.1
  did not list. The MCP tool `draft_shapes` returned Turtle or ShExC only at first. It
  now takes `shapesFormat: "shaclc"` for a SHACL draft in the compact syntax.
* **The writer's order of atoms.** A property line is written as its path, the node kind,
  the type, the count, shape references, parameters, `!` and `|`, and nested bodies last.
  This follows the Note's examples (`ex:ssn xsd:string [0..1] pattern=…`). §5 did not fix
  an order.
* **The UI** offers the syntax as a select next to the Format button rather than as tabs.
  Switching the syntax does not convert the editor's text. It only swaps the example
  shapes when they are untouched. The Format button is disabled for SHACLC, because the
  formatter has no SHACLC.
* **The test harness** compares results without `sh:detail`, and so does the guard's
  differential test, because the guard matches results without their details. Unit tests
  in `tests/components.rs` check the details.

**Chosen during implementation** (open to revision).
* An unknown `format` of inline shapes in a write-time configuration is now an error.
  It used to be read as Turtle.
* `sparkles validation --shapes` takes the syntax from the file name, so a JSON-LD or
  RDF/XML file is now read in its own syntax too. A compressed file is still not
  decompressed by that command, as before.
* A count in SHACLC follows the production rule exactly: `[0..0]` produces
  `sh:maxCount 0`, which Jena leaves out.
* Each list constraint walks the list again, without a cache shared between the
  components of one shape. A list longer than 2²⁴ members counts as ill-formed.

**Conformance at landing.** All 32 test pairs of the Working Group's SHACLC suite (Jena's
copy, and the copy in the SHACL 1.2 Compact Syntax draft's directory) read to isomorphic
graphs, and each `.ttl` graph writes as SHACLC and reads back to an isomorphic graph.
Jena's `nodeParams.shc` and `propertyParams.shc` round-trip and `nodeParam-bad-01.shc`
fails. The eight SHACL 1.2 list tests pass. SHACL Core and SHACL-SPARQL stay at 98/98
and 20/20. Every drafted shapes graph of `tests/draft.rs`, 60 random datasets included,
reads the same from its SHACLC as from its Turtle. The guard's differential test runs
twelve more scenarios with list shapes and list data, in `warn` and grandfather `reject`
mode, and the incremental results equal full validation's.

**Performance.** Not measured. Parsing and writing SHACLC are linear in the document.
A list constraint costs two index scans per list cell and value node.

**Later items, 2026-10-03.** Two items of §9 landed.
* **`sparkles shacl parse FILE…`** reads shapes files in any syntax, checks that they
  are well-formed SHACL, and prints them in the syntax of `--out`: `shaclc` (the
  default), `turtle`, `nt`, `jsonld` or `rdfxml`. It follows `sparkles shex parse`, with
  a `# FILE` line before each of several files, `-` for stdin, `--in` and `--base`. The
  validation form `sparkles shacl --shapes …` is unchanged. Clap's
  `args_conflicts_with_subcommands` keeps its flags apart from the subcommand. Two choices
  differ from validation. A SHACLC file is read without its location as the base, because
  the reader names the base as the ontology (`<base> a owl:Ontology`) when the document
  has no `BASE`, and a conversion should not add that triple. The predeclared SHACLC
  prefixes `rdf`, `rdfs`, `sh` and `xsd` are offered to the Turtle writer, which writes
  those the graph uses. `tests/cli_shaclc.rs` converts SHACLC to Turtle and back to the
  same graph, and checks stdin, several files, a graph SHACLC cannot express, ill-formed
  SHACL and the usage errors.
* **The MCP tool `draft_shapes`** takes `shapesFormat` (`turtle` or `shaclc`) for SHACL
  drafts and returns it with the text. The draft already rendered SHACLC for
  `/$/schema/{ds}/shapes`, so no new code renders it. `shapesFormat` with
  `language: "shex"` is a `bad-argument`.

**Not built.** A comment-preserving SHACLC formatter for `sparkles fmt`, `POST /$/format`
and the editor. §9 makes it wait for users who ask for it, and it needs a lossless
SHACLC parser in `sparkles-fmt` next to the one in `sparkles-shacl`.

### Roadmap follow-ups (2026-10-12)

**SHACL 1.2 node expressions** (item 13, Tier 2, [ROADMAP](../ROADMAP.md)). What
Sparkles supports of SHACL 1.2 today: the four list constraints of this spec, with
`sh:detail`; `sh:targetWhere` and the `sh:Debug` and `sh:Trace` severities, from
[C10](C10-write-time-validation.md) (`crates/sparkles-shacl/src/lib.rs`, `shapes.rs`,
`guard.rs`). The validator has no node expressions: nothing in `sparkles-shacl` reads
an expression where SHACL 1.2 allows one.

A node expression computes a set of nodes from a focus node. The SHACL 1.2 drafts
define constants, the focus node, path expressions, filters by a shape, union,
intersection, distinct, counts and other aggregates, ordering with limit and offset,
conditionals, and function calls, including SPARQL-based functions. The design pins the
vocabulary to the Working Draft fetched when the work starts, as C10 §2 did for
`sh:targetWhere`, and records the draft's date.

* **Evaluator.** A `NodeExpr` enum in `sparkles-shacl` is parsed from the shapes graph
  with the other shape parameters and evaluated against the data graph's index-backed
  reads (`data.rs`), returning a sorted, deduplicated set of terms. Path expressions
  reuse `path.rs`; filters by a shape reuse conformance checking; SPARQL-based
  functions run through the SHACL-SPARQL engine already used for `sh:sparql`, with the
  same prebinding rules. A cycle through filter shapes is a shapes-graph error.
* **Where expressions are read.** First, as targets: a target whose value is a node
  expression evaluated with no focus node. Second, as values of the constraint
  parameters that the draft allows to be computed, such as `sh:hasValue` and `sh:in`.
  Third, as the expression constraint that reports a result for each focus node whose
  expression yields a value other than `true`. Inference rules that write computed
  values (`sh:values`) are left out: Sparkles' reasoner owns inferred triples, and the
  roadmap's F16 and F17 decide how rules enter it.
* **Write-time validation.** The incremental guard needs the predicates an expression
  reads to decide whether a write can affect a focus node, as it does for anchored
  SHACL-SPARQL queries. An expression made of paths, shapes, sets and aggregates
  declares them exactly; one with a SPARQL function falls back to full validation on
  every write that touches a read predicate of the shapes graph, as unanchored
  SHACL-SPARQL does today.
* **SHACLC.** The compact syntax has no node expression syntax in the current Note;
  shapes with expressions are written as Turtle by `sparkles shacl parse`, which
  already reports a graph that SHACLC cannot express.
* **Tests.** The Working Group's node expression tests, when the suite publishes them,
  and otherwise the draft's examples, each run through the validator and the guard's
  differential test.

Other SHACL 1.2 Core additions that are not supported, such as constraints on RDF 1.2
reifiers, are listed against the same draft when this work starts, each with whether
it is built here or left out and why.

**Outcome of the node expression work (2026-10-13).** Node expressions are built against the
Working Group's `shacl12-node-expr` draft as it stood in the `w3c/data-shapes` repository
at commit 056ab61a on 13 October 2026, and the vocabulary is that draft's `shnex:`
namespace. The code is `nodeexpr.rs`, the expression tree and its parser, and
`nodeeval.rs`, the evaluator, in `sparkles-shacl`.

* *What is read.* A blank node that has triples is a named parameter function, such as
  `shnex:pathValues`, a list parameter function, such as `sparql:plus`, or a list
  expression. Constants are IRIs, literals and triple terms, and a blank node without
  triples is the empty expression. Functions declared in the shapes graph with
  `sh:bodyExpression` are called the same way. Expressions are read as targets, as
  `sh:values` and `sh:defaultValue`, as `sh:deactivated`, as the shapes
  `sh:nodeByExpression` names, and as the expression constraint that reports each focus
  node whose result is not `true`. A `sh:targetNode`
  whose blank node value is not a valid expression stays a plain node.
* *Where it differs from the design above.* `sh:values` and `sh:defaultValue` are built,
  as derivations made during validation that add nothing to the data, so the reasoner
  still owns every inferred triple. An argument of a custom function is evaluated in the
  focus node and the variable scope of the call. `sh:hasValue` compares RDF terms. The
  design allowed a computed `sh:hasValue` or `sh:in`, but both take constants, and `sh:in`
  needs an RDF list. The draft does not define computed forms of either. The
  two-argument `shnex:hasLang` is `LANGMATCHES(LANG(x), range)`, and `shnex:sameValue` is
  `COALESCE(a = b, false)`. Nesting is limited to 128 levels when parsing and 192 when
  evaluating, and deep evaluation grows its stack. One evaluation may take at most
  16,777,216 units of work. Each evaluation of a sub-expression is a unit, and so is each
  node that a concatenation or a mapping appends. The nodes that a read of the data
  returns are not counted, because the data bounds them and a class with millions of
  instances is a legitimate result. A shared sub-expression counts each time it is
  evaluated, so an expression graph that doubles at every level fails early. The ids that
  a concatenation appends come to 128 MiB at the limit, and that is a floor. The output
  and the appended vector are both live, and each worker thread of a parallel validation
  can hold such a vector, so the peak is a small multiple of 128 MiB for each evaluation
  that runs at once. The Rust field `ValidateOptions::max_expr_nodes` changes the limit
  and counts units of work, not nodes. The other nesting limits are fixed in the same
  way, so requests and the guard keep the default. Evaluation also honours the timeout and cancellation of the
  validation, which it checks every 4096 nodes that evaluations produce. A read of the
  data that is repeated many times over a shared sub-expression costs one unit each time
  and is bounded by the timeout, not by the budget.
* *Write-time validation.* The incremental guard works out the predicates an expression
  reads from the focus node, as it does for anchored SHACL-SPARQL queries. An expression
  that reads the graph away from the focus node falls back to full validation. Value
  nodes that `sh:values` or `sh:defaultValue` compute are not reached over the property's
  path, so a constraint that reads the graph at them falls back as well. These are
  `sh:class`, `sh:closed`, nested shapes, list constraints, an expression that reads the
  variable `value`, and a SHACL-SPARQL component that asks about `$value`. Constraints
  that look only at the value itself, such as `sh:datatype`, stay incremental. The
  analysis of one shape stops after 100,000 steps, so an expression graph that shares
  sub-expressions many times falls back to full validation with the budget reason. The
  differential test of the guard now includes shapes with expressions.
* *SHACLC.* Unchanged, as designed. Shapes with expressions are Turtle.
* *Tests.* The Working Group's node expression library is vendored under
  `crates/sparkles-shacl/tests/shacl12/node-expr`. All 143 `sht:EvalNodeExpr` tests
  and the 3 validation tests of the suite pass, in `w3c_shacl12_node_expr`, and
  `tests/node_expr.rs` adds eight tests. The earlier suites are unchanged, with SHACLC at
  32 pairs and no failures, Core at 98 of 98, SHACL-SPARQL at 20 of 20 and the list
  constraints at 8 of 8.
* *Not built.* The Working Group's own SHACL 1.2 Core tests, run with
  `SPARKLES_SHACL_TESTS` pointing at `shacl12-test-suite/tests`, pass 115 of 144 and are
  not part of the repository's runner. This work does not change that number. The 29
  failures are Core additions of SHACL 1.2 that are not expression syntax, namely the
  constraints on reifiers (`sh:reifierShape` and the reified forms of deactivation),
  `sh:uniqueValuesFor`, `sh:subsetOf`, `sh:someValue`, `sh:rootClass`, `sh:singleLine`,
  `equals`, `disjoint` and `lessThan` with paths, the new severity cases, a few
  closed, datatype, class, node-kind and unique-language cases, and implicit class
  targets. The same suite's SHACL-SPARQL group passes 21 of 26, and three of the five
  failures are custom functions that SPARQL queries call. Each is left out here because it
  is a separate change to the constraint model, and none of them blocks node
  expressions.


### Completion path

Roadmap item 48 owns the remaining comment-preserving SHACLC formatter. It requires
a lossless parser and the CLI, HTTP and editor surfaces named above. Acceptance must
retain comments and formatting stability while preserving the reader/writer's
semantic round trips and existing SHACLC and constraint conformance tests.

Completed item 13 covers the adopted list and node-expression contracts. The draft
SHACL 1.2 Core constraints listed above were not part of that node-expression phase,
and its completion does not claim the entire draft suite passes. Adopting additional
draft constraints requires a named design and conformance scope of its own.
