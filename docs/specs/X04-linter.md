# X04: Linter (`sparkles lint`, `sparkles lsp`, `POST /$/lint`, the query editor)

> **Status:** implemented
>
> **Phases:** one phase and a later extension. The phase covers the rules of §3 for SPARQL,
> Turtle and TriG, the `sparkles lint` command with `--fix`, the `[lint]` table of the
> formatter's config file, diagnostics and quick fixes in `sparkles lsp`, `POST /$/lint`,
> the browser module's `lint`, and linting in the query editor. The
> [extension](#linter-extensions-item-48-part-b) adds N-Triples, N-Quads, JSON-LD and
> SHACLC, the SPARQL in SHACL literals, an MCP tool and linting in the shapes editor.
>
> **User docs:** [API: Linting](../API.md#linting) · [Usage](../USAGE.md#linting) · [Editor integration](../editors.md) · [Features](../querying/README.md#formatter-and-editor-support)
>
> This is the design; the [Outcome](#outcome) section at the end records how it landed.

This is internal engineering, not derived from any other database product. [X02](X02-formatter/README.md)
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

- Linting RDF/XML. The first version also left out N-Triples, N-Quads and JSON-LD, because
  the line formats are usually machine output and their documents can be larger than
  memory. The [extension](#linter-extensions-item-48-part-b) added them.
- Checks that need the data, such as a predicate that no triple uses or a class with no
  instances. The schema browser (C02) covers those.
- Linting SPARQL inside SHACL literals (`sh:select`) in the first version. The literal has
  to be extracted, with its own positions and prefixes, and the
  [extension](#linter-extensions-item-48-part-b) does that.
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

## Provenance

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

- **Spec:** [`X04-linter.md`](X04-linter.md), written on 2026-10-02 as internal
  engineering, the `sparkles lint` that [X02](X02-formatter/README.md) deferred. Its sources are:
  - the W3C SPARQL 1.1 and 1.2 Query specifications (variable scope, grouping, and the
    algebra of FILTER, OPTIONAL and MINUS), RDF 1.1 and 1.2 Concepts, XML Schema 1.1
    Part 2, BCP 47 (RFC 5646) and the IANA language subtag registry, the OWL 2 mapping to
    RDF, and the Language Server Protocol 3.17;
  - Apache Jena's `ARQConstants` and `MappedLoader` (Apache-2.0), read for the old
    `jena.hpl.hp.com` namespaces and Jena's warning about them. No code was copied;
  - the public documentation of ESLint and Ruff (MIT) for the command's conventions:
    severities per rule, `--fix` for safe fixes only, and the exit statuses;
  - the Sparkles code: the formatter's lexer, syntax tree, prefix scopes, reference
    parses and equivalence checks.

  No code was read or copied from another linter.
- **Implementation** (2026-10-02): from the spec and the Sparkles code. **Dependencies:**
  `oxsdatatypes` 0.2 (MIT OR Apache-2.0), already a dependency of `sparkles`, became a
  dependency of `sparkles-fmt` for the XML Schema date, time and duration forms.
- **Rejected** (spec §6): rules over the SPARQL algebra, a config file of its own, and
  fixes that change a document's meaning.

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

**Not built at landing.** Linting N-Triples, N-Quads and JSON-LD, linting the SPARQL inside
SHACL literals, an MCP tool, and linting in the shapes editor. The extension below built
them, together with linting for SHACLC.

### Roadmap follow-ups (2026-10-12)

**More formats, embedded SHACL queries, the shapes editor and an MCP tool** (item 48,
Tier 5, [ROADMAP](../ROADMAP.md)). SHACLC formatting, the other half of item 48, is in
[X02](X02-formatter/OUTCOME.md#roadmap-follow-ups-2026-10-12).

* **What is missing today.** The Outcome's "Not built" list still holds:
  `sparkles_fmt::lint` accepts SPARQL, Turtle and TriG only (`lint/mod.rs`, the
  `matches!` on `Language`), the shapes editor does not lint, and `sparkles mcp` has a
  `format` tool but no `lint` tool. The linter is reached from MCP only indirectly:
  `explain_query` merges lint findings into its notes with `source: "lint"`
  (`crates/sparkles-server/src/explain/mod.rs`).
* **N-Triples, N-Quads and JSON-LD.** The term rules of §3.4 (`iri-space`, language
  tags, datatype lexical forms) apply to each term the line parser and the JSON-LD
  tree already produce. Rules about prefixes do not apply. JSON-LD adds two rules: a
  term in `@context` that no key uses (hint, with a fix that removes it), and a key that
  the context does not map, which JSON-LD drops silently (warning).
* **SPARQL inside SHACL.** In Turtle, TriG and JSON-LD, the literal objects of
  `sh:select`, `sh:ask`, `sh:construct` and `sh:update` are linted as SPARQL with the
  prefixes that `sh:prefixes`/`sh:declare` give them, plus the document's own. A
  finding's range maps back into the literal, accounting for escapes and long quotes
  as the formatter's cursor mapping does. `$this` and the pre-bound variables of SHACL
  (`$value`, `$PATH`, `$currentShape`, `$shapesGraph`) count as bound, so
  `unbound-variable` does not report them. Fixes are offered only when the literal is a
  long string without escapes, so that the edit is exact.
* **The shapes editor.** It lints Turtle shapes now and SHACLC once X02's SHACLC tree
  exists, through the WebAssembly module, as the query editor does.
* **MCP tool.** `lint` takes `text`, `language` and the `[lint]` options as camelCase,
  like `format`, and returns the findings with rule, severity, range and message, and
  `fixed` text when `fix` is true. It reads no dataset, is capped by `--mcp-max-bytes`,
  and runs within `timeoutSeconds`.

### Linter extensions (item 48, part b)

**Delivered on 2026-10-31.** The follow-up above is built as designed, except where the
deviations below say otherwise. The linter now takes every language the formatter does,
and RDF/XML stays out. `lint::lints` is true for all seven languages.

**Languages.**

* N-Triples and N-Quads get the term rules of §3.4 and no others. The Turtle lexer reads
  their terms, and the rules run over a flat tree that wraps each string with its language
  tag or datatype in the node the Turtle rules read. oxttl is the reference parser and
  decides what a syntax error is. An IRI with a space is an `iri-space` error, as in Turtle.
* SHACLC gets the prefix rules and the term rules. The reader of `sparkles-shaclc` is the
  reference parser. It refuses an undeclared prefix, so the lint declares those for the
  reference parse and reports `undefined-prefix` itself, as it does for Turtle. The four
  prefixes that every SHACLC document starts with, `rdf`, `rdfs`, `sh` and `xsd`, are
  declared and are never unused. SHACLC has no syntax for `sh:select` or the other query
  properties, so it has no embedded SPARQL.
* JSON-LD gets the term rules over value objects (`@language`, and `@type` against
  `@value`), over the strings of terms whose definition coerces a datatype, and over the
  IRIs of `@id`, `@type` and `@context`. A language tag fix rewrites the string, and a
  redundant `xsd:string` fix removes the `@type` member with its comma.

**JSON-LD rules.** `unused-context-term` is a hint with a fix that removes the term and its
comma. A term counts as used when any string of the document outside the keys that
define terms equals it or starts with `term:`. That covers keys, `@type` values, coerced
`@vocab` values, the prefixes of compact IRIs and the other definitions. A string with
spaces counts for each `label:` it holds, so a prefix that a SHACL query uses is used.
`unmapped-key` is a warning. It follows the contexts in effect for each object, including
the scoped contexts of terms and of `@type` values, and it skips the keys of map
containers, the values of JSON literals and value objects. It stays silent for a document
without a context, beside a context given by URL or `@import`, and under `@vocab`, because
any of them can define the key. A term defined as `null` is dropped on purpose and is not
reported. These rules read the document through a model of its contexts that follows
`@vocab`, keyword aliases and the scoped contexts of terms, types and aliases of `@nest`.
A JSON literal, the value of a term typed `@json` or of a value object whose type is
`@json`, is data, and nothing in it is read as a context, a term definition or a use of a
term. The model only guides the rules. Each fix of a JSON-LD finding is offered, in `fix`
and as an editor's quick fix alike, only when oxjsonld expands the document with that one
edit applied to a dataset isomorphic to the document's own, within the deadline. A
document with more than 256 fixable findings gets the findings without fixes, so the check
stays bounded. A document that cannot be expanded here, because it names a context by URL,
gets no fixes and keeps its text under `fix`.
oxjsonld became a dependency of `sparkles-fmt`, and the crate still builds for
`wasm32-unknown-unknown`.

**SPARQL in SHACL.** In Turtle, TriG and JSON-LD, the plain string objects of `sh:select`,
`sh:ask`, `sh:construct` and `sh:update` are linted as SPARQL. A string with a language tag
or a datatype other than `xsd:string` is not a query. The document is read a second time
as RDF, only when it has such a literal, to find the holder of each query and the
prefixes and parameters it sees. A query sees those of the node that holds it, in its own
graph, and a literal is linted only when that holder is certain.

* *Turtle and TriG.* When the statement's subject is an absolute IRI, a prefixed name or a
  blank node label, in the default graph or in a graph block with such a label, the
  holder is that node in that graph, confirmed by a quad of the parse. A literal in a blank
  node property list, a reified triple or an annotation has no holder in the source. It is
  linted when exactly one node holds its property and text and exactly one string literal
  of the document has that text.
* *JSON-LD.* Which values are queries comes from oxjsonld's expansion of the document, so
  `@vocab`, keyword aliases, default and scoped languages, coercions and redefined terms
  are decided as JSON-LD decides them. A query quad is mapped back to the source when
  exactly one value string of the document has its text, exactly one node holds it, and a
  probe shows that the string makes the quad: the document with that string replaced by a
  fresh text expands to the same dataset with only that quad's literal replaced. A query
  made from something else, such as the key of an `@index` map, fails the probe. The probe
  streams the expansion under the deadline and counts quads instead of keeping them. At
  most 256 strings are probed, and a document that cannot be expanded here gets no query
  findings.
* *Work.* Each probe and each fix check expands the whole document again. Together they
  expand at most four times the document, or 1 MiB for a small one, and never more than
  64 MiB, with or without a deadline, so linting in the browser stays bounded too. With a
  deadline, one is not started when an expansion as long as the document's own would end
  past it, because an expansion that must read the whole document before its first quad
  cannot stop early. A probe or check that does not fit is skipped, so its query gets no
  findings or its finding no fix. A JSON-LD document over 8 MiB is not expanded at all,
  so it gets no query findings and no fixes.
* *Ambiguity.* Any other literal is skipped. The same query text on two anonymous nodes,
  or a text that another string of the document repeats, costs the findings of that query
  and never gives one node the findings of another.

* *Prefixes.* A query sees the document's own prefixes first and then the `sh:declare`
  pairs of the nodes that `sh:prefixes` names on the node that holds the query and on the
  nodes that point at it, with the `owl:imports` closure. The prefixes are written in front
  of the query as `PREFIX` lines, and a finding in front of the query is dropped, so an
  injected prefix is never reported as unused. The engine of `sparkles-shacl` resolves
  only the `sh:declare` prefixes, so a query that works through the document's prefixes
  lints clean and fails at validation. That leniency follows the design above. A document
  prefix that a query writes counts as used, so `unused-prefix` does not report it and
  `fix` does not remove it.
* *Pre-bound variables.* `$this`, `$shapesGraph`, `$currentShape`, `$value` and `$PATH`
  are bound, and so are the parameters of a constraint component, by the local name of each
  `sh:path`, when the query is one of its validators. The variable rules do not report them
  as unbound, single-use or unused. They are not bound for `filter-scope`, because that
  rule is about where a pattern binds.
* *Positions.* Each string is decoded into a table that holds, for every byte of the
  value, where its source character starts and ends in the document. An escape such as
  `\n`, `\"`, `\u00e9` or a JSON surrogate pair is one source character, so a finding that
  covers it covers all of it. Multi-line long strings, non-ASCII text and escapes before the
  finding all land on the right bytes, and the line and column are those of the document.
  This is a table of its own and not the formatter's cursor map, which maps between an
  input and its formatted output and has no meaning for a string literal.
* *Fixes.* A finding has a fix only in a long string (`"""` or `'''`) without escapes,
  where the query text is the source text and an edit lands exactly. The edited query must
  parse to the same algebra, the string must not gain its own delimiter, and the edit may
  not insert a quote or a backslash. `fix` compares a Turtle or TriG document before and
  after as RDF with the text of its `xsd:string` query literals blanked, because the
  embedded linter has already checked that each query means what it meant. JSON strings
  are never long, so JSON-LD queries have findings and no fixes, and a JSON-LD document is
  compared with nothing blanked.

**Surfaces.** `sparkles lint` walks `.nt`, `.nq`, `.jsonld`, `.shaclc` and `.shc` files
too, and `--list-rules` shows each rule's languages. `sparkles lsp`, `POST /$/lint` and the
browser module take the new languages without a change of their own. The MCP `lint` tool
takes `text`, `language`, `options` with the rule names in camelCase, `fix` and
`timeoutSeconds`, as `format` takes its options. It returns each finding with its rule,
severity, message and range, the counts, and with `fix` the fixed text. The result is
measured as its structured content serializes, escapes included. A fixed text that does not
fit is refused with `too-large`, and a result with more findings than fit lists the ones
that do and sets `truncated`. `POST /$/lint` answers `415` with `unsupported-language` for
RDF/XML, named or detected, and `400` for a language name it does not know. The shapes editor on a
dataset's page lints 400 ms after the text stops changing, in the browser module when it
is there and through the endpoint otherwise, in Turtle and in SHACLC, with a count next to
the Format button.

**Deviations and decisions.**

* The first run of `fix` on a Turtle or SHACLC document with an `undefined-prefix` finding
  failed with `unsafe-fix`, because the check that compares the input with the fixed text
  parsed both with the prefix undeclared. The check now declares the undeclared prefixes of
  both, as the lint does, and a test covers it.
* The MCP option names are camelCase rule names, `unusedPrefix` for `unused-prefix`, and
  the kebab-case names are accepted too. The HTTP endpoint keeps its `rules` object.
* The line formats are read whole, so the CLI's `--max-bytes` applies to them. Linting a
  document larger than memory in bounded memory is left open. The reference parse streams
  without keeping the quads, and it and the term rules check the deadline every few steps.
  The lines and columns of all findings are set in one pass over the text, and so are the
  UTF-16 offsets of the HTTP endpoint and the MCP tool and the positions of the language
  server, so a document with many findings costs one scan rather than one per finding.
  Each of those passes checks the deadline as it goes, and the lint never returns findings
  once the deadline has passed. The language server gives each diagnostics request one
  deadline for the lint, the positions and the publication.
* The isomorphism check behind the JSON-LD fixes, the provenance probes and the SHACLC
  formatter never canonicalizes exactly, which takes factorial time on a few symmetric
  blank nodes and cannot be stopped. Graphs of any size are compared by colour refinement
  and a budgeted search that checks the deadline, and whatever that cannot verify is
  refused.
* Memory. Only the query literals that are linted get a position table, which holds two
  offsets for each byte of the query. The texts of anonymous query literals are counted
  among the document's strings without tables, and a string too long to be one of them is
  not decoded, so a large data literal costs no copy.
* N-Triples and N-Quads are not read for SHACL query literals. The three languages that
  can carry shapes in practice are covered.

**Review fixes.** An independent review of the extension found that a JSON literal's
payload could get a context-term fix, that the line-format pass ignored its deadline and
placed each finding by rescanning the text before it, that a prefix used only by a query
was removed by `fix`, that queries with the same text shared their holders' prefixes and
parameters, that JSON-LD queries were chosen without expanding types, languages, `@vocab`
and keyword aliases, that the MCP tool measured the fixed text before JSON escaping, that
the shapes editor could show the findings of text it no longer held, and that the API
promised a `415` for RDF/XML that the endpoint did not give. A second review found that
the hand-written model of JSON-LD contexts still let a fix reach a JSON literal through a
context scoped on an `@nest` alias and still misread scoped languages and redefined terms,
that matching literals to quads by their order swapped holders, and that error paths and
the final placing of findings ran past the deadline. Queries and fixes in JSON-LD now rest
on the real expansion, holders on identity or an unambiguous match, and the lint checks the
deadline before every parse, on every error path, while placing findings and before it
returns. The HTTP endpoint's and the MCP tool's UTF-16 offsets and the language server's
positions are counted under the same deadline. A third review found that the bounded
isomorphism check still canonicalized small graphs without a deadline, that holders were
deduplicated by a quadratic scan, that a JSON-LD query made from an `@index` key could be
placed on an unrelated string with its text, that every Turtle string got a position
table, and that the language server renewed its deadline for positions. The search
replaced the exact canonicalization, holders are hash sets built under the deadline, each
JSON-LD query's source is proved by a probe, only query literals get tables, and the
language server keeps one deadline per request. Each case has a regression test.

**Tests.** `crates/sparkles-fmt/src/lint` has unit tests for N-Triples, N-Quads, SHACLC,
JSON-LD and the embedded queries. They cover the pre-bound variables, the prefix sources,
escapes, multi-line and non-ASCII literals, long strings and the fix conditions.
`tests/lint_corpus.rs` also runs the W3C N-Triples and N-Quads suites, Jena's SHACLC files
and the JSON-LD files of the Jena and Oxigraph checkouts: no `syntax` finding on a valid
document, an error on every invalid one, and `fix` accepted by the equivalence checks. The
MCP tool has tests in `mcp/lint_tests.rs`, and the tool-list test lists it. The shapes
editor has a unit test of its lint request and a mock-backend test of the marks, and
`ui/src/lib/lint-runner.test.ts` tests that both editors drop the answer of a lint whose
text has changed, been cleared or been closed.
