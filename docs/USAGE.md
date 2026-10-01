# Usage

The operator's guide to the `sparkles` binary: running the server, the command-line tools,
the formatter, backups, outbound requests, integrity checks, the MCP server, embedding the
library and deploying on NixOS. The HTTP API is specified in [API.md](API.md); building
from source and testing are in [DEVELOPMENT.md](DEVELOPMENT.md).

Sparkles is experimental: the on-disk format, HTTP API and CLI may change between commits
without a migration path, so keep backups of anything you cannot regenerate.

* [Running the server](#running-the-server)
  * [Network exposure](#network-exposure)
  * [Endpoints and operations](#endpoints-and-operations)
  * [`serve` options](#serve-options)
* [Command-line tools](#command-line-tools)
* [Formatting](#formatting)
* [Backup repositories](#backup-repositories)
* [Outbound requests (SERVICE and LOAD)](#outbound-requests-service-and-load)
* [Checking a database](#checking-a-database)
* [MCP server (LLM agents)](#mcp-server-llm-agents)
* [Embedding the library](#embedding-the-library)
* [Deploying on NixOS](#deploying-on-nixos)

## Running the server

```sh
sparkles serve --data ./data --port 3030   # UI at http://localhost:3030/ui/
```

The server and CLI use [mimalloc](https://github.com/microsoft/mimalloc) as their
allocator (the default `mimalloc` cargo feature of `sparkles-server`; the `sparkles`
library leaves the choice to its embedder). Once no request has been active for
`--idle-release-ms` (default 1000 ms), `sparkles serve` hands free heap memory back to
the OS. Built with `--no-default-features --features reasoning,shacl` it uses the
system allocator and `malloc_trim` instead.

### Network exposure

`serve` listens on `127.0.0.1` by default. It refuses to start on a non-loopback `--host`
(such as `0.0.0.0`) without `--auth-config`, because without authentication every caller
may read, write and administer every dataset. `--allow-open-network` (or
`SPARKLES_ALLOW_OPEN_NETWORK=1`) serves it open anyway and logs a warning. A network
listener without request rate limits (`query`, `update` or `admin`) also logs a warning,
with or without auth. An authenticating reverse proxy in front does not make an open
backend safe: bind the backend to loopback or a Unix socket (`--unix-socket`), or
firewall it, so that nothing can bypass the proxy.

A server without `--auth-config` also guards against the web pages its operator opens:

* it answers only requests whose `Host` is an IP address, `localhost` (or
  `*.localhost`), `--host` or a `--public-host` name; anything else gets `421`, which
  stops a page that rebinds its own DNS name to the server;
* it refuses unsafe requests and anything that writes or administers from another site
  (`403 cross-origin request refused`, by `Origin` and `Sec-Fetch-Site`);
* it sends no CORS headers unless `--cors-origin` names an origin.

The UI served by the server itself, the CLI and other non-browser clients are
unaffected. Behind a reverse proxy, pass the name the proxy is reached by with
`--public-host` (the NixOS module does this for its nginx virtual host).

Every response carries `X-Content-Type-Options: nosniff` and `X-Frame-Options: DENY`.
The UI's pages have a Content Security Policy that allows scripts only from the UI
itself (its inline start-up scripts by hash) and no framing. Their `script-src` also has
`'wasm-unsafe-eval'` so the page can compile the formatter's WebAssembly module; that
permits WebAssembly compilation only, not JavaScript's `eval`. API responses have
`Content-Security-Policy: default-src 'none'; frame-ancestors 'none'`.

### Endpoints and operations

Fuseki-style endpoints for a dataset `ds`: `/ds/sparql`, `/ds/update`, `/ds/data` (GSP),
`/ds/upload`, plus `/$/datasets`, `/$/stats/ds`, `/$/compact/ds`, `/$/backup/ds`, `/$/tasks`
(see [API.md](API.md)). `--mem NAME` adds an in-memory dataset; `--loc NAME=PATH` serves an
existing database.

Operations: `/$/ping` is the liveness check and `/$/ready` the readiness check (`503`
once shutdown starts on SIGINT or SIGTERM); `/$/metrics` serves Prometheus metrics.
Every response carries an `X-Request-Id`, and each request is logged once under the
`sparkles::access` target.

### `serve` options

| Flag | Default | Meaning |
|---|---|---|
| `--host ADDR` | `127.0.0.1` | listen address; a non-loopback address needs `--auth-config` or `--allow-open-network` |
| `--allow-open-network` | off | serve without `--auth-config` on a non-loopback address (also `SPARKLES_ALLOW_OPEN_NETWORK=1`); logged as a warning |
| `--public-host NAME` | | a host name clients reach the server by, such as a reverse proxy's (repeatable); without `--auth-config`, names other than IP addresses, `localhost` and `--host` are refused with `421`, and with it so are requests carrying trusted proxy headers from loopback or the Unix socket |
| `--cors-origin ORIGIN` | none | a browser origin (`https://yasgui.example`) whose pages may call the API cross-origin, without credentials (repeatable; with `--auth-config`, added to `cors.origins`); without auth such a page may do everything the server allows |
| `--timeout S` | `60` | default query timeout in seconds (`timeout=` per request) |
| `--update-timeout S` | `0` | default SPARQL update timeout in seconds (`0`: none; `timeout=` per request); a timed-out update changes nothing |
| `--max-timeout S` | `1800` | largest `timeout=` a query or update may ask for (`0`: unlimited; never below `--timeout` / `--update-timeout`) |
| `--query-memory-mb N` | `8192` | budget for the estimated memory of a query's intermediate results (`0`: unlimited) |
| `--max-result-mb N` | `1024` | budget for the body of a SPARQL query response (`0`: unlimited) |
| `--max-export-mb N` | `0` | budget for the body of a Graph Store GET, i.e. a graph or whole-dataset export (`0`: unlimited) |
| `--max-rows N` | `200000000` | rows of any intermediate result |
| `--max-query-body-mb N` | `16` | largest SPARQL query body (also explain and `/shacl` shapes); `413` past it (`0`: unlimited) |
| `--max-update-body-mb N` | `256` | largest SPARQL update body; bulk data goes through the Graph Store or `/upload` (`0`: unlimited) |
| `--max-admin-body-mb N` | `16` | largest `/$/…` or prefix-change body (`0`: unlimited); `/$/auth/*` bodies are capped at 64 KiB |
| `--max-upload-mb N` | `4096` | largest Graph Store write or upload body, streamed to a temporary file and counted after HTTP decompression (`0`: unlimited) |
| `--min-free-disk-mb N` | `1024` | refuse (`507`) to spool a request body once the temporary directory's file system would keep less free, and to commit, rebuild, clone or write an N-Quads backup (`/$/backup`) once the data directory's would (`0`: no check) |
| `--max-mem-dataset-mb N` | `4096` | largest in-memory dataset; a commit that would grow one past it fails with `507` (`0`: unlimited) |
| `--max-tasks N` | `4` | background tasks (compaction, clones, reasoning, full-text and spatial index builds, N-Quads backups) running at once; more wait `queued` (`0`: no limit) |
| `--backup-config FILE` | | backup repositories, policies, credential sources and the limits of repositories registered through the API (TOML, also `$SPARKLES_BACKUP_CONFIG`; re-read on SIGHUP; read-only through the API) |
| `--backup-max-tasks N` | `2` | backup, restore, verify and GC tasks running at once; more wait `queued` |
| `--format-endpoint on\|authenticated\|off` | `on` | who may use `POST /$/format` (see [API.md](API.md#formatting)): every caller the server admits, every caller but the anonymous principal (`401`), or nobody (`404`); a UI built with the formatter's WebAssembly module formats in the page and needs the endpoint only as a fallback |
| `--format-max-mb N` | `16` | largest `POST /$/format` body (`0`: unlimited) |
| `--format-timeout S` | `10` | seconds a `POST /$/format` request may take, waiting for a free slot (one per core) included; `408` past it |
| `--vector-memory-mb N` | `4096` | memory for the packed vectors of `spk:vectorSearch`, per index generation |
| `--text NAME[=FILE]` | | enable full-text search for a dataset (with a `text.json`-shaped configuration file) |
| `--geo NAME[=FILE]` | | enable the spatial index for a dataset (with a `geo.json`-shaped configuration file); the build runs before the server starts listening |
| `--geo-mb N` | `4096` | memory for each dataset's spatial index (geometry column and trees); a build that would exceed it is refused, the status says `over-budget`, and queries run without the index |
| `--geo-op-vertices N` | `2000000` | largest sum of input vertices of one geometry operation (overlay, buffer, hull, relate); larger ones are a type error |
| `--log-format text\|json` | `text` | log format on stderr (global flag); `RUST_LOG` filters as usual |
| `--no-access-log` | | no per-request log lines |
| `--no-metrics` | | `/$/metrics` answers `404` and no request metrics are kept |
| `--metrics-max-datasets N` | `100` | datasets with their own metric labels (the rest share `$other`) |
| `--otel` | off | export traces and metrics over OTLP (also enabled by `OTEL_EXPORTER_OTLP_ENDPOINT`; the standard `OTEL_*` variables apply, see [API.md](API.md), OpenTelemetry) |
| `--otel-logs` | off | export log events over OTLP too |
| `--otel-query-text` | off | record query text (`db.query.text`) and plan operator descriptions in spans; they may hold data |
| `--otel-plan-spans` | off | one span per executed plan operator |
| `--rate-limit SPEC` | off (`preauth=30/min,burst=60` with `--auth-config`) | limit a request class per client, e.g. `query=100/s,burst=200,concurrency=64` or `auth=10/min,burst=5`; `preauth=…` limits authentication failures per address before credentials are checked (repeatable; see [API.md](API.md), Rate limiting) |
| `--rate-limit-config FILE` | | JSON rate-limit configuration, re-read on SIGHUP; `--rate-limit` applies on top |
| `--rate-limit-trusted-proxy CIDR` | | proxy whose `X-Forwarded-For` names the client (repeatable; `unix`: the `--unix-socket`); limits by address need a peer address clients cannot choose, so list only proxies that overwrite or append to the header |
| `--rate-limit-trusted-proxy-header H` | `x-forwarded-for` | the one header trusted proxies name the client in: `x-forwarded-for` or `forwarded` (RFC 7239); the other is ignored |
| `--no-service` | | refuse `SERVICE` for everyone |
| `--outbound-allow-private` | off | let `SERVICE` and `LOAD <http…>` reach loopback, private, shared (CGNAT) and unique-local addresses (see [Outbound requests](#outbound-requests-service-and-load)) |
| `--outbound-block-private` | | refuse those addresses: already the default of `serve` and `mcp`, an opt-in for the local `query` and `update` (which allow them by default) |
| `--outbound-allow HOST_OR_CIDR` | | contact only these destinations (repeatable) |
| `--outbound-timeout S` | `60` | total time of one outbound request, until the end of its response |
| `--outbound-max-mb N` | `256` | largest outbound response, decompressed |
| `--outbound-request-max-mb N` | 4 × `--outbound-max-mb` (`1024`) | bytes all the SERVICE calls and LOADs of one query or update may receive (`507` past it) |
| `--outbound-request-timeout S` | 4 × `--outbound-timeout` (`240`) | time all the SERVICE calls and LOADs of one query or update may take, summed |
| `--load-dir DIR` | | let `LOAD <file:…>` read the regular files under `DIR` (symbolic links resolved, nothing outside it); without it the server refuses file loads |
| `--max-prefixes N` | `1000` | prefixes per dataset (global flag; `0`: unlimited); a new one past it is refused with `400`, and loaded data stops adding its prefixes |

`sparkles serve --help` lists the rest (`--read-only`, `--result-cache-mb`,
`--auto-reason`, `--auth-config`, `--unix-socket`, `--map-style-url`, compression and
schema limits, …).

Over-budget requests fail with `507` and a JSON body naming the budget (`outbound-bytes` for
the outbound total). A query or write stops as soon as its client disconnects, and a write
then commits nothing. `sparkles query --memory-mb N` applies the memory budget on the
command line (unlimited by default).

## Command-line tools

The Jena `tdb2.*` / `arq` equivalents work on a database directory directly. A running
server locks the databases it holds; use the HTTP API, or `--server` where a command
takes it:

```sh
sparkles load    --loc db data/*.ttl.gz       # parallel bulk load (tdb2.tdbloader)
sparkles query   --loc db 'SELECT ...'        # --results text|json|xml|csv|tsv, --explain, --time
sparkles query   --data file.ttl --query q.rq # query files in memory (arq --data)
sparkles update  --loc db 'INSERT DATA {...}' # also LOAD <http…>
sparkles compact --loc db                     # merge updates into a new generation
sparkles dump    --loc db > dump.nq
sparkles dump    --loc db --out dump.nq.zst   # compression from the extension, or --compress
sparkles backup  --loc db --out backups/      # zstd; --compress gzip --level 9, --threads 8
sparkles clone   --loc db --to sandbox        # independent copy (same blank nodes, new dataset id)
sparkles stats   --loc db
sparkles log     --loc db                     # commit history (works next to a running server)
sparkles check   --loc db                     # verify the files, read-only (--quick, --format json)
sparkles infer   --loc db --profile owl-rl    # materialize inferences
sparkles infer   --loc db --status            # are the inferences up to date?
sparkles infer   --loc db --check             # OWL 2 RL inconsistency checks (exit 1 on violations)
sparkles infer   --loc db --vocab geosparql --geo-default-geometry   # + GeoSPARQL axioms, default geometries
sparkles text-index --loc db                  # full-text index: --predicate, --exclude-graph, --rebuild, --status, --disable
sparkles geo-index  --loc db                  # spatial index: --predicate, --feature-link, --exclude-graph, --wgs84,
                                              #   --distance geodesic|haversine, --rebuild, --status (JSON), --disable
```

`sparkles geo-index` enables the spatial index when it is off (with the defaults or the
given options), builds it and prints its status to stderr. When the index is already
enabled, opening the database starts the build, and the command reports the status after
it. It exits 2 when the binary was built without the `geo` feature.

The other commands: `schema`, `shacl`, `shex validate|parse`, `validation` (write-time
validation), `snapshot` (named snapshots and history retention), `repo` and
`backup create|list|show|restore|verify|delete|policy` ([below](#backup-repositories)),
`auth` (password hashes, tokens, `auth login` for remote `query` / `update` /
`load --server`), `mcp` ([below](#mcp-server-llm-agents)), `fmt` and `lsp`
([below](#formatting)). `sparkles help COMMAND` describes each.

## Formatting

`sparkles fmt` formats SPARQL queries and updates (`.rq`, `.ru`, `.sparql`), Turtle
(`.ttl`, `.turtle`), TriG (`.trig`), N-Triples (`.nt`), N-Quads (`.nq`) and JSON-LD
(`.jsonld`) with Prettier's modes and exit codes: 0 when everything is formatted (or was
written), 1 when `--check` or `-l` found a file that would change, 2 on any error (a
syntax error, a refused output, an unreadable file, a bad option). It processes every
file, so one run reports every problem, and formats files in parallel (`--threads N`).

```sh
sparkles fmt q.rq                    # print the formatted query (stdin to stdout without a path)
sparkles fmt --write queries/        # rewrite in place: temporary file, fsync, rename; unchanged files keep their mtime
sparkles fmt --check queries/        # [warn] per unformatted file on stderr; --diff adds unified diffs on stdout
sparkles fmt -l queries/             # names of the files that would change
sparkles fmt --stdin-filepath queries/q.rq < q.rq   # stdin named for detection, config and ignore files
sparkles fmt --sort data.nq          # N-Triples / N-Quads sorted (external sort past --sort-memory)
sparkles fmt --canonicalize data.nq  # RDFC-1.0 canonical form (drops comments)
```

- **Files.** Directories are walked recursively for the extensions above, with
  `.gitignore`, `.git/info/exclude` and the global gitignore applied (hidden files
  included; never `.git`, `node_modules` or `target`). A file named on the command line is
  formatted whatever its extension: `--language` names its language, else the extension
  does; with an unknown extension the content decides (it never reads as N-Triples,
  since N-Triples is valid Turtle too). Walks skip `.n3`, `.shc` and `.json` files, which
  need `--language` when named; RDF/XML and compressed files are refused.
- **Ignore file.** `.sparklesfmtignore` in the current directory (gitignore syntax), or
  `--ignore-path FILE` (repeatable). It applies to walks and to named files, which it
  skips silently; an ignored `--stdin-filepath` passes stdin through unchanged.
- **Config.** `.sparklesfmt.toml` (or `sparklesfmt.toml`), found by walking up from each
  file's directory; the nearest one wins and files are not merged. Flags override it;
  `--config FILE` uses one file for every input and `--no-config` the defaults. An unknown
  key or a bad value is an error naming the key and the file.

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
- **Messages.** Errors read `path:LINE:COL: error: …` (1-based lines and columns, in
  characters); a refused output reads `path: error: formatter refused its own output
  (algebra differs); input left unchanged; please report`.
- **Size.** A document is formatted in memory up to `--max-bytes` (default `256MiB`;
  sizes take a `KiB`, `MiB`, `GiB` or `TiB` suffix). A larger file is an error, with two
  exceptions:
  - N-Triples and N-Quads always stream: a file or stdin of any size is formatted in one
    pass in bounded memory (`--diff` formats them in memory).
  - Turtle and TriG over `--max-bytes` stream statement by statement unless sorted.
    `--max-bytes` then bounds the largest statement; with `prune-prefixes` the input is
    read twice, and stdin is kept in a temporary file.

  Sorting N-Triples and N-Quads spills sorted runs to the temporary directory past
  `--sort-memory` (default `1GiB`); `--canonicalize` holds at most
  `--max-canonicalize-quads` quads (default 20,000,000).

`sparkles lsp` serves the same formatting, with syntax errors and the formatter's warnings
as diagnostics, to editors over the Language Server Protocol; [editors.md](editors.md) has
the setups. `POST /$/format` is the HTTP form ([API.md](API.md#formatting)), and the UI
formats in the page when it is built with the formatter's WebAssembly module.

## Backup repositories

Backup repositories (see [API.md](API.md#backup-repositories)) work offline
too, on a stopped database; a server's own datasets are backed up through its HTTP API or
UI, or by its policies. `--repo` takes a name from the backup config file
(`--backup-config FILE`, `$SPARKLES_BACKUP_CONFIG`, default
`$XDG_CONFIG_HOME/sparkles/backup.toml`) or a URL: `file:///srv/backups/r`,
`s3://bucket/prefix?region=…&endpoint=…&path_style=true&allow_http=true`, or `memory://`.
Credentials never go in URLs; they come from the environment or a credentials file.
Only `repo add` and `backup create` initialize an empty location; the other commands
attach to an existing repository. Manifests are cached in
`$XDG_CACHE_HOME/sparkles/backup/`, progress goes to stderr, Ctrl-C cancels (twice:
quits), and the commands that print a result take `--format json` (or `--json`). Exit
codes: 0 ok, 1 errors, 2 warnings only (orphaned blobs in `repo verify`).

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

`restore --identity auto|new|keep` picks the dataset id (`auto` keeps it unless a dataset
of the target data directory has it) and `--check quick|full|none` the integrity check
before the restored database is published. `sparkles serve` holds a lock on
`<data>/sparkles-server.lock` (one server per data directory), and `restore --data`
refuses while a server holds it.

A server (`sparkles serve --backup-config FILE`) serves the file's repositories and
policies read-only through its API. It also takes repositories registered through its
API and UI (`POST /$/repositories`), under the operator's limits from that file:

* their credentials only name a source defined there, never environment variables, files
  or the instance's default chain of the caller's choosing;
* their S3 endpoints go through the [outbound policy](#outbound-requests-service-and-load)
  (a MinIO on localhost needs `--outbound-allow 127.0.0.1` or `--outbound-allow-private`),
  never through a proxy of the environment (`HTTPS_PROXY`; the config file's repositories
  and the CLI's use it);
* `fs` repositories stay out of the data directory and the config files' directories,
  and under `[api] fs_roots` when it is set:

```toml
[credentials.minio]              # named by {"source": "named", "name": "minio"}
source = "env"
access_key_id_var = "MINIO_ACCESS_KEY"
secret_access_key_var = "MINIO_SECRET_KEY"

[api]
fs_roots = ["/srv/backups"]
```

To register an S3 repository through the API, define its credential source in the
file first (by hand, or with `sparkles repo add … --credentials-name minio --credentials
…` on the same file), start the server with it (or send it SIGHUP), then
`POST /$/repositories` with `"credentials": {"source": "named", "name": "minio"}`.

## Outbound requests (SERVICE and LOAD)

`SERVICE <url>` and `LOAD <http…>` make the server open connections, so they follow a
network policy (with authentication on, they also need the `federate` permission). The
local `sparkles query` and `sparkles update` follow it too, with a different default
(below):

* only `http` and `https` URLs;
* the host is resolved once and the connection goes to exactly the addresses that were
  checked, so DNS rebinding cannot swap in another address; if any address a name
  resolves to is refused, the name is refused;
* by default only public addresses are contacted. Refused: loopback (`127.0.0.0/8`,
  `::1`), private (`10/8`, `172.16/12`, `192.168/16`), shared (`100.64.0.0/10`),
  link-local (`169.254.0.0/16` with the `169.254.169.254` metadata service, `fe80::/10`),
  unique-local (`fc00::/7`), multicast, broadcast, unspecified, documentation,
  benchmarking and reserved ranges, and the IPv4-mapped, IPv4-compatible, NAT64 and 6to4
  IPv6 forms of those;
* every redirect hop is checked the same way (at most 5 hops);
* a 10 s connect timeout, a total timeout (`--outbound-timeout`, default 60 s, and never
  past the query's own timeout), and a response ceiling counted as the body streams in and,
  for a compressed `LOAD`, after decompression (`--outbound-max-mb`, default 256);
* one budget for all the SERVICE calls and LOADs of a query or update, so that many
  requests cannot add up to more than a few large ones: the bytes they receive
  (`--outbound-request-max-mb`, by default 4 × `--outbound-max-mb`; a compressed `LOAD`
  counts its decompressed bytes) and the time they take, summed
  (`--outbound-request-timeout`, by default 4 × `--outbound-timeout`). Past the bytes the
  request fails with `507` (`"budget": "outbound-bytes"`) and an update commits nothing;
  past the time the next call gets only what is left and then fails like a timeout;
* a `LOAD` is parsed as its response streams in, not buffered first.

A refused destination fails with `403` before any connection is made. `SILENT` hides
neither that nor a spent budget; it hides failures of the remote side, such as timeouts.
Proxy environment variables (`HTTP_PROXY`, …) are ignored for these requests.

Error messages name the URL and its host, but neither what the host resolved to nor the
connection's own error: a refused name answers `… is refused by the outbound policy`, a
failed connection `cannot connect`. The server logs the details (target
`sparkles::outbound`: the resolved address and its kind, the OS error), so an operator can
tell why while a caller cannot map internal names to addresses.

`--outbound-allow-private` opens loopback, private, shared and unique-local addresses, for
example a local Fuseki during development:

```sh
sparkles serve --data ./data --outbound-allow-private
# SELECT * { SERVICE <http://localhost:3030/ds/sparql> { ?s ?p ?o } }
```

Link-local addresses, the metadata service among them, stay refused. In production,
prefer an allowlist: with `--outbound-allow` (repeatable) only the listed destinations are
contacted. An entry is one of:

* a host name (`--outbound-allow sparql.example.org`), or `*.example.org` for the
  subdomains of a name: contacted at public addresses (private ones too with
  `--outbound-allow-private`);
* an address or CIDR network (`--outbound-allow 10.20.0.0/16`): any address in it,
  private and link-local ones included.

A name vouches for the name, not for its addresses: to reach a name that resolves to a
private or link-local address, list that address or network as well (`--outbound-allow
fuseki.internal --outbound-allow 10.20.0.0/16`), so a hijacked or mistyped DNS record
never opens the metadata service. `sparkles mcp` takes the same flags. Library users set
`QueryOptions::outbound` (`sparkles::outbound::OutboundPolicy`, same defaults); the
default refusal of non-public addresses is the constant `BLOCK_PRIVATE_BY_DEFAULT`.

**Local commands.** `sparkles query` and `sparkles update` without `--server` run on the
operator's own machine, so they allow loopback, private, shared and unique-local
destinations by default, as `--outbound-allow-private` does for a server: a SERVICE call
to a local Fuseki or a `LOAD` from an intranet host needs no flag. Link-local addresses (the
metadata service) stay refused. They take the same `--outbound-*` flags;
`--outbound-block-private` restores the strict default of `serve` and `mcp`, e.g. for a
query from an untrusted source:

```sh
sparkles query --data local.ttl 'SELECT * { SERVICE <http://localhost:3030/ds/sparql> { ?s ?p ?o } }'
sparkles query --data local.ttl --outbound-block-private --query untrusted.rq
```

With `--server`, the request runs on that server under its own policy, and these flags
do not apply.

**Local files.** `LOAD <file:…>` over HTTP needs `server-admin` (with authentication on)
and `serve --load-dir DIR`: the file must be a regular file under `DIR` once `..` and
symbolic links are resolved (a link inside `DIR` may point elsewhere inside it). Without
the flag the server refuses file loads (`403`); `DIR` may not hold the data directory.
The local `sparkles update` reads any file its user can. Library users set
`QueryOptions::file_loads` (`FileLoads::Anywhere` by default, `FileLoads::under(dir)`, or
`FileLoads::Disabled`).

## Checking a database

`sparkles check --loc db` verifies a database directory without modifying it: it takes
no lock, truncates no WAL, repairs no catalog and rebuilds no full-text index, so it can
run next to a server that holds the database (`--data DIR` checks every database of a
server data directory). Each check prints one line, `ok`, `warning` or `error`, with the
file, offset, block, row, commit or id of every problem below it; `--format json`
prints the same report as JSON. The exit status is 0 when everything is clean, 1 when
any check found an error, and 2 when there are warnings only.

| Check | What it verifies |
|---|---|
| `layout` | `CURRENT` names an existing generation; `dataset.json`, the generation's `commit.json` (same dataset id) and `prefixes.json` parse; leftovers of interrupted work (`*.tmp`, `text.new`/`text.old`, old or unfinished `gen-NNNN`, a set-aside catalog) are warnings |
| `generation` | `meta.json` (index format) and `stats.json` parse and agree on the quad count |
| `vocabulary` | the front-coded vocabulary decodes, its keys strictly increase, its size matches `meta.json`, and every vocabulary id in the permutations is below it |
| `delta-vocabulary` | the update vocabulary is well formed and holds no duplicate (a torn tail is a warning) |
| `perm.spo` … `perm.gspo` | block metadata is contiguous and sorted and fits the file; the row count matches `meta.json`; every block decodes to its row count, its first and last keys match the metadata, keys strictly increase within and across blocks, and every id is valid for its position |
| `permutations` | the 7 permutations hold the same number of rows and, compared by an order-independent hash, the same quads |
| `wal` | records are well formed; every commit record's checksum matches (a damaged final transaction is a warning: open truncates it); commit numbers continue from the generation's base commit; ids resolve |
| `catalog` | `commits.bin` record checksums and continuity, its dataset id, and agreement with the WAL; a lagging catalog, or damage open can rebuild from the WAL, is a warning, lost history before the generation an error |
| `text` | `text.json` parses; the index opens read-only, every committed segment file exists and matches its checksum; its commit against the WAL (behind is a warning: caught up or rebuilt on open) |
| `geo` | `geo.json` parses and is a valid configuration; the current generation's index files (`geo/rtree.spkg`, `geo/column.spkg`): header, footer, that they belong to this generation and configuration, index checksums, and in full mode the data checksums (a damaged file is a warning: rebuilt on open) |
| `reasoning` | `reasoning.json` parses and names an existing commit |

Errors are what `Store::open` refuses, what loses acknowledged data or history, or what
queries would read wrongly; warnings are states open handles by itself. A server writing
meanwhile can cause transient warnings (an in-flight transaction looks like a torn
tail), never errors. `--quick` reads metadata only: block metadata instead of every
block, the first key of each vocabulary block, segment files' presence instead of
their checksums. On the 10.5M-quad benchmark database a full check takes about 0.3 s
and a quick one 20 ms (16 cores, warm page cache). The same check is a library call:
`sparkles::check::check(root, &CheckOptions::default())` returns a serializable
`CheckReport`.

## MCP server (LLM agents)

`sparkles mcp` serves databases, or RDF files loaded into memory, to an MCP host
(Claude Desktop, Claude Code, IDE agents, …) over stdin/stdout:

```sh
sparkles mcp --loc /data/books                 # a database directory (name: books)
sparkles mcp --loc books=/data/books --loc films=/data/films
sparkles mcp --data a.ttl b.nt --name demo     # files, in one in-memory dataset
```

Host configuration (`claude_desktop_config.json`, or a project's `.mcp.json` for Claude
Code; `claude mcp add sparkles -- sparkles mcp --loc /data/books` does the same):

```json
{
  "mcpServers": {
    "sparkles": { "command": "sparkles", "args": ["mcp", "--loc", "/data/books"] }
  }
}
```

The tools are read-only: `list_datasets`, `describe_schema`, `sparql_query`,
`explain_query`, `describe_resource`, `list_commits`, `search_text` (BM25 over a
full-text index; `--text` indexes `--data` files), `similar_entities` (exact search
over stored `spk:vector` embeddings; it never computes them), and `validate_shacl` and
`validate_shex` (a shapes graph, or a ShEx schema with a shape map, checked against a
snapshot: counts and the first 20 results with node, shape and reason; no imports).
Schemas are in [API.md](API.md#mcp-server). Results are sized for a model's context:
query rows come back as a compact table with the dataset's prefixes (100 rows / 64 KiB by
default), every truncation is announced with the exact total and how to continue, and
data values are escaped so they cannot pass for table structure or status lines. Every
result names the commit it read; passing it back as `atCommit` keeps a multi-call
exploration on one snapshot (the server holds the last 4 commits read per dataset for 10
minutes).

Every call runs under the query timeout (30 s by default, `--timeout` is the maximum),
a memory budget (`--query-memory-mb`, default 2048) and the intermediate-row cap, at
most `--max-concurrent` (4) at a time. SERVICE is off unless `--allow-service`, because
a prompt-injected model could otherwise send data to any URL; when allowed, it follows
the [outbound policy](#outbound-requests-service-and-load). `--disable-tool NAME`
removes a tool. A database held by a running `sparkles serve` is refused (the lock).
Only stdio is served for now. Logs go to stderr; stdout carries JSON-RPC only.

## Embedding the library

`crates/sparkles` is a plain Rust library. The CLI (everything except `serve`) and the
server are built on it, so anything they do can be done in-process. A database
directory is locked while open (`sparkles.lock`, like TDB2's `tdb.lock`).

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

Lower-level access (ids, snapshots, raw index scans, the bulk `Builder`) is available
through `Dataset::store()` and the `store` / `index` / `builder` modules. `mise run doc`
builds the API documentation of the library crates.

## Deploying on NixOS

The flake's `nixosModules.default` provides `services.sparkles`, which runs the server as
a hardened systemd service (the packages and other flake outputs are in
[DEVELOPMENT.md](DEVELOPMENT.md#nix)). Its state lives in `/var/lib/sparkles` (the
dataset registry, databases created from the UI or admin API, and backups). It can put an
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
`openFirewall`). Another `listenAddress` needs `auth.configFile` or
`allowOpenNetwork = true` (an assertion checks it). The nginx location sets:

* `client_max_body_size` to `nginx.clientMaxBodySize` (default 4g), for bulk uploads;
* proxy timeouts to `queryTimeout + 30` seconds;
* request/response buffering off, so large uploads and results stream through;
* `X-Forwarded-For` to the client's address (`$remote_addr`, replacing whatever the
  client sent), and `Forwarded` to nothing.

The server always trusts nginx for `X-Forwarded-For` (`--rate-limit-trusted-proxy` for
127.0.0.1 and ::1, or `unix` with `unixSocket`), whether or not `rateLimits` is set, so
failed logins and rate limits count each client rather than nginx. Behind another proxy
or CDN, set up nginx's realip module so that `$remote_addr` is the client.

`loadDir` passes `--load-dir`: `LOAD <file:…>` over HTTP may read from that directory
only (the service gets it read-only; it must not contain `dataDir` or lie under `/tmp`).

With `auth.configFile` the service starts with `--auth-config` and `systemctl reload
sparkles` re-reads it (SIGHUP). Keep the file out of the Nix store (agenix, sops-nix),
owned by the `sparkles` user. Do not also set nginx `basicAuthFile`: nginx would forward
its own `Authorization` header, which Sparkles would then reject. `unixSocket` makes the
server listen on a Unix socket that nginx proxies to, so trusted proxy headers can be
limited to it (`proxy.trusted = ["unix"]`).

Backup repositories: `backup.configFile` passes `--backup-config` (like
`auth.configFile` it stays out of the Nix store, and `systemctl reload sparkles`
re-reads it), `backup.maxTasks` passes `--backup-max-tasks`, and `backup.fsRoots` lists the
directories of `fs` repositories, which the module creates for the service user and
makes writable (the service sees the rest of the file system read-only):

```nix
services.sparkles.backup = {
  configFile = "/run/secrets/sparkles-backup.toml";  # [repositories.local] path = "/srv/backups/sparkles/local"
  fsRoots = [ "/srv/backups/sparkles" ];             # also [api] fs_roots, for API registrations
  maxTasks = 1;
};
```

The CLI goes on the system path unless `installCli = false`. The server holds a lock on
its databases, so for offline work (`sparkles load`, `compact`) stop the service first,
or use the HTTP API.
