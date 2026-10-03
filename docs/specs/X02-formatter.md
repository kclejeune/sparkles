# X02: Opinionated formatter (`sparkles fmt`, `POST /$/format`, the UI's Format button)

> **Status:** implemented
>
> **Phases:** Phase 1 covers SPARQL, Turtle/TriG, N-Triples/N-Quads, `sparkles fmt`, the
> config file, `POST /$/format` and the UI Format button. Phase 2 adds JSON-LD, `--sort`,
> `--prune-prefixes`, `--canonicalize`, the external sort, `sparkles lsp`, the two optional
> keys `turtle-layout` and `align-values`, and Format in the shapes editor. Phase 3 adds the
> WebAssembly build that the UI uses and streaming Turtle/TriG. The Phase 3 fuzzing targets
> had not landed when this record was written.
>
> **User docs:** [API: Formatting](../API.md#formatting) · [Editor integration](../editors.md) · [Features](../FEATURES.md#formatter-and-editor-support)
>
> This is the design as written before implementation; the [Outcome](#outcome) section at the end
> records how it landed.

This is internal engineering, not derived from any other database product. The sources
are the W3C grammars and data-model specifications, the Prettier and Oxc public
documentation, and the public docs and licenses of existing SPARQL and Turtle formatters
(§14). No code was read or copied from any GPL or LGPL project. Statements about the code
refer to commit `ad656ae`, with `spargebra` 0.4.7 (`sparql-12`), `oxttl` 0.2.4, `oxrdf`
0.3.4 (`rdf-12`, `rdfc-10`), `oxjsonld` 0.2.6 and `oxrdfxml` 0.2.4.

## 1. Summary, goals, non-goals

Sparkles users keep SPARQL queries, SHACL shapes and ontologies in git and type queries
into the UI. Today nothing formats them. This spec adds one formatter with three entry
points:
- `sparkles fmt`, a CLI with `--check`, `--write` and Prettier's exit codes;
- `POST /$/format`, which takes text and returns formatted text and a mapped cursor offset;
- a **Format** button and shortcut in the UI query editor.

It formats:
- SPARQL 1.2 queries and updates (`.rq`, `.ru`, `.sparql`);
- Turtle 1.2 and TriG 1.2, including SHACL shapes graphs;
- N-Triples 1.2 and N-Quads 1.2;
- JSON-LD 1.1 (Phase 2).

RDF/XML is rejected (§4.5).

**Goals**

1. **Prettier-like.** Output depends only on the syntax tree, the comments, the blank-line
   groups, and the options of §2.2. The defaults are the house style. Each of the few
   style keys switches one whole convention in wide use, such as `@prefix` files,
   trailing operators or quotes as written. No key tweaks a single construct (§13).
2. **Diff-friendly.** One statement per line. Ordering is stable. By default a multi-line
   Turtle block ends with `;` on every predicate line and a lone `.`, so adding a
   predicate is a one-line diff.
3. **Comments are never lost.** They stay attached to the node they describe, and runs
   of comment lines stay together (§3.3).
4. **Normalizes only what is provably safe.** Every rewrite in §5 cites the spec section
   that makes it meaning-preserving. Lexical forms of literals are never changed.
5. **Self-checking.** Before any output is returned or written:
   - the formatted SPARQL must parse to the same spargebra algebra as the input;
   - the formatted RDF must parse to an isomorphic graph or dataset;
   - every comment must be present;
   - `format(format(x)) == format(x)`.

   On any failure the formatter refuses, and the input is kept (§6).
6. The library crate has no server dependency and builds for `wasm32-unknown-unknown`.

**Non-goals**

- A generally style-configurable formatter. Brace styles, keyword case, predicate and
  prefix alignment, and per-construct indentation options are all out. §2.2 lists the
  complete set of keys.
- Linting beyond what formatting needs. A later `sparkles lint` could cover unused
  variables, undeclared prefixes, cartesian products and deprecated syntax.
- Formatting SPARQL embedded in string literals (`sh:select """…"""`), because that
  changes the literal (§4.2).
- Formatting N3, RDF/XML, SHACL compact syntax (`.shc`) or plain JSON.
- Range formatting. The LSP in Phase 2 formats the whole document and returns a minimal
  edit.
- Streaming Turtle/TriG larger than memory. Only the line formats stream.

## 2. User-visible behavior

### 2.1 CLI

```
sparkles fmt [OPTIONS] [PATH]...

Arguments:
  [PATH]...   Files or directories. Directories are walked recursively (known extensions
              only; .gitignore and .sparklesfmtignore apply). With no PATH, reads stdin and
              writes stdout.

Options:
  -c, --check                Check only; exit 1 if any file would change
  -l, --list-different       Print the names of files that would change (stdout); exit 1 if any
  -w, --write                Rewrite files in place (atomically; unchanged files are not touched)
      --diff                 With --check: print a unified diff per changed file (stdout)
      --stdin-filepath NAME  Name of the stdin input, for language detection, config and ignore
      --language LANG        sparql | turtle | trig | ntriples | nquads | jsonld
      --line-width N         Default 100
      --indent-width N       Default 2
      --sort / --no-sort     Opt-in sorting: Turtle, TriG, JSON-LD terms, N-Triples, N-Quads (default off)
      --canonicalize         N-Triples/N-Quads: RDFC-1.0 blank node labels, canonical form, no comments
      --prune-prefixes       Drop prefix declarations nothing uses
      --directive-style S    sparql (PREFIX, GRAPH) | turtle (@prefix, no GRAPH); default sparql
      --prefix-group LABELS  Comma-separated labels forming one prefix group (repeatable, in order)
      --type-shorthand, --no-type-shorthand
                             rdf:type → a, and a entries first (default on)
      --compact-iris, --no-compact-iris
                             Full IRI → prefixed name (default on)
      --quote-style S        double | preserve (default double)
      --operator-position P  leading | trailing, for every broken operator chain (default leading)
      --turtle-layout L      diff | conventional (default diff; Phase 2)
      --align-values, --no-align-values
                             Align multi-variable VALUES rows (default off; Phase 2)
      --config PATH          Use this config file (no discovery)
      --no-config            Ignore config files
      --ignore-path PATH     Ignore file (repeatable; default ./.sparklesfmtignore)
      --sort-memory SIZE     Line formats: in-memory sort budget before spilling (default 1GiB)
      --threads N            Files formatted in parallel (default: available cores)
```

- **Modes.** As in Prettier, the default mode prints the formatted output to stdout.
  `--check`, `--list-different` and `--write` are mutually exclusive (clap
  `conflicts_with`).
- **Exit codes** are Prettier's:
  - `0`: every file is formatted, or was written;
  - `1`: `--check` or `-l` found at least one file that would change;
  - `2`: an error, such as a syntax error, refused unsafe output (§6), an I/O error, an
    unsupported language or a usage error.

  A run with both unformatted files and errors exits `2`. Every file is still processed,
  so one run reports all problems.
- **`--check` output** goes to stderr, like Prettier:
  ```
  Checking formatting...
  [warn] queries/people.rq
  [warn] shapes/person.ttl
  [warn] Code style issues found in 2 files. Run sparkles fmt --write to fix.
  ```
  `--diff` adds a unified diff for each changed file on stdout (`similar`, 3 lines of
  context, `--- a/path` and `+++ b/path` headers).
- **Errors** use the compiler format `path:LINE:COL: error: message`. Lines and columns
  are 1-based, and columns count Unicode scalar values. For example:
  `queries/bad.rq:3:14: error: SPARQL syntax error: expected one of …`. When the formatter
  refuses unsafe output (§6), it prints
  `path: error: formatter refused its own output (algebra differs); input left unchanged; please report`.
- **`--write`** writes to a temporary file in the same directory, `fsync`s it and renames
  it over the original, keeping the permissions. Unchanged files keep their mtime.
- **Explicit files** are formatted by extension or `--language`, even when they have an
  unknown extension. An explicit file that the ignore file matches is skipped silently, as
  in Prettier. `.gitignore` applies only to directory walks.

### 2.2 Configuration and ignore files

- **Config file (Phase 1).** The file is `.sparklesfmt.toml`, or `sparklesfmt.toml`, the
  way rustfmt accepts both spellings. The formatter finds it by walking up from each
  file's directory, and the nearest file wins. Files are not merged, and CLI flags
  override the file. The config file moved up from Phase 2 once the style keys below
  existed. A style that has to be repeated as flags on every call drifts between CI,
  editors and the command line.

  ```toml
  line-width = 100               # 40..=400
  indent-width = 2               # 1..=8
  sort = false                   # opt-in (N15, N20, N21 terms; Phase 2); --canonicalize always sorts
  prune-prefixes = false         # N25 (Phase 2)
  directive-style = "sparql"     # "sparql" | "turtle"                    (N6)
  prefix-groups = []             # e.g. [["rdf", "rdfs", "xsd", "owl"]]    (N5)
  type-shorthand = true          # rdf:type → a, a entries first           (N8, N14)
  compact-iris = true            # full IRI → prefixed name                (N7)
  quote-style = "double"         # "double" | "preserve"                   (N10)
  operator-position = "leading"  # "leading" | "trailing"                  (§4.1)
  turtle-layout = "diff"         # "diff" | "conventional"                 (§4.2; Phase 2)
  align-values = false           # align multi-variable VALUES rows        (§4.1; Phase 2)
  ```

  The table lists each key with its flag and the languages it affects. The bullets after it
  describe what each style key switches.

  | Key | Type, default | CLI flag | Languages | Phase |
  |---|---|---|---|---|
  | `line-width` | integer 40..=400, `100` | `--line-width N` | all | 1 |
  | `indent-width` | integer 1..=8, `2` (spaces; never tabs) | `--indent-width N` | all | 1 |
  | `sort` | boolean, `false` | `--sort` / `--no-sort` | Turtle, TriG, JSON-LD, N-Triples, N-Quads | 2 |
  | `prune-prefixes` | boolean, `false` | `--prune-prefixes` | SPARQL, Turtle, TriG | 2 |
  | `directive-style` | `"sparql"` \| `"turtle"`, `"sparql"` | `--directive-style S` | Turtle, TriG | 1 |
  | `prefix-groups` | array of arrays of labels, `[]` | `--prefix-group LABELS` (repeatable) | SPARQL, Turtle, TriG | 1 |
  | `type-shorthand` | boolean, `true` | `--type-shorthand`, `--no-type-shorthand` | SPARQL, Turtle, TriG | 1 |
  | `compact-iris` | boolean, `true` | `--compact-iris`, `--no-compact-iris` | SPARQL, Turtle, TriG | 1 |
  | `quote-style` | `"double"` \| `"preserve"`, `"double"` | `--quote-style S` | SPARQL, Turtle, TriG | 1 |
  | `operator-position` | `"leading"` \| `"trailing"`, `"leading"` | `--operator-position P` | SPARQL | 1 |
  | `turtle-layout` | `"diff"` \| `"conventional"`, `"diff"` | `--turtle-layout L` | Turtle, TriG | 2, if easy |
  | `align-values` | boolean, `false` | `--align-values`, `--no-align-values` | SPARQL | 2, if easy |

  - **`directive-style`.** `"sparql"` prints `PREFIX`/`BASE`/`VERSION` without a final
    `.` and writes TriG graph blocks as `GRAPH g {`. `"turtle"` prints
    `@prefix`/`@base`/`@version` with the final ` .` and writes graph blocks as `g {`,
    without `GRAPH`. One key sets both, so a file never mixes the two families, and a
    mixed input is normalized entirely to the chosen style. The key does not affect SPARQL.
    Its grammar has only `PREFIX`/`BASE`, and `GRAPH` is mandatory in a query (§4.2,
    §5 N6).
  - **`prefix-groups`.** Each inner array is one group of prefix labels, and the groups
    keep the order they are listed in. The formatter applies them to each run of prefix
    declarations that N5 may sort (§5 N5):
    1. the declarations whose label is listed go into their group, in configured group
       order;
    2. every unlisted label goes into one final group;
    3. each group is sorted by label in codepoint order (the empty label first), whatever
       order the config lists the labels in;
    4. empty groups are skipped, and exactly one blank line separates two non-empty
       groups.

    With groups configured, the formatter owns the blank lines inside a run and drops any
    source blank lines between its declarations. With `[]`, the default, there is one
    group and N5 applies unchanged. A blank line between two declarations then acts as a
    sort barrier, so hand-made groups survive, the way `gofmt` keeps import blocks that
    blank lines separate. In both cases `VERSION` still comes first and `BASE` still ends
    a run. A run that N5 leaves unsorted, because a label is bound twice to different
    IRIs, is not regrouped.

    The empty label is written `""`. Every label must match `PN_PREFIX` or be `""`, no
    label may appear twice across all groups, and no group may be empty. On the command
    line, each `--prefix-group rdf,rdfs,xsd,owl` is one group, in order, and any
    `--prefix-group` replaces the file's whole list. The groups apply to SPARQL prologues
    and Turtle/TriG directive runs alike.
  - **`type-shorthand`.** `false` turns off N8 and N14 together. `rdf:type` stays spelled
    as written, subject to `compact-iris`, and entries keep their source order. An `a` in
    the input stays `a`, because the reverse rewrite is never done. `--sort` still puts
    the `a`/`rdf:type` entries first (N15).
  - **`compact-iris`.** `false` turns off N7: every `IRIREF` is printed as written.
  - **`quote-style`.** `"preserve"` turns off N10: every string keeps its quotes and
    escapes as written.
  - **`operator-position`.** This key sets where the operator goes when the printer
    breaks between the operands of a binary operator. `"leading"` puts it at the start of
    the continuation line, and `"trailing"` at the end of the line before. It applies to
    every broken chain in every context: `FILTER`, `BIND`, `HAVING`, projection
    expressions, `ORDER BY`, function arguments and nested chains. In this spec only `||`
    and `&&` chains ever break (§4.1). If another operator becomes breakable, it follows
    the same key. The indentation is the same in both positions (§4.1).
  - **`turtle-layout`** (Phase 2, only if it stays cheap). `"conventional"` prints
    multi-line Turtle/TriG statements in the compact form SPARQL uses: `s p₁ o₁ ;`,
    further entries at +1, ` .` after the last entry, and no trailing `;` (§4.2).
  - **`align-values`** (Phase 2, only if it stays cheap). `true` pads the cells of
    multi-variable `VALUES` rows into columns (§4.1).
  - **Validation.** An unknown key is an error, which catches typos. Enumerated values
    are exact lowercase strings. A wrong type or an out-of-range value is an error that
    names the key and the file. The flags (exit 2) and the HTTP options (400
    `bad-request`) get the same checks.

  These twelve keys are the whole configuration. A `[fmt]` table in a project-wide
  `sparkles.toml` was considered. Sparkles has no project-level config file to share it
  with, so a dedicated dotfile is simpler, like `.prettierrc` and `rustfmt.toml`.
  `.editorconfig` is not read (§13). The HTTP endpoint never reads config files, and
  callers pass options instead (§2.4).
- **Ignore files.** `.sparklesfmtignore` uses gitignore syntax. The formatter reads it
  from the current directory, or from `--ignore-path`. Directory walks also apply
  `.gitignore`, `.git/info/exclude` and the global gitignore. These are the `ignore`
  crate's defaults, with hidden files included. Walks never enter `.git`,
  `node_modules` or `target`.
- **Pragmas** (§3.4):
  - `# sparkles-fmt: ignore` keeps the next node verbatim;
  - `# sparkles-fmt: ignore-file` in the file header keeps the whole file verbatim.

### 2.3 Language detection

The first rule that matches wins:
1. `--language`, the `language` parameter, or the JSON body's `language` field.
2. The extension of the path or `--stdin-filepath`, case-insensitive:

| Extension | Language |
|---|---|
| `.rq`, `.ru`, `.sparql` | `sparql` (query or update, decided by the parse) |
| `.ttl`, `.turtle` | `turtle` (SHACL shapes graphs included) |
| `.trig` | `trig` |
| `.nt` | `ntriples` |
| `.nq` | `nquads` |
| `.jsonld` | `jsonld` (Phase 2) |
| `.rdf`, `.owl`, `.xml` | `rdfxml`. Skipped in walks. An explicit path is an error (§4.5). |
| `.n3`, `.shc`, `.json`, `*.gz\|.zst\|.br\|.lz4` | Skipped in walks. An explicit path needs `--language`. Compressed files are an error ("decompress first"). |

3. **Content sniffing**, for stdin without a name or a file with an unknown extension. It
   looks at the first significant token after comments and whitespace:
   - `{` or `[` → `jsonld`;
   - `<?xml` or `<rdf:RDF` → `rdfxml` (error);
   - after a prologue of `PREFIX`/`BASE`/`VERSION` lines, a SPARQL query form or update
     keyword (`SELECT`, `CONSTRUCT`, `ASK`, `DESCRIBE`, `INSERT`, `DELETE`, `WITH`,
     `LOAD`, `CLEAR`, `DROP`, `CREATE`, `ADD`, `MOVE`, `COPY`) → `sparql`;
   - a statement with four terms before `.` → `nquads`;
   - `GRAPH` or a `{` at statement level → `trig`;
   - otherwise → `turtle`.

   Sniffing never yields `ntriples`. A document with one triple per line is also valid
   Turtle, and N-Triples formatting rewrites terms into their canonical spelling, with
   lowercase tags and different escapes (§4.3). N-Triples needs `.nt` or `--language`.

### 2.4 HTTP: `POST /$/format`

```
POST /$/format?language=sparql&lineWidth=100&indentWidth=2&sort=false&prunePrefixes=false
```

Every §2.2 key is also an HTTP option, spelled in camelCase: `directiveStyle`,
`prefixGroups`, `typeShorthand`, `compactIris`, `quoteStyle`, `operatorPosition`,
`turtleLayout` and `alignValues`. Options go in the JSON body's `options` object or in the
query string. In the query string, each `prefixGroup=rdf,rdfs,xsd,owl` is one group, and
the parameter repeats in order. In JSON, `prefixGroups` is an array of arrays. Omitted
options take the §2.2 defaults. The server reads no config file and the UI sends no style
options, so the UI formats in the default style.

The endpoint accepts two body forms.

1. **JSON (the UI).** `Content-Type: application/json`:
   ```json
   { "text": "select * { ?s ?p ?o }", "language": "sparql", "cursorOffset": 9,
     "options": { "lineWidth": 100 } }
   ```
   `200 application/json`:
   ```json
   { "text": "SELECT *\nWHERE {\n  ?s ?p ?o .\n}\n", "changed": true, "language": "sparql",
     "cursorOffset": 11, "warnings": [] }
   ```
   `cursorOffset` counts **UTF-16 code units**, the editor's native unit, in both
   directions. The server converts it to and from byte offsets. `warnings` holds objects
   `{code, message, line, column}`, for example `undeclared-prefix` or `comment-moved`
   (§3.3).
2. **Raw (curl).** The body is the document, and its media type sets the language when
   `language` is absent:

   | Media type | Language |
   |---|---|
   | `application/sparql-query`, `application/sparql-update` | `sparql` |
   | `text/turtle` | `turtle` |
   | `application/trig` | `trig` |
   | `application/n-triples` | `ntriples` |
   | `application/n-quads` | `nquads` |
   | `application/ld+json` | `jsonld` |
   | `text/plain` | needs `language` |

   The response is `200` with the same media type and the formatted text as the body.
   The `Sparkles-Format-Changed: true|false` header says whether the text changed.

Errors are JSON. As with every other error, `error_request_id` adds `requestId`.

| Status | Body | When |
|---|---|---|
| 400 | `{error, detail?, line, column, code:"syntax", language}` | The reference parser rejects the input. The body has the same shape as a query syntax error (`syntax_error_body` in `http.rs`), with a 1-based line and column. |
| 400 | `{error, code:"bad-request"}` | An unknown language, a bad option, or a `cursorOffset` beyond the end of the text. |
| 413 | `{error}` | The body is larger than `--format-max-bytes` (default 16 MiB). This limit is independent of the global 8 GiB limit. |
| 415 | `{error}` | The media type is unsupported. `application/rdf+xml` gets the §4.5 message. |
| 408 | `{error}` | The deadline (`--format-timeout`, default 10 s) passed. The printer and check loops test it. |
| 422 | `{error, code:"unsafe-format" \| "unstable-format" \| "unsupported-syntax"}` | The self-check failed, or the CST parser does not support a construct that the reference parser accepted (§6). The server logs it at `warn` with the input's SHA-256, never the text. |

**Execution.** The request runs in `spawn_blocking` behind a semaphore with
`available_parallelism()` permits. A request waits for a permit until its deadline.
Over HTTP, line formats are formatted in memory, with no external sort.

**Authorization.** `/$/format` reads no dataset. `auth/routes.rs` adds it to `ROUTES` as
`("/$/format", &["POST"])` with `Need::Caller`, like `/$/server`. Anonymous callers can
use it whenever the server admits anonymous callers. Operators can lock it down with
`serve --format-endpoint on|authenticated|off` (default `on`):
- `authenticated`: the handler answers 401 for the anonymous principal;
- `off`: 404.

The check lives in the handler because `need()` is a pure function of the route.
Cookie-session POSTs still go through the existing CSRF check, and the UI's `request()`
already sends the CSRF header.

**Rate limiting.** `ratelimit::classify` treats every `/$/` POST as `Admin`, so
`/$/format` needs an explicit case, `Some(Class::Query)`, before that fallback.
Formatting is cheap, read-like CPU work, not an admin mutation.

**Registration.** `http.rs` adds `.route("/$/format", post(format::format))`, and a new
`http/format.rs` holds the handler. The router test that checks every template against
`ROUTES` covers it.

### 2.5 UI

- **Format button.** A **Format** button (icon `wand`) goes in the query toolbar before
  **Explain**, with the title `Format (Shift+Alt+F)`. It is enabled whenever the editor
  has text, even with no dataset selected.
- **Keymap** (`SparqlEditor.svelte`, `Prec.highest`):
  `{ key: 'Shift-Alt-f', run: () => (onformat(), true), preventDefault: true }`. This is
  VS Code's binding. `Mod-Shift-f` is not bound, because Firefox and some IMEs use it.
- **Flow** (`+page.svelte`):
  1. `const {text, cursorOffset} = editor.snapshot()`.
  2. `api.format({text, language: 'sparql', cursorOffset})`.
  3. If the document changed meanwhile (the string differs from `text`), drop the result.
  4. Otherwise `editor.replaceFormatted(newText, newCursor)` computes a minimal change
     (common prefix and suffix trimmed) and dispatches one transaction with
     `userEvent: 'input.format'`. Undo restores the original in one step, and the scroll
     position stays put. The selection collapses to the mapped cursor.
- **Errors.**
  - `400 syntax`: `editor.showError(line, column)` plus a toast,
    "Can't format: syntax error at line L".
  - `422`: toast "The formatter could not format this query safely; it was left
    unchanged".
- **Format on run.** A checkbox in a small chevron menu on the Format button turns it on.
  The setting is stored per viewer through `storage.ts` (`sparkles.formatOnRun`) and is
  **off** by default. When it is on, **Run** formats first. If formatting fails for any
  reason, the query runs unformatted and the run reports any syntax error as it does now.
- **Mock.** `ui/mock/server.mjs` answers `/$/format` by echoing the text and cursor with
  `changed: false`, so UI work does not need the Rust server.
- **Tests.** Vitest covers the minimal-change helper, cursor placement and the stale-result
  guard.
- **Later.** In Phase 3, WASM formatting in the browser replaces the round trip, and the
  endpoint stays as the fallback. In Phase 2, other text areas such as the validation
  shapes get the button.

## 3. Formatting model (shared by all languages)

### 3.1 Invariants: what formatting may change

The formatter may change **only** these things:
1. trivia: whitespace, line breaks and indentation;
2. comment placement, as §3.3 allows;
3. the normalizations listed in §5.

Every other token is printed exactly as written: IRIs, prefixed names, variables,
blank-node labels, string contents and escapes, numbers, language tags, and parentheses.
Tokens are never split or merged, except the rewrites §5 lists (for example
`?v+1` → `?v + 1`, §5 N12).

Output uses LF line endings, has no trailing whitespace, and ends with exactly one final
newline. A UTF-8 BOM is dropped. Input must be UTF-8, which every covered syntax
requires. CR characters inside long strings are content and are kept. Tabs never appear
in the output except inside strings.

### 3.2 Document IR and printer

Each language turns its CST into one document IR. A single printer lays it out with the
classic algorithm:
- Wadler's "prettier printer";
- Lindig's strict, imperative variant;
- extended the way Prettier's doc printer is (Prettier `commands.md`).

```rust
enum Doc {                         // arena-allocated; ids are u32
    Text(TextRef),                 // token text, remembering the source token id (cursor mapping)
    Line,                          // " " when flat, newline+indent when broken
    SoftLine,                      // "" when flat, newline+indent when broken
    HardLine,                      // always a newline; forces every enclosing group to break
    EmptyLine,                     // a blank line (collapses with adjacent EmptyLine)
    Indent(DocId),                 // +indent_width for lines inside
    Group(DocId, GroupId),         // print flat if it fits in the remaining width, else break
    IfBreak { broken: DocId, flat: DocId, group: Option<GroupId> },
    LineSuffix(DocId),             // trailing comment: flushed just before the next newline
    BreakParent,                   // forces the enclosing groups to break
    Concat(Vec<DocId>),
    Verbatim(TextRef),             // pragma-ignored source slice (§3.4)
}
```

- **Printing.**
  - A `propagate_breaks` pre-pass marks every group that contains `HardLine` or
    `BreakParent`.
  - The printer keeps a stack of `(indent, mode, doc)` commands. A group is flat when
    `fits(rest_of_line)` holds. `fits` stops at the first line break of the rest of the
    stream, as in Prettier.
  - Width is measured with `unicode-width`.
  - The cost is linear in output size, times a factor bounded by the line width.
- **`Fill` is not supported.** Filling several items per line is hostile to diffs, so a
  broken list always has one item per line.
- **Size.** The printer is about 600 lines. The `pretty` crate (MIT) was considered, but
  it has no line suffix and no break propagation, and trailing comments need both (§12).

### 3.3 Comments and blank lines

SPARQL, Turtle, TriG, N-Triples and N-Quads have only `#` line comments. They are
recognized outside IRIs and strings and run to the end of the line (SPARQL 1.1 §19.4,
Turtle §6.2, N-Triples 1.2 §5.2). The lexer is lossless: every byte belongs to a token,
a whitespace run or a comment.

**Attachment nodes.** Only certain CST nodes own comments:
- **SPARQL:**
  - prologue declarations;
  - query and update clauses (`SELECT`, dataset clause, `WHERE`, each solution
    modifier, the trailing `VALUES`);
  - each update operation;
  - each group element (a triples statement, `OPTIONAL`, `MINUS`, `UNION` branch,
    `GRAPH`, `SERVICE`, `FILTER`, `BIND`, `VALUES`, subquery);
  - inside a triples statement: each predicate-object entry and each object;
  - each entry of a `[ … ]` or `{| … |}` block, and each collection item;
  - each projection item, `VALUES` row or value, `GROUP BY`/`ORDER BY` condition,
    function argument, and each operand of an `&&`/`||` chain.
- **Turtle/TriG:**
  - directives, statements and graph blocks;
  - each predicate-object entry and each object;
  - each item of a collection, blank-node property list or annotation block.
- **Line formats:** each statement line.

**Classification.** Let `P` be the last significant token before comment `c` and `N`
the first after it. For attachment, a separator (`,` `;` `.`) counts as part of the item
before it, and so does the `&&` or `||` between two operands of a chain.
1. **File header.** Comments before the first significant token are the file header.
   They stay at the top, keep their internal blank-line groups, and are never attached to
   the first node, so sorting prefixes or lines cannot carry them away. One exception was
   decided during implementation (2026-09-30). When the header block ends with
   `# sparkles-fmt: ignore` directly before the first node, that last block leads the
   node instead. The pragma then stays with the node it protects even if sorting moves
   the node. The header is followed by one blank line or none, as written. Line formats
   always add one blank line after a non-empty header.
2. **Trailing.** `c` shares a line with `P`. It attaches to the outermost attachment node
   whose last significant token, ignoring separators, is `P`. For example:
   - `ex:o ; # c` attaches to the predicate-object entry;
   - `ex:o . # c` attaches to the statement;
   - `(?a, # c` attaches to the argument `?a`.

   It is printed as `LineSuffix(" " + c)` after the node's separator. It also adds
   `BreakParent`, because a line comment ends its line and the enclosing group cannot be
   flat.
3. **Leading.** `c` is on its own line, with only whitespace before it. Consecutive
   own-line comments with no blank line between them form a **comment block**.
   - A block followed directly by `N`, with no blank line, is `N`'s **leading block**. It
     attaches to the outermost attachment node that starts at `N`, within the innermost
     node containing both `P` and `N`. It is printed on its own lines at that node's
     indentation, and adds `BreakParent`.
   - A block followed by a blank line is **detached**. It becomes a comment pseudo-node,
     a sibling in the list, and keeps its blank lines on both sides, collapsed to one.
     A `# --- People ---` section header is a typical detached block.
4. **Dangling.** `N` is a closing bracket (`}` `]` `)` `|}` `>>` `)>>`) or the end of
   the file. The block belongs to the innermost container and is printed on its own
   lines at the container's inner indentation, just before the closing bracket.
5. **Displaced.** The comment sits where no attachment node starts or ends, for example
   between `FILTER` and `(`, inside `<<( … )>>`, between `^^` and a datatype, between
   `ORDER` and `BY`, or inside a prefix declaration. It becomes a leading comment of the
   nearest enclosing attachment node, and a `comment-moved` warning gives its line.
   Comments are never dropped.

**Comment text** is kept verbatim except for trailing whitespace, which is removed.
Nothing is inserted after `#`. Own-line comments are re-indented to their node.

**Blank lines.** A node in a list gets one empty line before it when the source had at
least one empty line between the previous sibling's end (including its trailing comment)
and the start of this node (including its leading block). Longer runs collapse to one.
Blank lines at the start or end of a `{ }`, `[ ]`, `( )` or `{| |}` are removed.

The formatter adds blank lines only in these places:
- one after the prologue or directive block when a body follows;
- one between SPARQL update operations;
- one between two statements (top-level or directly inside a TriG graph block) when
  either of them prints on more than one line.

**Moving nodes.** A node that moves, through sorting or `a`-first (§5), carries its
leading block and trailing comment with it. Detached comment blocks and directives are
**sort barriers**: sorting happens only within the runs between them, so a
`# --- People ---` section keeps its members. Deduplication (§5) removes only a node that
has no comments of its own, so when two identical statements both carry comments, both
are kept.

### 3.4 Pragmas

- **`# sparkles-fmt: ignore`** as the last line of a node's leading block. The node is
  printed as `Verbatim`:
  - its first line starts at the node's computed indentation;
  - later lines are copied byte for byte;
  - no normalization applies inside it.

  Use it for hand-aligned `VALUES` tables. The node still takes part in the safety check
  and in sorting, where it sorts by its source text.
- **`# sparkles-fmt: ignore-file`** anywhere in the file header. The output is the input
  unchanged, and `--check` passes.

### 3.5 Cursor mapping (Prettier's `cursorOffset`)

The printer records the output offset of every printed source token. Comments are tokens
here, and they survive one-to-one.

For a cursor at byte offset `o` in the input:
- **Inside token `t`,** at distance `d` from its start: map to `out(t) + min(d, len'(t))`.
  `len'` is the printed length, since normalization can change it (`rdf:type` becomes
  `a`, `"42"^^xsd:integer` becomes `42`).
- **In trivia between `t₁` and `t₂`:** map to the end of `t₁`. If the trivia contains a
  newline, map to the start of `t₂` instead.
- **Before the first token:** `0`. **After the last:** the end.

A token that a normalization removes, such as a deduplicated prefix or an optional
`WHERE`/`.`/`;`, maps to the next printed token. The HTTP layer converts between UTF-16
and bytes.

## 4. Rules per language

The rules and examples below assume the defaults: line width 100, indent width 2, and the
default of every other §2.2 key. "Fits" means the group fits in the rest of the line. The
effect of each non-default key is described with the rule it changes.

### 4.1 SPARQL 1.2 (queries and updates)

**Prologue**
- One declaration per line: `VERSION "1.2"`, then `BASE <…>`, then `PREFIX p: <…>`.
  There is one space after the label and no alignment.
- `VERSION` moves first.
- `PREFIX` declarations are sorted and deduplicated within runs that `BASE` does not
  split (§5 N5). With `prefix-groups`, each run is printed as its groups, one blank line
  between groups (§2.2).
- One blank line separates the prologue from the body.

**Query form line**
- `SELECT [DISTINCT|REDUCED] <projection>` is a group. When it does not fit, `SELECT`
  (with `DISTINCT` or `REDUCED`) stays alone on its line, and each projection item goes
  on its own line at +1 indent.
- `CONSTRUCT {`, `DESCRIBE <terms>`, `ASK {` and `CONSTRUCT WHERE {` follow the same
  pattern.

**Clauses**
- Each dataset clause (`FROM <g>`, `FROM NAMED <g>`) goes on its own line.
- `WHERE {` starts a line. `WHERE` is inserted for `SELECT`, `CONSTRUCT` with a template,
  and `DESCRIBE` with a pattern, and removed after `ASK` (§5 N13).
- Solution modifiers go one per line, each starting at column 0 of the query's level:
  `GROUP BY c₁ c₂`, `HAVING(expr)`, `ORDER BY c₁ c₂`, `LIMIT n`, `OFFSET n`.
  `LIMIT` comes before `OFFSET` (§5 N14). The final `VALUES` block follows.

**Group graph patterns**
- A group is always expanded: `{`, the elements one per line at +1 indent, then `}`.
  The only exception is the empty group, printed `{}`.
- Each element goes on its own line:
  - a triples statement ends with ` .`, including the last one before `}`;
  - other elements get no `.` (§5 N13).
- Block elements open on the keyword's line: `OPTIONAL {`, `MINUS {`, `GRAPH ?g {`,
  `SERVICE [SILENT] <x> {`.
- A union chain is printed `{` … `} UNION {` … `}`.
- A subquery is printed as `{`, then the indented query, then `}`.

**Filters, BIND and VALUES**
- A keyword followed by the `(` of its bracketed argument takes no space:
  `FILTER(expr)`, `BIND(expr AS ?v)`, `HAVING(expr)`. Other forms take one space:
  `FILTER REGEX(…)`, `FILTER NOT EXISTS {`, `FILTER EXISTS {`, `FILTER <iri>(…)`.
- `VALUES ?x { a b c }` stays inline when it fits and has no comments. Otherwise it
  breaks with one value per line.
- `VALUES (?x ?y) {` always puts one row per line, printed `(v₁ v₂)`. By default columns
  are not aligned, because alignment is hostile to diffs.
- **`align-values = true`** (Phase 2, if easy) aligns the rows of a multi-variable
  `VALUES` block into columns. Every cell except the last in its row is padded with
  spaces to the width of the widest cell in its column, measured with `unicode-width`,
  and then followed by one space. The `(` and `)` of each row stay tight. The variable
  list on the `VALUES (…) {` line is not aligned with the rows. A block stays unaligned
  when any row is `ignore`d or holds a line break (a long string), or when the aligned
  rows would exceed the line width. Comments take no part in alignment, and a trailing
  comment follows its row's `)`.
  ```sparql
    VALUES (?s ?currency) {
      (<https://example.org/p1> "EUR")
      (ex:p2                    UNDEF)
    }
  ```

**Triples**
- **Subject blocks.** A statement with one predicate and one object that fits is printed
  flat: `?s ex:p ?o .`. Otherwise it uses the compact broken form:
  - the subject and the first predicate-object entry on the first line;
  - each further entry on its own line at +1 indent;
  - `;` at the end of each non-final line and ` .` at the end of the last.

  SPARQL keeps this conventional form, not the Turtle trailing-`;` form. Queries are
  short and read in editors more than in diffs (§13). A trailing `;` in the input is
  removed (§5 N13).
- **Object lists** stay inline (`o₁, o₂`) when they fit on the entry's line. Otherwise
  the predicate stays on its line, and the objects go one per line at +2 indent relative
  to the entry, each followed by `,` except the last.
- **Blank-node property lists** `[ p o ]` stay inline only when they hold a single entry
  with a single object and fit. Otherwise:
  - `[` stays at the end of the owning line;
  - the entries go at +1 indent relative to that line, separated by ` ;`, with no
    trailing `;`;
  - `]` goes at the owning line's indent.

  Several bracketed objects hug: `], [`.
- **Collections** `( a b c )` stay inline when they fit. Otherwise `(`, one item per line
  at +1 indent, then `)`.
- **RDF 1.2 terms.**
  - Triple terms and reified triples are printed inline with single spaces:
    `<<( s p o )>>`, `<< s p o ~ r >>`.
  - Reifiers are printed ` ~ r` (or ` ~` for an anonymous reifier).
  - Annotation blocks `{| … |}` follow the `[ … ]` rules.
- **Property paths** are printed tight, e.g. `a/rdfs:subClassOf*`, `^ex:p`,
  `(ex:a|ex:b)+`, `!(rdf:type|^rdf:type)`. A path element is always followed by a space,
  so `?`, `*` and `+` never merge with the next token.

**Expressions**
- Binary operators get single spaces: `?a + ?b`, `?x >= 3`, `?x IN (1, 2)`,
  `?x NOT IN (…)`. Unary operators are tight: `!BOUND(?x)`, `-?x`.
- Call syntax takes no space: `STR(?x)`, `ex:fn(?a, ?b)`, `COUNT(DISTINCT ?x)`,
  `GROUP_CONCAT(?n; SEPARATOR = ", ")`. Arguments are separated by `, `.
- A projection expression is printed `(expr AS ?v)`.
- **Breaking.**
  - A maximal `||` or `&&` chain is one group. When it breaks, each operand goes on its
    own line. The first operand stays where the chain starts. Every later operand goes on
    a **continuation line**, one indent level deeper than the first operand's line, so the
    chain reads as one unit. With `operator-position = "leading"`, the default, the
    operator starts the continuation line (`&& ?b`). With `"trailing"`, it ends the line
    before, after one space (`?a &&`), and the continuation lines are indented the same
    way:
    ```sparql
      FILTER(
        ?price > 10
          && ?currency != "GBP"
      )
    # operator-position = "trailing":
      FILTER(
        ?price > 10 &&
          ?currency != "GBP"
      )
    ```
  - The rule nests. A chain that is an operand of another chain starts on its operand's
    line, and its own continuation lines go one level deeper than that line. A broken
    bracketed operand opens `(` on its operand's line, puts its content one level deeper,
    and closes `)` at that line's indent. In this example the operands are shortened, and
    both chains are assumed too long to fit:
    ```sparql
      FILTER(
        ?a
          || (
            ?b
              && ?c
          )
      )
    ```
  - A comment after an operator is attached to the operand before it (§3.3). It prints
    after that operand when operators lead, and after the operator when they trail.
  - Argument lists, bracketed expressions and `IN` lists break as `(`, the items one per
    line at +1 indent, then `)`. There is no trailing comma, since the grammar allows none.
  - Relational, additive and multiplicative operators never break. A long operand breaks
    inside its own argument list.
  - In a broken `BIND(`, `AS ?v` stays on the expression's last line.
- Parentheses are kept exactly as written (§5 N17).

**Updates**
- Operations are separated by `;` directly after the operation's last token (`};`) and
  one blank line. A trailing `;` at the end of the request is removed (§5 N13).
- `WITH <g>` goes on its own line, and so does each `USING [NAMED] <g>`.
- The blocks are printed `DELETE {` … `}`, `INSERT {` … `}`, `WHERE {` … `}`, as
  `DELETE WHERE {`, `INSERT DATA {`, `DELETE DATA {`, and `GRAPH <g> {` inside quad data.
- These operations take one line each: `LOAD [SILENT] <x> [INTO GRAPH <g>]`,
  `CLEAR`/`DROP`/`CREATE` …, and `ADD`/`MOVE`/`COPY a TO b`.

**Keywords** are printed in the spelling of the terminal in the SPARQL 1.2 grammar
(§19.7):
- `SELECT`, `WHERE`, `OPTIONAL`, `LANGMATCHES`, `GROUP_CONCAT`, `SEPARATOR`, `UNDEF`;
- `sameTerm`, `isIRI`, `isURI`, `isBLANK`, `isLITERAL`, `isNUMERIC`;
- `hasLANG`, `hasLANGDIR`, `isTRIPLE`, `LANGDIR`, `STRLANGDIR`, `TRIPLE`, `SUBJECT`,
  `PREDICATE`, `OBJECT`.

`a`, `true` and `false` stay lowercase (§5 N3).

**Variables** keep the sigil they were written with: `$this` stays `$this`, and `?x`
stays `?x` (§5 N4). SHACL-SPARQL and SPIN use `$` to mark pre-bound variables, and
rewriting it would erase that signal from `.rq` templates.

**Codepoint escapes.** SPARQL 1.2 allows `\u`/`\U` only inside IRIs and strings. The
`sparql12/codepoint-escapes` tests cover this, and `codepoint-esc-0[1-4]-bad` must fail.
The lexer follows 1.2 and keeps escapes verbatim.

#### Example S1: SELECT with OPTIONAL, FILTER, UNION and comments

Input:
```sparql
prefix foaf: <http://xmlns.com/foaf/0.1/>
PREFIX ex:<http://example.org/>   # our vocabulary
prefix foaf: <http://xmlns.com/foaf/0.1/>
select distinct ?name ?mbox where {
  # people only
  ?p a foaf:Person; foaf:name ?name .
  optional { ?p foaf:mbox ?mbox } .
  { ?p ex:role ex:Admin } union { ?p ex:role ex:Owner }
  filter( lang(?name)="en" || lang(?name)="" ) # English or untagged
}
order by ?name limit 10
```
Output:
```sparql
PREFIX ex: <http://example.org/> # our vocabulary
PREFIX foaf: <http://xmlns.com/foaf/0.1/>

SELECT DISTINCT ?name ?mbox
WHERE {
  # people only
  ?p a foaf:Person ;
    foaf:name ?name .
  OPTIONAL {
    ?p foaf:mbox ?mbox .
  }
  {
    ?p ex:role ex:Admin .
  } UNION {
    ?p ex:role ex:Owner .
  }
  FILTER(LANG(?name) = "en" || LANG(?name) = "") # English or untagged
}
ORDER BY ?name
LIMIT 10
```
The second `foaf:` declaration is identical and carries no comment, so it is removed.
The `FILTER` stays in its position.

#### Example S2: aggregates, a subquery and property paths

Input:
```sparql
PREFIX ex: <http://example.org/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?type (COUNT(DISTINCT ?item) AS ?n) (GROUP_CONCAT(?label;separator=", ") AS ?labels) (sample(?item) as ?example) WHERE {
  ?item a/rdfs:subClassOf* ?type ; rdfs:label ?label .
  { SELECT ?item WHERE { ?item ex:partOf+ ex:catalog } LIMIT 1000 }
} GROUP BY ?type HAVING (COUNT(DISTINCT ?item) > 5) ORDER BY DESC(?n)
```
Output:
```sparql
PREFIX ex: <http://example.org/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>

SELECT
  ?type
  (COUNT(DISTINCT ?item) AS ?n)
  (GROUP_CONCAT(?label; SEPARATOR = ", ") AS ?labels)
  (SAMPLE(?item) AS ?example)
WHERE {
  ?item a/rdfs:subClassOf* ?type ;
    rdfs:label ?label .
  {
    SELECT ?item
    WHERE {
      ?item ex:partOf+ ex:catalog .
    }
    LIMIT 1000
  }
}
GROUP BY ?type
HAVING(COUNT(DISTINCT ?item) > 5)
ORDER BY DESC(?n)
```

#### Example S3: VALUES, BIND, MINUS, NOT EXISTS, a long FILTER and `$` variables

Input:
```sparql
PREFIX schema: <https://schema.org/>
SELECT $s ?price ?discounted WHERE {
VALUES (?s ?currency) { (<https://example.org/p1> "EUR") (<https://example.org/p2> UNDEF) }
?s schema:price ?price ; schema:priceCurrency ?currency .
BIND(?price * 0.9 AS ?discounted)
FILTER NOT EXISTS { ?s schema:availability schema:Discontinued }
MINUS { ?s schema:category "internal" }
FILTER (?price > "10"^^<http://www.w3.org/2001/XMLSchema#decimal> && ?price < 1000 && contains(lcase(str(?s)), "sale") && ?currency != "GBP")
}
```
Output:
```sparql
PREFIX schema: <https://schema.org/>

SELECT $s ?price ?discounted
WHERE {
  VALUES (?s ?currency) {
    (<https://example.org/p1> "EUR")
    (<https://example.org/p2> UNDEF)
  }
  ?s schema:price ?price ;
    schema:priceCurrency ?currency .
  BIND(?price * 0.9 AS ?discounted)
  FILTER NOT EXISTS {
    ?s schema:availability schema:Discontinued .
  }
  MINUS {
    ?s schema:category "internal" .
  }
  FILTER(
    ?price > "10"^^<http://www.w3.org/2001/XMLSchema#decimal>
      && ?price < 1000
      && CONTAINS(LCASE(STR(?s)), "sale")
      && ?currency != "GBP"
  )
}
```
`"10"^^xsd:decimal` stays as written, because `10` would be an `xsd:integer` (§5 N9).
No `xsd:` prefix is declared, so the datatype stays a full IRI. `$s` keeps its `$` (§5 N4)
and is the same variable as `?s`. With `operator-position = "trailing"` the chain prints:
```sparql
  FILTER(
    ?price > "10"^^<http://www.w3.org/2001/XMLSchema#decimal> &&
      ?price < 1000 &&
      CONTAINS(LCASE(STR(?s)), "sale") &&
      ?currency != "GBP"
  )
```

#### Example S4: CONSTRUCT

Input:
```sparql
PREFIX ex: <http://example.org/>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
CONSTRUCT { ?p rdf:type ex:Contact ; ex:name ?n ; ex:address [ ex:city ?city ; ex:country ?country ] . }
WHERE { ?p ex:fullName ?n . OPTIONAL { ?p ex:city ?city ; ex:country ?country } }
```
Output:
```sparql
PREFIX ex: <http://example.org/>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>

CONSTRUCT {
  ?p a ex:Contact ;
    ex:name ?n ;
    ex:address [
      ex:city ?city ;
      ex:country ?country
    ] .
}
WHERE {
  ?p ex:fullName ?n .
  OPTIONAL {
    ?p ex:city ?city ;
      ex:country ?country .
  }
}
```

#### Example S5: update with DELETE/INSERT WHERE and INSERT DATA

Input:
```sparql
PREFIX ex: <http://example.org/>
# bump versions
with <http://example.org/g> delete { ?d ex:version ?v } insert { ?d ex:version ?v2 ; ex:modified ?now }
where { ?d ex:version ?v . bind(?v+1 as ?v2) bind(now() as ?now) } ;
insert data { graph <http://example.org/log> { ex:run1 ex:note """bumped
all""" } } ;
```
Output:
```sparql
PREFIX ex: <http://example.org/>

# bump versions
WITH ex:g
DELETE {
  ?d ex:version ?v .
}
INSERT {
  ?d ex:version ?v2 ;
    ex:modified ?now .
}
WHERE {
  ?d ex:version ?v .
  BIND(?v + 1 AS ?v2)
  BIND(NOW() AS ?now)
};

INSERT DATA {
  GRAPH ex:log {
    ex:run1 ex:note """bumped
all""" .
  }
}
```
In `?v+1` the lexer sees `?v` followed by the signed literal `+1`. Printing it as
`?v + 1` is §5 N12. The long string is copied byte for byte. The trailing `;` is removed.

#### Example S6: SPARQL 1.2 reification, comments in a list, and an ignore pragma

Input:
```sparql
PREFIX ex: <http://example.org/>
VERSION "1.2"
SELECT ?who ?since WHERE {
  ?who ex:knows ?other ~ ?r {| ex:since ?since |} .
  << ?who ex:worksFor ?org ~ ?claim >> ex:source ?src .
  ?claim ex:asserted <<( ?who ex:worksFor ?org )>> .
  VALUES ?org {
    ex:acme   # main customer
    ex:globex
  }
  # sparkles-fmt: ignore
  VALUES (?a     ?b) {
         (1      2)
         (10     20) }
}
```
Output:
```sparql
VERSION "1.2"
PREFIX ex: <http://example.org/>

SELECT ?who ?since
WHERE {
  ?who ex:knows ?other ~ ?r {| ex:since ?since |} .
  << ?who ex:worksFor ?org ~ ?claim >> ex:source ?src .
  ?claim ex:asserted <<( ?who ex:worksFor ?org )>> .
  VALUES ?org {
    ex:acme # main customer
    ex:globex
  }
  # sparkles-fmt: ignore
  VALUES (?a     ?b) {
         (1      2)
         (10     20) }
}
```
The trailing comment on `ex:acme` makes the `VALUES` group break.

### 4.2 Turtle 1.2 and TriG 1.2 (and SHACL shapes graphs)

**Directives**
- Directives are SPARQL-style, one per line, with no final `.` (§5 N6):
  `PREFIX p: <…>`, `BASE <…>` and `VERSION "1.2"`.
- With `directive-style = "turtle"` they are `@prefix p: <…> .`, `@base <…> .` and
  `@version "1.2" .` instead. Every directive in the file uses the chosen family, and TriG
  graph blocks follow the same key (see below).
- `VERSION` moves first. `PREFIX` declarations are sorted and deduplicated within a
  directive run (§5 N5). When `prefix-groups` is configured, they are printed in their
  groups, with one blank line between groups (§2.2).
- Directives in the middle of the document stay where they are and act as sort barriers.
- One blank line follows the leading directive block.

**Subject blocks.** A statement is **flat**, `s p o .`, when it has exactly one
predicate-object entry with one object, holds no comments, and fits. Otherwise it uses
the **expanded** form:
```
subject
  p₁ o₁ ;
  p₂ o₂ ;
.
```
- The subject stands alone on its first line.
- Each entry goes on its own line at +1 indent and ends with ` ;`.
- A lone `.` closes the statement at the subject's indentation.

The grammar allows a `;` directly before `.` (Turtle `predicateObjectList`), so
appending a predicate is a one-line diff. TopBraid-generated files use this layout (§14).
A subject that is a blank-node property list with no further predicates prints as
`[` … `] .`.

**`turtle-layout = "conventional"`** (Phase 2, if easy) replaces the expanded form with
the compact form SPARQL uses (§4.1). Most hand-written files and several serializers use
this layout (§14):
```
subject p₁ o₁ ;
  p₂ o₂ .
```
- The subject and the first entry share the first line, and each further entry goes on
  its own line at +1. Every entry but the last ends with ` ;`; the last ends with ` .`.
- There is no trailing `;` (§5 N13) and no lone `.`.
- Blank-node property lists and annotation blocks drop the trailing ` ;` of their last
  entry, as in SPARQL.
- The flat rule, object lists, collections and blank-line rules are unchanged. The
  printer reuses the SPARQL triples printer, which keeps this option cheap. If it turns
  out not to be cheap, the option is dropped rather than approximated.

**Object lists** stay inline (`p o₁, o₂ ;`) when they fit. Otherwise the predicate stays
alone on its line, and the objects go one per line at +1 relative to the predicate. Each
object but the last is followed by `,`, and the last by ` ;`.

**Blank-node property lists** `[ p o ]` stay inline only with a single entry and a
single object that fit. Otherwise they hug:
- `p [` on the entry line;
- the entries at +1, each ending with ` ;`;
- `]` at the entry's indentation, followed by its separator.

Several bracketed objects chain as `], [`.

**Collections** `( a b )` stay inline when they fit. Otherwise `(`, one item per line at
+1, then `)`.

**RDF 1.2 terms**
- Triple terms are printed inline with single spaces: `<<( s p o )>>`.
- Reified triples: `<< s p o >>` or `<< s p o ~ r >>`.
- Reifiers: ` ~ r`.
- Annotation blocks `{| … |}` follow the `[ … ]` rules and hug the object, as in
  `o ~ r {|` … `|} ;`.
- Base direction is kept as written, e.g. `"x"@en--ltr`.

**TriG**
- Graph blocks are printed `GRAPH g {` … `}` (§5 N6). With
  `directive-style = "turtle"` they are printed `g {` … `}`, without `GRAPH`, matching
  `@prefix`. Default-graph `{ … }` blocks and top-level triples stay as written.
- Statements inside a graph block follow the rules above at +1 indent, and each ends with
  `.`, although the grammar makes the last `.` optional.

**Literals**
- Numbers and booleans use the shorthand when the lexical form allows it (§5 N9).
- `'…'` becomes `"…"` (§5 N10). With `quote-style = "preserve"` quotes stay as written.
- Long strings (`"""…"""`) are unbreakable and are copied verbatim, line breaks
  included.

**Terms and entry order under the switches** (§2.2)
- `type-shorthand = false`: `rdf:type` stays as written and `a` entries are not moved
  first (N8 and N14 off). In T1 below, `rdf:type foaf:Person` would stay the second
  entry.
- `compact-iris = false`: full IRIs stay as written (N7 off). In T1,
  `<http://example.org/alice>` would stay.
- Both switches work the same way in SPARQL (N7 and N8), where entries are never
  reordered.

**SHACL.** Shapes graphs are Turtle and get no special rules. SPARQL inside `sh:select`,
`sh:ask`, `sh:update` and `sh:construct` literals is **not** formatted. Changing the
string changes the literal, and so the graph (RDF 1.2 Concepts, literal term equality).
A later `sparkles lint` may check it.

#### Example T1: prefixes, `rdf:type`, literal shorthands, comments and blank lines

Input:
```turtle
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix ex: <http://example.org/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

# --- People ---

<http://example.org/alice> foaf:name 'Alice' ; rdf:type foaf:Person ;   # the founder
    foaf:age "42"^^xsd:integer ; foaf:knows ex:bob , ex:carol .
ex:bob a foaf:Person .
```
Output:
```turtle
PREFIX ex: <http://example.org/>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>

# --- People ---

ex:alice
  a foaf:Person ; # the founder
  foaf:name "Alice" ;
  foaf:age 42 ;
  foaf:knows ex:bob, ex:carol ;
.

ex:bob a foaf:Person .
```
- The `rdf:type` entry becomes `a`, moves first, and takes its trailing comment with it
  (§5 N8, N14).
- `"42"^^xsd:integer` becomes `42`, the same term.
- A blank line is added between a multi-line statement and its neighbor.

#### Example T2: long object lists, collections, nested blank nodes and long strings

Input:
```turtle
PREFIX ex: <http://example.org/>
PREFIX dct: <http://purl.org/dc/terms/>

ex:report dct:title "Quarterly report"@en, "Rapport trimestriel"@fr, "Quartalsbericht"@de, "Informe trimestral"@es ;
  ex:authors ( ex:alice ex:bob ) ; ex:steps ( [ ex:label "collect" ] [ ex:label "clean" ; ex:tool ex:openrefine ] [ ex:label "publish" ] ) ;
  dct:description """Line one
  line two""" .
```
Output:
```turtle
PREFIX dct: <http://purl.org/dc/terms/>
PREFIX ex: <http://example.org/>

ex:report
  dct:title
    "Quarterly report"@en,
    "Rapport trimestriel"@fr,
    "Quartalsbericht"@de,
    "Informe trimestral"@es ;
  ex:authors ( ex:alice ex:bob ) ;
  ex:steps (
    [ ex:label "collect" ]
    [
      ex:label "clean" ;
      ex:tool ex:openrefine ;
    ]
    [ ex:label "publish" ]
  ) ;
  dct:description """Line one
  line two""" ;
.
```

#### Example T3: TriG with RDF 1.2 annotations, reified triples and triple terms

Input:
```trig
PREFIX ex: <http://example.org/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
VERSION "1.2"
ex:g1 { ex:alice ex:knows ex:bob ~ ex:r1 {| ex:since "2020"^^xsd:gYear ; ex:source ex:hr |} .
  ex:r1 ex:confidence 0.9 }
graph ex:g2 { << ex:alice ex:worksFor ex:acme >> ex:statedBy ex:carol . ex:carol ex:says <<( ex:acme ex:size "big"@en--ltr )>> }
```
Output:
```trig
VERSION "1.2"
PREFIX ex: <http://example.org/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>

GRAPH ex:g1 {
  ex:alice
    ex:knows ex:bob ~ ex:r1 {|
      ex:since "2020"^^xsd:gYear ;
      ex:source ex:hr ;
    |} ;
  .

  ex:r1 ex:confidence 0.9 .
}

GRAPH ex:g2 {
  << ex:alice ex:worksFor ex:acme >> ex:statedBy ex:carol .
  ex:carol ex:says <<( ex:acme ex:size "big"@en--ltr )>> .
}
```

#### Example T4: `--sort` (opt-in) with sections

Input:
```turtle
PREFIX ex: <http://example.org/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>

# Classes

ex:Zebra rdfs:subClassOf ex:Animal ; rdfs:label "Zebra" .
# the root class
ex:Animal rdfs:label "Animal" ; a rdfs:Class .

# Properties

ex:weight rdfs:label "weight" ; rdfs:domain ex:Animal .
ex:age rdfs:domain ex:Animal .
```
Output of `sparkles fmt --sort`:
```turtle
PREFIX ex: <http://example.org/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>

# Classes

# the root class
ex:Animal
  a rdfs:Class ;
  rdfs:label "Animal" ;
.

ex:Zebra
  rdfs:label "Zebra" ;
  rdfs:subClassOf ex:Animal ;
.

# Properties

ex:age rdfs:domain ex:Animal .

ex:weight
  rdfs:domain ex:Animal ;
  rdfs:label "weight" ;
.
```
The detached `# Classes` and `# Properties` blocks are sort barriers. `# the root class`
travels with `ex:Animal`.

#### Example T5: a SHACL shapes graph

Input:
```turtle
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix ex: <http://example.org/> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:datatype xsd:string ; sh:minCount 1 ; sh:maxCount "1"^^xsd:integer ] , [ sh:path ex:email ; sh:pattern '^[^@]+@[^@]+$' ] ;
  sh:property [ sh:path ex:status ; sh:in ( "active" "inactive" "suspended" ) ] ;
  sh:sparql [ sh:message "No self-knows" ; sh:select """
    SELECT $this WHERE { $this ex:knows $this }
  """ ] .
```
Output:
```turtle
PREFIX ex: <http://example.org/>
PREFIX sh: <http://www.w3.org/ns/shacl#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>

ex:PersonShape
  a sh:NodeShape ;
  sh:targetClass ex:Person ;
  sh:property [
    sh:path ex:name ;
    sh:datatype xsd:string ;
    sh:minCount 1 ;
    sh:maxCount 1 ;
  ], [
    sh:path ex:email ;
    sh:pattern "^[^@]+@[^@]+$" ;
  ] ;
  sh:property [
    sh:path ex:status ;
    sh:in ( "active" "inactive" "suspended" ) ;
  ] ;
  sh:sparql [
    sh:message "No self-knows" ;
    sh:select """
    SELECT $this WHERE { $this ex:knows $this }
  """ ;
  ] ;
.
```
The two `sh:property` entries are not merged (§5 N19), and the embedded SPARQL is left
untouched.

#### Example T6: T1 under non-default keys

With
```toml
directive-style = "turtle"
prefix-groups = [["rdf", "rdfs", "xsd", "owl"]]
```
the T1 input prints:
```turtle
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

@prefix ex: <http://example.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .

# --- People ---

ex:alice
  a foaf:Person ; # the founder
  foaf:name "Alice" ;
  foaf:age 42 ;
  foaf:knows ex:bob, ex:carol ;
.

ex:bob a foaf:Person .
```
`rdfs` and `owl` are not declared, so the first group holds only `rdf` and `xsd`; the
unlisted `ex` and `foaf` form the final group. Adding `turtle-layout = "conventional"`
(Phase 2) changes only the `ex:alice` block:
```turtle
ex:alice a foaf:Person ; # the founder
  foaf:name "Alice" ;
  foaf:age 42 ;
  foaf:knows ex:bob, ex:carol .
```
Adding instead `type-shorthand = false`, `compact-iris = false` and
`quote-style = "preserve"` keeps the author's spellings and entry order:
```turtle
<http://example.org/alice>
  foaf:name 'Alice' ;
  rdf:type foaf:Person ; # the founder
  foaf:age 42 ;
  foaf:knows ex:bob, ex:carol ;
.
```
`"42"^^xsd:integer` still becomes `42`, because N9 has no switch.

### 4.3 N-Triples 1.2 and N-Quads 1.2

These formats are machine-oriented, so the rules are different.
- **Line form.** One statement per line, written in the term spelling of *Canonical
  N-Triples* (RDF 1.2 N-Triples §3; N-Quads likewise):
  - a single space between terms, and after `<<(` and before `)>>`;
  - ` .` at the end;
  - no `^^xsd:string`;
  - uppercase hex in `UCHAR`;
  - `LANG_DIR` in lowercase;
  - only the characters the canonical form requires are escaped.

  Converting escapes changes the spelling, never the lexical form (§5 N11).
- **Comments.**
  - The file header stays at the top, followed by one blank line.
  - A leading comment block travels with its statement.
  - A trailing comment stays after ` .` with one space.
  - A `VERSION` line stays at the top, after the header.
- **Order.** By default statements keep their source order and duplicates are kept.
  Blank lines follow §3.3: they stay where the source had them, and runs collapse to one.
  A formatter should not reorder fixtures or dumps unless asked, and with this default
  one `--sort` flag means the same thing in every language.
- **`--sort`** is opt-in, as for Turtle, and `sort = true` sets it in the config. Lines
  are sorted by the bytes of the canonical line, the same order as `LC_ALL=C sort`,
  within sort barriers. Identical lines without comments are merged (§5 N20), and blank
  lines are dropped.
- **`--canonicalize`** always sorts, whatever `sort` says:
  - blank nodes are relabeled with RDFC-1.0 (`oxrdf` `Rdfc10 { Sha256 }`, `_:c14n0` …);
  - the output is the sorted canonical N-Quads that RDFC-1.0 defines;
  - comments are dropped, with a warning that counts them.

  RDFC-1.0 does not define triple terms, and oxrdf documents its behavior on them as
  non-standard. When blank nodes occur inside triple terms, the output carries the
  warning "labels are stable only for this oxrdf version". Canonicalization holds the
  whole dataset in memory, up to `--max-canonicalize-quads` (default 20M).
- **Streaming.** The input is read in line-aligned chunks (rayon), and each statement is
  formatted and checked on its own (§6, step 4). Without `--sort`, chunks are written back in
  input order. Plain `fmt`, `--check` and `--write` on a dump of any size are then one
  streaming pass in bounded memory, with no temporary files. With `--sort` or
  `--canonicalize`, the sort runs in memory up to `--sort-memory`. Beyond that, sorted
  runs spill to the temp directory, LZ4-framed with `lz4_flex` (already a workspace
  dependency), and a k-way merge deduplicates adjacent equal keys. HTTP always works in
  memory.

#### Example L1: N-Quads

Input:
```
# export 2026-09-30
<http://example.org/b>   <http://example.org/p> "café"@FR <http://example.org/g> .
_:x <http://example.org/p> <http://example.org/o> .
<http://example.org/a> <http://example.org/p> "1"^^<http://www.w3.org/2001/XMLSchema#string> <http://example.org/g> .   # first
_:x <http://example.org/p> <http://example.org/o> .
```
Output (default):
```
# export 2026-09-30

<http://example.org/b> <http://example.org/p> "café"@fr <http://example.org/g> .
_:x <http://example.org/p> <http://example.org/o> .
<http://example.org/a> <http://example.org/p> "1" <http://example.org/g> . # first
_:x <http://example.org/p> <http://example.org/o> .
```
Output of `sparkles fmt --sort`:
```
# export 2026-09-30

<http://example.org/a> <http://example.org/p> "1" <http://example.org/g> . # first
<http://example.org/b> <http://example.org/p> "café"@fr <http://example.org/g> .
_:x <http://example.org/p> <http://example.org/o> .
```
With `--canonicalize`, the output is sorted, `_:x` becomes `_:c14n0`, and the comments
are dropped.

### 4.4 JSON-LD 1.1 (Phase 2)

- **Layout** follows Prettier's JSON conventions:
  - 2-space indent (`indent-width`);
  - `"key": value`;
  - bracket spacing `{ "a": 1 }`;
  - no trailing commas.

  Objects and arrays break as follows:
  - An object with two or more members is always expanded. A one-member object stays
    inline (`{ "@id": "ex:a" }`) when it fits. Empty containers print `{}` and `[]`.
  - An array of scalars stays inline when it fits and otherwise puts one element per
    line.
  - An array that contains an object or an array always puts one element per line.
- **Key order.** This is the only JSON-LD normalization (§5 N21). Keywords come first, in
  a fixed order that depends on the kind of object:
  - node and value objects: `@context`, `@id`, `@type`, `@value`, `@language`,
    `@direction`, `@index`, `@reverse`, `@included`, `@nest`, `@list`, `@set`, then terms,
    then `@graph` last;
  - a context (a map under `@context`): `@version`, `@import`, `@base`, `@vocab`,
    `@language`, `@direction`, `@propagate`, `@protected`, then term definitions;
  - an expanded term definition: `@id`, `@reverse`, `@type`, `@language`, `@direction`,
    `@container`, `@context`, `@index`, `@nest`, `@prefix`, `@propagate`, `@protected`.

  Unknown `@`-keys come after the known keywords, in source order. Terms keep their
  source order, and `--sort` sorts them by codepoint.
- **Never changed:**
  - array order, since `@list` order and `@context` arrays are significant;
  - string escapes and number lexemes, printed verbatim.
- **Duplicate keys** are an error. RFC 8259 §4 leaves their meaning to the
  implementation, so reordering could change which one wins.
- **Comments** make the document invalid JSON, so input with comments is an error.

#### Example J1

Input:
```json
{"name":"Alice","@type":"Person","knows":[{"@id":"http://example.org/bob"},{"name":"Carol","@id":"http://example.org/carol"}],"@id":"http://example.org/alice","@context":{"name":"http://schema.org/name","@vocab":"http://schema.org/","knows":{"@type":"@id","@id":"http://schema.org/knows"}},"tags":["a","b"]}
```
Output:
```json
{
  "@context": {
    "@vocab": "http://schema.org/",
    "name": "http://schema.org/name",
    "knows": {
      "@id": "http://schema.org/knows",
      "@type": "@id"
    }
  },
  "@id": "http://example.org/alice",
  "@type": "Person",
  "name": "Alice",
  "knows": [
    { "@id": "http://example.org/bob" },
    {
      "@id": "http://example.org/carol",
      "name": "Carol"
    }
  ],
  "tags": ["a", "b"]
}
```

### 4.5 RDF/XML: rejected

`sparkles fmt x.rdf` exits 2 with "RDF/XML formatting is not supported; convert to Turtle
to format". The endpoint answers 415 with the same text. There are four reasons:
1. **Keeping comments needs an XML syntax tree.** That tree must also keep processing
   instructions, the DOCTYPE internal subset, and entity declarations. OWL files commonly
   use entities such as `&owl;` in namespace IRIs.
2. **Normalizing means re-serializing.** RDF/XML has many equivalent abbreviations:
   typed node elements, property attributes, `rdf:parseType="Resource"`, `rdf:li`,
   `rdf:nodeID`. Choosing among them rewrites the author's structure and drops comments.
3. **Whitespace can be content.** Re-indenting changes the value of
   `rdf:parseType="Literal"` XML literals and of property element text.
4. **Oxrdfxml cannot do it.** Its serializer does not abbreviate and would drop comments.

A comment-free mode that re-serializes to canonical RDF/XML would add little, because
users who want formatted data are better served by Turtle.

## 5. Normalizations

"Safe" means the output denotes the same thing under the cited definitions. For SPARQL,
the output has the same algebra, and so the same results. For RDF, it is the same
(isomorphic) graph or dataset. "Default" is Phase 1 behavior, and the opt-in items ship
in Phase 2. When a Default cell names a §2.2 key, the normalization is **configurable**:
the key turns it off or reverses it, and the safety argument covers both directions.
Every other row is fixed.

| # | Normalization | Decision | Default | Why it is safe (or why not) |
|---|---|---|---|---|
| N1 | Whitespace, line breaks, indentation | yes | on | Whitespace between tokens is insignificant (SPARQL 1.1 §19.3, Turtle §6.1, N-Triples 1.2 §5, RFC 8259 §2). Nothing changes inside tokens such as strings and IRIs (§3.1). |
| N2 | Comment placement. Content never changes, and no comment is dropped. | yes | on | Comments are whitespace (SPARQL §19.4, Turtle §6.2). |
| N3 | Keyword case. SPARQL keywords and builtins take their grammar spelling. Turtle `PREFIX`, `BASE`, `VERSION` and `GRAPH` are uppercase. | yes | on | SPARQL keywords are case-insensitive except `a` (SPARQL 1.1 §19.8 grammar notes), and SPARQL 1.2 §4.2.4 keeps `a` case-sensitive. In Turtle, `PREFIX`/`BASE` are case-insensitive and `@prefix` is case-sensitive (Turtle 1.2 §6.5 notes). `a`, `true` and `false` are case-sensitive and stay untouched. |
| N4 | `$v` → `?v` | **never** (decided 2026-09-30) | — | The rewrite would be safe, since `?abc` and `$abc` are the same variable (SPARQL §4.1.3). It is not done because SHACL-SPARQL and SPIN use `$this` and `$value` to mark pre-bound variables, and rewriting erases that signal. Each variable token keeps its sigil (§3.1). |
| N5 | Prefix declarations are sorted by label (codepoint order, empty label first), and exact duplicates are dropped. With `prefix-groups`, sorting happens within the configured groups (§2.2). | yes, when safe | on; grouping configurable (`prefix-groups`) | A declaration affects only how later prefixed names expand (SPARQL §4.1.1.1; Turtle 1.2 §2.5, §7.1 parser state). Prefix IRIs are `IRIREF`s, never prefixed names, so declarations do not depend on each other. **Conditions.** A run that breaks any of these is left as is. Sorting happens only within a contiguous run of declarations. A blank line ends a run unless `prefix-groups` is set (§2.2). `BASE` ends a run, since relative prefix IRIs resolve against the base in force (SPARQL §4.1.1.2, Turtle §2.5). No label in the run is bound twice to different IRIs. In SPARQL Update, each operation's prologue is its own run (1.1 grammar `Update ::= Prologue …`). Turtle directives in mid-document are never moved or merged, because a later directive may re-map a label (Turtle 1.2 §2.5: "Subsequent @prefix or PREFIX directives may re-map the same prefix label"). Duplicates are dropped only when they are identical and comment-free. |
| N6 | Turtle/TriG `@prefix`/`@base`/`@version` → `PREFIX`/`BASE`/`VERSION`. The TriG `GRAPH` keyword is always written. | yes | on; configurable (`directive-style = "turtle"` reverses both: `@prefix` family, no `GRAPH`) | Both forms are in the grammar with identical semantics: Turtle 1.2 §6.5 `prefixID`/`sparqlPrefix`, `base`/`sparqlBase` and `version`/`sparqlVersion`, and TriG `block ::= … \| "GRAPH" labelOrSubject wrappedGraph`, where `GRAPH` is optional. The SPARQL-style form matches SPARQL and has no trailing `.` to forget. Turtle 1.1 (2014) parsers accept it; only pre-REC Turtle 2008 parsers do not (§13). The reverse direction rests on the same grammar rules. `@prefix`, `@base` and `@version` are case-sensitive and printed lowercase. |
| N7 | Full IRI → prefixed name when a declared prefix covers it | yes | on; configurable (`compact-iris`) | A prefixed name denotes the prefix IRI plus the local part (SPARQL §4.1.1.1, Turtle §2.5, §7.2). **Conditions.** The `IRIREF` is absolute, since relative IRIs depend on `BASE`, and has no `UCHAR` escapes. A prefix declared **in scope at that position** covers it. The longest namespace wins, and a tie goes to the earliest declaration. The remainder is a `PN_LOCAL` that needs no `\` escape and does not end in `.`. The remainder may be empty, and `%HH` sequences are allowed because they are not decoded (Turtle §6.3). The rewrite never applies inside `PREFIX`/`BASE` and is never reversed. Undeclared (dataset) prefixes are never used. |
| N8 | `rdf:type` → `a` in verb position | yes | on; configurable (`type-shorthand`, with N14) | `a` in predicate position is `rdf:type` (Turtle 1.2 §2.5, SPARQL §4.2.4). The rewrite applies only to a simple verb: a Turtle predicate, or a SPARQL `VerbSimple`/`VerbPath` with a single IRI. It does not apply inside longer property paths or in subject or object position. |
| N9 | Typed literal → numeric or boolean shorthand when the lexical form **is** the shorthand token | yes | on | `INTEGER`, `DECIMAL`, `DOUBLE`, `true` and `false` tokens *are* literals of `xsd:integer`, `xsd:decimal`, `xsd:double` and `xsd:boolean` whose lexical form is the token text (Turtle 1.2 §2.6.2–2.6.3 and SPARQL §4.1.2: "the lexical form is the characters between the delimiters"). So `"1"^^xsd:integer` → `1`, `"01"^^xsd:integer` → `01` (lexical form kept), `"+1"^^xsd:integer` → `+1`, `".5"^^xsd:decimal` → `.5`, `"1e3"^^xsd:double` → `1e3` and `"true"^^xsd:boolean` → `true`. These stay unchanged: `"1"^^xsd:decimal` (`1` would be an integer), `"1."^^xsd:decimal`, `"INF"^^xsd:double`, and `"TRUE"` or `"1"^^xsd:boolean`. The datatype may be written as a full IRI or any prefixed name. The lexical form is **never** canonicalized (`01` → `1`). Literal term equality compares lexical forms (RDF 1.1 Concepts §3.3; RDF 1.2 Concepts, literal term equality), so `"01"^^xsd:integer` and `"1"^^xsd:integer` are different terms. The reverse, shorthand → typed, is never done. |
| N10 | Quote style. `'…'` → `"…"` and `'''…'''` → `"""…"""` when the content contains no `"`, and `\'` → `'` in converted strings. | yes | on; configurable (`quote-style = "preserve"`) | After unescaping, all four string forms denote the characters between the delimiters (Turtle 1.2 §2.6.1, §6.4; SPARQL §4.1.2, §19.7). Other escapes stay verbatim. Short and long forms are never switched. Turning `"a\nb"` into a long string would move an escape into raw content: the value is the same, but the author chose the spelling. |
| N11 | String/IRI escapes: `é` → `é` and similar | no in SPARQL/Turtle/TriG; yes in line formats | line formats only | The rewrite always preserves the value (Turtle §6.4). Human formats keep escapes verbatim because escapes often mark invisible or confusable characters (` `, `​`). turtlefmt (§14) reduces escapes, and this spec chose not to. Canonical N-Triples/N-Quads prescribe exact escaping (RDF 1.2 N-Triples §3). |
| N12 | SPARQL: a signed numeric literal directly after an operand (`A +n`, `A -n`) is printed as an operator and a literal (`A + n`, `A - n`) | yes | on | In an `AdditiveExpression`, the SPARQL grammar reads a signed literal after an operand as the operator plus the unsigned literal (1.1 grammar rule [116]). spargebra parses `?v+1` and `?v + 1` to the same `Add(?v, 1)`, and the algebra check (§6) confirms it every time. |
| N13 | Grammar-optional tokens. `WHERE` is added for SELECT, the CONSTRUCT template form and DESCRIBE with a pattern, and dropped after `ASK`. Every triples statement gets a `.`, and other elements get none. The trailing `;` of SPARQL property lists and of an update request is dropped. Expanded Turtle blocks get a trailing `;`, which `turtle-layout = "conventional"` drops. | yes | on; the Turtle `;` follows `turtle-layout` (Phase 2) | The SPARQL 1.1 grammar makes these tokens optional: `WhereClause ::= 'WHERE'? GroupGraphPattern` [17]; `TriplesBlock ::= TriplesSameSubjectPath ( '.' TriplesBlock? )?` [55]; `GroupGraphPatternSub ::= TriplesBlock? ( GraphPatternNotTriples '.'? TriplesBlock? )*` [54]; `PropertyListPathNotEmpty ::= … ( ';' ( … )? )*` [83]; `Update ::= Prologue ( Update1 ( ';' Update )? )?` [29]. Turtle has `predicateObjectList ::= verb objectList (';' (verb objectList)?)*`. The dots are separators and do not delimit BGPs, so the algebra is unchanged (§18.2.2.6). |
| N14 | Turtle/TriG: `a` entries come first within a subject block or `[ ]` block | yes | on; configurable (`type-shorthand`, with N8) | An RDF graph is a set of triples (RDF 1.1 Concepts §3; RDF 1.2 Concepts, RDF Graphs), and the triples a block produces do not depend on entry order (Turtle 1.2 §7.3). A reifier or annotation moves with its entry, so each still attaches to the same triple (§7.3.1–7.3.4). The move is stable: the other entries keep their source order. |
| N15 | Turtle/TriG `--sort`. Within sort barriers, the remaining entries are sorted by printed predicate, objects by printed form, and statements by printed subject. In TriG, graph blocks are sorted by printed name, default graph first, with the statements sorted inside each. | opt-in | off (`sort`) | The set argument of N14 applies, and datasets are sets of quads (RDF 1.2 Concepts, RDF Datasets). Sorting is off by default because the grouping expresses the author's intent. Sorting by printed form rather than by IRI makes the order match what the reader sees. |
| N16 | SPARQL: triple-pattern order in a BGP | **never** | — | A BGP is a set of triple patterns (SPARQL 1.1 §5.1, §18.1.4; 1.2 §5.1), so reordering would be safe. But the order expresses intent, and property-path triples become separate joined operators that a set comparison would not cover. Keeping the order keeps the algebra check exact. |
| N17 | Parentheses: adding or removing redundant ones | **never** | — | Removing them is safe only with precedence analysis, and `FILTER`, `BIND`, `HAVING` and `ORDER BY` expressions need a `BrackettedExpression`. The small gain does not justify the risk. |
| N18 | SPARQL order-sensitive constructs: `FILTER` position, `SELECT` projection, `ORDER BY` keys, `OPTIONAL`/`MINUS`/`UNION` operand order, `VALUES` rows, `GROUP BY` keys, update operations | **never** | — | **FILTER:** a filter restricts its whole group (§5.2.2), and 18.2.2.2 collects filters at the end of their group. Moving one *within* its group is therefore safe, and moving it *across* groups is not. Not moving it at all keeps the algebra check exact. **Projection:** the order of the projected variables is observable (§18.2.5, `head.vars` order in Results JSON). **ORDER BY:** conditions apply in sequence (§15.1). **OPTIONAL:** `LeftJoin` is neither commutative nor associative with its neighbors (§6.3, 18.2.2.6). **MINUS:** not commutative (§8.2). **UNION:** commutative as a multiset (§18.5), but the order of results is observable without `ORDER BY` and under `LIMIT`/`OFFSET`. **VALUES:** a multiset of solutions (§10.2) whose row order is observable in the same way. Rows are never deduplicated. **Updates:** operations run in order (SPARQL 1.1 Update §3). |
| N19 | Merging repeated subject blocks, splitting or joining `;`/`,` groups, `[ ]` ↔ `_:b`, collections ↔ `rdf:first`/`rest` | **never** | — | In Turtle these rewrites are safe only under conditions (§2.7–2.9), such as a label referenced once, or blank-node scope across TriG graphs. They also rewrite the author's structure and move comments. They are not formatting. |
| N20 | N-Triples/N-Quads: sort lines and drop duplicates | opt-in (decided 2026-09-30) | off (`sort`); `--canonicalize` always sorts | The document denotes a set of triples or quads (RDF 1.2 Concepts; N-Triples and N-Quads parse to a graph or dataset), so line order and duplicate lines carry no meaning. A duplicate that carries a comment is kept. Sorting is off by default for the same reason as N15: a formatter should not reorder fixtures or dumps unless asked. Without sorting, a large file formats in one streaming pass. |
| N21 | JSON-LD key order: keywords first, then terms (sorted with `--sort`) | yes | on | An object is an unordered collection (RFC 8259 §4). The JSON-LD 1.1 algorithms process `@context` and type-scoped contexts before the other entries, wherever they appear (JSON-LD 1.1 API, Context Processing and Expansion Algorithm). Term definitions in one context resolve their dependencies recursively (Create Term Definition), so their order is irrelevant. Arrays are never reordered. |
| N22 | Blank-node relabeling | `--canonicalize` (line formats) | off | Labels are local to a document (RDF 1.1 Concepts §3.4; SPARQL §19.6 scopes them to the BGP), so renaming gives an isomorphic graph (§3.6). RDFC-1.0 §4 gives the canonical labels. Human formats never relabel. |
| N23 | Language tags and directions | keep as written (Turtle/SPARQL); lowercase (line formats) | — | Tags are case-insensitive (BCP 47 §2.1.1). RDF 1.1 Concepts §3.3 allows lowercasing, and canonical N-Triples 1.2 requires it. `oxttl` lowercases tags when parsing, so the round-trip check **cannot** see a case change. The human-format printers must therefore copy tag tokens verbatim, and a unit test enforces it. |
| N24 | `"x"^^xsd:string` → `"x"` | line formats only | — | It is the same term in RDF 1.1 and later (Concepts §3.3, simple literals). Turtle and SPARQL keep it because it is the author's spelling, and older RDF 1.0 tools distinguish the two. |
| N25 | Unused prefix declarations | keep; `--prune-prefixes` drops them | off (`prune-prefixes`) | Unused prefixes affect neither SPARQL semantics (§4.1.1.1) nor the graph. They are kept for three reasons: they document vocabularies, tools reuse them for output, and authors keep templates. Jena and Fuseki, for example, pretty-print `CONSTRUCT` results with the query's prefixes; Sparkles uses the dataset's. "Unused" is computed per declaration scope: up to a redefinition in Turtle, or across all later operations in an update request. |
| N26 | `LIMIT`/`OFFSET` order → `LIMIT` then `OFFSET` | yes | on | Grammar rule [25] `LimitOffsetClauses` allows either order, and both translate to the same `Slice(start, length)` (§18.2.5.5). |

## 6. Safety checks

Every format runs the pipeline below. The CLI, the endpoint and the library all apply it.

1. **Reference parse of the input.**
   - SPARQL: `spargebra::SparqlParser` (query, falling back to update).
   - Turtle/TriG: `oxttl`.
   - Line formats: `oxttl`'s N-Triples/N-Quads parsers.
   - JSON-LD: `json-event-parser`.

   A failure is the user's syntax error, with the reference parser's position. The CST
   parser's error is never shown, because the reference parser is authoritative.
   - **Relative IRIs.** When a document has relative IRIs and no `BASE`, both parses use
     the synthetic base `http://sparkles-fmt.invalid/base/`.
   - **Undeclared SPARQL prefixes**, such as the dataset prefixes common in the UI, are
     registered in both parses with
     `with_prefix(label, "http://sparkles-fmt.invalid/prefix/<label>/")`. Each one adds
     an `undeclared-prefix` warning, and N7 never uses them.
2. **Lossless lex and CST parse.** If the CST parser rejects input that the reference
   parser accepted, the error is `unsupported-syntax` (422 over HTTP, exit 2 in the CLI)
   and is logged as a formatter bug.
3. **Normalize, build the document, print.**
4. **Equivalence check** of the output against the input.
   - **SPARQL.** Parse the output with the same parser settings and compare
     `canon(input_algebra) == canon(output_algebra)`. `canon` is a mutable pre-order
     visitor over `Query`/`Update` that renames:
     - every `BlankNode` to `_:b{n}`, in order of first occurrence. spargebra creates
       random blank nodes for `[]`, paths and reifiers (`BlankNode::default()`), and user
       labels are local anyway;
     - every **hidden variable** to `?_h{n}`. A hidden variable is one whose name is not
       among the input's `VAR` tokens. spargebra names aggregate and other internal
       variables with random 128-bit hex (`parser.rs`).

     The formatter never reorders SPARQL, so the visitor visits both sides in the same
     order. `base_iri` and the dataset clause take part in the comparison.
   - **Turtle/TriG.** Parse both sides into `Vec<Quad>`.
     - *Fast path:* relabel blank nodes by first occurrence, then compare as sorted,
       deduplicated vectors.
     - *Fallback.* N14/N15 reordering can change the order of first occurrence. In that
       case, build an `oxrdf::Dataset` for each side, run
       `canonicalize(CanonicalizationAlgorithm::Unstable)`, and compare with `==`. Both
       sides use the same oxrdf build, so it does not matter that the algorithm is not
       stable across versions.
   - **Line formats.** The check runs per statement while streaming, before sorting. The
     oxttl quad parsed from each input statement must equal the oxttl quad parsed from
     its formatted line, with labels kept. Without `--sort`, the output has one line per
     input statement, in input order, and a final count asserts it. With `--sort`,
     sorting and deduplication only permute and merge verified, byte-identical lines. A
     final pass asserts that the output is sorted and that its line count equals the
     unique count. With `--canonicalize`, re-canonicalizing the parsed output must
     reproduce the output bytes.
   - **JSON-LD.** Build an order-insensitive model with `json-event-parser`. Objects are
     key → value maps with duplicates rejected, arrays are ordered, strings are compared
     after unescaping, and numbers are compared by lexeme. The input and output models
     must be equal. This proves that the output is the same JSON document under any
     context, remote contexts included, so no network is needed. Tests also compare the
     `oxjsonld` datasets for self-contained documents.
5. **Comment check.** The sequence of comment texts in the output must equal the input
   sequence. When N14, N15 or N20 moved nodes, the two are compared as multisets.
   `--canonicalize` is exempt, because it drops comments by definition and says so.
6. **Idempotence check.** Run steps 2–3 on the output. The result must be byte-identical.
   Steps 1 and 4 need not rerun, since step 4 already covered the output.

**On any failure in steps 2 or 4–6:**
- CLI: the input file is left untouched. The error names the check (`algebra differs`,
  `graph differs`, `comment lost`, `not idempotent`) and says "please report". `--check`
  counts the file as an error (exit 2), not as unformatted.
- HTTP: 422 with `code` `unsupported-syntax`, `unsafe-format` or `unstable-format`.
- Library: `Err(FormatError::Unsafe { check })`.

Like Prettier, the formatter has no flag to skip the check. The per-line check for line
formats costs little.

**Fault injection.** A test-only cargo feature, `fault-injection`, makes the printer drop
one token when an env var is set. It proves that every check fires (§11 A11).

## 7. Architecture and crate layout

### 7.1 A lossless syntax tree

`spargebra` produces algebra and loses comments, layout, prefixed names and
abbreviations (`;`, `,`, `[]`, collections). `oxttl` produces quads and loses the same
things. Neither can drive a formatter. A formatter that keeps comments needs its **own
lossless tokenizer and concrete syntax tree**, and the existing parsers remain the
reference for §6.

| Option | License | Verdict |
|---|---|---|
| **Hand-written lexer + event-based recursive-descent parser + small arena tree** | ours | **Chosen.** It has no dependencies and builds for WASM as is. The formatter only reads the tree, so it needs no incremental reparse, mutation or cursor API. Recursive descent maps one-to-one onto the W3C EBNF, and expressions use a Pratt loop with the SPARQL precedence levels. The event interface (`Start(kind)`, `Token`, `Finish`) follows rust-analyzer's parser and keeps a later move to rowan mechanical. |
| `rowan` 0.16 (0.18 is alpha) | MIT OR Apache-2.0 | Deferred. A proven lossless green/red tree, worth having for an LSP with incremental reparsing and IDE features (Phase 3+). For formatting it adds API churn, since 0.18 is in alpha, and no capability the formatter uses. |
| `cstree` 0.14 | MIT OR Apache-2.0 | Like rowan, with interning and `Send` trees added. Same verdict. |
| tree-sitter (`tree-sitter-sparql` 0.25, MIT; tree-sitter-turtle, MIT, not on crates.io) | MIT | **Rejected.** It needs a C runtime and generated C parsers, and so a C toolchain for WASM. turtlefmt notes that building from source needs Node for grammar generation. Neither grammar supports RDF 1.2 (no `<<(`, `{\|`, `VERSION`). Error-recovering parses are wrong for a formatter that must reject exactly what the reference parser rejects. Each grammar has one maintainer. |
| Reuse spargebra `Display` / oxttl serializers | MIT OR Apache-2.0 | **Rejected.** Comments are lost, and the output follows the shape of the algebra. |

**Grammar coverage.**
- **SPARQL:** the SPARQL 1.2 grammar (§19.7), queries and updates, as Sparkles parses
  them (`spargebra` feature `sparql-12`, without `sep-0002`/`sep-0006`).
- **Turtle/TriG:** the RDF 1.2 grammars.
- **Shared lexer.** The terminals are shared by one lexer: `IRIREF`, `PNAME_*`,
  `BLANK_NODE_LABEL`, `LANG_DIR`, the numbers, the four string forms, `ANON`, `NIL`, and
  `<<(`, `)>>`, `<<`, `>>`, `{|`, `|}`, `~`. The lexer has one mode for SPARQL (variables,
  operators, keywords) and one for Turtle (`@prefix`/`@base`/`@version`).
- **Longest match.** Terminals are matched longest-first, as SPARQL §19.8 and Turtle
  §6.5 prescribe. That gives `?x-1` → `?x`, `-1` and `ex:a.` → `ex:a`, `.`. In an
  `AdditiveExpression` position the parser treats a signed numeric token as operator plus
  literal (N12). Any divergence from spargebra's scannerless parse surfaces as a §6
  failure, never as wrong output.

### 7.2 oxfmt and JSON-LD

These findings on `oxfmt`, the Oxc project's Prettier-compatible formatter, date from
2026-09-30:

| Question | Finding |
|---|---|
| License | MIT (Oxc monorepo, © VoidZero Inc. and Oxc contributors) |
| Distribution | npm `oxfmt` 0.71.0, a Node CLI with per-platform NAPI binaries (`@oxfmt/binding-*`). The docs present it as production-ready and say it passes all of Prettier's JS/TS conformance tests. |
| Usable as a Rust library? | **Not published.** In the monorepo, `apps/oxfmt`, `crates/oxc_formatter`, `crates/oxc_formatter_json` and `crates/oxc_formatter_core` all set `publish = false`. On crates.io, `oxfmt` is an unrelated binary-serialization crate, and `oxc_formatter` holds only versions 0.1–0.4 from 2023, an abandoned early experiment. Embedding would mean a git dependency pinned to a commit of a fast-moving monorepo with weekly minor releases and no API stability. It would pull in `oxc_allocator`, `oxc_ast`, `oxc_parser`, `oxc_diagnostics` and more. |
| JSON handling | Native Rust (`oxc_formatter_json`). It parses JSON with the **JS expression parser**, which is lenient and accepts comments, trailing commas, single quotes and unquoted keys. Its variants mirror Prettier's `json`/`jsonc`/`json5`/`json-stringify` parsers. The options are indent, width, line ending, bracket spacing, expand, trailing commas and quotes. Like Prettier, it **never reorders keys**. The oxfmt app sorts only `package.json`, through `sort-package-json`. |

**Recommendation: neither embed nor shell out.** Write a small JSON-LD printer, about 400
lines, on the shared document IR, with keyword-aware ordering. There are four reasons:
1. oxfmt does not do the one thing wanted here: keyword-aware key order.
2. The lenient parser is wrong for JSON-LD, which must be strict JSON. The check needs a
   strict, independent parser anyway. `json-event-parser` (MIT OR Apache-2.0) is
   already in the tree through `oxjsonld`.
3. Embedding costs a git-pinned tree of about 10 crates without semver.
4. Shelling out needs Node next to a single static binary, and cannot serve the HTTP
   endpoint or the WASM build.

Following Prettier's JSON conventions keeps the output close to Prettier's. A Phase 2 CI
test runs `prettier --check --parser json` over the golden JSON-LD outputs and records any
difference. Prettier's JSON language claims `.jsonld`, so repositories that run Prettier
over `*.jsonld` should list those files in `.prettierignore` (§13 Q8).

### 7.3 Crates, API, dependencies

**`crates/sparkles-fmt`** is a new library. It depends on neither `sparkles` nor the
server.

```
src/lib.rs            Language, Options, Formatted, FormatError, format(), detect(), format_lines()
src/doc.rs            document IR + printer (§3.2)
src/trivia.rs         comment classification/attachment, blank-line accounting (§3.3)
src/cursor.rs         token → output offset map (§3.5)
src/lex.rs            shared lossless lexer (SPARQL and Turtle modes)
src/sparql/{parse.rs, cst.rs, print.rs, keywords.rs}
src/turtle/{parse.rs, cst.rs, print.rs}      Turtle + TriG
src/lines.rs          N-Triples/N-Quads: line scanner, canonical term writer, sort/spill/merge
src/jsonld.rs         (Phase 2)
src/normalize.rs      prefix scopes, N5–N10, N13–N15, N21
src/check/{mod.rs, algebra.rs, graph.rs, json.rs}   §6
tests/golden/<lang>/<name>.in.<ext> + .out.<ext>
tests/w3c.rs, tests/props.rs
fuzz/                 (Phase 3)
```

```rust
pub enum Language { Sparql, Turtle, TriG, NTriples, NQuads, JsonLd }
pub struct Options {
    pub line_width: u16,            // 100
    pub indent_width: u8,           // 2
    pub sort: bool,                 // false for every language (N15, N20)
    pub prune_prefixes: bool,
    pub canonicalize: bool,         // implies sorting (line formats)
    pub directive_style: DirectiveStyle,       // Sparql | Turtle (N6)
    pub prefix_groups: Vec<Vec<String>>,       // [] (N5, §2.2)
    pub type_shorthand: bool,                  // true (N8, N14)
    pub compact_iris: bool,                    // true (N7)
    pub quote_style: QuoteStyle,               // Double | Preserve (N10)
    pub operator_position: OperatorPosition,   // Leading | Trailing (§4.1)
    pub turtle_layout: TurtleLayout,           // Diff | Conventional (Phase 2)
    pub align_values: bool,                    // false (Phase 2)
    pub cursor: Option<usize>,      // byte offset
    pub deadline: Option<std::time::Instant>,
}
pub struct Formatted { pub text: String, pub changed: bool, pub cursor: Option<usize>, pub warnings: Vec<Warning> }
pub enum FormatError {
    Syntax { message: String, line: u32, column: u32, offset: usize },   // 1-based line/column (code points)
    Unsupported { message: String, line: u32, column: u32 },
    Unsafe { check: Check },        // Algebra | Graph | Comments | Idempotence
    Timeout, TooLarge,
}
pub fn format(text: &str, lang: Language, opts: &Options) -> Result<Formatted, FormatError>;
pub fn detect(path: Option<&std::path::Path>, text: &str) -> Option<Language>;
pub fn format_lines(r: impl std::io::BufRead, w: impl std::io::Write, lang: Language,
                    opts: &Options, spill: &std::path::Path) -> Result<LineStats, FormatError>;
```

The server side lives in `sparkles-server`, behind the default-on feature `fmt`:
- `src/fmt.rs`: the `Cmd::Fmt` subcommand, directory walking, config discovery, diffs,
  exit codes and rayon parallelism;
- `src/http/format.rs`: the endpoint;
- the `auth/routes.rs` and `ratelimit.rs` changes of §2.4.

The CLI-only dependencies stay out of the library, which keeps it WASM-friendly.

All dependencies have permissive licenses:

| Crate | License | Where | New? |
|---|---|---|---|
| `spargebra` 0.4, `oxttl` 0.2, `oxrdf` 0.3 (`rdfc-10`, `rdf-12`) | MIT OR Apache-2.0 | lib (check, line formats) | no |
| `json-event-parser` 0.2 | MIT OR Apache-2.0 | lib (Phase 2) | no (already in the lock) |
| `oxjsonld` 0.2 | MIT OR Apache-2.0 | lib dev-dependency (tests) | no |
| `lz4_flex` 0.14 | MIT | lib (spill runs) | no |
| `unicode-width` | MIT OR Apache-2.0 | lib | no (already in the lock) |
| `thiserror` 2 | MIT OR Apache-2.0 | lib | no |
| `ignore` 0.4 | Unlicense OR MIT | server (walks, gitignore) | yes |
| `similar` 3 | Apache-2.0 | server (`--diff`) | yes |
| `toml` 1 | MIT OR Apache-2.0 | server (config file) | Already optional, for auth. Feature `fmt` now depends on it. |
| `proptest` 1 | MIT OR Apache-2.0 | lib dev-dependency | yes |

On implementation, add an entry to [PROVENANCE](PROVENANCE.md). It lists the design
sources (§14), records that prior art was consulted through public docs only, and names
the dependencies above.

### 7.4 WASM target (Phase 3)

- `sparkles-fmt` must build with `cargo check --target wasm32-unknown-unknown`, and a CI
  job enforces it from Phase 1.
- spargebra and oxrdf use `rand` for fresh blank nodes, which needs `getrandom`'s
  `wasm_js` backend. The backend is enabled only for that target.
- A thin `sparkles-fmt-wasm` crate (`wasm-bindgen`) exports
  `format(text, language, options) -> {text, cursorOffset} | {error, line, column}`.
- The UI loads it lazily and falls back to `/$/format` when the load fails.
- The module must stay under 1.5 MB gzipped. That budget allows for the reference
  parsers, which the module includes for the check.

### 7.5 Editor integrations (Phase 2)

- `sparkles fmt --stdin-filepath NAME` is enough for formatter plugins such as
  conform.nvim, Helix `formatter`, Emacs `apheleia` and VS Code "Run on Save". The docs
  give one snippet for each.
- `sparkles lsp` is a stdio language server built on `lsp-server` and `lsp-types` (MIT
  OR Apache-2.0, from rust-analyzer). It offers:
  - `textDocument/formatting`;
  - `rangeFormatting`, which formats the whole document and returns the minimal edit;
  - syntax-error diagnostics.

  A VS Code extension package is out of scope.

## 8. Performance

The targets are measured on a release build on a quiet machine, as in
[BENCHMARKS](../BENCHMARKS.md#setup), and include the §6 checks.

| Input | Target |
|---|---|
| UI-sized query (under 10 KB), HTTP round trip on localhost | p99 < 20 ms |
| 10,000-line SPARQL file (about 400 KB) | < 200 ms |
| 1 MB Turtle (about 20k triples; SHACL/ontology-like, 10% blank nodes) | < 100 ms on the fast check path. < 300 ms when the canonicalization fallback runs. |
| N-Triples/N-Quads, default (no sort) | ≥ 100 MB/s single-threaded, scaling with `--threads`. One streaming pass, with memory bounded by the chunk size and no temporary files. |
| 10 GB N-Triples with `--sort` | One spill pass plus a merge, with memory bounded by `--sort-memory`. About 1.5× the unsorted time. |
| Any combination of the style keys (§2.2) | Within noise of the defaults. Each key selects a printer branch or skips a normalization, so the targets above hold for every combination. |
| Turtle/TriG/SPARQL/JSON-LD in-memory limit | `--max-bytes`, default 256 MiB. Larger inputs get the error "convert to N-Triples". |

Memory for the in-memory languages stays at or below 12× the input size, counting tokens,
the arena tree, the document and the output. Benchmarks in `crates/sparkles-fmt/benches`
use plain `std::time` loops without criterion, like the existing bench tooling. They run
over:
- a synthetic ontology (`gen-data`-like);
- the concatenated W3C SPARQL eval queries;
- a 1 GB N-Triples file.

Results go in `docs/BENCHMARKS.md` (measured numbers only).

## 9. Testing

1. **Golden files.** `tests/golden/<lang>/*.in.*` and `*.out.*` pairs cover every example
   in §4 and every row of §5. Each rule in §3.3 also gets one file: trailing, leading,
   dangling, detached and displaced comments, the header, pragmas, and blank-line
   collapsing.
   - The runner asserts `format(in) == out`, `format(out) == out`, and that the checks
     pass.
   - `SPARKLES_FMT_BLESS=1` rewrites the `.out` files.
   - Width variants live in `*.w40.out.*`. Option variants live in `*.<variant>.out.*`,
     with their options in a sibling `<name>.<variant>.toml` that the real config parser
     reads. For T6, these are `t1.turtle-groups.toml` and `t1.turtle-groups.out.ttl`.
     Every non-default value of every key has at least one golden file. That includes
     the §4.1 trailing-operator and nested-chain examples, and `prefix-groups` with the
     empty label and with a `BASE` inside the run.
2. **W3C corpora.** These reuse the Apache Jena checkout that the SPARQL suites use.
   `SPARKLES_W3C_DIR` works as in `crates/sparkles/tests/w3c.rs` and defaults to Jena's
   `jena-arq/testing/rdf-tests-cg` in a sibling checkout. The tests are skipped when the
   checkout is absent.
   - **SPARQL** (`sparql/sparql10`, `sparql11`, `sparql12`): every positive syntax test
     (about 360) and every query and update of every evaluation test (about 1,130 `.rq`
     and 170 `.ru`) must format, pass §6, and be idempotent. Every negative syntax test,
     including the `codepoint-escapes` bad cases, must return `FormatError::Syntax`
     without panicking.
   - **RDF** (`rdf/rdf11` and `rdf/rdf12`: `rdf-turtle`, `rdf-trig`, `rdf-n-triples`,
     `rdf-n-quads`): every positive syntax and evaluation input must format, pass §6, and
     be idempotent. Every negative input must be a syntax error.
   - **SHACL:** all Turtle files under `jena-shacl/src/test/files/std`
     (`SPARKLES_SHACL_TESTS`).
   - Documented exceptions go in `tests/fmt-known-failures.txt`, as the other suites do.
   - **Optional semantic cross-check.** `SPARKLES_W3C_FORMAT=1` makes the existing W3C
     evaluation harness format each query before running it. Results must be unchanged.
     This guards against a hole in the algebra comparison.
3. **Property tests** (`proptest`, over the corpora above):
   - **comment injection:** insert `# c<n>` at random token boundaries where a comment is
     legal. The format succeeds, every `c<n>` survives, and the checks pass;
   - **layout independence:** randomly re-space within lines and replace single newlines
     with spaces, keeping the blank-line structure. The output must equal the
     unperturbed output;
   - **generated SPARQL:** random spargebra algebra, printed with `Display`, then
     formatted and checked;
   - **random options:** each case also draws an `Options` value. Every enumerated key is
     drawn uniformly, `line-width` from 40..=400 and `indent-width` from 1..=8.
     `prefix-groups` is a random partition of the labels the input declares, `""`
     included. The §6 checks and idempotence must hold under the drawn options.

     Three keys fully determine their construct: `directive-style`,
     `operator-position` and `align-values`. For these, formatting is also
     **convergent**: `format(format(x, A), B) == format(x, B)` for any `A` and `B` that
     differ only in those keys. The other keys are exempt by design, because the second
     run keeps what the first produced. Some keep spellings as written
     (`type-shorthand = false`, `compact-iris = false`, `quote-style = "preserve"`).
     Some set an order (`sort`, `prefix-groups`). `turtle-layout`, `line-width` and
     `indent-width` change which statements span several lines, and so where blank
     lines are added (§3.3).
4. **Configuration matrix.** Idempotence and the round-trip self-check (§6) must hold
   for **every** option combination, not only the default:
   - every golden input is formatted under the full cross product of the boolean and
     enumerated keys: `sort`, `prune-prefixes`, `directive-style`, `type-shorthand`,
     `compact-iris`, `quote-style`, `operator-position`, `turtle-layout` and
     `align-values`, for 2⁹ = 512 combinations. Each combination runs with
     `prefix-groups` empty and with `[["rdf", "rdfs", "xsd", "owl"]]`, at widths 40 and
     100. Keys that do not apply to a language are skipped, which keeps the SPARQL and
     Turtle matrices to a few hundred cases per file. Keys join the matrix in the phase
     that implements them. The matrix runs in the default `cargo test`;
   - the W3C corpora (§9.2) run under the defaults and under one combination with every
     key flipped. `SPARKLES_FMT_MATRIX=1` runs the full matrix over the corpora, which
     is slow and not part of the default run;
   - config parsing: every key's type, range and enumeration errors, unknown keys,
     duplicate and invalid `prefix-groups` labels, and a check that the CLI and HTTP
     spellings of each key parse to the same `Options`.
5. **Unit tests:**
   - the printer: groups, `LineSuffix`, `BreakParent`, width accounting with wide
     characters;
   - cursor mapping;
   - each normalization's conditions: N5 with `BASE` inside the run, N7 with `PN_LOCAL`
     escapes and out-of-scope prefixes, N9's table, N23's verbatim tags, and N4. For N4,
     `$v` and `?v` tokens keep the sigil they were written with, including in a query
     that spells one variable both ways;
   - each configurable normalization with its key off or reversed (§5);
   - canonical N-Triples term writing against the RDF 1.2 N-Triples §3 rules;
   - JSON-LD key order.
6. **Server and UI tests:**
   - router tests: `ROUTES` lists `/$/format`; `need()` returns `Caller`; `classify`
     returns `Query`;
   - the endpoint: JSON and raw bodies, the 400 error shape, 413, 415 for RDF/XML, 422
     with fault injection, `--format-endpoint authenticated|off`;
   - the CLI: exit codes 0/1/2, `--check` output, `--diff`, `--write` atomicity and
     mtime, ignore files, `--stdin-filepath`, config discovery (the nearest file wins,
     flags override it, `--config`, `--no-config`);
   - Vitest for the UI helpers.
7. **Fuzzing (Phase 3).** `cargo-fuzz` targets `fmt_sparql`, `fmt_turtle`, `fmt_lines`
   and `fmt_jsonld`. On arbitrary UTF-8, format must not panic, and any `Ok` output must
   be idempotent. Seed corpora come from the W3C files.

## 10. Phasing and estimates

Estimates are in working days for one engineer.

**Phase 1 (about 31.5 days, 6–7 weeks): SPARQL, Turtle/TriG, N-Triples/N-Quads, CLI,
config file, HTTP, UI**

| Work | Days |
|---|---|
| Document IR and printer; trivia and comment attachment; cursor map | 4 |
| Shared lexer; SPARQL 1.2 CST parser (queries, updates, paths, expressions, RDF 1.2 terms) | 5 |
| SPARQL printing rules (§4.1), including both operator positions and the continuation indent | 4 |
| Turtle/TriG 1.2 parser and printer (§4.2) | 3 |
| Line formats: canonical writer, per-line check, ordered streaming output | 1 |
| Normalizations N1–N3, N5–N14, N16–N19, N23, N24, N26 | 1.5 |
| Style keys (§2.2): `directive-style` with TriG `GRAPH`, `prefix-groups`, and the `type-shorthand`, `compact-iris` and `quote-style` switches | 1.5 |
| `.sparklesfmt.toml` discovery and validation (moved from Phase 2); CLI flags and HTTP options for every key | 1.5 |
| Safety checks (§6): algebra canonicalizer, graph comparison, comments, idempotence | 2 |
| CLI: walk, ignore files, `--check`/`-l`/`--write`/`--diff`, exit codes | 1.5 |
| HTTP endpoint, auth and rate-limit tables, UI button, shortcut, format on run, mock | 1.5 |
| Golden files (defaults and option variants), W3C sweeps, property tests, configuration matrix (§9.4), fixing fallout; API.md and README docs | 5 |

An earlier version of this table summed to 27 days, headlined "about 26". The style
keys, the config file moving up from Phase 2, and the larger test matrix add 5 days.
Dropping line-format sorting from Phase 1 saves 0.5. Each switch is cheap in the printer
but doubles a slice of the test matrix, so most of the added cost is golden files and
matrix fallout. In Phase 1, line formats stream in source order without sorting. Their
`--sort` arrives in Phase 2 with the other sorting and the external sort.

**Phase 2 (about 10.5 days, plus 2.5 for the optional keys)**

| Work | Days |
|---|---|
| JSON-LD printer, key order, JSON check (§4.4, N21) | 2.5 |
| `--sort` (N15, and N20 in memory), `--prune-prefixes` (N25), `--canonicalize` (N22) | 3 |
| External sort for line formats | 1.5 |
| `sparkles lsp` formatting provider; editor snippets | 3 |
| Prettier comparison for JSON-LD; Format button on other UI text areas | 0.5 |
| Optional, only if easy: `turtle-layout = "conventional"` (§4.2) | 1.5 |
| Optional, only if easy: `align-values` (§4.1) | 1 |

The two optional keys are not critical (§13). If either one needs much more than its
estimate, for example because comments or annotation blocks do not fit the shared SPARQL
triples printer, it is dropped rather than shipped approximately.

RDF/XML stays rejected (§4.5).

**Phase 3 (about 7 days)**

| Work | Days |
|---|---|
| WASM crate and UI integration with endpoint fallback | 3 |
| Fuzzing targets and a first campaign | 2 |
| Streaming Turtle and TriG statement by statement, when not sorting | 2 |

A `sparkles lint` needs its own spec.

## 11. Acceptance examples

- **A1.** `sparkles fmt q.rq` prints the formatted query. `--write` rewrites the file. A
  second `--write` leaves the mtime unchanged.
- **A2.** `sparkles fmt --check queries/`, where one file is unformatted and one is
  formatted, exits 1 and prints `[warn] queries/a.rq` and the summary. Adding a file
  with a syntax error makes it exit 2 and print `queries/bad.rq:3:14: error: …`.
- **A3.** Every golden output is a fixpoint, and every example in §4 holds byte for byte.
- **A4.** All W3C positive SPARQL, Turtle, TriG, N-Triples and N-Quads inputs (§9.2)
  format and pass §6. All negative ones give syntax errors, and nothing panics.
- **A5.** A comment-injection run over the corpora loses no comment.
- **A6.** `ex:s ex:p "01"^^xsd:integer .` formats to `ex:s ex:p 01 .`.
  `"1"^^xsd:decimal` is unchanged. `"042"@EN` keeps `@EN`.
- **A7.** In S1 the `FILTER` keeps its position. For every SPARQL test, the canonicalized
  algebra of the output equals the input's, which covers projection order, `ORDER BY`,
  `OPTIONAL`/`MINUS`/`UNION` and `VALUES` order.
- **A8.** L1 holds: the default output keeps source order and duplicates, and `--sort`
  gives the sorted form. `--canonicalize` on an RDFC-1.0 example (two isomorphic inputs with
  different labels) gives byte-identical outputs.
- **A9.** `POST /$/format` with the JSON body of §2.4 returns 200 with the mapped cursor.
  - `select * {` returns 400 `{error, line: 1, column, code: "syntax"}`, with spargebra's
    end-of-input position.
  - A 17 MiB body returns 413, and `application/rdf+xml` returns 415.
  - An anonymous caller succeeds.
  - The request is counted in the `query` rate-limit class.
  - With `--format-endpoint off` the route returns 404; with `authenticated`, an
    anonymous caller gets 401.
- **A10.** In the UI, Shift+Alt+F formats the editor. One undo restores the original,
  and the cursor stays next to the same token. A syntax error highlights its line. With
  format on run enabled, **Run** formats first.
- **A11.** With `fault-injection` dropping a token, `sparkles fmt --write x.ttl` exits 2
  with `graph differs` and leaves `x.ttl` unchanged, and the endpoint returns 422
  `unsafe-format`.
- **A12.** The §8 targets are met and recorded.
- **A13.** Configuration:
  - T6 holds with its `.sparklesfmt.toml`, and the trailing-operator variant of S3 holds
    with `operator-position = "trailing"`;
  - `SELECT $this WHERE { $this ex:p ?v }` keeps both `$this` tokens;
  - a `.sparklesfmt.toml` with an unknown key, a misspelled enumerated value, or a label
    in two `prefix-groups` makes `sparkles fmt` exit 2 with the key and file named, and
    the same option over HTTP returns 400 `bad-request`;
  - every combination of the §9.4 matrix passes §6 and is idempotent on every golden
    input.

## 12. Rejected alternatives

- **Re-serializing through spargebra `Display` / `oxttl` serializers.** It loses comments,
  prefixed names and structure (§7.1).
- **tree-sitter grammars.** They bring C and Node build steps, have no RDF 1.2 support,
  and use error-recovering parses (§7.1).
- **Embedding oxfmt, or shelling out to oxfmt or Prettier,** for JSON-LD (§7.2).
- **The `pretty` crate.** It has no line suffix or break propagation for trailing comments
  (§3.2).
- **Alignment of predicate columns, prefix IRIs, or `VALUES` columns.** Every rename
  re-indents unrelated lines, which is hostile to diffs. Prettier makes the same call.
  The one exception is opt-in: `align-values` (Phase 2, if easy) for hand-maintained
  `VALUES` tables, which otherwise need the ignore pragma.
- **Many style options.** Prettier's option philosophy applies: every option splits the
  ecosystem, and consistent output is the product. The §2.2 keys are the bounded
  exception, decided item by item (§13). Each one maps to a convention that existing
  files and tools already use, so forcing the default would churn those files.
- **Formatting RDF/XML** (§4.5).
- **Formatting SPARQL inside SHACL literals.** It changes the graph (§4.2).
- **Canonicalizing literal lexical forms.** It changes RDF terms (N9).

## 13. Decisions and open questions

### 13.1 Decided (2026-09-30)

The maintainer reviewed and decided the default style item by item. "Kept" means the rule
this spec already stated; a key name means the default can be changed (§2.2).

- **Line width** (was Q4): 100; `line-width` 40..=400.
- **Indent width:** 2 spaces, never tabs; `indent-width` 1..=8.
- **Keyword and builtin case:** grammar spelling (N3); fixed.
- **Turtle subject blocks:** diff-first (subject alone, trailing `;`, lone `.`);
  `turtle-layout = "conventional"` in Phase 2, only if easy.
- **SPARQL triples** (was Q2): compact conventional form, not the Turtle form; fixed.
- **`WHERE`:** on its own line, inserted or removed per N13; fixed.
- **SPARQL `{ }` groups:** always expanded; fixed.
- **`PREFIX` vs `@prefix`** (was Q1): `PREFIX` by default; `directive-style = "turtle"`.
- **Prefix order:** sorted, not aligned (N5); `prefix-groups` adds goimports-style groups.
- **SPARQL `.`:** after every triples statement and nowhere else (N13); fixed.
- **Blank lines:** kept and collapsed, added around multi-line statements (§3.3); fixed.
- **`rdf:type` → `a` and `a` first** (was Q3): on; `type-shorthand = false` keeps both
  as written.
- **Full IRI → prefixed name** (N7): on; `compact-iris = false`.
- **Numeric and boolean shorthand** (N9): on; fixed.
- **Quotes** (N10): double; `quote-style = "preserve"`.
- **SPARQL clause layout:** as §4.1; fixed.
- **Broken `&&`/`||` chains:** operator leading, continuation lines one level deeper
  than the first operand (§4.1); `operator-position = "trailing"`, applied to every
  broken chain.
- **Brackets, object lists and collections:** as §4.1 and §4.2; fixed.
- **Comments and pragmas:** as §3.3 and §3.4; fixed.
- **Unused prefixes:** kept (N25); `prune-prefixes` in Phase 2.
- **`$v` → `?v`:** changed. Variables keep their sigil, and N4 is now "never".
- **`VALUES`:** one row per line, not aligned; `align-values` in Phase 2, only if easy.
- **Escapes, tag case, `^^xsd:string` in SPARQL/Turtle/TriG:** verbatim (N11, N23, N24);
  fixed.
- **Turtle/TriG sorting:** off; `sort` / `--sort`.
- **TriG `GRAPH`:** written by default; follows `directive-style`, so `@prefix` files
  omit it.
- **N-Triples/N-Quads sorting** (was Q5): changed to off by default. `--sort` opts in as
  for Turtle, and `--canonicalize` still sorts (N20).
- **N-Triples/N-Quads term spelling:** canonical N-Triples; fixed.
- **JSON-LD key order and layout:** §4.4; fixed.
- **Configuration surface** (was Q9): the twelve §2.2 keys. `.editorconfig` is not read.
  Output always has LF line endings, no trailing whitespace, one final newline and no
  tabs.

### 13.2 Open

- **Q6. Should `/$/format` default to `authenticated`** when the server's anonymous
  principal has no dataset grants? Default: no. It reads no data and is rate-limited.
- **Q7. Format on run default.** Default: off.
- **Q8. Conflict with Prettier on `*.jsonld`.** Document `.prettierignore`, or run
  Prettier on JSON-LD instead of our printer when a project already uses Prettier?
  Default: document the ignore.
- **Q10. An MCP `format` tool for agents that write queries?** Default: later, with
  [C11](C11-mcp-server.md)'s next revision.
- **Q11. spargebra 0.4.7 parses `-` and `/` as right-associative.** This finding is
  unrelated to the formatter and came up while researching this spec.
  `AdditiveExpression` and `MultiplicativeExpression` recurse on the right (`parser.rs`
  near line 2165). At the time of writing, `sparkles query` returns `2` and `4` for
  `SELECT (1 - 2 - 3 AS ?x) (8 / 4 / 2 AS ?y) {}`. The correct values are `-4` and `1`.
  The formatter is unaffected, because it never re-associates and the algebra check
  compares spargebra with itself. The query engine needs its own fix and a regression
  test. The fix can rebuild the chains left-associatively after parsing, or go upstream.

## 14. Sources

**W3C and IETF**
- SPARQL 1.1 Query Language (REC 2013-03-21):
  - §4.1.1.1–4.1.4 terms;
  - §4.2.4 `a`;
  - §5.1, §5.2.2 filter scope;
  - §6, §7, §8.2, §10.1–10.2, §15.1, §15.5;
  - §18.2.2.x, §18.2.5, §18.5;
  - §19.3–19.8, where the §19.8 notes cover case-insensitive keywords and longest match.
- SPARQL 1.2 Query Language (WD 2026-09-30): §4.3 Nested triple patterns, §4.3.2
  Annotation syntax, §4.4 Version announcement, §5.1, §5.2.2, §18.3, §19.2 Escape
  sequences, §19.7 Grammar. Also the `sparql12/codepoint-escapes` tests in rdf-tests-cg.
- SPARQL 1.1 Update (REC 2013) §3. SPARQL 1.1 Query Results JSON Format §3.1.
- RDF 1.1 Turtle (REC 2014): §2.4, §2.5, §6. RDF 1.2 Turtle (WD 2026-09-14): §2.4
  Version announcement, §2.5 IRIs, §2.6.1–2.6.3 literals, §2.8–2.11 (incl. §2.11.1
  Annotation syntax), §6.1–6.5, §7.1–7.3.
- RDF 1.1 TriG (REC 2014) and RDF 1.2 TriG (WD): the `block`/`wrappedGraph` grammar, with
  `GRAPH` optional.
- RDF 1.2 N-Triples (WD 2026-09-24): §2.3 Triple terms, §3 Canonical N-Triples, §5.2
  Comments. RDF 1.2 N-Quads (WD): Canonical N-Quads.
- RDF 1.1 Concepts and Abstract Syntax (REC 2014): §3 (graphs are sets), §3.3 literals and
  language tags, §3.4 blank nodes, §3.6 graph comparison, §4 datasets. RDF 1.2 Concepts
  (WD): literal term equality, base direction, triple terms.
- RDF Dataset Canonicalization RDFC-1.0 (REC 2024-05-21), §4.
- JSON-LD 1.1 (REC 2020-07-16) and JSON-LD 1.1 Processing Algorithms and API: Context
  Processing, Create Term Definition, the Expansion Algorithm.
- RFC 8259 §2 and §4. BCP 47 (RFC 5646) §2.1.1.

**Pretty printing**
- P. Wadler, "A prettier printer" (1997/2003).
- C. Lindig, "Strictly Pretty" (2000).
- D. Oppen, "Prettyprinting" (TOPLAS 1980).
- Prettier docs (MIT): Rationale, Option Philosophy, CLI (`--check`, `--list-different`,
  exit codes 0/1/2), API `formatWithCursor`, Ignoring Code (`prettier-ignore`), and
  `commands.md` (doc builders: `group`, `line`, `softline`, `hardline`, `lineSuffix`,
  `breakParent`, `ifBreak`).

**Oxc / oxfmt** (MIT)
- oxc.rs/docs/guide/usage/formatter.html;
- the oxc repository `Cargo.toml` files of `apps/oxfmt`, `crates/oxc_formatter` and
  `crates/oxc_formatter_json` (`publish = false`);
- the `oxc_formatter_json` public API and options docs;
- npm `oxfmt` 0.71.0 metadata;
- crates.io records for `oxfmt` (unrelated crate) and `oxc_formatter` (2023 only).

**Prior art** (public documentation and licenses only; no code read)
- Apache Jena (Apache-2.0): `qparse --print=query`, and `riot --formatted=turtle`, which
  re-serializes and drops comments.
- rdflib (BSD-3-Clause): its Turtle serializer, which re-serializes.
- turtlefmt by Helsing (Apache-2.0; Rust, tree-sitter-turtle): its README format rules
  (`"` quotes, shorthand literals, `a`, fewer escapes).
- atextor/turtle-formatter (Apache-2.0, Java; highly configurable).
- sparqling/sparql-formatter (MIT, JS).
- Qlue-ls (MIT, Rust SPARQL language server): its documented `[format]` options, an
  example of the configurability this spec avoids.
- TopBraid: the published TopBraid/DASH Turtle files (TopBraid SHACL API, Apache-2.0),
  whose layout puts the subject on its own line and a lone `.`.
- tree-sitter-sparql and tree-sitter-turtle (MIT): their grammar sources were checked
  only for RDF 1.2 constructs, which they lack.

**Crates:** rowan and cstree (MIT OR Apache-2.0), `ignore` (Unlicense OR MIT), `similar`
(Apache-2.0), `lsp-server` and `lsp-types` (MIT OR Apache-2.0), `json-event-parser`
(MIT OR Apache-2.0).

**Sparkles code:**
- `crates/sparkles-server/src/main.rs` (the `Cmd` subcommands);
- `http.rs`:
  - the route table;
  - `syntax_error_body`;
  - the `DefaultBodyLimit` of 8 GiB;
- `auth/routes.rs` (`ROUTES`, `need`, `Need::Caller`);
- `ratelimit.rs` (`classify`, where `/$/` POSTs default to `Admin`);
- `ui/src/lib/components/SparqlEditor.svelte` (CodeMirror 6 keymap, `Prec.highest`);
- `ui/src/lib/sparql-lang.ts` (`highlightError`, 1-based columns);
- `ui/src/routes/query/+page.svelte` (toolbar, `showError`);
- `crates/sparkles/src/sparql/mod.rs` (`parse_query` with dataset prefixes);
- `crates/sparkles/tests/w3c.rs` (`SPARKLES_W3C_DIR`);
- `mise.toml` (`fmt:check`: `cargo fmt --check` plus Prettier);
- spargebra 0.4.7 `parser.rs` (random blank nodes and variables; `AdditiveExpression`);
- oxttl 0.2.4 (anonymous blank nodes are random, labels are kept, language tags are
  lowercased);
- oxrdf 0.3.4 `CanonicalizationAlgorithm` (`Rdfc10` does not cover RDF 1.2 triple terms).

## Outcome

**Phases delivered** (2026-09-30 to 2026-10-01). SPARQL landed first, end to end: the
`sparkles-fmt` crate, `sparkles fmt`, `POST /$/format`, and the UI Format button with
Shift+Alt+F, one-step undo and format on run. The next round added JSON-LD, Turtle/TriG
and N-Triples/N-Quads. The line formats came with streaming, `--sort` with LZ4-framed
spill runs, and `--canonicalize` with RDFC-1.0. That round also added `sparkles lsp` with
[editor snippets](../editors.md), `align-values`, `prune-prefixes`, Turtle/TriG sorting
and `turtle-layout = "conventional"`, so neither optional key was dropped. The
WebAssembly build (`crates/sparkles-fmt-wasm`) and streaming Turtle/TriG came last. The
Phase 3 fuzzing targets had not landed when this record was written.

**Decided by the maintainer:** the §13.1 style defaults and the twelve configuration keys
were reviewed item by item before Phase 1. The WebAssembly module is optional. A
separate `mise run ui:wasm` task builds it, and the UI uses it when present and falls
back to `POST /$/format` otherwise. CI only checks the `wasm32-unknown-unknown` build,
which is now listed in `rust-toolchain.toml`. The Nix UI package includes the module by
default.

**Chosen during implementation** (open to revision): with sort off, the CLI streams
Turtle/TriG of any size, and `--max-bytes` (256 MiB) then bounds the largest statement.
The "convert to N-Triples" error remains for SPARQL, JSON-LD and sorted Turtle/TriG.
`sparkles lsp` reads only the nearest `.sparklesfmt.toml`. In Turtle and TriG,
`prune-prefixes` drops an unused declaration even when a later directive redefines its
label, because its scope ends at the redefinition. An unused redefinition stays only when
the last declaration of its label that the output keeps binds a different namespace.
Dropping it would extend that binding over the redefinition's scope, and the next run
would compact IRIs there with it. Streamed and in-memory formatting follow the same rule.
When pruning leaves a directive block with only the section comments of its dropped
declarations, those comments are spaced as a comment block before the next statement,
which is how the next run reads them. A blank line separates them from a multi-line
statement on either side. Before this rule the output was not a fixpoint, and a stream
reported `unstable-format` there instead of a syntax error later in the input.
The opt-in task `fmt:jsonld-compare`, outside `ci`, compares the JSON-LD layout with
`oxfmt`, which follows Prettier's JSON conventions and is already pinned in `ui/`. The
SHACL shapes field became a CodeMirror editor so that its Format keeps one-step undo.

**Other deviations:** the body limit flag is `--format-max-mb` (default 16), like the
server's other size flags. An ignore pragma right before the first node leads that node
instead of joining the file header (§3.3). JSON-LD nested deeper than 256 levels is
refused. The UI formats in the browser when the module is built, so
`--format-endpoint off` disables only the endpoint. The spargebra associativity bug
(§13.2 Q11) was fixed in a patched copy under `vendor/spargebra`.

**Tests at landing:** all 1,102 W3C SPARQL queries and updates format, pass the checks
and are fixpoints. The comment-injection sweeps are clean: a comment after every token
under both operator positions, and 300k property cases. The reference parsers agree with
all 1,132 W3C Turtle, TriG, N-Triples and N-Quads cases. The Turtle/TriG suites and the
SHACL test files format under the defaults and with every key flipped. N-Triples/N-Quads
output matches the canonical-form tests byte for byte, and streamed Turtle/TriG equals
the in-memory output. JSON-LD has no local W3C suite. Jena's and Oxigraph's JSON-LD files
and the W3C RDF results written as JSON-LD stand in for one, plus
`SPARKLES_JSONLD_TESTS` when set.

**Performance:** the §8 targets are not yet recorded in [BENCHMARKS](../BENCHMARKS.md).
The WebAssembly module is about 250 KB with brotli, well under the 1.5 MB budget of §7.4.

**MCP tool.** The `format` tool of §13.2 Q10 landed on 2026-10-02 in `sparkles mcp`
([C11](C11-mcp-server.md)). It formats the six languages with `sparkles_fmt::format`, the
engine of `sparkles fmt`. It takes the camelCase style options of `POST /$/format` and
shares that endpoint's parsing of them. It returns the language, the text, whether the
text changed and the warnings. The text may be up to 1 MiB, and `--mcp-max-bytes` caps
the result. A call runs within its `timeoutSeconds`. It reads no dataset and no config
file.

**Deferred:** fuzzing (§9.7), `sparkles lint`.
