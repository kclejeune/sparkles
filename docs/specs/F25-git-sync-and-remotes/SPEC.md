# F25: Git sync and remotes

> **Status:** in progress (Phases 1 and 2 shipped; Phases 3 and 4 remain).
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

Phases 1 and 2 are implemented in `sparkles::git` and the `sparkles git` CLI (`export`, `write-tree`, and `verify`).

* **Phase 1 (Git serialization profile).** `sparkles::git::profile` defines a versioned canonical layout (`format_version = 1`, profile `sparkles-git-v1`) writing `.sparkles-git/manifest.json` alongside one file per non-empty graph under `graphs/` in sorted, subject-grouped Turtle or sorted N-Quads with dataset-wide RDFC-1.0 (`SHA-256`) blank-node labels, feature-independent canonical formatting, composite-literal (`cdt:List`/`cdt:Map`) blank-node link preservation, and stable prefixes. Writes reject symlinks across the root, `.sparkles-git`, `graphs`, and manifest paths, prune only previously profile-owned graph files, and write via exclusively created temporary files. The streaming verifier (`verify_git_tree`), reader (`read_git_tree`), and additive dataset importer (`import_git_tree`, applying prefixes only after a committed non-dry-run transaction) preserve exact quads and cross-graph blank-node isomorphism while refusing unknown format versions with `GitError::UnsupportedFormatVersion`, duplicate JSON keys, traversal/symlink/non-regular paths with `GitError::UnsafePath`, unlisted graph files, oversized inputs or excessive blank-node canonicalization ambiguity, duplicate quads, and checksum or count mismatches.
* **Phase 2 (Git export of a branch).** `sparkles::git::export` exports a Sparkles branch (from either the owning dataset or a branch handle, and via `sparkles --branch ... git export`) to a standalone Git repository either commit-by-commit or squashed, preserving recorded authors (when the change log is enabled and retained), UTC timestamps, messages, and RFC 822 commit trailers (`Sparkles-Dataset-Id`, `Sparkles-Branch`, `Sparkles-Branch-Id`, `Sparkles-Commit`, `Sparkles-Commit-Kind`, `Sparkles-Timestamp`, `Sparkles-Format-Version`, and `Sparkles-Export-Mode`, plus origin, merge, replay, digest, author, and squash trailers). Exports scrub inherited Git environment overrides, refuse dirty indexes or ancestor-repository hijacking, stage only verified profile files via filter-free object hashing, start new Git branches as clean orphans, and persist versioned per-branch bidirectional Sparkles-commit to Git-SHA mappings (`format_version = 1`, profile `sparkles-git-mapping-v1`, stored under `<git-dir>/sparkles/mappings/` by default) after every committed revision with full lineage verification and unknown-version refusal.
* **Remaining work.** Phase 3 (git import onto `git/<remote>/<ref>` branches, F09 merge integration, and webhook-triggered sync, which depends on item 339) and Phase 4 (server remotes with fetch, push, and pull over RDF Patch change feeds, which depends on items 45, 66, and 74) remain tracked under [item 460](../../ROADMAP.md).
