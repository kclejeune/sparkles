# F09: Branches and merges

> **Status:** implemented in part
>
> **Phases:** Phase 1 shipped. It covers branches of persistent datasets that share
> their upstream's index until they compact, reads and writes on a branch with
> `?branch=` and the path form, per-branch commits and history, three-way merges of quad
> sets with cell and subject conflicts and resolutions, deletion, holds and quotas,
> access control by branch, the HTTP API, the CLI, and the UI's branch selector,
> Branches panel and conflict-free Merge button. Of Phase 2, the UI's merge page, the
> History panel's commit graph with `GET /$/commit-graph/{ds}`, squash merges, reverts,
> replayed fast-forwards, renames, deletions that re-parent, predicates exempt from
> conflicts, merges as tasks, per-branch gauges and the backup manifest's count of
> left-out branches have shipped, and so have Python bindings, in-memory branches,
> selected-branch backups, upstream full-text/spatial/vector index reuse, and
> cherry-picks and automatic recursive virtual merge bases from Phase 3. Explicit
> relinking of persistent linked branches is available in Rust, the offline CLI, the
> HTTP API (`POST /$/branches/{ds}/{name}/relink`) and the Python, Node and JVM
> bindings. Cross-server clones are not built.
>
> **User docs:** [API: Branches and merges](../API.md#branches-and-merges) ·
> [Usage: Branches and merges](../USAGE.md#branches-and-merges) ·
> [Features](../FEATURES.md#storage-tdb2-equivalent)
>
> This is the design as written before implementation. The [Outcome](#outcome) section
> at the end records how it landed.

This spec is Phase 3 of [C06](C06-clone-to-sandbox.md), which listed
"cloning at a past commit, cloning across servers through archives, and branch and merge
semantics" as later work. Cloning at a past commit shipped with
[F06 Phase 2](F06-snapshots-and-point-in-time.md#phase-2), so this spec covers it only
where branches change it (§2.11). The design builds on durable commit identity
([CI](CI-commit-identity.md)), retained generations, diffs and the change feed
([F06](F06-snapshots-and-point-in-time.md)), automatic compaction
([C13](C13-automatic-compaction.md)), backup repositories
([F05](F05-snapshot-repositories.md)), write-time validation
([C10](C10-write-time-validation.md)), write previews ([C15](C15-write-previews.md)),
inference freshness ([C08](C08-inference-freshness.md)) and access control
([C09](C09-dataset-access-control.md), [C12](C12-graph-access-control.md)).

The model of branches, merge bases and three-way merges comes from Git. The handling of
conflicts and their resolution draws on the public documentation of Dolt, lakeFS and
Project Nessie, and the idea of a branch as a cheap layer over immutable data on
TerminusDB's and lakeFS's documentation and on Datomic's speculative databases. The
quad-level merge rule is the three-way merge of sets from the literature on diff3 and
on replicated sets. §10 lists the sources.

## 1. Summary, goals, non-goals

A team that edits a knowledge graph often wants to stage a change before it reaches the
data everyone reads. Today Sparkles offers three partial answers. A clone
([C06](C06-clone-to-sandbox.md)) is an independent copy, but it costs a full index
build, and nothing brings its changes back. A write preview ([C15](C15-write-previews.md))
shows what one write would do and then rolls it back, so it cannot hold a change that
takes several steps or several people. A named snapshot ([F06](F06-snapshots-and-point-in-time.md))
keeps a past state readable, but nobody can write to it.

A **branch** fills the gap. It is a named, writable line of commits that starts from a
commit of another branch. It shares that branch's index files until it compacts, so
creating one costs milliseconds and almost no disk. Readers and writers choose a branch
with `?branch=NAME` or the path form `/{ds}@NAME/…`. A **merge** brings the changes of
one branch into another. When the target has not moved since the branches parted, the
merge is a fast-forward. Otherwise Sparkles computes a three-way merge of quad sets
against their common ancestor. It reports a conflict when both sides changed the same
subject and predicate of the same graph in different ways, and it lets the caller
resolve conflicts by rule or one by one.

Every dataset has a branch called `main`. It is the dataset as it exists today, so a
client that never names a branch sees no change.

**Goals**

- Creating a branch takes milliseconds and writes a few kilobytes, whatever the size of
  the dataset. The branch shares the base index and the write-ahead log prefix of the
  branch it came from. It keeps its own delta, its own log and its own commits.
- Writes to different branches never wait for each other. Each branch has its own writer
  lock, its own compaction and its own history.
- Every read and write endpoint works on every branch with the same semantics. A plain
  SPARQL client reaches a branch through an endpoint URL alone.
- A merge is a single commit on the target. It is crash-safe, it passes the target's
  write-time validation, and it can be previewed without committing.
- Conflicts are defined precisely, reported in full and resolvable without server-side
  merge state.
- Blank nodes keep their labels across branches and merges. A label that a client saw
  on a branch names the same node on the target after a merge.
- Nothing changes for datasets without branches. There is no new cost on the write path
  of `main`.

**Non-goals (Phase 1)**

- Branches of in-memory datasets (Phase 2).
- Rebasing a branch onto a newer commit of its upstream. Merging the upstream into the
  branch serves the same purpose and keeps commit identities stable (§8).
- Merging configuration. Shapes, reasoning settings, prefixes, full-text settings and
  compaction settings belong to each branch and are not merged.
- Detecting that two blank-node structures added on the two sides are isomorphic. They
  are different nodes, and a merge keeps both.
- Moving refs. A branch is a line of commits with its own numbering. It is not a pointer
  that a fast-forward moves (§4.6).
- Remote branches, push and pull between servers. The replication work covers shipping
  commits between servers. This spec covers only a one-off clone from another server
  (Phase 3).

## 2. User-visible behaviour

### 2.1 Names and identity

A branch name matches `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`, the rule for snapshot names.
It must contain at least one letter, and it cannot be `head`, so a name never reads as a
commit selector. Names are case-sensitive. `main` exists in every dataset and cannot be
created, renamed or deleted.

Each branch has a **branch id**, a version 4 UUID minted when the branch is created. The
branch id names the branch's lineage in the sense of [CI §2.1](CI-commit-identity.md).
The branch id of `main` is the dataset id, so the identifiers that clients already hold
for `main` do not change. A deleted branch's name can be used again, and the new branch
gets a new id.

Each branch also has an **ordinal**, a number from 1 to 65,535 that is never reused
within a dataset. `main` has ordinal 0. The ordinal partitions blank-node ids (§4.8).

### 2.2 Commits on a branch

A branch continues the commit numbering of the commit it starts from. A branch `dev`
created at commit 42 of `main` makes commit 43 as its first commit, while `main` may make
its own commit 43 at the same time. The pair of branch id and seq identifies a commit, as
[CI](CI-commit-identity.md) defines for the dataset id and seq.

The history of a branch is its own commits plus the history of its starting point. On
`dev`, commits 0 to 42 are the commits of `main`, and commits from 43 on are the commits
of `dev`. A commit listing on `dev` shows both, and each commit names the branch that
made it. This mirrors Git, where a branch's log reaches back through the commits it
shares with other branches.

A commit gains three fields in JSON:

| Field | Meaning |
|---|---|
| `branch` | The name of the branch that made the commit, or `null` when that branch was deleted. |
| `branchId` | The id of the branch that made the commit. |
| `mergedFrom` | For a merge commit, the merged commit as `{branch, branchId, seq}`. Absent otherwise. |

`parent` keeps its meaning of `seq − 1`. On a branch's first commit it is the commit the
branch started from. A merge commit's first parent is the previous commit of the target,
and `mergedFrom` is its second parent.

A new commit kind, `merge` with the next free code (13, after the `embed` kind of [F08](F08-embeddings-on-write.md) and the `patch` kind of [F10](F10-replication.md)), marks merge commits. Creating or deleting a
branch makes no commit, just as creating a snapshot makes none.

The commit IRI that RDF Patch output uses becomes `urn:uuid:<branch id>#commit:<seq>`.
For `main` this is the IRI it has today.

### 2.3 Choosing a branch in a request

A request chooses a branch in one of two ways.

- The query parameter `branch=NAME` works on every endpoint of a dataset, in the query
  string or in the form body of a form POST, like `at`. A request that gives `branch`
  twice with different values gets `400`.
- The **path form** `/{ds}@{branch}` replaces `/{ds}` in every dataset route, for example
  `/prod@dev/sparql`, `/prod@dev/update` and `/prod@dev/data?graph=…`. Dataset names
  cannot contain `@`, so the form is unambiguous. It lets a client that only takes an
  endpoint URL, such as Jena's `RDFConnection`, rdflib's SPARQL store or a SPARQL
  notebook, work on a branch.

When both are given they must agree, or the request gets `400`. Without either, the
request works on `main`. Admin routes under `/$/` take `branch` as a parameter only.

Every response from a branch other than `main` carries two more headers:

```
Sparkles-Branch: dev
Sparkles-Branch-Id: 9d0c41e2-…
```

`Sparkles-Commit` gives the seq on that branch, and `Sparkles-Dataset-Id` keeps giving the
dataset id. CORS exposes both new headers. Entity tags on a branch use the branch id,
`W/"<branchId>:<seq>:<format>"`, so `main`'s tags do not change and two branches never
share a tag. The result cache keys on the generation instance and the commit, as
[F06 §5.5](F06-snapshots-and-point-in-time.md) changed it, and each branch has its own
generation instance (§5.2), so branches never share cached results by mistake.

`at` works on a branch as it does on `main`. A selector resolves within the branch's
history, so `?branch=dev&at=commit:40` reads commit 40 of `main` when `dev` started at
42. Snapshots are per branch, so `snapshot:NAME` names a snapshot of the chosen branch.

### 2.4 Branches over HTTP

| Method | Path | Result |
|---|---|---|
| GET | `/$/branches/{ds}` | `200 { dataset, datasetId, branches: Branch[] }`, sorted by name with `main` first. |
| POST | `/$/branches/{ds}` | Creates a branch from JSON `{ name, from?: branch (default "main"), at?: selector (default "head"), protected?: bool, note?: string }`. Returns `201` with `Location: /$/branches/{ds}/{name}` and a `Branch`. |
| GET | `/$/branches/{ds}/{name}` | `200 Branch`, or `404`. |
| PATCH | `/$/branches/{ds}/{name}` | Changes `protected` or `note`. Returns `200 Branch`. |
| DELETE | `/$/branches/{ds}/{name}` | Deletes a branch. `?force=true` deletes one with unmerged commits. Returns `204`. |
| GET | `/$/merge/{ds}?source=&target=` | Previews a merge without writing. Returns `200 MergePreview`. |
| POST | `/$/merge/{ds}` | Merges. Takes a `MergeRequest` body. Returns `200 MergeResult`, or `409` with a `ConflictReport`. |

```ts
type Branch = {
  name: string; id: string; ordinal: number;
  head: number; modified: string;           // head seq and its timestamp
  from: { branch: string | null; branchId: string; seq: number };
  upstream: string | null;                   // the branch it was created from; null for main
  mergeBase: { branch: string; seq: number } | null;   // with its upstream
  ahead: number; behind: number;             // commits relative to its upstream
  protected: boolean; note: string | null; created: string;
  storage: { linked: boolean; ownBytes: number; heldBytes: number };
};
type MergeRequest = {
  source: string; target?: string;           // target defaults to "main"
  ff?: "auto" | "only";                      // default "auto"
  conflicts?: "cell" | "subject" | "quad";   // default "cell"
  onConflict?: "fail" | "ours" | "theirs" | "union";   // default "fail"
  resolutions?: Resolution[];
  expect?: { source?: number; target?: number };       // heads the caller saw
  base?: { branch: string; seq: number };    // only to choose among several bases
  inferences?: "exclude" | "include";        // default "exclude"
  message?: string; dryRun?: boolean;
};
type Resolution = {
  graph: string | null;                      // N-Triples IRI or blank node; null = default graph
  subject?: string; predicate?: string;      // narrow the resolution to a subject or a cell
  take: "ours" | "theirs" | "base" | "union" | "objects";
  objects?: string[];                        // with "objects": the cell's new objects
};
type MergeResult = {
  merged: boolean; upToDate: boolean; fastForward: boolean;
  source: { branch: string; seq: number }; target: { branch: string; seq: number };
  base: { branch: string; seq: number } | null;
  changes: { inserted: number; deleted: number };
  conflicts: { found: number; resolved: number };
  commit: Commit | null;                     // the merge commit, CI §2.4 shape
  inferences: { excluded: number; stale: boolean } | null;
  validation?: object;                       // C10 summary, when a guard ran
};
```

`MergePreview` has the fields of `MergeResult` with `merged: false` and `commit: null`,
plus the `ConflictReport` members when there are conflicts. A preview never writes and
never fails because of conflicts. `POST /$/merge/{ds}` with `dryRun: true` goes further.
It runs the target's write-time validation and the quota check, and it returns the
[C15](C15-write-previews.md) preview document with the merge fields added.

The terms in `Resolution` and in the conflict report use the encoding of the diff JSON
of [F06](F06-snapshots-and-point-in-time.md#phase-2): N-Triples syntax for each term, and
`null` for the default graph.

The diff endpoint compares branches with two new parameters. `GET /{ds}/diff` takes
`fromBranch` and `toBranch`, and each defaults to the request's branch. `from` and `to`
keep their meaning as selectors within those branches, so
`/prod/diff?fromBranch=main&toBranch=dev` gives the net changes from `main`'s head to
`dev`'s head.

`GET /$/commits/{ds}?branch=dev` lists the branch's history as §2.2 describes it.
`GET /$/snapshots/{ds}`, `/$/history/{ds}`, `/$/compaction/{ds}`, `/$/stats/{ds}`,
`/$/schema/{ds}`, `/$/reason/{ds}` and `/$/compact/{ds}` take `branch`. Dataset entries
in `GET /$/datasets` gain `branches`, the number of branches including `main`.

### 2.5 Merge outcomes and errors

| Condition | Status | `code` |
|---|---|---|
| The source's changes are already in the target. Nothing is written. | 200 | none, `upToDate: true` |
| The merge committed, as a fast-forward or a three-way merge. | 200 | none |
| Conflicts remain after `onConflict` and `resolutions`. Nothing is written. | 409 | `merge-conflict` |
| `ff: "only"` and the target has moved since the merge base. | 409 | `not-fast-forward` |
| `expect` names a head that is no longer the head. | 409 | `head-moved` |
| The branches have more than one best common ancestor, `base` is missing, and the ancestors conflict with each other, so no virtual base can be built (§4.3). | 409 | `ambiguous-merge-base` |
| The merge base is no longer reconstructable. | 410 | `merge-base-gone` |
| The target's write-time validation refuses the result. | 422 | as in [C10](C10-write-time-validation.md) |
| The change sets or the result exceed a budget or the quota. | 507 | `budget`, as in [C01](C01-observability-and-budgets.md) |
| A write to a protected branch outside a merge. | 403 | `branch-protected` |
| An unknown branch. | 404 | `no-such-branch` |
| A bad branch name or a malformed request. | 400 | `invalid-branch` or `invalid-merge` |
| Creating a branch whose name exists. | 409 | `branch-exists` |
| Creating a branch past `--max-branches`. | 409 | `branch-limit` |
| Deleting a branch with unmerged commits, without `force`. | 409 | `unmerged` |
| Deleting a branch that other branches were created from. | 409 | `has-children` |
| Branches of an in-memory dataset in Phase 1. | 501 | `branches-unsupported` |

The conflict report looks like this:

```json
{ "error": "2 conflicts merging dev (commit 57) into main (commit 61)",
  "code": "merge-conflict",
  "source": { "branch": "dev", "seq": 57 }, "target": { "branch": "main", "seq": 61 },
  "base": { "branch": "main", "seq": 42 },
  "scope": "cell", "conflicts": 2, "truncated": false,
  "graphs": [ { "graph": null, "conflicts": 2 } ],
  "cells": [
    { "graph": null, "subject": "<http://ex.org/a>", "predicate": "<http://ex.org/age>",
      "base": ["\"30\"^^<http://www.w3.org/2001/XMLSchema#integer>"],
      "ours": ["\"31\"^^<http://www.w3.org/2001/XMLSchema#integer>"],
      "theirs": ["\"32\"^^<http://www.w3.org/2001/XMLSchema#integer>"] } ] }
```

`ours` is the target and `theirs` is the source, as in Git. `limit` (default 100, at most
10,000) caps the listed cells, and `truncated` says whether more exist. Each side lists
at most 100 objects per cell. `graphs` always counts every conflict, grouped by graph.

### 2.6 Merge options

- **`ff`.** With `auto`, a merge whose target has not moved since the merge base is a
  fast-forward. With `only`, any other merge fails with `409 not-fast-forward`.
- **`conflicts`.** The scope decides what counts as a conflict (§4.4). `cell` is the
  default, `subject` is stricter and `quad` never reports a conflict.
- **`onConflict`.** The rule for every conflict that no resolution covers. `fail` is the
  default and refuses the merge. `ours`, `theirs` and `union` resolve every remaining
  conflict that way. This mirrors lakeFS's `source-wins` and `dest-wins` strategies, with
  `union` added for sets.
- **`resolutions`.** Per-graph, per-subject or per-cell choices. The most specific
  resolution that covers a cell wins. A resolution that covers no conflict is an error,
  because it usually means the caller resolved a stale report.
- **`expect`.** The heads the caller saw when it read the conflict report. When either
  head has moved, the merge fails with `409 head-moved` rather than applying resolutions
  to changes the caller has not seen. This is the optimistic check that Nessie makes
  with an expected hash.
- **`inferences`.** With `exclude`, the default, changes to the inferred graph
  `urn:x-sparkles:inferred` are left out of the merge (§4.9). With `include`, the
  inferred graph merges like any other graph.
- **`message`.** The merge commit's message. The default is
  `merge dev (commit 57) into main`.

### 2.7 CLI

```
sparkles branch list    --loc DB [--format text|json]
sparkles branch create  --loc DB NAME [--from BRANCH] [--at REF] [--protected] [--note TEXT]
sparkles branch show    --loc DB NAME [--format text|json]
sparkles branch delete  --loc DB NAME [--force]
sparkles branch protect --loc DB NAME [--off]
sparkles merge --loc DB SOURCE [--into TARGET] [--ff-only] [--conflicts cell|subject|quad]
               [--on-conflict fail|ours|theirs|union] [--resolve FILE.json]
               [--expect-source N] [--expect-target N] [--message TEXT] [--dry-run]
               [--format text|json]
```

Each command also works against a server with `--server URL --dataset NAME`, as
`sparkles compaction` does. `--branch NAME` is a global option of `query`, `update`,
`load`, `dump`, `log`, `diff`, `snapshot`, `compact`, `stats`, `check` and `clone`.
`branch list` and `log --branch` read the files without the database lock, like `log`.

`branch list` prints one line per branch:

```
branch   head  from        ahead  behind  storage           note
main       61  -               -       -  gen-0004
dev        57  main@42        15      19  linked (2.1 MiB)  schema migration
```

`merge` prints the result or the conflicts:

```
merge dev (commit 57) into main (commit 61), base main@42
CONFLICT  <http://ex.org/a> <http://ex.org/age>  (default graph)
  base    30
  ours    31   (main)
  theirs  32   (dev)
1 conflict, nothing merged. Resolve with --on-conflict or --resolve FILE.
```

The exit status is 0 after a merge or when the target is up to date, 2 when conflicts
stopped the merge, and 1 on any other error. `--resolve` reads a JSON array of
`Resolution` objects.

### 2.8 Rust library API

```rust
// crates/sparkles-core/src/branch.rs (new)
pub struct BranchInfo { pub name: String, pub id: uuid::Uuid, pub ordinal: u16,
                        pub head: CommitInfo, pub from: CommitRef, pub upstream: Option<String>,
                        pub merge_base: Option<CommitRef>, pub ahead: u64, pub behind: u64,
                        pub protected: bool, pub note: Option<String>, pub created_ms: i64,
                        pub storage: BranchStorage }
pub struct CommitRef { pub branch_id: uuid::Uuid, pub seq: u64 }
pub struct BranchOptions { pub from: String, pub at: At, pub protected: bool,
                           pub note: Option<String> }
pub enum ConflictScope { Cell, Subject, Quad }
pub enum Take { Ours, Theirs, Base, Union, Objects(Vec<Term>) }
pub struct Resolution { pub graph: GraphName, pub subject: Option<Term>,
                        pub predicate: Option<NamedNode>, pub take: Take }
pub struct MergeOptions { pub ff_only: bool, pub scope: ConflictScope,
                          pub on_conflict: Option<Take>, pub resolutions: Vec<Resolution>,
                          pub expect_source: Option<u64>, pub expect_target: Option<u64>,
                          pub base: Option<CommitRef>, pub include_inferences: bool,
                          pub write: crate::guard::WriteOptions,       // message, preconditions
                          pub cancel: Option<Arc<AtomicBool>>, pub deadline: Option<Instant> }
pub enum MergeOutcome { UpToDate, Merged(MergeReport), Conflicts(ConflictReport) }

impl Store {
    pub fn branch_name(&self) -> &str;                 // "main" on the dataset's own store
    pub fn branch_id(&self) -> uuid::Uuid;
    pub fn branches(&self) -> Result<Vec<BranchInfo>>;
    pub fn create_branch(&self, name: &str, o: &BranchOptions) -> Result<BranchInfo>;
    pub fn branch(&self, name: &str) -> Result<Arc<Store>>;      // opens lazily
    pub fn delete_branch(&self, name: &str, force: bool) -> Result<()>;
    pub fn set_branch_protected(&self, name: &str, on: bool) -> Result<BranchInfo>;
    pub fn merge_base(&self, a: &str, b: &str) -> Result<Vec<CommitRef>>;
    pub fn merge(&self, source: &str, target: &str, o: &MergeOptions)
        -> Result<MergeOutcome>;
    pub fn preview_merge(&self, source: &str, target: &str, o: &MergeOptions)
        -> Result<crate::preview::Preview>;
}
impl Dataset { pub fn branch(&self, name: &str) -> Result<Dataset>; /* forwards the rest */ }
// error.rs
Error::NoSuchBranch(String)          // 404
Error::BranchConflict(String, code)  // 409 branch-exists, branch-limit, unmerged, has-children
Error::MergeConflict(ConflictReport) // 409 merge-conflict
Error::BranchProtected(String)       // 403
// commit.rs
CommitKind::Merge                    // code 13, JSON "merge"
```

The branch methods are reachable from any branch's store. They act on the dataset's
branch set, which every branch store of a dataset shares.

### 2.9 UI

- **Branch selector.** The dataset page header gets a branch menu next to the dataset
  name. It lists the branches with their heads and keeps the choice in the URL
  (`?branch=dev`), so a link opens the same branch. The query page gets a Branch field
  next to the At field, and its result label reads "dev · at commit 57".
- **Branches panel.** The dataset page lists branches with their head, the commit they
  started from, commits ahead and behind their upstream, the disk they hold, a protected
  badge and a note. It can create a branch from any branch at the head, a commit or a
  snapshot, protect or unprotect one, and delete one, with a warning when the branch has
  unmerged commits.
- **History.** The History panel marks merge commits and links to the merged commit, and
  shows inherited commits with their branch's name.
- **Merge page (Phase 2).** `/datasets/[name]/merge?source=dev&target=main` shows the
  merge base, the counts, and the changes as signed N-Quads lines, reusing the diff view.
  Conflicts appear in a table grouped by graph and subject, with the base, ours and
  theirs objects side by side and a choice per row or per group. The page sends the
  heads it showed as `expect`, and it shows the validation report when the target's
  guard refuses the result. In Phase 1 the panel's Merge button merges only when there
  are no conflicts, and it otherwise shows the report and the equivalent CLI command.
- **Commit graph (Phase 2).** A Graph toggle in the History panel draws the commits of
  every branch the caller may read in time order, one lane per branch, as
  `git log --graph` does. Fork edges start at each branch's starting commit, and merge
  edges at each merge commit's merged commit. Long runs of commits on one branch fold
  into a segment that expands. The graph pages, and a commit shows its changes and a
  link to query at it. `GET /$/commit-graph/{ds}` serves it: the commits of several
  branches with their parents, newest first, with each branch's head, upstream and
  starting commit.

### 2.10 Python (Phase 2)

```python
ds.branches() -> list[Branch]
ds.create_branch("dev", source="main", at=None, protected=False, note=None) -> Branch
dev = ds.branch("dev")                 # a Dataset bound to the branch
dev.update("INSERT DATA { … }")
ds.merge("dev", into="main", conflicts="cell", on_conflict="fail",
         resolutions=None, expect=None, message=None, dry_run=False) -> MergeResult
ds.merge_base("dev", "main") -> list[CommitRef]
ds.delete_branch("dev", force=False)
```

A merge that stops on conflicts raises `sparkles.MergeConflict`. Its `conflicts`
attribute holds the cells as term objects, and `report` holds the JSON document of §2.5.

### 2.11 Clones

- **At a past commit.** `POST /$/datasets/{ds}/clone?at=REF` and `sparkles clone --at`
  shipped with [F06 Phase 2](F06-snapshots-and-point-in-time.md#phase-2). This spec adds
  `branch=`, so a clone can copy any branch at any readable commit. `forkedFrom` then
  holds the branch id and seq of the copied commit. A clone copies one branch and no
  branches of the source.
- **Across servers, Phase 1.** A backup repository that both servers can reach already
  moves a dataset between servers. `sparkles backup create` on the source and a restore
  into a new name with `identity: "new"` on the destination give the same result as a
  clone, with `forkedFrom` set ([F05 §2.3](F05-snapshot-repositories.md)). The API docs
  will describe this as the way to clone across servers.
- **Across servers, Phase 3.** `POST /$/datasets/{ds}/clone` gains a remote source,
  `{ "name": "NEW", "remote": { "url": "https://a.example/prod", "branch": "dev",
  "at": "commit:57", "credential": "a-example" } }`. The destination streams the state
  from a new endpoint on the source, `GET /$/export/{ds}?branch=&at=`. Its body is a JSON
  header with the dataset id, branch id, seq, blank-node counter, prefixes and the
  configuration files, then the quads as RDF Thrift with the stored `_:b` labels, then a
  trailer with the quad count and a SHA-256 of the stream. The destination builds its
  generation with the labels kept, as a local clone does. The stream is independent of
  the index format, so it also works between versions with different formats. The
  request goes through the outbound policy and a named credential, as S3 repositories
  do. The export needs `admin` on the source dataset.

## 3. Prior art and standards basis

- **Git.** A branch is a name for a line of commits. The merge base of two commits is
  their best common ancestor, which Git defines as a common ancestor that no other common
  ancestor descends from. Criss-cross histories can have several. A fast-forward applies
  when one side is an ancestor of the other, and otherwise Git makes a merge commit with
  two parents from a three-way merge. Conflicts are reported and left for the user to
  resolve, and strategies such as `-X ours` resolve them by rule. Sparkles takes the
  model of merge bases, three-way merges, two-parent merge commits and protected
  branches. It does not take Git's content-addressed object store (§8).
- **Dolt.** Dolt computes merges per table row and reports a conflict when both sides
  changed the same cell of the same row in different ways. Conflicts are kept in
  `dolt_conflicts` system tables until they are resolved with "ours" or "theirs" or by
  hand. Constraint violations found by the merge are reported in the same way, and Dolt
  will not commit while conflicts remain. Sparkles adapts the cell idea to RDF (§4.4)
  and treats a constraint violation as a refusal by the write guard (§4.10). It does not
  keep conflicts on the server (§8).
- **lakeFS.** Branches are cheap pointers over immutable objects, so creating one copies
  nothing. lakeFS finds the merge base and merges three ways by object path, and its
  `source-wins` and `dest-wins` strategies resolve every conflict one way. Its
  documentation notes that the strategy applies to all conflicts at once. Sparkles
  shares immutable index files the same way (§4.2) and offers the same global rules next
  to per-cell resolutions.
- **Project Nessie.** Commits carry the hash of the head that the client expected, and
  a commit whose expectation fails is rejected with a conflict. Sparkles' `expect`
  option is the same check (§2.6).
- **TerminusDB.** Each commit is an immutable layer of added and removed triples over
  the layers before it, and a branch is a new layer stack over a shared prefix. Its
  merges replay the source's commits on top of the target, which is a rebase. Sparkles
  shares the idea of a delta over a shared, immutable base. It rejects rebase as the
  merge model because rebase gives commits new identities (§8).
- **Datomic.** `with` applies transactions to a database value and returns a new value
  without making them durable, which gives speculative databases. That is the model of
  Sparkles' write previews ([C15](C15-write-previews.md)). A branch is the durable,
  shareable counterpart.
- **Three-way merge of sets.** For one element, the states in the base, ours and theirs
  are three booleans. When ours and theirs differ, exactly one of them equals the base,
  so the side that changed wins. A three-way merge of sets therefore never conflicts at
  the element level. This is the reasoning behind diff3's treatment of unchanged regions
  and behind add and remove sets with a known base in the literature on replicated
  data. Conflicts in RDF must therefore be defined above single quads (§4.4).
- **Version vectors.** Sparkles keeps, for each commit, the newest commit of each branch
  that it descends from. This is the version vector of the distributed systems
  literature, and comparing two vectors gives the common ancestors (§4.3).
- **RDF 1.2 Concepts.** Blank nodes are local to a dataset. Sparkles keeps one blank-node
  space for all branches of a dataset, so a blank node shared by two branches is the same
  node in both (§4.8).
- **RDF Dataset Canonicalization (RDFC-1.0)** is a test oracle. The acceptance examples
  compare canonical N-Quads of merge results with expected datasets.
- **RDF Patch.** Merge commits appear in the change feed and in diffs as ordinary
  patches with `H prev` naming the first parent ([F06](F06-snapshots-and-point-in-time.md#replay-speedups-rdf-patch-and-the-change-feed)).
- **SPARQL 1.1 Protocol** allows extension parameters and "other 4XX or 5XX" codes, which
  cover `branch` and the new status codes, as they covered `at`.

## 4. Semantics

### 4.1 The commit graph

Each branch L has a starting point `from(L)`, which is a commit of another branch, its
**upstream**. `main` has none. The branches and their starting points form a tree.

A commit is a pair `(L, s)`. The own commits of L have seqs in `(from(L).seq, head(L)]`.
A seq at or below `from(L).seq` on L means the commit of the same seq in L's upstream
chain, so `(dev, 40)` and `(main, 40)` are the same commit when `dev` started at 42.

The **first parent** of `(L, s)` is `(L, s − 1)`, with the rule above for the first own
commit. A merge commit has a second parent, the merged source commit. The commits and
both kinds of parent edge form a directed acyclic graph.

The **first-parent chain** of a commit follows first parents only. It runs through L's
own commits, then through L's upstream from `from(L)`, and so on to `main`. Every log
that Sparkles keeps follows a first-parent chain, which is why the change sets of §4.5
walk along these chains.

### 4.2 Branch storage

A new branch writes no index. Its first generation, `gen-0000`, is a **linked
generation**. A linked generation names the generation of the upstream that owns the
starting commit, the byte offset in that generation's write-ahead log where the starting
commit ends, and the length of that generation's delta vocabulary at creation. It has its
own empty `delta.vocab` and `wal.log`.

Opening a linked generation does four things:

1. It shares the upstream generation's base files: the vocabulary, the seven
   permutations, the statistics and the lazily built caches. In one process both
   branches use the same mapped files and the same block cache entries.
2. It layers the delta vocabulary. An id below the recorded length resolves in the
   upstream's `delta.vocab`, and an id at or above it resolves in the branch's own file.
   A lookup by key in the upstream layer ignores ids at or above the recorded length,
   because those are terms the upstream added after the branch started.
3. It replays the upstream's log from the generation's base to the recorded offset,
   which rebuilds the delta of the starting commit.
4. It replays its own log, as a normal generation does.

When the upstream is itself linked, the linked generation lists a chain of log segments,
oldest first. A chain is at most `--max-branch-depth` segments long, 4 by default. A
branch created from a commit whose chain would be longer is built as its own generation
at creation, through the clone path of [C06](C06-clone-to-sandbox.md), and its creation
then costs a full build.

The branch shares storage until its first rebuild. A compaction of the branch
([C13](C13-automatic-compaction.md)) or a bulk commit on it builds `gen-0001` in the
branch's own directory from its state, as compaction does for `main`. From then on the
branch owns a full index, and it releases its holds on the upstream's generations.

The tradeoff is explicit. A branch costs almost nothing while its changes are small and
it stays linked. Its memory then includes the upstream's delta at the starting commit,
because each branch keeps its own delta, and a restart replays the inherited log segment.
Once it compacts, the branch costs a full index on disk, about what a clone costs. A
branch that inherits a large delta reaches the compaction thresholds sooner, so it is
cheapest to create long-lived branches soon after `main` compacts.

A branch's compaction policy counts its own changes, those since its starting commit,
for the delta-size triggers, and the whole delta for the memory and log-size triggers.
Without this rule a branch created from an upstream with a large delta would compact at
once and lose the sharing.

### 4.3 Merge bases

For each commit, Sparkles can compute its **frontier**. The frontier maps each branch to
the newest own commit of that branch that the commit descends from. The frontier of
`(L, s)` is the pointwise maximum of `{L: s}`, the frontier of `from(L)`, and the
frontiers of the second parents of L's merge commits up to s. Merge records are few, so
the frontiers come from the in-memory table of merges in milliseconds.

The common ancestors of two commits are the commits at or below the pointwise minimum of
their frontiers. A **merge base** is a common ancestor that no other common ancestor
descends from. Usually there is exactly one. After criss-cross merges there can be
several, as in Git. Phase 1 refused such a merge with `409 ambiguous-merge-base`. Since
Phase 3 the candidates are merged into a virtual base, as Git's recursive strategies do,
without writing a commit, and the report's `base` is `null`. The synthesis uses the
merge's conflict scope and exempt predicates, but not its resolutions or `onConflict`.
Only when the candidates conflict with each other is the merge refused with
`409 ambiguous-merge-base`, listing the candidates, and the caller picks one with `base`.

**Ahead** is the number of own commits of the source that the target does not descend
from, counted over every branch in the source's frontier. **Behind** is the same count the
other way.

### 4.4 Conflicts

Write B for the merge base, O for the target head (ours) and T for the source head
(theirs). For each quad q, `b(q)`, `o(q)` and `t(q)` say whether q is present in each
state. The quad-level result follows the three-way rule:

| b | o | t | result |
|---|---|---|---|
| any | x | x | x |
| x | x | y | y (only theirs changed) |
| x | y | x | y (only ours changed) |

This rule never conflicts. Conflicts are defined on groups of quads that callers think of
as one value. The **conflict scope** chooses the group.

- **`cell`**, the default. A cell is a graph, a subject and a predicate. It holds the
  objects of that subject and predicate in that graph, which is the RDF counterpart of a
  cell in Dolt. A cell conflicts when both sides changed it relative to the base and the
  two changed cells differ.
- **`subject`**. A group is a graph and a subject. It conflicts under the same rule. This
  catches a subject deleted on one side and edited on the other.
- **`quad`**. No group conflicts, and the merge takes the quad-level result everywhere.
  This suits data that only grows, such as observations or logs.

Two sides that made the same change, such as both correcting the same typo, do not
conflict. A conflict needs both sides to have changed the group, so a cell that only one
side touched is never a conflict, whatever the other side did elsewhere.

The `cell` scope is conservative for properties with many values. When both sides add a
different `rdfs:label` to the same subject, that cell conflicts, and `union` resolves it
by keeping both. Open question 1 asks whether some predicates should be exempt by
default.

### 4.5 Change sets and the merge result

Sparkles never materializes B, O and T to merge them. It works with **toggle sets**. The
toggle set `T(X→Y)` holds every quad whose presence differs between states X and Y. F06's
diffs already compute these from the logs. Each logged change took effect, so each change
toggles its quad, and a quad changed back cancels out. The first change of a quad in a
walk gives its presence at X.

Toggle sets compose. For any three states, `T(X→Z) = T(X→Y) △ T(Y→Z)`, where △ is the
symmetric difference. The merge needs `T(B→O)` and `T(B→T)`. When B is on the
first-parent chain of O, `T(B→O)` is a log walk along that chain, crossing compactions
and branch starting points as F06's planner crosses generations. Otherwise Sparkles finds
the newest commit Z that lies on the first-parent chains of both B and O, walks from Z to
each, and composes `T(B→O) = T(Z→B) △ T(Z→O)`. The same holds for T. A stretch that no
retained log covers, because of a bulk commit or a collected generation, is compared
state against state, as F06 diffs do.

The changes to apply to the target are then simple set operations:

- With no conflicts, apply `T(B→T) − T(B→O)` to O. A quad toggled by theirs only is
  changed, and a quad toggled by both is already in its final state.
- For a conflicting group, the resolution decides:

| Take | Toggles applied to the group in O |
|---|---|
| `ours` | none |
| `theirs` | `T(B→T) △ T(B→O)`, restricted to the group |
| `base` | `T(B→O)`, restricted to the group |
| `union` | `T(B→T) − T(B→O)`, restricted to the group, which is the quad-level result |
| `objects` | the difference between the group in O and the given objects. The group in O is read with one scan of the target. |

The result is applied to the target as one write transaction of kind `merge`. Quads are
interned through their vocabulary keys, because ids are local to each generation. A merge
with more changes than the store's bulk threshold commits through the bulk path, like a
large load.

### 4.6 Fast-forward and up to date

When T's changes are all in O, which means B is T itself, the merge is up to date. It
writes nothing and answers `upToDate: true`.

When B is O, the target has not moved since the branches parted. The merge is a
fast-forward. It has no conflicts, and the result equals T exactly. Sparkles has no
moving refs, because each branch is a store with its own numbering, so a fast-forward
still writes one merge commit on the target, whose state equals the source head. Phase 2
adds `ff: "replay"`, which instead replays each source commit as its own commit on the
target, with its kind and message and with `replayedFrom` recording the original.

A merge commit is made even when its net change is empty. That happens when ours already
holds all of theirs' changes in a different history. This is an exception to
[CI §2.2](CI-commit-identity.md), which makes no commit for a net-zero write. The commit
records the second parent, so the next merge finds a newer base. The commit has
`inserted: 0` and `deleted: 0`.

### 4.7 Merge procedure

1. Resolve the source and target heads from their live snapshots, and check `expect`.
2. Compute the merge base (§4.3). Return early for up to date.
3. Compute the two toggle sets (§4.5) without any lock. The change sets count against
   the rows budget (`--max-rows`), as diffs do, and the time against the request's
   timeout.
4. Group the toggles, find conflicts, and apply `resolutions`, then `onConflict`. Remove
   orphaned blank nodes from resolved groups (§4.8). Answer `409` when conflicts remain.
5. Take the target's writer lock. If the target head moved since step 1 and `expect`
   was not given, release the lock and repeat from step 2, at most three times. If it
   moved and `expect` was given, answer `409 head-moved`.
6. Commit the result through the normal write path, with the write guard (§4.10), the
   quota check, the merge record (§5.4) and the commit message.

The source is never locked. Writes to the source during the merge are not included,
because the merge uses the source head captured in step 1, and the merge commit records
that head as its second parent.

### 4.8 Blank nodes

All branches of a dataset share one blank-node space. A blank node that existed at a
branch's starting commit has the same id, and the same `_:b<hex>` label, on both
branches.

New blank nodes never collide across branches, because each branch allocates from its
own range. The 59-bit payload of a stored blank node splits into a 16-bit branch ordinal
and a 43-bit counter, which allows 65,535 branches over a dataset's life and about
8.8 × 10¹² blank nodes per branch. `main` keeps ordinal 0, so existing labels do not
change. A merge copies blank nodes with their ids, so a label that a client saw on the
source names the same node on the target after the merge. A clone carries over the
blank-node counter and the next ordinal, so a clone of a branch never reuses an ordinal
that its data contains.

Two sides that each added an equivalent blank-node structure, such as the same address
written as `[ ex:city "Oslo" ]`, end up with two nodes after the merge. RDF says they are
two nodes, and recognizing equivalent structures would need a canonicalization of the
changes. That is out of scope (§8).

When a resolution discards one side's change to a group, blank nodes that only that side
created since the base can be left without any reference. Sparkles removes them with the
discarded change. Concretely, when a discarded toggle inserted a quad whose object is a
blank node allocated by that side after the base, the toggles of that side whose subject
is that blank node are discarded too, recursively. The counter value at the base is read
from the base commit's log record, which already stores the blank-node counter. This
keeps RDF lists and other blank-node structures whole when one side's version of a cell
wins. Blank nodes inside triple terms are not followed in Phase 1.

### 4.9 Inferences

The inferred graph is derived from the other graphs, so merging it as data mixes two
derivations. With `inferences: "exclude"`, the default, the merge leaves the inferred
graph's toggles out on both sides, and the target keeps its own inferred graph. The
merge commit changes the default graph, so [C08](C08-inference-freshness.md) reports the
target's inferences as stale, and an automatic re-run, when the target has one enabled,
updates them incrementally. `MergeResult.inferences` reports how many inferred quads the
merge left out.

With `inferences: "include"`, the inferred graph merges like other graphs and can
conflict. This suits a dataset whose inferred graph is curated by hand.

Each branch has its own reasoning status. A new branch copies its upstream's
`reasoning.json`. The recorded position is a commit of the branch's history, so a
branch created at a fresh commit is fresh, without the rebasing that clones need.

### 4.10 Validation

A merge commit passes the target branch's write guard ([C10](C10-write-time-validation.md))
like any other commit. The guard validates the state after the merge, incrementally when
C10 can, and a refusal answers `422` with the validation report and commits nothing. This
is how SHACL or ShEx constraints protect a branch from a merge, as Dolt reports
constraint violations found by a merge. A dry run reports the same result without
committing.

Each branch has its own guard. A new branch copies its upstream's `validation.json` and
the shapes, so a branch can try new shapes before they reach `main`.

### 4.11 Protected branches

A protected branch refuses SPARQL updates, Graph Store writes, uploads, loads and
reasoning runs with `403 branch-protected`. It accepts merges. Protecting `main` gives a
workflow where every change reaches it through a branch, a review and a merge. Only an
admin of the branch can protect, unprotect or delete a protected branch.

### 4.12 Deleting a branch

Deleting removes a branch's name, its commits, its snapshots and its storage. It is
refused with `409 unmerged` when the branch has own commits that its upstream does not
descend from, unless `force` is given. It is refused with `409 has-children` while other
branches were created from it, because their linked generations and their history read
its files. Phase 2 can re-parent the children instead.

A deleted branch's commits stay in the history of other branches that merged them.
Their `mergedFrom` keeps the branch id and seq, with `branch: null`. Reading such a
commit is no longer possible unless another branch's history holds the same state.

Deleting a dataset deletes all of its branches.

### 4.13 History, retention and compaction

Each branch has its own `history.json` with its own snapshots, retention window and
catalog horizon. A new branch starts with no snapshots and copies its upstream's
retention settings. The F06 machinery runs per branch, with two new holds:

- **`branch:NAME` on a generation.** A linked generation holds every upstream
  generation it reads, until the branch's first rebuild. `GET /$/history/{ds}` lists the
  hold in `heldBy`, like a backup lease. A hold names a generation, not a commit, for the
  reason F05 gave: a commit-based hold would let the upstream's compaction collect the
  generation that the link reads.
- **`branch-base:NAME` on a commit.** Each branch pins its merge base with its upstream,
  so a merge back is always possible. The pin moves when a merge moves the merge base,
  and it goes away with the branch. It counts against the retained generations like a
  named snapshot, so it can keep one generation of the upstream. Open question 3 asks
  whether this should be optional.

The automatic compaction scheduler treats each open branch as it treats a dataset. A
branch compaction takes a task slot and counts toward the server-wide cap. The quota rule
for compactions changes for a linked branch (§4.14).

### 4.14 Quotas, limits and disk

A dataset's storage quota covers its whole directory, which includes every branch. A
write on any branch is refused when the dataset as a whole would exceed the quota.

[C13](C13-automatic-compaction.md) never refuses a compaction because of the quota, since
a compaction usually makes a dataset smaller. The first rebuild of a linked branch is
different, because it adds a full index. It is refused with `507 dataset-bytes` when it
would take the dataset over its quota or below `--min-free-disk-mb`. The branch then
stays linked, and the compaction status gives `quota` as the reason it waits. Later
compactions of the branch follow C13's rule.

| Limit | Default | Flag | Enforcement |
|---|---|---|---|
| Branches per dataset, including `main` | 64 | `--max-branches` | `409 branch-limit` on create |
| Segments in a linked generation | 4 | `--max-branch-depth` | A deeper branch is built at creation. |
| Listed conflict cells | 100, at most 10,000 | `limit` | `truncated: true` |
| Change set of a merge | `--max-rows` | | `507` with `budget: "rows"` |

### 4.15 Crash safety

| Step | What is written | Commit point |
|---|---|---|
| Create | Under the upstream's writer lock, Sparkles captures the starting commit, the log offset, the delta-vocabulary length and the blank-node counter, and registers the holds in memory. It then builds `branches/.create-<id>/` with `branch.json`, `gen-0000/link.json`, an empty log and delta vocabulary, and copies of the configuration files. It syncs the files, renames the directory to `branches/<id>`, syncs `branches/`, and writes `branches.json` with `write_atomic`. | The write of `branches.json` |
| Merge | The merge record is appended to the target's `merges.bin` and synced before the commit, as annotations are. | The log sync, or the `CURRENT` switch of a bulk commit |
| Branch rebuild | As for `main`. After the `CURRENT` switch, `branches.json` drops the branch's generation holds. | The `CURRENT` switch |
| Delete | `branches.json` without the entry, then a rename to `branches/<id>.deleting`, then removal. | The write of `branches.json` |
| Open | Removes `branches/.create-*` and `*.deleting`, removes a `branches/<id>` that `branches.json` does not list after checking its `branch.json` names this dataset, and truncates merge records above the head. A listed branch whose directory is missing is reported as broken, and its holds are kept. | |

Invariants:

1. The upstream never collects a generation that a branch listed in `branches.json`
   reads. Holds are in memory before the branch exists on disk, and they are durable in
   `branches.json` before anything relies on them after a restart. Opening `main` alone,
   as the CLI does, reads `branches.json` and honours the holds.
2. A merge commit and its merge record are durable together or not at all.
3. A crash during a merge leaves the target at its old head or at the merge commit, and
   never between them.

## 5. Design

### 5.1 Files

```
<ds>/                         main, as today
  CURRENT  gen-NNNN/  commits.bin  annotations.bin  history.json  dataset.json  …
  merges.bin                  merge records of main (created by its first merge)
  branches.json               the branch table
  branches/
    <branch-id>/
      branch.json             name, id, ordinal, starting commit
      CURRENT                 "gen-0000" until the first rebuild
      gen-0000/link.json      the linked generation: segments, offsets, vocabulary length
      gen-0000/delta.vocab    terms the branch added while linked
      gen-0000/wal.log        the branch's log while linked
      gen-NNNN/               generations the branch built
      commits.bin  annotations.bin  merges.bin  history.json
      prefixes.json  reasoning.json  validation.json  text.json  geo.json  vector.json
      compaction.json
```

`branches.json` (format 1) holds the next ordinal and one entry per branch:

```json
{ "format": 1, "datasetId": "3f1c9a2e-…", "nextOrdinal": 3,
  "branches": [
    { "name": "dev", "id": "9d0c41e2-…", "ordinal": 1,
      "from": { "branchId": "3f1c9a2e-…", "seq": 42 },
      "created": "2026-10-02T10:00:00.000Z", "protected": false, "note": null,
      "holds": [ { "branchId": "3f1c9a2e-…", "generation": "gen-0003" } ],
      "baseHold": { "branchId": "3f1c9a2e-…", "seq": 42 } } ] }
```

`link.json` lists the segments of the linked generation, oldest first:

```json
{ "format": 1, "baseSeq": 42,
  "segments": [ { "branchId": "3f1c9a2e-…", "generation": "gen-0003",
                  "baseSeq": 31, "endSeq": 42, "walEnd": 18843, "dvocabLen": 112 } ] }
```

`merges.bin` holds fixed 48-byte records: the merge commit's seq, the second parent's
branch id and seq, the number of resolved conflicts, flags and a CRC-32. The catalog
record of [CI §5.3](CI-commit-identity.md) stays 64 bytes.

Configuration files are copied into the branch at creation and are independent from then
on. The storage quota, access control and stored queries ([C16](C16-stored-queries.md))
stay per dataset.

An older binary sees `branches/` as an unknown directory and leaves it alone. It ignores
`branches.json`, so its compaction of `main` can collect a generation that a branch
reads. The upgrade notes will say that a database with branches must not be opened by an
older version, and `branches.json` raises the database's minimum version in
`dataset.json`, which older versions that check it refuse to open.

### 5.2 Engine changes

- **`Generation`** splits into a shared `BaseIndex` (vocabulary, permutations,
  statistics, metadata, vectors, spatial base, count cache, characteristic sets), held
  in an `Arc`, and the per-branch parts: the delta vocabulary, the log index and the
  `uid`. Each branch gets its own `Generation` instance and therefore its own `uid`, so
  result-cache keys stay distinct, while block-cache keys follow the shared
  `PermIndex::uid`.
- **`DeltaVocab`** gains a layered form over an `Arc` of the upstream's delta vocabulary
  and a length, with the lookup rule of §4.2.
- **`Store`** gains a branch identity (name, id, ordinal), a reference to the dataset's
  `BranchSet`, and a merge table loaded from `merges.bin`. `Store::open` opens a linked
  `gen-0000` by replaying the listed segments with `replay_wal` and `Stop::AfterSeq`,
  then its own log. `next_bnode` starts in the branch's ordinal range.
- **`BranchSet`** (new, `branch.rs`) owns `branches.json`, opens branch stores lazily and
  keeps them open, computes frontiers and merge bases, plans merges and reports holds to
  each store's history state. It lives as long as any store of the dataset.
- **`history.rs`** gains `Hold::Branch(name)` and `Hold::BranchBase(name)`. The needed set
  of §4.2 of F06 adds the generations of `branches.json` holds and the commit of each base
  hold. Resolving a seq at or below a branch's starting commit delegates to the upstream
  store.
- **`store/diff.rs`** generalizes the planner from one store to a path of first-parent
  segments across branches, with toggle composition (§4.5). `Store::diff` across
  branches uses it.
- **`branch/merge.rs`** (new) groups toggles by scope, applies resolutions and the
  blank-node rule, and builds the write transaction.
- **`commit.rs`** gains `CommitKind::Merge`. `annotations.rs` is unchanged.
- **`sparkles check`** verifies `branches.json`, every `link.json` against the files it
  names, the merge records and the ordinal ranges of blank nodes.

### 5.3 Server changes

- **Routing.** A layer before the dataset routes rewrites `/{ds}@{branch}/…` to
  `/{ds}/…` with a branch extension, and `branch=` sets the same extension. Handlers
  look up the store with `dataset.branch(name)` in place of `dataset.store`.
- **Routes.** `/$/branches/{ds}[/{name}]` and `/$/merge/{ds}`, with the errors of §2.5.
- **Access control** (§6.1) in the C09 and C12 checks.
- **Scheduler.** The compaction and history ticks iterate the open stores of each
  dataset's `BranchSet`.
- **Metrics** of §6.3.
- **UI** changes of §2.9, and `api.ts` gains the branch calls and a branch parameter on
  existing calls.

## 6. Cross-cutting concerns

### 6.1 Access control

A [C09](C09-dataset-access-control.md) grant gains an optional `branches` list of names
and `*` patterns. Without it, the grant covers every branch, so existing grants keep
working and cover new branches. With it, the grant covers only the matching branches.
Graph restrictions of [C12](C12-graph-access-control.md) apply on every branch the grant
covers.

| Operation | Needs |
|---|---|
| Read a branch | `read` on the branch |
| Write a branch | `write` on the branch, and the branch is not protected |
| Create a branch | `read` on the source branch and `write` on the new name, from a grant without graph restrictions |
| Merge | `read` on the source and `write` on the target, from grants without graph restrictions |
| Delete a branch | `write` on the branch, or `admin` when it is protected |
| Protect, unprotect | `admin` on the branch |
| Compact, snapshots and history settings of a branch | `admin` on the branch, as on a dataset |

Merges and branch creation act on whole datasets, so they need grants without graph
restrictions, in line with C12's rule for admin operations. A partial merge for a
restricted principal would break the meaning of the merge base.

Scratch branches are the exception, added by [C17 §5.7](C17-agent-memory.md#57-branches-as-scratchpads).
A branch that the MCP tool `create_branch` makes is marked as a scratch branch in the
branch table, with the principal that created it. Through the MCP tools, a principal
whose grants cover only some graphs may create a scratch branch when it may write at
least one graph of the dataset on the new name. It may merge or delete only the scratch
branches it created, and its merge is refused with `forbidden` when the branch changes a
graph that the principal may not write on the target, whoever made the change. The
merge itself is not partial, so the merge base keeps its meaning. A grant without the
`merge` endpoint still allows no merge. The HTTP branch routes keep the rules of the
table above for every branch, scratch or not, and principals with unrestricted grants
keep their rights over every branch. `--mcp-scratch-branch-ttl` deletes idle scratch
branches, and branches made any other way are never expired.

A branch that a principal's grants do not cover answers as C09 answers for a dataset
the principal cannot see, so a principal cannot learn whether such a branch exists.
Listings show only covered branches. C12 gains two endpoint names, `branches` for the
branch routes other than listing and `merge` for merges and merge previews.

### 6.2 Backups

In Phase 1 a backup ([F05](F05-snapshot-repositories.md)) copies `main` only, as it does
today. Its manifest records the number of branches it left out, and the dataset's Backups
panel says that branches are not backed up. Branch holds keep generations on disk, and a
backup's own lease works as before.

Phase 2 adds `branch=` to backups. A branch backup is captured like a backup of an
in-memory dataset, by building a temporary generation from the branch's head when the
branch is linked, or by uploading its files when it owns its generation. A restore of a
branch backup creates a new dataset whose `forkedFrom` names the branch. Restoring
branches as branches, and backing up all branches of a dataset together, are open
question 7.

### 6.3 Metrics

| Metric | Type | Labels |
|---|---|---|
| `sparkles_branches` | gauge | `dataset` |
| `sparkles_branch_linked` | gauge, branches still linked | `dataset` |
| `sparkles_branch_held_bytes` | gauge, upstream generations held only by branches | `dataset` |
| `sparkles_merges_total` | counter | `dataset`, `result` = `merged`, `fast-forward`, `up-to-date`, `conflict`, `refused` |
| `sparkles_merge_conflicts_total` | counter, conflicting groups found | `dataset` |
| `sparkles_merge_seconds_sum`, `_count` | summary | `dataset` |
| `sparkles_merge_changes_total` | counter, quads changed by merges | `dataset`, `op` = `insert`, `delete` |

Per-branch gauges such as delta size and disk use carry a `branch` label, with the
cardinality cap that C01 applies to datasets. Branches past the cap are summed under
`$other`. Merges log one line at info level with the source, target, base, counts,
conflicts and time.

### 6.4 Performance targets

These are estimates for Phase 1 to confirm, on a 10.5M-quad dataset.

- Creating a branch takes under 50 ms and writes under 64 KiB.
- The first request to a linked branch after a restart costs one open of the shared
  generation, when `main` has not opened it, plus a replay of the inherited segment at
  the rates F06 measured.
- A merge base takes under 1 ms with 64 branches and 10,000 merges.
- A merge of a branch with 10,000 changed quads into a `main` with 10,000 own changes
  since the base takes under 1 s, of which the log walks take tens of milliseconds as
  F06's diffs do.
- A query on a linked branch with an empty own delta runs as fast as the same query on
  `main` at the starting commit.

## 7. Phasing

**Phase 1 (about 10 days).**

- Days 1–3: `BaseIndex` and the layered delta vocabulary, linked generations, `branch.rs`
  with `branches.json`, ordinals and blank-node ranges, creation, lazy opening and
  deletion, holds in the history state, branch compaction, and crash recovery.
- Days 4–6: frontiers and merge bases, the cross-branch diff planner, merges with all
  three scopes, `onConflict`, resolutions, the blank-node rule, `expect`, the guard, dry
  runs, the merge record and the zero-change merge commit.
- Days 7–8: the server routes, the path form, headers and tags, access control by
  branch, quotas and limits, metrics, `branch` on the other admin routes, and `clone`
  with `branch`.
- Days 9–10: the CLI, the UI's branch selector, Branches panel and conflict-free Merge
  button, `docs/API.md`, `docs/USAGE.md`, the README, and tests A1–A22.

**Phase 2.**

- The merge page of the UI with per-cell resolution.
- The commit graph of the History panel, and `GET /$/commit-graph/{ds}` (§2.9).
- Python (§2.10).
- Squash merges, which apply the changes as one commit without recording a second parent.
- Reverts, which are merges of a commit's parent with the commit as base:
  `POST /$/revert/{ds}?branch=&commit=`.
- `ff: "replay"`.
- Renaming branches, and re-parenting children when a branch is deleted.
- In-memory branches share immutable vocabulary and permutation indexes, copy the
  inherited delta vocabulary, and retain a private generation identity, mutable index
  state, history and validation guard. Creation cost follows the inherited delta.
- Branch backups (§6.2), and merges as cancellable tasks with `Prefer: respond-async`.
- Full-text and spatial indexes on a new branch built from the upstream's index files
  rather than from scratch. In Phase 1 a branch of a dataset with these indexes rebuilds
  them in the background on its first open, as a clone does, and `text:query` answers as
  F03 does while an index builds.
- Predicates exempt from cell conflicts, per merge and per dataset.

**Phase 3.**

- Cross-server clones through `/$/export` (§2.11).
- Relinking. When `main` compacts, a linked branch could move its link to the new
  generation by replaying the upstream's changes from its starting commit in reverse,
  which keeps it sharing storage for longer.
- Virtual merge bases for criss-cross histories.
- Cherry-picks, which are merges of one commit with its parent as base. They shipped
  with Phase 2, since they mirror reverts.
- Merges that keep conflicts on the server for long reviews, if the stateless model
  proves too limited in use.

## 8. Rejected alternatives

- **A branch as a clone.** Creating a clone costs a full build, which defeats the main
  use of branches as cheap, short-lived sandboxes. A branch ends up with a full index only
  if it lives long enough to compact.
- **Branches inside one store, with one log tagged by branch.** All branches would share
  one writer lock and one compaction, and commit numbering would need a branch component
  everywhere. Separate stores keep every existing invariant per branch.
- **A content-addressed index**, as Git, Dolt and TerminusDB use, where structural
  sharing makes branches and diffs cheap at every scale. Sparkles' sorted permutations
  and delta are the core of its query speed, and a content-addressed layout would replace
  them.
- **Rebase as the merge model.** Rebasing replays a branch's commits on a new base and
  gives them new identities. Receipts, entity tags, snapshots and change-feed positions
  that clients hold would stop naming those commits. Merging the upstream into a branch
  keeps every commit's identity and serves the same purpose.
- **Quad-level conflicts as the default.** They never fire, so a functional property
  changed on both sides ends with two values and no warning. The `quad` scope remains
  an option.
- **Conflicts from the ontology alone,** for example only on `owl:FunctionalProperty`
  predicates or properties with `sh:maxCount 1`. That makes merge behaviour depend on
  schema data that may itself be part of the merge. It can come later as an exemption
  rule.
- **Conflicts kept on the server until resolved,** as in Dolt's conflict tables. That
  needs merge-in-progress state per branch, with its own crash safety and access
  control. Stateless resolutions with `expect` give the same safety. Merging the upstream
  into the branch and editing there covers long reviews.
- **Relabelling blank nodes at merge time** instead of partitioning the counters. The
  labels a client saw on the source would change on the target, and a merge would need
  a relabelling table.
- **Merging blank-node structures that are isomorphic.** Deciding which added blank nodes
  are "the same" needs a canonicalization of each side's changes and a policy for partial
  matches. RDF treats them as distinct nodes.
- **Hard links or reflinks to share generation files.** A branch lives inside the
  dataset directory, so it can name the upstream's files directly, and naming them lets
  both branches share mapped files and block-cache entries in one process. Links would
  still leave the mutable files to handle.
- **Branches as datasets in the registry,** for example `prod--dev`. Quotas, access
  control and deletion would need to know that two datasets belong together, and dataset
  ids could not show the relationship.
- **Moving refs, so that a fast-forward rewrites the target's head to the source's
  commit.** The target's commits after that point would have the source's numbering and
  identity, which breaks the rule that a branch's seqs are its own and increase by one.
- **`?at=` syntax for branches,** such as `at=dev@commit:42`. Every endpoint that takes
  `at` would need to understand branches. A separate `branch` parameter keeps `at` within
  one history.

## 9. Acceptance examples

Setup: `sparkles serve --data /tmp/s`, a persistent dataset `ds`, and
`Store::set_clock` returning 1000, 2000, 3000 … ms. On `main`, commit 1 inserts
`<urn:a> <urn:age> 30`, commit 2 inserts `<urn:b> <urn:name> "B"`. Q is
`SELECT ?s ?p ?o { ?s ?p ?o } ORDER BY ?s ?p ?o`.

**A1. Create and isolate.**
- `POST /$/branches/ds {"name":"dev"}` → `201`, `from: {branch:"main", seq:2}`,
  `storage.linked: true`, and `ds/branches/<id>/gen-0000/link.json` exists. No new
  `gen-*` directory is built.
- `POST /ds/update?branch=dev` inserting `<urn:c> <urn:p> 1` → `Sparkles-Commit: 3`,
  `Sparkles-Branch: dev`.
- `POST /ds/update` inserting `<urn:d> <urn:p> 1` → `Sparkles-Commit: 3` on `main`.
- Q on `main` lists a, b and d. Q on `dev` lists a, b and c.
- `GET /$/commits/ds?branch=dev` lists 3 (`branch: "dev"`), 2, 1 and 0 (`branch:
  "main"`).

**A2. Path form.** `GET /ds@dev/sparql?query=Q` equals `?branch=dev`. `GET
/ds@dev/sparql?query=Q&branch=main` → `400`. `GET /ds@nope/sparql` → `404
no-such-branch`. `GET /ds@dev/data?default` returns the default graph of `dev`, with
`ETag: W/"<dev id>:3:turtle"`.

**A3. Fast-forward.** A fresh `dev` at `main` 2 inserts c (commit 3).
`GET /$/merge/ds?source=dev&target=main` → `fastForward: true`, `changes.inserted: 1`.
`POST /$/merge/ds {"source":"dev"}` → `200`, a commit 3 on `main` of kind `merge` with
`mergedFrom: {branch:"dev", seq:3}`. Q on `main` equals Q on `dev`. A second merge →
`upToDate: true` and no commit.

**A4. Clean three-way merge.** From A1's state, merge `dev` into `main` → `200`,
`fastForward: false`, `base: {branch:"main", seq:2}`. `main` holds a, b, c and d, and
`dev` is unchanged. `ff: "only"` on the same state → `409 not-fast-forward`.

**A5. Cell conflict.** `dev` at `main` 2. `main` changes `<urn:a> <urn:age>` from 30 to
31. `dev` changes it to 32. The merge → `409 merge-conflict` with one cell, `base: [30]`,
`ours: [31]`, `theirs: [32]`. Nothing is committed on `main`.
- With `onConflict: "theirs"` → `main` has 32.
- With a resolution `{graph:null, subject:"<urn:a>", predicate:"<urn:age>",
  take:"objects", objects:["33"]}` → `main` has 33.
- With `take: "union"` → `main` has 31 and 32.
- With `conflicts: "quad"` → `200` and `main` has 31 and 32.
- Both sides changing 30 to 31 → `200` with no conflict.

**A6. Subject scope.** `main` deletes every triple of `<urn:a>`. `dev` changes its age.
With `cell`, the age cell conflicts. With `subject`, the conflict is reported for
`<urn:a>` as a whole. A cell that only `main` deleted, with `cell` scope, is deleted in
the result.

**A7. Stale resolutions.** After A5's report, `main` makes another commit. A merge with
the report's heads in `expect` → `409 head-moved`. Without `expect`, the merge re-plans
and reports the new conflicts. A resolution for a cell that does not conflict → `400
invalid-merge`.

**A8. Blank nodes.** On `main`, commit 1 inserts `<urn:a> <urn:addr> _:x .
_:x <urn:city> "Oslo"`. Create `dev`. `SELECT ?o { <urn:a> <urn:addr> ?o }` gives the
same `_:b…` label on both branches.
- `dev` inserts `<urn:z> <urn:p> []`, whose label has `dev`'s ordinal in its high bits.
  After a merge into `main`, the same label appears on `main`, and a new blank node made
  on `main` has a different label.
- Both sides replace the address with a new blank node and city. The cell
  `<urn:a> <urn:addr>` conflicts. Resolving it as `ours` leaves no quad whose subject is
  `dev`'s new address node, and the canonical N-Quads of `main` equal the expected
  dataset.

**A9. Inferences.** `main` has RDFS reasoning materialized at its head. Create `dev`,
which reads as fresh. `dev` adds a typed resource and re-runs reasoning. The merge into
`main` copies the asserted triple and not the inferred one, reports
`inferences.excluded > 0` and `stale: true`, and `GET /$/reason/ds` reports `main` stale.
With `inferences: "include"`, the inferred triple is merged.

**A10. Validation.** `main` has a SHACL guard requiring one `<urn:name>` per
`<urn:Person>`. `dev` has no guard and adds a person without a name. The merge → `422`
with the report, and nothing is committed. `dryRun: true` → `200` with the C15 preview
and the failing validation.

**A11. Protected branches.** `PATCH /$/branches/ds/main {"protected":true}`. An update on
`main` → `403 branch-protected`. A merge into `main` → `200`. Unprotecting needs admin.

**A12. Deletion.** Deleting `dev` with unmerged commits → `409 unmerged`, with
`?force=true` → `204`, and its directory is gone after the response. Deleting `main` →
`400`. Deleting a branch that has a child → `409 has-children`.

**A13. Holds and compaction.** `dev` linked to `main`'s `gen-0001`. Compact `main`.
`GET /$/history/ds` lists `gen-0001` with `heldBy: ["branch:dev", "branch-base:dev"]`,
and Q on `dev` still works. Compact `dev`: `branches/<id>/gen-0001` exists, `dev` is no
longer linked, and `gen-0001` of `main` is held only by `branch-base:dev`. Merge `dev`
into `main`: the base pin moves to the merged commit and `gen-0001` is collected.

**A14. Reading inherited commits.** `dev` from `main` 2. `?branch=dev&at=1` reads `main`'s
commit 1. `?branch=dev&at=snapshot:x` reads `dev`'s snapshot x and is `404` for a
snapshot of `main`.

**A15. Crash safety (unit).** Fail after `branches/.create-*` is written, after the
rename, and after `branches.json`. Reopen: in the first two cases no branch exists and no
directory remains, and in the third the branch opens and reads its starting state. Fail
a merge after its merge record and before its log sync: reopen shows the old head and no
merge record. Copy a sealed `gen-0001` that only a branch holds back after a simulated
crash during `main`'s GC: reopen keeps it.

**A16. Access control.** A token with `write` on `ds` and `branches: ["dev*"]` can
update `dev`, create `dev2` and gets `404 no-such-branch` for `?branch=other`, the same
as for a branch that does not exist. Its update on `main` → `403`. A token with a graph
restriction cannot create branches or merge.

**A17. Criss-cross.** `main` and `dev` each merge the other after both made changes, then
both change again. A merge uses a virtual base of the two candidates and reports
`base: null`. When the two candidates changed the same cell differently, the merge →
`409 ambiguous-merge-base` with two candidates, and repeating it with one of them as
`base` merges.

**A18. Quota.** With a dataset quota slightly above its current size, a linked branch's
compaction is refused, `GET /$/compaction/ds?branch=dev` reports `quota`, and writes to
`dev` that fit still succeed.

**A19. Clone of a branch.** `POST /$/datasets/ds/clone?name=x&branch=dev&at=3` creates a
dataset whose quads equal `dev` at commit 3 and whose `forkedFrom` holds `dev`'s id and 3.
A blank node created in `x` gets a label that `ds` does not use.

**A20. Diff across branches.** `GET /ds/diff?fromBranch=main&toBranch=dev` gives
`T(main head → dev head)`, and it equals the diff of the two dumps. The RDF Patch form
applies to `main`'s dump to give `dev`'s.

**A21. Random histories (library).** Two hundred random sequences of inserts, deletes,
blank nodes, named graphs, compactions, bulk commits, branch creation and merges in both
directions. After each merge with `onConflict: "union"` and `conflicts: "quad"`, the
target equals the result of applying the three-way rule to dumps of the base, ours and
theirs. With `cell`, the reported conflicts equal those computed from the dumps.

**A22. CLI.** `sparkles branch create --loc db dev`, `sparkles update --loc db --branch
dev 'INSERT DATA {…}'`, `sparkles merge --loc db dev` exits 0. A conflicting merge exits
2 and prints the cells. `sparkles branch list --loc db --format json` works while a server
holds the database.

**A23. Performance (bench, not CI).** The targets of §6.4 on 10.5M quads, recorded in
`docs/BENCHMARKS.md`.

The examples from A24 on cover Phase 2 and the cherry-picks of Phase 3. They use the
setup above.

**A24. Squash merges.** `dev` at `main` 2 inserts c (commit 3) and d (commit 4).
`POST /$/merge/ds {"source":"dev","squash":true}` → `200`, `squashed: true`, a commit 3
on `main` of kind `merge` with the message `squash dev (commit 4) into main` and no
`mergedFrom`. Q on `main` equals Q on `dev`, and `GET /$/branches/ds/dev` still reports
`ahead: 2`. The same request again → `upToDate: true` and no commit. After `dev` inserts
e, a squash inserts only e. An ordinary merge after that records the second parent, and
`dev` is then 0 ahead. A protected `main` accepts squash merges.

**A25. Reverts.** `main` commit 3 inserts c, and commit 4 changes `<urn:a> <urn:age>`
from 30 to 31. `GET /$/revert/ds?commit=3` → `200` with `changes.deleted: 1`.
`POST /$/revert/ds?commit=3` → `200`, a commit 5 of kind `revert` with the message
`revert commit 3`, `base` commit 3, `source` commit 2 and `reverted: {branch:"main",
seq:3}`, no `mergedFrom`, and Q on `main` lists a and b only. The same request again →
`upToDate: true`. Reverting commit 1 → `409 merge-conflict` on the cell `<urn:a>
<urn:age>`, because commit 4 changed it, and with `{"onConflict":"theirs"}` the age is
gone. `?branch=dev&commit=2` on a new `dev` removes b from `dev` alone. `commit=0` →
`400 invalid-merge`, a commit past the head → `404`, and a revert on a protected branch
→ `403 branch-protected`. `sparkles revert --loc db 3` exits 0, and exits 2 on the
conflict.

**A26. Cherry-picks.** `dev` at `main` 2 inserts c (commit 3) and d (commit 4).
`POST /$/cherry-pick/ds?source=dev&commit=4` → `200`, a commit 3 on `main` of kind
`cherry-pick` with the message `cherry-pick commit 4 of dev`, `base` dev's commit 3,
`picked: {branch:"dev", seq:4}` and no `mergedFrom`. `main` holds d and not c. The same
request again → `upToDate: true`, and so does picking commit 2, which `main` already
has. A merge of `dev` afterwards inserts c only and reports no conflict. A commit of
`dev` that changes a cell `main` changed too → `409 merge-conflict`, and a protected
`main` → `403 branch-protected`. `&branch=qa` applies a commit to `qa`.
`sparkles cherry-pick --loc db dev 3` exits 0.

**A27. Replayed fast-forwards.** `dev` at `main` 2 inserts c as `ann` with the message
`add c` (commit 3), then deletes b (commit 4). `GET /$/merge/ds?source=dev&ff=replay`
lists two commits to replay. `POST /$/merge/ds {"source":"dev","ff":"replay"}` → `200`,
`replayed: [{from: dev 3, commit: 3}, {from: dev 4, commit: 4}]`, and `main`'s commits 3
and 4 have the kinds of `dev`'s, commit 3 the message `add c` and the author `ann`, and
each lists `replayedFrom` and no `mergedFrom`. Q on `main` equals Q on `dev`, and `dev`
is 0 ahead. A second replay → `upToDate: true`. A later commit of `dev` replays alone,
onto a protected `main` too. After `main` commits on its own → `409 not-fast-forward`.
When `dev` merged `main` after `main` moved, the base is not on `dev`'s own line →
`409 cannot-replay`, and an ordinary merge goes through. `ff: "replay"` with
`squash: true` → `400 invalid-merge`. `sparkles merge --loc db dev --replay` exits 0.

**A28. Renames.** `dev` at `main` 2 inserts c, and `feat` is created from `dev`.
`PATCH /$/branches/ds/dev {"name":"work"}` → `200` with `Location:
/$/branches/ds/work`, the same id and `grantsChanged: 0`. Q on `/ds@work` equals Q on
the old `dev`, `/ds@dev/sparql` → `404 no-such-branch`, an update of `work` answers
`Sparkles-Branch: work`, `feat` reports `upstream: "work"`, the commit listing of `work`
names `work`, the history's holds read `branch:work`, `branch.json` names `work`, and the
server's dataset object of the branch is kept under `ds@work`. Renaming to `feat` or
`main` → `409 branch-exists`, renaming `main` → `400`, and the name `dev` can be used
again. A token whose grant covers `dev*` can rename `dev2` to `dev3`, not to `other`
(`403`). After the owner renames `dev` to `feature`, `grantsChanged` is 1 and that token
gets `404` for `br@feature`. A crash after the table names `work` and before `branch.json`
does reopens with the branch named `work` and `branch.json` rewritten.
`sparkles branch rename --loc db dev work` works.

**A29. Re-parenting.** `dev` at `main` 2 inserts c (commit 3), `feat` from `dev` 3
inserts f, and `dev` inserts d. `DELETE /$/branches/ds/dev` → `409 has-children`.
`?reparent=true` → `409 unmerged`, because d is in no other history, and with
`&force=true` → `204`. The listing shows `main` and `feat`, `feat` has `upstream: "main"`
and `from: {branch: null, branchId: <dev's id>, seq: 3}`, its quads are unchanged, its
commit listing names commit 3 `branch: null`, and a read of it at commit 3 works.
`branches.json` has format 2 and lists `dev` under `retired`, and `dev`'s directory
remains. Compactions of `main` and a restart keep `feat` readable, a merge of `feat` into
`main` brings c and f, and the name `dev` can be used again. Deleting `feat` removes
the retired directory as well, and the table returns to format 1. A crash after the
table retires `dev` reopens with the same state, and a crash while the last child's
removal renames directories reopens with no branch directory left.

**A30. Exempt predicates.** `main` and `dev` each add a different `rdfs:label` to
`<urn:a>`. A merge → `409 merge-conflict` on that cell.
`GET /$/merge/ds?source=dev&exempt=<rdfs:label>` reports no conflict and two inserted
quads. `PATCH /$/branches/ds {"exemptPredicates":["<…#label>"]}` → `200`, the list
survives a restart and shows in `GET /$/branches/ds`, and the merge then keeps both
labels. With `<urn:age>` exempt and the `subject` scope, ages changed to 31 and 32 on the
two sides give both. A list entry that is not an IRI → `400`. `sparkles merge --exempt
IRI` and `sparkles branch exempt` do the same.

**A31. Merges as tasks.** `POST /$/merge/ds {"source":"dev"}` with `Prefer:
respond-async` → `202` with a task of kind `merge` that is cancellable,
`Location: /$/tasks/{id}` and `Preference-Applied: respond-async`. The task ends `done`
with the merge result as its `detail`. A merge that conflicts ends `failed` with the
conflict report as its `detail`. A revert with the same header runs as a `revert` task.
A dry run answers `200` at once. In the library, a merge under a `Control` reports
"reading the changes of both sides", "finding conflicts", "committing" and "committed",
and a cancel that comes while it commits leaves the target at its old head with no merge
record.

**A32. Gauges and backups.** With `dev` open and one commit ahead, `GET /$/metrics`
gives `sparkles_branch_quads` for `main` (2) and `dev` (3), `sparkles_branch_delta_quads`,
`sparkles_branch_wal_bytes` and `sparkles_branch_disk_bytes` with a `branch` label,
`sparkles_branches` 2 and `sparkles_branch_held_bytes` 0. After `main` compacts, the
held bytes are those of the generation `dev`'s link reads, and they are 0 again once
`dev` compacts. A backup of `main` records `branchesOmitted: 1` in its manifest, and the
dataset page's Backups panel says that the other branch is not backed up.

## 10. Open questions

1. **Exempt predicates.** Should `cell` scope exempt some predicates by default, such as
   `rdf:type`, `rdfs:label` and `skos:altLabel`, whose cells often gain values on both
   sides? Default: no exemptions in Phase 1, and per-merge exemptions in Phase 2.
   Answered in Phase 2: no predicate is exempt by default, and exemptions are given per
   merge and per dataset.
2. **Configuration.** Branches copy the configuration at creation. Should the validation
   shapes instead be shared, so `main`'s guard applies on every branch? Default: copy,
   so a branch can try new shapes.
3. **Base pins.** Each branch pins its merge base with its upstream, which can keep one
   generation of the upstream per branch. Should this be optional, with `410
   merge-base-gone` when the base was collected? Default: always pinned.
4. **Names.** Should names allow `/`, as in `feature/x`? It would need percent-encoding
   in the path form. Default: no.
5. **Path form.** `/{ds}@{branch}/…` or `/{ds}/branches/{branch}/…`? The second clashes
   with service names. Default: `@`.
6. **Branches of branches.** Should Phase 1 allow branches whose upstream is not `main`?
   Default: yes, with the depth limit of §4.2.
7. **Backups.** Should a backup include all branches, so a restore recreates them? It
   needs a manifest per branch and a restore of the branch table. Default: Phase 2 backs
   up one branch at a time.
8. **Idle branches.** Should open branch stores close after an idle period to free their
   delta memory? Default: stay open once opened, until the server stops.
9. **RDF Patch for merges.** Should a merge commit's patch carry its second parent in a
   header? Jena's documented headers are `id` and `prev`. Default: no extra header, and
   the commit listing gives the second parent.
10. **Per-branch quotas.** Default: none, and the dataset quota covers all branches.

## 11. Sources

- **Sparkles repository, read for this spec:**
  - the specs [C06](C06-clone-to-sandbox.md), [F06](F06-snapshots-and-point-in-time.md),
    [CI](CI-commit-identity.md), [C13](C13-automatic-compaction.md),
    [F05](F05-snapshot-repositories.md) (summary, restore and Outcome),
    [C12](C12-graph-access-control.md) (summary and model), and the openings of
    [C08](C08-inference-freshness.md), [C10](C10-write-time-validation.md),
    [C15](C15-write-previews.md) and [P01](P01-python-bindings.md);
  - `crates/sparkles-core/src/store.rs`: `Generation`, `Delta`, `Snapshot`, `WriterState`,
    `Store`, the WAL record constants, `bnode_for` and `parse_bnode_label`;
  - `crates/sparkles-core/src/id.rs`: the tag layout and the blank-node payload bits;
  - `crates/sparkles-core/src/history.rs`: `Hold` and the history state's leases;
  - `crates/sparkles-core/src/commit.rs`: `CommitKind` and `ForkedFrom`;
  - `crates/sparkles-core/src/store/quota.rs`, and the storage quota and diff sections of
    `docs/API.md`;
  - `crates/sparkles-server/src/state.rs`: `valid_name`.
- **Fetched:**
  - Dolt, "Merges" in the SQL reference,
    https://www.dolthub.com/docs/sql-reference/version-control/merges: cell-level data
    conflicts, `dolt_conflicts` tables, resolution with ours and theirs, constraint
    violations, and the refusal to commit with conflicts;
  - Project Nessie, "Specification", https://projectnessie.org/develop/spec/: the
    expected hash of commits and the rejection of conflicting commits;
  - lakeFS, "Merges", through a search of https://docs.lakefs.io/understand/how/merge/:
    the merge base, the three-way merge by object, and the `source-wins` and `dest-wins`
    strategies that apply to all conflicts at once;
  - TerminusDB's public documentation and blog, through a search: immutable delta layers
    per commit, the rebase model of merges, and squashed layers.
- **Cited from general knowledge, not fetched:**
  - Git's documentation of `git-merge`, `git-merge-base` (best common ancestors and
    criss-cross merges), `gitglossary`, and the branching chapter of *Pro Git*;
  - lakeFS's documentation of zero-copy branches and protected branches;
  - Datomic's documentation of `with`, `as-of` and `since`;
  - S. Khanna, K. Kunal and B. C. Pierce, "A Formal Investigation of Diff3", FSTTCS 2007,
    for three-way merges;
  - M. Shapiro, N. Preguiça, C. Baquero and M. Zawirski, "Conflict-free Replicated Data
    Types", SSS 2011, for sets with add and remove;
  - D. S. Parker et al., "Detection of Mutual Inconsistency in Distributed Systems",
    IEEE TSE 1983, for version vectors;
  - N. Arndt et al., "Decentralized Collaborative Knowledge Management using Git",
    Journal of Web Semantics 2019, and M. Graube et al., "R43ples: Revisions for Triples",
    2014, for versioned RDF stores that merge quad sets;
  - W3C RDF 1.2 Concepts (blank-node scope), RDF Dataset Canonicalization (RDFC-1.0),
    the SPARQL 1.1 Protocol, and Apache Jena's RDF Patch documentation.

## Outcome

**Delivered.** Phase 1 landed on 2026-10-03. The engine came first, then the HTTP API,
the CLI and the UI, each with its tests.

* **Engine.** `sparkles::branch` holds the public types. `store/branching.rs` keeps the
  branch table (`branches.json`), opens branch stores on first use, creates, protects and
  deletes branches, keeps the holds, records merges and computes frontiers and merge
  bases. `store/link.rs` opens linked generations, and `store/merge.rs` computes toggle
  sets, conflicts, resolutions and the blank-node rule, and writes the merge commit.
  `CommitKind::Merge` has code 13. A branch store is an ordinary `Store` rooted at
  `branches/<id>/`, so compaction, history, snapshots, the change log and write guards
  work on a branch unchanged.
* **Server.** `?branch=`, the path form and `branch=` in form bodies choose a branch
  before routing. `/$/branches/{ds}[/{name}]` and `/$/merge/{ds}` serve the routes of
  §2.4, and responses from a branch carry `Sparkles-Branch` and `Sparkles-Branch-Id`.
  Commit listings follow a branch's history into its upstream, the diff endpoint takes
  `fromBranch` and `toBranch`, clones copy a branch, grants take `branches`, and the
  compaction scheduler and the history upkeep cover open branches. The metrics
  `sparkles_branches`, `sparkles_branch_linked`, `sparkles_merges_total`,
  `sparkles_merge_conflicts_total`, `sparkles_merge_changes_total` and
  `sparkles_merge_seconds` are exported.
* **CLI.** `sparkles branch list|create|show|delete|protect`, `sparkles merge`, and the
  global `--branch` of `query`, `update`, `load`, `dump`, `log`, `diff`, `snapshot`,
  `compact`, `stats`, `clone` and `patch`. `sparkles check` verifies the branch table,
  the links and the merge records.
* **UI.** A branch menu in the dataset header keeps the choice in `?branch=`, and every
  panel of the dataset page works on the chosen branch. The query page has a Branch
  field next to At. The Branches panel creates, protects and deletes branches, warns
  before deleting one with unmerged commits, and merges a branch without conflicts after
  a preview, sending the previewed heads as `expect`. A conflicting merge shows the cells
  and the equivalent `sparkles merge` command. History marks merge commits and the branch
  of inherited commits. In-memory datasets show no branch controls.

Tests cover A1–A22. The library tests run A1, A3–A9, A11–A15, A17, A18, A20 and A21,
with failpoint crash tests for creation and for a merge between its record and its log
(A15), a partial compaction of a linked branch, and branches of branches past the depth
limit. The router tests run A1–A5, A7, A9–A12, A14, A16, A19 and A20, the CLI tests run
A22, and the UI's mock and real-server end-to-end tests cover the selector, the panel and
the Merge button. A21 runs 40 random histories in CI (`SPARKLES_A21_RUNS` raises it),
each with inserts, deletes, blank nodes, named graphs, compactions, bulk commits, branch
creation and merges in both directions, and checks every merge against the three-way
rule applied to dumps.

**Performance.** At 10.5M triples ([BENCHMARKS](../BENCHMARKS.md#branches-and-merges-105m-triples)),
creating a branch took 19.5 to 32.7 ms over HTTP and wrote about 1.4 KB of files; the
store's first open adds a 68 KiB change-log segment. A query on a new linked branch took
as long as on `main` (38.7 against 38.3 ms). A merge of 10,000 inserted quads into a
`main` with 10,000 of its own took 115 ms, and its preview 62 ms. An interleaved A/B of
the 1.05M query suite against the previous `main` showed no regression on `main`'s
paths: server CPU time per request at 0.94 times and wall time at 0.89 times the old
build's, both within the noise of a heavily loaded machine. The merge base of 64 branches
with 10,000 merges was not measured.

**Deviations.**

* **First generation.** A branch's first generation is `gen-0001`, linked or built, not
  `gen-0000`. Generation number 0 names an in-memory store's generation, so commits made
  in a `gen-0000` would have reported their generation as `mem`. Rebuilds continue
  from `gen-0002`.
* **Layered vocabulary.** A linked generation's delta vocabulary is one in-memory
  vocabulary that starts with a copy of the upstream entries below the recorded length,
  read from the segments' files, followed by the branch's own file. Ids are as §4.2
  gives them. The copy costs memory proportional to the upstream's delta vocabulary,
  which the branch's delta already costs in the same proportion.
* **No `BaseIndex` split.** A linked generation opens the upstream generation's files
  again, so the mappings are shared by the page cache rather than by one `Arc`. Branch
  stores share the dataset's block cache, and a linked generation takes the block-cache
  identities of the upstream permutations when the upstream generation is open, so the
  two read the same cached blocks.
* **The base pin stays.** `branch-base:NAME` pins the branch's starting commit for the
  branch's life instead of moving with merges. Every later merge between the branch and
  its upstream walks the logs from the newest commit both first-parent chains share,
  which is that starting commit, so a moved pin would not keep the merge possible. The
  pin is attributed like a named snapshot, to the newest retained generation that covers
  the commit. The change log answers the walks when generations are collected.
* **Main-level operations.** The branch-set operations (create, delete, protect, merge,
  merge bases, listings and reads that follow a history into the upstream) are methods of
  the dataset's own store. A branch store answers them with `400 invalid-branch`, since
  it cannot reach the dataset's store. `Store::branch(name)` returns a `BranchStore`,
  the store itself for `main` and a shared branch store otherwise. The facade adds
  `Dataset::branch`, `branches`, `create_branch`, `merge` and `delete_branch` in a new
  file.
* **One error variant.** `Error::Branch(Box<BranchError>)` carries the kind, the code
  and the HTTP status of every branch error, with the conflict report, the merge-base
  candidates or the inherited commit, in place of four variants.
* **Inherited commits.** A branch store answers a read of a commit it shares with its
  upstream with code `inherited-commit`. `Store::branch_snapshot_at` and
  `Store::branch_resolve` follow the history, and the server follows it for SPARQL reads
  with `at`, diffs and commit listings. Other admin routes answer such a read with `404
  inherited-commit`. `sparkles log --branch` lists the branch's own commits.
* **Merges commit through the delta.** A merge applies its changes as one write
  transaction even past the bulk threshold, so a very large merge is slower than a
  rebuild would be.
* **`take: objects`** needs the cell scope.
* **Result fields.** `target.seq` of a merge result is the target's head before the
  merge, and `commit` gives the merge commit. A preview lists the remaining conflicts
  with the report's members and counts them in `conflictCount`, since its own
  `conflicts` member is `{found, resolved}`. A dry run answers `200` with the write
  preview and puts the merge fields under `merge`.
* **Grants.** `branches` is a field of `[[grants]]` entries. A grant limited to branches
  alone, without graphs or endpoints, may be `admin`.
* **Subsequent storage work.** The reader requirement in `dataset.json` is now enforced
  before recovery and updated before publishing branch metadata. New branches reuse
  compatible upstream spatial/vector files and copy a pinned full-text checkpoint,
  replaying inherited WAL changes; missing or incompatible indexes rebuild safely.
  Per-branch gauges, `sparkles_branch_held_bytes`, the backup manifest's count of
  left-out branches and the Backups panel's note came with Phase 2.

The open questions kept their defaults: no exempt predicates, configuration copied at
creation, base pins always kept, no `/` in names, the `@` path form, branches of
branches with a depth limit of 4, backups of `main` only, branch stores open until the
server stops, no extra RDF Patch header, and the dataset's quota for all branches.

**Phase 2: the merge page and the commit graph.** Both landed on 2026-10-04.

* **Merge page.** `/datasets/[name]/merge?source=&target=` reads the preview with up to
  1,000 conflict cells and shows the merge base, the counts and the conflicts. The
  conflicts are grouped by graph, then by subject, with the base, ours and theirs objects
  side by side. Each row takes Ours, Theirs, Base or Both, and a subject's menu sets
  all its rows at once. A row's own choice wins over its subject's, which is how the
  server orders resolutions too. A rule for every conflict without a choice becomes
  `onConflict`, so a truncated report can still be merged. The changes come from a
  dry run of the merge with the choices made so far, which lists up to 500 changed
  quads. The dry run also runs the target's guard, so the page warns before the merge
  when validation would refuse it. Conflicts without a choice keep the target's
  values in that list. The merge sends the previewed heads as `expect`. On
  `head-moved` the page reads the branches again and keeps the choices for the cells
  still in conflict, and on `422` it shows the guard's results. The Branches panel's
  Merge button opens the page as soon as its preview finds conflicts. The page sends
  the existing preview and merge requests unchanged. Its options sit next to the commit
  message, where squash and revert controls can join them. `take: "objects"` is left to
  the CLI, since it needs values typed in.
* **Commit graph.** `GET /$/commit-graph/{ds}` and `Store::commit_graph` page the own
  commits of several branches by `(timestamp, ordinal, seq)`, newest first. A cursor of
  that key finds where each branch's run continues with a binary search, so a page
  costs at most `limit` catalog reads per branch. Each commit lists its parents: the
  previous commit, or the normalized starting commit for a branch's first commit, and a
  merge commit's merged commit. The route draws the branches the caller may read through
  the `info` endpoint, as `/$/commits` does, and a branch it names that the caller cannot
  see answers `404`. The UI gives each branch with own commits a lane in a depth-first
  walk from `main`, with siblings that started later nearer their upstream, so fork edges
  cross no lane in use at their row. A branch without own commits shows its name on the
  commit it started from. Runs of four or more commits of one branch that are not heads,
  merges or parents of forks and merges fold into a segment. Only the rows in view are
  drawn, and the graph loads 200 commits per page. Lane colours are the term colours,
  which have light and dark variants.

The mock server gained resolutions, dry runs, the commit graph, diffs of the commits it
records, and a small write guard read from `sh:targetClass` and `sh:minCount`, so the
mock end-to-end tests cover both features. The real-server end-to-end tests cover a
resolved merge, a guard refusal and the graph.

**Phase 2: merge kinds, renames, re-parenting, exemptions and tasks.** These landed on
2026-10-04, with cherry-picks from Phase 3.

* **Engine.** `store/merge.rs` splits a merge into resolving the heads and the merge
  base, and a shared three-way step that every kind of merge uses. Squash merges set
  `MergeOptions::squash`. `Store::revert` and `Store::cherry_pick` are three-way merges
  of a commit's parent over the commit and of a commit over its parent, and
  `store/replay.rs` replays a source's commits for `ff: "replay"`.
  `Store::rename_branch`, `Store::delete_branch_with`, `Store::merge_exempt` and
  `Store::set_merge_exempt` are in `store/branching.rs`. `MergeOptions` gains
  `squash`, `replay`, `exempt`, `progress` and `with_control`, and the facade adds
  `Dataset::revert`, `cherry_pick`, their previews, `rename_branch`,
  `delete_branch_with`, `merge_exempt`, `set_merge_exempt` and `merge_with`.
* **HTTP.** `POST /$/merge/{ds}` takes `squash`, `ff: "replay"` and `exempt`.
  `GET` and `POST` of `/$/revert/{ds}?branch=&commit=` and
  `/$/cherry-pick/{ds}?source=&commit=&branch=` preview and make reverts and
  cherry-picks, with the merge options in an optional JSON body. `PATCH
  /$/branches/{ds}/{name}` takes `name`, `DELETE` takes `reparent=true`, and `PATCH
  /$/branches/{ds}` sets `exemptPredicates`. A merge, revert or cherry-pick with `Prefer:
  respond-async` runs as a task.
* **CLI.** `sparkles merge --squash`, `--replay` and `--exempt`, `sparkles revert N`,
  `sparkles cherry-pick SOURCE N`, `sparkles branch rename`, `sparkles branch delete
  --reparent` and `sparkles branch exempt`.
* **Metrics.** `sparkles_branch_quads`, `sparkles_branch_delta_quads`,
  `sparkles_branch_wal_bytes`, `sparkles_branch_disk_bytes` and
  `sparkles_branch_held_bytes`.

Tests cover A24–A32. The library tests run all of them, with failpoint crash tests for a
rename after its table write, for a deletion that re-parents after its table write, and
for the removal of a retired branch's directory. The router tests run A24–A32, with
grants limited to branches for renames, reverts and cherry-picks, and the CLI tests run
A24–A30. The Phase 1 test A18 now waits for the change log's background writer before it
sets the quota. It had read the dataset's usage before the writer appended the load, so
a later walk counted 221,184 more bytes and refused the commit on a busy machine.

**Performance of Phase 2.** The default branch's paths gained one check in the commit
path, whether a commit carries a merge record. Four interleaved runs each of
`scripts/bench.sh 100000` with `ENGINES=sparkles`, against `main` at 6b82cf67 on a
machine that other builds kept at a load of about 7, gave server CPU time of 10.00 s
against 9.98 s per run, wall time of 22.8 s against 22.7 s, and query medians at 1.01
times the old build's as a geometric mean over 30 series. Eight interleaved loads of the
1.05M triples took 1.87 s of user CPU against 1.89 s. All of these are within the noise.

**Deviations of Phase 2.**

* **Commit kinds.** Reverts and cherry-picks have kinds of their own, `revert` (code 14)
  and `cherry-pick` (code 15), and record no second parent. A protected branch refuses
  them, since their changes do not come through a merge, and accepts squash merges and
  replays.
* **Cherry-picks.** The route mirrors reverts: `POST /$/cherry-pick/{ds}?source=&commit=&branch=`,
  where `branch` is the branch that receives the commit in both routes. The CLI's
  `revert` and `cherry-pick` write to the branch of the global `--branch`.
* **Squash merges** make no commit when they change nothing, since they record no second
  parent, and answer `upToDate: true`.
* **Replayed fast-forwards** need a target whose state equals the merge base's, which
  holds also after earlier replays and merges from the same source, and a merge base on
  the source's own first-parent history, or they answer `409 cannot-replay`. Each
  replayed commit records the commit it replays with a merge record flagged as a replay,
  shown as `replayedFrom`. Replayed commits get new times, and their authors come from
  the change log. A replay commits as it goes, so a cancel or a moving target stops it
  after complete commits.
* **Renames** leave the grants of the configuration file as written. The answer counts
  the grants whose `branches` cover only one of the two names, and the server log lists
  them. A rename commits with `branches.json`, and an open rewrites a stale
  `branch.json`.
* **Re-parenting** keeps the deleted branch, retired, in a `retired` list of
  `branches.json`, which then has format 2. Children keep its id as their starting
  point, and listings name it `branch: null`. Their upstream becomes the nearest listed
  ancestor.
* **Exempt predicates** live in `branches.json`, and a merge adds its own to the
  dataset's.
* **Gauges.** The per-branch values are new `sparkles_branch_*` families with `dataset`
  and `branch` labels, not a `branch` label on the per-dataset families, which keep their
  series. `sparkles_branch_held_bytes` counts upstream generations that links read and
  retired branches, not generations kept by base pins.
* The Branches panel supports renames, and Python bindings and the remaining Phase 2
  storage work are implemented, as described below.


**Phase 2: memory branches, standalone backups and index reuse.**

* Memory branches share immutable base vocabulary and mapped permutation indexes.
  Their inherited append vocabulary is copied to prevent one branch's rollback from
  truncating another's terms. Each branch has a private generation identity, delta,
  result cache, history, spatial/vector state and independently forked validation guard.
  RDFS configuration is inherited with a separate schema cache. Fork snapshots remain
  available for merges beyond the eviction of the memory commit ring and release their
  holds once no live or retired branch needs them. Only fork points are kept this way.
  A merge base that is the source commit of an earlier merge, and the anchors of a
  virtual base after a criss-cross, are read through the commit ring. Once such a
  commit is evicted, a merge that needs it fails with `410 merge-base-gone`. The default
  ring holds 65,536 commits per branch. Ordinals remain monotonic after deletion.
* `/$/backups/{ds}?branch=NAME`, the dataset backup handle, and
  `sparkles backup create --branch NAME` capture a selected branch as a standalone
  dataset. Linked branches and memory branches materialize a leased temporary
  generation; branches with their own persistent generation upload its files. Such a
  restored branch keeps its head, its blank-node numbering and its own commit records
  from the fork point on. The records of the commits it inherited stay with its
  upstream, as they do for the branch itself.
  Persistent materialization retains the persistent source type. The manifest's
  optional `dataset.branch` records the owning dataset UUID, captured branch UUID/name
  and next ordinal. Automatic restore always gives a branch backup a fresh dataset
  UUID, records `forkedFrom`, restores only `main`, and reserves inherited blank-node
  ordinals. Explicit identity preservation is refused for branch backups. Scheduled
  policies continue to capture `main` only.
* Compatible upstream spatial and vector files are mapped read-only with independent
  overlays. Reconfiguration and damaged-file fallback never modify upstream files.
  Full-text seeding pins committed segment metadata, copies and verifies that exact
  checkpoint, and replays every inherited WAL segment through the fork. A checkpoint
  after the fork or without complete replay coverage causes a safe rebuild.
* `dataset.json.minimumReader` defaults to 1 for legacy metadata. Branch-table
  publication requires reader 2, and a newer requirement or unknown dataset metadata
  format is refused before recovery mutates files. Restore checks the requirement
  before reidentification, even when integrity checking is disabled. Binaries predating
  this check still ignore the field, so the downgrade restriction remains necessary.
* Clone and backup capture admission follows cancellation and deadlines while waiting
  for a writer; the core options also support immediate no-wait refusal. Default
  capture APIs retain their existing behavior.

**Phase 3: recursive virtual bases and explicit Rust relinking.** Multiple best
common ancestors are recursively synthesized from sparse changes without publishing
a commit. Conflict-free synthesis reports `base: null`; conflicting ancestors refuse
with real candidates and retain explicit-base selection. Replay requires a real base.
Synthesis checks cancellation, deadlines, changed-quad budgets and bounded recursion.
Tests cover two/three best ancestors, recursive histories, memory stores, restart,
blank-node identity, deletes on both sides, exempt predicates, the quad scope and
conservative ancestor conflict refusal.

`Dataset::relink_branch` and `Store::relink_branch` explicitly move a persistent
currently linked branch onto main's captured immutable index. A new linked generation
contains a translated sparse `base.delta` overlay; compaction catch-up and atomic
CURRENT publication preserve branch identity, head and concurrent writes. Ordinary
compaction still builds an independent index. Main, memory and independent-index
branches are refused by this initial scope. Old upstream holds remain while historical
generations need them. Reader 3 is required before publication, and link format 2
authenticates the immutable overlay's length/checksum. Children inherit overlays and
WAL segments; standalone backups materialize their complete state. Restart, crash
boundaries, cancellation, concurrent writes, history, corruption, child branches and
materialized parent/child backup regressions pass. Core integrity checks report corrupt
link metadata and overlays. The offline `sparkles branch relink --loc db NAME` command
uses the controlled facade with responsive Ctrl-C handling, progress on stderr and
text/JSON reports. The facade reports the fraction reached at each stage of the relink.
It requires an existing database and exclusive directory lock, and remote mode and a
global branch selector are refused. Python's `Branches.relink` uses the same facade.
This first scope left out the HTTP surface, the Node and C bindings and cross-server
clones. The first three came later, as the last part of this section describes.

The relink records its hold on main's generation before the branch's `CURRENT` names
the relinked generation, so that main never collects a generation the branch reads. A
crash between the two leaves a hold that no generation of the branch needs. Opening
the dataset releases such holds, by comparing each branch's holds with the links of its
published generations on disk. The reader requirement of 3 is also written before
`CURRENT` and stays after such a crash, since lowering it would need proof that no
relinked generation remains anywhere in the dataset.


Memory branch creation publishes the initialized store and branch entry together, so
concurrent branch readers cannot fall through to opening a directory. The branch table
is not locked while the new store's text, spatial and vector indexes are built. The
ordinal is reserved first, and the name and branch limit are checked again before the
entry is published. Scoped backup
creation keeps the branch selector in its Location, including for dataset administrators.
Memory branch backups preserve graph-sourced ShEx validation settings as well as inline
schemas. Focused concurrency, authenticated link-following and restore/write-rejection
regressions cover these cases.

**Phase 3: relinking over HTTP and in the Node and JVM bindings.**

* **HTTP.** `POST /$/branches/{ds}/{name}/relink` relinks a branch through the same
  facade call as the CLI, `Dataset::relink_branch_with`. The body is empty or `{}`. The
  answer is the compaction report of the new linked generation with `dataset`, `branch`
  and `branchId`, described as `RelinkResult` in the OpenAPI description. With
  `Prefer: respond-async` the relink runs as a cancellable task of kind `relink`, whose
  `target` is the branch, whose progress follows the stages of the relink, and whose
  `detail` is the result. The route needs `admin` on the branch, so a grant limited to
  some branches may relink those it covers, and a read-only server refuses it as it
  refuses other branch mutations. Relinking `main` answers `400 invalid-branch`, a
  branch the caller cannot see `404 no-such-branch`, and a branch that owns its index or
  belongs to an in-memory dataset `409 not-relinkable`. The route is in `auth::ROUTES`,
  and the Node remote client's types were regenerated from the OpenAPI description.
* **Bindings.** Node's `Dataset.branches.relink(name, options)` and the JVM's
  `DatasetGraphSparkles.branches().relink(name[, operation])` call the same facade
  method. The JVM call goes through `branches_relink` of the native library's C ABI and
  returns a `RelinkReport`. Both take cancellation from their usual operation options.
  The parity table now maps the `relinkBranch` operation to the surface key
  `branches.relink`, with names in all three bindings, in place of the library-only key
  `dataset.relink_branch`.
* **CLI.** `sparkles branch relink` stays offline. It still refuses `--server`, since a
  server's own relink route serves that case.

The router tests cover relinking at once and as a task, a relinked branch that keeps
its id, head and data and takes writes afterwards, the refusals for `main`, a missing
branch, unknown body fields, a branch that owns its index and an in-memory dataset, the
grants that allow and refuse it, and the read-only server. The JVM and Node binding
tests relink a persistent branch after `main` compacts and check its id, head and
data.
