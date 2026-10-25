# Questions and ingestion in natural language

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

See the [spec landing page](README.md), [design](SPEC.md) and
[delivery record](OUTCOME.md) for their respective contracts.

- **Spec:** [`C18-natural-language-questions-and-ingest.md`](README.md),
  written on 2026-10-09 from:
  - the Model Context Protocol specification, revision 2026-07-28 and its changelog
    (multi round-trip requests, elicitation, the deprecation of sampling), revisions
    2025-06-18 and 2025-11-25, and the MCP Apps extension;
  - W3C SPARQL, RDF 1.2, SHACL, PROV-O and the Web Annotation Data Model, RFC 5147 for
    `#char=` fragments, and the NLP Interchange Format (Hellmann et al., ISWC 2013);
  - the QALD-9, QALD-9-plus, QALD-10, LC-QuAD, DBLP-QuAD, SciQA, Spider4SPARQL and
    TEXT2SPARQL'25 benchmarks, and SPINACH, SPARQLGEN, FIRESPARQL, mKGQAgent, SPARQL-LLM
    and Gashkov et al. on memorization in text-to-SPARQL;
  - DIN-SQL, DAIL-SQL, Self-Debugging, BIRD and Spider 2.0 from text-to-SQL;
  - GraphRAG, Zhu et al. on knowledge graph construction with language models, KGGen,
    iText2KG, Text2KGBench, REBEL, LightRAG and Docling;
  - Zep, Mem0, A-MEM, LongMemEval and LoCoMo on agent memory;
  - the OWASP Top 10 for LLM Applications 2025, Simon Willison's "lethal trifecta"
    (2025), OpenAI's and Anthropic's guidance on API keys in browsers, and the
    structured output documentation of Ollama, OpenAI and llama.cpp;
  - FrugalGPT (Chen, Zaharia and Zou, 2023) and RouteLLM (Ong et al., ICLR 2025) on
    cascades and routers, and Anthropic's model and pricing documentation of October
    2026 for the example configuration;
  - pdf-inspector 1.25.2 (MIT), its README, crate documentation and source, read for
    its API, OCR reasons, Markdown options and features, and its dependency tree with
    `cargo tree`;
  - the Sparkles code, including the plan structures of `sparkles-core` and the UI's
    `PlanView`, and specs C01, C02, C05, C09, C10, C11, C12, C12b, C15, C16, C17, F03,
    F04, F06, F08, F09, X04 and X05.

  The revision of the same day added model roles with escalation, explanations, the
  optimizer and the choice of pdf-inspector. A later revision of that day added the import of
  coding agents' memory and the `sparkles memory` commands, written from Claude Code's
  documentation on memory and hooks, OpenAI's Codex documentation on `AGENTS.md`, hooks
  and memories, Gemini CLI's and Cursor's documentation of their instruction files, the
  documented token formats of common providers for redaction, the structure of local
  Claude Code and Codex files read only for their keys, and specs G05 and P02.
- **Implementation:** not built. Phase 1's clients for Ollama, OpenAI-compatible
  endpoints and Anthropic's API reuse the outbound HTTP client of F08, and the rest of
  Phases 1 to 3 plans no new runtime dependency. Phase 4 adds `pdf-inspector` at an
  exact version (MIT) behind the `pdf` feature, which brings 22 crates under MIT,
  Apache-2.0 or Unlicense, and optionally its OCR features behind `pdf-ocr`, which
  bring about 132 more under permissive licenses, rav1e's patent license among their
  notices. An HTML extractor for Phase 4 will be recorded here when chosen.
- **Rejected** (spec §13):
  - the browser calling a model provider, MCP sampling, and a delegated agent as the
    UI's model;
  - a free agent loop inside the server;
  - answers that hide their query;
  - an intermediate query language compiled to SPARQL;
  - retrieval over chunks without facts;
  - committing ingestion to `main` by default;
  - new predicates proposed by ingestion;
  - ask history in a graph of the dataset;
  - running queries from a handoff link automatically;
  - holding conversation facts on a review branch by default;
  - a review flag stored on each fact;
  - agents that merge their own branches;
  - model confidence as a signal for bulk acceptance;
  - MCP Apps;
  - OCR by default, and media transcription;
  - a built-in PDF text extractor, and an external PDF converter configured like a
    provider;
  - a router model that picks the model for each question, and a single model per
    dataset;
  - applying rewrites automatically, a planner that calls a model, and rewrites
    checked by estimate alone;
  - the server's providers behind MCP's `explain_query` and `optimize_query`.
