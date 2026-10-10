# Documentation

Start with the guide for the task you are doing. The repository's
[README](../README.md) introduces Sparkles and has the quick start.

## Guides

| Task | Read |
|---|---|
| Run the server, import data, query or administer a dataset | [Usage](USAGE.md#running-the-server), including [CLI tools](USAGE.md#command-line-tools), [Docker](USAGE.md#docker) and [NixOS](USAGE.md#deploying-on-nixos) |
| Embed the engine or connect an application | [Rust library](USAGE.md#embedding-the-library), [Python](USAGE.md#python), [JVM (Jena)](USAGE.md#jvm-apache-jena), [Node.js](USAGE.md#javascript-and-typescript) and [Rust remote client](USAGE.md#rust-client) |
| Connect an agent, use memory or configure models | [MCP](USAGE.md#mcp-server-llm-agents), [agent memory](USAGE.md#agent-memory) and [questions and models](USAGE.md#asking-questions-with-a-model) |
| Understand storage, execution and library boundaries | [Architecture](ARCHITECTURE.md) |
| Set up an editor | [Editor integration](editors.md) |
| Build, test or contribute | [Development](DEVELOPMENT.md) and [UI development](../ui/README.md) |

## Reference and evidence

| Document | Owns |
|---|---|
| [Features](FEATURES.md) | The capability inventory and current [known gaps](FEATURES.md#known-gaps) |
| [HTTP API](API.md) | Request and response contracts, status codes, parameters, formats and protocol semantics |
| [OpenAPI](openapi.json) | The machine-readable HTTP contract, checked against the route table |
| [Comparison](COMPARISON.md) | Compatibility differences, tradeoffs and gaps against other engines |
| [Benchmarks](BENCHMARKS.md) | Published performance results, measurement context and comparison methodology |
| [Running benchmarks](BENCHMARKING.md) | Harness commands, modes, outputs and regression-measurement instructions |
| [Query optimizations](internals/OPTIMIZATIONS.md) | Detailed operator fast paths, applicability rules and local cost-model measurements |
| [Design specs](specs/README.md) | Feature rationale, delivery status and implementation Outcomes |
| [Source audit](AUDIT.md) | The historical audit of Jena, QLever and Oxigraph, and the original design choices |
| [Provenance](specs/PROVENANCE.md) | Design sources, adopted dependencies, licenses and rejected alternatives |

## Maintaining documentation

* Put workflows and examples in Usage, exact HTTP contracts in API, and system
  structure in Architecture. Link between them when a reader needs the other view.
* Keep current capability and limitation statements in Features. Comparison should
  explain their consequence for compatibility or migration, with links to the details.
* Keep published timings and their setup in Benchmarks. Local optimization
  measurements belong with their rationale and must be identified as such.
* Preserve a spec's original design. Update its status and Outcome alongside a
  behavior change, and update the relevant user guide. A specified phase is not a
  shipped capability.
* Treat the source audit as historical context. Use the current guides for supported
  behavior. Preserve existing heading anchors when moving sections, or update every
  reference to them.
* Check local links and cited source paths after moves. `mise run lint:doc-paths`
  checks source-path citations. It does not check all Markdown links or anchors.
