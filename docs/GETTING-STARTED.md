# Getting started and operations

Sparkles is experimental and has no released packages. The on-disk format, HTTP API, CLI,
and library APIs can change between commits without a migration path. Keep your original
source data and the pipeline needed to regenerate it. Backups recover datasets within a
compatible build, but they do not guarantee forward compatibility with a later commit.

## Install and try Sparkles

The [README quick start](../README.md#getting-started) builds `sparkles-server` from
source, loads a Turtle file, and runs a query both locally and over HTTP. To include the
embedded web UI at `/ui/`, build `ui/` with Node.js and pnpm before compiling the server.

Sparkles also deploys with Docker or NixOS, or embeds directly into another language.

| Option | How to start | Guide |
|---|---|---|
| Docker | Run the repository image with `docker compose up --build -d`. | [Docker](USAGE.md#docker) |
| NixOS / Nix | Run directly with `nix run` or enable the NixOS module. | [Deploying on NixOS](USAGE.md#deploying-on-nixos) |
| Language bindings | Build the Rust, Python, JVM (Jena `DatasetGraph`), or Node.js bindings from source. | [Libraries and clients](README.md#libraries-and-clients) |

The server binds to loopback (`127.0.0.1`) by default. While running, it holds an
exclusive lock on its open datasets, so query running datasets over HTTP or stop the
server before running local `--loc` commands. See
[command-line tools](USAGE.md#command-line-tools) for offline loading, dumping,
compaction, and inspection.

## Evaluate compatibility

Before migrating an existing RDF workload or Jena/Fuseki application, check scope and
tradeoffs across three references:

* **[Capability status](FEATURES.md#capability-status)** separates shipped features from
  partially complete and planned work, and links to each category's known gaps.
* **[Sparkles compared](COMPARISON.md)** details feature differences against Apache
  Jena/Fuseki, QLever, Fluree, and Oxigraph, including deliberate divergences from Jena
  and QLever.
* **[Benchmark summary](BENCHMARKS.md)** reports measured query, load, write, and memory
  results alongside [measured losses](BENCHMARKS.md#where-sparkles-loses) and coverage
  limits.

## Secure a server

Without an authentication configuration, any caller who can reach the server can read,
write, and administer datasets. Sparkles refuses to listen on a non-loopback address
without authentication unless you explicitly opt in to an open network.

Complete three steps before exposing a server beyond loopback.

1. **Configure authentication and grants.** Choose Basic credentials, Bearer tokens, OIDC,
   or trusted proxy assertions, and scope grants by dataset, named graph, or endpoint. A
   reverse proxy does not protect a backend port that clients can reach directly. See
   [network exposure](USAGE.md#network-exposure) and
   [authentication and access control](API.md#authentication-and-access-control).
2. **Enable TLS.** Terminate HTTPS directly in Sparkles with rustls or behind a locked-down
   proxy. See [TLS](USAGE.md#tls).
3. **Set explicit resource budgets.** Several query budgets and rate limits are disabled
   or generous by default. Configure them to match your workload and caller trust level as
   described in [limits that are off by default](USAGE.md#limits-that-are-off-by-default).

## Protect and recover data

Operational tooling covers per-dataset backups, integrity verification, and format checks:

* **Backups and restore drills.** Configure filesystem or S3
  [backup repositories](USAGE.md#backup-repositories) for incremental, deduplicated
  backups, and test restores in your target environment before relying on them.
* **Integrity checks.** Run `sparkles check` to verify WAL checksums and commit
  continuity, as described in [checking a database](USAGE.md#checking-a-database).
* **Format upgrades.** Check stored format versions and run available migration commands
  documented in [upgrading databases](USAGE.md#upgrading-databases).

Whole-server disaster recovery and a release compatibility contract remain planned. See
the [operations guide](operations/README.md#operations) and
[production readiness](ROADMAP.md#production-readiness) for the remaining scope.
