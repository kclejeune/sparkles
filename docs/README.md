# Documentation

This index organizes the Sparkles documentation by task. Start with the
[README](../README.md) for a high-level overview, headline benchmarks, and a quick start.

## Evaluating Sparkles

| Document | What it covers |
|---|---|
| [Features](FEATURES.md) | The capability matrix, compatibility boundaries, known gaps, and links to every category guide. |
| [Comparison](COMPARISON.md) | Feature differences and design tradeoffs against Jena/Fuseki, QLever, Fluree, and Oxigraph. |
| [Benchmark summary](BENCHMARKS.md) | Representative results, coverage limits, and recorded comparisons where other engines lead. |
| [Web UI tour](visualization/README.md#web-ui-tour) | Visual walkthrough of the embedded web UI across querying, history, GraphRAG, and authoring. |

## Getting started and operating

| Document | What it covers |
|---|---|
| [Getting started and operations](GETTING-STARTED.md) | Install and try Sparkles, evaluate compatibility, secure a server, and plan backups and recovery. |
| [Usage](USAGE.md) | Run the server, operate datasets, and use the [CLI tools](USAGE.md#command-line-tools), [Docker](USAGE.md#docker), and [NixOS](USAGE.md#deploying-on-nixos). |
| [HTTP API](API.md) | Exact request and response contracts for Fuseki endpoints and Sparkles extensions. |
| [OpenAPI specification](openapi.json) | Machine-readable HTTP contract verified against the server's route table. |
| [Agents and models](USAGE.md#mcp-server-llm-agents) | Connect agents over MCP, use [agent memory](USAGE.md#agent-memory), and [ask questions with a model](USAGE.md#asking-questions-with-a-model). |
| [Editor integration](editors.md) | Format SPARQL and RDF in an editor and configure the language server. |

## Libraries and clients

Applications can embed the engine in process from [Rust](USAGE.md#embedding-the-library),
[Python](USAGE.md#python), [Java and Kotlin through Jena](USAGE.md#jvm-apache-jena), or
[Node.js](USAGE.md#javascript-and-typescript), or connect over HTTP with the
[Rust client](USAGE.md#rust-client). The
[integration guide](integrations/README.md#libraries-and-bindings) lists the API surfaces
and limitations of each binding.

## Feature guides

Each category guide documents current capabilities, configuration, usage links, known
gaps, and roadmap items for its area.

| Guide | Area |
|---|---|
| [Querying](querying/README.md) | SPARQL 1.0–1.2, RDF 1.2, Jena ARQ extensions, federation, incremental views, CLI, and formatting. |
| [Storage and history](storage/README.md) | Columnar indexes, vocabulary encoding, bulk loading, transactions, snapshots, diffs, and branches. |
| [Search and graph analysis](search/README.md) | BM25 full-text search, HNSW vector indexes, hybrid ranking, GeoSPARQL 1.1, path search, and graph algorithms. |
| [Reasoning and validation](reasoning/README.md) | RDFS, OWL 2 RL, OWL 2 EL classification, Jena rules, OWL 2 ontologies, SHACL, and ShEx 2.2. |
| [Server and security](server/README.md) | Fuseki endpoints, `/$/` admin API, authentication, dataset/graph/triple grants, budgets, and telemetry. |
| [AI and agents](ai/README.md) | MCP server, Ask pipeline, cited agent memory, document ingestion, GraphRAG communities, and evidence profiles. |
| [Visualization and editing](visualization/README.md) | Embedded SvelteKit UI, visual query builder, graph explorer, maps, forms, and ontology/SKOS/SHACL editors. |
| [Libraries and integrations](integrations/README.md) | Rust, Python, JVM (`DatasetGraph`), Node.js, TypeScript, GraphQL, and tabular/RML mappings. |
| [Operations](operations/README.md) | Backup repositories, integrity checks, format migrations, degraded startup, and notifications. |

## Benchmarks

| Document | What it covers |
|---|---|
| [Benchmark summary](BENCHMARKS.md) | Headline numbers, measured losses, and rules for interpreting comparisons. |
| [Workload index](benchmarks/README.md) | Per-suite methodology, published results, generated charts, and immutable raw run catalogs. |
| [Running benchmarks](BENCHMARKING.md) | Harness modes, regression screens, instruction-count checks, and reproduction steps. |
| [Performance experiments](PERFORMANCE-EXPERIMENTS.md) | Measured optimizations that were reverted because they were not faster, with conditions where they could still pay off. |

## Internals and design specs

| Document | What it covers |
|---|---|
| [Architecture](ARCHITECTURE.md) | Library boundaries, storage formats, query planner, execution modes, and architectural tradeoffs. |
| [Query optimizations](internals/OPTIMIZATIONS.md) | Operator fast paths, eligibility rules, and the local cost-model measurements behind them. |
| [Roadmap](ROADMAP.md) | Planned work in priority order, with size, impact, dependencies, and design spec links. |
| [Design specs](specs/README.md) | Design rationale, contracts, status, and recorded outcomes for each major feature. |
| [Provenance](specs/PROVENANCE.md) | Index of design sources, adopted dependencies, licenses, and rejected alternatives. |
| [Source audit](AUDIT.md) | Historical audit of Jena, QLever, and Oxigraph that informed the initial architecture. |

## Contributing

| Document | What it covers |
|---|---|
| [Development](DEVELOPMENT.md) | Workspace layout, Boxington builds, test suites, batch gates, mise tasks, and Nix setup. |
| [UI development](../ui/README.md) | Building, testing, and modifying the SvelteKit web UI. |
| [Remote executors](REMOTE-EXECUTORS.md) | Running builds, gates, and benchmark screens on local and Boat workers. |

## Maintaining documentation

* Keep workflows and examples in [Usage](USAGE.md), exact HTTP contracts in
  [HTTP API](API.md), and system design in [Architecture](ARCHITECTURE.md). Link between
  them instead of duplicating content.
* [Features](FEATURES.md) owns the capability matrix, while the category guides own
  current scope, configuration, usage links, and known gaps. [Comparison](COMPARISON.md)
  explains what those boundaries mean when migrating from another engine.
* Benchmark categories keep methodology, manual reports, generated tables, and immutable
  run catalogs distinct. Local optimization notes stay in
  [Query optimizations](internals/OPTIMIZATIONS.md) and identify themselves as local.
* Design specs preserve their original design. Update a spec's status and Outcome section
  alongside user-facing guides when phases ship; a specified phase is not a shipped
  feature.
* Preserve heading anchors when editing or moving sections, or update every link that
  targets them. Run `mise run lint:doc-paths` to check cited source paths and links on
  public entry pages, and `mise run test:quickstart` to test the README quick start
  against a freshly built server.
