# Usage

This guide covers operating the `sparkles` binary: running the server, the command-line
tools, the formatter, backups, outbound requests, integrity checks, the MCP server,
embedding the library and deploying on NixOS. [API.md](API.md) specifies the HTTP API.
[DEVELOPMENT.md](DEVELOPMENT.md) covers building from source and testing.

Sparkles is experimental. The on-disk format, HTTP API and CLI may change between commits
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
allocator. It comes from the `mimalloc` cargo feature of `sparkles-server`, which is on
by default. The `sparkles` library leaves the choice of allocator to its embedder. Once
no request has been active for `--idle-release-ms` (default 1000 ms), `sparkles serve`
hands free heap memory back to the OS. The interval counts from the end of the last
request, so requests that keep arriving, even with short gaps, never meet a release. A build with
`--no-default-features --features reasoning,shacl` uses the system allocator and
`malloc_trim` instead.

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

### Endpoints and operations

A dataset `ds` has the Fuseki-style endpoints `/ds/sparql`, `/ds/update`, `/ds/data`
(GSP) and `/ds/upload`. The server also has `/$/datasets`, `/$/stats/ds`,
`/$/compact/ds`, `/$/backup/ds` and `/$/tasks` (see [API.md](API.md)). `--mem NAME` adds
an in-memory dataset, and `--loc NAME=PATH` serves an existing database.

