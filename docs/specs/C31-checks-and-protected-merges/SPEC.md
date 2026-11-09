# C31: Checks and protected merges

> **Status:** planned.
>
> **Roadmap:** [item 459](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

The design covers gate suites of SHACL, ShEx, lint, reasoning and quality checks defined in RDF, required checks on protected branches, breaking-change analysis, change-request workflows and comments.

## Problem and goal

Protected branches accept merges that pass the write guard, but nothing else is
checked. A vocabulary release can break the datasets that depend on it without warning,
and review on branches has no states, required reviewers or comments.

The goal is continuous integration for knowledge graphs inside the server.

## Scope

A gate suite is an RDF description of checks with severities. A protected branch can
require a suite to pass on the merge candidate's dry-run state, with an authorized
override recorded otherwise. Check results are kept per commit.

## Candidate phases

**Ontology quality report pack.** Ship SPARQL and SHACL checks for the ROBOT report set, OOPS! pitfalls, qSKOS and SKOS integrity, OBO principles and FOOPS! FAIR checks with ERROR, WARN and INFO levels, a report UI and sparkles ontology report, usable as a guard and a release gate. The checks are cheap and declarative. It depends on item 31.

**Gate suites.** Define in RDF a suite of checks (SHACL, ShEx, lint, RL diagnostics, unsatisfiable classes, quality packs, stored-query must-be-empty, breaking-change) with severities. It is continuous integration for knowledge graphs.

**Required checks on protected branches.** Run the gate suite on the merge candidate's dry-run state, attach results to the merge request and refuse the merge unless it passes or an authorized override is recorded. Protected branches accept merges but require no checks beyond the write guard.

**Breaking-change analysis for vocabularies.** Diff a vocabulary release against dependents' pinned usage, classify it as major, minor or patch, and block or warn. Shared vocabularies must not silently break datasets.

**Per-commit check records.** Store gate results per commit as F22 run records or U07 quality records with stale detection when inputs change. It keeps a history of quality for review and audit. It depends on items 31 and 340.

**Change-request workflow on branches.** Add workflow states (draft, review, approved, merged, rejected) on branches, required reviewers by role, per-change comments and an audit record, configurable as RDF and applied to every resource kind. TopBraid EDG workflows and VocBench validation work this way.

**Comments and discussions on resources.** Store threads as W3C Web Annotation RDF anchored to an IRI, axiom or diff line, with resolve state, mentions and C21 notifications, shown in Explore, forms and the review page. WebProtégé and EDG offer the same collaboration. It depends on item 21.

**Embedded gate runner for git CI.** Ship a CLI and GitHub Action that load a repository into an in-memory dataset, run the gate suite and emit JUnit or SARIF. It runs pull-request checks without a server.

**Role presets and a commenter permission.** Map viewer, commenter, editor and manager presets onto existing grants and add a commenter permission. WebProtégé and EDG expose roles rather than raw grants.

**Check status badges and history.** Show gate results on the commit graph and merge page and expose a status API for external CI. No status view exists.

## Dependencies

* The resource catalog of [C30](../C30-resource-catalog/README.md) supplies pinned
  imports and dependents for breaking-change analysis.
* Item 31 supplies the quality records, item 21 the forms where comments appear, and item
  340 the run manifests that check records adopt later.

## Acceptance gates

* A merge that fails a required check is refused unless an authorized override is
  recorded, and the refusal reports each failed check.
* Check results recorded for a commit are marked stale when the inputs they read change.

## Non-goals

* Hosted pull-request services. The embedded gate runner supplies checks to external
  CI instead.

## Design constraints

Phase 1 stores check results itself in a managed graph. It adopts item 340's run
manifests once those exist, without changing the recorded results.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: ROBOT report, OOPS!, qSKOS, OBO Dashboard, FOOPS!, ROBOT verify and report, ODK, GitHub checks, GitHub branch protection, TopBraid EDG workflows, LinkML schema diff, semantic versioning, TopBraid EDG, VocBench, PoolParty, WebProtégé, ROBOT in CI, WebProtégé sharing and EDG roles. No code from another project is copied.

## Outcome

The checks, workflow and badge phases are not built, and
[item 459](../../ROADMAP.md) tracks them.

The storage format of the comments phase is chosen, and the ontology editor's
discussions of item 618 use it, as the
[U12 Outcome](../U12-ontology-workbench/SPEC.md#discussions) describes. A thread is a
W3C Web Annotation `oa:Annotation` motivated by `oa:commenting`, with an
`oa:TextualBody`, `dcterms:creator` and `dcterms:created`, `schema:mentions` for each
mentioned principal and `urn:x-sparkles:discussion#resolved` for its state. Its
`oa:hasTarget` is the resource's IRI, or an `oa:SpecificResource` whose source is the
resource and whose `oa:TextQuoteSelector` holds an axiom. A reply is an `oa:replying`
annotation that targets the thread. The annotations are kept in an ordinary named graph
beside the graph they discuss, named by that graph's IRI under
`urn:x-sparkles:discussions:`, and each comment, resolve or reopen
is an ordinary commit. They therefore appear in history and exports, protected branches
refuse them and writing needs `write`. Item 769 covers an optional separate store that
keeps comments outside versioning. Diff-line anchors, C21 notifications and the threads
in Explore, forms and the review page remain open.
