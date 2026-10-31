# G05: Command-line equivalents of Jena's tools, and IRI and language-tag checks

> **Status:** implemented
>
> **Phases:** One phase. Every command of §3 shipped, with the checks of §4 in `convert`,
> `load` and `/$/validate/iri`.
>
> **User docs:** [Usage: File tools](../USAGE.md#file-tools) ·
> [Features](../querying/README.md#command-line) ·
> [Comparison with Jena](../COMPARISON.md)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This spec draws on the Sparkles code, the command sources of Apache Jena (`jena-cmds`,
`jena-langtag` and `jena-iri3986`, Apache-2.0) for behaviour and flags, and the
standards listed in §9.

## 1. Summary

Sparkles has commands for databases (`load`, `query`, `update`, `dump`, `compact`,
`backup`, `stats`, `check`) and for validation and reasoning (`infer`, `shacl`, `shex`).
Jena users also reach for a set of file-level tools that Sparkles lacks. This spec adds
them as subcommands of the `sparkles` binary:

| Jena | Sparkles | What it does |
|---|---|---|
| `riot`, `turtle`, `ntriples`, `nquads`, `trig`, `rdfxml` | `convert` (alias `riot`) | Parse, validate, count and convert RDF files, streaming |
| `qparse` | `qparse` | Print a query as SPARQL, as SPARQL algebra, or as the physical plan |
| `uparse` | `uparse` | Print an update as SPARQL or as SPARQL algebra |
| `rdfdiff`, `rdfcompare` | `compare` (aliases `rdfcompare`, `rdfdiff`) | Compare two files up to blank-node isomorphism |
| `iri` | `iri` | Show how an IRI parses and what is wrong with it |
| `langtag` | `langtag` | Show how a language tag parses and what is wrong with it |
| `rsparql`, `rupdate` | `rsparql`, `rupdate` | Query and update any SPARQL 1.1 Protocol endpoint |
| `rset` | `rset` | Convert a SPARQL result set between formats |

It also adds IRI and language-tag warnings to parsing (Jena's RIOT checking). They are
off by default and turned on with `--check` on `convert` and `load`.

Goals:
- A Jena user finds each tool under a familiar name or alias, with the flags that matter
  (`--syntax`, `--output`, `--validate`, `--count`, `--base`, `--results`, `--service`).
- The tools reuse the existing input code: format detection, gzip, zstd, brotli and LZ4
  inputs, the parsers' error positions, and the SPARQL parser and planner.
- Conversion streams, so a file larger than memory converts in bounded memory.
- Exit statuses are useful in scripts.

Non-goals:
- `schemagen`, `rdfpatch` and Jena's TDB commands. `rdfpatch` needs an RDF Patch reader,
  which belongs with applying patches.
- Jena's `--rdfs` inference while parsing, and `--formatted` pretty printing. `sparkles
  fmt` formats Turtle and TriG.
- A registry of IANA subtags or URI schemes. The checks are syntax and scheme rules only.

## 2. `--server` and plain SPARQL endpoints

`query`, `update` and `load` take `--server URL --dataset NAME`. They build Sparkles'
URLs from the two (`/{dataset}/sparql`, `/{dataset}/update`, `/{dataset}/data`), send the
token saved by `sparkles auth login`, and refuse plain http to a host other than
localhost. A plain endpoint URL such as `https://query.wikidata.org/sparql` or a QLever
endpoint therefore does not work with `--server`. The new `rsparql` and `rupdate`
commands take the endpoint URL as it is and speak the SPARQL 1.1 Protocol, so they work
with any server.

## 3. Commands

### 3.1 `convert` (alias `riot`)

```
sparkles convert [FILES]... [--syntax LANG] [--output LANG] [--base IRI]
                 [--count | --validate | --sink] [--check] [--strict] [--lenient]
                 [--merge] [--compression CODEC] [--compress [CODEC]] [--time]
                 [-r] [--include GLOB]... [--exclude GLOB]...
                 [-o FILE | --out-dir DIR [--overwrite] [-j N]]
                 [--mapping FILE | --template FILE.rq | --key COLUMN]
```

* Inputs are files, or standard input when there are none or a file is `-`. The syntax
  comes from `--syntax`, else from the file extension (`data.ttl.gz` is gzipped Turtle),
  else N-Quads, which also reads N-Triples. Compression is detected from the magic bytes
  and the extension, as `load` does, or set with `--compression`.
* Syntax names are Jena's and Sparkles' (`Turtle`, `TTL`, `N-Triples`, `NT`, `N-Quads`,
  `NQ`, `TriG`, `RDF/XML`, `JSON-LD`, `N3`) in any case, plus media types.
* The output goes to standard output in `--output` syntax, N-Quads by default, which is
  N-Triples for triples in the default graph. `--compress` compresses it, with gzip when
  no codec is named, as Jena's `--compress` does.
* Conversion streams. The output starts after the first statement. Turtle, TriG and
  RDF/XML output declare the prefixes that the input declared before its first
  statement. Later prefixes are not used, and their IRIs are written in full.
* A quad in a named graph cannot be written as Turtle, N-Triples or RDF/XML. It is
  dropped with one warning that counts them, as Jena does. `--merge` writes it into the
  default graph instead.
* `--count` prints the number of triples (triple syntaxes) or quads (quad syntaxes) of
  each input, and a total for several. `--sink` parses and prints nothing. `--validate`
  is `--sink --check --strict`, as in Jena.
* Without output, files are parsed in parallel, as `load` parses them. When a parse fails,
  the file is parsed again in order, so that errors have exact line and column numbers,
  and up to 20 errors are reported.
* `--lenient` skips the parsers' IRI and language-tag validation, as `load --lenient`
  does.
* `--time` prints the time and rate of each input on standard error.

### 3.2 `qparse` and `uparse`

```
sparkles qparse [QUERY] [--query FILE] [--print query|algebra|plan]... [--base IRI]
                [--loc DB | --data FILE...]
sparkles uparse [UPDATE] [--update FILE] [--print update|algebra]... [--base IRI]
```

The text comes from the argument, the file, or standard input. The parse is the engine's
own, so Sparkles' extensions (custom aggregates, the scoping check of `BIND`) apply.

* `query` (the default) prints the parsed query as SPARQL. With the `fmt` feature it
  goes through the formatter.
* `algebra` (alias `op`) prints the SPARQL algebra in SSE, the S-expression form of Jena's
  `qparse --print=op`, indented. The operators are spargebra's: `(project (?x) (bgp
  (triple ?x ?p ?o)))`. They are close to Jena's but not equal. Jena writes `(table
  unit)` where spargebra writes nothing, and the names of path operators differ.
* `plan` (alias `opt`) prints the physical plan with estimated rows and costs, as `query
  --explain` does. It is planned against an empty in-memory database by default, or
  against `--loc` or `--data` for realistic statistics. Jena's `opt` is its optimized
  algebra. Sparkles has no separate algebra optimizer, so the plan takes its place.

A syntax error prints the message with its line and column and exits with status 1.

### 3.3 `compare` (aliases `rdfcompare`, `rdfdiff`)

```
sparkles compare A B [--syntax LANG] [--base IRI] [--merge] [-q|--quiet]
```

Both files are read into memory as RDF datasets (sets of quads, so duplicates do not
count). They are isomorphic when a bijection between their blank nodes makes them equal.
The comparison:

1. Splits each dataset into ground quads and the connected groups of quads that share
   blank nodes.
2. Compares the ground quads as sets.
3. Canonicalizes each blank-node group with RDFC-1.0 (oxrdf's `Rdfc10` with SHA-256, as
   the formatter's `canonicalize` does) and compares the groups as multisets of
   canonical forms.

An isomorphism maps groups to groups, and a canonical form identifies its group up to
isomorphism, so the datasets are isomorphic exactly when both comparisons find no
difference. Comparing groups rather than whole canonical datasets keeps the diff local.
One changed blank node shows its own group and not a relabeling of every other.

The exit status is 0 when the files are isomorphic, 1 when they differ and 2 on an error,
as `diff(1)` does. The diff lists the quads only in A with `<` and the quads only in B
with `>`. Ground quads come first, then each differing blank-node group with labels
`_:c14nN` that are unique in the output. A summary goes to standard error. `--merge`
compares the merged graphs and ignores graph names. `--quiet` prints nothing and only
sets the exit status.

### 3.4 `iri` and `langtag`

```
sparkles iri IRI... [--base IRI] [--format text|json] [--strict]
sparkles langtag TAG... [--format text|json] [--strict]
```

`iri` prints each IRI's components (scheme, authority, user info, host, port, path, query
and fragment), whether it is absolute or a relative reference, its resolution against
`--base`, its RFC 3986 §6 normalized form when that differs, and its errors and
warnings. An error is a violation of the RFC 3987 grammar, which oxiri reports. The
command adds the position of the first character that can never occur in an IRI.

Warnings come from the rules of §4. `langtag` prints the subtags (language, extended
language, script, region, variants, extensions, private use), the canonical case form,
an RDF 1.2 base direction (`en--ltr`), and errors and warnings. Angle brackets around an
IRI and `@` before a tag are accepted.

The exit status is 1 when any input has an error, or a warning under `--strict`, and 0
otherwise.

### 3.5 `rsparql`, `rupdate` and `rset`

```
sparkles rsparql --service URL [QUERY] [--query FILE] [--results FMT] [--post]
                 [--default-graph-uri IRI]... [--named-graph-uri IRI]...
                 [--header 'Name: value']... [--user NAME[:PASSWORD]] [--timeout SECS]
                 [--insecure-http]
sparkles rupdate --service URL [UPDATE] [--update FILE] [--form]
                 [--using-graph-uri IRI]... [--using-named-graph-uri IRI]...
                 [--header ...]... [--user ...] [--timeout SECS] [--insecure-http]
sparkles rset [FILE] [--in FMT] [--results FMT]
```

`rsparql` sends the query by GET when the URL stays under 2,000 bytes and as a POST form
otherwise or with `--post`, as the protocol allows. `--results` takes `text` (the
default), `json`, `xml`, `csv` and `tsv`, and for CONSTRUCT and DESCRIBE `ttl`, `nt`,
`nq`, `trig`, `jsonld` and `rdfxml`. Each asks for the matching media type. With `text`,
results come back as SPARQL JSON or XML and are printed as Jena's text table, and a
graph comes back as Turtle and is printed as it is. A status other than 2xx prints the
server's message and exits with status 1.

`rupdate` posts the update as `application/sparql-update`, or as a form with `--form`.

`--user NAME` without a password asks for one on the terminal. Credentials are never
read from the Sparkles credentials file, because the endpoint may be any server. Plain
http to a host other than localhost is refused when credentials or headers would be
sent, unless `--insecure-http` is given.

`rset` reads a result set in SPARQL JSON, XML or TSV (from the extension, `--in`, or JSON
on standard input) and writes it as `text`, `json`, `xml`, `csv` or `tsv`.

## 4. IRI and language-tag checks

### 4.1 IRI warnings

Each rule names a standard. Errors are oxiri's grammar errors. Everything here is a
warning.

| Code | Rule | Source |
|---|---|---|
| `scheme-case` | The scheme is not in lower case | RFC 3986 §3.1, §6.2.2.1 |
| `percent-case` | A percent-encoding uses lower-case hex digits | RFC 3986 §2.1, §6.2.2.1 |
| `percent-unreserved` | A percent-encoding encodes an unreserved character | RFC 3986 §2.3, §6.2.2.2 |
| `host-case` | A registered host name has upper-case letters | RFC 3986 §3.2.2, §6.2.2.1 |
| `userinfo` | The authority has user information, or a password | RFC 3986 §3.2.1, §7.5 |
| `dot-segments` | The path of an absolute IRI has `.` or `..` segments | RFC 3986 §5.2.4, §6.2.2.3 |
| `empty-port` | The authority ends with `:` | RFC 3986 §3.2.3, §6.2.3 |
| `default-port` | An `http` IRI gives port 80, or `https` port 443 | RFC 3986 §6.2.3, RFC 9110 §4.2 |
| `ipv4` | A dotted host of four numbers has one above 255 | RFC 3986 §3.2.2 |
| `http-host` | An `http` or `https` IRI has no authority or an empty host | RFC 9110 §4.2.1, §4.2.2 |
| `urn-syntax` | A `urn:` IRI has a malformed namespace identifier or an empty specific string | RFC 8141 §2 |
| `urn-x` | A `urn:` namespace starts with `X-` | RFC 8141 §5.1 |
| `urn-ascii` | A `urn:` IRI has characters outside ASCII | RFC 8141 §2 |
| `uuid` | A `urn:uuid:` IRI is not 8-4-4-4-12 hex digits, has upper-case digits, or has a query or fragment | RFC 9562 §4, RFC 8141 |
| `uuid-scheme` | `uuid:` is not a registered scheme | IANA URI schemes |
| `oid` | A `urn:oid:` IRI is not dotted decimal numbers | RFC 3061 |
| `oid-scheme` | `oid:` is not a registered scheme | IANA URI schemes |
| `file` | A `file:` IRI has a relative path | RFC 8089 §2 |
| `did` | A `did:` IRI has no lower-case method name or no method-specific identifier | W3C DID Core §3.1 |
| `relative` | The input is a relative reference, not an IRI (for the `iri` command) | RFC 3987 §2.2 |

### 4.2 Language-tag warnings

| Code | Rule | Source |
|---|---|---|
| `case` | The tag is not in the canonical case | BCP 47 §2.1.1 |
| `grandfathered` | The tag is one of the grandfathered tags | BCP 47 §2.2.8 |
| `extlang` | The tag has an extended language subtag. The extlang alone is the preferred tag | BCP 47 §2.2.2, §4.1 |
| `reserved-language` | The primary language has four letters (reserved) or five to eight (rarely registered) | BCP 47 §2.2.1 |
| `direction` | An RDF 1.2 base direction is neither `ltr` nor `rtl` (an error) | RDF 1.2 Concepts §3.3 |

The case of a language tag does not change its meaning in RDF (RDF 1.1 Concepts §3.3),
so `case` is informational.

### 4.3 Checking while parsing

`convert --check` and `load --check` run the rules over every IRI of the data (subjects,
predicates, objects, graph names, datatypes, and the terms inside triple terms) and
every language tag. Each distinct value is reported once per input, with at most 100
warnings printed per input and a count of the rest. `--strict` turns warnings into a
failure. For `load` the check is a separate parse before the load, so it costs a second
pass over the input, and with `--strict` nothing is loaded when a warning is found. The
rules run as an extra pass and leave the loader unchanged.

`/$/validate/iri` adds the warnings of §4.1 to its `warning` array.

## 5. Exit statuses

| Command | 0 | 1 | 2 |
|---|---|---|---|
| `convert` | Parsed (and written) without errors | A parse error, or a warning under `--strict` | Usage error |
| `qparse`, `uparse` | Parsed | Syntax error | Usage error |
| `compare` | Isomorphic | Different | Error (parse, I/O, usage) |
| `iri`, `langtag` | No errors | An error, or a warning under `--strict` | Usage error |
| `rsparql`, `rupdate` | 2xx response | Any other response, or no connection | Usage error |

## 6. Code layout

The commands live in a new module directory of the server crate, `src/tools/`, with one
file per command group. A flattened clap subcommand enum keeps the changes to `main.rs`
to a few lines, so that other work on the CLI merges cleanly. The term rules live in
`src/tools/terms.rs` and are shared with `/$/validate/iri`. The core crate exposes
`sparql::update::parse_update` for `uparse`. Nothing else in the core changes.

New dependencies: none. `sparesults` (MIT OR Apache-2.0) is already a dependency of the
`sparkles` crate and becomes a direct dependency of the server crate for `rset` and
`rsparql`.

## 7. Acceptance examples

* **A1.** `sparkles convert data.ttl.gz --output nt` prints N-Triples. `cat data.nt |
  sparkles riot --syntax nt --output ttl` prints Turtle with the input's prefixes.
* **A2.** `sparkles convert --count a.ttl b.nq` prints one count per file and a total.
* **A3.** `sparkles convert --validate bad.ttl` prints `bad.ttl: Parser error at line 3
  column 9: …` and exits with 1.
* **A4.** `sparkles convert --check data.ttl` warns about `<HTTP://Example.org/a>`
  (`scheme-case`) and `"x"@en-us` (`case`), writes the data, and exits with 0. With
  `--strict` it exits with 1.
* **A5.** `sparkles qparse --print algebra 'SELECT * { ?s ?p ?o }'` prints `(bgp (triple
  ?s ?p ?o))`. A syntax error exits with 1.
* **A6.** `sparkles compare a.ttl b.ttl` on two files that differ only in blank-node
  labels exits with 0. When one triple of a blank-node group differs, the diff shows
  that group only.
* **A7.** `sparkles iri 'http://Example.org:80/a/../b'` reports `host-case`,
  `default-port` and `dot-segments` and the normalized form `http://example.org/b`.
* **A8.** `sparkles langtag zh-yue-hk` reports the extended language, the canonical form
  `zh-yue-HK` and the `extlang` warning.
* **A9.** `sparkles rsparql --service http://localhost:3030/ds/sparql 'SELECT …'` prints a
  text table, and with `--results csv` prints CSV, against Sparkles and any other
  endpoint.
* **A10.** `sparkles rset results.srj --results csv` prints CSV.

## 8. Rejected alternatives

* **A plain endpoint URL in `query --server`.** `--server` names a Sparkles server and
  uses its auth tokens and URL layout. Overloading it would send saved tokens to other
  servers. Separate commands keep the two apart.
* **Canonicalizing whole datasets for `compare`.** It decides isomorphism, but RDFC-1.0
  numbers blank nodes in hash order, so one change can relabel every blank node and the
  diff shows unrelated lines.
* **Checking terms inside the loader.** It would touch the parallel parse and the store.
  A separate pass costs a second parse only when asked for.
* **A subtag registry for `langtag`.** It is a large data file that needs updates, for
  warnings that are rarely wrong in practice.

## 9. Sources

* Apache Jena `jena-cmds` (`riot`, `CmdLangParse`, `ModLangParse`, `ModLangOutput`,
  `qparse`, `uparse`, `rdfdiff`, `rdfcompare`, `iri`, `rsparql`, `rupdate`, `rset`),
  `jena-langtag` (`CmdLangTag`) and `jena-iri3986` (`Issue`), Apache-2.0, read for
  behaviour, flags and output. No code was copied.
* RFC 3986 (URI syntax), RFC 3987 (IRIs), RFC 9110 §4.2 (http and https URIs), RFC 8141
  (URNs), RFC 9562 (UUIDs), RFC 3061 (the OID URN namespace), RFC 8089 (file URIs), W3C
  DID Core 1.0 §3.1, the IANA URI scheme registry, BCP 47 (RFC 5646), RDF 1.1 and RDF 1.2
  Concepts.
* W3C RDF Dataset Canonicalization (RDFC-1.0) and SPARQL 1.1 Protocol, Query Results
  JSON, XML, CSV and TSV.
* The Sparkles code: `sparkles::io`, `sparkles::codec`, the formatter's canonicalization,
  `query --explain`, the remote client and `/$/validate/*`.

## Provenance

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

- **Spec:** [`G05-command-line-tools.md`](G05-command-line-tools.md), written on
  2026-10-02 independently from:
  - the Sparkles code;
  - Apache Jena's `jena-cmds` (`riot`, `CmdLangParse`, `ModLangParse`, `ModLangOutput`,
    `qparse`, `uparse`, `rdfdiff`, `rdfcompare`, `iri`, `rsparql`, `rupdate`, `rset`),
    `jena-langtag` (`CmdLangTag`) and `jena-iri3986` (`Issue`) sources (Apache-2.0),
    read for behaviour, flags, output and the list of IRI issues. No code was copied;
  - RFC 3986, RFC 3987, RFC 9110 §4.2, RFC 8141, RFC 9562, RFC 3061, RFC 8089, W3C DID
    Core 1.0 §3.1, the IANA URI scheme registry, BCP 47 (RFC 5646), RDF 1.1 and 1.2
    Concepts, RDFC-1.0, the SPARQL 1.1 Protocol and the SPARQL result formats.
