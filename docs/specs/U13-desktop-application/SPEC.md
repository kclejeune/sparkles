# Standalone desktop application

## Purpose

Provide an installable desktop version of the Sparkles UI that opens local datasets
and connects to remote Sparkles servers. Preserve the web interface's workflows,
permissions, query semantics and editing safeguards as both interfaces evolve.

## Architecture decision

GPUI is the preferred candidate for a native interface. Its selection depends on a
prototype demonstrating useful performance gains and a sustainable approach to feature
and interaction parity. A native rewrite must account for the query editor, graph
rendering, ontology trees, accessibility, keyboard navigation and future web changes.

Compare that prototype with an application that reuses the existing Svelte UI through
Tauri or Electron. Record startup time, idle and workload memory, frame times, large
tree and graph interaction, packaging effort and the amount of duplicated UI logic.
Use matched datasets and hardware and report the supported operating systems. Choose
measurable acceptance thresholds from the web baseline before timing the candidates.
Choose the framework only after the comparison shows how parity will be checked and
whether those thresholds are met. If maintaining a separate interface risks drift,
prefer reuse of the existing UI.

## Application contract

The application offers local datasets without requiring a separately managed server,
and authenticated connections to remote servers. The architecture decision specifies
whether local operation embeds the engine or manages a bundled server, and defines
its lifecycle, exclusive store ownership, crash recovery and upgrade compatibility.
Reuse the existing engine and API contracts rather than introducing desktop-specific
query or ontology semantics.

Keep dataset, branch and revision scope explicit. Preserve access checks, commit
preconditions, write previews, cancellation and unsaved-edit warnings. Store credentials
through an operating-system credential facility and keep them out of URLs and logs.
Define trusted application content, remote navigation, file access and any native
bridge so a remote server or dataset cannot gain local privileges.

## Delivery and acceptance

Phase 1 produces the architecture decision and a representative prototype covering
query editing and results, a large ontology tree, an entity edit with a stale-commit
conflict, and Explore's graph interaction. Document what can be shared with the web UI,
what must be implemented separately and how a web change reaches the desktop release.

Phase 2 delivers the agreed desktop scope and installers for the operating systems
selected in Phase 1. Cover local dataset creation and reopening, clean shutdown,
recovery after a forced exit and authenticated remote connections. Document packaging,
updates and the compatibility policy before shipping.

Use shared fixtures and workflow expectations for the desktop and web interfaces.
Verify the main pages, rendering preferences, keyboard actions, accessibility, graph
scope, read-only access and editing conflicts. Record any deferred surface explicitly
in the capability documentation and roadmap. Benchmark the release build against the
web baseline under the Phase 1 thresholds before claiming a performance gain.

## Dependencies

Build on the shared application shell and rendering policy, then the initial
workbench hierarchy, metrics and inferred views in the roadmap's UI sequence.
The evaluation can proceed while later web features are developed, but delivery
must include a plan for keeping those features aligned. Correctness and performance
regression repairs retain priority over this application.

## Provenance

This design records the maintainer's request for a standalone desktop UI, with GPUI
preferred for performance and Tauri or Electron acceptable when they reduce drift.
It draws its behavioral contract from the existing Sparkles UI and engine. No external
framework implementation or third-party source code was used to choose an architecture.
Framework support, licensing and platform constraints remain evaluation work.

## Outcome

No desktop application has shipped. Framework selection, the prototype, packaging and
parity validation remain planned under [item 746](../../ROADMAP.md).
