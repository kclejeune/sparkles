# Usage

This guide covers operating the `sparkles` binary. It describes running the server, the
command-line tools, automatic compaction, the formatter and linter, backups, outbound
requests, path search, integrity checks, the MCP server, embedding the library, the
Python package, the JVM library for Apache Jena, the Rust client, Docker and deploying on
NixOS. [API.md](API.md)
specifies the HTTP API.
[DEVELOPMENT.md](DEVELOPMENT.md) covers building from source and testing.

Sparkles is experimental. The on-disk format, HTTP API and CLI may change between commits
without a migration path, so keep backups of anything you cannot regenerate.

* [Running the server](#running-the-server)
  * [Network exposure](#network-exposure)
  * [TLS](#tls)
  * [Endpoints and operations](#endpoints-and-operations)
  * [Previewing a write](#previewing-a-write)
  * [Fuseki and Jena clients](#fuseki-and-jena-clients)
  * [Migrating from Fuseki](#migrating-from-fuseki)
  * [Restricting users to some graphs](#restricting-users-to-some-graphs)
  * [Hiding some triples from some users](#hiding-some-triples-from-some-users)
  * [`serve` options](#serve-options)
* [Command-line tools](#command-line-tools)
  * [Shell completions and man pages](#shell-completions-and-man-pages)
  * [Constraints next to the counts](#constraints-next-to-the-counts)
  * [What each class uses, and what changed](#what-each-class-uses-and-what-changed)
  * [File tools](#file-tools)
  * [Validating with SHACL and ShEx](#validating-with-shacl-and-shex)
  * [Drafting shapes from the data](#drafting-shapes-from-the-data)
  * [Stored queries](#stored-queries)
  * [DESCRIBE modes](#describe-modes)
  * [GraphQL](#graphql)
  * [Loading CSV and TSV](#loading-csv-and-tsv)
* [Automatic compaction](#automatic-compaction)
* [Formatting](#formatting)
* [Linting](#linting)
* [Backup repositories](#backup-repositories)
* [Outbound requests (SERVICE and LOAD)](#outbound-requests-service-and-load)
  * [Correlated, bulk and cached SERVICE](#correlated-bulk-and-cached-service)
* [Finding paths](#finding-paths)
* [Embeddings computed on write](#embeddings-computed-on-write)
* [Checking a database](#checking-a-database)
* [MCP server (LLM agents)](#mcp-server-llm-agents)
* [Embedding the library](#embedding-the-library)
* [Python](#python)
* [JVM (Apache Jena)](#jvm-apache-jena)
  * [Opening a Jena dataset](#opening-a-jena-dataset)
  * [Jena transactions](#jena-transactions)
  * [Jena queries and updates](#jena-queries-and-updates)
  * [Loads, receipts and the TDB2 import](#loads-receipts-and-the-tdb2-import)
  * [Blank node labels](#blank-node-labels)
  * [Jena exceptions](#jena-exceptions)
  * [Differences from TDB2](#differences-from-tdb2)
* [Rust client](#rust-client)
* [Docker](#docker)
* [Deploying on NixOS](#deploying-on-nixos)

## Running the server

```sh
sparkles serve --data ./data --port 3030   # UI at http://localhost:3030/ui/
```

The server and CLI use [mimalloc](https://github.com/microsoft/mimalloc) as their
allocator. It comes from the `mimalloc` cargo feature of `sparkles-server`, which is on
by default. The `sparkles` library leaves the choice of allocator to its embedder. Once
no request has been active for `--idle-release-ms` (default 1000 ms), `sparkles serve`
hands free heap memory back to the OS. The interval counts from the end of the last
request, so requests that keep arriving, even with short gaps, never meet a release. A build with
`--no-default-features --features reasoning,shacl` uses the system allocator and
`malloc_trim` instead.

The binary includes EPSG-derived definitions of projected coordinate reference systems,
so GeoSPARQL literals can name codes such as `EPSG:27700` without a `--geo-crs` file.
They come from the `crs-definitions` crate through the `geo-epsg` cargo feature of
`sparkles-server`, which is on by default. The EPSG Dataset they derive from belongs to
IOGP and is distributed under the [EPSG terms of use](https://epsg.org/terms-of-use.html).
The terms allow free use and redistribution but forbid selling the data. They require
that every recipient gets the terms and that modified data is not presented as EPSG data.
`THIRD_PARTY_LICENSES.md` carries the terms. A build without the definitions leaves
`geo-epsg` out of the default features.

```sh
cargo build --release -p sparkles-server --no-default-features \
  --features reasoning,shacl,shex,mimalloc,text,geo,otel,auth,mcp,backup,fmt,tls
```

The `sparkles` library and the Python package do not include the definitions.
[API.md](API.md#epsg-codes) describes which codes resolve and how accurate their datum
shifts are.

### Network exposure

`serve` listens on `127.0.0.1` by default. It refuses to start on a non-loopback `--host`,
such as `0.0.0.0`, without `--auth-config`, because without authentication every caller
may read, write and administer every dataset. `--allow-open-network` (or
`SPARKLES_ALLOW_OPEN_NETWORK=1`) serves it open anyway and logs a warning. A network
listener without request rate limits (`query`, `update` or `admin`) also logs a warning,
with or without auth.

An authenticating reverse proxy in front does not make an open backend safe. Bind the
backend to loopback or a Unix socket (`--unix-socket`), or firewall it, so that nothing
can bypass the proxy.

A server without `--auth-config` also guards against web pages that its operator opens:

* It answers only requests whose `Host` is an IP address, `localhost` or `*.localhost`,
  `--host`, or a `--public-host` name. Anything else gets `421`. This stops a page that
  rebinds its own DNS name to the server.
* It refuses unsafe requests, and anything that writes or administers, when they come
  from another site. It decides by `Origin` and `Sec-Fetch-Site`, and answers
  `403 cross-origin request refused`.
* It sends no CORS headers unless `--cors-origin` names an origin.

These checks do not affect the UI that the server itself serves, the CLI, or other
non-browser clients. Behind a reverse proxy, pass the name that clients use to reach the
proxy with `--public-host`. The NixOS module does this for its nginx virtual host.

Every response carries `X-Content-Type-Options: nosniff` and `X-Frame-Options: DENY`.
The UI's pages have a Content Security Policy that forbids framing and allows scripts
only from the UI itself, with its inline start-up scripts allowed by hash. Their
`script-src` also has `'wasm-unsafe-eval'` so the page can compile the formatter's
WebAssembly module. That permits WebAssembly compilation only, not JavaScript's `eval`.
API responses have `Content-Security-Policy: default-src 'none'; frame-ancestors 'none'`.

### TLS

Most deployments terminate TLS at a reverse proxy, such as the nginx virtual host of the
NixOS module, and keep the server on loopback or a Unix socket. When nothing sits in
front of it, the server can serve HTTPS itself:

```sh
sparkles serve --host 0.0.0.0 --port 443 --auth-config /etc/sparkles/auth.toml \
  --tls-cert /etc/sparkles/fullchain.pem --tls-key /etc/sparkles/key.pem --loc wiki=db/wiki
```

`--tls-cert` is a PEM file with the server's certificate first and its intermediates
after it. `--tls-key` is its PEM private key (PKCS#8, PKCS#1 or SEC1). The server checks
that the key fits the certificate before it binds. It speaks TLS 1.2 and 1.3 through
rustls, and clients choose HTTP/2 or HTTP/1.1 through ALPN. Plain HTTP on the same port
gets no answer.

The server reads both files again on SIGHUP and when either file changes, which it
checks once a minute. A certificate renewed by an ACME client is therefore picked up
without a restart. A pair that does not load, such as a new certificate next to the old
key, is logged as an error, and the server keeps the pair it has. Connections that are
open keep their certificate. Handshakes run beside the accept loop, at most 1024 at once
and each for at most 10 seconds, so slow clients cannot hold up others.

Over TLS, the server sets `X-Forwarded-Proto: https` on requests that do not carry the
header, so session cookies get `Secure` and the `__Host-` prefix, and origin checks see
`https`. The `--metrics-addr` listener and `--unix-socket` stay plain HTTP, and
`--tls-cert` cannot be combined with `--unix-socket`.

### Endpoints and operations

A dataset `ds` has the Fuseki-style endpoints `/ds/sparql`, `/ds/update`, `/ds/data`
(GSP), `/ds/upload` and `/ds/patch` (RDF Patch). The server also has `/$/datasets`, `/$/stats/ds`,
`/$/compact/ds`, `/$/backup/ds` and `/$/tasks` (see [API.md](API.md)). `--mem NAME` adds
an in-memory dataset, and `--loc NAME=PATH` serves an existing database.

`/$/ping` is the liveness check and `/$/ready` the readiness check. `/$/ready` returns
`503` once shutdown starts on SIGINT or SIGTERM. The requests in flight then get
`--shutdown-grace` seconds to finish, after which the rest are cancelled and commit
nothing ([API.md](API.md#shutdown)). `/$/metrics` serves Prometheus metrics.
Every response carries an `X-Request-Id`, and each request is logged once under the
`sparkles::access` target.

### Previewing a write

Add `dryRun=true` to an update, a Graph Store write or an upload to see what it would do
without doing it. The write runs against the current data, passes the dataset's
validation and quota checks, and rolls back. The response gives the commit it would
create, its counts per graph, the validation result and whether it fits the quota, and
its status is the one the write would get. `changes=N` lists up to `N` changed quads:

```sh
curl 'localhost:3030/ds/update?dryRun=true&changes=20' \
  -H 'Content-Type: application/sparql-update' --data-binary @migration.ru
```

A preview of a migration answers `422` if the dataset's SHACL or ShEx guard would reject
it, and `507` if it would go over the storage quota, so a script can stop there. A
Graph Store write can then be made only if nothing changed since the preview, by sending
it with `If-Match: W/"<datasetId>:<head>:ttl"`, built from the `datasetId` and `head`
of the preview. With `Accept: application/rdf-patch`, the preview is the change as an
RDF Patch. The MCP `sparql_update` tool takes `dryRun: true` too.
[API.md](API.md#write-previews) describes the response.

Sparkles has no upsert verb. To replace the values of some properties, delete the old
values and insert the new ones in one update, which commits atomically:

```sparql
DELETE { GRAPH <urn:g> { ?s ?p ?o } }
WHERE  { VALUES (?s ?p) { (<urn:a> <urn:title>) (<urn:b> <urn:title>) } GRAPH <urn:g> { ?s ?p ?o } } ;
INSERT DATA { GRAPH <urn:g> { <urn:a> <urn:title> "A" . <urn:b> <urn:title> "B" } }
```

To make a graph equal to a file, `PUT` the file to `/ds/data?graph=…`. The commit records
only the quads that changed.

### Fuseki and Jena clients

Clients written for Fuseki work unchanged. Jena's `RDFConnectionRemote` and
`RDFConnectionFuseki` connect to `http://host:3030/ds`, and `GSP`, `DSP`,
`QueryExecHTTP` and `UpdateExecHTTP` to the endpoints under it. `RDFConnectionFuseki`
sends and asks for RDF Thrift, which the server reads and writes. Fuseki's admin calls
work too: `POST /$/datasets?dbName=ds&dbType=tdb2`, a `config.ttl` body on
`POST /$/datasets`, `POST /$/backup/ds` and its alias `POST /$/backups/ds`,
`GET /$/backups-list`, `POST /$/compact/ds?deleteOld=true`, `GET /$/tasks/{id}`,
`GET /$/stats`, `POST /$/datasets/ds?state=offline` and the `/$/validate/*` services.
[API.md](API.md#datasets-admin) lists where Sparkles differs, such as the assembler
settings it refuses.

Fuseki's `patch` operation is `/ds/patch`, which applies an RDF Patch in one commit.
It takes `POST` and `PATCH`, the text form `application/rdf-patch`, and also the binary
form `application/rdf-patch+thrift` that Fuseki refuses. A `POST` of a patch to `/ds`
works too. A patch whose `prev` header names a commit of the dataset applies only while
that commit is the head, so a chain of patches from `/ds/diff` or `/ds/changes` stops at
the first gap. [API.md](API.md#applying-rdf-patch) has the details.

```sh
curl -X POST http://localhost:3030/ds/patch -H 'Content-Type: application/rdf-patch' \
  --data-binary @changes.rdfp
```

Fuseki's direct Graph Store naming, where the request URL names the graph, is off by
default. `sparkles serve --gsp-direct-naming` turns it on for every dataset, so that
`curl -X PUT -H 'Content-Type: text/turtle' --data-binary @g.ttl
http://localhost:3030/ds/graphs/one` writes the graph
`<http://localhost:3030/ds/graphs/one>`. Behind a proxy, the graph IRI takes its scheme
and host from `X-Forwarded-Proto` and `X-Forwarded-Host`.

`mise run test:jena-clients` runs Jena's own clients against a server on a temporary
directory. It takes Jena and a JDK from nixpkgs, or from `JENA_HOME` and `JAVA`
([DEVELOPMENT.md](DEVELOPMENT.md)).

### Migrating from Fuseki

`sparkles config import fuseki` reads a Fuseki configuration and writes the equivalent
Sparkles setup into a directory. The first argument of `config import` names the kind of
configuration, and `fuseki` is the only kind. The command takes a `config.ttl`, any
service files, or a Fuseki base directory such as `run/`. A directory is read with its
`config.ttl`, every file in its `configuration/` directory and its `shiro.ini`:

```sh
sparkles config import fuseki /srv/fuseki/run --out sparkles/
sh sparkles/load.sh      # once, with Fuseki stopped
sh sparkles/serve.sh     # extra arguments go to sparkles serve
```

The directory holds these files:

* `serve.sh` runs `sparkles serve` with the converted flags, such as `--loc`, `--mem`,
  `--text`, `--geo`, `--rdfs`, `--timeout` and `--union-default-graph`.
* `load.sh` moves the data. Sparkles cannot read TDB files, so for each TDB2 database it
  runs Jena's `tdb2.tdbdump` and loads the dump with `sparkles load`. It also loads the
  files that `ja:data` and `ja:externalContent` name, and materializes inferences with
  `sparkles infer`.
* `auth.toml` is the auth configuration, written when the configuration has users or
  access rules. Users come from `shiro.ini` or from the file that `fuseki:passwd` names.
  Plain-text passwords are hashed with argon2id, and a user whose password is already
  hashed is left commented out until you add a hash from `sparkles auth hash`. Shiro's
  `[urls]` rules, `fuseki:allowedUsers` and graph access control become grants. Where
  Sparkles cannot express a rule exactly, the grant gives less access, never more.
* `datasets/NAME/` holds a dataset's `text.json`, `geo.json`, RDFS schema and rules.
* `report.txt` lists every element of the configuration as converted, approximated, a
  manual step, ignored or unsupported, with the reason.

The report is also printed. `--check` prints it and writes nothing, and
`sparkles config check fuseki PATH` does the same. `--format json` prints the report as
JSON. The exit status is 1 when something important has no Sparkles equivalent, such as
an endpoint at a name Sparkles does not serve, a dataset assembled from selected graphs
or custom Java code. It is 2 when the configuration cannot be read.

`sparkles serve --fuseki-config PATH` converts the configuration at each start and
serves the result. It loads `ja:data` files into in-memory datasets at each start, as
Fuseki does, keeps its persistent datasets in `<data>/fuseki/`, and refuses to start when
the report has an unsupported item. Flags given on the command line win over the
converted ones. It suits trying a configuration out, and the files of `config import`
are the better base for a lasting migration. [Spec G08](specs/G08-fuseki-configuration.md)
lists how each Fuseki setting converts.

### Restricting users to some graphs

With `--auth-config`, a grant can cover only some named graphs of a dataset, or only some
of its endpoints. A dataset that keeps each tenant's data in its own named graphs can
then be shared without splitting it:

```toml
# a tenant reads the shared reference data and writes its own graphs
[[roles.tenant-a.grants]]
dataset = "crm"
level = "read"
graphs = ["urn:x-arq:DefaultGraph", "https://example.org/reference/*"]
[[roles.tenant-a.grants]]
dataset = "crm"
level = "write"
graphs = ["https://example.org/tenant/a/*"]

# analysts query every graph but cannot download the data through the Graph Store
[[roles.analysts.grants]]
dataset = "crm"
level = "read"
endpoints = ["query", "info"]

[[users]]
name = "ann"
password = "$argon2id$…"
roles = ["tenant-a"]
```

A member of `tenant-a` sees the default graph, the reference graphs and its own graphs.
Its queries, Graph Store reads, searches, schema pages and diffs cover those graphs only,
and the other tenants' graphs behave as if they did not exist. Its writes may change only
`https://example.org/tenant/a/*`. Routes that summarize the whole dataset, such as
`/$/stats/crm`, refuse it with `403`. `sparkles auth check --config FILE` validates the
grants. [API.md](API.md#graph-level-access-control) describes every rule and maps
Fuseki's `access:entry` and `fuseki:allowedUsers` settings onto grants.

### Hiding some triples from some users

A protection hides some triples of a dataset from everyone whose grants do not lift it.
It can match triples by predicate, by the class of their subject, or by a SPARQL pattern
that sees the caller's name and roles:

```toml
# only HR reads and writes salaries
[[protections]]
name = "salaries"
dataset = "staff"
predicates = ["https://example.org/salary"]

[[roles.hr.grants]]
dataset = "staff"
level = "write"
lifts = ["salaries"]

# expense reports are visible to their submitter and to finance
[[protections]]
name = "expenses"
dataset = "staff"
classes = ["https://example.org/ExpenseReport"]
pattern = "?s ex:submittedBy ?user"
prefixes = { ex = "https://example.org/" }

[[roles.finance.grants]]
dataset = "staff"
level = "read"
lifts = ["expenses"]
```

Everyone with `staff = "read"` reads the dataset without salaries and without other
people's expense reports. A member of `hr` also reads and writes salaries. The triples
stay hidden in every answer, including counts, searches, schema pages, exports and diffs,
and writing a protected triple without the protection lifted fails with `403`.
`sparkles auth check --config FILE` validates the protections and their patterns.
[API.md](API.md#protections-of-triples) describes the rules, the caller variables, what
happens to materialized inferences, and the limits.

### `serve` options

| Flag | Default | Meaning |
|---|---|---|
| `--host ADDR` | `127.0.0.1` | Listen address. A non-loopback address needs `--auth-config` or `--allow-open-network`. |
| `--allow-open-network` | off | Serve without `--auth-config` on a non-loopback address, and log a warning. Also `SPARKLES_ALLOW_OPEN_NETWORK=1`. |
| `--public-host NAME` | | A host name that clients use to reach the server, such as a reverse proxy's. Repeatable. Without `--auth-config`, names other than IP addresses, `localhost` and `--host` are refused with `421`. With `--auth-config`, the same applies to requests that carry trusted proxy headers from loopback or the Unix socket. |
| `--tls-cert FILE`, `--tls-key FILE` | | Serve HTTPS with this PEM certificate chain and key (see [TLS](#tls)). Both are re-read on SIGHUP and when they change. |
| `--cors-origin ORIGIN` | none | A browser origin, such as `https://yasgui.example`, whose pages may call the API cross-origin without credentials. Repeatable. With `--auth-config`, it is added to `cors.origins`. Without auth, such a page may do everything the server allows. |
| `--timeout S` | `60` | Default query timeout in seconds. `timeout=` sets it per request. |
| `--update-timeout S` | `0` | Default SPARQL update timeout in seconds; `0` means none. `timeout=` sets it per request. An update that times out changes nothing. |
| `--max-timeout S` | `1800` | Largest `timeout=` a query or update may ask for; `0` means unlimited. Never below `--timeout` or `--update-timeout`. |
| `--query-memory-mb N` | `8192` | Budget for the estimated memory of a query's intermediate results; `0` means unlimited. |
| `--max-result-mb N` | `1024` | Budget for the body of a SPARQL query response; `0` means unlimited. |
| `--max-export-mb N` | `0` | Budget for the body of a Graph Store GET, which exports a graph or the whole dataset; `0` means unlimited. |
| `--max-rows N` | `200000000` | Rows in any intermediate result. |
| `--max-rows-produced N` | `0` | Rows that all the operators of one query produce together; `0` means unlimited. It caps the work of a query independently of the machine's speed. |
| `--max-query-body-mb N` | `16` | Largest SPARQL query body, which also covers explain and `/shacl` shapes; `0` means unlimited. A larger body gets `413`. |
| `--max-update-body-mb N` | `256` | Largest SPARQL update body; `0` means unlimited. Bulk data goes through the Graph Store or `/upload`. |
| `--max-admin-body-mb N` | `16` | Largest `/$/…` or prefix-change body; `0` means unlimited. `/$/auth/*` bodies are capped at 64 KiB. |
| `--max-upload-mb N` | `4096` | Largest Graph Store write or upload body; `0` means unlimited. The body is streamed to a temporary file and counted after HTTP decompression. |
| `--min-free-disk-mb N` | `1024` | Free disk space to keep; `0` turns the check off. The server refuses with `507` to spool a request body when the temporary directory's file system would keep less. It refuses to commit, rebuild, clone or write an N-Quads backup (`/$/backup`) when the data directory's file system would keep less. |
| `--max-mem-dataset-mb N` | `4096` | Largest in-memory dataset; `0` means unlimited. A commit that would grow one past it fails with `507`. |
| `--max-dataset-mb N` | `0` | Default storage quota of a persistent dataset, in MiB of its directory on disk; `0` means unlimited. A write that would take a dataset past its quota fails with `507`. `sparkles quota` and `/$/quota/{ds}` set a quota per dataset ([API.md](API.md#storage-quotas)). |
| `--shutdown-grace S` | `20` | Seconds that requests in flight get to finish after SIGTERM or SIGINT. The rest are then cancelled, and a cancelled write commits nothing. |
| `--max-tasks N` | `4` | Background tasks that may run at once: compaction, clones, reasoning, full-text, spatial and vector index builds, and N-Quads backups. More tasks wait as `queued`. `0` means no limit. |
| `--max-clones N` | `2` | Clones that may run at once, within `--max-tasks`. More clones wait as `queued`, and other tasks still start in free slots. `0` leaves only the limit of `--max-tasks`. |
| `--no-auto-compact` | | Never compact automatically. `POST /$/compact/{ds}` and `sparkles compact` still work. See [Automatic compaction](#automatic-compaction) for the `--auto-compact-*` flags. |
| `--backup-config FILE` | | TOML file with the backup repositories, policies, credential sources and the limits on repositories registered through the API. Also `$SPARKLES_BACKUP_CONFIG`. Re-read on SIGHUP, and read-only through the API. |
| `--backup-max-tasks N` | `2` | Backup, restore, verify and GC tasks that may run at once. More wait as `queued`. |
| `--format-endpoint on\|authenticated\|off` | `on` | Who may use `POST /$/format` (see [API.md](API.md#formatting)). `on` admits every caller the server admits, `authenticated` every caller but the anonymous principal (`401`), and `off` nobody (`404`). A UI built with the formatter's WebAssembly module formats in the page and needs the endpoint only as a fallback. |
| `--format-max-mb N` | `16` | Largest `POST /$/format` body; `0` means unlimited. |
| `--format-timeout S` | `10` | Seconds a `POST /$/format` request may take, including the wait for a free slot (one per core). A slower request gets `408`. |
| `--vector-memory-mb N` | `4096` | Memory for packed vectors and HNSW graphs (`spk:vectorSearch` and vector indexes), per index generation. A build past it leaves the index `over-budget`, and a search past it gets `507`. A global flag. |
| `--text NAME[=FILE]` | | Enable full-text search for a dataset. `FILE` is a `text.json`-shaped configuration file. |
| `--validate NAME[=FILE]` | | Set a dataset's write-time validation from `FILE`, a `PUT /$/validation/{ds}` body, or with `NAME` alone validate the dataset with the configuration it has. Shapes and schemas without inline text are read from the path in `source`, relative to `FILE`. The data is validated in full before the server listens, and the result is logged. A `reject` configuration the data does not pass stops the start ([API](API.md#write-time-validation)). |
| `--rdfs NAME=FILE` | | Answer a dataset's queries over the RDFS closure of its graphs with respect to the schema in `FILE`, as Fuseki's `--rdfs` does. The setting is kept like one made with `PUT /$/rdfs/{ds}` ([API](API.md#rdfs-on-read)). |
| `--fuseki-config PATH` | | Start from a Fuseki configuration, a `config.ttl` or a Fuseki base directory, converted at each start (see [Migrating from Fuseki](#migrating-from-fuseki)). Flags given on the command line win. A part with no Sparkles equivalent stops the start. |
| `--geo NAME[=FILE]` | | Enable the spatial index for a dataset. `FILE` is a `geo.json`-shaped configuration file. The build runs before the server starts listening. |
| `--geo-mb N` | `4096` | Memory for each dataset's spatial index (geometry column and trees). A build that would exceed it is refused, the status says `over-budget`, and queries run without the index. |
| `--geo-op-vertices N` | `2000000` | Largest total of input vertices for one geometry operation (overlay, buffer, hull, relate). A larger operation is a type error. |
| `--geo-crs FILE` | | Projected CRSs to support beyond the built-in ones and the EPSG codes the binary resolves, as a JSON file that maps CRS IRIs to proj4 definitions. A global flag, also read from `SPARKLES_GEO_CRS`. A file that cannot be read stops the command ([API](API.md#crss-from-proj4-definitions)). |
| `--log-format text\|json` | `text` | Log format on stderr. A global flag. `RUST_LOG` filters as usual. |
| `--no-access-log` | | No per-request log lines. |
| `--no-metrics` | | `/$/metrics` answers `404`, and no request metrics are kept. |
| `--metrics-max-datasets N` | `100` | Datasets that get their own metric labels. The rest share `$other`. |
| `--metrics-fuseki-names` | off | Also expose Fuseki's metric names (`fuseki_requests`, `fuseki_requests_good`, `fuseki_requests_bad`) on `/$/metrics`, for dashboards built for Fuseki. See [API.md](API.md#fuseki-metric-names). |
| `--gsp-direct-naming` | off | Fuseki's direct Graph Store naming on every dataset: a request to `/{ds}/{path}` that names no endpoint reads or writes the graph whose IRI is the request URL. See [Fuseki and Jena clients](#fuseki-and-jena-clients). |
| `--metrics-addr HOST:PORT` | | Also serve `/$/metrics` on this address, with the same authentication. Without `--auth-config`, an address that is not loopback needs `--allow-open-network`. |
| `--otel` | off | Export traces and metrics over OTLP. `OTEL_EXPORTER_OTLP_ENDPOINT` also turns this on, and the standard `OTEL_*` variables apply (see [API.md](API.md), OpenTelemetry). |
| `--otel-logs` | off | Export log events over OTLP as well. |
| `--otel-query-text` | off | Record query text (`db.query.text`) and plan operator descriptions in spans. These may hold data. |
| `--otel-plan-spans` | off | One span per executed plan operator. |
| `--rate-limit SPEC` | off (`preauth=30/min,burst=60` with `--auth-config`) | Limit a request class per client, for example `query=100/s,burst=200,concurrency=64` or `auth=10/min,burst=5`. `preauth=…` limits authentication failures per address before credentials are checked. Repeatable; see [API.md](API.md), Rate limiting. |
| `--rate-limit-config FILE` | | JSON rate-limit configuration, re-read on SIGHUP. `--rate-limit` applies on top. |
| `--rate-limit-trusted-proxy CIDR` | | A proxy whose `X-Forwarded-For` names the client. Repeatable; `unix` means the `--unix-socket`. Limits by address need a peer address that clients cannot choose, so list only proxies that overwrite or append to the header. |
| `--rate-limit-trusted-proxy-header H` | `x-forwarded-for` | The one header in which trusted proxies name the client: `x-forwarded-for` or `forwarded` (RFC 7239). The other is ignored. |
| `--no-service` | | Refuse `SERVICE` for everyone. |
| `--outbound-allow-private` | off | Let `SERVICE` and `LOAD <http…>` reach loopback, private, shared (CGNAT) and unique-local addresses (see [Outbound requests](#outbound-requests-service-and-load)). |
| `--outbound-block-private` | | Refuse those addresses. This is already the default for `serve` and `mcp`. The local `query` and `update` allow them by default, so for those commands it is an opt-in. |
| `--outbound-allow HOST_OR_CIDR` | | Contact only these destinations. Repeatable. |
| `--outbound-timeout S` | `60` | Total time of one outbound request, until the end of its response. |
| `--outbound-max-mb N` | `256` | Largest outbound response, after decompression. |
| `--outbound-request-max-mb N` | 4 × `--outbound-max-mb` (`1024`) | Bytes that all the SERVICE calls and LOADs of one query or update may receive together. Past it, the request gets `507`. |
| `--outbound-request-timeout S` | 4 × `--outbound-timeout` (`240`) | Time that all the SERVICE calls and LOADs of one query or update may take, summed. |
| `--service-bulk-size N` | `10` | Inputs per request of `SERVICE <loop:bulk:…>` (see [Correlated and cached SERVICE](#correlated-bulk-and-cached-service)). |
| `--service-bulk-max N` | `100` | The most inputs per request of any bulk SERVICE. `bulk+n` is capped to it. |
| `--service-cache-mb N` | `64` | The cache of remote results that `SERVICE <cache:…>` uses, per dataset. `0` turns it off. A global flag. |
| `--load-dir DIR` | | Let `LOAD <file:…>` read the regular files under `DIR`, with symbolic links resolved and nothing outside it. Without this flag, the server refuses file loads. |
| `--embedding-secret NAME=SOURCE` | | A secret that vector indexes may name as their embedding API key: `NAME=env:VARIABLE` or `NAME=file:PATH`, read when a request is made. Repeatable. See [Embeddings computed on write](#embeddings-computed-on-write). |
| `--no-embedding` | | Compute no embeddings. No worker sends text to a provider, and searches cannot pass text. The configurations are kept. |
| `--wal-prealloc-kb N` | `4096` | The most a write-ahead log grows ahead of its commits at once, as zero bytes written and synced in advance; `0` means each commit appends to the file. A commit that overwrites preallocated bytes syncs them without a file-system journal commit, which on ext4 and XFS is faster and no longer waits behind other files' writeback. The log ends at its last commit for readers, backups and the quota, and a close trims the zeros. A global flag. |
| `--max-prefixes N` | `1000` | Prefixes per dataset; `0` means unlimited. A global flag. A new prefix past the limit is refused with `400`, and loaded data stops adding its prefixes. |
| `--reason-cache-triples N` | `10000000` | The largest closure of a materialization that a dataset keeps in memory, so that the next re-run or automatic run updates it incrementally. A closure takes about 135 bytes per triple. With `0`, a run reads the closure back from a persistent dataset, and an in-memory dataset runs in full. |

`sparkles serve --help` lists the other options. They include `--read-only`,
`--union-default-graph`, the cache sizes (`--cache-mb`, `--result-cache-mb`,
`--history-cache-mb`), `--auto-reason`, `--auth-config`, `--unix-socket`,
`--map-style-url`, the GraphQL limits ([GraphQL](#graphql)), the `--mcp-*` options
([MCP over HTTP](#over-http)), response compression (`--http-compression`) and the
schema limits.

A request over budget fails with `507` and a JSON body that names the budget. The
outbound total is named `outbound-bytes`. A query can lower its own budgets with the
`memory-mb`, `max-rows`, `max-rows-produced` and `max-result-mb` parameters, but never
raise them ([API.md](API.md#budgets)). A query or write stops as soon as its client
disconnects, and a write then commits nothing. `sparkles query --memory-mb N` applies the
memory budget on the command line, where there is no limit by default.

Short requests run on the thread that received them rather than on a separate pool, so
they do not wait for other threads to wake up. These are queries whose previous run with
the same text took under 10 ms, and updates of up to 64 KiB that only use `INSERT DATA`
and `DELETE DATA`, on a dataset without write-time validation, when no other write holds
the dataset. Such a request runs to its end even if its client disconnects, though its
timeout still applies. Everything else runs on the pool and stops on a disconnect.

## Command-line tools

The equivalents of Jena's `tdb2.*` and `arq` tools work on a database directory
directly. A running server locks the databases it holds, so use the HTTP API instead, or
`--server` where a command takes it:

```sh
sparkles load    --loc db data/*.ttl.gz       # parallel bulk load (tdb2.tdbloader)
sparkles load    --loc db data.trix data.rt   # TriX and Jena's binary syntaxes, read into N-Quads first
sparkles load    --loc db people.csv --base http://ex.org/p/ --key id   # CSV and TSV tables, see below
sparkles query   --loc db 'SELECT ...'        # --results text|json|xml|csv|tsv, --explain, --time
sparkles query   --data file.ttl --query q.rq # query files in memory (arq --data)
sparkles query   --loc db --rdfs schema.ttl 'SELECT ...'   # RDFS on read (--rdfs-graph IRI|default)
sparkles query   --loc db --describe scbd 'DESCRIBE <http://ex.org/a>'   # cbd|scbd|outgoing, --describe-labels
sparkles update  --loc db 'INSERT DATA {...}' # also LOAD <http…>
sparkles patch   --loc db changes.rdfp        # apply RDF Patch files, one commit per file
sparkles compact --loc db                     # merge updates into a new generation
sparkles compact --loc db --if-due            # only when the compaction policy says so (for cron)
sparkles dump    --loc db > dump.nq
sparkles dump    --loc db --out dump.nq.zst   # compression from the extension, or --compress
sparkles dump    --loc db --out dump.trig.gz  # syntax from the extension too, or --format
sparkles dump    --loc db --format ttl --merge   # every graph as one Turtle graph
sparkles dump    --loc db --format nt --graph http://ex.org/g   # one graph's triples
sparkles dump    --server URL --dataset ds --out ds.rt   # a server's dataset, in RDF Thrift
sparkles backup  --loc db --out backups/      # zstd; --compress gzip --level 9, --threads 8
sparkles clone   --loc db --to sandbox        # independent copy (same blank nodes, new dataset id)
sparkles clone   --loc db --to part --graph default --graph 'http://ex.org/g/*'   # some graphs only
sparkles stats   --loc db
sparkles log     --loc db                     # commit history (works next to a running server)
sparkles diff    --loc db 41 42               # what commit 42 changed, as + and - N-Quads lines
sparkles check   --loc db                     # verify the files, read-only (--quick, --format json)
sparkles infer   --loc db --profile owl-rl    # materialize inferences, updating the last run when it can
sparkles infer   --loc db --profile owl-rl --full   # materialize in full
sparkles infer   --loc db --status            # are the inferences up to date?
sparkles infer   --loc db --check             # OWL 2 RL inconsistency checks (exit 1 on violations)
sparkles infer   --loc db --vocab geosparql --geo-default-geometry   # + GeoSPARQL axioms, default geometries
sparkles infer   --loc db --ontology-graph http://ex.org/onto --imports fetch   # input graphs and owl:imports
sparkles text-index --loc db                  # full-text index: --predicate, --exclude-graph, --language en|all,
                                              #   --rebuild, --status, --disable
sparkles geo-index  --loc db                  # spatial index: --predicate, --feature-link, --exclude-graph, --wgs84,
                                              #   --distance geodesic|haversine, --rebuild, --status (JSON), --disable
sparkles vector create --loc db --name emb --predicate http://example.org/emb --dim 384
                                              # vector index with an HNSW graph: --metric, --m, --ef-construction,
                                              #   --ef-search, --exact-threshold, --no-hnsw
sparkles vector list|status|rebuild|drop --loc db [--name emb]   # or --server URL --dataset NAME
sparkles vector create --loc db --name docs --predicate http://example.org/emb --dim 768 \
    --embed-url http://127.0.0.1:11434/v1/embeddings --embed-model nomic-embed-text \
    --embed-from http://www.w3.org/2000/01/rdf-schema#label   # an index that computes its vectors
sparkles vector embed   --loc db              # embed the text that waits, then exit
sparkles vector reembed --loc db --name docs  # embed every text again (a new model behind the name)
sparkles quota   --loc db --max-mb 10240      # storage quota; --default removes it, no flag prints it
sparkles quota   --server URL --dataset db --max-mb 0   # on a server, as server-admin; 0 is unlimited
sparkles compaction --loc db --set deltaRatio=0.02     # automatic compaction settings; --default removes them
sparkles describe-settings --loc db --set mode=scbd   # how DESCRIBE describes a resource; --default removes it
sparkles ping    127.0.0.1:3030               # GET /$/ready over HTTP, then HTTPS; exit 0 on 200 (health checks)
```

`sparkles dump` writes N-Quads by default. `--format`, or the extension of `--out`, picks
another syntax: TriG, N-Triples, Turtle, JSON-LD, RDF/XML, TriX, RDF Thrift (`rt`), RDF
Protobuf (`rpb`) or RDF/JSON (`rj`). A compression extension after it, as in
`dump.ttl.gz`, compresses the output. The dump streams from one snapshot, and Turtle, TriG
and RDF/XML declare the dataset's prefixes. A triple syntax holds the default graph only,
so the other graphs are left out with a warning unless `--merge` writes them into the
default graph. `--graph IRI`, which can be repeated, limits the dump to some graphs, and
`--graph default` names the default graph. In a triple syntax the graphs it names are
written as one graph. `--at` dumps a past state. With `--server URL --dataset NAME` the
dump comes from the server's Graph Store endpoint in the syntax asked for. When the
endpoint cannot give that subset in that syntax, such as two graphs or `--merge`, the
dump is read as N-Quads and converted locally.

`sparkles infer` updates the materialization that `reasoning.json` records when its
rules are the same and monotonic and the commit diff still reaches its commit. It reads
the previous closure from the database, removes what the removed triples no longer
support and derives what the added triples support. It prints whether the run was
full or incremental and, for a full run that could have been incremental, why
([API.md](API.md#reasoning-status-and-diagnostics)).

A run reads the default graph and the graphs its `owl:imports` lead to, unless
`--data-graph` and `--ontology-graph` name other graphs. `--imports none|dataset|fetch`
decides what happens to imports, and with `fetch` the missing ones are loaded into the
database, each into the graph named by its IRI, as `LOAD` would. `--location-mapping
FILE` reads a Jena location-mapping file, and `--refresh-imports` loads the fetched
imports again. A run without these options reads the graphs the recorded status names
([API.md](API.md#input-graphs-and-imports)).

`sparkles vector create` writes the index to `vector.json`, builds it, and waits for the
build. Later openings of the database map the built index from its file. Every
`sparkles vector` command also works against a server with `--server URL --dataset NAME`
in place of `--loc` ([API](API.md#vector-indexes)).

The web UI manages the same indexes. The dataset page has a card for each index, and an
admin of the dataset can create, edit, rebuild and drop indexes there. Anyone who can
read the dataset can measure an index's recall from its card. The **Similar** page
(`/ui/similar`) searches an index from an entity's vector or from a pasted vector, with
controls for k, the metric, `ef` and exact search, and it shows how the server ran each
search.

`sparkles patch` applies RDF Patch files to a database, one commit per file, as the
patch endpoint applies them, and prints each commit as `sparkles update` does. `-` reads
standard input. A file ending in `.trp` is in the binary form and any other in the text
form, as Jena names them, unless `--format text|binary` says otherwise. With `--server URL
--dataset NAME` the files go to the server's `/{ds}/patch` instead.

`sparkles update`, `sparkles load` and `sparkles patch` take `--message TEXT`, which is stored with the
commit they make and shown by `sparkles log` and `/$/commits`. With `--server`, the
message travels in the `Sparkles-Commit-Message` header, and each file that `load` sends
becomes its own commit with the same message. A message is at most 1024 bytes of UTF-8
with no control characters.

```sh
sparkles update --loc db --message 'Fix the labels of ex:alice' 'DELETE … INSERT …'
sparkles load   --server http://localhost:3030 --dataset ds --message 'Nightly import' data.ttl
sparkles log    --loc db                      # the message follows each commit's columns
```

A commit that skipped the dataset's write-time validation, through `--no-validate` or an
HTTP bypass, shows `[unvalidated]` before its message in `sparkles log`.

Past states are read with `--at`, which takes a commit number, `commit:N`,
`time:<RFC 3339>` or `snapshot:NAME`. Every commit since the last compaction or bulk
commit can be read, and `sparkles log` marks them with `*`. A named snapshot or the
retention window keeps older ones. `sparkles diff` shows the quads added and removed
between two states, and `--format json`, `--format count` or `--format patch` change its
output. `patch` writes an RDF Patch that Jena's tools read, and `patch-binary` writes it
as RDF Thrift.

```sh
sparkles query    --loc db --at snapshot:release-1 'SELECT ...'
sparkles dump     --loc db --at time:2026-09-30T14:00:00Z > then.nq
sparkles clone    --loc db --to sandbox --at commit:40
sparkles diff     --loc db snapshot:release-1 head --graph http://ex.org/g
sparkles diff     --loc db 40 head --format patch > changes.rdfp
sparkles snapshot create   --loc db release-1 --note 'before the migration'
sparkles snapshot create   --loc db tmp --expires 7d
sparkles snapshot create   --loc db hot --at commit:40 --warm   # kept materialized
sparkles snapshot retain   --loc db --keep-age 7d --max-bytes 20GiB
sparkles snapshot schedule --loc db --prefix daily- --every 1d --keep-last 7
sparkles snapshot catalog  --loc db --keep-commits 100000 --keep-age 90d
sparkles snapshot gc       --loc db        # expire pins, make scheduled ones, collect, prune
```

A running server does the work of `snapshot gc` every minute. `snapshot catalog` sets how
long the commit catalog keeps the metadata of commits that can no longer be read. Without
it, every commit stays listed. Over HTTP the same features are `?at=`, `GET /{ds}/diff`,
`/$/snapshots/{ds}` and `/$/history/{ds}`
([API: Point-in-time reads and snapshots](API.md#point-in-time-reads-and-snapshots)).

`sparkles history` lists the recorded changes of a subject, predicate, object or graph
across commits, with each commit's time, author and message. It reads the change log,
which keeps the changes after a compaction too
([API: History queries](API.md#history-queries)).

```sh
sparkles history --loc db --subject http://example.org/alice
sparkles history --loc db --predicate http://example.org/price --from time:2026-10-01T00:00:00Z
sparkles history --loc db --subject http://example.org/alice --desc --limit 1   # the last change
```

The same queries run in SPARQL through `SERVICE <urn:x-sparkles:history#changes>`, and
over HTTP as `GET /{ds}/history`.

A server also offers a change feed, `GET /{ds}/changes?after=N`. It lists the commits after
commit N with their changes, as JSON or as one RDF Patch per commit. With `wait=30` a
request waits for the next commit, and with `Accept: text/event-stream` the commits arrive
as server-sent events that resume after the last one a client saw
([API: Change feed](API.md#change-feed)).

```sh
curl 'http://localhost:3030/ds/changes?after=41&wait=30'
curl -H 'Accept: application/rdf-patch' 'http://localhost:3030/ds/changes?after=41'
curl -N -H 'Accept: text/event-stream' 'http://localhost:3030/ds/changes?after=41'
```

The global flag `--commit-digests` makes a command record a change digest with every
commit of the databases it opens. A database keeps the setting once it is on, so later
commands and servers record digests too. See [API: Commits](API.md#commits).

`sparkles geo-index` enables the spatial index if it is off, with the defaults or the
given options. It then builds the index and prints its status to stderr. When the index
is already enabled, opening the database starts the build, and the command reports the
status after it. The command exits 2 when the binary was built without the `geo`
feature. Give it the same `--geo-crs` file as the server, or literals in those CRSs are
left out of the index and the server rebuilds it when it opens the database.

The other commands are:

* `schema` ([below](#constraints-next-to-the-counts));
* `shacl`, `shacl parse`, `shex validate|parse` and `validation`, for validation on request and
  write-time validation ([below](#validating-with-shacl-and-shex));
* `queries`, for stored queries ([below](#stored-queries));
* `describe-settings`, for a dataset's DESCRIBE mode ([below](#describe-modes));
* `graphql`, for a dataset's GraphQL schema and queries ([below](#graphql));
* `csv`, for CSV and TSV tables ([below](#loading-csv-and-tsv));
* `config import` and `config check`, for Fuseki configurations
  ([above](#migrating-from-fuseki));
* `snapshot`, for named snapshots and history retention;
* `quota`, for the storage quota of a dataset, locally or on a `--server`;
* `compaction`, for a dataset's automatic compaction settings, locally or on a `--server`
  ([below](#automatic-compaction));
* `repo` and `backup create|list|show|restore|verify|delete|policy`
  ([below](#backup-repositories));
* `auth`, for password hashes, tokens, and `auth login` for remote `query`, `update` and
  `load --server`;
* `mcp` ([below](#mcp-server-llm-agents));
* `fmt` and `lsp` ([below](#formatting)), and `lint` ([below](#linting));
* `completions`, `man` and `openapi` ([below](#shell-completions-and-man-pages));
* `convert` (`riot`), `qparse`, `uparse`, `compare` (`rdfcompare`, `rdfdiff`), `iri`,
  `langtag`, `rsparql`, `rupdate`, `rset` and `rdfpatch`, for files and endpoints
  ([below](#file-tools));
* `vocab-index`, which adds the sparse vocabulary index (`vocab.idx`) to a database
  whose index was built before it existed. Loads and compactions write it. With it, a
  server that starts with a cold page cache looks up a term with one read instead of one
  per step of a binary search over the vocabulary.

`sparkles help COMMAND` describes each one.

### Shell completions and man pages

`sparkles completions SHELL` prints a completion script for `bash`, `zsh`, `fish`,
`elvish` or `powershell`. The scripts complete subcommands, flags and the values of flags
that take one of a fixed set, and they come from the same definition as `--help`, so they
match the binary that printed them. Write the script where your shell looks for
completions:

```sh
sparkles completions bash > ~/.local/share/bash-completion/completions/sparkles
sparkles completions zsh > ~/.zfunc/_sparkles      # a directory in $fpath
sparkles completions fish > ~/.config/fish/completions/sparkles.fish
sparkles completions powershell >> $PROFILE
```

`sparkles man --dir DIR` writes a man page for `sparkles` and one for each subcommand,
such as `sparkles-serve.1` and `sparkles-auth-login.1`. Without `--dir` it prints
`sparkles.1`. The Nix package installs the bash, zsh and fish completions and the man
pages, so `man sparkles-serve` works after `nix profile install`.

`sparkles openapi` prints the OpenAPI 3.1 description of the HTTP API, and
`--format yaml` prints it as YAML. A running server serves the same document at
`/$/openapi.json` ([API.md](API.md#openapi-description)).

Two environment variables switch off read paths for comparisons.
`SPARKLES_IO_HINTS=off` maps the index and vocabulary files without access hints, so
that a page fault reads the device's whole read-ahead window and blocks are not read
ahead. `SPARKLES_SPARSE_VOCAB=off` looks terms up without `vocab.idx`.

`SPARKLES_UI_DIR=DIR` makes the server read the web UI from a UI build directory, such as
`ui/build`, instead of the copy embedded in the binary. The Nix package sets it to its UI
build, so that its binary needs no UI at compile time.

`sparkles schema --loc db --format void` prints the schema report as a VoID description
in Turtle, and `--format turtle` adds the declared RDFS/OWL schema. The server answers
`GET /$/schema/{ds}` the same way when the request asks for Turtle or another RDF syntax
([API.md](API.md#schema-discovery)).

### Constraints next to the counts

The schema report keeps what the data shows apart from what its SHACL shapes require.
When a database has write-time SHACL validation, `sparkles schema` ends with the
constraints of its shapes per class, and each line says whether a write that breaks the
constraint is refused, committed with a warning, or not checked at all. `--shapes` names
other shapes graphs to read, and `--shapes none` leaves the constraints out.
`--subject-classes` adds, under each predicate, the classes of its subjects with their
triple counts:

```sh
sparkles schema --loc db                                     # counts, then the guard's constraints
sparkles schema --loc db --shapes http://example.org/shapes  # constraints of a shapes graph
sparkles schema --loc db --subject-classes                   # which classes use each predicate
```

An observed `max/subject 1` only describes the current data. A constraint such as
`max 1  [reject-on-write]` is what stops the next write from adding a second value. The
server gives the same layer in `GET /$/schema/{ds}` and `GET /$/schema/{ds}/constraints`,
and the UI's schema browser shows it as SHACL chips next to the observed counts
([API](API.md#constraints-layer)).

### What each class uses, and what changed

`--profiles` lists, for each class, the predicates its instances use, and `--diff`
compares the schema of two states of the database:

```sh
sparkles schema --loc db --profiles                        # every class
sparkles schema --loc db --profiles --class http://ex.org/Person --format json
sparkles schema --loc db --diff 41                         # from commit 41 to the head
sparkles schema --loc db --diff snapshot:before-import --to commit:57
```

A profile line such as
`<http://ex.org/name>  instances 1890/2000  triples 2210  values 1..3  string 2210`
says that 1,890 of the 2,000 people have a name, and that one person has three. An arrow
lists the classes of the values, and a line that starts with `←` names a predicate that
points at the class's instances. A diff prints one line per class or predicate that was
added or removed, and for a changed one each count, declaration or label that differs,
such as `~ class http://ex.org/Person: observed.instances 2000 -> 2004`. Both states must
still be readable, as they must be for `query --at`. The
server offers the same as `GET /$/schema/{ds}/profiles` and `GET /$/schema/{ds}/diff`,
and the UI's schema browser shows the profile of the selected class and has a
**Compare** dialog ([API](API.md#class-profiles)).

### File tools

These commands work on files and endpoints rather than databases. They match Jena's
`riot`, `qparse`, `uparse`, `rdfdiff`, `rdfcompare`, `iri`, `langtag`, `rsparql`,
`rupdate`, `rset` and `rdfpatch`, and `convert` and `compare` answer to Jena's names as
aliases. The design is in [spec G05](specs/G05-command-line-tools.md), and `rdfpatch` is
in [spec F10](specs/F10-replication.md).

```sh
sparkles convert data.ttl.gz --output nt      # stream to N-Triples (default output: N-Quads)
sparkles riot --syntax ttl --output trig < in # standard input needs --syntax, else N-Quads
sparkles convert data.trig --output ttl --merge   # named graphs into the default graph
sparkles convert big.nt --output nq --compress zstd > big.nq.zst   # --compress alone is gzip
sparkles convert --count *.ttl                # triples (or quads) per file and a total
sparkles convert --validate data.ttl          # syntax errors and term warnings, exit 1 on any
sparkles convert --check data.ttl > out.nq    # convert, and warn about IRIs and language tags
sparkles convert data.trix --output ttl       # Jena's TriX, RDF Thrift, RDF Protobuf and RDF/JSON too
sparkles convert -r data/ -o all.nq.zst       # a directory tree into one file
sparkles convert -r data/ --out-dir nt/ --format nt -j 4   # one file per input, 4 at a time
sparkles convert -r data/ --include '*.ttl' --exclude 'drafts/**' --count
sparkles convert people.csv --base http://ex.org/p/ --key id --output ttl   # CSV and TSV tables
sparkles load --loc db --check --strict data.ttl   # the same checks before a load
sparkles qparse 'SELECT ...'                  # the query, formatted
sparkles qparse --print algebra,plan --query q.rq  # SPARQL algebra (SSE) and the physical plan
sparkles qparse --syntax sparql --query q.rq  # strict SPARQL, without ARQ's extensions
sparkles uparse --print algebra 'DELETE ...'  # the update as SPARQL algebra
sparkles compare a.ttl b.nt                   # exit 0 when isomorphic, 1 with a diff, 2 on errors
sparkles iri '<http://Example.org:80/a/../b>' # components, normal form, warnings
sparkles langtag en-us zh-yue-HK en--ltr      # subtags, canonical case, warnings
sparkles rsparql --service https://query.wikidata.org/sparql --query q.rq --results csv
sparkles rupdate --service http://localhost:3030/ds/update 'INSERT DATA {...}'
sparkles rset results.srj --results text      # JSON, XML or TSV results to another format
sparkles rdfpatch changes.rdfp                # the rows of RDF Patch files, and their counts
```

`convert` reads files, or standard input when no file is given or a file is `-`. It takes
the syntax from `--syntax`, then from the file extension. When neither decides, it looks
at the first 8 KiB of the content after decompression. XML is RDF/XML or TriX by its root
element. JSON is RDF/JSON when its top level maps subjects to objects of predicates, and
JSON-LD otherwise. Binary content is RDF Thrift or RDF Protobuf when its first row has
the shape Jena writes. Text whose statements are each one line of terms is N-Quads or
N-Triples, other Turtle-like text is TriG when it has a `GRAPH` keyword or a `{` block and
Turtle otherwise, and lines with the same number of tabs or commas are TSV or CSV.
Content that fits two syntaxes, such as `{}`, is an error that names both. Standard
input that matches nothing is read as N-Quads, as before. An extension or `--syntax`
always wins over the content. Besides the W3C syntaxes `convert` reads and writes Jena's
TriX (`trix`), RDF Thrift (`rt`), RDF Protobuf (`rpb`) and RDF/JSON (`rj`). `load` takes them as well, and
`query --results trix` (or `rt`, `rpb`, `rj`) writes CONSTRUCT and DESCRIBE results in
them. Compressed inputs are detected as `load` detects them. The output
streams, so a file larger than memory converts in bounded memory. Turtle, TriG and
RDF/XML output declare the prefixes that the input declared before its first statement.
A quad in a named graph cannot be written in a triple syntax, so `convert` drops it with
a warning, or with `--merge` writes it into the default graph.

A directory is read with `--recursive` (`-r`), file by file in path order. `--include`
and `--exclude` take globs, which can be repeated. A glob without `/` matches the file
name, and one with `/` matches the path below the directory, where `*` stays within one
directory and `**` crosses them. A file in a directory whose syntax cannot be told is
skipped with a warning. CSV and TSV files are converted with the mapping options of
`load` (`--mapping`, `--template`, `--key`), and `--base` gives the default mapping's
namespace. A `-metadata.json` file next to a table is read with it rather than converted.

The output goes to standard output, or to `--output-file` (`-o`), whose extension picks
the syntax and the compression unless `--output` and `--compress` name them. `--out` and
`--format` are aliases of `--output`, so the file needs its own flag. `--out-dir DIR`
writes one file per input instead, at the input's path below the directory it was found
in, with the output syntax's extension in place of the input's. `--compress` adds its
extension. The files are converted in parallel, `--jobs` (`-j`) at a time, by default
one per CPU up to 8. A file that exists is replaced only with `--overwrite`, and a file
that fails to convert leaves nothing behind. With directories or `--out-dir`, `convert`
prints the statements of each file and a summary with the number of files, the total
and the time. Errors give the file, line and column, and the exit status is 1 when any
file failed.

`--count`, `--sink` and `--validate` write no data. Files are then parsed in parallel, as
`load` parses them. When a file has a syntax error, it is parsed again in order so that
up to 20 errors are reported with their exact line and column. `--validate` is `--sink
--check --strict`, as in Jena. The exit status is 1 when an input had errors, or warnings
under `--strict`.

`--check` reports suspicious IRIs and language tags that the parsers accept. The IRI
rules cover upper-case schemes and hosts, lower-case or needless percent-encodings, user
information, dot segments, empty and default ports, `http` IRIs without a host, and the
syntax of `urn:` (with `uuid` and `oid`), `file:` and `did:` IRIs. The language-tag rules
flag grandfathered tags, extended language subtags and unusual primary languages. Each
distinct value is reported once per file. `load --check` runs the same checks as a
separate pass before the load, and `--strict` then loads nothing when a value has a
warning. `iri` and `langtag` show the rules for one value at a time, with the
components, the RFC 3986 normal form or the canonical case, and `--format json`.
`/$/validate/iri` returns the same warnings.

`qparse` uses the engine's own parser. Its default output is the query formatted by
`sparkles fmt`. `--print algebra` prints the SPARQL algebra in SSE, the form of Jena's
`qparse --print=op`. The operators are spargebra's, which are close to Jena's but not
the same, and made-up names of aggregates and blank nodes print as `?.0` and `_:b0`.
`--print plan` prints the physical plan of `query --explain`. It is planned against an
empty database unless `--loc` or `--data` gives one with real statistics. A syntax
error exits with status 1. Like Fuseki, `qparse` and `uparse` accept Jena ARQ's syntax
extensions by default ([API.md](API.md#arq-syntax-extensions)). `--syntax sparql` (or
Jena's `SPARQL_11` and `SPARQL_12`) rejects them, and `--syntax arq` is the default. In
SSE, ARQ's forms print as Jena prints them, such as `(assign …)`, `(unfold …)`,
`(semijoin …)`, `(antijoin …)` and `(fold …)`.

`compare` reads both files into memory and compares them as RDF datasets up to
blank-node isomorphism. The diff lists quads only in the first file with `<` and quads
only in the second with `>`. Blank nodes are labeled by RDFC-1.0 canonicalization, one
group of connected blank nodes at a time, so a change shows only the group it touches.
`--merge` ignores graph names, and `-q` prints nothing.

`rsparql` and `rupdate` take any SPARQL 1.1 Protocol endpoint as a full URL, such as a
Fuseki, QLever or Wikidata endpoint. `query --server` is different, because it names a
Sparkles server and a dataset and sends the token of `sparkles auth login`. `rsparql`
sends a query by GET, or as a POST form when it is long or `--post` is given. With
`--results text`, the default, it prints result sets as a table and graphs as Turtle.
The other formats are `json`, `xml`, `csv`, `tsv`, and for graphs `ttl`, `nt`, `nq`,
`trig`, `jsonld` and `rdfxml`. `--header 'Name: value'` and `--user NAME[:PASSWORD]` add
credentials, which are refused over plain http to a host other than localhost unless
`--insecure-http` is given. `--default-graph-uri`, `--named-graph-uri` and, for
`rupdate`, `--using-graph-uri` and `--using-named-graph-uri` set the protocol's dataset.

`rdfpatch` reads RDF Patch files, or standard input for `-`, and writes their rows back
in the text form, or in the binary form with `--binary-out`. As Jena's `rdfpatch` does, it
prints the counts of data rows, prefix rows and transaction rows of each file to standard
error. The input form follows the file extension, `.trp` for binary, or `--format`. An
error exits with status 1 and names the line and column, or the row of a binary patch.

### Validating with SHACL and ShEx

`sparkles shacl` and `sparkles shex validate` validate a database or data files on
request, and exit with status 1 when the data does not conform. `sparkles validation`
installs a guard that validates every later write to a database before it commits
([API](API.md#write-time-validation)):

```sh
sparkles shacl --loc db --shapes shapes.ttl                  # a Turtle report; --format json|text
sparkles shacl --data data.ttl --shapes shapes.shaclc --graph union
sparkles shex validate --loc db --schema people.shex --shape-map '{FOCUS a ex:Person}@ex:PersonShape'
sparkles shacl parse shapes.ttl > shapes.shaclc              # Turtle to SHACLC; --out turtle|nt|jsonld|rdfxml
sparkles shex parse people.shex --out shexr > people.ttl     # ShExC to ShExR, ShExJ or ShExC
sparkles validation --loc db --mode reject --shapes shapes.ttl
sparkles validation --loc db --mode warn --schema people.shex --shape-map '{FOCUS a ex:Person}@ex:PersonShape'
sparkles validation --loc db --status                        # the configuration and its counts
sparkles validation --loc db --off
```

Both commands read the default graph unless `--graph` names another, and include the
materialized inferences unless `--no-inferences` is given. `shex validate` also takes a
shape map file (`--map`) or a single node (`--node`), and Jena's flag names as aliases.
`validation --grandfather` blocks only the results a write introduces, so a guard in
`reject` mode can be installed on data that does not conform.

### Drafting shapes from the data

`sparkles schema --draft-shapes` writes SHACL shapes for the classes of a database, with
the cardinalities, node kinds, datatypes, classes, small value sets and languages that
its instances have. Review the draft, then use it for write-time validation:

```sh
sparkles schema --loc db --draft-shapes > shapes.ttl                 # the data conforms
sparkles schema --loc db --draft-shapes --support 0.95 --closed      # rules most instances follow
sparkles schema --loc db --draft-shapes --format shaclc > shapes.shaclc  # the SHACL Compact Syntax
sparkles schema --loc db --draft-shapes --format shexc               # a ShEx schema and its shape map
sparkles validation --loc db --mode warn --shapes shapes.ttl
```

Shapes files may be in any RDF syntax or in the SHACL Compact Syntax. `sparkles shacl`
and `sparkles validation` read a file ending in `.shaclc` or `.shc` as SHACLC, and the
database keeps write-time shapes as Turtle whatever syntax they came in
([API.md](API.md#shacl-compact-syntax-shaclc)).

`sparkles shacl parse FILE…` converts shapes files between syntaxes, like Jena's `shacl
parse`. It checks that each file is well-formed SHACL and prints it in the syntax of
`--out`, which is `shaclc` by default and can also be `turtle`, `nt`, `jsonld` or
`rdfxml`. Several files are printed one after another, each after a `# FILE` line. `-`
reads stdin, which is Turtle unless `--in` names another syntax. A graph with triples that
SHACLC cannot express, such as labels on shapes, fails with `--out shaclc` and names the
triples. A SHACLC file without `BASE` is read without a base IRI, so the output has no
`owl:Ontology` triple for the file's location, and `--base` sets one.

With the default support of 1, every constraint holds for every instance, so the current
data conforms. With `--support 0.95`, a constraint is drafted when 95% of the instances
it applies to satisfy it. The comments in the output say how many instances each
constraint would exclude, and list the candidates that missed the threshold. Turning the
guard on in `warn` mode first shows what later writes would break before anything is
rejected. The server offers the same as `GET /$/schema/{ds}/shapes`, and the UI's schema
browser has a **Draft shapes** action that opens the draft in the dataset page's shapes
editor or installs it as a guard in `warn` mode ([API](API.md#drafted-shapes)).

### Stored queries

A database can keep named queries with typed parameters. Applications and agents then
run them by name instead of building query text, and the values can never change the
query, because each one is bound to its variable as a term.

```sh
cat > adults.rq <<'EOF'
PREFIX ex: <http://ex.org/>
SELECT ?name WHERE { ?p ex:age ?age ; ex:name ?name FILTER(?age >= ?minAge) }
EOF
sparkles queries put --loc db adults --query adults.rq --param minAge:integer=18 \
    --description "People at least minAge years old" --message "first version"
sparkles queries run --loc db adults --set minAge=40
sparkles queries list --loc db
sparkles queries versions --loc db adults
```

On a server, `PUT /$/queries/{ds}/{name}` stores a definition (it needs `admin`), and
`GET /{ds}/queries/{name}?minAge=40` runs it with the caller's permissions, budgets and
rate limits:

```sh
curl -X PUT localhost:3030/$/queries/books/adults -H 'Content-Type: application/json' \
  -d '{"query": "PREFIX ex: <http://ex.org/> SELECT ?name WHERE { ?p ex:age ?age ; ex:name ?name FILTER(?age >= ?minAge) }",
       "parameters": {"minAge": {"type": "integer", "default": 18}}}'
curl 'localhost:3030/books/queries/adults?minAge=40' -H 'Accept: text/csv'
```

Each change is a new version with its time, author and message. The MCP server offers
every stored query as a tool named `<dataset>__<query>`, and the UI's query page lists
them with a form for their parameters. [API.md](API.md#stored-queries) describes the
definitions, the parameter types and the versions.

### DESCRIBE modes

A DESCRIBE query returns the concise bounded description of each resource by default.
That is the resource's triples, the triples of the blank nodes they lead to, and the
descriptions of the reifiers of those triples. It reads the default graph and each named
graph in which the resource appears, as Jena does. A dataset's setting can choose the
symmetric description (`scbd`), which adds the triples that point at the resource, or
only the resource's own triples (`outgoing`). It can also add the labels of linked IRIs
and limit the size and depth of a description:

```sh
sparkles describe-settings --loc db                                  # print the setting
sparkles describe-settings --loc db --set mode=scbd --set labels=true
sparkles describe-settings --loc db --set maxTriples=10000 --set maxDepth=4
sparkles describe-settings --loc db --default                        # back to the defaults
sparkles describe-settings --server URL --dataset db --set mode=outgoing
```

The dataset page of the web UI shows the setting too. An admin of the dataset can change
the mode, the labels and reifiers and the two limits there, or go back to the defaults.

A request can choose another mode with `describe=scbd` and lower the limits with
`describe-max-triples` and `describe-max-depth`. `sparkles query` takes the same options
as `--describe MODE`, `--describe-labels`, `--describe-reifiers`, `--describe-max-triples
N` and `--describe-max-depth N`. They apply to a local query, and with `--server` they
travel to the server as the request parameters. [API.md](API.md#describe) describes the
modes and options.

### GraphQL

A dataset can also answer GraphQL queries, for front-end code and typed clients that do
not speak SPARQL. An administrator installs a mapping schema, GraphQL SDL whose types and
fields name classes and predicates. The server drafts one from the write-time guard's
SHACL shapes or from the data, and nothing is installed until someone reviews and puts it.

```sh
sparkles graphql --loc db schema draft --source observed > schema.graphql
# review schema.graphql: rename types and fields, make fields lists or single values
sparkles graphql --loc db schema put schema.graphql --message "first schema"
echo '{ allPerson(first: 10) { nodes { name knows { name } } } }' | sparkles graphql --loc db run
sparkles graphql --loc db schema get --api
```

A drafted type looks like this. The comments say what each decision rests on, such as a
field drafted as one value because the data never had two.

```graphql
extend schema @prefix(name: "ex", iri: "http://example.org/")

type Person @rdf(iri: "ex:Person") {
  # observed at most one value per instance at commit 12; nothing enforces this
  name: String @rdf(iri: "ex:name")
  knows: [Person!]! @rdf(iri: "ex:knows")
}
```

On a server the same steps are `GET /$/graphql/{ds}/draft`, `PUT /$/graphql/{ds}` (it
needs `admin`) and `POST /{ds}/graphql`. A GraphQL client such as GraphiQL or a code
generator works against `/{ds}/graphql` with introspection:

```sh
curl 'localhost:3030/$/graphql/books/draft?source=observed' > schema.graphql
curl -X PUT 'localhost:3030/$/graphql/books' -H 'Content-Type: application/graphql' \
  --data-binary @schema.graphql
curl localhost:3030/books/graphql -H 'Content-Type: application/json' \
  -d '{"query": "{ allPerson(first: 2, orderBy: [NAME_ASC]) { nodes { name } pageInfo { endCursor } } }"}'
```

A request runs as a fixed number of SPARQL queries, one per level of nested objects and
per group of list fields, whatever the number of nodes, and `explain=true` shows them.
The server bounds each request with `serve --graphql-max-depth` (default 12),
`--graphql-max-nodes` (100,000), `--graphql-default-first` (100, the page size of a list
without `first` or `last`) and `--graphql-max-first` (1,000).
The caller's graph view, protections of triples, budgets and rate limits apply as on
`/{ds}/sparql`, and a grant can be limited to the `graphql` endpoint. Pages of a
connection read the commit of their cursor, so they stay consistent while the data
changes. [API.md](API.md#graphql) describes the directives, the generated schema, the
filters and the errors.

### Loading CSV and TSV

`sparkles load` reads `.csv` and `.tsv` files, compressed or not, next to RDF files, and
loads them all in one commit. Each table is mapped to triples in one of three ways
([spec C05](specs/C05-tabular-imports.md)).

Without options, every row becomes one subject and every column one predicate. `--base`
sets the namespace of both, and `--key` names the column whose value names the row:

```sh
printf 'id,name\n7,Ann\n8,Bob\n' > people.csv
sparkles load --loc db people.csv --base http://ex.org/p/ --key id
# <http://ex.org/p/7> <http://ex.org/p/name> "Ann" .   and so on
```

Without `--key`, a row is named by its position in the file, as
`<file:///…/people.csv#row=2>`, and without `--base` the namespace is the file's URL
followed by `#`. Every value is a plain string, and an empty cell gives no triple.

A CSVW metadata file gives the columns datatypes, null values, list separators and their
own subjects, predicates and objects. It follows the W3C Metadata Vocabulary for Tabular
Data, so other CSVW tools read the same files. `sparkles csv mapping` prints the default
mapping as such a file, which is the easiest way to start one:

```sh
sparkles csv mapping people.csv --base http://ex.org/p/ --key id > people.csv-metadata.json
# edit it: "datatype": "integer", "propertyUrl": "schema:name", "lang": "en", ...
sparkles load --loc db people.csv --mapping people.csv-metadata.json
```

A file named `people.csv-metadata.json` next to `people.csv` is used without
`--mapping`, and the command says so. With `--mapping` and no table file, the tables of
the mapping are loaded from their `url`.

A CONSTRUCT template, as in Tarql, maps each row with SPARQL. Each column is a variable
named after its header, `?ROWNUM` is the row number, and an empty cell leaves its
variable unbound:

```sh
cat > people.rq <<'EOF'
PREFIX schema: <http://schema.org/>
CONSTRUCT { ?person a schema:Person ; schema:name ?name }
WHERE { BIND (IRI(CONCAT("http://ex.org/person/", ?id)) AS ?person) }
EOF
sparkles load --loc db people.csv --template people.rq
```

With `--mapping` as well, the template sees the mapping's typed values. The template
runs against an empty dataset, and `LIMIT`, `ORDER BY`, aggregates, `FROM` and `SERVICE`
are refused, because rows are mapped in batches of 10,000.

A cell that does not match its datatype, or a row with the wrong number of cells, stops
the load with the file, row and column, and nothing is committed. `sparkles csv convert`
writes the triples without loading them, as N-Triples, N-Quads (`--graph`) or Turtle.
`--server` loads into a running server: the CLI converts the tables and sends the
triples. On a server, `POST /{ds}/upload` takes tables too
([API](API.md#csv-and-tsv-uploads)).

## Branches and merges

A branch lets a change take several steps, or several people, before it reaches the data
everyone reads. Every persistent dataset has the branch `main`. A new branch starts from
a commit of another branch and shares that branch's index files until it compacts, so
creating one is cheap at any size. Reads and writes choose a branch with `--branch` on
the command line, `?branch=NAME` over HTTP, or the endpoint URL `/{ds}@{branch}/sparql`.
[API.md](API.md#branches-and-merges) describes the routes, the merge rules and the
conflict report.

```sh
sparkles branch create --loc db dev --note "schema migration"
sparkles update --loc db --branch dev 'INSERT DATA { <urn:a> <urn:p> 1 }'
sparkles query --loc db --branch dev 'SELECT * { ?s ?p ?o }'
sparkles branch list --loc db
sparkles merge --loc db dev                    # into main; exits 2 on conflicts
```

`branch list` prints one line per branch, with its head, the commit it started from,
the commits it is ahead of and behind its upstream, and whether it still shares its
upstream's index (`linked`):

```
branch  head  from     ahead  behind  storage           note
main      61  -            -       -  gen-0004
dev       57  main@42     15      19  linked (2.1 MiB)  schema migration
```

It reads the files when a server holds the database, and then shows heads and starting
points without the ahead and behind counts. `branch create` takes `--from BRANCH` and
`--at REF` to start elsewhere than `main`'s head, and `--protected` to refuse every write
but merges. `branch protect NAME [--off]`, `branch show NAME`, `branch rename NAME NEW` and `branch
delete NAME [--force] [--reparent]` complete the set. A rename keeps the branch's
commits and storage, and the old name stops working. `--reparent` deletes a branch that
other branches start from: they take its upstream, and its storage stays while their
history needs it, and every branch command works against a server with
`--server URL --dataset NAME`.

`merge` prints the result, or the conflicts that stopped it:

```
merge dev (commit 57) into main (commit 61), base main@42
CONFLICT  <http://ex.org/a> <http://ex.org/age>  (default graph)
  base    30
  ours    31   (main)
  theirs  32   (dev)
1 conflict, nothing merged. Resolve with --on-conflict or --resolve FILE.
```

A conflict is a cell, a subject and a predicate in a graph, that both sides changed in
different ways. `--on-conflict ours`, `theirs` or `union` resolves every conflict one way,
and `--resolve FILE.json` reads a JSON array of resolutions for single graphs, subjects or
cells, such as `[{"graph": null, "subject": "<http://ex.org/a>", "predicate":
"<http://ex.org/age>", "take": "objects", "objects": ["33"]}]`. `--expect-source N` and
`--expect-target N` refuse the merge when either head moved since the report was read.
`--exempt IRI`, once per predicate,
keeps both sides' changes to that predicate instead of reporting a conflict, and `branch
exempt --loc db IRI…` sets the dataset's own list of such predicates (`--clear` empties
it). `--conflicts subject` treats a whole subject as one value, and `--conflicts quad` never
reports a conflict. `--replay` makes a fast-forward
that replays the source's commits one by one, each with its kind, message and author.
`--squash` applies the changes as one commit that records no second
parent, so the source stays ahead of the target and keeps its own history to itself.
`--dry-run` shows what the merge would do. The exit status is 0 after
a merge or when the target is already up to date, 2 when conflicts stopped the merge,
and 1 on any other error.

`sparkles revert N` undoes commit N of the `--branch` branch, `main` by default, with
a new commit of kind `revert`. Later changes to the same cells conflict as in a merge,
and `revert` takes the conflict options of `merge` and exits the same way:

```sh
sparkles revert --loc db 57                     # undo commit 57 on main
sparkles revert --loc db --branch dev 61 --on-conflict theirs
```

`sparkles cherry-pick SOURCE N` applies the changes of commit N of branch SOURCE to the
`--branch` branch, `main` by default, as one commit of kind `cherry-pick`. It records no
second parent, so a later merge of SOURCE still brings its other commits:

```sh
sparkles cherry-pick --loc db dev 57             # commit 57 of dev, applied to main
sparkles cherry-pick --loc db --branch qa dev 57
```

`--branch NAME` works with `query`, `update`, `load`, `dump`, `log`, `diff`, `snapshot`,
`compact`, `stats`, `clone`, `patch`, `revert` and `cherry-pick`. With `--server`, these commands send the
dataset's name in the path form, `ds@NAME`. The web UI has a branch menu next to the
dataset's name, and its Branches panel creates, protects, deletes and merges branches.

The panel's Merge button merges a branch without conflicts after a preview. When the
merge has conflicts, the button opens the merge page,
`/ui/datasets/NAME/merge?source=dev&target=main`. The page shows the merge base and the
counts, and it lists the conflicts grouped by graph and subject, with the base, ours and
theirs objects side by side. You choose a side for each row or for all the rows of a
subject, or set a rule for every conflict without a choice. The page then lists the
changes those choices make as signed N-Quads lines. The merge sends the heads the page
showed, so a branch that moved in the meantime makes the page read both branches again
instead of merging. When the target's write-time validation refuses the result, the page
shows the guard's report and nothing is written.

The History panel's Graph button draws the commits of every branch in time order, one
lane per branch, as `git log --graph` does. Each branch's lane starts from the commit
it was created from, and a merge commit links to the commit it merged. Long runs of
commits on one branch fold into a segment that opens on a click. Clicking a commit shows
its changes and a link that opens the query page at that commit.

A branch costs almost nothing while it stays linked. Its memory then holds the upstream's
delta at the starting commit, and its first compaction builds a full index, about what a
clone costs. `--max-branches` (64) limits the branches of a dataset, and the dataset's
storage quota covers every branch.

## Automatic compaction

A server compacts each dataset in the background when its delta of updates grows large,
and writes go on while it does. By default a dataset is compacted when its delta reaches
10,000 quads plus 5% of its base index, or a million quads, or 512 MiB of memory, or when
its write-ahead log passes 1 GiB. A delta of at least 10,000 quads is also compacted
after 5 minutes without a commit, and any change is compacted within a day. The build
runs on a quarter of the cores at a lower priority, and at most one automatic compaction
runs on the server at a time. [API.md](API.md#automatic-compaction) describes when a due
compaction waits and the status at `/$/compaction/{ds}`.

| Flag | Default | Meaning |
|---|---|---|
| `--no-auto-compact` | | Turn automatic compaction off for every dataset. |
| `--auto-compact-min-quads N` | `10000` | The floor. The quad-count and idle triggers need a delta at least this large. |
| `--auto-compact-ratio R` | `0.05` | Compact when the delta reaches the floor plus this share of the base index's quads. |
| `--auto-compact-max-quads N` | `1000000` | Compact at this delta size, whatever the base; `0` means no limit. |
| `--auto-compact-max-delta-mb N` | `512` | Compact when the delta takes about this much memory; `0` means no limit. |
| `--auto-compact-max-wal-mb N` | `1024` | Compact when the write-ahead log passes this size; `0` means no limit. |
| `--auto-compact-idle S` | `300` | Compact a delta of at least the floor after this many seconds without a commit; `0` turns it off. |
| `--auto-compact-max-age S` | `86400` | Compact when the oldest change not yet compacted is this old; `0` turns it off. |
| `--auto-compact-min-interval S` | `60` | Seconds between the end of a compaction and the start of the next automatic one. |
| `--auto-compact-threads N` | a quarter of the cores | Threads of an automatic compaction's build. On Linux they run at nice 10. |
| `--auto-compact-io-mb N` | `0` | The average rate, in MiB per second, at which an automatic compaction may write its new index; `0` means no limit. |
| `--auto-compact-max-running N` | `1` | Automatic compactions that may run on the server at once. |
| `--auto-compact-partial MODE` | `auto` | Whether a compaction, automatic or not, may rewrite only the index blocks its delta touches: `auto`, `off` or `always`. |

A dataset can override every setting but the threads, the rate and the running limit.
The settings are stored in `compaction.json` in its directory:

```sh
sparkles compaction --loc db                                  # the policy, the delta and the verdict
sparkles compaction --loc db --set deltaRatio=0.02 --set idleSeconds=60
sparkles compaction --loc db --set enabled=false              # off for this dataset
sparkles compaction --loc db --default                        # back to the server's settings
sparkles compaction --server URL --dataset db --set maxAgeSeconds=0
curl -X PUT 'localhost:3030/$/compaction/db' -H 'Content-Type: application/json' -d '{"deltaRatio": 0.02}'
```

A database that no server holds is never compacted on its own. `sparkles compact --loc db
--if-due` compacts it only when its policy says so, which suits a cron job.

A compaction whose delta uses only terms the dataset already has rewrites only the index
blocks that the delta touches and copies the others. Numbers, dates, booleans and blank
nodes never add terms, and neither do links between existing resources. A delta that adds
a term, such as a new IRI or string, rebuilds the whole index. The `partial` setting
chooses: `auto` compacts partially when the delta adds no term and that is estimated to
be quicker than a rebuild, `off` always rebuilds, and `always` compacts partially whenever
the delta adds no term. `sparkles compact --partial off` rebuilds once, which also drops terms that no quad
uses any more. The compaction's task message and `last` in its status say which it was.

```sh
sparkles compaction --loc db --set partial=off     # always rebuild this dataset's index
sparkles compact --loc db --partial always         # one partial compaction, if no term was added
```

Each compaction makes the next incremental backup upload the whole new generation, and a
retention window keeps the old generation until its commits age out. Raise the ratio or
the minimum interval for datasets where that costs too much.

## Formatting

`sparkles fmt` formats SPARQL queries and updates (`.rq`, `.ru`, `.sparql`), Turtle
(`.ttl`, `.turtle`), TriG (`.trig`), N-Triples (`.nt`), N-Quads (`.nq`) and JSON-LD
(`.jsonld`). It follows Prettier's modes and exit codes. It exits 0 when everything is
formatted or was written, and 1 when `--check` or `-l` found a file that would change.
It exits 2 on any error: a syntax error, a refused output, an unreadable file or a bad
option. It processes every file, so one run reports every problem, and it formats files
in parallel (`--threads N`).

```sh
sparkles fmt q.rq                    # print the formatted query (stdin to stdout without a path)
sparkles fmt --write queries/        # rewrite in place: temporary file, fsync, rename; unchanged files keep their mtime
sparkles fmt --check queries/        # [warn] per unformatted file on stderr; --diff adds unified diffs on stdout
sparkles fmt -l queries/             # names of the files that would change
sparkles fmt --stdin-filepath queries/q.rq < q.rq   # stdin named for detection, config and ignore files
sparkles fmt --sort data.nq          # N-Triples / N-Quads sorted (external sort past --sort-memory)
sparkles fmt --canonicalize data.nq  # RDFC-1.0 canonical form (drops comments)
```

- **Files.** Directories are walked recursively for the extensions above. The walk
  applies `.gitignore`, `.git/info/exclude` and the global gitignore. It includes hidden
  files but never enters `.git`, `node_modules` or `target`. A file named on the command
  line is formatted whatever its extension. `--language` names its language; otherwise
  the extension does. With an unknown extension, the content decides, and it is never
  detected as N-Triples, because N-Triples is also valid Turtle. Walks skip `.n3`, `.shc`
  and `.json` files, which need `--language` when named. RDF/XML and compressed files are
  refused.
- **Ignore file.** `.sparklesfmtignore` in the current directory, in gitignore syntax, or
  `--ignore-path FILE` (repeatable). It applies to walks and to named files. A named file
  that it matches is skipped silently. An ignored `--stdin-filepath` passes stdin through
  unchanged.
- **Config.** `.sparklesfmt.toml` or `sparklesfmt.toml`, found by walking up from each
  file's directory. The nearest one wins, and files are not merged. Flags override it.
  `--config FILE` uses one file for every input, and `--no-config` uses the defaults. An
  unknown key or a bad value is an error that names the key and the file.

  ```toml
  line-width = 100               # 40..=400
  indent-width = 2               # 1..=8
  prefix-groups = []             # e.g. [["rdf", "rdfs", "xsd", "owl"]]; --prefix-group rdf,rdfs,xsd,owl
  type-shorthand = true          # rdf:type → a (and a entries first)
  compact-iris = true            # full IRI → prefixed name
  quote-style = "double"         # "double" | "preserve"
  operator-position = "leading"  # "leading" | "trailing": where a broken || or && chain puts its operator
  align-values = false           # SPARQL: pad multi-variable VALUES rows into columns
  prune-prefixes = false         # SPARQL, Turtle, TriG: drop prefix declarations nothing uses
  directive-style = "sparql"     # Turtle, TriG: "sparql" (PREFIX, GRAPH) | "turtle" (@prefix, no GRAPH)
  turtle-layout = "diff"         # Turtle, TriG: "diff" | "conventional"
  sort = false                   # Turtle, TriG, JSON-LD terms, N-Triples, N-Quads
  ```

  Every key has a flag of the same name (`--no-type-shorthand`, `--quote-style preserve`, …).
- **Messages.** Errors read `path:LINE:COL: error: …`, with 1-based lines and columns
  counted in characters. A refused output reads
  `path: error: formatter refused its own output (algebra differs); input left unchanged; please report`.
- **Size.** A document is formatted in memory up to `--max-bytes` (default `256MiB`).
  Sizes take a `KiB`, `MiB`, `GiB` or `TiB` suffix. A larger file is an error, with two
  exceptions:
  - N-Triples and N-Quads always stream. A file or stdin of any size is formatted in one
    pass in bounded memory. With `--diff`, they are formatted in memory.
  - Turtle and TriG over `--max-bytes` stream statement by statement unless sorted.
    `--max-bytes` then bounds the largest statement. With `prune-prefixes`, the input is
    read twice, and stdin is kept in a temporary file.

  Sorting N-Triples and N-Quads spills sorted runs to the temporary directory past
  `--sort-memory` (default `1GiB`). `--canonicalize` holds at most
  `--max-canonicalize-quads` quads (default 20,000,000).

`sparkles lsp` serves the same formatting to editors over the Language Server Protocol,
with syntax errors and the formatter's warnings as diagnostics. [editors.md](editors.md)
has the editor setups. `POST /$/format` is the HTTP form ([API.md](API.md#formatting)).
The UI formats in the page when it is built with the formatter's WebAssembly module.

## Linting

`sparkles lint` finds mistakes and doubtful constructs in SPARQL queries and updates
(`.rq`, `.ru`, `.sparql`) and in Turtle (`.ttl`) and TriG (`.trig`) documents. It walks
directories and reads ignore files and config files as `sparkles fmt` does. The design is
in [spec X04](specs/X04-linter.md).

```sh
sparkles lint queries/ shapes/            # path:line:column: severity [rule] message
sparkles lint --fix queries/              # apply the safe fixes in place
sparkles lint --strict --format json .    # warnings fail too; a JSON report
sparkles lint --rule cartesian-product=error --rule single-use-variable=off q.rq
sparkles lint --stdin-filepath q.rq < q.rq
sparkles lint --list-rules                # every rule with its default severity
```

The exit status is 0 when no finding is an error, 1 when one is (or a warning is under
`--strict`), and 2 when a file, flag or config file cannot be used. With `--fix` and
stdin, the fixed text goes to stdout and the remaining findings go to stderr.

| Rule | Default | What it finds |
|---|---|---|
| `syntax` | error | The document does not parse. This rule cannot be turned off. |
| `undefined-prefix` | error | A prefixed name whose prefix is not declared before it. |
| `unused-prefix` | warning | A prefix declaration nothing uses. `--fix` removes it. |
| `unused-variable` | warning | A variable bound by `BIND` or `VALUES` that nothing reads. |
| `single-use-variable` | hint | A variable that occurs once in a pattern and is not projected. It is a wildcard or a typo. |
| `unbound-variable` | warning | A variable that is projected, filtered, ordered or used in a template but never bound. |
| `cartesian-product` | warning | A pattern that shares no variable with the patterns before it in its group. |
| `select-star-group-by` | error | `SELECT *` in a query that groups. |
| `ungrouped-variable` | error | A projected variable that is neither grouped nor aggregated. |
| `filter-scope` | warning | A FILTER in a nested group that tests a variable bound only outside the group, so it removes every row. |
| `filter-equality` | hint | `FILTER(?v = <iri>)` where the IRI could go in the triple pattern. |
| `iri-space` | warning | An IRI, an `IRI("…")` argument or an `xsd:anyURI` that holds whitespace. |
| `language-tag-case` | warning | A language tag not written as BCP 47 recommends (`en-US`). `--fix` rewrites it. |
| `deprecated-language-tag` | warning | A deprecated language tag or subtag, with its replacement. |
| `suspicious-datatype` | warning | An invalid lexical form, an unknown XML Schema datatype, a misspelled XML Schema namespace, or `rdf:langString` as a datatype. |
| `redundant-datatype` | info | `"…"^^xsd:string`, which is the plain string. `--fix` drops the datatype. |
| `deprecated-syntax` | warning | Jena's old `jena.hpl.hp.com` function namespaces, or `owl:DataRange`. |

The SPARQL rules look at variables in their scope. A subquery's variables are its own,
and `MINUS` and `EXISTS` patterns bind nothing outside themselves. A variable whose name
starts with `_` is exempt from the two unused-variable rules. A safe fix never changes
what the document means. `--fix` checks this, and it writes nothing when the fixed text
does not parse to the same algebra, graph or dataset.

The `[lint]` table of `.sparklesfmt.toml` sets the rules' severities for the files under
it, and `--rule` overrides it. The formatter ignores the table.

```toml
[lint]
unused-prefix = "error"
single-use-variable = "off"
```

`sparkles lsp` publishes the same findings in editors and offers the safe fixes as quick
fixes ([editors.md](editors.md)). The UI's query editor lints while you type, and its
Format menu fixes the safe findings. `POST /$/lint` is the HTTP form
([API.md](API.md#linting)).

## Backup repositories

Backup repositories (see [API.md](API.md#backup-repositories)) also work offline, on a
stopped database. A server's own datasets are backed up through its HTTP API or UI, or by
its policies.

In-memory datasets (`--mem`, or `dbType=mem`) are backed up by the server too. They have
no files on disk, so each backup first writes the dataset's current state to a temporary
index generation in `<data>/tmp`. That needs about as much free disk space as the
compacted dataset, on top of the `--min-free-disk-mb` reserve. The copy is removed when
the upload ends. A backup taken this way restores as a new persistent dataset, with the
same content, prefixes, validation and index settings. Each server start gives an
in-memory dataset a new id, and retention keeps `min_count` backups per id. A policy for
such datasets should therefore set `expire_after` with `min_count = 0` (see
[API.md](API.md#lifecycle-policies)).

`--repo` takes either a name from the backup config file or a URL. The config file is
`--backup-config FILE` or `$SPARKLES_BACKUP_CONFIG`, and defaults to
`$XDG_CONFIG_HOME/sparkles/backup.toml`. A URL is `file:///srv/backups/r`,
`s3://bucket/prefix?region=…&endpoint=…&path_style=true&allow_http=true` or `memory://`.
Credentials never go in URLs. They come from the environment or a credentials file.

Only `repo add` and `backup create` initialize an empty location. The other commands
attach to an existing repository. Manifests are cached in
`$XDG_CACHE_HOME/sparkles/backup/`, and progress goes to stderr. Ctrl-C cancels, and a
second Ctrl-C quits. The commands that print a result take `--format json` (or
`--json`). The exit code is 0 on success, 1 on errors, and 2 on warnings only, such as
orphaned blobs in `repo verify`.

```sh
sparkles repo add local --path /srv/backups/r    # edits the config file (mode 0600), initializes, tests
sparkles repo add s3 --s3 kg-backups --prefix prod --region eu-central-1 --credentials env
#   --endpoint URL --path-style --allow-http (MinIO, R2, …); --credentials default | env |
#   env:KEY_VAR,SECRET_VAR[,TOKEN_VAR] | file:PATH; --readonly; --no-init (attach only)
sparkles repo add lab --s3 lab --endpoint http://127.0.0.1:9000 --path-style --allow-http \
  --credentials-name minio --credentials env:MINIO_ACCESS_KEY,MINIO_SECRET_KEY
#   keeps the source as [credentials.minio], which the repository names (without
#   --credentials: uses the one defined); a server reading the file can then name it too
sparkles repo list | show local | test local | remove local   # remove leaves the contents alone
sparkles repo verify local --level data          # every backup, plus orphaned blobs
sparkles repo gc local --dry-run --grace 24h     # delete blobs no backup references
sparkles repo locks local [--break ID]
sparkles backup create  --loc db --repo local [--name N] [--note T] [--dataset NAME]  # refused while a server has db open
sparkles backup list    --repo file:///srv/backups/r [--dataset ds | --dataset-id UUID] [--policy P]
sparkles backup show    --repo local b2
sparkles backup verify  --repo local b2 --level restore   # exists | data | restore; exit 1 on failure
sparkles backup restore --repo local b2 --to /srv/dr/ds [--replace]
sparkles backup restore --repo local b2 --data /srv/sparkles [--as ds]  # into a stopped server
sparkles backup delete  --repo local b1          # blobs go at the next gc
sparkles backup policy list | show P | history P # policies of the config file (run: on a server)
sparkles backup policy preview '30 2 * * *' --tz Europe/Berlin [--count 5]
```

`restore --identity auto|new|keep` picks the dataset id. `auto` keeps the id unless a
dataset in the target data directory already has it. `--check quick|full|none` picks the
integrity check that runs before the restored database is published. `sparkles serve`
holds a lock on `<data>/sparkles-server.lock`, which allows one server per data
directory, and `restore --data` refuses while a server holds it.

A server started with `sparkles serve --backup-config FILE` serves the file's
repositories and policies through its API, read-only. It also accepts repositories
registered through its API and UI (`POST /$/repositories`), within the operator's limits
from that file:

* Their credentials can only name a source defined in the file. The caller cannot choose
  environment variables, files or the instance's default chain.
* Their S3 endpoints go through the [outbound policy](#outbound-requests-service-and-load),
  so a MinIO on localhost needs `--outbound-allow 127.0.0.1` or
  `--outbound-allow-private`. They never go through a proxy from the environment
  (`HTTPS_PROXY`). The config file's repositories and the CLI's do use that proxy.
* `fs` repositories must stay out of the data directory and the config files'
  directories. When `[api] fs_roots` is set, they must lie under it:

```toml
[credentials.minio]              # named by {"source": "named", "name": "minio"}
source = "env"
access_key_id_var = "MINIO_ACCESS_KEY"
secret_access_key_var = "MINIO_SECRET_KEY"

[api]
fs_roots = ["/srv/backups"]
```

To register an S3 repository through the API, first define its credential source in the
file. Write it by hand, or run
`sparkles repo add … --credentials-name minio --credentials …` on the same file. Start the
server with the file, or send it SIGHUP. Then `POST /$/repositories` with
`"credentials": {"source": "named", "name": "minio"}`.

## Outbound requests (SERVICE and LOAD)

`SERVICE <url>` and `LOAD <http…>` make the server open connections, so they follow a
network policy. With authentication on, they also need the `federate` permission. The
local `sparkles query` and `sparkles update` follow the same policy with a different
default, described below. The policy works as follows:

* Only `http` and `https` URLs are allowed.
* The host is resolved once, and the connection goes to exactly the addresses that were
  checked, so DNS rebinding cannot swap in another address. If any address that a name
  resolves to is refused, the name is refused.
* By default, only public addresses are contacted. The policy refuses loopback
  (`127.0.0.0/8`, `::1`), private (`10/8`, `172.16/12`, `192.168/16`), shared
  (`100.64.0.0/10`), link-local (`169.254.0.0/16`, which holds the `169.254.169.254`
  metadata service, and `fe80::/10`), unique-local (`fc00::/7`), multicast, broadcast,
  unspecified, documentation, benchmarking and reserved ranges. It also refuses the
  IPv4-mapped, IPv4-compatible, NAT64 and 6to4 IPv6 forms of those.
* Every redirect hop is checked the same way, for at most 5 hops.
* Each request has a 10 s connect timeout and a total timeout (`--outbound-timeout`,
  default 60 s) that never runs past the query's own timeout. Its response has a ceiling
  (`--outbound-max-mb`, default 256), counted as the body streams in and, for a
  compressed `LOAD`, after decompression.
* One budget covers all the SERVICE calls and LOADs of a query or update, so that many
  requests cannot add up to more than a few large ones. It limits the bytes they receive
  (`--outbound-request-max-mb`, by default 4 × `--outbound-max-mb`), and a compressed
  `LOAD` counts its decompressed bytes. It also limits the time they take, summed
  (`--outbound-request-timeout`, by default 4 × `--outbound-timeout`). Past the byte
  limit, the request fails with `507` (`"budget": "outbound-bytes"`) and an update
  commits nothing. Past the time limit, the next call gets only the time that is left and
  then fails like a timeout.
* A `LOAD` is parsed as its response streams in. It is not buffered first.

A refused destination fails with `403` before any connection is made. `SILENT` hides
neither that refusal nor a spent budget. It hides failures of the remote side, such as
timeouts. Proxy environment variables (`HTTP_PROXY`, …) are ignored for these requests.

Error messages name the URL and its host. They do not say what the host resolved to, and
they do not include the connection's own error. A refused name answers
`… is refused by the outbound policy`, and a failed connection answers `cannot connect`.
The server logs the details under the target `sparkles::outbound`: the resolved address,
its kind and the OS error. An operator can see why a request failed, but a caller cannot
use the errors to map internal names to addresses.

`--outbound-allow-private` opens loopback, private, shared and unique-local addresses,
for example to reach a local Fuseki during development:

```sh
sparkles serve --data ./data --outbound-allow-private
# SELECT * { SERVICE <http://localhost:3030/ds/sparql> { ?s ?p ?o } }
```

Link-local addresses, including the metadata service, stay refused. In production,
prefer an allowlist. With `--outbound-allow` (repeatable), only the listed destinations
are contacted. An entry is one of:

* A host name (`--outbound-allow sparql.example.org`), or `*.example.org` for the
  subdomains of a name. The name is contacted at public addresses, and at private ones
  too with `--outbound-allow-private`.
* An address or CIDR network (`--outbound-allow 10.20.0.0/16`). Any address in it is
  contacted, including private and link-local ones.

Listing a name vouches for the name, not for the addresses it resolves to. To reach a
name that resolves to a private or link-local address, list that address or network as
well (`--outbound-allow fuseki.internal --outbound-allow 10.20.0.0/16`). A hijacked or
mistyped DNS record then cannot open the metadata service. `sparkles mcp` takes the same
flags. Library users set `QueryOptions::outbound`
(`sparkles::outbound::OutboundPolicy`), which has the same defaults. Embedding requests
([Embeddings computed on write](#embeddings-computed-on-write)) follow the server's policy
too, and library users set theirs with `sparkles::vector::embed::set_environment`. The default refusal
of non-public addresses is the constant `BLOCK_PRIVATE_BY_DEFAULT`.

**Local commands.** `sparkles query` and `sparkles update` without `--server` run on the
operator's own machine. They therefore allow loopback, private, shared and unique-local
destinations by default, as `--outbound-allow-private` does for a server. A SERVICE call
to a local Fuseki or a `LOAD` from an intranet host needs no flag. Link-local addresses,
including the metadata service, stay refused. The local commands take the same
`--outbound-*` flags. `--outbound-block-private` restores the strict default of `serve`
and `mcp`, for example for a query from an untrusted source:

```sh
sparkles query --data local.ttl 'SELECT * { SERVICE <http://localhost:3030/ds/sparql> { ?s ?p ?o } }'
sparkles query --data local.ttl --outbound-block-private --query untrusted.rq
```

With `--server`, the request runs on that server under its own policy, and these flags
do not apply.

**Local files.** `LOAD <file:…>` over HTTP needs `serve --load-dir DIR`, and
`server-admin` when authentication is on. The file must be a regular file under `DIR`
once `..` and symbolic links are resolved. A link inside `DIR` may point elsewhere inside
it. Without the flag, the server refuses file loads with `403`. `DIR` may not hold the
data directory. The local `sparkles update` reads any file its user can read. Library
users set `QueryOptions::file_loads` to `FileLoads::Anywhere` (the default),
`FileLoads::under(dir)` or `FileLoads::Disabled`.

### Correlated, bulk and cached SERVICE

A SERVICE that depends on the solutions before it runs once per solution with `loop:` in
front of its IRI, as Jena's service enhancer does. `bulk+n:` sends `n` solutions in one
request, and `cache:` keeps each solution's remote result for the next query:

```sparql
SELECT ?film ?label WHERE {
  ?film a :Film ; :wikidata ?item .
  SERVICE <loop:bulk+50:cache:https://query.wikidata.org/sparql> {
    SELECT ?label { ?item rdfs:label ?label FILTER(lang(?label) = "en") } LIMIT 1
  }
}
```

Without `loop:`, the endpoint evaluates the sub-select once over all of its items, and
every film gets the same label. With `loop:` and no `bulk`, it gets one request per film.
With `bulk+50`, 1,000 films take 20 requests, and with `cache:` a second run takes none.
The requests follow the outbound policy and its budgets like any SERVICE call.

The cache belongs to the dataset, holds `--service-cache-mb` (64), and keeps each caller's
entries apart. `cache+clear:` refreshes the entries a query reads, and
`POST /$/cache/clear/{ds}` drops them all. `SERVICE <loop:> { … }` and
`SERVICE <urn:x-arq:self> { … }` read the dataset itself without a request. The options
and the request shapes are in [API.md](API.md#service-options-loop-bulk-and-cache).

## Finding paths

A property path tells whether two nodes are connected. A path search returns the
connections themselves, one row per edge or per path, through the local service
`SERVICE path:search`. The parameters are in [API.md](API.md#path-search), and the design
is in [F07](specs/F07-path-search.md).

The shortest chain of `foaf:knows` between two people, edge by edge:

```sh
sparkles query --loc ./db '
PREFIX path: <urn:x-sparkles:path#>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
SELECT ?i ?s ?o WHERE {
  SERVICE path:search {
    [] path:source <http://example.org/person/0> ;
       path:target <http://example.org/person/999999> ;
       path:predicate foaf:knows ;
       path:edgeIndex ?i ; path:edgeSubject ?s ; path:edgeObject ?o .
  }
} ORDER BY ?i'
```

The ends can come from the rest of the query. This finds the distance from each of two
people to everyone who works for one organization:

```sparql
SELECT ?a ?b ?len WHERE {
  VALUES ?a { <http://example.org/person/1> <http://example.org/person/2> }
  ?b <http://example.org/worksFor> <http://example.org/org/7> .
  SERVICE path:search {
    [] path:source ?a ; path:target ?b ; path:predicate foaf:knows ; path:length ?len .
  }
}
```

`path:algorithm path:allShortest` returns every shortest path, `path:kShortest` with
`path:k 5` the five shortest, and `path:all` with `path:maxLength 4` every path of up to
four edges. `path:direction path:both` ignores the direction of the triples. Name the
predicates when you can, because a search without them follows every triple, including
`rdf:type`. A search stops with an error when it visits more than 10 million nodes. Raise
the limit with `path:maxVisited`, or narrow the search with `path:maxLength` or
predicates.

A predicate can be followed in its own direction. A family tree stored with
`ex:parent` from child to parent and `ex:sibling` both ways is searched from an
ancestor down to a person with `path:predicate (ex:parent path:backward), (ex:sibling
path:both)`. When the edges are not single triples, a nested pattern can give them.
`path:start` and `path:end` name its variables of each edge's ends, and this search
follows co-authorship, two people who wrote the same paper:

```sparql
SELECT ?len WHERE {
  SERVICE path:search {
    [] path:source <http://example.org/person/1> ; path:target <http://example.org/person/2> ;
       path:start ?x ; path:end ?y ; path:length ?len .
    ?x <http://example.org/authorOf> ?doc . ?y <http://example.org/authorOf> ?doc .
    FILTER(?x != ?y)
  }
}
```

The pattern is evaluated once, before the search, so it suits edges that a predicate
list cannot describe rather than large graphs that predicates can.

Paths are computed per query and nothing is indexed ahead of time. On the 10.5M-triple
benchmark data, the shortest path between two people 17 edges apart takes about 3 ms,
where `ASK { … foaf:knows+ … }` for the same pair takes about 125 ms, because the search
runs from both ends at once ([BENCHMARKS.md](BENCHMARKS.md#path-search)).

## Embeddings computed on write

A vector index can compute its vectors from the dataset's text through an embeddings
endpoint that speaks OpenAI's `POST /v1/embeddings` protocol. That covers OpenAI and most
hosted providers, and also local models served by Ollama, vLLM, LM Studio, llama.cpp's
server or Text Embeddings Inference. Sparkles has no model runtime of its own, so a local
model runs behind one of those servers. The design is in
[F08](specs/F08-embeddings-on-write.md), and the fields and endpoints are in
[API.md](API.md#embeddings-on-write).

With Ollama on the same machine:

```sh
ollama pull nomic-embed-text
sparkles serve --data ./data --outbound-allow 127.0.0.1/32 --outbound-allow api.openai.com
curl -X PUT http://localhost:3030/$/vector/ds/docs -H 'Content-Type: application/json' -d '{
  "predicate": "http://example.org/embedding", "dimension": 768,
  "embedding": {
    "url": "http://127.0.0.1:11434/v1/embeddings", "model": "nomic-embed-text",
    "predicates": ["http://www.w3.org/2000/01/rdf-schema#label"],
    "inputPrefix": "search_document: ", "queryPrefix": "search_query: "
  }
}'
```

The server refuses loopback and private destinations by default, also for embeddings,
so a local provider needs `--outbound-allow` with its address, or
`--outbound-allow-private`. An allowlist limits SERVICE and `LOAD` to the same
destinations, which is why the example lists the hosted endpoint too.

With OpenAI, the key comes from a secret the operator defines. The configuration names
the secret, never the key:

```sh
OPENAI_API_KEY=sk-… sparkles serve --data ./data --embedding-secret openai=env:OPENAI_API_KEY
# "embedding": { "url": "https://api.openai.com/v1/embeddings", "model": "text-embedding-3-small",
#                "apiKey": {"secret": "openai"}, "predicates": ["http://www.w3.org/2000/01/rdf-schema#label"] }
```

`file:PATH` reads the key from a file, such as a systemd credential, each time a request
is made, so a rotated key takes effect without a restart.

After that, writes are embedded in the background. A write commits at once and its
vectors follow in a commit of kind `embed`. The index's card on the dataset page shows
the worker's state, the backlog and the last error, and has a "Re-embed" action.
`sparkles vector status` prints the same as JSON. A search can pass text instead of a
vector:

```sparql
PREFIX spk: <urn:x-sparkles:>  PREFIX ex: <http://example.org/>
SELECT ?s ?score WHERE { (?s ?score) spk:vectorSearch (ex:embedding "rivers of southern France" 10) }
```

**What leaves the machine.** The text of the selected literals, and the texts that
searches pass, go to the configured endpoint, with the model name and, when set, the API
key. Nothing else is sent, and nothing is sent for an index without an `embedding`
object. `--no-embedding` stops all of it on a server. Search texts are sent by any
caller who may read the dataset, which a `queryText: false` index refuses.

**Local commands.** `sparkles update --loc` and the other local commands do not embed.
The next server start, or `sparkles vector embed --loc db`, catches their writes up, and
sends nothing for text that was already embedded. The local commands use the local
outbound policy, which allows private addresses, and take `--embedding-secret` for keys
named by a secret. The Python package does the same with `Dataset.embed()`.

## Checking a database

`sparkles check --loc db` verifies a database directory without modifying it. It takes
no lock, truncates no WAL, repairs no catalog and rebuilds no full-text index, so it can
run next to a server that holds the database. `--data DIR` checks every database in a
server's data directory. Each check prints one line, `ok`, `warning` or `error`, and
lists the file, offset, block, row, commit or id of every problem below it.
`--format json` prints the same report as JSON. The exit status is 0 when everything is
clean, 1 when any check found an error, and 2 when there are warnings only.

| Check | What it verifies |
|---|---|
| `layout` | `CURRENT` names an existing generation. `dataset.json`, the generation's `commit.json` (with the same dataset id) and `prefixes.json` parse. Leftovers of interrupted work are warnings: `*.tmp`, `text.new`/`text.old`, an old or unfinished `gen-NNNN`, or a set-aside catalog. |
| `generation` | `meta.json` (index format) and `stats.json` parse and agree on the quad count. |
| `vocabulary` | The front-coded vocabulary decodes, its keys strictly increase, and its size matches `meta.json`. Every vocabulary id in the permutations is below that size. Every entry of the sparse index `vocab.idx` names the offset and first key of its block. An index that does not read as one for this vocabulary is a warning, because the server ignores it. |
| `delta-vocabulary` | The update vocabulary is well formed and holds no duplicate. A torn tail is a warning. |
| `perm.spo` … `perm.gspo` | Block metadata is contiguous, sorted and fits the file, and the row count matches `meta.json`. Every block decodes to its row count, and its first and last keys match the metadata. Keys strictly increase within and across blocks, and every id is valid for its position. |
| `permutations` | The 7 permutations hold the same number of rows and, compared by an order-independent hash, the same quads. |
| `wal` | Records are well formed, and every commit record's checksum matches. A damaged final transaction is a warning, because open truncates it. So is a final transaction that names update-vocabulary terms the file lacks, which a crash during its commit can leave, and one with zero records before its commit record. Zero bytes after the last commit are preallocated space, reported in the summary. Commit numbers continue from the generation's base commit, and ids resolve. |
| `catalog` | `commits.bin` has valid record checksums, continuous records and the right dataset id, and agrees with the WAL. A lagging catalog, or damage that open can rebuild from the WAL, is a warning. Lost history before the generation is an error. |
| `text` | `text.json` parses. The index opens read-only, and every committed segment file exists and matches its checksum. The index's commit is compared with the WAL. An index that is behind is a warning, because open catches it up or rebuilds it. |
| `geo` | `geo.json` parses and is a valid configuration. The current generation's index files (`geo/rtree.spkg`, `geo/column.spkg`) have a valid header and footer, belong to this generation and configuration, and match their index checksums. In full mode, the data checksums are checked too. A damaged file is a warning, because open rebuilds it. |
| `reasoning` | `reasoning.json` parses and names an existing commit. |

Errors are states that `Store::open` refuses, that lose acknowledged data or history, or
that queries would read wrongly. Warnings are states that open handles by itself. A
server that writes during the check can cause transient warnings, because an in-flight
transaction looks like a torn tail, but never errors.

`--quick` reads metadata only. It reads block metadata instead of every block, the first
key of each vocabulary block, and whether segment files exist instead of their
checksums. On the 10.5M-quad benchmark database, a full check takes about 0.3 s and a
quick one 20 ms (16 cores, warm page cache). The same check is a library call:
`sparkles::check::check(root, &CheckOptions::default())` returns a serializable
`CheckReport`.

## MCP server (LLM agents)

`sparkles mcp` serves databases, or RDF files loaded into memory, over stdin/stdout to
an MCP host such as Claude Desktop, Claude Code or an IDE agent:

```sh
sparkles mcp --loc /data/books                 # a database directory (name: books)
sparkles mcp --loc books=/data/books --loc films=/data/films
sparkles mcp --data a.ttl b.nt --name demo     # files, in one in-memory dataset
```

Configure the host in `claude_desktop_config.json`, or in a project's `.mcp.json` for
Claude Code. For Claude Code, `claude mcp add sparkles -- sparkles mcp --loc /data/books`
does the same:

```json
{
  "mcpServers": {
    "sparkles": { "command": "sparkles", "args": ["mcp", "--loc", "/data/books"] }
  }
}
```

The tools are read-only unless the operator turns on the write tool:

* `list_datasets`, `describe_schema`, `sparql_query`, `explain_query`,
  `describe_resource` and `list_commits`. `describe_schema` with
  `section: "constraints"` lists the SHACL constraints per class, and with
  `subjectClasses: true` it names the classes that use each predicate. With
  `section: "profiles"` it lists the predicates the instances of each class use.
  `describe_resource` with `mode` (`cbd`, `scbd` or `outgoing`) adds the resource's
  DESCRIBE in that mode.
* `find_paths` finds the shortest path, all shortest paths, the k shortest or all paths
  up to a length between two nodes, or from one node, as
  [`SERVICE path:search`](#finding-paths) does, and returns each path's edges.
* `list_changes` lists the recorded changes of a range of commits, each quad added or
  removed with its commit, time, author and message, filtered by subject, predicate,
  object and graph. It answers when a fact changed and who changed it.
* `graphql_query` runs read-only GraphQL on a dataset with a GraphQL schema installed.
  Called without a query, it returns the API schema to write queries against. It is
  listed only while such a dataset exists.
* `draft_shapes` drafts SHACL shapes or a ShEx schema from the data, with the number of
  instances each constraint would exclude.
* `diff_schema` lists the classes and predicates that changed between two commits or
  named snapshots, with each count or declaration before and after.
* Each stored query of a dataset is a tool of its own, `<dataset>__<query>`, whose
  arguments are the query's parameters ([below](#stored-queries)). `--no-stored-queries`
  (or `serve --mcp-no-stored-queries`) leaves them out.
* `search_text` runs BM25 search over a full-text index. `--text` indexes `--data` files.
* `similar_entities` runs exact search over stored `spk:vector` embeddings. It never
  computes embeddings.
* `validate_shacl` and `validate_shex` check a shapes graph, or a ShEx schema with a
  shape map, against a snapshot. They return counts and the first 20 results with node,
  shape and reason. They do not follow imports. `validate_shacl` takes the shapes in
  Turtle, or in SHACLC with `shapesFormat: "shaclc"`.
* `format` formats a SPARQL query or update, Turtle, TriG, N-Triples, N-Quads or JSON-LD
  the way `sparkles fmt` does, and returns the text with any warnings. It reads no
  dataset.
* `sparql_update` runs SPARQL Update, or applies an RDF Patch given as `patch`. It is
  offered only with `--allow-update` (or `serve --mcp-allow-update`), never on a
  read-only server. Writes pass the dataset's write-time validation, the call's
  `message` becomes the commit message, and `LOAD` is refused. `ifHead` makes the write
  conditional. It happens only while that commit is still the head, so an agent that
  read commit 42 does not overwrite a change it has not seen. Hosts that confirm
  destructive tools ask before each call.

Hosts can also attach resources as context: the schema summary
(`sparkles://{ds}/schema`) and the prefixes (`sparkles://{ds}/prefixes`) of each
dataset, and the definition of each stored query (`sparkles://{ds}/queries/{name}`).
The prompts `explore_dataset` and `answer_question` start a session with the tool
workflow and the dataset's prefixes, `run_stored_query` runs a stored query with its
parameters explained, and `explain_term` explains a class, predicate or resource. Hosts
that offer completion suggest dataset names, stored queries and their parameters, named
graphs and prefixes for these arguments, from what the caller may see.

[API.md](API.md#mcp-server) has the tool schemas. Results are sized for a model's
context. Query rows come back as a compact table with the dataset's prefixes, up to 100
rows or 64 KiB by default. Every truncation is announced with the exact total and how to
continue. Data values are escaped so they cannot pass for table structure or status
lines. Every result names the commit it read. Passing that commit back as `atCommit`
keeps a multi-call exploration on one snapshot. The server holds the last 4 commits read
per dataset for 10 minutes, and older commits stay readable while the dataset's
[history](API.md#point-in-time-reads-and-snapshots) keeps them. `at` reads a past state by
commit, time or snapshot name, such as `"at": "time:2026-09-01T00:00:00Z"` or
`"at": "snapshot:release-3"`, and `list_commits` lists the commits and snapshots that
are readable.

A call of a host that supports the tasks extension becomes a task when it runs longer
than 2 seconds (`--task-after-ms`). The host polls for its result and can cancel it.
Hosts that subscribe to changes hear when tools appear or go away, for example when a
stored query is saved, and when a subscribed resource changes.

Every call runs under the query timeout, a memory budget (`--query-memory-mb`, default
2048), the intermediate-row cap and, with `--max-rows-produced`, a cap on the rows all of
a query's operators produce. The timeout is 30 s by default, and `--timeout` is
the maximum. At most `--max-concurrent` calls (4) run at a time. SERVICE is off unless
`--allow-service` is given, because a prompt-injected model could otherwise send data to
any URL. When allowed, SERVICE follows the
[outbound policy](#outbound-requests-service-and-load). `--disable-tool NAME` removes a
tool. A database held by a running `sparkles serve` is refused, because the server holds
its lock. Use the server's HTTP endpoint for such a database. Logs go to stderr, and
stdout carries JSON-RPC only.

### Over HTTP

`sparkles serve --mcp` serves the same tools at `/$/mcp` with the Streamable HTTP
transport, next to the SPARQL endpoints:

```sh
sparkles serve --data ./data --mcp                       # read-only tools
sparkles serve --data ./data --mcp --mcp-allow-update    # plus sparql_update
sparkles serve --data ./data --mcp --mcp-dataset 'wiki*' # only these datasets
```

Each call runs as the HTTP request's caller. With `--auth-config`, an agent sees only
the datasets its credentials may read, and `sparql_update` appears only when they may
write to one of them. Give the agent its own API token, scoped to what it needs:

```sh
sparkles auth token create --name agent --dataset wiki=write --dataset 'docs-*=read'
```

Then point the host at the endpoint with the token as a bearer header. In a project's
`.mcp.json` for Claude Code, `${SPARKLES_TOKEN}` is read from the environment:

```json
{
  "mcpServers": {
    "sparkles": {
      "type": "http",
      "url": "https://sparql.example.org/$/mcp",
      "headers": { "Authorization": "Bearer ${SPARKLES_TOKEN}" }
    }
  }
}
```

The command line does the same with
`claude mcp add --transport http sparkles 'https://sparql.example.org/$/mcp' --header
"Authorization: Bearer $SPARKLES_TOKEN"`. Other hosts take the same URL and header. A
server without auth needs no header, and it listens on loopback only.

A host that only launches stdio servers can reach the endpoint through
`sparkles mcp --url`, which forwards each message to the server. It signs in with the
token that `sparkles auth login` saved for that server, or with `--token` or
`SPARKLES_TOKEN`:

```sh
sparkles auth login --server https://sparql.example.org
claude mcp add sparkles -- sparkles mcp --url https://sparql.example.org
```

This is also the way to give a stdio host a database that a running server holds, since
`sparkles mcp --loc` cannot open it while the server has its lock.

MCP calls follow the server's rules. The rate limits of the `query` and `update` classes
apply per dataset, as for `/{ds}/sparql` and `/{ds}/update`, and the memory budget is the
smaller of `--mcp-query-memory-mb` and `--query-memory-mb`. A call may ask for up to the
server's `--timeout`. Requests from web pages pass the same Origin and Host checks as the
rest of the API. Hosts that still use the older `initialize` handshake get a session,
which belongs to the caller that opened it. [API.md](API.md#http-endpoint-mcp) lists the
flags and the transport details.

## Embedding the library

`crates/sparkles` is a plain Rust library. The CLI (everything except `serve`) and the
server are built on it, so anything they do can be done in-process. A database
directory is locked while it is open, with `sparkles.lock`, like TDB2's `tdb.lock`.

```rust
use sparkles::{Dataset, io::RdfFormat};
use sparkles::querybuilder::{SelectBuilder, UpdateBuilder, expr, lit, var};

let ds = Dataset::open("mydb")?;                 // or Dataset::memory()
ds.load_file("data.ttl.gz")?;                    // parallel bulk path for large inputs

// SPARQL text
let q = "PREFIX foaf: <http://xmlns.com/foaf/0.1/>
         SELECT ?s ?name WHERE { ?s foaf:name ?name } LIMIT 10";
for row in &ds.select(q)? {
    println!("{} {}", row.get("s").unwrap(), row.get("name").unwrap());
}

// fluent builder (jena-querybuilder): typed terms, escaped literals, prepared queries
let adults = SelectBuilder::new()
    .select("?name")
    .where_("?p", "foaf:name", "?name")
    .where_("?p", "foaf:age", "?age")
    .filter(expr::gt(var("age"), 17))
    .order_by("?name")
    .limit(100)
    .execute(&ds)?;
UpdateBuilder::new()
    .insert_data("<http://ex/carol>", "foaf:name", lit(user_input))
    .execute(&ds)?;

// term-level graph access (Jena Graph / DatasetGraph)
let g = ds.default_graph();                       // named_graph(iri), union_graph()
let knows = g.find(Some(&alice), Some(&foaf_knows), None)?;
ds.transaction(|tx| {                              // committed on Ok, rolled back on Err
    tx.remove_triple(&knows[0])?;
    tx.insert_triple(&new_triple)?;
    Ok(())
})?;
ds.dump(std::io::stdout(), RdfFormat::TriG)?;
```

| Jena | Sparkles |
|---|---|
| `TDB2Factory.connectDataset(dir)` / `DatasetFactory.createTxnMem()` | `Dataset::open(dir)` / `Dataset::memory()` |
| `RDFDataMgr.read` / `RDFParser` | `Dataset::load_file`, `load_str`, `load_*_into(graph)`; `sparkles::io` |
| `QueryExecution` / `RDFConnection.query` | `Dataset::query`, `select`, `ask`, `construct` (`QueryOptions` for timeouts, datasets, initial bindings) |
| `UpdateExecution` / `RDFConnection.update` | `Dataset::update` |
| `Graph.find/add/delete/size`, `DatasetGraph.find` | `GraphView::find/insert/remove/len`, `Dataset::find` |
| `Txn.executeWrite` | `Dataset::transaction` |
| `jena-querybuilder` `SelectBuilder` & co. | `sparkles::querybuilder` |
| `QueryExec.substitution` / `setVar` | `QueryOptions::initial_bindings` / builder `set_var` |
| reasoners (`InfModel`) | `sparkles_reasoner::materialize` (crate `sparkles-reasoner`) |
| `ShaclValidator` | `sparkles_shacl::validate` (crate `sparkles-shacl`) |
| `ShexValidator` | `sparkles_shex::validate` (crate `sparkles-shex`) |

Opening a database sets it up as the server does. `Dataset::open` installs the write
guard that the database's `validation.json` configures, loads RDFS on read, and opens
the stored queries, the GraphQL configuration and the reasoning record. When the guard
cannot be installed, for example because the build lacks the `shacl` or `shex` feature,
writes fail with `Error::GuardMissing`, and `Dataset::guard_error` says why.

A dataset's administration goes through handles that `Dataset` returns. Each handle is
cheap to clone and can move to another thread. Settings have `get`, `set` and `reset`,
and long-running calls take a `sparkles::task::Control`, which carries a cancel flag, a
progress callback and a deadline.

```rust
use sparkles::history::{At, SnapshotOptions};
use sparkles::reasoning::rdfs::NewSchema;
use sparkles::task::{Control, Progress};

let (snap, _created) = ds.snapshots().create("before", &At::Head, &SnapshotOptions::default())?;
let diff = ds.history().diff(&At::Snapshot("before".into()), &At::Head, &Default::default())?;
ds.settings().quota().set(2 << 30)?;               // reset() returns to the default
ds.indexes().vector().list();                      // also text() and geo()
ds.queries().run("by-author", &params, &Default::default())?;
ds.reasoning().rdfs().set(NewSchema::Graph("http://ex.org/schema".into()))?;
let ctl = Control { progress: Progress::new(|f, msg| eprintln!("{f:.2} {msg}")), ..Control::none() };
ds.compact_with(&Default::default(), &ctl)?;
```

The handles that need another crate sit behind the `sparkles` crate's Cargo features.
`reasoning`, `shacl`, `shex`, `graphql`, `backup` and `fmt` turn on the reasoner's
calls, the validators, the GraphQL configuration, `ds.backups(&repo)` with
`sparkles::backup`, and `sparkles::fmt`. The feature `full` turns on what the server
has. The default is empty. Some operations of the HTTP API, such as the schema report,
dataset statistics and reasoning runs, have no library call yet.
[Spec P06](specs/P06-library-admin-api.md) lists what is still to come.

`Dataset::store()` and the `store`, `index` and `builder` modules give lower-level
access: ids, snapshots, raw index scans and the bulk `Builder`. `mise run doc` builds
the API documentation of the library crates.

## Python

The `sparkles` Python package embeds the same engine. It is built from
`crates/sparkles-py` with PyO3 and maturin into an abi3 wheel, which works on CPython
3.10 and later. The release workflow builds wheels for Linux (manylinux 2.28 and
musllinux 1.2, on x86_64 and aarch64), macOS (x86_64 and arm64) and Windows x64, and a
source distribution. The package is not on PyPI. Build and install it from the
repository:

```sh
mise run py:build                                  # target/wheels/sparkles_rdf-*.whl
pip install target/wheels/sparkles_rdf-*.whl
pip install ./crates/sparkles-py                   # or build from source with pip (needs Rust)
mise run py:sdist                                  # the source distribution
nix build .#sparkles-py                            # or the flake's package for nixpkgs' Python
```

[Spec P01](specs/P01-python-bindings.md) is the design. The stubs in
`crates/sparkles-py/python/sparkles/_sparkles.pyi` are the API reference, and editors
and type checkers read them.

### Datasets, loading and queries

```python
from sparkles import Dataset, Literal, NamedNode, Quad, Triple, Variable

ds = Dataset()                                     # in memory
ds = Dataset("mydb")                               # a database directory, locked while open
ds.load(path="data.ttl.gz")                        # format and compression from the name
ds.load(text, "turtle", to_graph="http://ex.org/g")
ds.load(open("data.nq.zst", "rb"), "nq")           # read as it is parsed, in one commit
ds.load(path="data.trix")                          # Jena's TriX, RDF Thrift, RDF Protobuf, RDF/JSON
ds.load(path="people.csv", base_iri="http://ex.org/p/", key="id")   # a CSV or TSV table
ds.dump("dump.rt.gz")                              # RDF Thrift, gzipped

rows = ds.query("""
    PREFIX foaf: <http://xmlns.com/foaf/0.1/>
    SELECT ?person ?name WHERE { ?person foaf:name ?name } ORDER BY ?name""")
print(rows.variables)                              # [Variable('person'), Variable('name')]
for row in rows:
    print(row["name"], row[0], row.as_dict())      # by name, by position, as a dict
    person, name = row                             # a row is also a tuple of terms

ds.ask("ASK { ?s ?p ?o }")                         # True or False
for t in ds.construct("CONSTRUCT WHERE { ?s ?p ?o }"):
    print(t.subject, t.predicate, t.object)
ds.update("INSERT DATA { <http://ex.org/a> <http://ex.org/p> 1 }").inserted   # 1
ds.apply_patch(open("changes.rdfp").read()).commit   # an RDF Patch in one commit
ds.apply_patch(open("changes.trp", "rb").read(), binary=True)   # the RDF Thrift form

ds.select(query).serialize("results.srj")          # SPARQL results: json, xml, csv or tsv
ds.construct(query).serialize(format="turtle")     # bytes of Turtle
```

`query` returns a `QuerySolutions` for SELECT, a `bool` for ASK and a `QueryTriples` for
CONSTRUCT and DESCRIBE. `select`, `ask` and `construct` check the query form and raise
`InvalidInputError` on another one. The query methods take these keyword arguments:

* `bindings` pre-binds variables, as in `bindings={"s": NamedNode("http://ex.org/a")}`.
* `default_graph` and `named_graphs` replace the query's dataset, like the SPARQL
  protocol's `default-graph-uri` and `named-graph-uri`.
* `include_inferred=True` adds the reasoner's inferences to the default graph.
* `prefixes` declares prefixes, `base_iri` sets the base, and `timeout` is in seconds.
* `max_rows`, `max_memory_bytes` and `max_rows_produced` are budgets. A query past one
  raises `BudgetExceededError`.
* `cancel` takes a `CancelToken`, and `at` reads a past state (see below).
* `describe` changes how DESCRIBE describes a resource for this query. It takes a mode
  (`"cbd"`, `"scbd"` or `"outgoing"`) or a dict of `mode`, `labels`, `reifiers`,
  `max_triples` and `max_depth`, where `None` removes a limit. The options apply over the
  dataset's setting ([DESCRIBE modes](#describe-modes)). A transaction's `query` takes
  it too.

`update` takes the same `timeout`, budgets and `cancel`. `apply_patch` applies an RDF
Patch from a `str` or `bytes` as the server's patch endpoint does, and returns a
`PatchStats` with the commit and the counts of the rows that took effect. A row is `None` at an unbound
variable, and `row.get("name", default)` returns the default instead.
`QuerySolutions.serialize` must come before the rows are iterated, and it consumes them.

`RdfFormat` names oxrdfio's syntaxes and Jena's TriX (`RdfFormat.TRIX`), RDF Thrift
(`RDF_THRIFT`), RDF Protobuf (`RDF_PROTOBUF`) and RDF/JSON (`RDF_JSON`). `load`, `dump`,
`parse` and `serialize` take all of them, by name, extension or media type as well.
An input in one of Jena's syntaxes streams through the engine's reader on a thread of
its own. `parse` and a file object's `load` take the quads as the reader finds them.
`load` from a path or from bytes writes them as N-Quads to a temporary file in the
system's temporary directory (`TMPDIR`) for the bulk loader and removes the file
afterwards. The reader of RDF/JSON reads its whole document first. RDF/JSON holds one
graph, so `dump` writes the default graph, or `from_graph`.

`load` reads a CSV or TSV table when the format is `"csv"` or `"tsv"`, or the path ends
in `.csv`, `.tsv` or `.tab`, before any compression extension. The table is mapped as
`sparkles load` maps it ([Loading CSV and TSV](#loading-csv-and-tsv)). `base_iri` is the
default mapping's namespace and `key` names the column of each row's subject. `mapping`
names a CSVW metadata file, and `template` a SPARQL CONSTRUCT query run for each row. A
`-metadata.json` file next to the table is used when none of them is given. Warnings of
the conversion are Python warnings.

`sparkles.parse` parses as it reads, from a path, bytes or a file object, so the first
quads come before the input has been read to the end. A syntax error is raised by the
iteration, after the quads that came before it. `Dataset.load` from a file object streams
the same way into one transaction. Loads from a path or from bytes go through the
engine's bulk loader instead.

### Terms

`NamedNode`, `BlankNode`, `Literal`, `Triple`, `Quad`, `DefaultGraph` and `Variable` are
immutable and hashable, and they compare as RDF terms. `str()` gives their N-Triples
form. A `Literal` is made from a string with a `language` or a `datatype`, or from a
Python value:

```python
Literal("chat", language="fr")
Literal("مرحبا", language="ar", direction="rtl")   # an RDF 1.2 directional string
Literal("42", datatype=NamedNode("http://www.w3.org/2001/XMLSchema#integer"))
Literal(42), Literal(1.5), Literal(True), Literal(Decimal("9.99"))
Literal(datetime.date(2024, 1, 2)), Literal(b"bytes")   # xsd:date, xsd:base64Binary
Literal(42).to_python()                            # 42
```

`to_python()` returns a `bool`, `int`, `float`, `Decimal`, `datetime`, `date`, `time` or
`bytes` for the matching XSD datatypes, and the lexical form for anything else. A
`Triple` can be the object of another triple, as an RDF 1.2 triple term.

Every argument that takes a term also accepts rdflib's `URIRef`, `BNode` and `Literal`.
The package recognizes them without importing rdflib. `sparkles.rdflib.to_rdflib` and
`from_rdflib` convert explicitly.

### Quads and transactions

```python
alice = NamedNode("http://ex.org/alice")
ds.add(Quad(alice, NamedNode("http://ex.org/p"), Literal(1)))   # a commit; True if new
ds.extend(triples_or_quads)                       # many quads in one commit
ds.remove(quad)
quad in ds; len(ds); ds.named_graphs()

for q in ds.quads_for_pattern(alice, None, None):  # None matches anything
    print(q.predicate, q.object, q.graph_name)

with ds.transaction() as tx:                       # commits at the end of the block
    tx.add(quad)
    tx.remove(other)
    tx.quads_for_pattern(alice)                    # sees the transaction's own changes
    tx.update("DELETE WHERE { ?s <http://ex.org/old> ?o }")
    tx.query("SELECT ?s WHERE { ?s ?p ?o }")       # also sees them
```

`graph_name=None` matches every graph, and `DefaultGraph()` matches only the default
graph. `quads_for_pattern` reads the snapshot it started on, in batches, so a large match
is never held in memory at once. A transaction rolls back when its block raises, or when
it is garbage-collected without `commit()`. Queries on the dataset read the last commit,
not the open transaction. A write on the dataset from the thread that holds the
transaction raises `ConflictError` instead of waiting for itself. Writes from other
threads wait.

An update in a transaction that fails after it began to change data may have done part
of its work. The transaction is then aborted. Later writes raise `InvalidInputError`, and
`commit()` rolls it back and raises. A SPARQL syntax error changes nothing, so it does
not abort the transaction.

A blank node read from the dataset has a label like `_:b1f` that names the stored node.
It names that node in later patterns, removals, bindings and writes. Any other label,
such as `BlankNode("x")`, names a new node in each write. The same label within one
`extend` or transaction names one node. pyoxigraph keeps a label's node across writes. A
blank node that a query makes, such as with `BNODE()`, has a label like `_:q0`. It is not
stored, so a later pattern finds nothing for it and a query binding takes it for a new
node.

### History, snapshots and clones

```python
ds.head_commit                                     # Commit(seq, kind, inserted, deleted, quads, timestamp)
ds.commits(10)                                     # the latest ten, newest first
ds.create_snapshot("before-cleanup", note="…")     # keeps the head readable under a name
ds.query(q, at="snapshot:before-cleanup")          # also at=42, "commit:42" or "time:<RFC 3339>"
ds.set_retention(keep_commits=100, keep_age=86400) # keep recent states readable
ds.history()                                       # the readable ranges, retention and snapshots
ds.clone_to("copy-db", at="snapshot:before-cleanup")
```

A past state is readable while a snapshot or the retention window holds it. Reading one
that is gone raises `NotFoundError`. An in-memory dataset keeps the states its snapshots
and window hold. `clone_to` writes a new database directory with its own dataset id and
can leave graphs out with `exclude_graphs`.

### Search indexes and write-time validation

```python
ds.enable_text({"predicates": ["http://www.w3.org/2000/01/rdf-schema#label"]})
ds.query("""PREFIX text: <http://jena.apache.org/text#>
            SELECT ?s WHERE { ?s text:query 'fox' }""")
ds.create_vector_index("emb", "http://ex.org/embedding", 384, options={"metric": "cosine"})
ds.vector_index("emb", wait=True)                  # the status once the build is done
ds.create_vector_index("docs", "http://ex.org/docEmb", 768, options={"embedding": {
    "url": "http://127.0.0.1:11434/v1/embeddings", "model": "nomic-embed-text",
    "predicates": ["http://www.w3.org/2000/01/rdf-schema#label"]}})
ds.embed(timeout=600)                              # embed the waiting text on this thread

ds.set_write_validation({"mode": "reject", "shapes": {"inline": shapes_turtle}})
ds.add(quad_that_breaks_the_shapes)                # raises WriteRejectedError
ds.write_validation()                              # the configuration and its status
ds.set_write_validation(None)                      # removes it
```

The configurations and statuses are dicts in the server's JSON shape: `text.json` for
the text index ([API](API.md#full-text-search)), the vector index configuration, and
`validation.json` with the shapes or ShEx schema given inline
([API](API.md#write-time-validation)). A ShEx configuration has `"language": "shex"`,
a `schema` and a `shapeMap`. Opening a database directory installs the write-time
validation its `validation.json` sets up, as the server does. When the configuration
cannot be loaded, the package warns and writes stay refused. `text_status`,
`rebuild_text`, `disable_text`, `drop_vector_index`, `rebuild_vector_index`,
`reembed_vector_index` and `vector_indexes` complete the administration.

### Query builder

`sparkles.querybuilder` has `SelectBuilder`, `AskBuilder`, `ConstructBuilder`,
`DescribeBuilder`, `UpdateBuilder` and `WhereBuilder`, the bindings of the Rust builder.
Each method returns a new builder, so a partly built query serves as a template.

```python
from sparkles.querybuilder import SelectBuilder, WhereBuilder

by_name = (SelectBuilder()
    .select("?age")
    .where_("?p", "foaf:name", "?name")
    .optional(WhereBuilder().where_("?p", "foaf:age", "?age")))
ds.query(by_name.set_var("?name", Literal(user_input)).build())
```

A `str` argument is SPARQL term syntax, such as `"?x"`, `"<http://ex.org/a>"`,
`"foaf:name"` or a property path in predicate position. Terms, numbers and booleans are
values, which are always escaped, so `set_var` with a term makes a prepared query.
Expressions are SPARQL text. `build()` checks the text with the SPARQL parser, and
`str(builder)` returns it unchecked.

### rdflib

The wheel registers an rdflib store plugin, `Sparkles`, which keeps rdflib's triples in
a Sparkles dataset:

```python
import rdflib

g = rdflib.Graph("Sparkles", identifier="http://ex.org/g")
g.open("mydb", create=True)                        # or leave it in memory
g.parse("data.ttl")                                # one commit for the whole parse
g.query("SELECT ?s WHERE { ?s ?p ?o }")            # runs in Sparkles' engine
g.close()

ds = rdflib.Dataset("Sparkles")                    # rdflib's default graph is Sparkles'
store = sparkles.rdflib.SparklesStore(dataset=sparkles_dataset, autocommit=False)
```

`Graph`, `ConjunctiveGraph` and `Dataset` work on it. The store is context-aware,
formula-aware and graph-aware. rdflib's default graph is the dataset's default graph,
and a context named by an IRI is that named graph. A context named by a blank node, such
as a `Graph` made without an identifier, is the named graph
`urn:x-sparkles:rdflib:graph:<label>`, so that SPARQL can name it. N3 formulae are named
graphs under `urn:x-sparkles:rdflib:formula:`, and their triples are left out of the
union of the contexts. Blank nodes keep their rdflib labels for the life of the store
object. Graphs added empty are listed until the store is closed, because Sparkles keeps
no empty graphs. IRIs must be absolute, while rdflib also takes relative ones.

SPARQL queries and updates run in Sparkles' engine, with `initNs` and `initBindings`,
and not in rdflib's evaluator. A prepared query, an update with `initBindings`, and an
update of a graph other than the default graph fall back to rdflib's evaluator over the
store. With `autocommit=True`, the default, every write is committed. Writes are
gathered and committed together before the next read or query through the store,
`commit()` or `close()`, so a parse is one commit. Another handle on the same dataset
sees them after that. With `autocommit=False`, writes go into a transaction that reads
and queries through the store see, and `commit()` or `rollback()` ends it. `close()`
without `commit_pending_transaction=True` rolls it back.

On 100,000 triples, compared with rdflib's in-memory store on the same machine, a parse
into the plugin takes about as long, iterating every triple takes about four times as
long, and a SPARQL join with grouping runs about 40 times faster.

### Errors and threads

Every engine error is a `sparkles.SparklesError`. Most classes also derive from the
built-in exception Python code expects:

| Exception | Also a | Raised for |
|---|---|---|
| `SparqlSyntaxError`, `RdfSyntaxError` | `SyntaxError` (through `ParseError`) | queries, data, rules, shapes or ShEx text that does not parse |
| `InvalidInputError` | `ValueError` | refused arguments, a closed dataset, the wrong query form |
| `UnsupportedError` | `NotImplementedError` | a feature the engine or the build lacks |
| `QueryTimeoutError` | `TimeoutError` | a query past its `timeout` |
| `StorageError`, `DatasetLockedError` | `OSError` | corruption, a full disk, a directory that another process or `Dataset` holds |
| `NotFoundError` | `LookupError` | a missing graph or snapshot, and a past state that is no longer kept |
| `ConflictError` | | write conflicts, and a write that would wait for its own thread's transaction |
| `BudgetExceededError` | | a request past a budget, with `kind`, `limit` and `requested` |
| `CancelledError` | | a request cancelled through its `CancelToken` |

I/O failures raise the built-in `OSError` subclasses, such as `FileNotFoundError`. The
other classes are `PermissionDeniedError`, `ServiceError` and `WriteRejectedError`.

Queries, updates, loads, dumps, reasoning and validation release the GIL, so other
Python threads keep running and several threads can query one dataset at once. Ctrl-C
stops a running query or update and raises `KeyboardInterrupt`. On the main thread a
request runs on a helper thread while the main thread waits and handles signals, which
adds a few microseconds to each request. A `CancelToken` passed as `cancel=` stops the
requests it was given from any thread.

## JVM (Apache Jena)

The `sparkles-jena` library embeds the same engine in a JVM process behind Apache Jena's
own interfaces. `SparklesDatasets.open` returns a `DatasetGraph`, and code written for
TDB2 runs on it after a change to the line that opens the dataset. Jena's `Model`,
RIOT, `Txn`, `QueryExecution` and `UpdateExecution` APIs work on it. Queries and updates
run in Sparkles' planner and executor, and a query that uses a function registered only
in Java runs in ARQ instead.

The library is written in Kotlin and is meant to be used from Java. It needs Java 17 or
later, and it is built and tested against Jena 5.6. Testing with Jena 6, which needs
Java 21, is planned. The native part is the
`crates/sparkles-ffi` crate, which the library calls through UniFFI and JNA. The library
is not published to Maven Central. Build the jar from the repository:

```sh
mise run jvm:build            # jvm/sparkles-jena/build/libs/sparkles-jena-0.1.0.jar
nix build .#sparkles-jena     # or the flake's package, in result/share/java
```

The jar holds the native library for the platform it was built on, and the build has
been tested on Linux x86_64. It extracts the library into the temporary directory on
first use. The system property `sparkles.native.dir` chooses another directory, and
`sparkles.native.path` names a library file to load instead, such as one built for
another platform with `cargo build --release --manifest-path crates/sparkles-ffi/Cargo.toml`.
On Java 24 and later, the JVM warns when JNA loads native code unless the program runs
with `--enable-native-access=ALL-UNNAMED`.

The library depends on `jena-arq`, JNA, the Kotlin standard library and JSpecify. The
TDB2 import also needs `jena-tdb2` on the classpath. [Spec P04](specs/P04-jvm-bindings.md)
is the design, and `jvm/sample-java` is a Java program written against the public API.

### Opening a Jena dataset

```java
import io.github.kclejeune.sparkles.jena.*;

try (DatasetGraphSparkles dsg = SparklesDatasets.open(Path.of("DB"))) {
    Dataset ds = DatasetFactory.wrap(dsg);
    dsg.loadFiles(List.of(Path.of("data.ttl.gz")));         // one bulk commit
    Txn.executeWrite(ds, () -> ds.getDefaultModel().add(s, p, o));
    Txn.executeRead(ds, () -> {
        try (QueryExecution qe = QueryExecution.dataset(ds)
                .query("SELECT ?s WHERE { ?s a <http://ex.org/T> }").build()) {
            ResultSetFormatter.out(qe.execSelect());
        }
    });
    CommitReceipt r = dsg.lastReceipt();                     // the last commit of this thread
}
```

`SparklesDatasets.open(path)` opens or creates a database directory, and
`SparklesDatasets.memory()` makes an in-memory dataset. Two opens of one directory in a
JVM share the native dataset, and its directory lock is released when the last of them
is closed. Another process that has the directory open makes `open` throw
`SparklesDatasetLockedException`.

`SparklesOptions.builder()` sets the options of a dataset, and `SparklesOptions.DEFAULT`
holds the defaults.

| Option | Default | Effect |
|---|---|---|
| `unionDefaultGraph` | false | Queries and updates see the union of the named graphs as their default graph. |
| `fallback` | `AUTO` | `NEVER` makes a query that needs ARQ fail, and `ALWAYS` runs every query in ARQ. |
| `blankNodeLabels` | `DATASET` | `TRANSACTION` scopes the labels that Jena makes to the transaction that writes them. |
| `autocommit` | false | A write outside a transaction commits on its own instead of throwing. |
| `readOnly` | false | Every write fails. |
| `outboundPolicy` | `OPEN` | `SERVER` applies the server's rules to SERVICE and `LOAD`, which refuse private addresses. |
| `termCacheSize` | 262,144 | The most distinct terms that one result keeps decoded at a time. |

### Jena transactions

A dataset has many readers and one writer, as TDB2 does. Jena's `begin`, `commit`,
`abort`, `end` and `promote` keep their meaning. A read transaction reads one commit for
its whole length. A write transaction runs on a worker thread that holds the writer
lock, so a transaction may move between threads, as a virtual thread does. `add` and
`delete` collect their changes in a buffer, which goes to the worker in batches of 4,096
operations, before every read in the transaction, and at the commit.

A write in a `READ_PROMOTE` transaction promotes it. The promotion succeeds only if no
commit has landed since the transaction began, and otherwise the write throws
`JenaTransactionException`. In a `READ_COMMITTED_PROMOTE` transaction, the promotion waits
for the writer lock and continues from the newest commit. A write outside a transaction
throws `JenaTransactionException`, as in TDB2, unless the dataset was opened with
`autocommit(true)`. Each such write is then a commit of its own, which is slow for many
writes.

An iterator from `find` inside a transaction throws `JenaTransactionException` once the
transaction has ended. Outside a transaction, an iterator reads the commit that was the
newest when it was made. Close or exhaust iterators, because an open one keeps its
commit's data alive.

### Jena queries and updates

A query through `QueryExecution` or `QueryExec` runs in Sparkles when the dataset is a
`DatasetGraphSparkles` or a plain wrapper of one. Its solutions come back to Jena in
batches. Jena builds the result of a CONSTRUCT, ASK or DESCRIBE query from those
solutions, as it does for TDB2. Timeouts and `abort()` cancel the native query.

A query runs in ARQ over the dataset's `find()` when it uses something that Sparkles
cannot see. The first case is a function that Jena's `FunctionRegistry` knows and
Sparkles does not, or a `java:` function. The second is a property function or a custom
aggregate that is registered in Jena and unknown to Sparkles. The third is a dataset
description that names `urn:x-arq:UnionGraph` or `urn:x-arq:DefaultGraph`. A query that
Sparkles rejects as a syntax error or as unsupported also runs in ARQ. Functions that
both know, such as `afn:localname` and the `geof:` functions, run in Sparkles. Each
fallback is logged at debug level with its reason, and `dsg.stats()` counts the queries
by where they ran.

An update request through `UpdateExecution` or `UpdateExec` outside a transaction is one
commit. Inside a write transaction it joins the transaction. `INSERT DATA` and
`DELETE DATA` go to the transaction as quad batches. The other operations run in
Sparkles as one request, except an operation that needs Java, which runs in ARQ in its
place in the request. When an operation fails after it began to change data, Sparkles
aborts the transaction, and `commit()` then throws `JenaTransactionException`.

These context symbols apply to a query or update. They can be set in the global context,
in the dataset's `getContext()` or on one execution.

| Symbol | Meaning |
|---|---|
| `Sparkles.UNION_DEFAULT_GRAPH`, TDB2's `tdb2:unionDefaultGraph` | The default graph is the union of the named graphs. |
| `Sparkles.FALLBACK` | The fallback mode, as a `SparklesFallback` or its name. |
| `Sparkles.INCLUDE_INFERRED` | The reasoner's `urn:x-sparkles:inferred` graph is part of the default graph. |
| `Sparkles.MAX_ROWS`, `MAX_MEMORY_BYTES`, `MAX_ROWS_PRODUCED` | The request's budgets. |
| `Sparkles.NO_CACHE` | The request neither reads nor fills the result cache. |
| `ARQ.httpServiceAllowed` | `false` refuses SERVICE. |

The union default graph has TDB2's scope. It changes what queries and updates see as the
default graph, while `getDefaultGraph()` and `find` on the default graph still reach the
stored default graph.

### Loads, receipts and the TDB2 import

`loadFiles(paths)` and `loadFiles(paths, graph)` load files with Sparkles' parsers in one
commit, taking each file's format from its extension. `load(InputStream, Lang)` loads data
from a stream the same way. `bulkSink()` returns a `StreamRDF` for Jena's parsers. It sends
quads in batches of 65,536 to one write transaction and commits at `finish()`. These run
outside a Jena transaction and throw `JenaTransactionException` when the thread is in
one. `lastReceipt()` returns the receipt of the last commit the thread made on the
dataset, and `headCommit()` returns the newest commit.

`SparklesDatasets.importTdb2(tdbDir, sparklesDir)` copies a TDB2 database into an empty
Sparkles database in one commit. It reads the TDB2 database with Jena's own code, so
literals come back as TDB2 stored them, and it copies the prefixes. It returns an
`ImportReport` with the counts, the receipt and the time taken.

### Blank node labels

A stored blank node's label is `b` followed by a hexadecimal number, and that label names
the node in every read and write. A label that Jena made, such as the label of
`NodeFactory.createBlankNode()`, is mapped to the stored node when its transaction
commits. Later transactions find the node by that label, and reads return the label. The
mapping costs about 150 bytes of memory per label and lasts while the dataset is open, so
after a restart the node has its `b…` label. `blankNodeLabels(BlankNodeLabels.TRANSACTION)`
turns the mapping off, which suits programs that write many new blank nodes through `add`.
Bulk loads never add to the mapping.

### Jena exceptions

Each error of the engine becomes the exception that Jena raises in the same situation, and
each also implements `SparklesError`, which gives the engine's kind and message.

| Exception | Base class | Raised for |
|---|---|---|
| `SparklesQueryParseException` | `QueryParseException` | a query or update that Sparkles cannot parse |
| `SparklesRiotException` | `RiotException` | data that does not parse |
| `QueryCancelledException` | | a timeout or a cancelled query |
| `SparklesBudgetExceededException` | `QueryExecException` | a request past a budget, with the budget, limit and amount |
| `SparklesUnsupportedException` | `QueryExecException` | a feature this build lacks |
| `SparklesTransactionException` | `JenaTransactionException` | a write conflict, or a transaction aborted by a failed update |
| `SparklesStorageException`, `SparklesDatasetLockedException` | `JenaException` | corruption, a full disk, a directory open in another process |

The other classes are `SparklesInvalidException`, `SparklesWriteRejectedException`,
`SparklesNotFoundException`, `SparklesNotPermittedException` and
`SparklesInternalException`.

### Differences from TDB2

* TDB2 stores many literals by value, so `"01"^^xsd:integer` reads back as
  `"1"^^xsd:integer`. Sparkles keeps the lexical form a literal was written with. Value
  comparisons and `=` in SPARQL agree on both.
* Sparkles makes no commit for a write transaction that changed nothing, so such a
  transaction does not make a later promotion fail, as it does in TDB2.
* Prefixes are saved at once and outside the write transaction, so a prefix set in a
  transaction that aborts stays.
* A failed SPARQL Update aborts the transaction it ran in. TDB2 keeps the partial changes
  and leaves the decision to the caller.
* Sparkles stores RDF triples only. A triple whose predicate is a blank node or a literal,
  or whose subject is a literal, fails at the next flush of the write buffer.
* Graph listeners see changes made through the `Graph` and `Model` APIs, and not those of
  SPARQL Update in Sparkles or of bulk loads, as in TDB2.
* A query keeps its whole result in native memory while Jena reads it. That memory is
  outside the Java heap and `-Xmx`, and `Sparkles.MAX_MEMORY_BYTES` bounds it.

## Rust client

`crates/sparkles-client` is a client library for a Sparkles server, and for any server
that speaks the SPARQL 1.1 Protocol, such as Fuseki, QLever, Oxigraph or Wikidata. It is
async on tokio and reqwest, with a blocking form for programs without a runtime. It does
not depend on the engine or the server crate. [Spec P02](specs/P02-rust-client.md) is the
design, and `cargo doc -p sparkles-client` builds the API reference.

```toml
[dependencies]
sparkles-client = { git = "https://github.com/kclejeune/sparkles" }
```

### Queries and updates

```rust
use sparkles_client::{At, Client, QueryOptions};

let client = Client::builder("https://sparql.example.org")
    .saved_credentials()                     // the token of `sparkles auth login`
    .build()?;
let ds = client.dataset("library");

let receipt = ds.update("INSERT DATA { <urn:a> <urn:p> 1 }").await?;
println!("commit {:?}", receipt.commit_seq);

let mut rows = ds.select("SELECT ?s ?o WHERE { ?s ?p ?o }").await?;
while let Some(row) = rows.next().await {
    let row = row?;                          // a sparesults::QuerySolution of oxrdf terms
    println!("{:?} {:?}", row.get("s"), row.get("o"));
}

let before = ds
    .query_with("SELECT * { ?s ?p ?o }", &QueryOptions::new().at(At::Commit(41)))
    .await?;
```

`query` returns `QueryResults`, whose variant comes from the response's media type.
`Solutions` stream SELECT results, `Boolean` is an ASK's answer, and `Graph` streams the
triples of a CONSTRUCT or DESCRIBE. `select`, `ask` and `construct` expect one form and
return an error for another. Results are parsed as they arrive, and dropping a stream
closes the connection, which cancels the query on the server. Every result carries
`meta()`, with the commit it read (`Sparkles-Commit`), the state of an `at` read, the
entity tag and the rate-limit fields.

An update returns a `Receipt` with the commit it produced, the commit's counts and the
server's whole JSON answer. `UpdateOptions` sets a commit message, a dry run,
`using-graph-uri` and the server's `timeout`. `ds.batch()` collects updates and quads
for `INSERT DATA` and `DELETE DATA`, and sends them as one request, which commits all of
them or none.

The names follow Jena's `RDFConnection`:

| Jena | Rust client |
|---|---|
| `query`, `querySelect`, `queryAsk`, `queryConstruct` | `query`, `select`, `ask`, `construct` |
| `update` | `update`, `batch().…commit()` |
| `fetch(graph)`, `fetchDataset` | `get_graph`, `get_dataset` |
| `load(graph, file)`, `put`, `delete` | `load`, `load_into`, `post_graph`, `put_graph`, `delete_graph` |
| `loadDataset`, `putDataset` | `post_dataset`, `put_dataset` |
| `RDFConnectionRemote.service(url)` | `Endpoint::new(url)`, `Endpoint::fuseki(base, name)` |

### Graph Store, uploads and the admin API

```rust
use sparkles_client::{RdfBody, UploadOptions, UploadPart, WriteOptions};
use sparkles_client::oxrdf::NamedNode;

let g = NamedNode::new("http://example.org/g")?;
ds.put_graph(g.clone(), RdfBody::file("data.ttl.gz")?).await?;   // sent gzipped
let triples = ds.get_graph(g.clone()).await?;
let etag = triples.meta().etag.clone().unwrap();
// replace the graph only if nobody wrote since the read
ds.put_graph_with(g, RdfBody::file("new.ttl")?, &WriteOptions::new().if_match(etag)).await?;

ds.upload(vec![UploadPart::file("people.csv")?],
          &UploadOptions::new().base("http://example.org/p/").key("id")).await?;
let rows = ds.run_stored("older", &serde_json::json!({ "min": 40 })).await?;

let task = ds.backup_nquads().await?;
client.wait_for_task(&task.id, std::time::Duration::from_secs(1)).await?;
```

`Dataset` also has `commits`, `commit`, `info`, `stats`, `schema`, `backups` and the
stored query calls, and `Client` has `server`, `whoami`, `datasets`, `create_dataset`,
`delete_dataset`, `tasks`, `task` and `cancel_task`. `client.call_json(method, path,
query, body)` reaches any other JSON operation of the API.

### Other SPARQL servers

```rust
use sparkles_client::Endpoint;

let wikidata = Endpoint::new("https://query.wikidata.org/sparql")?;
let answer = wikidata.ask("ASK { wd:Q42 wdt:P31 wd:Q5 }").await?;

let fuseki = Endpoint::fuseki("http://localhost:3030", "ds")?;   // /ds/sparql, /update, /data
```

A plain endpoint gets only the protocol's parameters. Options that only Sparkles
understands, such as `at` or a commit message, are refused with `Error::Config` rather
than sent and ignored. A query goes as a GET while its URL stays under 2,000 bytes, and as
a POST form otherwise.

### Credentials, retries and deadlines

The builder takes `basic_auth(user, password)`, `bearer_token(token)` for API tokens and
OIDC access tokens, `token_source(…)` for tokens that expire and are refreshed after a
`401`, and `saved_credentials()` for the token of `sparkles auth login`. `SPARKLES_TOKEN`
takes precedence over the saved token. `Client::from_env()` picks the server as the
CLI's `--server` does. Credentials are never sent over plain http to a host other than
localhost unless the builder calls `allow_insecure_http()`.

A request is retried, three times by default, when the connection failed, on `429`, and
on `503` with `Retry-After`. In those cases the server did no work, so writes are
retried too. Reads, queries, `PUT` and `DELETE` are also retried after other transport
errors and on `502`, `503` and `504`. The client waits as long as `Retry-After` says, or
until the reset time of an exhausted `RateLimit` field, else it backs off exponentially
with jitter. `RetryPolicy` changes the counts and delays. Each options type takes a
`deadline` for the whole call, a `CancellationToken`, extra headers and `no_retry()`.

### Blocking

```rust
let client = sparkles_client::blocking::Client::new("http://localhost:3030")?;
for row in client.dataset("ds").select("SELECT * { ?s ?p ?o } LIMIT 10")? {
    println!("{:?}", row?);
}
```

The blocking client runs its own tokio runtime with one worker thread, and its results
are iterators. Like reqwest's blocking client, it must not be called from inside an async
runtime. Without the default `blocking` feature, the crate has only the async API.

## Docker

The repository's `Dockerfile` builds an image with the server and its web UI, and
`compose.yaml` runs it with the data in a named volume:

```sh
docker compose up --build -d     # UI at http://localhost:3030/ui/
docker compose logs -f sparkles
docker compose down              # stops and removes the container; the volume stays
```

The build runs in stages. It first builds the formatter's WebAssembly module with the
wasm-bindgen CLI of the version that `Cargo.lock` pins, and then the UI with that module,
as `mise run ui:wasm` and `mise run ui:build` do. Then
`cargo build --release --locked -p sparkles-server` embeds the UI into the binary with the
default features. Node and pnpm are the versions that `mise.toml` pins. Rust is a fixed
stable release named in the `Dockerfile`, because `rust-toolchain.toml` only asks for
"stable". The runtime image is Debian bookworm slim with CA certificates, for outbound
HTTPS to SERVICE endpoints, OIDC providers, S3 and embedding providers. The server runs
in it as the unprivileged user `sparkles` (uid and gid 10001). The image is about 185 MB,
or 64 MB compressed, of which the binary takes 89 MB. The workspace's release profile
keeps line tables for profiling, which would make the binary 450 MB, so the image strips
them and keeps the symbol table that names functions in backtraces.
`--build-arg KEEP_DEBUGINFO=1` keeps the line tables.

A first build took 17 minutes on a 16-core machine that was busy with other work, with
8 compile jobs. The server's compilation took 11 of those minutes. BuildKit cache mounts
keep the cargo registry, the pnpm store and the cargo target directories, so a rebuild
compiles only the crates that changed. On the same machine, a rebuild without changes
took 14 seconds, and one after a change to the server crate took 5 minutes.
`SPARKLES_BUILD_JOBS=4 docker compose build` caps the parallel compile jobs on a busy
machine, and `mise run docker:build` runs the same build. The cache mounts live in
Docker's build cache, about 3.5 GB after a build, and `docker builder prune` removes
them.

### Network exposure in a container

A published port cannot reach a loopback listener inside a container, so the image runs
`sparkles serve --host 0.0.0.0`. Without `--auth-config`, the server refuses to start on
that address unless `SPARKLES_ALLOW_OPEN_NETWORK=1` is set, as described in
[Network exposure](#network-exposure). `compose.yaml` sets the variable and publishes the
port on the host's loopback address only, as `127.0.0.1:3030:3030`. The server is then
reachable from the host as a server listening on the host's loopback address would be,
and not from the network. Other users of the host and other containers on the same
Docker network can still reach it. `SPARKLES_PORT=8080 docker compose up -d` publishes it
on another host port.

Browsers reach the server as `localhost` or `127.0.0.1`, which an open server answers,
so the UI works through the published port without further flags. The host and origin
checks of an open server stay on. Requests usually arrive from the address of Docker's
bridge gateway rather than the client's, so limits per client address
(`--rate-limit`) count every client as one.

Without Compose, the same container runs like this. Without the variable or
`--auth-config`, it exits at once with the server's explanation.

```sh
docker build -t sparkles .
docker run -d --name sparkles -p 127.0.0.1:3030:3030 -v sparkles-data:/data \
  -e SPARKLES_ALLOW_OPEN_NETWORK=1 --stop-timeout 30 sparkles
```

### Authentication in a container

Turn on authentication before you publish the port beyond the host's loopback address,
or when you do not trust every user and container on the host. Write an auth
configuration as described in [API.md](API.md#configuration). The image hashes passwords
for it:

```sh
docker compose run --rm -T sparkles auth hash < password.txt
```

Then mount the file, pass `--auth-config` and remove `SPARKLES_ALLOW_OPEN_NETWORK`.
`compose.yaml` has the lines for both commented out. With them in place, the service
reads:

```yaml
    command: [serve, --data, /data, --host, 0.0.0.0, --port, "3030",
              --log-format, "${SPARKLES_LOG_FORMAT:-text}",
              --auth-config, /etc/sparkles/auth.toml]
    volumes:
      - sparkles-data:/data
      - ./auth.toml:/etc/sparkles/auth.toml:ro
```

The file must be readable by uid 10001, and it should not be readable by others, so
`chown 10001:10001 auth.toml && chmod 600 auth.toml` on the host. With authentication on,
`ports` can publish the port on every interface, as `"3030:3030"` does. Serve it over
HTTPS, through a reverse proxy in front of the container or with `--tls-cert` and
`--tls-key` on mounted files ([TLS](#tls)), because plain HTTP sends passwords and tokens
in the clear. The image's health check runs `sparkles ping`, which asks for `/$/ready`
over plain HTTP and then over HTTPS, so it works with `--tls-cert` as well. On the
loopback address it accepts the server's certificate without checking the name, since
the certificate names the public host. `SPARKLES_HEALTHCHECK_PORT` sets the port when
`serve` listens on another one than 3030, and `SPARKLES_HEALTHCHECK_URL` sets the whole
target, such as `https://127.0.0.1:8443`.

### Configuring the container

Other `serve` flags go into the `command` list of `compose.yaml`. Files that a flag
names, such as a `--text` or `--validate` configuration or a `--load-dir` directory, are
mounted into the container like the auth configuration. The compose file reads a few
variables from the environment or from an `.env` file next to it:

| Variable | Default | Meaning |
|---|---|---|
| `SPARKLES_PORT` | `3030` | The host port, on `127.0.0.1`. |
| `RUST_LOG` | the server's default filter | The log filter. |
| `SPARKLES_LOG_FORMAT` | `text` | `text` or `json`, passed as `--log-format`. |
| `SPARKLES_BUILD_JOBS` | all cores | Parallel compile jobs of the image build. |

The server logs to stderr, which `docker compose logs` shows. The container's health
check asks for `GET /$/ready` every 30 seconds, and every 2 seconds during the first five
minutes until the first success, so that a large database can open first. On
`docker compose down` or `docker stop`, the server gets SIGTERM and finishes the
requests in flight within `--shutdown-grace` (20 seconds). `compose.yaml` waits 30
seconds before it kills the server.

Request bodies are spooled to `/tmp` in the container's writable layer. For uploads of
several gigabytes, mount a volume at `/tmp`. The commented `deploy.resources` block of
`compose.yaml` limits CPU and memory. Keep `--query-memory-mb`, `--max-mem-dataset-mb`
and `--vector-memory-mb` below the memory limit, so that a budget refuses a large request
before the kernel ends the container.

Commands other than `serve` run in the same image. The CLI can talk to the running
server from inside its container:

```sh
docker compose exec sparkles sparkles query --server http://localhost:3030 --dataset films \
  'SELECT (COUNT(*) AS ?n) { ?s ?p ?o }'
docker compose run --rm sparkles help    # a new container with the same image and volume
```

### Upgrading the container

```sh
git pull
docker compose up --build -d
```

Compose rebuilds the image and replaces the container, and the volume keeps the data.
Sparkles is experimental, and its on-disk format may change between commits without a
migration path, so take a backup before you upgrade.

### Backing up the container's data

The `sparkles-data` volume holds the dataset registry, the databases and the server's
own state. Docker names it after the Compose project, such as `sparkles_sparkles-data`.
Backup repositories ([Backup repositories](#backup-repositories)) work in the container
as on a host. To back up to a directory of the host, mount it and a backup configuration
that names it:

```toml
version = 1

[repositories.local]
type = "fs"
path = "/backups"
```

```yaml
    environment:
      SPARKLES_BACKUP_CONFIG: /etc/sparkles/backup.toml
    volumes:
      - sparkles-data:/data
      - ./backups:/backups
      - ./backup.toml:/etc/sparkles/backup.toml:ro
```

The host directory must be writable by uid 10001 (`chown 10001:10001 backups`). Backups
are then taken through the UI, the API or the configuration's policies, and an S3
repository works the same way without the mount. Copying the volume's files is a
consistent backup only while the server is stopped:

```sh
docker compose stop
docker run --rm -v sparkles_sparkles-data:/data:ro -v "$PWD":/out debian:bookworm-slim \
  tar -C /data -czf /out/sparkles-data.tar.gz .
docker compose start
```

## Deploying on NixOS

The flake's `nixosModules.default` provides `services.sparkles`, which runs the server
as a hardened systemd service. [DEVELOPMENT.md](DEVELOPMENT.md#nix) lists the packages
and other flake outputs. The service keeps its state in `/var/lib/sparkles`: the dataset
registry, databases created from the UI or admin API, and backups. The module can put an
nginx virtual host in front of the server, which you then extend through the usual
`services.nginx.virtualHosts.<name>` options:

```nix
{
  inputs.sparkles.url = "github:kclejeune/sparkles";
  outputs = { nixpkgs, sparkles, ... }: {
    nixosConfigurations.host = nixpkgs.lib.nixosSystem {
      modules = [
        sparkles.nixosModules.default
        {
          services.sparkles = {
            enable = true;
            datasets = {
              wiki = { };                       # persistent: /var/lib/sparkles/declarative/wiki
              scratch.type = "mem";
              archive.path = "/srv/rdf/archive"; # an existing database directory
            };
            queryTimeout = 120;
            resultCacheMb = 1024;
            # users, tokens, OIDC (docs/API.md); a secret, never in the Nix store
            auth.configFile = "/run/secrets/sparkles-auth.toml";
            # readOnly = true; allowService = false; unionDefaultGraph = true;
            nginx = {
              enable = true;
              virtualHost = "sparql.example.org";
            };
          };
          # standard nginx semantics: TLS, extra locations …
          services.nginx.virtualHosts."sparql.example.org" = {
            enableACME = true;
            forceSSL = true;
          };
          security.acme.acceptTerms = true;
          security.acme.defaults.email = "admin@example.org";
        }
      ];
    };
  };
}
```

The server listens on `127.0.0.1:3030` by default (`listenAddress`, `port`,
`openFirewall`). Any other `listenAddress` needs `auth.configFile` or
`allowOpenNetwork = true`, and an assertion checks this. The nginx location sets:

* `client_max_body_size` to `nginx.clientMaxBodySize` (default 4g), for bulk uploads;
* proxy timeouts to `queryTimeout + 30` seconds;
* request/response buffering off, so large uploads and results stream through;
* `X-Forwarded-For` to the client's address (`$remote_addr`, replacing whatever the
  client sent), and `Forwarded` to nothing.

The server always trusts nginx for `X-Forwarded-For`, whether or not `rateLimits` is
set. It passes `--rate-limit-trusted-proxy` for 127.0.0.1 and ::1, or `unix` with
`unixSocket`. Failed logins and rate limits therefore count each client rather than
nginx. Behind another proxy or CDN, set up nginx's realip module so that `$remote_addr`
is the client.

`loadDir` passes `--load-dir`, so `LOAD <file:…>` over HTTP may read from that
directory only. The service gets the directory read-only. It must not contain `dataDir`
or lie under `/tmp`.

`mcp.enable = true` passes `--mcp` and serves the [MCP tools](#over-http) at `/$/mcp`.
`mcp.allowUpdate` passes `--mcp-allow-update`, and `mcp.datasets` passes one
`--mcp-dataset` per name or pattern. Other `--mcp-*` flags go in `extraArgs`.

`metrics.fusekiNames = true` passes `--metrics-fuseki-names`, and
`metrics.listenAddress = "127.0.0.1:9464"` passes `--metrics-addr` for a scrape port of
its own. The firewall is not opened for that port.

With `auth.configFile`, the service starts with `--auth-config`, and
`systemctl reload sparkles` re-reads the file (SIGHUP). Keep the file out of the Nix
store (agenix, sops-nix), owned by the `sparkles` user. Do not also set nginx
`basicAuthFile`. nginx would forward its own `Authorization` header, which Sparkles
would then reject. `unixSocket` makes the server listen on a Unix socket that nginx
proxies to, so trusted proxy headers can be limited to it (`proxy.trusted = ["unix"]`).

`tls.certFile` and `tls.keyFile` pass `--tls-cert` and `--tls-key` for a server that
serves HTTPS itself. `systemctl reload sparkles` re-reads them, and the server also
notices when they change. The service user must be able to read both files. For a
certificate from `security.acme`, add the user to the certificate's group, for example
`users.users.sparkles.extraGroups = [ "acme" ]`. With `nginx.enable` as well, nginx
connects to the server over https. The key file must lie outside the Nix store.

For backup repositories, `backup.configFile` passes `--backup-config`. Like
`auth.configFile`, it stays out of the Nix store, and `systemctl reload sparkles`
re-reads it. `backup.maxTasks` passes `--backup-max-tasks`. `backup.fsRoots` lists the
directories of `fs` repositories. The module creates them for the service user and makes
them writable, while the service sees the rest of the file system read-only:

```nix
services.sparkles.backup = {
  configFile = "/run/secrets/sparkles-backup.toml";  # [repositories.local] path = "/srv/backups/sparkles/local"
  fsRoots = [ "/srv/backups/sparkles" ];             # also [api] fs_roots, for API registrations
  maxTasks = 1;
};
```

`maxTasks` and `maxClones` pass `--max-tasks` and `--max-clones`. The options under
`compaction.auto` set the server-wide [automatic compaction](#automatic-compaction)
policy. `compaction.auto.enable = false` passes `--no-auto-compact`. Each of the others
passes its `--auto-compact-*` flag when set, and leaves the server's default when `null`.
The options are `minQuads`, `ratio`, `maxQuads`, `maxDeltaMb`, `maxWalMb`, `idleSeconds`,
`maxAgeSeconds`, `minIntervalSeconds`, `threads`, `ioMb` and `maxRunning`. A dataset's
own settings, made with `PUT /$/compaction/{ds}`, still override them:

```nix
services.sparkles = {
  maxClones = 1;
  compaction.auto = {
    minQuads = 50000;
    maxWalMb = 4096;
    idleSeconds = 600;
    threads = 2;
  };
};
```

When the service stops, requests in flight get `shutdownGrace` seconds (default 20) to
finish before they are cancelled. The unit's `TimeoutStopSec` is `shutdownGrace + 15`,
which leaves time for the cancelled requests to stop and for the final flush.

The CLI goes on the system path unless `installCli = false`. The server holds a lock on
its databases, so stop the service before offline work such as `sparkles load` or
`compact`, or use the HTTP API instead.