- **Implementation** (2026-10-02): from the spec and the Sparkles code. The IRI and
  language-tag rules, dot-segment removal and the per-group isomorphism check are written
  from the RFCs. **Dependencies:** none new. `sparesults` 0.3 (MIT OR Apache-2.0), already
  a dependency of `sparkles`, became a direct dependency of `sparkles-server`.
- **Rejected** (spec §8):
  - a plain endpoint URL in `query --server`, which would send saved tokens to other
    servers;
  - canonicalizing whole datasets for the diff of `compare`;
  - checking terms inside the loader;
  - a subtag registry for `langtag`.

## Outcome

**Delivered.** All of §3 and §4 landed on 2026-10-02 in `crates/sparkles-server/src/tools/`,
with the commands flattened into `sparkles`' own through one variant of the CLI enum:
- `convert` (alias `riot`) with `--syntax`, `--output` (alias `--out`), `--count`,
  `--sink` (alias `--null`), `--validate`, `--check`, `--strict`, `--lenient`, `--base`,
  `--merge` (alias `--union`), `--compression`, `--compress [CODEC]` and `--time`;
- `qparse` and `uparse` with `--print query|algebra|plan` (and Jena's `op` and `opt`);
- `compare` (aliases `rdfcompare` and `rdfdiff`) with the per-group comparison of §3.3;
- `iri` and `langtag`, with `--format json` and `--strict`;
- `rsparql`, `rupdate` and `rset`;
- `load --check` and `load --strict`, and the scheme warnings of `/$/validate/iri`.

The language-tag canonical case of `/$/validate/langtag` now comes from oxilangtag's
normalization, shared with `langtag`. The text table of `query` moved to
`tools/table.rs` and is shared with `rsparql` and `rset`. The core crate's only change is
that `sparql::update::parse_update` became public.

Later, `convert` and `load` took Jena's TriX, RDF Thrift, RDF Protobuf and RDF/JSON, by
file extension (also behind a compression extension) or by `--syntax` and `--output`
names. `convert` transcodes such an input to N-Quads on a second thread while it streams,
and `load` transcodes each file into a temporary N-Quads file before the load, as the
server does with request bodies. The reader and writer are the server's
(`http/jena_formats.rs`), and TriX's live in the core crate (`sparkles::trix`). The
module later moved into the core crate as `sparkles::jena_formats`, so that the Python
bindings read and write the same syntaxes.

On 2026-10-03 `convert` learned to read directories and to tell a syntax from the
content. `--recursive` reads the files below a directory, filtered by `--include` and
`--exclude` globs. `--output-file` writes one output stream to a file, and `--out-dir`
writes one file per input, mirroring the tree, with `--jobs` files converted in
parallel and `--overwrite` needed to replace a file. When neither `--syntax` nor the
extension names a syntax, the first 8 KiB after decompression decide it
(`tools/sniff.rs`). A file in a directory whose syntax is unknown or ambiguous is skipped
with a warning, and a file given by name or standard input with ambiguous content is an
error that names the candidates. CSV and TSV files go through the mapping of `load`
(spec [C05](C05-tabular-imports.md)), with `--mapping`, `--template` and `--key`. Errors
read `file:line:column: message`. Directory runs and `--out-dir` print each file's count
and a summary, and exit with status 1 when any file failed.

**Deviations and decisions.**
- `convert --out` was already Jena's alias of `--output`, the output syntax. The output
  file therefore has its own flag, `--output-file` (`-o`), and `--format` became a
  third alias of `--output`.
- Standard input that matches no syntax is still read as N-Quads, so that a parse error
  names the line, as before content detection existed.
- `qparse` and `uparse` print the input formatted by `sparkles fmt` rather than
  spargebra's rendering of the parse. The rendering moves aggregates into a subquery and
  names them with random hex, which reads worse than the input. The parse still decides
  whether the text is valid. Without the `fmt` feature the rendering is printed.
- The made-up names of aggregates and blank nodes in the algebra and the plan print as
  `?.0` and `_:b0`, as Jena names them, instead of spargebra's random hex.
- oxrdf lowercases language tags while parsing, so data checks cannot see a tag's
  original case. The `case` warning is given by `langtag` only, and `convert --check` and
  `load --check` skip it.
- `load --check` also runs before `load --server`, since it only reads the files.
- `rsparql` and `rupdate` are built with the `auth` cargo feature, which brings the HTTP
  client, as `--server` is.
- `iri` reports the position of the first character that cannot occur in an IRI, which
  oxiri's errors do not give.
- A missing input file is reported before anything is read, and `compare` exits with 2.

**Tests.** `tests/cli_tools.rs` runs the binary for A1–A10, including `rsparql` and
`rupdate` against a server on a port in 5380–5399. Unit tests cover the IRI and
language-tag rules, dot-segment removal, normalization, the term checker's merge, SSE
indentation and renaming, the group comparison and multiplicities, and result
conversions. The router test of `/$/validate/iri` checks the new warnings. The W3C SPARQL
suites are unchanged (482/328/157/269).

**Measurements.** On the 1.05M-triple benchmark N-Triples file, with a debug build:
`convert --count` takes 2.5 s on 16 threads, and `convert --output ttl` streams in 5.0 s
with 55 MiB peak memory, which does not grow with the input. Release builds were not
measured.

**Not built.** `schemagen`, a §1 non-goal, was not built. `rdfpatch`, the other §1
non-goal, came later with the patch reader of [F10](F10-replication.md#outcome), and it
prints the rows of RDF Patch files and counts them. Jena's `--rdfs` and `--formatted`
flags of `riot` and a configurable severity per rule were not built.

### Roadmap follow-ups (2026-10-12)

**Item 73 (Tier 5, [ROADMAP](../ROADMAP.md)): every syntax in `load_str`, `dump_graph` and
SPARQL `LOAD`, and Jena's syntaxes loaded without a temporary N-Quads copy.**

* **Where each syntax is handled today.** Jena's TriX, RDF Thrift, RDF Protobuf and
  RDF/JSON have a streaming reader each in `crates/sparkles-core/src/jena_formats.rs`
  (`ThriftReader::read_stream`, `ProtoReader::read_stream`, `read_rdf_json`, and
  `crate::trix::parse`), which call a quad sink. The only public entry point is
  `transcode_with_base`, which feeds that sink into an N-Quads or N-Triples serializer.
  Every loading path goes through it:
  * `Dataset::load_file`, `load_file_into` and `load_files` call
    `syntax::file_sources` (`crates/sparkles/src/syntax.rs`), which transcodes each such
    file into a `tempfile::NamedTempFile` and loads that as `RdfFormat::NQuads`.
  * `sparkles load` does the same in `jena_to_nquads` (`crates/sparkles-server/src/
    csv_cmd.rs`), with temporary files named `sparkles-load-*.nq`.
  * The server's uploads and Graph Store bodies go through `transcode_body`
    (`crates/sparkles-server/src/http.rs`), which writes the N-Quads in memory and spills
    to a `sparkles-transcode-` or `sparkles-body-` temporary file past `SPOOL_AFTER`.
  * The Python bindings transcode on a thread through a pipe for `parse`, and spool to a
    temporary file for `Dataset.load` (`crates/sparkles-py/src/io.rs`).
  * `sparkles convert` transcodes to N-Quads on a second thread as it streams, which is
    not a temporary file.
  The paths that take only the W3C syntaxes: `Dataset::load_str` and `load_str_into` take
  an `RdfFormat` and a `&str`, so a binary syntax cannot even be passed;
  `Dataset::dump_graph` takes an `RdfFormat`, while `Dataset::dump` takes
  `impl Into<RdfSyntax>` and writes Jena's syntaxes through `dump_jena`. SPARQL `LOAD`
  (`load` and `insert_parsed` in `crates/sparkles-core/src/sparql/update.rs`) has
  `LoadSyntax::Rdf` and `LoadSyntax::TriX` and nothing for the other three, and its
  `Accept` header names TriX but not them. The JVM binding's `load_files` and
  `load_bytes` (`crates/sparkles-ffi/src/lib.rs`) build sources with `Source::from_path`
  and `RdfFormat::from_media_type`, and the Node.js binding's `format`
  (`crates/sparkles-node/src/streams.rs`) returns an `RdfFormat`, so neither reads
  Jena's syntaxes.
* **A quad source for Jena's syntaxes.** `jena_formats` gains
  `pub fn read(fmt, input, base, sink: &mut dyn FnMut(Quad) -> Result<()>)`, the dispatch
  that `transcode_with_base` has inside it, and `transcode_with_base` becomes a caller of
  it. `Source::format` (`crates/sparkles-core/src/io.rs`) becomes an `RdfSyntax`, which
  moves from `crates/sparkles/src/syntax.rs` into the core crate beside `Source`, with
  `From<RdfFormat>` so existing callers compile unchanged. `parse_source` sends a Jena
  syntax to the streaming path it already has for non-splittable inputs: decompression by
  `source_reader`, then `read` into the sink that `make_sink` gives. Blank node labels are
  scoped to the source as they are for N-Quads, through the same label map.
* **Callers drop the temporary files.** `syntax::file_sources` returns plain sources and
  no `TempPath`s, `jena_to_nquads` is removed, `transcode_body` is replaced by a body
  source of the Jena syntax, and the Python `Dataset.load` passes the stream through.
  Parallel parsing of the N-Quads copy is the one thing the copy gave: the bulk loader
  splits N-Quads into chunks, while a Thrift or Protobuf stream has no record boundaries
  to split at. The decoders are cheap next to the loader's encoding and sorting, which
  stay parallel, so the direct path is expected to be faster; the change lands only if
  the measurement below shows it is not slower, and otherwise keeps the copy for files
  over a size threshold that the measurement sets.
* **The remaining APIs.** `Dataset::load_str` and `load_str_into` take
  `impl Into<RdfSyntax>`, and a new `load_bytes(data: &[u8], format, base, graph)` takes
  the binary syntaxes. `dump_graph` takes `impl Into<RdfSyntax>`; a quad syntax writes the
  graph's triples in the default graph, as `RdfWriter::triple` already does for
  Graph Store `GET`. SPARQL `LOAD` gains `LoadSyntax::Jena(JenaFormat)`, chosen by
  `JenaFormat::from_media_type` or the file extension as TriX is now, read through
  `jena_formats::read` into `insert`, with the decompressed-size accounting it already
  applies; its `Accept` header adds `application/rdf+thrift`, `application/rdf+protobuf`
  and `application/rdf+json` at `q=0.8`. The JVM and Node.js bindings parse format names
  into `RdfSyntax` and use `file_sources`, and P06's mapping test covers the new
  signatures.
* **Tests and measurement.** Round trips through `load_str`/`load_bytes` and
  `dump_graph` for each Jena syntax, over the TriX files of `testsuite/trix` and files
  of the other syntaxes written by `dump`; `LOAD <file:…>`
  and `LOAD <http…>` of each syntax with and without compression, and `LOAD SILENT` of a
  broken Thrift body leaving nothing behind; a check that no `sparkles-load-`,
  `sparkles-transcode-` or `sparkles-body-` file is created during `sparkles load`, an
  upload or a Graph Store `PUT` of each syntax. A load measurement of RDF Thrift and RDF
  Protobuf copies of the 10.5M-quad benchmark data (`scripts/gen-data.py`) compares the
  direct path with the copy for time and peak disk use, and its numbers go in the
  Outcome.

**Outcome of item 73.**

* **Delivered.**
  * `jena_formats::read` is the quad source of the four syntaxes. `read_into` wraps it for
    the loaders and keeps the sink's own errors, byte budgets and codec failures. A source's
    errors read `name: syntax: message`. RDF Thrift and RDF Protobuf messages end with how
    many statements were read before the fault, TriX messages give a byte offset, and
    RDF/JSON messages give a line and column.
  * `RdfSyntax` moved to `sparkles::io` beside `Source`, whose `format` field is an
    `RdfSyntax`. `RdfSyntax::from_name`, `from_extension` and `from_path` find a syntax by
    any of its names, so the bindings share one lookup.
  * `parse_source` sends a Jena syntax to one decoder on the calling thread. The decoder
    passes batches of 4096 quads through a bounded channel to as many sinks as the loader
    has threads, so the encoding and sorting stay parallel behind a decoder that cannot
    split its input. The transaction path reads the same decoder through
    `parse_source_into`.
  * `file_sources` returns plain sources, and `jena_to_nquads` and `transcode_body` are
    gone. `sparkles load`, uploads, Graph Store writes, `Dataset::load_file`, `load_files`,
    `load_file_into` and the Python `Dataset.load` of a path or bytes write no copy. A
    Python file object is still copied once to a temporary file before it loads, as for
    every syntax.
  * `Dataset::load_str`, `load_str_into` and `dump_graph` take `impl Into<RdfSyntax>`, and
    `Dataset::load_bytes(data, format, base, graph)` loads any syntax from memory.
    `dump_graph` writes the triples of a graph as default-graph triples in a quad syntax.
  * SPARQL `LOAD` reads all four syntaxes by media type or file name extension, and its
    `Accept` header names them at `q=0.8`.
  * The JVM binding's `load_bytes` takes any syntax name, media type or extension, and its
    `load_files` reads Jena's file extensions. The Node.js binding's `format` option takes
    every syntax in `load`, `dump`, `dumpToFile`, `parse` and `serialize`, and a dump to a
    file with a Jena extension picks that syntax.
* **Gaps found before the change.** `Dataset::load_str`, `load_str_into` and `dump_graph`
  took an `RdfFormat`. SPARQL `LOAD` read TriX only. The JVM `load_bytes` and the Node.js
  `format` took W3C syntaxes only, and Node's `parse`, `serialize` and `load` stream could
  not read or write a Jena syntax. `Dataset::load_file`, `load_files`, `load_file_into` and
  `sparkles load` wrote a temporary N-Quads file for each Jena file, the server's uploads and
  Graph Store bodies wrote one past a size, and the Python `Dataset.load` wrote one.
* **Deviations and decisions.**
  * The server's `--max-decompressed-mb` counts the bytes of a Thrift, Protobuf, RDF/JSON
    or TriX body as the reader takes them, as it counts those of every other syntax. It
    counted the size of the N-Quads rewrite before.
  * `Source::triples_only` keeps the refusal of a named graph in a TriX, RDF Thrift or RDF
    Protobuf body sent to one graph, which the rewrite as N-Triples gave. The Graph Store
    routes set it. The W3C quad syntaxes keep their graphs there, as before.
  * `Source::format` changed type, so code that reads it as an `RdfFormat` calls `rdf()`.
    `Source::from_bytes` takes `impl Into<RdfSyntax>`, so existing calls compile.
  * Only TriX uses the base IRI, since the other three syntaxes hold absolute IRIs.
  * The Python `parse` keeps its thread and pipe. It streams quads and writes no file.
* **Tests.** `crates/sparkles-core/tests/jena_load.rs` loads each syntax as a source
  through the bulk and the transaction loaders with and without gzip. It checks the graph
  mapping, the blank node scope of each source, byte budgets, `LOAD` of files and HTTP
  bodies with and without compression, `LOAD SILENT` of a broken body, and malformed input
  of each syntax. `crates/sparkles/tests/format_coverage.rs` round-trips every syntax
  through `load_bytes`, `load_str`, `load_str_into`, files, `dump`, `dump_graph` and `LOAD`,
  and runs Jena's TriX files against their N-Quads. `no_temp_files.rs` makes the
  temporary directory unusable and loads each syntax through every library entry point.
  `crates/sparkles-server/tests/cli_jena_load.rs` does the same for `sparkles load`. The
  JVM, Node.js and Python suites cover the bindings.
* **Measurements.** On a shared 16-core laptop, the command `sparkles load` takes 11 to
  22 percent less wall time on 1.05M-triple files and 26 to 28 percent less on 5.26M-triple
  files, and it writes no temporary file where it wrote 105 MiB and 533 MiB. Peak RSS
  falls from about 1.17 GiB to 0.9 GiB on the larger files. The first build, in which the
  decoder also encoded the quads, measured level with the old loader, and the batches to
  parallel sinks made the difference. The full table and the method are in the
  [loading results](../benchmarks/loading/RESULTS.md#jenas-syntaxes). These are local
  numbers and not forge numbers.
