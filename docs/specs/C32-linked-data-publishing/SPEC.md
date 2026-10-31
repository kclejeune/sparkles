# C32: Linked-data publishing

> **Status:** planned.
>
> **Roadmap:** [item 461](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

Content negotiation for hosted namespaces, term and ontology pages with generated documentation, catalog-wide term search and an OLS API subset.

## Problem and goal

Vocabularies need resolvable IRIs, and Sparkles has no content negotiation, no HTML
page per term and no search across the terms of its datasets.

The goal is to publish the vocabularies and datasets of one server as linked data.

## Scope

Requests for IRIs under hosted namespaces get HTML for browsers and RDF for agents.
Term and ontology pages render hierarchy, definitions, usage and examples, and an
ontology documentation generator serves them and exports them as static files. Term
search covers every catalog resource the caller can read.

## Candidate phases

**Linked-data front end.** Answer requests for IRIs under hosted namespaces with HTML for browsers and DESCRIBE RDF for agents, with Vary: Accept, 303 for slash IRIs, version IRIs resolved to snapshots and PURL or w3id recipes. Vocabularies need resolvable IRIs, and there is no content negotiation or HTML per term today.

**Term and ontology pages with a documentation generator.** Render server-side pages per term and per vocabulary or dataset with hierarchy, definitions, usage and examples, and generate Widoco-style ontology documentation served at `/$/ontology/{ds}/doc` and as a static export. OLS and Ontobee browsing and ontology publishing need human-readable docs. It depends on items 31 and 71.

**Catalog-wide term search.** Index labels, synonyms and definitions of every catalog resource the caller can read in one full-text and vector index with ontology facets and autocomplete. OLS offers the same search.

**Public hub pages.** Publish resource pages with README, licence, size, endpoints and downloads under owner/name addressing. Public hosting needs these pages. This phase is deferred until the condition under Design constraints holds.

**Public-hosting quotas and moderation.** Add per-owner storage quotas, anonymous rate limits and takedown for public hosting. A public hub needs them before it opens. This phase is deferred until the condition under Design constraints holds.

**Forks into a user namespace.** Fork by clone with forkedFrom or by branch for cheap forks. Hub users fork as their first step. This phase is deferred until the condition under Design constraints holds.

**Download and usage statistics.** Count downloads and queries per resource version with privacy controls. They show which resources a hub's users rely on. This phase is deferred until the condition under Design constraints holds.

**OLS API subset.** Serve an OLS4 REST subset over the catalog so existing clients keep working. Zooma, OxO and Protégé plugins call the OLS API.

**Stars, follows and user profiles.** Add per-user records and profile pages. Community features need them. This phase is deferred until the condition under Design constraints holds.

## Dependencies

* The resource catalog of [C30](../C30-resource-catalog/README.md) supplies hosted
  namespaces and vocabulary resources.
* Item 71 supplies the axiom model that ontology pages render, and item 31 the quality
  data they show.

## Acceptance gates

* A browser and an agent requesting the same IRI get HTML and RDF with `Vary: Accept`,
  and a slash IRI answers with a 303 redirect.
* Search and pages show only what the caller may read.

## Non-goals

* Federation between servers or a public hub without a compelling reason.

## Design constraints

Vocabularies are per server. The hub phases, which are public resource pages,
hosting quotas, forks into user namespaces, usage statistics and profiles, are later work
that waits for a compelling reason to host a public hub.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: Ontobee, Pubby, LodView, OLS, Widoco, pyLODE, BioPortal, DoltHub, Hugging Face Hub, data.world, GitHub and Kaggle. No code from another project is copied.

## Outcome

Nothing is built. [Item 461](../../ROADMAP.md) tracks the work.
