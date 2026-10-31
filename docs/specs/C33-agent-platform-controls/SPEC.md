# C33: Agent platform controls

> **Status:** planned.
>
> **Roadmap:** [item 462](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

The design covers mCP tool profiles, shape templates, an approval queue for agent updates, data-change resources, composite tools, branch handoff, framework adapters, in-process memory APIs and A2A skills.

## Problem and goal

The MCP server exposes about 34 tools from one schema to every session, which costs
tokens and confuses small models. Only `assert_facts` has a review path, and agents poll
for changes. Agent frameworks that do not speak MCP reach the memory tools only through
the generic HTTP routes, such as `POST /{ds}/recall` and `POST /{ds}/facts`, because no
adapter fits them into those frameworks' abstractions.

The goal is a set of controls and adapters that let agents use Sparkles safely and
cheaply from the hosts and frameworks they run in.

## Scope

The controls govern which tools a session sees, which updates need review and how work
passes between principals. The adapters expose the same operations to Python and
JavaScript agent frameworks, the bindings and A2A. Tools that extend the MCP server
directly are follow-ups of [C11](../C11-mcp-server.md#outcome), memory operations are
follow-ups of [C17](../C17-agent-memory.md#outcome), and Ask operations are follow-ups of
[C18](../C18-natural-language-questions-and-ingest/README.md).

The cheap tools come first. These are the `ask` tool, item 488, tool profiles, item 489,
and a hybrid search tool that embeds query text, item 490.

## Candidate phases

**MCP tool profiles.** Add `--mcp-tools` and `--mcp-profile` subsets such as explore, memory and admin. About 34 tools from an 80 KB schema source cost tokens every session and confuse small models. Roadmap item 489 holds this phase.

**Shape template tool.** Return a JSON schema and example for `assert_facts` from a class's SHACL shapes. Well-formed writes become the default instead of a refusal loop. It depends on item 21.

**Approval queue for sparql_update.** Redirect general agent updates to a review branch by policy, as `assert_facts` does under conversationFacts review. Only `assert_facts` has a human-in-the-loop path. It depends on item 435.

**MCP data-change resources.** Publish a resource per query, graph or entity with resources/updated on change. Agents poll /changes. Webhooks on matched data changes are already planned as C23 trigger actions under item 36, so only the MCP side is new. It depends on items 36, 339 and 424.

**LangChain and LlamaIndex adapters.** Ship GraphStore, PropertyGraphStore, retriever and chat-memory classes for Python frameworks. Most agent builders start in these frameworks, and MCP alone does not plug into their abstractions. It depends on item 58.

**Composite agent tools.** Add `impact_analysis`, recommend and `explain_path` tools composed from algorithm, path and vector calls. Agents compose poorly over raw SPARQL plus algorithm calls.

**Branch handoff between principals.** Share a scratch branch with another agent for review. Supervisor and worker setups pass work on branches. It depends on item 435.

**JavaScript agent adapters.** Ship Vercel AI SDK tools and LangChain.js over the HTTP API. It covers the TypeScript agent ecosystem. It depends on item 58.

**In-process memory APIs in the bindings.** Expose recall, link and assert in the Python, Node and JVM bindings. Embedded agents get memory without a server. It depends on item 58.

**Thin framework adapters for CrewAI, Haystack and DSPy.** Wrap the Python adapters for these frameworks. They are thin once the Python adapters exist. It depends on item 58.

**A2A agent card and task endpoint.** Expose Ask, ingestion and GraphRAG as A2A skills. Other agents could delegate graph tasks. It depends on item 339.

## Dependencies

* Items 81, 341, 396 and 435 supply evidence, valid time, entities and scratch-branch
  limits that the approval queue and handoff use.
* Item 339 supplies durable jobs and the outbox, which data-change resources and other
  triggers wait for.
* Item 58 supplies published packages for the framework adapters.

## Acceptance gates

* Every tool keeps the caller's grants, budgets and disclosure guards, and a profile only
  narrows the tools a session sees.
* An update redirected to review commits nothing to its target branch until a person
  accepts it.

## Non-goals

* Generative models inside the server, which F12 keeps out of scope.

## Design constraints

Agent-facing writes keep provenance, review and supersession as C17 defines
them.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: GitHub MCP toolsets, Neo4j data-modeling MCP, SHACL forms, Stardog and GraphDB governed writes, Neo4j CDC, Zep events, Neo4j, GraphDB and Stardog integrations, Multi-agent frameworks, Vercel AI SDK, Cognee, Mem0 embedded and Google A2A. No code from another project is copied.

## Outcome

Nothing is built. [Item 462](../../ROADMAP.md) tracks the work.
