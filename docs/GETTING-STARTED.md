# Getting started and operations

Sparkles is experimental and has no released packages. The on-disk format, HTTP API,
CLI and library APIs may change between commits without a migration path. Keep the
original data and the tools needed to regenerate it. Backups are useful for recovery
within a compatible build, but do not guarantee that a later build can read the data.

## Install and try Sparkles

The [README quick start](../README.md#getting-started) builds the server, loads a
small Turtle file and runs a query locally and over HTTP. You can also run the server
with [Docker](USAGE.md#docker) or [NixOS](USAGE.md#deploying-on-nixos). The language
bindings are built from source, as described in [libraries and clients](README.md#libraries-and-clients).

The server listens on loopback by default. A running server locks its datasets, so use
HTTP to query them or stop the server before opening them with a local CLI command.
The [command-line guide](USAGE.md#command-line-tools) describes local operations.

## Secure a server

Without an auth configuration, callers can read, write and administer datasets. Sparkles
refuses a non-loopback listener without authentication unless the operator explicitly
allows an open network. A reverse proxy does not protect a backend that clients can
reach directly. Read [network exposure](USAGE.md#network-exposure),
[authentication](API.md#authentication-and-access-control) and
[TLS](USAGE.md#tls) before exposing a server.

Some limits are disabled or generous by default. Set budgets and rate limits for the
workload and callers described in [limits that are off by default](USAGE.md#limits-that-are-off-by-default).

## Protect and recover data

Use [backup repositories](USAGE.md#backup-repositories) and verify that a restore
works in your environment. [Checking a database](USAGE.md#checking-a-database) covers
integrity checks. [Upgrading databases](USAGE.md#upgrading-databases) describes the
available format checks and migration commands. Whole-server recovery and a release
compatibility contract remain planned, as the [operations guide](operations/README.md#operations)
and [roadmap](ROADMAP.md#production-readiness) describe.

## Evaluate compatibility

The [feature matrix](FEATURES.md#capability-status) identifies available and planned
scope. [Sparkles compared](COMPARISON.md) describes differences from Jena, Fuseki,
QLever, Fluree and Oxigraph. The [benchmark summary](BENCHMARKS.md) covers measured
workloads, losses and limitations.
