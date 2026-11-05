# Standalone desktop application

## Purpose

Provide an installable desktop version of the Sparkles UI that opens local datasets
and connects to remote Sparkles servers. Preserve the web interface's workflows,
permissions, query semantics and editing safeguards as both interfaces evolve.

## Architecture decision

The initial application reuses the existing Svelte UI through Tauri or Electron.
Choose between them from a representative prototype, supported operating systems,
packaging requirements and the local engine lifecycle. Keep the evolving UI shared
with the browser application so its features and interactions advance together.

Migrate to GPUI after the UI and its interactions have stabilized. Before starting the
native interface, record the stable workflows and how future web changes will reach
both applications. Cover the query editor, graph rendering, ontology trees,
accessibility and keyboard navigation in the migration plan.

Record startup time, idle and workload memory, frame times and large tree and graph
interaction for the initial application and the GPUI prototype. Use matched datasets
and hardware and report the supported operating systems. Set measurable acceptance
thresholds from the existing application before timing the native candidate. Cut over
only after it meets the agreed performance and parity requirements.

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

Phase 1 selects Tauri or Electron and produces a representative prototype covering
query editing and results, a large ontology tree, an entity edit with a stale-commit
conflict, and Explore's graph interaction. Document what can be shared with the web UI,
what must be implemented separately and how a web change reaches the desktop release.

Phase 2 delivers the agreed desktop scope and installers for the operating systems
selected in Phase 1. Cover local dataset creation and reopening, clean shutdown,
recovery after a forced exit and authenticated remote connections. Document packaging,
updates and the compatibility policy before shipping.

Phase 3 migrates the stabilized interface to GPUI. Use the shipped desktop application
as the workflow and measurement baseline, keep it available during migration and
switch releases only after parity and performance acceptance. Record remaining web
maintenance responsibilities before retiring the initial desktop implementation.

Use shared fixtures and workflow expectations for the desktop and web interfaces.
Verify the main pages, rendering preferences, keyboard actions, accessibility, graph
scope, read-only access and editing conflicts. Record any deferred surface explicitly
in the capability documentation and roadmap. Benchmark the release build against the
agreed baseline under thresholds set before each comparison before claiming a
performance gain.

## Dependencies

Build on the shared application shell and rendering policy, then the initial
workbench hierarchy, metrics and inferred views in the roadmap's UI sequence.
The evaluation can proceed while later web features are developed, but delivery
must include a plan for keeping those features aligned. Correctness and performance
regression repairs retain priority over this application.

## Provenance

This design records the maintainer's request to start with Tauri or Electron sharing
the web UI, then migrate to GPUI once the UI and its interactions stabilize.
It draws its behavioral contract from the existing Sparkles UI and engine. No external
framework implementation or third-party source code was used to choose an architecture.
Framework support, licensing and platform constraints remain evaluation work.

## Outcome

No desktop application has shipped. Initial Tauri or Electron delivery and the later
GPUI migration remain planned under [item 746](../../ROADMAP.md).
