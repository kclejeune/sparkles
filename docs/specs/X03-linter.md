# X03: Linter (`sparkles lint`, `sparkles lsp`, `POST /$/lint`, the query editor)

> **Status:** implemented
>
> **Phases:** one phase. It covers the rules of §3 for SPARQL, Turtle and TriG, the
> `sparkles lint` command with `--fix`, the `[lint]` table of the formatter's config file,
> diagnostics and quick fixes in `sparkles lsp`, `POST /$/lint`, the browser module's
> `lint`, and linting in the query editor.
>
> **User docs:** [API: Linting](../API.md#linting) · [Usage](../USAGE.md#linting) · [Editor integration](../editors.md) · [Features](../FEATURES.md#formatter-and-editor-support)
>
> This is the design; the [Outcome](#outcome) section at the end records how it landed.

This is internal engineering, not derived from any other database product. [X02](X02-formatter.md)
left linting out of the formatter and said that a `sparkles lint` needs its own spec. This
is that spec. The sources are the W3C SPARQL 1.1 and 1.2 Query specifications (variable
scope, grouping and the algebra of FILTER, OPTIONAL and MINUS), RDF 1.1 and 1.2 Concepts,
XML Schema 1.1 Part 2, BCP 47 and the IANA language subtag registry, the OWL 2 mapping to
RDF, the Language Server Protocol 3.17, Apache Jena's public constants for its old
function namespaces, and the public documentation of ESLint and Ruff for the command's
conventions. No code was read or copied from another linter.

## 1. Summary, goals, non-goals

`sparkles lint` finds mistakes and doubtful constructs in SPARQL queries and updates and in
Turtle and TriG documents. It reports each finding with a rule name, a severity and a
range, and it can fix the findings whose fix never changes what the document means.

**Goals**

1. The findings are the same everywhere. The CLI, the language server, the HTTP endpoint
   and the UI all call one library function.
2. The rules work on the formatter's lossless syntax tree, so every finding has an exact
   range and comments do not confuse it.
3. Each rule has a default severity, and a project can change any of them or turn rules
   off. Syntax errors cannot be turned off.
4. `--fix` applies only safe fixes. The fixed document must parse to the same SPARQL
   algebra, or to an isomorphic graph or dataset, as the input. Otherwise nothing is
   written.
5. The rules are cheap. A query lints in well under a millisecond, so the editor can lint
   while the user types.

**Non-goals**

- Linting N-Triples, N-Quads, JSON-LD or RDF/XML. The line formats are usually machine
  output, and their documents can be larger than memory.
- Checks that need the data, such as a predicate that no triple uses or a class with no
  instances. The schema browser (C02) covers those.
- Linting SPARQL inside SHACL literals (`sh:select`). The literal would have to be
  extracted, with its own positions and prefixes.
- Plugins or user-defined rules.

## 2. User-visible behaviour

### 2.1 CLI

```
sparkles lint [PATH…]          files or directories; stdin without a PATH
  --language LANG              sparql | turtle | trig (default: extension, then content)
  --stdin-filepath NAME        stdin's name for detection, config and ignore files
  --rule RULE=LEVEL            error | warning | info | hint | off (repeatable)
  --fix                        apply the safe fixes in place, or print the fixed stdin
  --strict                     warnings fail too
  --format text|json
  --list-rules
  --config PATH | --no-config  as in sparkles fmt
  --ignore-path PATH           as in sparkles fmt
  --max-bytes SIZE             the largest file (default 256 MiB)
```

Directories are walked as `sparkles fmt` walks them, with the same ignore files, and only
`.rq`, `.ru`, `.sparql`, `.ttl` and `.trig` files are taken. A text report has one line
per finding, `path:line:column: severity [rule] message`, and a summary on stderr. With
`--fix` and stdin, the fixed text goes to stdout and the findings go to stderr. The exit
status is 0 when no finding is an error, 1 when one is (or a warning is under `--strict`),
and 2 when a file, flag or config file cannot be used.

### 2.2 Configuration

The formatter's config file gets a `[lint]` table. Each key is a rule and each value is a
severity or `off`. The nearest `.sparklesfmt.toml` applies, as it does for formatting, and
`--rule` overrides it. The formatter ignores the table.

```toml
line-width = 100

[lint]
unused-prefix = "error"
single-use-variable = "off"
```

### 2.3 Language server

`sparkles lsp` lints every SPARQL, Turtle and TriG document it formats and publishes the
findings next to the formatter's, with the source `sparkles lint` and the rule as the
code. The lint's `undefined-prefix` replaces the formatter's `undeclared-prefix` note.
The server announces `codeActionProvider` with two kinds. A `quickfix` action applies the
fix of one finding, and `source.fixAll.sparkles` applies every safe fix through the same
path as `--fix`.

### 2.4 HTTP and the browser module

`POST /$/lint` takes `{text, language?, rules?, fix?}` as JSON and answers
`{language, diagnostics}`, plus the fixed `text` and the count `applied` with `fix`. Each
diagnostic has its rule, severity, message, lines and columns, and its range in UTF-16
code units (`from`, `to`), which is the editor's unit. A safe fix comes with its edits.
A syntax error is a finding, not an error response. The endpoint follows
`--format-endpoint`, `--format-max-mb` and `--format-timeout`, and it uses the
formatter's slots. The WebAssembly module of X02 gets a `lint` function with the same
request and answer.

### 2.5 UI

The query editor lints the query 400 ms after it stops changing, in the browser module
when it is there and through the endpoint otherwise. Findings are underlined in the
colour of their severity and carry their message as a title. A short count sits next to
the Format button. The Format menu gets "Lint while typing" and "Fix lint problems", and
the fix is one undoable change.

## 3. Rules

| Rule | Languages | Default | Fix | What it finds |
|---|---|---|---|---|
| `syntax` | all | error | no | The reference parser's error. It is always on. |
| `undefined-prefix` | all | error | no | A prefixed name whose prefix is not declared before it. |
| `unused-prefix` | all | warning | yes | A declaration that no prefixed name uses before the label is declared again. The fix removes it. |
| `unused-variable` | SPARQL | warning | no | A variable bound by `BIND` or `VALUES` that nothing reads. |
| `single-use-variable` | SPARQL | hint | no | A variable that occurs once, in a triple pattern or `GRAPH`, and is not projected. It is a wildcard or a typo. |
| `unbound-variable` | SPARQL | warning | no | A variable that is projected, filtered, ordered or used in a template but never bound. |
| `cartesian-product` | SPARQL | warning | no | A pattern that shares no variable with the patterns before it in its group. |
| `select-star-group-by` | SPARQL | error | no | `SELECT *` in a query that groups. |
| `ungrouped-variable` | SPARQL | error | no | A projected variable that is neither grouped nor aggregated. |
| `filter-scope` | SPARQL | warning | no | A FILTER in a nested group that tests a variable bound only outside the group. |
| `filter-equality` | SPARQL | hint | no | `FILTER(?v = <iri>)` where the IRI could be written in the pattern. |
| `iri-space` | all | warning | no | An IRI, an `IRI("…")` argument or an `xsd:anyURI` with whitespace. |
| `language-tag-case` | all | warning | yes | A language tag not in BCP 47's recommended case. The fix rewrites it. |
| `deprecated-language-tag` | all | warning | no | A deprecated language, region or whole tag, with its replacement. |
| `suspicious-datatype` | all | warning | no | An invalid lexical form, an unknown XML Schema name, a misspelled XML Schema namespace or `rdf:langString` as a datatype. |
| `redundant-datatype` | all | info | yes | `"…"^^xsd:string`. The fix drops the datatype. |
| `deprecated-syntax` | all | warning | no | Jena's old `jena.hpl.hp.com` namespaces, or `owl:DataRange`. |

### 3.1 Variables and scope

Each query form, subquery and update operation is a level. A level's variables are its
own, and a subquery contributes only what it projects (all of its in-scope variables for
`SELECT *`). A variable is bound by a triple pattern, `GRAPH ?g`, `BIND … AS ?v`,
`VALUES`, a subquery, or a projection or `GROUP BY` alias. Patterns inside `MINUS`,
`FILTER EXISTS` and `FILTER NOT EXISTS` bind nothing outside themselves, and their own
variables are exempt from the single-use rules because they are wildcards by design.

A variable whose name starts with `_` is exempt from `unused-variable` and
`single-use-variable`. `SELECT *`, `ASK`, `DESCRIBE *`, the short form of `CONSTRUCT` and
`DELETE WHERE` project or use every variable, so the single-use rules skip them.

### 3.2 FILTER

A FILTER sees only the variables of its own group. A FILTER in a nested group that tests a
variable bound only outside that group always sees it unbound, so it removes every row.
The FILTER of an OPTIONAL's group is exempt, because it is the left join's condition and
sees the outer row. So is any FILTER inside `EXISTS`, which also sees the outer row.

`filter-equality` is a hint. Sparkles substitutes the IRI into the pattern itself when the
variable appears only in triple patterns, as Jena's filter-equality transform does, but
other engines may not. Equality with a literal is not reported, because `=` compares
literals by value.

### 3.3 Cartesian products

Within one group, the elements other than FILTER and MINUS are joined. Two elements are
connected when they share a variable or a blank node label. A `BIND` connects the
variables it reads with the one it binds. A component without variables is an existence
test and is skipped. Every component after the first is reported at its first element.
A FILTER that compares the components does not count as a connection, because the product
is still computed before the filter.

### 3.4 Terms

BCP 47 recommends the language in lower case, a four-letter script in title case and a
two-letter region in upper case. Everything after a singleton is lower case, and so is a
base direction. RDF compares tags without
regard to case, so the fix does not change the data. The deprecated tags are the withdrawn
ISO 639 codes (`iw`, `in`, `ji`, `jw`, `mo`), the deprecated regions (`BU`, `DD`, `FX`,
`TP`, `YD`, `ZR`) and the grandfathered tags with a preferred value.

Lexical forms are checked for the XML Schema numeric, boolean, date, time, duration,
binary and string-family types. Integer types are checked against their ranges, and
`xsd:integer` and `xsd:decimal` by their grammar, so large values are not refused. Leading
or trailing whitespace in a numeric or temporal literal is reported, because RDF compares
the lexical form as written.

## 4. Architecture

The linter is a module of `sparkles-fmt`, next to the formatter whose syntax tree it uses.
It keeps the crate's constraints: no server dependency, and a build for
`wasm32-unknown-unknown`. The only new dependency is `oxsdatatypes`, which Sparkles
already uses and which validates the XML Schema date, time and duration forms.

A document is lexed and then checked by the reference parser, which is spargebra for
SPARQL and oxttl for Turtle and TriG. In Turtle, undeclared prefixes are declared for the reference parse,
so that an undefined prefix is the lint's finding and the other rules still run. When the
reference parse fails, the syntax error is the finding, with two exceptions. If the tree's
parser accepts the query, the failure is one of variable scope, and the grouping rules
explain it. If the error lies inside `<scheme:… …>` with whitespace, `iri-space` reports
it as an error.

`lint::fix` applies the safe fixes that do not overlap, lints again, and repeats a few
times. It then compares the result with the input through the formatter's checks and
refuses the result if they differ.

## 5. Testing

- Unit tests for each rule, with the cases that must not be reported.
- Every query and update of the W3C SPARQL suites, and every Turtle and TriG test of the
  RDF suites. A valid document must get no `syntax` finding, an invalid one must get an
  error, and `fix` must keep the meaning of every valid document.
- The CLI as a binary, the language server as a child process, the endpoint through the
  router, the browser module, and the query editor against the mock server.

## 6. Rejected alternatives

- **Rules over the algebra.** spargebra's algebra loses positions and comments, and it
  has already flattened groups, so a finding could not point at the text.
- **A separate config file.** One `.sparklesfmt.toml` per project is simpler, and the
  formatter and the linter already share the walk and the ignore files.
- **Fixes for every rule.** Renaming a variable or moving a FILTER changes the query's
  meaning, so those stay findings.

## Outcome

**Delivered on 2026-10-02**, in one phase, with this spec.

**Deviations and decisions.**

- The first version reported every variable that occurs once as `unused-variable`. Over
  the W3C suites that gave 222 findings, nearly all of them wildcards such as the subject
  of `SELECT ?name { ?s foaf:name ?name }`. Single-use variables in triple patterns and
  `GRAPH` became the separate `single-use-variable` rule at hint level, and
  `unused-variable` kept the bindings of `BIND` and `VALUES` that nothing reads.
- `ungrouped-variable` was added next to `select-star-group-by`. spargebra refuses both
  cases, and the tree's parser accepts them, so both explain a syntax error in plain
  words.
- An IRI with a space stops the reference parser. The lint then reports `iri-space` as an
  error at the IRI, whatever the configured level, because the document does not parse.
- The UI lints the query editor only. The SHACL shapes editor on the dataset page does
  not lint, because its text may be SHACLC.
- `POST /$/lint` takes JSON bodies only.

**Tests at landing.** The lint unit tests cover every rule. The W3C corpus test passes
over the SPARQL suites and the Turtle and TriG suites, with no `syntax` finding on a valid
document, an error on every invalid one, and `fix` accepted by the equivalence checks on
every valid document. Over the valid SPARQL tests the rules report 113 cartesian products,
38 unbound variables and 4 FILTER scope problems. All 4 FILTER scope findings and a
sample of 25 of each of the others were read, and each one is what the test sets out to
exercise. `tests/cli_lint.rs`, `tests/cli_lsp.rs`, the router
tests, the WebAssembly module's tests and `ui/tests/mock/lint.spec.ts` cover the front
ends.

**Performance.** Not measured separately. The rules make a few passes over the syntax
tree, and the reference parse that the formatter also runs dominates the cost.

**Not built.** Linting N-Triples, N-Quads and JSON-LD, linting the SPARQL inside SHACL
literals, an MCP tool, and linting in the shapes editor.