`/$/ping` is the liveness check and `/$/ready` the readiness check. `/$/ready` returns
`503` once shutdown starts on SIGINT or SIGTERM. The requests in flight then get
`--shutdown-grace` seconds to finish, after which the rest are cancelled and commit
nothing ([API.md](API.md#shutdown)). `/$/metrics` serves Prometheus metrics.
Every response carries an `X-Request-Id`, and each request is logged once under the
`sparkles::access` target.

### `serve` options

| Flag | Default | Meaning |
|---|---|---|
| `--host ADDR` | `127.0.0.1` | Listen address. A non-loopback address needs `--auth-config` or `--allow-open-network`. |
| `--allow-open-network` | off | Serve without `--auth-config` on a non-loopback address, and log a warning. Also `SPARKLES_ALLOW_OPEN_NETWORK=1`. |
| `--public-host NAME` | | A host name that clients use to reach the server, such as a reverse proxy's. Repeatable. Without `--auth-config`, names other than IP addresses, `localhost` and `--host` are refused with `421`. With `--auth-config`, the same applies to requests that carry trusted proxy headers from loopback or the Unix socket. |
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
| `--backup-config FILE` | | TOML file with the backup repositories, policies, credential sources and the limits on repositories registered through the API. Also `$SPARKLES_BACKUP_CONFIG`. Re-read on SIGHUP, and read-only through the API. |
| `--backup-max-tasks N` | `2` | Backup, restore, verify and GC tasks that may run at once. More wait as `queued`. |
| `--format-endpoint on\|authenticated\|off` | `on` | Who may use `POST /$/format` (see [API.md](API.md#formatting)). `on` admits every caller the server admits, `authenticated` every caller but the anonymous principal (`401`), and `off` nobody (`404`). A UI built with the formatter's WebAssembly module formats in the page and needs the endpoint only as a fallback. |
| `--format-max-mb N` | `16` | Largest `POST /$/format` body; `0` means unlimited. |
| `--format-timeout S` | `10` | Seconds a `POST /$/format` request may take, including the wait for a free slot (one per core). A slower request gets `408`. |
| `--vector-memory-mb N` | `4096` | Memory for packed vectors and HNSW graphs (`spk:vectorSearch` and vector indexes), per index generation. A build past it leaves the index `over-budget`, and a search past it gets `507`. A global flag. |
| `--text NAME[=FILE]` | | Enable full-text search for a dataset. `FILE` is a `text.json`-shaped configuration file. |
| `--geo NAME[=FILE]` | | Enable the spatial index for a dataset. `FILE` is a `geo.json`-shaped configuration file. The build runs before the server starts listening. |
| `--geo-mb N` | `4096` | Memory for each dataset's spatial index (geometry column and trees). A build that would exceed it is refused, the status says `over-budget`, and queries run without the index. |
| `--geo-op-vertices N` | `2000000` | Largest total of input vertices for one geometry operation (overlay, buffer, hull, relate). A larger operation is a type error. |
| `--log-format text\|json` | `text` | Log format on stderr. A global flag. `RUST_LOG` filters as usual. |
| `--no-access-log` | | No per-request log lines. |
| `--no-metrics` | | `/$/metrics` answers `404`, and no request metrics are kept. |
| `--metrics-max-datasets N` | `100` | Datasets that get their own metric labels. The rest share `$other`. |
| `--metrics-fuseki-names` | off | Also expose Fuseki's metric names (`fuseki_requests`, `fuseki_requests_good`, `fuseki_requests_bad`) on `/$/metrics`, for dashboards built for Fuseki. See [API.md](API.md#fuseki-metric-names). |
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
| `--load-dir DIR` | | Let `LOAD <file:…>` read the regular files under `DIR`, with symbolic links resolved and nothing outside it. Without this flag, the server refuses file loads. |
| `--max-prefixes N` | `1000` | Prefixes per dataset; `0` means unlimited. A global flag. A new prefix past the limit is refused with `400`, and loaded data stops adding its prefixes. |

`sparkles serve --help` lists the other options, including `--read-only`,
`--result-cache-mb`, `--auto-reason`, `--auth-config`, `--unix-socket`,
`--map-style-url`, and the compression and schema limits.

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
sparkles diff    --loc db 41 42               # what commit 42 changed, as + and - N-Quads lines
sparkles check   --loc db                     # verify the files, read-only (--quick, --format json)
sparkles infer   --loc db --profile owl-rl    # materialize inferences
sparkles infer   --loc db --status            # are the inferences up to date?
sparkles infer   --loc db --check             # OWL 2 RL inconsistency checks (exit 1 on violations)
sparkles infer   --loc db --vocab geosparql --geo-default-geometry   # + GeoSPARQL axioms, default geometries
sparkles text-index --loc db                  # full-text index: --predicate, --exclude-graph, --rebuild, --status, --disable
sparkles geo-index  --loc db                  # spatial index: --predicate, --feature-link, --exclude-graph, --wgs84,
                                              #   --distance geodesic|haversine, --rebuild, --status (JSON), --disable
sparkles vector create --loc db --name emb --predicate http://example.org/emb --dim 384
                                              # vector index with an HNSW graph: --metric, --m, --ef-construction,
                                              #   --ef-search, --exact-threshold, --no-hnsw
sparkles vector list|status|rebuild|drop --loc db [--name emb]   # or --server URL --dataset NAME
sparkles quota   --loc db --max-mb 10240      # storage quota; --default removes it, no flag prints it
sparkles quota   --server URL --dataset db --max-mb 0   # on a server, as server-admin; 0 is unlimited
```

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

`sparkles update` and `sparkles load` take `--message TEXT`, which is stored with the
commit they make and shown by `sparkles log` and `/$/commits`. With `--server`, the
message travels in the `Sparkles-Commit-Message` header, and each file that `load` sends
becomes its own commit with the same message. A message is at most 1024 bytes of UTF-8
with no control characters.

```sh
sparkles update --loc db --message 'Fix the labels of ex:alice' 'DELETE … INSERT …'
sparkles load   --server http://localhost:3030 --dataset ds --message 'Nightly import' data.ttl
sparkles log    --loc db                      # the message follows each commit's columns
```

Past states are read with `--at`, which takes a commit number, `commit:N`,
`time:<RFC 3339>` or `snapshot:NAME`. Every commit since the last compaction or bulk
commit can be read, and `sparkles log` marks them with `*`. A named snapshot or the
retention window keeps older ones. `sparkles diff` shows the quads added and removed
between two states, and `--format json` or `--format count` change its output.

```sh
sparkles query    --loc db --at snapshot:release-1 'SELECT ...'
sparkles dump     --loc db --at time:2026-09-30T14:00:00Z > then.nq
sparkles clone    --loc db --to sandbox --at commit:40
sparkles diff     --loc db snapshot:release-1 head --graph http://ex.org/g
sparkles snapshot create   --loc db release-1 --note 'before the migration'
sparkles snapshot create   --loc db tmp --expires 7d
sparkles snapshot retain   --loc db --keep-age 7d --max-bytes 20GiB
sparkles snapshot schedule --loc db --prefix daily- --every 1d --keep-last 7
sparkles snapshot gc       --loc db        # expire pins, make scheduled ones, collect
```

A running server does the work of `snapshot gc` every minute. Over HTTP the same
features are `?at=`, `GET /{ds}/diff`, `/$/snapshots/{ds}` and `/$/history/{ds}`
([API: Point-in-time reads and snapshots](API.md#point-in-time-reads-and-snapshots)).

The global flag `--commit-digests` makes a command record a change digest with every
commit of the databases it opens. A database keeps the setting once it is on, so later
commands and servers record digests too. See [API: Commits](API.md#commits).

`sparkles geo-index` enables the spatial index if it is off, with the defaults or the
given options. It then builds the index and prints its status to stderr. When the index
is already enabled, opening the database starts the build, and the command reports the
status after it. The command exits 2 when the binary was built without the `geo`
feature.

The other commands are:

* `schema`, `shacl` and `shex validate|parse`;
* `validation`, for write-time validation ([API](API.md#write-time-validation));
* `snapshot`, for named snapshots and history retention;
* `quota`, for the storage quota of a dataset, locally or on a `--server`;
* `repo` and `backup create|list|show|restore|verify|delete|policy`
  ([below](#backup-repositories));
* `auth`, for password hashes, tokens, and `auth login` for remote `query`, `update` and
  `load --server`;
* `mcp` ([below](#mcp-server-llm-agents));
* `fmt` and `lsp` ([below](#formatting)).

`sparkles help COMMAND` describes each one.

`sparkles schema --loc db --format void` prints the schema report as a VoID description
in Turtle, and `--format turtle` adds the declared RDFS/OWL schema. The server answers
`GET /$/schema/{ds}` the same way when the request asks for Turtle or another RDF syntax
([API.md](API.md#schema-discovery)).

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
(`sparkles::outbound::OutboundPolicy`), which has the same defaults. The default refusal
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
| `vocabulary` | The front-coded vocabulary decodes, its keys strictly increase, and its size matches `meta.json`. Every vocabulary id in the permutations is below that size. |
| `delta-vocabulary` | The update vocabulary is well formed and holds no duplicate. A torn tail is a warning. |
| `perm.spo` … `perm.gspo` | Block metadata is contiguous, sorted and fits the file, and the row count matches `meta.json`. Every block decodes to its row count, and its first and last keys match the metadata. Keys strictly increase within and across blocks, and every id is valid for its position. |
| `permutations` | The 7 permutations hold the same number of rows and, compared by an order-independent hash, the same quads. |
| `wal` | Records are well formed, and every commit record's checksum matches. A damaged final transaction is a warning, because open truncates it. Commit numbers continue from the generation's base commit, and ids resolve. |
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
  `describe_resource` and `list_commits`.
* `search_text` runs BM25 search over a full-text index. `--text` indexes `--data` files.
* `similar_entities` runs exact search over stored `spk:vector` embeddings. It never
  computes embeddings.
* `validate_shacl` and `validate_shex` check a shapes graph, or a ShEx schema with a
  shape map, against a snapshot. They return counts and the first 20 results with node,
  shape and reason. They do not follow imports.
* `format` formats a SPARQL query or update, Turtle, TriG, N-Triples, N-Quads or JSON-LD
  the way `sparkles fmt` does, and returns the text with any warnings. It reads no
  dataset.
* `sparql_update` runs SPARQL Update. It is offered only with `--allow-update` (or
  `serve --mcp-allow-update`), never on a read-only server. Writes pass the dataset's
  write-time validation, the call's `message` becomes the commit message, and `LOAD` is
  refused. Hosts that confirm destructive tools ask before each call.

Hosts can also attach two resources per dataset as context, the schema summary
(`sparkles://{ds}/schema`) and the prefixes (`sparkles://{ds}/prefixes`). Two prompts,
`explore_dataset` and `answer_question`, start a session with the tool workflow and the
dataset's prefixes.

[API.md](API.md#mcp-server) has the tool schemas. Results are sized for a model's
context. Query rows come back as a compact table with the dataset's prefixes, up to 100
rows or 64 KiB by default. Every truncation is announced with the exact total and how to
continue. Data values are escaped so they cannot pass for table structure or status
lines. Every result names the commit it read. Passing that commit back as `atCommit`
keeps a multi-call exploration on one snapshot. The server holds the last 4 commits read
per dataset for 10 minutes.

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

`Dataset::store()` and the `store`, `index` and `builder` modules give lower-level
access: ids, snapshots, raw index scans and the bulk `Builder`. `mise run doc` builds
the API documentation of the library crates.

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

When the service stops, requests in flight get `shutdownGrace` seconds (default 20) to
finish before they are cancelled. The unit's `TimeoutStopSec` is `shutdownGrace + 15`,
which leaves time for the cancelled requests to stop and for the final flush.

The CLI goes on the system path unless `installCli = false`. The server holds a lock on
its databases, so stop the service before offline work such as `sparkles load` or
`compact`, or use the HTTP API instead.
