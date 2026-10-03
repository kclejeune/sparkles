# X03: OpenAPI description, shell completions and man pages

> **Status:** implemented in part
>
> **Phases:** Phase 1 shipped. It covers the OpenAPI 3.1 description at
> `GET /$/openapi.json` and `GET /$/openapi.yaml`, its checked-in copy, the test that
> keeps it in step with the route table, `sparkles openapi`, shell completions, man pages
> and their installation by the Nix package. Phase 2, which describes the remaining admin
> types field by field, was not built.
>
> **User docs:** [API: OpenAPI description](../API.md#openapi-description) ·
> [Usage: Shell completions and man pages](../USAGE.md#shell-completions-and-man-pages) ·
> [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This is internal engineering, not derived from any other database product. The sources
are the OpenAPI Specification 3.1.1, JSON Schema 2020-12, YAML 1.2, the SPARQL 1.1
Protocol and Graph Store Protocol, RFC 9110, RFC 6265, RFC 9512, the documentation of
`clap_complete` and `clap_mangen`, the nixpkgs manual's section on `installShellFiles`,
and the Sparkles code and API reference (§10). Fluree was not consulted.

## 1. Summary, goals, non-goals

The HTTP API has grown to about 170 operations over 120 route templates. The only
description of it is the prose of `docs/API.md`, so a client in another language has to be
written by reading that file, and nothing tells a client author when a route changes.
The command line has the same problem on a smaller scale. `sparkles` has around 60
subcommands and several hundred flags, and shells cannot complete any of them.

This spec adds three things:
- an OpenAPI 3.1 description of the HTTP API, served by the server and checked into the
  repository, that a test keeps in step with the route table;
- `sparkles completions <shell>` for bash, zsh, fish, elvish and PowerShell;
- `sparkles man`, which writes a man page for the command and each subcommand.

The Nix package installs the completions and the man pages.

**Goals**

1. **Complete paths.** Every route template of `auth::ROUTES` that serves the API is a
   path of the description, with exactly the methods the route table lists. A test fails
   when a route is missing from the description or the description has a path the server
   does not serve.
2. **Permissions from the code.** Each operation's security requirements and its
   `x-sparkles-permission` come from `auth::need`, the function the authorization layer
   uses. A change to a route's permission changes the description without anyone
   editing it.
3. **Usable by generators.** Every operation has a unique, stable `operationId` and one
   tag. Schemas are named components and are referenced, not repeated inline. Request and
   response media types are listed per operation. The P02 Rust client is meant to be
   generated against this document or written from it, so the document should not
   depend on features that common generators skip.
4. **Main types described.** The common error body, the SPARQL results, dataset and server
   information, tasks, commits, readiness, whoami, tokens, schema pages and the formatter's
   request and result are described field by field. Other admin bodies are named schemas
   that allow any members and link to their section of `docs/API.md`.
5. **Cheap.** No macro on every handler, and no change to the handlers. The description
   adds little to compile time and binary size.
6. **Completions and man pages from the clap definition**, so they never drift from the
   flags.

**Non-goals**

- A bundled Swagger UI or Redoc. Their bundles are several hundred KiB of JavaScript and
  CSS, and the UI already has a query editor. The description is linked from the UI, and
  any OpenAPI viewer can open it.
- Describing the bodies of the MCP endpoint. `/$/mcp` speaks JSON-RPC, whose methods the
  MCP specification describes. The description lists the route and its transport.
- Validating requests against the description at run time.
- Describing every member of every admin type in Phase 1.
- Nushell completions, which need a separate crate (`clap_complete_nushell`).

## 2. Approach

### 2.1 Alternatives

| Approach | For | Against |
|---|---|---|
| `utoipa` attributes on every handler, with `ToSchema` derives | The description sits next to the code. | About 170 handlers in 20 files gain attributes, and most bodies are built with `json!`, so they have no Rust type to derive from. Agents adding routes in parallel would conflict in every file. The proc macros add compile time. |
| `schemars` derives for the bodies | The schemas come from the types. | The same lack of types. `schemars` is already in the tree through `rmcp`, but it would only cover the handful of bodies that are typed structs. |
| A hand-written YAML file | Readable, and the usual format. | Nothing ties it to the code. Security and the common errors would be repeated on every operation, and a YAML parser would be needed to serve JSON. |
| **A Rust module that builds the document as a `serde_json::Value`** | Helper functions remove the repetition. Security and errors come from `auth::need`. A test compares it with `auth::ROUTES`. A checked-in copy shows API changes in review. | The schemas are written by hand. |

The design takes the last row. The description lives in
`crates/sparkles-server/src/openapi/`. It is built once, on the first request, and kept
as JSON and YAML text.

### 2.2 The checked-in copy

`docs/openapi.json` is the document as the server serves it, with the version set to
`0.0.0`. A test builds the document and compares it with the file. When they differ, the
test fails and prints the command that rewrites the file:
`SPARKLES_UPDATE_OPENAPI=1 cargo test -p sparkles-server openapi` (also `mise run
openapi`). A pull request that changes the API therefore shows the change in the
description's diff. A client generator can read the file without starting a server. The
file's object keys are sorted, so the output does not depend on whether `serde_json`
preserves insertion order in a given build.

### 2.3 YAML

JSON is valid YAML 1.2, but a YAML file that is JSON is not what a reader expects. A small
emitter writes block mappings and sequences. A scalar is written plain when it cannot be
read as anything but a string, and otherwise double-quoted with JSON escapes, which YAML
reads the same way. Text with line breaks is a literal block (`|-`) when that is exact.
The emitter needs no dependency.

## 3. The document

### 3.1 Top level

- `openapi: 3.1.0`, and `jsonSchemaDialect` left at its default.
- `info` has the title, the server's version, the Apache-2.0 license, and a description
  of authentication, errors, pagination and conditional requests.
- `servers` is `[{url: "/"}]`, which resolves against the document's own URL. A client
  that read the document from a server talks to that server.
- `tags` groups the operations as `docs/API.md` does: Server, Datasets, SPARQL, Graph
  Store, Schema, Stored queries, Commits and history, Search, Reasoning, Validation,
  Tasks, Backups, Backup repositories, Backup policies, Formatting, Authentication,
  Tokens, Fuseki and MCP. Each tag links to its section of `docs/API.md`.
- `externalDocs` links `docs/API.md`.

### 3.2 Paths

Each template of `auth::ROUTES` is a path, except the web UI's (`/`, `/ui`, `/ui/` and
`/ui/{*path}`), which serve HTML and assets. OpenAPI has no wildcard parameter, so
`/{ds}/{*graph}` becomes `/{ds}/{graph}`, and its description says the graph path may hold
slashes. A path parameter is declared once as a component (`ds`, `name`, `id`, `repo`,
`backup`, `policy`, `reference`, `user_code`, `graph`) and referenced by every operation
on a path with it.

A route the table lists with explicit methods has exactly those operations. A route
listed with `*` dispatches on the request, and the description gives the methods a client
uses:

| Route | Operations |
|---|---|
| `/{ds}` | GET, HEAD, POST, PUT, DELETE (query, update or Graph Store by the request) |
| `/{ds}/sparql`, `/{ds}/query` | GET, POST |
| `/{ds}/data`, `/{ds}/{graph}` | GET, HEAD, PUT, POST, DELETE |
| `/{ds}/prefixes` | GET, POST, PUT, DELETE |
| `/$/mcp` | POST, GET, DELETE |

`operationId`s are lower camel case and name the action: `listDatasets`, `createDataset`,
`getDataset`, `sparqlQueryPost`, `gspPut`, `createToken`. The two query endpoints and the
dataset root get distinct ids, because a generator makes one method per id.

### 3.3 Security

The security schemes are:

| Scheme | Type | Used for |
|---|---|---|
| `basicAuth` | `http`, `basic` | A configured user and password, or an API token as the password. |
| `apiToken` | `http`, `bearer`, format `spk_…` | API tokens minted at `/$/auth/tokens` or configured as `[[tokens]]`. |
| `oidcAccessToken` | `http`, `bearer`, format `JWT` | Access tokens of the OIDC provider, with `oidc.api_audience` set. |
| `sessionCookie` | `apiKey` in the cookie `sparkles_session` | The web UI's session. Over https the cookie is `__Host-sparkles_session`. |
| `csrfToken` | `apiKey` in the header `X-Sparkles-CSRF` | The `csrfToken` of `/$/whoami`, sent by session and proxy principals on unsafe requests. |
| `cloudflareAccess` | `apiKey` in the header `Cf-Access-Jwt-Assertion` | The assertion Cloudflare Access adds, with `[cloudflare_access]`. |

Trusted proxy headers are not credentials a client sends, so they are described in text
only.

Each operation's `security` follows from `auth::need` for its route and method:

| Need | `security` |
|---|---|
| `Public`, `Caller` | `{}` first, then every scheme. Anonymous callers are admitted. |
| `Authed`, `Dataset(level)`, `Server(perm)` | Every scheme. |
| `Interactive` | The session cookie with the CSRF token, and Cloudflare Access. |

On an unsafe method, the session cookie appears together with `csrfToken` in one
requirement, because both are needed. `x-sparkles-permission` records the need as `public`,
`any caller`, `signed in`, `web session`, `read`, `write`, `admin`, `metrics`,
`federate` or `server-admin`. For `/{ds}` the permission depends on the request, and the
description says so. The description notes that a server without `--auth-config` needs
no credentials at all.

### 3.4 Errors

`components.schemas.Error` is the common error body of `docs/API.md`'s Errors section:
`error` (required), and optionally `detail`, `line`, `column`, `code` and `requestId`.
Other members are allowed, because some errors add their own, such as `budget`, `limit`
and `requested` of a budget error (`BudgetError`) or the readable ranges of
`history-gone`. Named responses cover the statuses: `BadRequest`, `Unauthorized` (with
`WWW-Authenticate`), `Forbidden`, `NotFound`, `Conflict`, `PreconditionFailed`,
`PayloadTooLarge`, `UnsupportedMediaType`, `TooManyRequests` (with `Retry-After`),
`Timeout`, `Unavailable` (with `Retry-After`), `InsufficientStorage` (`BudgetError`) and
`default`.

Every operation gets `default`. One that needs more than `Public` gets `401` and `403`,
and one that needs a level on `{ds}` gets `404`, since a hidden dataset answers like a
missing one. Each operation lists the other statuses its section of `docs/API.md` names.

### 3.5 Pagination

Three styles exist, and each is described where it is used and named by an
`x-sparkles-pagination` extension so a generator can wrap it:

| Style | Routes | Request | Response |
|---|---|---|---|
| `cursor` | `/$/schema/{ds}/classes`, `/$/schema/{ds}/predicates` | `limit`, `cursor` | `{items, total, next}` with `next` an opaque cursor or `null` |
| `seq` | `/$/commits/{ds}`, `/{ds}/changes` | `limit`, `before` or `after` | `next`, a URL or the commit to ask after |
| `time` | `/$/repositories/{repo}/backups` | `limit`, `before` | `next`, a time or `null` |

### 3.6 Media types

The SPARQL operations list the request forms of the SPARQL 1.1 Protocol
(`application/sparql-query`, `application/sparql-update` and
`application/x-www-form-urlencoded`) and every result and RDF media type the server
negotiates. `application/sparql-results+json` and `application/x-sparkles+json` have
schemas. The others are strings, or binary for the Thrift and Protobuf formats. The Graph
Store operations list the RDF syntaxes for bodies and responses, and the conditional
headers (`If-Match`, `If-None-Match`, `ETag`). Write operations list `dryRun`,
`Sparkles-Dry-Run`, `Sparkles-Commit-Message` and `receipt`, and the commit headers of
responses (`Sparkles-Commit`, `Sparkles-Dataset-Id`).

### 3.7 Serving

`GET /$/openapi.json` answers `application/json`, and `GET /$/openapi.yaml` answers
`application/yaml` (RFC 9512). Both are public, like `/$/ping`. The document holds no data
and no configuration, only what the binary can do. Responses carry a strong `ETag` and
`Cache-Control: no-cache`, and answer `If-None-Match` with `304`. The route table gains
both routes, and the description lists them.

## 4. Keeping it correct

A test in `openapi/tests.rs`:
1. collects `(path, method)` from the document and from `auth::ROUTES`, with the UI
   routes left out and `{*graph}` written `{graph}`;
2. fails on a template missing from the document, on a document path missing from the
   table, and on a method that differs for a route with explicit methods;
3. checks every `$ref` resolves, every `operationId` is unique, every operation has a
   tag that `tags` defines, every `{param}` of a path is declared, and every operation has
   at least one response;
4. compares the document with `docs/openapi.json`, as §2.2 describes.

`router_tests::route_coverage` already ties the router to `auth::ROUTES`. Together, a
route added to the router without the description fails two tests, each with a message
that names the template.

## 5. `sparkles openapi`

`sparkles openapi [--format json|yaml]` prints the document without starting a server.
P02's generator and scripts can use it, and it needs no network.

## 6. Shell completions and man pages

`sparkles completions <SHELL>` prints a completion script for `bash`, `zsh`, `fish`,
`elvish` or `powershell` on stdout, generated by `clap_complete` from the CLI definition.
The usual setup lines are in USAGE.md:

```sh
sparkles completions bash > ~/.local/share/bash-completion/completions/sparkles
sparkles completions zsh > "${fpath[1]}/_sparkles"
sparkles completions fish > ~/.config/fish/completions/sparkles.fish
```

`sparkles man [--dir DIR]` writes roff man pages with `clap_mangen`: `sparkles.1`, and
`sparkles-<sub>.1` for each subcommand, recursively, such as `sparkles-auth-login.1`.
Without `--dir` it prints `sparkles.1` on stdout. Hidden subcommands get no page.

The scripts complete subcommands and flags, and values of `ValueEnum` arguments such as
`--log-format`. Paths complete as paths where the argument is a `PathBuf`. Dynamic
completion of dataset names would need the server, and is out of scope.

## 7. Nix

`nix/package.nix` adds `installShellFiles` to `nativeBuildInputs`. When the build can run
the binary it builds (`stdenv.buildPlatform.canExecute stdenv.hostPlatform`), `postInstall`
runs it:

```nix
installShellCompletion --cmd sparkles \
  --bash <($out/bin/sparkles completions bash) \
  --zsh <($out/bin/sparkles completions zsh) \
  --fish <($out/bin/sparkles completions fish)
$out/bin/sparkles man --dir man
installManPage man/*.1
```

This puts the files in `share/bash-completion/completions`, `share/zsh/site-functions`,
`share/fish/vendor_completions.d` and `share/man/man1`. The OpenAPI document is also
installed as `share/doc/sparkles/openapi.json`.

## 8. Phasing

**Phase 1.** Everything above. The admin types other than those of goal 4 are open
objects with a description and a link.

**Phase 2.** Field-by-field schemas for the remaining admin types: backups, repositories,
policies, the text, vector and spatial index status, reasoning status, write-time
validation, history and snapshots, compaction and stats. A schema test that sends
requests to a test server and validates each response against the description's schema
would keep them honest.

## 9. Acceptance examples

- **A1.** `curl localhost:3030/$/openapi.json` answers `200` with `openapi: 3.1.0` and a
  path for every API template of `auth::ROUTES`. With `If-None-Match` set to the `ETag`,
  it answers `304`.
- **A2.** With `--auth-config`, an anonymous `GET /$/openapi.yaml` answers `200`, and the
  YAML reads back to the same document as the JSON.
- **A3.** A route added to `auth::ROUTES` and the router but not to the description fails
  the drift test with `route /$/new-thing is not in the OpenAPI description`.
- **A4.** `GET /$/metrics` in the description has `x-sparkles-permission: metrics`, and
  `POST /{ds}/update` has `write` and a `csrfToken` in its session requirement.
- **A5.** `sparkles completions zsh` prints a script that starts with `#compdef sparkles`.
  `sparkles completions nushell` is a usage error.
- **A6.** `sparkles man --dir out` writes `out/sparkles.1` and `out/sparkles-serve.1`, and
  `man -l out/sparkles.1` shows the subcommands.
- **A7.** The Nix package has `share/zsh/site-functions/_sparkles`,
  `share/bash-completion/completions/sparkles.bash`,
  `share/fish/vendor_completions.d/sparkles.fish` and `share/man/man1/sparkles.1.gz`.
- **A8.** `docs/openapi.json` equals the built document. Changing a summary without
  regenerating the file fails the test.

## 10. Sources

- OpenAPI Specification 3.1.1 (the OpenAPI Initiative, Apache-2.0).
- JSON Schema 2020-12, Core and Validation.
- YAML 1.2.2, for the scalar and block rules of the emitter.
- W3C SPARQL 1.1 Protocol and Graph Store HTTP Protocol.
- RFC 9110 (HTTP semantics, conditional requests), RFC 6265 (cookies), RFC 7617 (Basic),
  RFC 6750 (Bearer), RFC 9512 (`application/yaml`).
- The docs.rs documentation of `clap_complete` 4 and `clap_mangen` 0.2 (MIT OR
  Apache-2.0), and the nixpkgs manual on `installShellFiles`.
- The Sparkles code (`auth::ROUTES`, `auth::need`, the router and the handlers) and
  `docs/API.md`.

## Outcome

**Delivered.** Phase 1 landed on 2026-10-02.
- `crates/sparkles-server/src/openapi/` builds the document. `paths.rs` describes 174
  operations on 105 paths, which is every API template of `auth::ROUTES`. `schemas.rs`
  has 110 schemas. 52 are described member by member, and 58 are open objects that link
  to their section of `docs/API.md`. `yaml.rs` is the YAML emitter of §2.3.
- `GET /$/openapi.json` and `GET /$/openapi.yaml` are public routes of the route table.
  The document is built on the first request and kept, with an ETag for each form.
- `docs/openapi.json` is the checked-in copy, and `mise run openapi` rewrites it.
- `sparkles openapi [--format json|yaml]`, `sparkles completions <shell>` and
  `sparkles man [--dir DIR]` are new subcommands, in `openapi` and `cli_docs.rs`.
- `nix/package.nix` installs the bash, zsh and fish completions, a man page per
  subcommand (96 pages) and `share/doc/sparkles/openapi.json`.
- The UI's Server page links both forms of the description from its Endpoints panel.
  API.md has an OpenAPI section, and USAGE.md a section on completions and man pages.

**Deviations and decisions.**
- An operation lists `security` only when it differs from the document's. The document's
  default is every scheme with the session cookie alone, which is what a safe operation
  that needs a permission accepts. Public operations, operations open to any caller,
  web-session operations and every unsafe operation list their own. Repeating the
  default on each of the 174 operations made up a sixth of the file.
- The repeated responses and bodies became shared components. They are the responses
  `QueryResults`, `GraphRead`, `WriteDone` and `TaskStarted`, and the request bodies
  `RdfData` and `SparqlQuery`.
- Unions of open objects use `anyOf`, not `oneOf`. An update's statistics, a receipt and
  a dry-run report all allow any member, so one body can match several of them, and
  `oneOf` would reject it. `oneOf` remains where the choices exclude each other, such as
  `null` or an object.
- `HEAD` is a separate operation where the route table lists it (`/{ds}/get`) and on the
  Graph Store routes, where it matters for entity tags. On `/{ds}/sparql` and
  `/{ds}/query` the GET operation says that HEAD works the same way.
- `auth::need` and `auth::Need` are now exported outside tests, since the description
  calls them at run time.
- `sparkles man` without `--dir` prints only `sparkles.1`. The Nix package installs
  elvish and PowerShell completions nowhere, because nixpkgs has no standard place for
  them. `sparkles completions elvish` and `powershell` print them for manual setup.
- Writes to stdout ignore a closed pipe, so `sparkles completions zsh | head` exits
  quietly.

**Tests.**
- `openapi::tests` checks:
  - the paths and methods against `auth::ROUTES`, in both directions;
  - that every `$ref` resolves, `operationId`s are unique lower camel case, every tag is
    defined and used, path parameters match the template, and no parameter repeats;
  - security and `x-sparkles-permission` for public, metrics, write, signed-in,
    web-session and server-admin routes;
  - that every link into `docs/API.md` names a heading that exists;
  - the checked-in copy, and that the YAML form names every operation.
- `yaml::tests` covers the scalar rules and the block layout.
- `router_tests::auth::openapi_is_public` fetches both forms anonymously from a server
  with auth on, revalidates with the ETag (`304`), and checks that invalid credentials
  still get `401`.
- `tests/cli_docs.rs` runs `completions` for every shell, `man --dir` and `openapi`
  against the binary.

The document was also checked outside the test suite at landing. Redocly CLI's
`lint` with its recommended rules found it valid, with 9 warnings for public operations
that have no 4xx response, such as `/$/ping`. `openapi-typescript` 7 generated types
from it without errors. The YAML form, read back with `yq`, equals the JSON form.

**Measurements.** The JSON copy is 454 KB and the YAML form 317 KB. The server builds
the document on the first request and keeps both forms, about 0.8 MB of memory. In the
release binary of the Nix package, whose code and data take 58 MB, the functions that
build the description take 0.8 MB. The `json!` literals expand to code rather than
data. `clap_complete` takes 84 KB, and `clap_mangen` with `roff` 66 KB. Embedding the
checked-in copy instead would save about 0.35 MB of code but make the served document
only as current as the copy, so it was not done. The tradeoff is binary size for a
description computed from the code that serves it.

**Not built.** Phase 2's schemas for backups, the search indexes, reasoning, write-time
validation, history and the other open objects, and the response-validation test that
would keep them honest. A "try it" page in the UI, and a bundled Swagger UI. Dynamic
completion of dataset names, and Nushell completions.
