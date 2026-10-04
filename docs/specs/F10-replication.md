# F10: Applying RDF Patch, replication and read replicas

> **Status:** implemented in part (Phase 1). Phases 2 and 3 are deferred.
>
> **Phases:** Phase 1 applies RDF Patch to a dataset, as Fuseki's `patch` operation does,
> in the text and binary forms, with `H prev` as an optimistic concurrency check. It is
> built. Phase 2 adds read replicas that follow a primary over HTTP, keep its commit ids,
> bootstrap from a copy of a generation or from a backup, and can be promoted by hand.
> Phase 3 covers continuous archiving to a backup repository, synchronous replication,
> write forwarding and automated failover. Both wait until the rest of the planned work
> is done.
>
> **User docs:** [Applying RDF Patch](../API.md#applying-rdf-patch) in the API reference,
> and `sparkles patch` and `sparkles rdfpatch` in
> [the usage guide](../USAGE.md#command-line-tools).
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This spec was written from the RDF Patch and RDF Delta documentation,
the Apache Jena sources of the patch reader and of Fuseki's patch service, the PostgreSQL
documentation of streaming replication and hot standby, the LiteFS and Litestream
documentation, Oxigraph's API documentation, the Raft paper and the Sparkles code and
specs. §12 lists the sources.

It builds on durable commit identity ([CI](CI-commit-identity.md)), retained history and
the change feed ([F06](F06-snapshots-and-point-in-time.md)), background compaction
([C13](C13-automatic-compaction.md)), backup repositories and their capture
([F05](F05-snapshot-repositories.md)), and access control
([C09](C09-dataset-access-control.md), [C12](C12-graph-access-control.md)).

## 1. Summary, goals, non-goals

Sparkles writes RDF Patch. `GET /{ds}/diff` and the change feed `GET /{ds}/changes` emit
one patch per commit, in Jena's text form and in its RDF Thrift form, with `H id` and
`H prev` headers that name commits. Nothing in Sparkles reads a patch back. A Jena or
RDF Delta user cannot push a patch into Sparkles, and a second Sparkles server cannot
follow the first.

Every dataset is also a single node. Read load cannot be spread over several servers,
and when the server's disk or host fails, the newest state is the newest backup.

This spec adds two things.

* **Applying RDF Patch** (Phase 1, the item called J6 in the project's planning). A
  `patch` endpoint on each dataset takes a patch in either form and applies it as one
  commit. The rows `A`, `D`, `PA` and `PD` change quads and prefixes. `TX`, `TC` and
  `TA` delimit transactions, and `TA` aborts the request. A `H prev` header that names a
  commit of the dataset makes the patch apply only when that commit is the head, which
  is the check RDF Delta's patch logs make. A library API and a CLI command apply patches
  offline.
* **Replication** (Phase 2). A server can follow a dataset of another server. The
  follower, a *read replica*, pulls the primary's commits over HTTP as RDF Patches with
  extra headers, and applies each one as the same commit: the same number, timestamp,
  kind, message, blank-node ids and digest. It bootstraps from a copy of the primary's
  current generation, or from a backup in a repository, and copies a generation again
  when it falls too far behind. It serves reads at every commit it holds, refuses
  writes, and reports its lag. A client that wrote to the primary can read its own write
  from a replica by naming the commit it got back. An operator can promote a replica to
  a primary by hand.

**Goals.**

* A patch applied to a dataset gives the same data as the same changes made by SPARQL
  Update. A patch that Sparkles wrote for one commit, applied to the parent state of
  that commit in another dataset, gives the commit's state.
* A replica's state is always the state of some commit of the primary, and a replica
  never moves backwards. Commit `n` on a replica holds exactly the quads of commit `n`
  on the primary, and the replica checks that as it applies each commit.
* The primary's write path does not change. A replica costs the primary one long-poll
  request at a time and the reads of its logs.
* Replicas compact on their own schedule. A compaction on the primary sends nothing to
  replicas.
* A crash of either side at any point loses nothing that was acknowledged and leaves a
  replica that resumes where it stopped.
* The configuration is a few flags, a TOML file and NixOS options.

**Non-goals.**

* Consensus, quorum writes and automatic leader election. Phase 2 has one writable
  primary per dataset, asynchronous replication and manual promotion. §10 explains why.
* Writes on a replica, including writes forwarded to the primary (Phase 3).
* Multi-primary replication and merging of divergent histories.
* Replicating the server's registry, users, tokens and other server-level state. Each
  server is configured on its own.
* Replicating a filtered part of a dataset, such as some graphs.

## 2. User-visible behaviour

### 2.1 Roles

A role belongs to a dataset on a server, not to the server. One server can be the
primary of one dataset and a replica of another.

| Role | Writes | Reads | Follows |
|---|---|---|---|
| `primary` | Accepted | Served | Nothing. This is the role of every dataset today. |
| `replica` | Refused with `403 read-replica` | Served at every commit the replica holds | One primary, or another replica |
| `demoted` | Refused with `403 demoted` | Served | Nothing. A former primary that an operator took out of service after a promotion elsewhere. |

A *standby* is not a separate role. It is a replica that an operator means to promote if
the primary fails. A replica can serve as the primary of another replica, so replicas can
cascade.

### 2.2 Applying RDF Patch (Phase 1)

#### Requests

```
POST  /{ds}/patch      Content-Type: application/rdf-patch | application/rdf-patch+thrift
PATCH /{ds}/patch      (the same)
POST  /{ds}            Content-Type: application/rdf-patch | application/rdf-patch+thrift
```

These follow Fuseki's `PatchApply` service. Fuseki registers `patch` as a standard
endpoint of a writable dataset, takes `POST` and `PATCH`, and dispatches a `POST` to the
dataset's own URL by its content type. A missing content type, or
`application/x-www-form-urlencoded`, which `curl --data` sends, means the text form. A
charset other than UTF-8 is `415`. Fuseki answers `415` to the binary form, and Sparkles
accepts it, because it already writes it. `GET`, `PUT`, `DELETE` and `HEAD` are `405`,
and `OPTIONS` lists `OPTIONS,POST,PATCH`.

The request needs `write` on the dataset. C12 gains the endpoint name `patch`, which
covers these requests. A caller whose grants cover some graphs can write only quads in
graphs it writes, as with `INSERT DATA`, and cannot send `PA` or `PD` rows, as it cannot
change `/{ds}/prefixes`.

The body limit is `--max-upload-mb`, because a patch is a bulk transfer more often than
an edit. `dryRun=true` previews the patch as it previews other writes
([C15](C15-write-previews.md)). `Sparkles-Commit-Message` gives the commit a message.
Without that header, a `H message` row with a string literal does.

#### Rows

| Row | Meaning in Sparkles |
|---|---|
| `H name value .` | A header. `prev` is checked as below and `message` is used as above. Other headers are read and ignored, as Fuseki ignores all of them. |
| `TX .` (also `TB .`) | Ignored as a marker. The request is one transaction, as in Fuseki. |
| `TC .` | Ignored as a marker. |
| `TA .` | Aborts the whole request. Nothing is applied, and the answer is `200` with `"committed": false, "aborted": true`. Fuseki also answers success to a patch that aborts. |
| `Z .` | A segment marker. Ignored. |
| `A s p o [g] .` | Add a quad. Adding a quad that is present changes nothing. |
| `D s p o [g] .` | Delete a quad. Deleting an absent quad changes nothing. |
| `PA prefix iri [g] .` | Set a prefix of the dataset. |
| `PD prefix [g] .` | Remove a prefix of the dataset. |

Rows apply in order, so `A x` followed by `D x` leaves `x` absent. A row with no graph
term is in the default graph. Terms use N-Triples syntax, including triple terms written
`<<( s p o )>>` and directional language-tagged literals. A literal is checked as the
N-Quads loader checks it.

Fuseki wraps the patch in one transaction of its own and turns `TX` and `TC` into
counters, so a patch with several transactions commits once, and a `TA` anywhere aborts
everything. Sparkles does the same. A patch therefore makes at most one commit, of the
new kind `patch` (the next free code, 12, after the `embed` kind of [F08](F08-embeddings-on-write.md)). Open question 3 asks
whether to offer one commit per transaction.

Prefixes are not data in Sparkles, and a prefix change makes no commit
([CI §2.2](CI-commit-identity.md)). A patch with only `PA` and `PD` rows changes the
prefix map and answers `"committed": false`. A graph term on `PA` or `PD` is accepted
and does not narrow the change. Sparkles has one prefix map per dataset, and so does
Jena: a graph view of a Jena dataset adapts the dataset's prefixes (`GraphView` in
`jena-arq`), so Fuseki's `RDFChangesApply` changes the same map whichever graph a row
names.

#### Blank nodes

RDF Patch writes blank nodes with a "system identifier", so that a change can name a
blank node that already exists in the data. Jena keeps the label as the node's identity.
Sparkles stores blank nodes as numbers and labels them `_:b<hex>`
([API: Blank nodes](../API.md#blank-nodes)), so it applies this rule:

* When the patch's `prev` names a commit of this dataset's lineage (§4.7), a label
  exactly in the stored form `b<hex>` names the stored node with that number, if the
  dataset has allocated it. This is the case of a patch read from this dataset's own
  diff or change feed.
* Every other label is local to the request. Its first use makes a new stored node, and
  later uses in the same request name that node. This is what `INSERT DATA` does with a
  label.

A patch from another dataset, or from Jena, therefore adds new blank nodes and cannot
delete existing ones by label. Open question 13 asks for a parameter that chooses the
rule. Replication uses a third rule of its own (§4.2).

#### `H prev`

A `prev` header whose value is a commit IRI, `<urn:uuid:<dataset id>#commit:<n>>`, is a
precondition when the dataset id is this dataset's or an earlier id in its lineage
(§4.7). The patch applies only if commit `n` is the head, checked after the writer lock
is taken, as `If-Match` is checked for Graph Store writes. Otherwise the answer is `412`:

```json
{ "error": "patch expects commit 41 as the head of ds; the head is 43",
  "code": "prev-mismatch", "prev": "urn:uuid:3f1c…#commit:41", "head": 43 }
```

This is the check that an RDF Delta patch log makes when a patch is appended: the patch
names the log's latest entry in `prev`, and a mismatch rejects it. A patch that Sparkles
wrote for commit `n` has `prev` naming `n − 1`, so a chain of them applies in order and
fails at the first gap or repeat.

A `prev` that names another dataset, or is not a commit IRI, is ignored, and the receipt
says `"prevChecked": false`. That keeps a diff from one dataset applicable to another.
A patch without `prev` always applies.

#### Responses

The default body follows the SPARQL Update response of Sparkles, `200` with JSON:

```json
{ "inserted": 2, "deleted": 1, "prefixesSet": 0, "prefixesRemoved": 0,
  "rows": 5, "aborted": false, "prevChecked": true, "timing": { "totalMs": 0.8 } }
```

`inserted` and `deleted` count the rows that took effect. Receipt negotiation adds the
CI receipt members, and `Sparkles-Commit` and `Sparkles-Dataset-Id` are sent as on every
write.

| Condition | Status | `code` |
|---|---|---|
| Syntax error, with line and column | 400 | `patch-syntax` |
| A row with a term the store cannot hold, such as an ill-typed literal | 400 | `patch-term` |
| Unsupported content type or charset | 415 | |
| `prev` names this lineage and is not the head | 412 | `prev-mismatch` |
| Body over `--max-upload-mb` | 413 | |
| Write-time validation rejects the result | 422 | as for other writes |
| A replica or demoted dataset | 403 | `read-replica`, `demoted` |

A failed patch applies nothing.

#### CLI and library

```
sparkles patch --loc DB FILE…                   # apply, one commit per file
sparkles patch --server URL --dataset ds FILE…  # the same through the HTTP endpoint
sparkles tools rdfpatch FILE…                   # parse, print the rows and a summary
```

`sparkles patch` reads `-` as standard input and picks the form by the file's extension,
`.rdfp` for text and `.trp` for binary, as Jena names them, or by `--format`. It prints
the commit as `sparkles update` does. `sparkles tools rdfpatch` is Jena's `rdfpatch`
command, which parses patches, writes them back as text and prints the counts of data,
prefix and transaction rows to standard error. [G05](G05-command-line-tools.md) left it
out until Sparkles could read patches.

```rust
// sparkles::patch
pub struct PatchReader<R: Read> { /* text or binary */ }
pub enum PatchRow { Header(String, Term), Begin, Commit, Abort, Segment,
                    Add(Quad), Delete(Quad), PrefixSet(String, String), PrefixRemove(String) }
impl<R: Read> Iterator for PatchReader<R> { type Item = Result<PatchRow>; }

// Store and Dataset
pub struct PatchOptions { pub write: WriteOptions, pub binary: bool }
pub struct PatchOutcome { pub receipt: Receipt, pub rows: u64, pub aborted: bool,
                          pub prev_checked: bool, pub prefixes_set: u64, pub prefixes_removed: u64 }
impl Store { pub fn apply_patch(&self, r: impl Read, o: &PatchOptions) -> Result<PatchOutcome>; }
```

The Python package gains `Dataset.apply_patch(data, binary=False)`.

### 2.3 Replication endpoints on the primary (Phase 2)

Every dataset serves these routes. They work on a primary and on a replica, which is
what lets replicas cascade.

| Method | Path | Result |
|---|---|---|
| GET | `/$/replication/{ds}` | The dataset's replication status (§2.6). |
| GET | `/$/replication/{ds}/commits` | The commits after a commit, as RDF Patches (§2.3.1). |
| POST | `/$/replication/{ds}/captures` | Captures the current generation for a copy (§2.3.2). |
| GET | `/$/replication/{ds}/captures/{id}` | The capture's manifest. |
| GET | `/$/replication/{ds}/captures/{id}/files/{path}` | A file of the capture, in pieces. |
| DELETE | `/$/replication/{ds}/captures/{id}` | Releases the capture. |
| GET | `/$/replication/{ds}/settings` | The dataset's settings that replicas follow (§4.6). |

#### 2.3.1 The commit stream

```
GET /$/replication/{ds}/commits?after=N&wait=30&maxBytes=16777216&replica=NAME&applied=M
Accept: application/rdf-patch+thrift
Sparkles-Dataset-Id: 3f1c9a2e-…
```

The body is one RDF Patch per commit after `N`, oldest first, in the binary form or, with
`Accept: application/rdf-patch`, in the text form. Each patch leads from its parent's
state to its commit's state. It lists the commit's net changes, deletions first, as the
change feed does, and carries the commit's metadata as header rows:

```
H id <urn:uuid:3f1c9a2e-…#commit:42> .
H prev <urn:uuid:3f1c9a2e-…#commit:41> .
H timestamp "2026-10-02T14:03:11.482Z"^^<http://www.w3.org/2001/XMLSchema#dateTime> .
H kind "update" .
H inserted "2"^^<http://www.w3.org/2001/XMLSchema#integer> .
H deleted "1"^^<http://www.w3.org/2001/XMLSchema#integer> .
H quads "1204"^^<http://www.w3.org/2001/XMLSchema#integer> .
H flags "exact" .
H nextBnode "4711"^^<http://www.w3.org/2001/XMLSchema#integer> .
H message "fix labels" .
H digest "9f2c…" .
TX .
D <urn:a> <urn:p> "1"^^<http://www.w3.org/2001/XMLSchema#integer> .
A <_:b1f> <urn:p> "two"@en <urn:g1> .
A <urn:b> <urn:p> <urn:c> .
TC .
```

`flags` lists `exact`, `bulk` and `unvalidated` as they apply, separated by spaces.
`message` and `digest` appear when the commit has them. Every patch is valid RDF Patch,
so Jena's `rdfpatch` reads the stream, and an RDF Delta tool can store it as a log.

A bulk commit is sent as a patch with header rows and no transaction, plus
`H transfer "copy"`. It tells the replica to copy a generation (§4.3) instead of
applying changes. With `bulkChanges=true` the primary sends the bulk commit's changes
instead, computed by comparing the two states as the change feed does. The replica asks
for that only under `bulk = "stream"` (§2.8).

The page ends after `maxBytes` of patches, which defaults to 16 MiB, or after 10,000
commits. It always holds at least one whole commit, however large, because a commit is
never split. Unlike the change feed, the stream has no row budget, does not refuse large
commits with `507`, and never leaves changes out. The response streams, so a large
commit costs the primary the memory of one walk of its log, not of the whole body.

With `wait=S`, at most 60 seconds, a request that finds no commit after `N` waits for one,
as the change feed's long poll does. The response headers are:

```
Sparkles-Dataset-Id: 3f1c9a2e-…
Sparkles-Head: 57
Sparkles-Replication-Next: 49          (the last commit in the body, or N)
Sparkles-Settings-Tag: "a41c…"         (changes when the followed settings change)
```

`replica` names the follower and `applied` reports the commit it has made durable. The
primary records both, as PostgreSQL's standbys report their replay position, and they
drive the follower list and holds of §2.3.3.

The request's `Sparkles-Dataset-Id` is the follower's dataset id. If it is not the
primary's, the answer is `409 lineage-mismatch` with the primary's id and lineage (§4.7).
A commit after `N` that the primary can no longer read is `410 history-gone`, as for the
change feed. Both tell the replica what to do next (§4.3, §2.7).

#### 2.3.2 Captures

A capture is the consistent copy of one generation that a backup uploads
([F05 §5.3](F05-snapshot-repositories.md)), served over HTTP instead. `POST` takes the
writer lock for about a millisecond, leases the current generation, records the WAL,
delta-vocabulary and catalog lengths at the head, and answers `201` with a manifest:

```json
{ "id": "c-6f0e…", "expires": "2026-10-02T15:03:11.482Z",
  "dataset": { "name": "ds", "id": "3f1c9a2e-…", "type": "persistent" },
  "commit": { "seq": 57, … }, "generation": "gen-0009", "indexFormat": 2,
  "pieceBytes": 33554432,
  "files": [ { "path": "gen-0009/spo.dat", "kind": "immutable", "size": 41234567 }, … ] }
```

The files are those of an F05 backup, with the same exclusions, plus `annotations.bin`.
`GET …/files/{path}?piece=K` returns piece `K` of a file, 32 MiB at most, with
`Content-Digest: sha-256=:…:` (RFC 9530). The replica checks each piece's digest and
retries a piece, not a file. An in-memory primary builds its capture as its F05 backups
do, in a temporary generation on disk.

A capture lives until it is deleted, or for `--replication-capture-ttl` (one hour) after
its last request. At most `--replication-max-captures` (4) are open per dataset, and a
fifth `POST` is `429` with `Retry-After`. A capture holds its generation as a backup
lease does, shown as `capture:<id>` in `/$/history/{ds}`.

#### 2.3.3 Followers and holds

The primary keeps a record of each follower that named itself with `replica=`: its
applied commit, its address and when it was last seen. `GET /$/replication/{ds}` lists
them.

A follower's record also *holds* history, in the way a PostgreSQL replication slot keeps
WAL. The generations that own the commits after the follower's applied commit are kept
([F06 §4.2](F06-snapshots-and-point-in-time.md)), so a follower that was away can catch
up from the logs instead of copying a generation. A hold shows as `replica:<name>` in
`heldBy`. Holds are bounded the way the retention window is. They share
`--history-max-generations` with it, pins always win, and a hold ends after
`--replication-hold-ttl` (one hour) without a request from its follower. PostgreSQL's
documentation warns that slots can fill the disk, and `max_slot_wal_keep_size` exists for
that reason. A hold with a time limit and a generation limit cannot grow without bound.
The records and holds are stored in the dataset's `replication.json`, so they survive a
restart of the primary.

### 2.4 A replica

A replica is a persistent dataset whose `replication.json` says `"role": "replica"` and
names its primary. It has the primary's dataset id. It runs a *follower*, one task per
replicated dataset, which loops:

1. Read the settings tag. When it changed since the last round, fetch
   `/$/replication/{ds}/settings` and apply it (§4.6).
2. Request the commits after the replica's head, with `wait=30`.
3. Apply each commit as the same commit (§4.2). Apply a `transfer "copy"` commit by
   copying a generation (§4.3).
4. On `410`, copy a generation. On `409 lineage-mismatch`, follow the lineage or stop
   (§2.7). On an error of the network or the primary, retry with a backoff from one
   second to 30 seconds.

Writes to a replica are refused before anything is read:

```json
{ "error": "ds is a read replica of https://primary.example.org/ds; send writes to the primary",
  "code": "read-replica", "primary": "https://primary.example.org/ds" }
```

The status is `403`, as for a `--read-only` server. `primary` is included only when the
operator set `advertise` (§2.8), because the address the replica uses can be private.
The engine refuses the write too, so the reasoning loop, the MCP write tool and library
callers cannot write either. These operations stay allowed on a replica, because they
change nothing that the primary's commits define: compaction, named snapshots and
retention, backups to repositories, clones into new datasets, cache clearing, and index
rebuilds. Reasoning runs, restores in place and changes to followed settings are refused.

Every response from a replica dataset carries one more header, an RFC 9651 dictionary:

```
Sparkles-Replication: role=replica, primary-head=1005, lag-ms=120
```

### 2.5 Consistency and reading your writes

A replica guarantees three things:

* **Consistent prefix.** Every state a replica publishes is the state of a commit of the
  primary, and commit `n` on the replica has the data of commit `n` on the primary.
* **Monotonic reads.** A replica's head never decreases, including across a restart and a
  copied generation (§4.3). A client that keeps talking to one replica never sees time go
  backwards.
* **Bounded lag that is reported.** The lag is in the status, the metrics and the
  `Sparkles-Replication` header (§2.6).

Reads on a replica never conflict with the commits it applies. A PostgreSQL hot standby
can cancel a query whose rows replay is about to remove, after
`max_standby_streaming_delay`. A Sparkles reader holds an immutable snapshot, and a newer
commit only publishes a newer snapshot.

A client that wrote to the primary and reads from a replica can wait for its write:

```
GET /ds/sparql?query=…&minCommit=1001
Sparkles-Min-Commit: 1001                 (the same, as a header)
Sparkles-Dataset-Id: 3f1c9a2e-…           (optional: the id from the write's response)
```

`minCommit` works on every read route of a dataset, on a primary and on a replica. The
request waits until the dataset's head is at least the commit, then reads the head as
usual. It waits at most `minCommitWait` seconds, which defaults to
`--min-commit-wait` (10) and is capped at 60. When the wait ends first, the answer is
`503` with `code: "commit-not-visible"`, `Retry-After: 1` and `Sparkles-Head`. A request
with `at` reads that commit and needs no `minCommit`. On a replica, an `at` naming a
commit after the replica's head waits the same way.

When the request names a dataset id, the server checks it against its lineage (§4.7). An
id of an earlier lineage with a commit at or before the point where that lineage ended
is satisfied at once, because the states are the same. An id of an earlier lineage with
a later commit is `409 commit-not-in-lineage`, which means the commit was lost in a
failover. Any other id is `409 dataset-id-mismatch`.

The web UI sends `minCommit` after an update it made, so its next query sees the update
even behind a load balancer.

### 2.6 Lag, health and readiness

`GET /$/replication/{ds}` on a replica returns:

```ts
type ReplicaStatus = {
  dataset: string; datasetId: string; role: "replica";
  primary: { url: string; dataset: string };
  state: "bootstrapping" | "streaming" | "copying" | "disconnected" | "stalled"
       | "diverged" | "paused";
  applied: number;              // the replica's head
  primaryHead: number | null;   // the primary's head when last seen
  lagCommits: number | null;    // primaryHead − applied
  lagMs: number;                // how long the oldest commit not applied has been known
  lastContact: string | null;   // RFC 3339, the last answer from the primary
  lastError: { at: string; message: string; code?: string } | null;
  copies: number;               // generation copies since the server started
  appliedSinceStart: number; bytesReceived: number;
  settingsTag: string;
};
```

On a primary it returns `role: "primary"`, the head, the followers with their applied
commits and lag, the open captures and the holds.

`lagMs` is measured on the replica's clock alone. It is the time since the replica first
saw the newest primary head it has not reached, and 0 when it has caught up. That keeps
it free of clock skew between the servers. The primary's commit timestamps are still
available in each commit for those who want the age of the data.

`/$/ready/{ds}` gains `replication: { role, state, applied, primaryHead, lagCommits,
lagMs }`. A replica dataset is not ready while it bootstraps, and is not ready in state
`diverged`. It stays ready while it is disconnected, because its data is consistent and
only stale, as a PostgreSQL standby keeps serving when its primary goes away. An
operator who would rather drain a lagging replica sets `max_lag_commits` or
`max_lag_seconds` (§2.8), and the dataset is not ready while the lag passes them.

The metrics gain:

| Metric | Type | Labels |
|---|---|---|
| `sparkles_replication_applied_commit` | gauge | `dataset` |
| `sparkles_replication_primary_head` | gauge | `dataset` |
| `sparkles_replication_lag_commits` | gauge | `dataset` |
| `sparkles_replication_lag_seconds` | gauge | `dataset` |
| `sparkles_replication_last_contact_seconds` | gauge | `dataset` |
| `sparkles_replication_state` | gauge, 1 for the current state | `dataset`, `state` |
| `sparkles_replication_commits_applied_total` | counter | `dataset` |
| `sparkles_replication_bytes_received_total` | counter | `dataset`, `kind="commits\|copy\|settings"` |
| `sparkles_replication_copies_total` | counter | `dataset`, `reason="bootstrap\|bulk\|gap\|lineage"` |
| `sparkles_replication_errors_total` | counter | `dataset`, `code` |
| `sparkles_replication_followers` | gauge, on a primary | `dataset` |
| `sparkles_replication_follower_lag_commits` | gauge, on a primary | `dataset`, `replica` |

### 2.7 Promotion and failover

Phase 2 has manual failover only. When the primary is lost, an operator picks the
replica with the highest applied commit and promotes it:

```
POST /$/replication/{ds}/promote        { "catchUp": 30 }
sparkles replication promote --server URL --dataset ds [--catch-up 30s]
sparkles replication promote --loc DB                     # a database no server holds
```

With `catchUp`, the replica first tries for up to that many seconds to apply the commits
the primary still has. That is useful when the primary is reachable and is being retired.
Then promotion:

1. stops the follower and takes the writer lock;
2. records the old dataset id and the head in `lineage` and gives the dataset a new id,
   with `forkedFrom: { id, seq }` and the origin `promote`, through
   `commit::reidentify`;
3. sets the role to `primary`, and turns on what a replica keeps dormant: the write-time
   validation guard, the reasoning schedule and automatic writes;
4. publishes, so that the next write gets the head plus one, under the new id.

The answer gives the new id, the old id and the fork commit. Commits the old primary made
after that commit and never sent are lost. That loss is the price of asynchronous
replication, and §4.7 makes it visible instead of silent.

Promotion needs `server-admin`, because it changes which server takes writes. Clients,
load balancers and DNS are pointed at the new primary by the operator.

The old primary must stop taking writes. If it is still running, the operator demotes it:

```
POST /$/replication/{ds}/demote
sparkles replication demote --server URL --dataset ds
```

A demoted dataset refuses writes with `403 demoted` and keeps serving reads, and the role
survives a restart. Sparkles has no means of its own to stop a primary it cannot reach.
The operator fences it at the load balancer, the firewall or the host, as PostgreSQL's
documentation leaves fencing to the operator.

Other replicas are pointed at the new primary through their configuration or with
`POST /$/replication/{ds}/follow { "primary": URL, "dataset": NAME }`. Their next request
gets `409 lineage-mismatch`, because their id is the old one, and they compare their head
with the new primary's lineage (§4.7):

* A replica whose head is at or before the fork commit takes the new id with
  `reidentify` and goes on streaming. This is how a PostgreSQL standby follows a timeline
  change with `recovery_target_timeline = 'latest'`.
* A replica whose head is after the fork commit holds commits the new primary does not
  have. It stops in state `diverged`. `POST /$/replication/{ds}/reseed` replaces it with a
  copy of the new primary, and keeps the old directory as
  `databases/.replaced-<ds>-<time>` for salvage. `sparkles diff --loc OLD FORK HEAD
  --format patch` writes the lost commits as one RDF Patch, which an operator can review
  and apply to the new primary with Phase 1's `patch` endpoint.

A former primary rejoins the same way, as a replica of the new primary, and is usually in
the second case.

### 2.8 Configuration

Short flags cover one primary:

| Flag | Default | Meaning |
|---|---|---|
| `--replica-of URL` | | The base URL of the primary server. |
| `--replicate DS[=REMOTE]` | | Replicate the primary's dataset `REMOTE` as the local `DS`. Repeatable. |
| `--replication-token-file PATH` | | A file with the bearer token for the primary, read at each request. |
| `--replication-config FILE` | | The TOML file below, re-read on SIGHUP. |
| `--min-commit-wait SECS` | 10 | The default wait of `minCommit`. |
| `--replication-hold-ttl DUR` | `1h` | How long a silent follower's hold lasts on this server. `0` turns holds off. |
| `--replication-capture-ttl DUR` | `1h` | How long an idle capture lasts. |
| `--replication-max-captures N` | 4 | Open captures per dataset. |

The TOML file covers several primaries and per-dataset settings. Like the backup
configuration, it holds no secrets.

```toml
[defaults]
primary = "https://primary.example.org"
token_file = "/run/secrets/sparkles-replication"
ca_file = "/etc/ssl/internal-ca.pem"     # optional, for a private CA
wait_seconds = 30
apply_batch = 256                        # commits per fsync (§4.2)
bulk = "copy"                            # or "stream"
advertise = false                        # name the primary in write refusals

[[replica]]
dataset = "wiki"
remote = "wiki"                          # the dataset's name on the primary
name = "replica-eu-1"                    # how the primary lists this follower
bootstrap = "primary"                    # or { repository = "s3-main", backup = "latest" }
max_lag_commits = 1000                   # readiness thresholds, off when absent
max_lag_seconds = 30

[[replica]]
dataset = "catalog"
primary = "https://other.example.org"
allow_http = true                        # plain HTTP, for a loopback or test primary
```

A dataset named in the configuration that does not exist is created by its bootstrap. A
dataset whose `replication.json` says it is a replica, but which the configuration no
longer names, stays a read-only replica in state `paused` and is logged, so a restart
with a missing flag cannot turn a replica into a second primary. `promote` is the only
way out of the role.

The NixOS module gains:

```nix
services.sparkles.replication = {
  primary = "https://primary.example.org";
  tokenFile = "/run/secrets/sparkles-replication";
  datasets = { wiki = { }; catalog = { remote = "catalog-v2"; maxLagSeconds = 30; }; };
  configFile = null;            # or a TOML file outside the Nix store
  minCommitWait = 10;
};
```

The module renders the TOML file from these options unless `configFile` is set, and
asserts that `tokenFile` and `configFile` are absolute paths outside the Nix store.

### 2.9 CLI

```
sparkles replication status  --server URL --dataset ds [--format text|json]
sparkles replication status  --loc DB                 # role, lineage, offline
sparkles replication promote --server URL --dataset ds [--catch-up DUR]
sparkles replication promote --loc DB
sparkles replication demote  --server URL --dataset ds
sparkles replication pause|resume|reseed --server URL --dataset ds
sparkles replication follow  --server URL --dataset ds --primary URL [--remote NAME]
```

The text status of a replica reads:

```
wiki  replica of https://primary.example.org/wiki  streaming
  applied 1005 · primary head 1007 · lag 2 commits, 120 ms · last contact 0.4 s ago
  generation gen-0012 (local) · copies 1 · dataset 3f1c9a2e-…
```

### 2.10 Rust library API

```rust
// sparkles::replication (new)
pub struct ReplicatedCommit {
    pub commit: CommitInfo,          // seq, timestamp, kind, counts, flags as on the primary
    pub next_bnode: u64,
    pub message: Option<Arc<str>>,
    pub digest: Option<[u8; 32]>,
    pub changes: Vec<(DiffOp, Quad)>,
}
pub enum Role { Primary, Replica { primary: PrimaryRef }, Demoted }
pub struct LineageEntry { pub id: Uuid, pub end: u64, pub promoted_ms: i64 }

impl Store {
    pub fn role(&self) -> Role;
    pub fn set_role(&self, r: Role) -> Result<()>;
    /// Apply commits that follow the head, in order, with one fsync; fails on a gap,
    /// a count or digest mismatch, or an add of a present quad (Error::Diverged).
    pub fn apply_replicated(&self, commits: &[ReplicatedCommit]) -> Result<u64>;
    /// Switch to a copied generation whose base is at or after the head (§4.3).
    pub fn adopt_generation(&self, staged: &Path) -> Result<CommitInfo>;
    pub fn lineage(&self) -> Vec<LineageEntry>;
    pub fn promote(&self) -> Result<(Uuid, LineageEntry)>;
}
```

## 3. Prior art and standards basis

* **RDF Patch** (Andy Seaborne). Rows of `H`, `TX`, `TC`, `TA`, `PA`, `PD`, `A` and `D`,
  with header keys and RDF terms. Blank nodes carry a system identifier so that a change
  can name an existing node. The `id` and `prev` headers chain patches into a log, and
  `TA` exists so that changes can be streamed without buffering. A binary form uses RDF
  Thrift. Sparkles writes both forms already
  ([F06 Outcome](F06-snapshots-and-point-in-time.md#outcome)).
* **RDF Delta patch logs.** A patch log is an append-only sequence of patches. Appending
  requires `prev` to name the log's latest patch, and a mismatch rejects the patch, which
  serializes writers. With an initial state and the log, any later version can be
  rebuilt. Phase 1's `prev` check and Phase 2's stream follow this model, with the commit
  number in the IRI so that the order is also arithmetic.
* **Apache Jena and Fuseki.** `PatchApply` takes `POST` and `PATCH`, defaults to the text
  form, answers `415` to other content types and to the binary form, and applies the
  patch inside one write transaction, with `RDFChangesExternalTxn` turning `TA` into an
  abort of the whole request. `RDFChangesApply` ignores headers and segments and puts
  prefix rows on the graph's prefix mapping, which a graph view shares with its dataset.
  `RDFPatchReaderText` accepts `TB` as a synonym of `TX`, reads `<_:label>` as a blank
  node, and reads triple terms. Fuseki registers `patch` among the standard endpoints of a
  writable dataset and dispatches `application/rdf-patch` bodies sent to the dataset URL.
* **PostgreSQL streaming replication and hot standby.** A standby connects to the primary
  and pulls WAL as it is written, asynchronously by default and usually under a second
  behind. Replication slots keep WAL for standbys, with `max_slot_wal_keep_size` as the
  bound. If WAL a standby needs is gone, it needs a new base backup. A hot standby
  serves read-only queries and refuses writes. Cascading standbys relay WAL. Promotion is
  manual (`pg_promote`) and starts a new timeline, which other standbys follow, and a
  standby whose history went past the fork needs rebuilding. `pg_stat_replication`
  reports each standby's positions for lag. `synchronous_commit = remote_apply` makes a
  commit wait until standbys have applied it. Sparkles takes pull, base copies, slots
  with a bound, the hot-standby rules, timeline following, cascading and position
  reports. It replaces physical WAL with a logical stream (§10).
* **LiteFS and Litestream.** LiteFS replicates SQLite page sets as LTX files and keeps a
  replication position made of a transaction id and a rolling checksum of the whole
  database. A node whose position shows a split takes a fresh snapshot from the primary.
  One node is primary at a time, chosen by a lease. Litestream ships WAL to object
  storage and restores from a snapshot plus WAL. Sparkles' position is the commit number
  with the optional chained digest, its re-seed is LiteFS's snapshot, and Phase 3's
  archive follows Litestream.
* **Oxigraph.** `Store::open_secondary` in Oxigraph 0.3 opened "a read-only clone of a
  running read-write Store", which follows the primary's files on the same file system
  after a lag. Sparkles rejects that shape of follower (§10).
* **Raft and etcd.** Raft (Ongaro and Ousterhout, 2014) replicates a log by majority, with
  leader election by terms, log matching and membership changes. etcd uses it and
  recommends odd cluster sizes of three or five. §10 explains why Phase 2 does without it.
* **HTTP.** RFC 9110 for the status codes (`403`, `409`, `410`, `412`, `415`, `429`,
  `503`), RFC 9530 for `Content-Digest`, RFC 9651 for the `Sparkles-Replication`
  dictionary and the integer headers, and RFC 6648 for header names without `X-`.

## 4. Semantics

### 4.1 Commit identity on replicas

CI requires that the pair (dataset id, seq) names exactly one state. A replica keeps the
primary's id and applies each commit with the primary's seq, so the pair names the same
state on both. Nothing else may write to a replica, which is what keeps the pair unique.

The commit record a replica stores copies the primary's timestamp, kind, counts, flags,
message and digest. The one field it does not copy is `generation`. Generation numbers
are local to a server, because each server compacts on its own, and F06 derives a
generation's range from the catalog's `generation` field. A replica's catalog record
names the local generation that holds the commit, or none (generation 0, shown as
`null`) for a commit the replica received in a copied catalog but never held. `GET
/$/commits/{ds}` on the two servers therefore differs in `generation` only.

### 4.2 Applying a replicated commit

`Store::apply_replicated` applies commits that follow the head, in order, and takes these
steps for each one.

1. It checks that the commit's seq is the head plus one. A commit at or before the head
   is skipped, because a retried page repeats commits. A gap fails.
2. It applies the changes in a write transaction without the validation guard and
   without the quota's refusal. The primary validated the commit, and a replica that
   refused it would diverge. A replica out of disk stops in state `stalled` and retries.
3. It expects each `D` row's quad to be present and each `A` row's quad to be absent. The
   rows are net changes against the parent, so anything else means the replica's state is
   not the parent's, and the commit fails with `Error::Diverged`.
4. It checks the net counts against `inserted` and `deleted` when the commit is `exact`,
   and the resulting quad count against `quads` always.
5. It maps blank-node labels by number. In this mode every label is the primary's stored
   form, `_:b<hex>`, and names the node with that number, whether or not the replica has
   seen it. The replica's blank-node counter becomes the larger of its own and the
   commit's `nextBnode`. Blank nodes therefore have the same ids, and the same labels in
   every output, on both servers, and a promoted replica never reuses an id.
6. It writes the WAL commit record with the primary's seq, timestamp, kind and flags
   ([CI §5.3](CI-commit-identity.md)), and the annotation with the message.
7. When digests are on, which a replica inherits from the primary's `annotations.bin`, it
   computes the digest and fails with `Error::Diverged` if it differs from the commit's.

The digest chains each commit to its parent's, so a match checks the whole history the
replica holds, not only the last commit, as LiteFS's rolling checksum does.

The follower applies up to `apply_batch` commits (256) or 64 MiB of changes with one WAL
fsync, then publishes the last of them. No client waits on a replica's commit, so there
is nothing to acknowledge per commit. A crash before the fsync loses commits that were
never published, and the follower fetches them again. A replica therefore applies a
stream of small commits faster than the primary made them, because the primary paid one
fsync for each.

The term ids of a replica differ from the primary's, because each interns terms into its
own delta vocabulary and builds its own generations. The term-to-id mapping is a
bijection on both servers ([API: Blank nodes](../API.md#blank-nodes) and the inline
values of `id.rs`), so equal terms give equal quads.

### 4.3 Bootstrap, copies and re-seeding

A replica gets its first state, and later catches up past what the logs can give it, by
copying a generation. A copy is the same operation in all of these cases:

| Case | Cause |
|---|---|
| Bootstrap | The dataset does not exist on the replica. |
| Bulk | The next commit is a bulk commit and `bulk = "copy"`. A bulk commit rebuilds the primary's generation, and its changes would cost a full comparison of two states. |
| Gap | The primary answered `410`, because no retained generation covers the next commit. |
| Lineage | An operator asked for `reseed` after a divergence or a promotion elsewhere. |

The follower opens a capture, downloads every piece into a staging directory under the
dataset, checks each piece's `Content-Digest`, runs `sparkles::check` in quick mode, opens
the staged store and checks that its head and quad count equal the manifest's commit, as
an F05 restore does. Downloads resume by piece after a network error, while the capture
lives. The capture is released at the end.

A bootstrap with `bootstrap = { repository, backup }` restores a backup instead, with
identity `keep` ([F05 §2.3](F05-snapshot-repositories.md)), and then streams the commits
after the backup's commit from the primary. `backup = "latest"` picks the newest backup
of the primary's dataset id in that repository. That moves the bulk of the bytes from
object storage instead of from the primary. If the primary cannot read the commit after
the backup's, the follower falls back to a capture.

Installing the copy depends on the case.

* **Bootstrap.** The staged directory is renamed into `databases/<ds>` and opened, as an
  F05 restore into a new dataset is.
* **Bulk and gap.** The replica already serves reads, and the copy's commit is after its
  head. `Store::adopt_generation` makes the staged generation a new generation of the
  running store, under the writer lock, the way a bulk commit switches generations. It
  appends the catalog records and annotations after the old head from the copy, writes
  `commit.json`, syncs, and switches `CURRENT`. Readers keep their snapshots of the old
  generation, which F06's collection removes or keeps. The dataset never goes out of
  service, and the head only moves forward.
* **Lineage.** The replica's commits after the fork are not the new primary's. Adopting
  a copy in place would let the head move backwards and reuse commit numbers with
  different data, so the dataset is replaced as an F05 in-place restore replaces it. New
  requests get `503` with `Retry-After` for the few seconds of the swap, and the old
  directory is kept as `.replaced-<ds>-<time>`.

Copied generations keep their files byte for byte, except two. `commit.json` and the
catalog records name generations, which get local numbers as §4.1 requires. A copy also
excludes what F05 excludes, so the replica builds its full-text index again (§4.5).

A capture needs the same index format on both servers, as a restore does, and fails with
`422 incompatible-format` otherwise. The commit stream does not depend on the index
format. A replica can therefore run a newer version than its primary, which allows a
rolling upgrade: upgrade the replicas, promote one, then upgrade the old primary and
rejoin it.

### 4.4 Generations and compaction

Replicas compact independently. The stream carries commits as terms, so it does not care
which generations either side has. A compaction on the primary sends nothing. A replica
runs C13's scheduler with its own policy, `compaction.json` and thresholds, which can
differ from the primary's, for example to compact more often on a replica that serves
heavy query load.

The cost of independence is CPU on each replica for its own compactions. The alternative,
copying each compacted generation from the primary, would send a full index over the
network for every compaction instead of the commits since the last one (§10).

History follows the same rule. A replica's retention window, pins and their generations
are its own, and `?at=` reads on a replica answer for the commits it holds. A replica that
was bootstrapped at commit 500 cannot read commit 400, even if the primary can.

### 4.5 Full-text, vector and spatial indexes

Replicated commits go through the same commit path as local writes, so the derived
indexes follow them as they follow local commits. The full-text index records the
commit it reflects, the spatial index takes each commit's delta, and the vector indexes
see each snapshot's delta and are rebuilt in the background after a compaction.

Their configuration follows the primary through the settings (§4.6). When the primary
turns a full-text index on, each replica builds its own. After a bootstrap or a copy, a
replica builds its full-text index from scratch, as a restored dataset does, and its
status is `stale` until it catches up. Queries behave as they do on any dataset whose
text index is stale. Phase 3 can ship the Tantivy segments in a capture.

### 4.6 Settings that follow the primary

Some of a dataset's state is not in its commits but changes what a query answers or what
a promoted replica must enforce. The settings endpoint returns these files, and the tag
is the SHA-256 of their canonical contents:

| File | Followed because |
|---|---|
| `prefixes.json` | Prefix changes make no commit, and they shape results and serializations. |
| `text.json`, `geo.json`, the vector configuration | They decide which indexes exist and how they analyse terms. |
| `queries.json` | Stored queries are run by name on any server. |
| `reasoning.json` and the RDFS-on-read setting | They change answers with `reasoning=true` and the `Sparkles-Inferences` header. |
| `validation.json` and its shapes or schema copy | A replica does not validate, but a promoted replica must enforce the same guard. |

These stay local: `compaction.json`, `quota.json`, `history.json`, the server's
authentication and the replication configuration itself. The replica applies a new
settings document by writing each file with `write_atomic` and reconfiguring what it
changed, such as building a full-text index. A replica refuses changes to followed
settings through its own API with `403 read-replica`. Open question 11 asks whether a
replica may keep indexes of its own.

### 4.7 Lineages

`dataset.json` gains `lineage`, the ids a dataset had before its promotions, each with
the commit where it ended:

```json
{ "format": 1, "id": "9b1e…", "origin": "promote",
  "forkedFrom": { "id": "3f1c…", "seq": 1003 },
  "lineage": [ { "id": "3f1c…", "end": 1003, "promoted": "2026-10-02T15:10:00.000Z" } ] }
```

A promotion appends the old id and copies any older entries, so the list is the chain of
promotions. The rule everywhere is the same. A pair (id, n) of an earlier lineage names
the same state as (current id, n) when `n ≤ end` of that entry, and names a lost state
otherwise. That rule decides `H prev` in a patch (§2.2), `minCommit` with a dataset id
(§2.5), and whether a replica can follow a promoted primary (§2.7).

PostgreSQL gives the same role to timeline ids and history files. Here the dataset id is
the timeline. A promotion could instead keep the id, but then the old primary, if it took
one more write, and the new one would both have a commit `n + 1` with different data under
one name, and a client holding (id, n + 1) could not tell. A new id makes that loss
detectable.

### 4.8 Access control

The commit stream, the captures and the settings expose every graph of a dataset, the
commit messages, the stored queries and the validation shapes. They need `read` on the
dataset with no graph limits, and a caller whose grants cover some graphs gets the `403`
of other whole-dataset routes. C12 gains the endpoint name `replication` for these routes,
so a token can be limited to replication and refused queries:

```toml
[[tokens]]
name = "replica-eu-1"
hash = "…"
[[tokens.grants]]
dataset = "wiki"
level = "read"
endpoints = ["replication"]
```

A grant without `endpoints` covers replication, as it covers every other endpoint. That
grants nothing new, since such a caller can already read the whole dataset through the
Graph Store. Captures and holds consume disk on the primary, and their limits (§2.3.2,
§2.3.3) bound what a reader can hold.

On a replica, the replica's own authentication applies to its readers. It does not
replicate users or tokens, so an operator must give a replica the same grants as the
primary if its readers should see the same data. `promote`, `demote`, `follow`, `pause`,
`resume` and `reseed` need `server-admin`.

## 5. Design

### 5.1 Engine (`crates/sparkles`)

* **`patch.rs`** gains `PatchReader`, a streaming reader of the text form, written from
  the row grammar of §2.2 with the N-Triples term parser of `oxttl`, and of the binary
  form, a Thrift compact-protocol decoder of `RDF_Patch_Row` that mirrors the encoder the
  module already has. The decoder bounds string and container lengths by the remaining
  body.
* **`store.rs`** gains `apply_patch`, which runs a `WriteTxn` of kind `patch` over the
  rows, and the `prev` precondition as a `guard::Precondition`. It also gains `Role`,
  checked at the start of every write path, and `Error::ReadReplica`, `Error::Demoted`
  and `Error::Diverged`.
* **`store/replication.rs`** is new. It holds `apply_replicated`, which reuses
  `publish_log` with the seq, timestamp, kind and flags given instead of computed, and a
  batch mode with one `sync_commit`. It also holds `adopt_generation`, which reuses the
  generation switch of `rebuild_locked`, and `promote`.
* **`commit.rs`.** `CommitKind::Patch`, `lineage` in `DatasetFile`, the origin `promote`,
  and a `reidentify` that also rewrites the header of `annotations.bin`.
* **`history.rs`.** `Hold::Replica(name)` and `Hold::Capture(id)`, with the follower
  records and their expiry in `replication.json`.
* **`store/backup.rs`.** The capture of F05 becomes usable without a repository, with a
  piece reader for HTTP.

### 5.2 Server (`crates/sparkles-server`)

* **`http/patch.rs`.** The patch endpoint and the dispatch by content type on `/{ds}`.
* **`replication/primary.rs`.** The routes of §2.3, the follower registry and the
  captures, with their limits and expiry.
* **`replication/follower.rs`.** One tokio task per replicated dataset that runs the
  loop of §2.4. HTTP goes through the server's existing `reqwest` client with the
  configured CA, a timeout of `wait + 30` seconds, and the bearer token read from its
  file at each request, so that rotation needs no restart.
* **`replication/config.rs`.** The flags, the TOML file and SIGHUP reloading.
* **`replication/admin.rs`.** Promote, demote, follow, pause, resume and reseed, and the
  CLI subcommands.
* **`http.rs`.** Read routes take `minCommit` and wait on `Store::subscribe_commits`.
  Responses of a replica dataset carry `Sparkles-Replication`, writes to it are refused,
  and CORS exposes the new headers.
* **`obs.rs`.** The metrics of §2.6, and `replication` in `ReadyInfo`.
* **`nix/module.nix`.** The options of §2.8, and a two-node VM test.

### 5.3 Files

| File | Where | Written by | Contents |
|---|---|---|---|
| `<db>/replication.json` | both roles | `write_atomic` | `format`, `role`, the primary on a replica, the followers and holds on a primary, and `promoting` while a promotion runs |
| `<db>/dataset.json` | both | `reidentify` | gains `lineage` |
| `<db>/.seed-<id>/` | replica | the follower | a copy being downloaded, removed at open |
| `databases/.replaced-<ds>-<time>/` | replica | a lineage re-seed | the replaced directory, kept for salvage |

Backups and clones leave `replication.json` out, like `compaction.json`. A restore or a
clone of a replica is a primary.

## 6. Failure modes and crash safety

| Failure | What happens |
|---|---|
| The primary is unreachable or answers `5xx` | The follower retries with a backoff from 1 to 30 seconds. The state is `disconnected` and reads go on at the last applied commit. |
| The primary crashes | It never sent an unacknowledged commit, because the stream reads only published commits and a commit is published after it is durable ([CI §4.1](CI-commit-identity.md)). After its restart the stream resumes. |
| The replica crashes while applying | Only fsynced commits were published. WAL replay at open truncates a torn tail, and the follower asks for the commits after the replayed head. |
| The replica crashes during a download | The staging directory is removed at open, and the next round starts a new capture. |
| The replica crashes during `adopt_generation` | The same as a crash during a bulk commit ([C13 §4.9](C13-automatic-compaction.md)). Before the `CURRENT` switch, the old generation is current and the new directory is removed at open. After it, the new generation is current, with its catalog records synced before the switch. |
| The replica crashes during a promotion | `replication.json` records `promoting` with the new id before `reidentify` starts. The open finishes the promotion, which is idempotent. |
| The replica crashes during a lineage swap | F05's recovery of an in-place restore applies. A `.replaced-X` with no `X` is renamed back. |
| A retried page repeats commits | Commits at or before the head are skipped. |
| A commit fails a check of §4.2 | The state becomes `diverged`, the error is logged and counted, nothing more is applied, and reads go on. An operator decides whether to `reseed`. |
| The next commit is no longer readable on the primary | `410`, then a copy (§4.3). |
| The primary's dataset was deleted and created again | A new id with no lineage entry for the replica's id: `409 lineage-mismatch` that the replica cannot follow. It stops as `diverged`. |
| The primary was restored in place under a new id | The restore records `forkedFrom`. If the replica's head is at or before it, the replica follows as after a promotion. Otherwise it stops. |
| The replica's disk or quota is full | `stalled`, retried every 30 seconds. Nothing is skipped. |
| The primary's clock jumps | Timestamps come from the primary and are already clamped there. `lagMs` uses only the replica's clock. |
| A capture expires during a slow download | The next piece is `404`, and the follower starts a new capture. |
| Two servers both believe they are the primary | Their ids differ after a promotion, so their commits never share a name, and replicas follow only the lineage they were configured for. Writes to both are lost on one side. Fencing is the operator's job (§2.7). |

## 7. Security

* **Transport.** The primary's URL must be `https`, unless it is a loopback address, a
  Unix socket or `allow_http` is set. `ca_file` adds a trust root for a private CA. The
  replica sends its token only to the configured primary's origin and never follows a
  redirect to another origin.
* **Credentials.** The token comes from a file, never from the TOML file or a flag value,
  is read at each request, and is never logged. A WARN is logged when the file is group-
  or world-readable, as for F05's credential files.
* **Untrusted input.** A replica trusts its primary for data, but not for format. Patch
  rows are parsed with bounded lengths. A capture's manifest is validated as an F05
  manifest is: paths are relative, inside the dataset, without `..`, and sizes are
  bounded by the free space. Every piece is checked against its digest and the result
  against `sparkles::check`.
* **Server-side request forgery.** The primary's URL comes from the configuration file or
  flags. `follow` over HTTP takes a URL, needs `server-admin`, and is checked against the
  outbound policy ([Usage: Outbound requests](../USAGE.md#outbound-requests-service-and-load)).
* **Exposure.** Replication exposes the whole dataset to its token (§4.8). The endpoint
  name `replication` keeps such a token from querying.
* **Resource use.** Long polls count against the rate limiter's concurrency like other
  requests. Captures and holds have counts, lifetimes and generation limits. A page has a
  byte limit.
* **Split brain.** A replica refuses writes in the engine as well as over HTTP, and keeps
  its role across restarts, so a configuration mistake cannot make it a second primary.

## 8. Phasing

**Phase 1, applying RDF Patch (about 2 days).** The text and binary readers, the patch
endpoint with Fuseki's methods, content types and dispatch, the `patch` commit kind,
`prev` as a precondition, `TA`, prefix rows, the blank-node rule, dry runs, the C12
endpoint name, `sparkles patch`, `sparkles tools rdfpatch`, `Dataset.apply_patch` in
Python, the API documentation, and tests P1–P15.

**Phase 2, read replicas (about 2 weeks).**

* Engine: `Role`, `apply_replicated` with its checks and batched fsync,
  `adopt_generation`, `lineage`, `promote`, holds.
* Primary: the commit stream with its headers, captures, settings, the follower registry
  and holds.
* Replica: the follower, bootstrap from a capture or a backup, copies for bulk commits,
  gaps and lineages, settings, write refusals.
* `minCommit`, the `Sparkles-Replication` header, the status, readiness and metrics.
* Promotion, demotion, `follow`, `pause`, `resume`, `reseed`, and following lineages.
* Flags, the TOML file, the NixOS options and VM test, the CLI, the UI's replica badge
  and its `minCommit` after updates, documentation, and tests R1–R18.

**Phase 3.**

* Continuous archiving. A primary uploads each new WAL segment to an F05 repository
  between backups, as Litestream does. A replica can then bootstrap and catch up from the
  repository without the primary, and a restore can reach any archived commit.
* Synchronous replication, as `synchronous_commit = remote_apply`: a commit waits until
  N named replicas have applied it.
* `minCommit=latest`: a replica asks the primary for its head and waits for it, which
  gives linearizable reads at the cost of one round trip, as Raft's read index does.
* Write forwarding from a replica to its primary.
* Automated failover through an external lease (Consul, etcd or a Kubernetes Lease), as
  LiteFS chooses its primary.
* Following every dataset of a server, including creation and deletion.
* Shipping the full-text index in captures, and indexes local to a replica.
* Streaming bulk commits from a logged change set instead of a copy.

## 9. Acceptance examples

Setup for Phase 1: `sparkles serve --data /tmp/s` with an empty persistent dataset `ds`
whose id is `D`.

**P1. Text patch.**
```
POST /ds/patch   Content-Type: application/rdf-patch
TX .
A <urn:a> <urn:p> "1" .
A <urn:b> <urn:p> <urn:c> <urn:g> .
TC .
```
→ `200`, `"inserted": 2, "deleted": 0, "rows": 4`, `Sparkles-Commit: 1`.
`GET /$/commits/ds/1` has `"kind": "patch"`.

**P2. No net change.** `A <urn:a> <urn:p> "1" .` and
`D <urn:x> <urn:p> <urn:y> .` → `200`, `"committed": false` with a receipt, head 1.

**P3. Row order.** `A <urn:z> <urn:p> "1" . D <urn:z> <urn:p> "1" .` → no commit, and
`<urn:z>` is absent.

**P4. `prev`.** A patch with `H prev <urn:uuid:D#commit:1> .` and one new `A` → commit 2.
The same patch again → `412 prev-mismatch` with `"head": 2`, and nothing changes. With
`H prev <urn:uuid:00000000-0000-4000-8000-000000000000#commit:1> .` → applied, and
`"prevChecked": false`.

**P5. Abort.** `TX . A <urn:q> <urn:p> "1" . TA .` → `200`, `"aborted": true,
"committed": false`, and `<urn:q>` is absent. A `TA` after a `TC` aborts the whole patch
too.

**P6. Prefixes.** `PA "ex" <http://example.org/> .` → `"prefixesSet": 1`,
`"committed": false`, and `GET /ds/prefixes?prefix=ex` returns the IRI.
`PD "ex" <urn:g> .` removes it.

**P7. Round trip between datasets.** For random histories of updates with blank nodes,
named graphs and triple terms on dataset `a`, `GET /a/diff?from=0&to=head&format=patch`
applied to an empty dataset `b` gives a dump of `b` isomorphic to `a`'s. The same with
`format=patch-binary`.

**P8. Blank nodes of the same lineage.** `INSERT DATA { _:x <urn:p> 1 }` makes commit 3
with node `_:b1f`, say. `GET /ds/diff?from=2&to=3&format=patch` is applied with each `A`
turned into `D` and with its `prev` set to commit 3 → the quad is deleted. The same rows
with a foreign `prev` delete nothing.

**P9. Binary from Jena.** A patch written by Jena's `RDFChangesWriterBinary` with adds,
deletes and prefixes applies with the same result as its text form.

**P10. Media types.** `Content-Type: text/turtle` → `415`. `charset=ISO-8859-1` → `415`.
No content type → read as text. `GET /ds/patch` → `405`. `OPTIONS` → `Allow:
OPTIONS,POST,PATCH`.

**P11. Syntax.** `A <urn:a> <urn:p> .` → `400 patch-syntax` naming line 1 and a column,
and nothing changes. A patch whose third row is malformed applies none of its rows.

**P12. Dispatch.** `POST /ds` with `Content-Type: application/rdf-patch` applies the
patch.

**P13. Access.** A `read` token → `403`. A `write` grant on graph `urn:g` only: a row in
`urn:h` → `403`, a `PA` row → `403`. A token with `endpoints = ["update"]` → `403` on
`/ds/patch`.

**P14. Validation and dry run.** With a SHACL guard that rejects the patch's result →
`422` and no commit. `dryRun=true` with a valid patch → the preview, and no commit.

**P15. CLI.** `sparkles patch --loc db p.rdfp` prints `inserted 2 · deleted 0 · commit 1`.
`sparkles tools rdfpatch p.rdfp` prints the rows and `# Data: Adds=2 Deletes=0` on
standard error.

Setup for Phase 2: server `P` on port 3030 is the primary of `ds`, and server `R` on port
3031 replicates it with `--replica-of http://127.0.0.1:3030 --replicate ds`. Both are
persistent.

**R1. Bootstrap.** `P` has 1,000 commits, compacted once. Start `R`. Within seconds
`GET R/$/ready/ds` is `200` with `"state": "streaming"`. `R`'s dump equals `P`'s, and
`GET /$/commits/ds?limit=50` on both is equal except for `generation`.
`Sparkles-Dataset-Id` is the same on both.

**R2. Read your write.** An update on `P` returns `Sparkles-Commit: 1001`. A query on `R`
with `minCommit=1001` returns the new data with `Sparkles-Commit: 1001` or later.

**R3. Waiting ends.** Pause `R`'s follower. A query with `minCommit=1002&minCommitWait=1`
after a write on `P` → `503 commit-not-visible` with `Retry-After` and `Sparkles-Head`.

**R4. Writes refused.** `POST R/ds/update`, a Graph Store `PUT`, `R/ds/patch` and
`POST R/$/reason/ds` → `403 read-replica`. A library `Store::write` on `R`'s store fails
with `Error::ReadReplica`.

**R5. Crash and resume.** While a client writes 5,000 single-quad commits to `P`,
`kill -9` `R` twice. After restarts, `R`'s commits equal `P`'s with no gap and no repeat,
and with digests on, every digest matches.

**R6. Independent compaction.** Compact `P` and `R` at different times, with different
policies. Dumps match at every head, and `at=commit:N` reads match for every N that both
hold.

**R7. Gap.** Stop `R`. On `P`, with `--replication-hold-ttl 0`, make commits and compact
twice. When `R` starts again, it gets `410`, copies a generation and adopts it. Readers running
queries in a loop against `R` throughout see no error and never a smaller
`Sparkles-Commit`.

**R8. Bulk commit.** Load 300,000 triples on `P` (a bulk commit). `R` copies a generation,
its head equals `P`'s, and its catalog record has `"bulk": true` with `P`'s counts. With
`bulk = "stream"` the commit is applied from its changes instead and the dumps match.

**R9. Bootstrap from a backup.** `P` backs up at commit 500 to an `fs` repository. A
fresh `R` with `bootstrap = { repository = "r", backup = "latest" }` restores it,
streams 501 to the head, and receives from `P` far fewer bytes than the index's size.

**R10. Divergence.** A test hook inserts a quad into `R`'s store behind the follower. The
next commit that touches it, or the next quad-count check, stops `R` in state
`diverged`, increments `sparkles_replication_errors_total{code="diverged"}`, makes
`/$/ready/ds` answer `503`, and reads keep working.

**R11. Promotion.** Stop `P` at head 1,105, with `R` at 1,103. `POST
R/$/replication/ds/promote` → a new id `D2`, `forkedFrom { id: D, seq: 1103 }` and
`lineage` with `{ id: D, end: 1103 }`. The next write on `R` is commit 1,104 under `D2`.
A second replica `S` at 1,100 that is pointed at `R` takes `D2` and streams on. `P`,
started again as a replica of `R`, stops as `diverged`, and `reseed` replaces it and keeps
`.replaced-ds-…`, from which `sparkles diff` exports commits 1,104 and 1,105 as a patch.

**R12. `minCommit` across a failover.** On `R` after R11, `minCommit=1100` with
`Sparkles-Dataset-Id: D` → served at once. `minCommit=1105` with `D` →
`409 commit-not-in-lineage`. Any id never seen → `409 dataset-id-mismatch`.

**R13. Access.** `R`'s token with `endpoints = ["replication"]` replicates and gets `403`
on `P/ds/sparql`. A graph-limited token → `403` on `/$/replication/ds/commits`. No token
→ `401`. Replacing the token file's contents is picked up by the next request.

**R14. Settings.** On `P`, add a prefix, turn the full-text index on and store a query.
After one round, `R` has the prefix, builds its index, and runs the stored query by name.
`PUT R/$/text/ds` → `403 read-replica`.

**R15. Cascade.** Server `T` follows `R`. Its dump and commits equal `P`'s.

**R16. Lag.** With `P` taking 200 commits per second and `R` paused for 10 seconds,
`lagCommits` reaches about 2,000 and `lagMs` about 10,000. After resuming, both return to
0, and `sparkles_replication_lag_commits` shows the same.

**R17. NixOS.** A two-node VM test configures `services.sparkles.replication`, writes on
the primary, and reads the write on the replica with `minCommit`.

**R18. Performance (bench, not CI).** On one machine with the 10.5M-triple dataset,
a bootstrap by capture takes under 10 seconds. A replica keeps up with the update-heavy
benchmark with a p99 `lagMs` under 1 second. Applying 50,000 single-quad commits takes
less time on the replica than making them took on the primary. The numbers go to
`docs/BENCHMARKS.md`.

## 10. Rejected alternatives

* **Physical replication of WAL bytes and generation files**, as PostgreSQL streams WAL.
  A Sparkles WAL holds generation-local ids, so a replica could apply it only to a
  byte-identical generation. Every compaction on the primary would then have to be sent
  to every replica as a full index, and both sides would have to run the same index
  format. The logical stream costs a re-intern of terms per change, and lets replicas
  compact on their own and run a newer version.
* **The public change feed as the replication protocol.** It has a row budget, refuses
  large commits with `507`, filters graphs by the caller's grants, and carries none of
  the metadata a replica must keep. A separate stream keeps the feed simple for its
  readers.
* **Shipping SPARQL Update text** (statement-based replication). `NOW()`, `RAND()`,
  `UUID()`, `STRUUID()`, `BNODE()`, `SERVICE` and `LOAD` give different results on each
  server. Net changes are deterministic.
* **Push from the primary.** The primary would need credentials for every replica,
  a queue and retries for each, and a way through the replica's firewall. Pull needs
  nothing on the primary beyond a read endpoint, and a replica resumes by naming its
  head.
* **Followers that read the primary's files**, as Oxigraph's `open_secondary` follows a
  RocksDB primary. That needs a shared file system, and Sparkles' collection of
  generations and its memory maps assume one process owns the directory.
* **Consensus (Raft) in Phases 1 and 2.** Raft acknowledges a commit after a majority has
  written it, so every write pays a network round trip and a remote fsync. It needs three
  or more voting servers, leader election with terms, log matching that can truncate a
  follower's log, and membership changes. In Sparkles, durability is a local WAL fsync
  under a single writer lock, and commit numbers are assigned by that writer. Putting
  Raft under it would rewrite the commit path, the WAL format and the recovery rules for
  a guarantee that most deployments of a read-heavy RDF store do not need. Manual
  promotion with lineages detects lost writes, and Phase 3's synchronous replication and
  external leases cover more of the need without a consensus log in the store.
* **Keeping the dataset id on promotion.** Two servers could then hold different data
  under one (id, seq) pair, which CI forbids and no client could detect (§4.7).
* **Replicating every compaction as a generation copy.** The network cost of a full index
  per compaction, against the CPU of a local compaction (§4.4).
* **Applying bulk commits from their changes by default.** The primary would compare two
  full states for each one, and the change set can be as large as the dataset. A copy of
  the generation is sequential reads on the primary. `bulk = "stream"` remains for links
  where bandwidth costs more than the primary's CPU.
* **A 307 redirect of writes from a replica to the primary.** A client would resend its
  credentials and body to another origin, which a browser and many HTTP clients do not do
  safely. The refusal names the primary when the operator allows it.
* **Jena's blank-node labels as identities.** Sparkles stores blank nodes as numbers, and
  keeping arbitrary labels would add a label table to every store. The rule of §2.2
  covers patches from Sparkles exactly and treats other labels as `INSERT DATA` does.
* **One commit per `TX … TC` block** as the default. Fuseki applies a patch in one
  transaction, and clients written against it expect that. It stays open question 3.

## 11. Open questions

1. **Spec ids.** This spec takes F10. Branches, encryption at rest and other storage specs
   are being written at the same time and may collide. A branch is a second lineage of
   one dataset, and whether a replica follows branches waits for that spec.
2. **Bulk commits.** `copy` is the default. Should `auto` choose by the change count
   against the dataset's size? Logging bulk change sets, F06's alternative B, would make
   streaming cheap and remove the question.
3. **One commit per transaction** with `?txn=each` for patches with several `TX` blocks,
   as RDF Delta logs them.
4. **Idempotent patches by `H id`.** Recording each patch's id with its commit would let a
   retried patch answer with its earlier commit instead of `412`. That needs an index of
   ids in the annotations.
5. **Readiness defaults.** Lag thresholds are off by default, so a disconnected replica
   stays ready. Should a default `max_lag_seconds` drain it after a few minutes?
6. **Hold lifetime.** One hour, sharing the generation limit with the retention window.
   Should holds have a byte limit of their own, as `max_slot_wal_keep_size` is?
7. **Automatic re-seed after a divergence.** Default: stop and wait for an operator,
   because it discards commits.
8. **Promotion permission.** `server-admin`, or `admin` on the dataset?
9. **`minCommit` on writes.** A primary could take it as a precondition of a write, so a
   client's writes stay ordered across sessions.
10. **Replicating users, tokens and grants** for a fleet of identical replicas, or leaving
    that to the configuration management that already writes the auth file.
11. **Indexes local to a replica.** A replica dedicated to vector search could keep its
    own vector indexes instead of following the primary's settings.
12. **In-memory replicas.** A replica must be persistent in Phase 2. An in-memory
    replica would bootstrap into memory at each start.
13. **Blank-node rule for patches.** A parameter `bnodes=stored|local` would let a client
    choose the rule of §2.2 instead of inferring it from `prev`.
14. **Graph terms on prefix rows.** Accepted and not narrowing, as in Jena. Should Sparkles
    refuse them instead, since it cannot honour them?

## 12. Sources

* **Sparkles repository, read for this spec:**
  * `crates/sparkles-core/src/patch.rs`, the RDF Patch writer and its Thrift encoding;
  * `crates/sparkles-core/src/store.rs`, the `WriteTxn` commit path (`commit_inner`,
    `publish_log`, the WAL record, `sync_commit`, the compaction tap), blank-node labels
    (`bnode_for`, `parse_bnode_label`), `StoreOptions::bulk_threshold`,
    `index_config_files`;
  * `crates/sparkles-core/src/store/wal.rs`, `store/changes.rs`, `store/backup.rs` by name;
  * `crates/sparkles-core/src/guard.rs`, `WriteOptions`;
  * `crates/sparkles-core/src/commit.rs`, the commit kinds, `reidentify`, `ForkedFrom`, the
    WAL flags;
  * `crates/sparkles-server/src/http/changes.rs`, the change feed;
  * `docs/API.md`, the change feed, blank nodes, permissions, graph-level access control,
    tokens, readiness and write previews;
  * `nix/module.nix`, the options of the backup configuration;
  * the specs [CI](CI-commit-identity.md), [F06](F06-snapshots-and-point-in-time.md) and
    [C13](C13-automatic-compaction.md) in full, [F05](F05-snapshot-repositories.md) §§1,
    2.1–2.3, 4, 5.1–5.5, 11, 12 and the Outcome, and the index and format of
    [PROVENANCE.md](PROVENANCE.md).
  * The project's private planning notes, for this item's place in the order of work
    only. `docs/COMPARISON.md` has a row that describes another product's deployment.
    This spec does not use it.
* **Apache Jena sources (Apache-2.0), read for behaviour.** No code was copied.
  * `jena-fuseki2/jena-fuseki-core`: `servlets/PatchApply.java` for the methods, the
    content types, the `415` answers, the single transaction and `TA`.
    `build/FusekiConfig.java` and `server/OperationRegistry.java` for the `patch`
    endpoint and the dispatch by content type. `servlets/ServletOps.java` for the `200`
    on success.
  * `jena-rdfpatch`: `text/RDFPatchReaderText.java` for the rows, `TB`, `<_:label>` and
    triple terms. `changes/PatchCodes.java` for the row codes and `Z`.
    `changes/RDFChangesApply.java`, which ignores headers and puts prefixes on the graph's
    mapping. `changes/RDFChangesExternalTxn.java`, where `TA` aborts.
    `RDFPatchConst.java` for `id`, `prev`, `.rdfp` and `.trp`.
  * `jena-cmds`: `rdfpatch/rdfpatch.java`, the `rdfpatch` command.
  * `jena-arq`: `sparql/core/GraphView.java`, where a graph's prefixes are the dataset's,
    and `riot/WebContent.java` for the media types.
* **Fetched:**
  * Apache Jena, "RDF Patch", https://jena.apache.org/documentation/rdf-patch/;
  * RDF Delta, https://afs.github.io/rdf-delta/, "RDF Patch Logs",
    https://afs.github.io/rdf-delta/rdf-patch-logs.html, and "Delta",
    https://afs.github.io/rdf-delta/delta.html;
  * PostgreSQL documentation, "Log-Shipping Standby Servers",
    https://www.postgresql.org/docs/current/warm-standby.html, and "Hot Standby",
    https://www.postgresql.org/docs/current/hot-standby.html;
  * LiteFS, "How LiteFS works", https://docs.fly.io/litefs/how-it-works;
  * Oxigraph 0.3.22 `Store` documentation,
    https://docs.rs/oxigraph/0.3.22/oxigraph/store/struct.Store.html.
* **Cited from general knowledge, not fetched:** Litestream's design (WAL shipping to
  object storage, restore from snapshot and WAL); D. Ongaro and J. Ousterhout, "In Search
  of an Understandable Consensus Algorithm" (USENIX ATC 2014) and the etcd documentation
  on cluster sizes; PostgreSQL's `pg_promote`, timelines and `pg_rewind`; RFC 9110,
  RFC 9530, RFC 9651 and RFC 6648.

## Outcome

**Phase 1 delivered on 2026-10-02** as applying RDF Patch, a Jena and Fuseki compatibility
feature. Phases 2 and 3 are deferred. The maintainer decided during Phase 1 that
replication waits until all other planned work is done, so the apply path was built for
patches from clients and was not shaped for replicas that keep a primary's commit ids.
The work landed in six commits after the spec. `44dc19e` added the reader, `e0366f4` the
engine's apply path and the commit kind, `8121894` the endpoint, `7d170ba` the commands,
`d7f135c` the Python method, and `496ca00` the documentation, the UI's commit kind
and the Jena client checks.

* **Reader.** `sparkles::patch` gained `PatchReader`, `PatchRow`, `PatchError` and
  `binary_for_path`, in `patch/read.rs` and `patch/thrift.rs`. The text reader has its own
  streaming tokenizer for the row grammar of Jena's `RDFPatchReaderText`. It reads IRIs,
  `_:label` and `<_:label>` blank nodes, literals in single, double and long quotes with
  language tags, base directions and datatypes, Turtle numbers, `true` and `false`, triple
  terms, comments and `TB`. It reports line and column. The binary reader decodes
  `RDF_Patch_Row` structs in the Thrift compact protocol, skips unknown fields by their
  wire type, reads strings in pieces so that a length the body cannot hold fails at its
  end, and bounds nesting. It accepts the value forms `valInteger`, `valDouble` and
  `valDecimal`. Prefixed names and variables are term errors. `PatchWriter` gained
  `header_term`, `prefix`, `row` and `flush`, so every row the reader returns writes back
  in either form.
* **Engine.** `Store::apply_patch` and `Dataset::apply_patch` are in
  `store/patch_apply.rs`, apart from the WAL code. The patch runs in one `WriteTxn` of the
  new `CommitKind::Patch`, code 12, so write-time validation, quotas, free-disk checks,
  graph-limited writes, protections of triples, dry runs, cancellation and deadlines apply
  as they do to any write. `Error::Patch` carries the kind (`Syntax`, `Term` or
  `PrevMismatch`), the row, the line and column or the byte offset, and for a mismatch the
  commit IRI, the expected commit and the head. `WriteTxn::lookup_key` finds a term
  without adding it, so a `D` row of unknown terms adds nothing to the vocabulary.
* **Server.** `http/patch.rs` serves `POST` and `PATCH /{ds}/patch` and the dispatch of a
  `POST /{ds}` by its content type. The endpoint name `patch` joined C12's list, the
  access log and metrics gained the operation `patch`, Fuseki's metric names gained the
  `patch` endpoint, dataset descriptions list the `patch` service at the dataset URL and
  `patch`, and the assembler accepts `fuseki:patch`. The route counts as an update for the
  rate limiter. CORS allows `PATCH`. The OpenAPI description has `patchPost` and `patch`
  and the `PatchResult` schema.
* **Commands and Python.** `sparkles patch` applies files to `--loc` or sends them to
  `--server`, one commit per file. `sparkles rdfpatch` prints rows and Jena's counts.
  `Dataset.apply_patch(data, binary=False, *, message=None)` returns a `PatchStats`.

**Deviations from the design.**

- The command is `sparkles rdfpatch`, since the file tools of G05 are flattened into the
  top-level commands. It also takes `--format` for the input and `--binary-out`.
- The CORS layer answers `OPTIONS` on every route, so `OPTIONS /{ds}/patch` lists the
  CORS methods, `PATCH` among them, and not Fuseki's `Allow`. The `405` answers to the
  other methods carry `Allow: OPTIONS,POST,PATCH`.
- `prev` in the headers at the start of the patch becomes the write's precondition, which
  is checked under the writer lock and reported by a dry run. A `prev` after the first
  other row is checked against the head when it is read. The blank-node rule is decided by
  the leading headers alone. A lineage is only the dataset's own id, because the lineages
  of §4.7 belong to Phase 2.
- Prefix rows are checked as `PUT /{ds}/prefixes` checks a prefix before the data
  commits, including whether the map would pass `--max-prefixes`, and are applied once it
  has committed. A new prefix that another request's change has left no room for in
  between is skipped with a warning instead of failing a committed patch.
- A patch of `A` rows alone, with at least `bulk_threshold` rows, takes the bulk path and
  rebuilds the index. Its `inserted` is then the commit's net count.
- `If-Match` is not read on patch requests. `prev` is the patch's own precondition.
- `PatchOutcome` also has `inserted` and `deleted`, which the response needs. The rows of
  `PatchRow::PrefixSet` and `PrefixRemove` drop a graph term, as the design's signature
  has them.
- Python maps syntax errors to `RdfSyntaxError`, term errors to `InvalidInputError` and a
  `prev` mismatch to `ConflictError`, and `apply_patch` takes `message`.

**Tests at landing.**

- `patch/tests_read.rs` ports the patches of Jena's `TestPatchIO_Text`,
  `AbstractTestPatchIO` and `testing/files/syntax-1.rdfp` (Apache-2.0), round-trips every
  row kind through the writer in both forms, and checks syntax and term errors with their
  positions and the binary decoder's bounds. `tests/patch/` holds a patch that Jena 6.2's
  text and binary writers wrote, with the program that wrote it, and both forms read to
  the same rows.
- `tests/patch.rs` covers P1 to P9, P11 and P14 on the engine: P7 applies the diff of a
  history with blank nodes, named graphs, triple terms and directional literals to an
  empty dataset in both forms and compares canonical dumps. It also covers the bulk path
  and crash safety. A patch commit replays with its kind and blank-node counter after a
  restart, and a torn tail of one is dropped, after which the next commit takes its
  number.
- `http/patch_tests.rs` covers P1, P2, P4, P5, P6, P9 to P12 and P14 over HTTP, with
  a SHACL guard that rejects a patch. `router_tests::auth::graphs` covers P13: a write
  outside the caller's graphs, prefix rows from a limited caller, a reader, a grant limited
  to `update`, and the dispatch on `/{ds}`. `tests/cli_patch.rs` covers P15, the binary
  form, several files, messages, errors, and `--server`. The Python suite applies text and
  binary patches.
- `mise run test:jena-clients` writes a patch with Jena's `RDFChangesCollector`, sends it
  through Jena's text and binary writers to `/{ds}/patch` and `/{ds}`, and compares the
  datasets with the one `RDFPatchOps.applyChange` makes. All 144 checks pass.
- Crate and server tests, Clippy over all targets, `mise run lint:features`, the Python
  and UI suites, the W3C suites (SPARQL 1.0 482/482, 1.1 query 328/328, 1.1 update
  157/157, 1.2 269/269) and `mise run ci` pass. Two tests unrelated to patches timed out
  once on a machine with a load average above 60 and passed when run again.

**Not built.** Phases 2 and 3 were not built. They cover roles, replicas, the commit
stream, captures, holds, `minCommit`, promotion and lineages, archiving, synchronous
replication and failover.
Open questions 3 (a commit per transaction), 4 (idempotent patches by `H id`), 13 (a
parameter for the blank-node rule) and 14 (refusing graph terms on prefix rows) keep the
designed defaults.
