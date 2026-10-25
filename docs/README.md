# Documentation

This index lists the Sparkles documentation by what a reader wants to do. The repository's
[README](../README.md) introduces Sparkles, its performance and a quick start.

## Using Sparkles

| Document | What it covers |
|---|---|
| [Usage](USAGE.md) | How to run the server, load and query data, and operate datasets, with the [CLI tools](USAGE.md#command-line-tools), [Docker](USAGE.md#docker) and [NixOS](USAGE.md#deploying-on-nixos). |
| [Libraries and bindings](USAGE.md#embedding-the-library) | How to embed the engine from [Rust](USAGE.md#embedding-the-library), [Python](USAGE.md#python), the [JVM through Jena](USAGE.md#jvm-apache-jena) and [Node.js](USAGE.md#javascript-and-typescript), or connect with the [Rust client](USAGE.md#rust-client). |
| [Agents and models](USAGE.md#mcp-server-llm-agents) | How to connect an agent over MCP, use [agent memory](USAGE.md#agent-memory) and [answer questions with a model](USAGE.md#asking-questions-with-a-model). |
| [HTTP API](API.md) | The request and response contract of every endpoint, with status codes, parameters, formats and protocol semantics. |
| [OpenAPI](openapi.json) | The machine-readable HTTP contract, checked against the server's route table. |
| [Editor integration](editors.md) | How to format SPARQL and RDF in an editor and use the language server. |

## Understanding Sparkles

| Document | What it covers |
|---|---|
| [Features](FEATURES.md) | Every capability, area by area, and the [known gaps](FEATURES.md#known-gaps). |
| [Architecture](ARCHITECTURE.md) | The library boundaries, storage, execution and the main design decisions. |
| [Comparison](COMPARISON.md) | The gaps and tradeoffs against Jena, QLever, Fluree and Oxigraph, and where Sparkles departs from Jena on purpose. |
| [Benchmarks](BENCHMARKS.md) | The published performance results, their setup and every case where Sparkles loses. |
| [Query optimizations](internals/OPTIMIZATIONS.md) | Operator fast paths, when they apply and the local cost-model measurements behind them. |

## Design and planning

| Document | What it covers |
|---|---|
| [Roadmap](ROADMAP.md) | Planned work in priority order, with the size, impact and design spec of each item. |
| [Design specs](specs/README.md) | The rationale, status and outcome of each larger feature. |
| [Provenance](specs/PROVENANCE.md) | An index to per-spec and shared records of design sources, adopted dependencies, licenses and rejected alternatives. |
| [Source audit](AUDIT.md) | The original audit of Jena, QLever and Oxigraph that set the first design choices. |

## Contributing

| Document | What it covers |
|---|---|
| [Development](DEVELOPMENT.md) | The project layout, builds, tests, mise tasks and Nix setup. |
| [Running benchmarks](BENCHMARKING.md) | The benchmark harness, its modes and outputs, and how to measure a regression. |
| [UI development](../ui/README.md) | How to build, test and change the web UI. |

## Maintaining documentation

* Workflows and examples belong in Usage, exact HTTP contracts in API, and system
  structure in Architecture. Link between them when a reader needs the other view.
* Features holds current capabilities and limitations. Comparison explains what they mean
  for compatibility or migration and links to the details.
* Benchmarks holds published timings and their setup. Local optimization measurements stay
  with their rationale and say that they are local.
* A spec keeps its original design. Its status and Outcome change with the behavior, and
  the user guide changes with them. A specified phase is not a shipped capability.
* The source audit is historical context, and the guides describe supported behavior.
* Keep heading anchors when moving sections, or update every link to them. Check local
  links and cited source paths after a move. `mise run lint:doc-paths` checks cited source
  paths, but not every Markdown link or anchor.
