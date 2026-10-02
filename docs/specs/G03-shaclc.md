# G03: SHACL Compact Syntax and SHACL 1.2 list constraints

> **Status:** implemented in part (Phase 1)
>
> **Phases:** Phase 1 is the SHACLC reader and writer in `sparkles-shacl`, SHACLC wherever
> shapes are read or shown, and the four SHACL 1.2 list constraint components. The later
> items of §9 are not built.
>
> **User docs:** [API: SHACL Compact Syntax](../API.md#shacl-compact-syntax-shaclc) ·
> [API: SHACL validation](../API.md#shacl-validation) ·
> [Usage: drafting shapes](../USAGE.md#drafting-shapes-from-the-data) ·
> [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This is a clean-room spec. It depends on the SHACL validator in `crates/sparkles-shacl`,
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
  did not list. The MCP tool `draft_shapes` still returns Turtle or ShExC only.
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

**Not built.** A comment-preserving SHACLC formatter for `sparkles fmt`, `POST /$/format`
and the editor. A CLI command that converts shapes between syntaxes, like Jena's `shacl
parse --out=compact`. SHACLC output from the MCP tool `draft_shapes`.
