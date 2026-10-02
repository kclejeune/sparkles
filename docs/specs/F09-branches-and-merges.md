# F09: Branches and merges

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 covers branches of persistent datasets that share
> their parent's index until they compact, reads and writes on a branch, per-branch
> commits and history, three-way merges at the quad level with cell conflicts and
> resolutions, deletion, holds and quotas, access control by branch, the HTTP API, the
> CLI and a branch selector in the UI. Phase 2 adds the merge page in the UI, Python,
> squash merges, reverts, replayed fast-forwards, renames, in-memory branches and
> backups of branches. Phase 3 adds cross-server clones, relinking a branch to a newer
> base, virtual merge bases and cherry-picks.
>
> **User docs:** none yet.
>
> This is the design as written before implementation. The [Outcome](#outcome) section
> at the end will record how it lands.

This is a clean-room spec. It is Phase 3 of [C06](C06-clone-to-sandbox.md), which listed
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
on replicated sets. §10 lists the sources. Fluree was not consulted.

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

A new commit kind, `merge` with the next free code (12, after the `patch` kind of [F10](F10-replication.md)), marks merge commits. Creating or deleting a
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
| The branches have more than one best common ancestor and `base` is missing. | 409 | `ambiguous-merge-base` |
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
// crates/sparkles/src/branch.rs (new)
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
CommitKind::Merge                    // code 12, JSON "merge"
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
several, as in Git. Phase 1 then refuses the merge with `409 ambiguous-merge-base` and
lists the candidates, and the caller picks one with `base`. Phase 3 can merge the
candidates into a virtual base, as Git's recursive strategies do.

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
- Python (§2.10).
- Squash merges, which apply the changes as one commit without recording a second parent.
- Reverts, which are merges of a commit's parent with the commit as base:
  `POST /$/revert/{ds}?branch=&commit=`.
- `ff: "replay"`.
- Renaming branches, and re-parenting children when a branch is deleted.
- In-memory branches, which share the `Arc<Generation>` and clone the delta, so they cost
  nothing until they diverge.
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
- Cherry-picks, which are merges of one commit with its parent as base.
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
both change again. A merge → `409 ambiguous-merge-base` with two candidates. Repeating
it with one of them as `base` merges.

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

## 10. Open questions

1. **Exempt predicates.** Should `cell` scope exempt some predicates by default, such as
   `rdf:type`, `rdfs:label` and `skos:altLabel`, whose cells often gain values on both
   sides? Default: no exemptions in Phase 1, and per-merge exemptions in Phase 2.
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
  - `crates/sparkles/src/store.rs`: `Generation`, `Delta`, `Snapshot`, `WriterState`,
    `Store`, the WAL record constants, `bnode_for` and `parse_bnode_label`;
  - `crates/sparkles/src/id.rs`: the tag layout and the blank-node payload bits;
  - `crates/sparkles/src/history.rs`: `Hold` and the history state's leases;
  - `crates/sparkles/src/commit.rs`: `CommitKind` and `ForkedFrom`;
  - `crates/sparkles/src/store/quota.rs`, and the storage quota and diff sections of
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
- **Not consulted:** Fluree, in any form. Nothing from its source repository,
  documentation, site, tests or talks was opened, searched or fetched, and the project's
  notes that describe Fluree's features were not used.

## Outcome
