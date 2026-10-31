# C34: Audit and erasure

> **Status:** planned.
>
> **Roadmap:** [item 466](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

The design covers a tamper-evident audit stream of data access and agent activity, personal-data erasure across history and backups, field-level encryption with crypto-shredding, PII detection, comparable digests and verifiable credentials.

## Problem and goal

The audit log covers dataset and branch administration only, so regulated users
cannot audit reads, writes or MCP tool calls. Agent memory over personal data needs a real
deletion of a subject's facts, which C17 §3.3 and C18 §8.4 leave to a separate design.

The goal is a durable audit of data access and a verifiable erasure path.

## Scope

The audit stream records queries, writes and tool calls with principal, query hash,
argument digest, graphs touched and outcome, with an optional hash chain and a queryable
form. Erasure removes a subject's facts from the current state, retained generations,
history and backup repositories, and produces a verifiable record. Field-level
encryption with destroyable keys supports erasure where rewriting history is too costly.

## Candidate phases

**Durable, tamper-evident data-access and agent activity audit.** Write a separate audit stream of queries, writes and MCP tool calls with principal, query hash, argument digest, graphs touched and outcome, with an optional hash chain and a queryable form. `sparkles::audit` covers dataset and branch administration only (item 62, completed). Regulated users and agent governance need read audit. It depends on items 42 and 222.

**Personal-data erasure across history and backups.** Design a real deletion of a subject's facts from current state, retained generations, history and backup repositories, with a verifiable record. Agent memory over personal data needs it. It depends on items 52, 61 and 65.

**PII detection across ingestion and results.** Extend the personal-data detectors of ingestion redaction to server ingestion and to result shaping for model providers. C18 client redaction has personal detectors for email, phone, cards, IBAN and IP (C18 OUTCOME). Server ingestion redaction is item 77. It depends on items 77 and 395.

**Field-level encryption and crypto-shredding.** Encrypt chosen predicates or literals with separate keys and destroy keys to erase. Item 65 covers dataset and index encryption only. It depends on item 65.

**Digest coverage for bulk commits and comparable digests.** Digest bulk commits and define an RDFC-based digest that matches across databases. The chain restarts after a bulk load and uses internal blank-node labels. It depends on item 65.

**Author attribution through crash recovery.** Persist the author with the commit so WAL-recovered commits keep it. A crash drops attribution for the last commits.

**Verifiable credentials and DIDs.** Authenticate with or sign imports and exports using VCs and DIDs. No support exists.

## Dependencies

* Item 42 supplies MCP observability and item 222 view access.
* Items 61, 52 and 65 supply backups, history and encryption that erasure must reach.
* Items 77 and 395 supply ingestion redaction and bounded permission preparation.

## Acceptance gates

* After an erasure, no read at any retained commit, no restored backup and no derived
  index returns the erased facts, and the erasure record verifies.
* The audit stream survives a crash without losing an acknowledged request.

## Non-goals

* Searchable or order-preserving encryption, which F11 keeps out of scope.

## Design constraints

Every new on-disk record has a version and a clean refusal of unknown
versions.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: PostgreSQL pgaudit, enterprise audit practice, GDPR Article 17 practice, Datomic excision, Enterprise RAG guardrails, MongoDB queryable encryption, W3C VC Data Model and DID Core. No code from another project is copied.

## Outcome

Nothing is built. [Item 466](../../ROADMAP.md) tracks the work.
