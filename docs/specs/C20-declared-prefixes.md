# C20: Declared prefixes

> **Status:** implemented
>
> **Phases:** one phase. It covers the `prefixes` settings kind with its layers, locks
> and removals, the Fuseki-style prefixes routes on the runtime layer, warnings for
> prefixes that shadow well-known ones, the Prefixes section of the UI's Settings tab,
> completion of prefixes in the query editor and in `sparkles lsp`, and the NixOS
> example and test.
>
> **User docs:** [API: Prefixes](../API.md#prefixes) ·
> [API: Settings](../API.md#settings) ·
> [Usage: Declared prefixes](../USAGE.md#declared-prefixes) ·
> [Editor integration](../editors.md) ·
> [Features](../FEATURES.md)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This design was written from the SPARQL 1.1 Query grammar (`PN_PREFIX` and `PNAME_NS`),
RDF 1.1 Turtle, RFC 7396 (JSON Merge Patch), the Language Server Protocol 3.17 and the
public documentation of Fuseki's prefixes service, together with the Sparkles code. It
builds on layered settings ([C19](C19-layered-settings.md)), the linter
([X04](X04-linter.md)) and the formatter ([X02](X02-formatter.md)).

## 1. Summary

A dataset's prefixes are metadata kept next to its data. Loaded data adds its prefixes,
and `/{ds}/prefixes` reads and changes them as Fuseki's prefixes service does. An
operator who runs Sparkles with Nix has no way to declare them, so every server starts
with the prefixes its datasets happened to collect. The query editor and `sparkles lsp`
complete only the prefixes they already know, and the language server knows none.

This spec makes prefixes a settings kind of C19. The operator declares prefixes for
every dataset or for one dataset in the settings file, as in
`defaults.prefixes = { kclj = "https://kclj.io/sparkles/"; }` in the NixOS module. The
prefixes a dataset stores today become its runtime layer, so the API, the CLI and the UI
change them as they change other settings, and the operator can lock them. The query
editor and the language server complete the effective prefixes and add the `PREFIX`
line that a completed name needs.

## 2. Goals and non-goals

Goals:

- Operators declare prefixes in the settings file, for every dataset and per dataset,
  and may lock them.
- Users with `admin` on a dataset add, change and remove prefixes through the settings
  routes, `sparkles settings` and the UI, and the change persists. Removing a declared
  prefix persists too, and a reset brings the declared value back.
- `/{ds}/prefixes` keeps its Fuseki behaviour.
- Prefixes of loaded data never replace a declared or locked binding.
- The UI's query editor and `sparkles lsp` complete the effective prefixes and insert
  the declarations they need.

Non-goals:

- Prefixes in query text. A query still declares its own prefixes, as SPARQL requires,
  and the server does not add the dataset's prefixes to a query.
- Prefixes per named graph or per branch. Prefixes belong to the dataset, as other
  settings do.

## 3. Layers

The effective prefixes of a dataset are computed from four layers. A later layer
overrides an earlier one.

1. The well-known prefixes, which are the built-in defaults of the kind (§3.1).
2. The declared `defaults.prefixes` of the settings file.
3. The declared `datasets.<name>.prefixes` of the settings file.
4. The runtime layer, which holds the prefixes the dataset stores.

The layers merge as C19 §4 says. The kind is a map from a prefix name to an IRI, and
each prefix name is a field. A runtime value `null` for a name removes that name, which
is how a declared or well-known prefix is removed at runtime. The removal persists across
restarts and reloads, and a reset of the field removes the `null`, so the declared or
well-known value applies again. This follows the providers of C19 §11.1, whose `null`
removes a declared provider.

### 3.1 Well-known prefixes

The well-known prefixes are those that `/$/prefixes/{ds}` adds today, which are Jena's
standard prefixes and a few common vocabularies, and the namespaces that Sparkles
defines itself.

| Prefix | IRI |
|---|---|
| `rdf` | `http://www.w3.org/1999/02/22-rdf-syntax-ns#` |
| `rdfs` | `http://www.w3.org/2000/01/rdf-schema#` |
| `owl` | `http://www.w3.org/2002/07/owl#` |
| `xsd` | `http://www.w3.org/2001/XMLSchema#` |
| `dc` | `http://purl.org/dc/elements/1.1/` |
| `dcterms` | `http://purl.org/dc/terms/` |
| `foaf` | `http://xmlns.com/foaf/0.1/` |
| `skos` | `http://www.w3.org/2004/02/skos/core#` |
| `schema` | `http://schema.org/` |
| `sh` | `http://www.w3.org/ns/shacl#` |
| `prov` | `http://www.w3.org/ns/prov#` |
| `spk` | `urn:x-sparkles:` |
| `hist` | `urn:x-sparkles:history#` |
| `path` | `urn:x-sparkles:path#` |
| `mem` | `urn:x-sparkles:mem:` |
| `sparkles` | `urn:x-sparkles:assembler#` |
| `text` | `http://jena.apache.org/text#` |

### 3.2 Storage

The runtime layer is stored by the dataset's store, as the prefixes are today. Its
bindings stay in `prefixes.json`, which keeps its form of an object of strings, so an
older version of Sparkles reads the same bindings. The removals are kept in a new file,
`prefixes-removed.json`, as a JSON array of names. An older version ignores that file,
and it knows no declared prefixes for a removal to hide. An in-memory dataset keeps both
in the process. A backup holds `prefixes-removed.json` when it exists, as it holds the
other metadata files. A clone or a branch does not copy removals, since settings belong
to the dataset's name and a branch reads them from its dataset.

A binding and a removal of the same name never coexist. Storing a binding clears the
removal of its name, and storing a removal clears the binding.

### 3.3 Locks

A lock names `prefixes.NAME` for one prefix or `prefixes` for the whole kind. A locked
prefix takes its value from the declared and well-known layers. A runtime value for it is
kept and ignored, and it is listed as `overridden`, as C19 §4.1 says. A lock on a name
that no layer declares keeps that name unbound, so it cannot be added at runtime. A lock
on the whole kind fixes every prefix, and the runtime layer cannot add one.

### 3.4 Prefixes of loaded data

Loaded data adds the prefixes it declares, keeping the names that are already bound, as
it does today. It also skips the names that the settings file declares or locks for the
dataset and the names that the runtime layer removes. A name declared in the settings
file is therefore never replaced by data, even when a later reload declares it. The
prefix rows of an RDF Patch (`PA`) count as loaded data, and `PD` removes only a runtime
binding.

## 4. The settings kind

`prefixes` is a dataset-wide kind of C19 §3. The routes, the answer and the CLI of C19
work for it unchanged.

- `GET /$/settings/{ds}/prefixes` answers the effective map, the declared and runtime
  layers, a source for each name (`default` for a well-known prefix, `declared`,
  `runtime` or `locked`), `locked`, `overridden`, `overrides`, `status` and the `ETag`.
- `PATCH` merges into the runtime layer. A string binds a name. `null` removes the
  runtime value of a name, and for a name that a declared or well-known layer binds it
  stores the removal.
- `PUT` makes the effective map equal to the body. A name that the body leaves out is
  removed, which for a declared or well-known name stores a removal.
- `DELETE ?field=NAME` resets one name, and `DELETE` resets the whole runtime layer.

A name is a SPARQL `PN_PREFIX`, in the ASCII subset that `/{ds}/prefixes` accepts today,
or the empty name. A name has at most 256 bytes and an IRI at most 4096, and the IRI must
be absolute. A write that breaks a rule is a `400`, and so is a write that would give the
runtime layer more than `--max-prefixes` entries, bindings and removals together. A
settings file that breaks a rule fails `sparkles settings check` and the start, as other
invalid settings do. A settings file that declares more than `--max-prefixes` prefixes
for one dataset is refused in the same way, and `sparkles settings check` takes
`--max-prefixes` with the server's default.

### 4.1 Shadowing

A declared or runtime prefix whose name is a well-known one but whose IRI differs shadows
it. A query that relies on the well-known meaning of `mem:` then reads differently. The
settings allow it, since a dataset may have its own meaning for a short name, but they
report it.

- `sparkles settings check` prints a warning for each shadowing prefix of the file.
- The answer of the kind has a `warnings` member, a list of `{prefix, iri, wellKnown,
  message}`.
- The UI shows the warning next to the prefix.

## 5. HTTP

`GET /$/prefixes/{ds}` answers the effective prefixes, with the well-known ones as
before. The answer keeps its shape `{prefixes}`.

`/{ds}/prefixes` keeps the behaviour of docs/API.md on the runtime layer.

| Method | Behaviour |
|---|---|
| `GET` | With neither parameter, the stored bindings, as today. `?prefix=p` and `?uri=u` look in the stored bindings too. The effective prefixes are at `/$/prefixes/{ds}` and in the settings answer. |
| `POST`, `PUT` | Binds `prefix` to `uri` in the runtime layer and clears a removal of the name. A locked name is a `409` with `locked-by-config`, unless the binding restates the locked value, which stores nothing. |
| `DELETE ?prefix=p` | Removes the runtime binding. A name that the settings file declares also gets a removal, so the declared value stops applying. A locked name is a `409` with `locked-by-config`. A name that is neither stored nor declared, or is already removed, is a `404`. |

Keeping `GET` on the stored bindings keeps the answers that Fuseki clients get today.
A client that wants the prefixes in force reads `/$/prefixes/{ds}`.

The prefixes used to write results, Graph Store answers and SHACL targets are the
effective ones without the well-known layer, which are the declared and runtime
bindings. Before this change they were the stored ones. Well-known prefixes stay out of
these, so an answer does not grow seventeen `@prefix` lines.

## 6. Command line

`sparkles settings` handles the kind as any other.

```
sparkles settings set slurp prefixes.kclj=https://kclj.io/sparkles/
sparkles settings set slurp prefixes.notes=null
sparkles settings reset slurp prefixes.notes
sparkles settings diff slurp
sparkles settings check settings.json --max-prefixes 1000
```

`set NAME=null` removes the prefix, and `reset` brings back the declared value.

## 7. UI

The dataset's Settings tab gets a Prefixes section. It lists the effective prefixes in a
table of name, IRI and source, where the source is `well-known`, `server config`,
`changed` or `locked`, with the labels of C19 §10. A user with `admin` adds a prefix,
edits an IRI and deletes a prefix. A runtime value over a declared one shows the declared
value and offers Use server config. A declared or well-known prefix removed at runtime is
listed as removed with Use server config, so it can be brought back. A shadowing prefix
shows the warning of §4.1. The section uses the layout of the other settings sections.

The query editor completes the effective prefixes, which it reads from
`/$/prefixes/{ds}`. Accepting a prefix or a prefixed name whose prefix the query does not
declare inserts `PREFIX name: <iri>` after the query's last `PREFIX` or `BASE` line, or
at the top.

## 8. Language server

`sparkles lsp` reads the prefixes it knows from three sources, a later one overriding an
earlier one for the same name.

1. The well-known prefixes of §3.1.
2. A server's dataset, when the config file names one (§8.1).
3. The `[prefixes]` table of the `.sparklesfmt.toml` that `sparkles fmt` and
   `sparkles lint` read for the document.

```toml
[prefixes]
kclj = "https://kclj.io/sparkles/"
memory = "https://kclj.io/sparkles/memory/"

[lsp]
server = "https://sparkles.example.org"
dataset = "slurp"
```

The server announces `completionProvider`. In a SPARQL, Turtle or TriG document it
completes a prefix declaration after `PREFIX` or `@prefix`, and a prefix name where a
prefixed name starts. Completing a name whose prefix the document does not declare also
inserts the declaration, after the last declaration at the top of the document or at
its start, in the document's style: `PREFIX name: <iri>` in SPARQL and
`@prefix name: <iri> .` in a Turtle document that uses `@prefix`. The lint finding
`undefined-prefix` gets a quick fix that adds the declaration when the prefix is known.

### 8.1 Prefixes from a server

The `[lsp]` table's `server` and `dataset` name a dataset whose effective prefixes the
language server reads from `GET /$/prefixes/{ds}`. The token comes from `SPARKLES_TOKEN`
or the credentials that `sparkles auth login` saved for that server. The language server
reads the prefixes in the background the first time a document uses the config file and
keeps them for the session. A failed read is logged to standard error and leaves the
other sources in place, and editing never waits for it. Without the `[lsp]` table the
language server never contacts a server. A build without the `auth` feature ignores the
table and logs that it does.

## 9. NixOS module

The module needs no new option, since `services.sparkles.settings` is free-form JSON
that `sparkles settings check` validates at build time. The description of the option and
the NixOS section of docs/USAGE.md show `defaults.prefixes`. The VM test declares a prefix
and a locked prefix, checks that the declared one appears in `/$/prefixes/{ds}`, and
checks that a runtime write to the locked one is refused with `409`.

## 10. Rejected alternatives

- **Declared prefixes in their own option.** A `services.sparkles.prefixes` option would
  duplicate the settings file and its locks, reload and check.
- **Removals as `null` in `prefixes.json`.** An older version reads that file as an
  object of strings and would drop every runtime prefix after a downgrade.
- **Effective prefixes at `GET /{ds}/prefixes`.** Fuseki clients read that route as the
  stored prefixes, and the answer would gain the well-known ones.
- **Refusing a declared prefix that shadows a well-known one.** A dataset may use a
  short name for its own vocabulary, and a warning is enough to catch a mistake.
- **Adding the dataset's prefixes to queries.** SPARQL requires a query to declare its
  prefixes, and a query that relied on the dataset's would change meaning when they
  change. The editors insert the declarations instead.
- **The language server always asking a server.** Editing must work offline and must not
  send a token anywhere the user did not configure.

## 11. Acceptance examples

- **A1.** The settings file declares `defaults.prefixes.kclj`. A new dataset created
  through the API answers it in `/$/prefixes/{ds}` and in the settings answer with the
  source `declared`, without a restart.
- **A2.** `PATCH /$/settings/{ds}/prefixes` with `{"kclj": null}` removes the declared
  prefix. After a restart it is still removed. `DELETE ?field=kclj` brings it back.
- **A3.** With `prefixes.kclj` locked, `POST /{ds}/prefixes` that binds `kclj` to another
  IRI and `DELETE /{ds}/prefixes?prefix=kclj` are `409` with `locked-by-config`, and so
  is a settings `PATCH` of `kclj`.
- **A4.** Data that declares `@prefix kclj: <http://other/>` is loaded into a dataset
  whose settings file declares `kclj`. The effective `kclj` keeps the declared IRI, and
  the stored prefixes do not gain `kclj`.
- **A5.** `GET /{ds}/prefixes` without parameters answers only the stored prefixes.
- **A6.** A settings file that declares `mem` as `https://example.org/mem#` passes
  `sparkles settings check` with a warning, and the settings answer lists it in
  `warnings`. A file that declares `bad name` or a relative IRI fails the check.
- **A7.** `sparkles settings set slurp prefixes.kclj=https://kclj.io/x/` stores a runtime
  value, `diff` lists it over the declared value, and `reset` removes it.
- **A8.** In the query editor, completing `kclj:` in a query without its declaration
  inserts `PREFIX kclj: <https://kclj.io/sparkles/>` at the top.
- **A9.** `sparkles lsp` with a `[prefixes]` table completes `kclj:` and inserts the
  declaration, and offers a quick fix for `undefined-prefix` on `kclj:x`.
- **A10.** The NixOS VM test reads a declared prefix from `/$/prefixes/{ds}` and gets a
  `409` for a write to a locked one.

## 12. Sources

- SPARQL 1.1 Query Language, section 19.8, for `PN_PREFIX` and `PNAME_NS`.
- RDF 1.1 Turtle, for `@prefix` and `PREFIX` in Turtle documents.
- RFC 7396, JSON Merge Patch, for the merge rule and `null`.
- The Language Server Protocol 3.17, for completion items with `additionalTextEdits` and
  code actions.
- Fuseki's public documentation of its prefixes service.
- The Sparkles code and specs C19, X02 and X04.

## Outcome

The whole design landed on 2026-10-10 in one phase. The `prefixes` kind joins the
registry of C19 next to `assistant`, `memory` and `ingest`. Its built-in defaults are
the seventeen prefixes of §3.1, its declared layers come from the settings file, and its
runtime layer is the store's bindings with a `null` for each removed name. The settings
routes, `sparkles settings`, the UI's Settings tab, the NixOS module and its tests, and
`sparkles lsp` work as §4 to §9 describe. The tests cover A1 to A10 across the store's
tests, the server's unit tests, a test of `sparkles settings` against a server process
with a restart, the UI's end-to-end tests, the tests of `sparkles lsp`, and the NixOS
build and VM tests.

These points differ from the design or settle what it left open.

- The answer of the kind lists a lock of the whole kind as the empty path `""` in
  `locked`, since its path has no members.
- A `DELETE /{ds}/prefixes?prefix=p` for a well-known name that the dataset does not
  store is a `404`, as before. A well-known prefix is removed through the settings
  routes, `sparkles settings` or the UI.
- The `PA` rows of an RDF Patch skip the declared, locked and removed names, as loaded
  data does, and `PD` removes only a runtime binding.
- `--max-prefixes` counts the effective prefixes that differ from the well-known ones,
  and separately the runtime bindings and removals together. The global flag sets the
  limit for `sparkles settings check` as well.
- A clone or a branch does not copy `prefixes-removed.json`, as §3.2 says, and a backup
  holds it.
- The query editor needed no change. It already completed the prefixes of
  `/$/prefixes/{ds}` and inserted their `PREFIX` lines, and that route now answers the
  effective prefixes.
- The schema summary, the MCP prefixes resource and tools, the assistant and the SHACL
  shape drafts still read the stored prefixes with the well-known ones, and not the
  declared layers. Query results, Graph Store answers and SHACL report targets use the
  declared and runtime prefixes as §5 says.
- `sparkles lsp` reads the server of the `[lsp]` table when the first document that uses
  the config file opens, not at process start, since the config file is found per
  document. The read happens once per server and dataset for the session.
- A `[prefixes]` or `[lsp]` table that does not validate is a config error of
  `sparkles fmt` too, as other mistakes in the file are, and `sparkles lint` ignores the
  tables.
- The quick fix that declares a known prefix is not part of `source.fixAll.sparkles`,
  because its IRI comes from the configuration and not from the document.
- Plain `http` to a host other than loopback is refused for the `[lsp]` server, and the
  language server never falls back to `SPARKLES_SERVER` or the default server of the
  credentials file.

