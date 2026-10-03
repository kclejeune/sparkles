# G05: Command-line equivalents of Jena's tools, and IRI and language-tag checks

> **Status:** implemented
>
> **Phases:** One phase. Every command of §3 shipped, with the checks of §4 in `convert`,
> `load` and `/$/validate/iri`.
>
> **User docs:** [Usage: File tools](../USAGE.md#file-tools) ·
> [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui) ·
> [Comparison with Jena](../COMPARISON.md)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This spec draws on the Sparkles code, the command sources of Apache Jena (`jena-cmds`,
`jena-langtag` and `jena-iri3986`, Apache-2.0) for behaviour and flags, and the
standards listed in §9. Fluree was not consulted.

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
(`http/jena_formats.rs`), and TriX's live in the core crate (`sparkles::trix`).

**Deviations and decisions.**
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

**Not built.** `rdfpatch` and `schemagen` (§1 non-goals). `rdfpatch` came later with
the patch reader of [F10](F10-replication.md#outcome). Jena's `--rdfs` and
`--formatted` flags of `riot`. A configurable severity per rule.
