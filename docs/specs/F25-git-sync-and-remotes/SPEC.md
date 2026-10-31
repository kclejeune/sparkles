# F25: Git sync and remotes

> **Status:** planned.
>
> **Roadmap:** [item 460](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

The design covers a canonical git serialization, export of branches to git, import and sync from git refs, and remotes with fetch, push and pull between Sparkles servers.

## Problem and goal

Ontology-as-code workflows keep vocabularies in git and review them on GitHub, but
Sparkles cannot write or read a git repository. Item 45 covers one-way clones and
archives, and no operation moves divergent history between servers.

The goal is two-way sync with git and with other Sparkles servers.

## Scope

A versioned canonical layout writes one file per graph in sorted, subject-grouped
Turtle or sorted N-Quads with stable prefixes and blank-node labels. Export maps commits
to git commits and records the mapping in both directions. Import brings a git ref onto a
branch as net-change commits and merges it through
[F09](../F09-branches-and-merges.md). Remotes fetch divergent history and push and pull
branches as RDF Patch streams over the change feed, with commit identity preserved.

## Candidate phases

**Git serialization profile.** Define a versioned canonical layout of one file per graph in sorted subject-grouped Turtle or sorted N-Quads with stable prefixes and blank-node labels, refused when unknown. Text that diffs well in git is the base of every git workflow. The formatter already sorts with spill and writes RDFC-1.0.

**Git export of a branch.** Map Sparkles commits to git commits per commit or squashed, with author, message and trailers, and record the SHA mapping in both directions. Ontology-as-code workflows and GitHub review need commits that map to git commits.

**Git import and sync.** Import a git ref onto a git/<remote>/<ref> branch as net-change commits, merge with F09, trigger by inbound webhook and choose fast-forward or merge policy. It keeps vocabularies maintained in GitHub in sync with the dataset. It depends on item 339.

**Two-way server sync with remotes, fetch, push and pull.** Add remotes, fetch of divergent history and push and pull of branches between Sparkles servers as RDF Patch streams over the change feed with commit identity preserved, and merge after fetch. Item 45 covers one-way clone and archive transfer only. It depends on items 45, 66 and 74.

## Dependencies

* Items 280 and 66 settle the replication contract and replicas that remotes build on.
* Item 45 supplies cross-server cloning, item 74 multi-transaction RDF Patch, and item 339
  the durable jobs that run imports from webhooks.

## Acceptance gates

* Export followed by import reproduces the branch's quads and blank-node structure.
* The canonical layout has a version, and an unknown version is refused.
* A push or pull preserves commit identity and refuses a non-fast-forward update unless a
  merge is requested.

## Non-goals

**Rebase verb.** Moving a branch onto a newer base stays outside scope, because
replayed merges cover most uses. At most, a later phase could squash-replay scratch
branches that were never shared.

## Design constraints

Remotes reuse the change feed and RDF Patch rather than a new wire format.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: ROBOT sorted Turtle, rdf-toolkit, TerminusDB, Dolt, ODK, LinkML repositories and Git. No code from another project is copied.

## Outcome

Nothing is built. [Item 460](../../ROADMAP.md) tracks the work.
