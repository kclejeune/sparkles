# X16: Storage consolidation

> **Status:** in progress. The plan below is ordered and audited against the code, and
> phase 2 has shipped.
>
> **Roadmap:** [item 455](../../ROADMAP.md) holds this work. Items 479 and 830 to 833
> hold the phases that are ready to start, and each is named in its phase. Item 484 is
> complete. The later candidate phases get their numbers when their prerequisites land.

The design covers a first-release format cutoff, budgeted readers through one Control
trait, a shared commit publish tail, one load plan, partitioned vocabularies with
per-range generations, and the remaining replica, clone and format-policy work.

## Problem and goal

The workspace has not had a release, yet the store reads every pre-release form of its
files. The registry in `crates/sparkles-core/src/formats.rs` records the following
readable windows. The generation index reads formats 2, 3 and 4 and writes 3 or
4, where format 2 is the unsegmented layout and format 3 is format 4 without a `features`
list. The write-ahead log reads versions 0, 1 and 2. Version 0 is headerless and carries
the `walStrictFrom` and `Framing::Mixed` machinery that tells a legacy commit from a torn
one, and versions 1 and 2 are both current, since a store without configuration history
writes version 1 and an activated store writes version 2. `delta.vocab` reads versions 1
and 2, and the one registered `migrate::STEPS` entry rewrites version 1. `dataset.json`
admits readers 1 to 8. `commit.json` and `link.json` read formats 1 and 2, and both
formats are current, since format 2 of `commit.json` belongs to activated stores and
format 2 of `link.json` to branches with a base overlay. Backup key slots read formats 1
and 2. The sparse vocabulary index `vocab.idx` is written by every build but remains
optional on read, and `add_vocab_index` writes it for a generation an older build left
without one. About 124 lines in the WAL, replay, history, backup and commit modules
branch on legacy framing alone.

About 114 readers exist as twins of an ordinary reader in `_limited`, `_controlled`,
`_checked`, `_prepared` and read-only forms, so every reader fix is made more than once.
They split into 42 in the history and changelog stacks, of which `history_limited.rs`,
`changelog/readonly.rs` and `branching/readonly.rs` hold 3,753 lines of mirrors, 37 in
the vocabulary scanners, 13 in the cold open and commit readers, 8 in the index readers,
6 in WAL replay and 8 elsewhere. Four carriers express the budget, `ReadLimit`,
`ColdBudget`, `Preparation` and ad hoc reserve closures. `ReadLimit` checks cancellation
and the deadline on every call and its memory cap only when one is set, so an unlimited
read still pays the runtime test of `memory`. The 92 `|| Ok(())` closures that stand for
no control are generic and do compile away where the callee is generic, but each one is
a second call site to keep in step with the controlled one. Two readers,
`snapshot_at_inner` and `WalCursor::next_inner`, fork at run time on whether a limit is
present, the first in fifteen places. The only form that removes the control at
monomorphization is the `Meter` trait of `store/keyset.rs`, whose `()` implementation
compiles to nothing.

Three publish paths and a compaction writer encode WAL records and build the snapshot.
The serial path `WriteTxn::publish_log`, the group path `publish_grouped`, `write_prefix`
and `publish_group_prefix`, the bulk path `rebuild_locked` and the compaction writer
`CatchUp::apply` with `compact_inner` each carry the work. The record encoder loop is
copied three times, in `store.rs`, `group.rs` and `compaction.rs`, and the 17-field
`Snapshot` is written as a full literal in 12 places and as a struct update in 7 more.
The copies have drifted. The serial path tells the write guard about a commit after it
publishes the snapshot and the group path before, and only the serial path forgets
annotations and remembers past heads, which the group path can omit only because it never
serves an in-memory store.

Four rules decide bulk loading, all keyed on `StoreOptions::bulk_threshold` of 250,000
quads. `load_with` builds when the write is not restricted to some graphs, the sources
are not small readers, and the store is empty or the estimate exceeds the threshold.
`replace_with` builds when the write is unrestricted and the estimate or the stream
length exceeds the threshold, with no empty-store case. `WriteTxn::insert_bulk` stages a
rebuild when the batch reaches the threshold and the transaction has neither triple-level
write protections to check nor a managed admission, and `patch_apply` repeats that rule
for a patch of additions with no deletion. SPARQL `LOAD` never builds. A compaction
rewrites only the touched blocks when the delta adds no term and copies the rest, but it
still writes a whole generation, and a delta with new terms forces a full build.

The goal is one implementation per reader, one commit publish path, one load plan and a
storage layout whose maintenance cost follows the size of a change.

## Scope

The format cutoff comes first, because it removes the legacy readers that the Control
trait would otherwise have to unify. The budgeted readers start with the history, WAL and
changelog stacks and then move to the vocabulary scanners under the instruction-count
screen of item 473. The shared publish tail has two halves. The short-term half, item
484, extracts the WAL encoder, a snapshot builder and the shared tail now. The long-term
half, item 833, makes a group of one the ordinary commit, since items 446 and 447 have
landed and the leader already writes through the direct handles. Partitioned
vocabularies and per-range generations follow item 14's per-block located deltas.

The tiered segments of [X17](../X17-tiered-segments/README.md) and the fragment
composition of [X18](../X18-fragment-composition/README.md) build on this layout.

## Plan

The phases below are in dependency order. Each names its roadmap item, the files it
changes, its format decisions, its gates and whether it is routine or needs engineering
judgment in the sense of the agent guidance. The [parallelism](#parallelism) section says
which phases run at the same time.

### Phase 1. First-release format cutoff (item 479)

The cutoff removes every reader of a form that no current writer produces, and keeps
every form that a current writer still produces, with two exceptions that mark the cutoff
itself, the dataset file's `format` and the index format. It follows item 343, as the roadmap
orders, unless the maintainer takes [open decision 2](#open-decisions).

The table gives each kind's decision. Every refusal goes through `FileKind::refuse`, which
already names `sparkles dump` from the release that wrote the file and `sparkles load`.
The forms that stay are the ones that `wal::version_for`, `gen_commit_bytes_for` and the
branch creation of `branching.rs` choose between today, so the cutoff changes no log,
commit or link writer. It changes two writers, both to mark the cutoff itself. The
dataset file gains `format` 2 and the index gains format 5.

| Kind | Today | After the cutoff | How |
|---|---|---|---|
| `index` | It reads 2, 3 and 4, and writes 3 without features and 4 with them. | It reads and writes 5. Format 5 is format 4 with the `features` list always present and `vocab.idx` required. | Drop the format 2 unsegmented layout from `PermIndex::open_format`, `index.rs` and `cold.rs`, and `PLAIN_FORMAT_VERSION` from `builder.rs`. Add format 5 to `READABLE_INDEX_FORMATS` alone and move `builder::FORMAT_VERSION` to it, subject to open decision 3. |
| `vocab-index` | Every build writes `vocab.idx`, but a reader opens a generation without one, and `add_vocab_index` writes it for a generation an older build left bare. | Index format 5 requires it, and a format 5 generation without one is corrupt. | Make `Sparse` a required field of the vocabulary, delete `add_vocab_index`, and add a refusal test for a format 4 generation and a corruption test for a format 5 generation without the file. |
| `wal` | It reads 0, 1 and 2. A plain store writes 1 and an activated store writes 2. | It reads 1 and 2 and writes them as today. | Delete `Framing::Mixed`, `fold_legacy`, `walStrictFrom` in `commit.json`, the headerless branches of `WalCursor`, `WalIndex::scan`, `apply_backward`, `probe.rs`, `history_limited.rs`, `backup.rs` and `diff.rs`. |
| `delta-vocab` | It reads 1 and 2, with a `migrate` step from 1. | It reads and writes 2. | Delete `convert_legacy_file`, the step and the legacy parser of `vocab/delta.rs`. |
| `dataset` | It reads `format` 1 with readers 1 to 8. A new plain store writes reader 5 and an activated store reader 8. | It reads and writes `format` 2 with readers 5 to 8, and `minimumReader` keeps its meaning. | Write `format` 2 from both dataset writers, `dataset_file_bytes` and `clone_dataset_file_bytes`, which `Store::clone_to` uses, refuse `format` 1 with the dump-and-reload message in `check_dataset_value`, and delete the branches that handle readers below 5, which are a dataset without branches, relinked generations, framed deltas or log headers. The branches for `managed.json` and `holds.json` stay, since a plain store still lacks both. |
| `generation-commit` and `link` | They read 1 and 2. A plain store writes `commit.json` 1 and an activated store 2, and a branch writes `link.json` 1 without an overlay and 2 with one. | They stay as they are. | Both forms are current, so nothing is removed. |
| `backup-slots` | It reads 1 and 2 and writes 2. | It reads and writes 2. | Pre-release repositories follow the same window as databases. |
| Derived kinds | `view-state` and `internal-circuit-state` read several versions. | They stay as they are. | A derived file never needs a cutoff, since an unread version is rebuilt. |

The cutoff marker is therefore `format` 2 of `dataset.json`, which every store of the
first release writes, and not `minimumReader`, because a plain store writes reader 5 and
`managed_history::activation_recorded` reads a reader of 8 or more as a recorded
activation. The reader stays what it is, a capability requirement that activation and
the managed and held files raise. The private import transport of
`store/backup/transport.rs`, which no production path wires and whose staged destination
has no store constructor, writes `minimumReader` 9 for an imported destination, and its
test asserts that a reader-8 build refuses those bytes. That is a compatibility decision
for the lane that wires the import, which then raises `DATASET_READER` and the registry
together, and the cutoff does not change it. The acceptance cases of the phase are a
plain store created, written, reopened, branched, cloned and compacted, with the clone
reopened and carrying `format` 2 with its own reader requirement, an activated store doing
the same apart from the clone, since `clone_to` refuses an activated source until item
764 changes that, and a backup and restore of each, all writing `format` 2 and index format 5
and reading only the retained forms, together with the refusal of a `format` 1 dataset,
a format 4 generation and a headerless log.

A fixture labelled with the cutoff commit is written with `scripts/format-fixture.sh` and
committed under `testsuite/formats/`. The two pre-release fixtures `8dd019da` and
`c7a6811c` stay, and `tests/formats.rs` and `cli_formats.rs` check their refusal messages
instead of opening them, as X12 §4.6 says for a fixture outside the window. Each removed
form also gets a hand-written refusal test from a minimal file, so the refusal does not
depend on a 1 MB tarball.

The gates are the format fixtures, `scripts/power-loss.sh` and `tests/power_loss.rs`
unchanged, since the cutoff changes replay of a log with a header in no way, the
instruction-count screen `mise run bench:insn` for WAL replay, and the X14 forge gate for
query paths. The deletion of `Framing::Mixed` from the replay and the probe is
judgment-sensitive durability work. The rest of the phase is routine deletion once the
table above is agreed, and a cheaper worker can take it with the fixture tests as its
check.

### Phase 2. Shared commit publish tail, short-term half (item 484)

This phase extracts three pieces without changing what any path makes durable or in which
order.

`wal::encode_txn` replaces the three copies of the record encoder loop at `store.rs`
7133, `group.rs` 832 and `compaction.rs` 684. It takes the operations, the configuration
record, the next blank node and the seal parameters, and returns the records of one
transaction. The compaction writer passes translated ids through the same function.
`build_relinked` keeps its overlay writer, which writes no commit record.

`SnapshotBuilder` replaces the 12 full `Snapshot` literals. It starts from the previous
snapshot or from an opened generation and sets only what the path changes, so a new field
is added once, with a default, instead of in a dozen places. The 7 struct-update literals
stay, since they already copy from a base.

`Store::publish_committed` is the shared tail that the serial path, the group leader and
the bulk rebuild call after their durable write returns, and it takes two inputs. A
logical commit, which the serial, group and bulk paths publish, performs the WAL index
and quota update, the changelog push, the catalog append, `deltas.noted`,
`note_configuration`, `current.store`, `commits.send_replace` and `embed_noted`. A
generation switch, which the compaction switch publishes, republishes the commit the
store already has after `switch_generation`, so it appends no catalog entry, pushes no
changelog commit and announces no delta, and it calls `guard.rekey` between
`current.store` and `send_replace` as `compact_inner` does today. The compaction tap
stays where it is, since the group path pushes it under the writer lock before it
enqueues and the serial path under the same lock, and a tail that runs after the fence
would change that contract. The serial and group paths disagree on whether the write
guard learns of a commit before or after the snapshot is stored. The shared tail adopts
the group's order and notifies the guard before `current.store`, because a writer that
waits for the publication of commit N and then validates must check against the state
that N left, which the group path guarantees and the serial path does not. The
regression case is a serial writer that waits on `commits` for N and validates at once,
which must see the guard's state of N. The in-memory-only steps `forget_annotations` and
`remember_past` become conditional on the store having no log, so the group path keeps
omitting them for the right reason.

The durability argument is that this phase moves no write. Each path still performs its
synchronization as section 17.4 of [X09](../X09-group-commit-by-default.md) and the
module documentation of `wal.rs` describe. In the serial and group buffered paths the
terms are flushed before the records and both are synchronized together by
`sync_commit_reused`. The compaction catch-up flushes its records before its terms, and
its safety comes from synchronizing the whole candidate before `CURRENT` switches to it,
which the shared tail does not touch. In the direct path the records and the terms go
through the `O_DIRECT | O_DSYNC` handles in parallel from `commit_direct`, and the commit
record's seal, its check of the delta term count and its `unfenced` count let the replay
tell a complete transaction from a torn one, so the order between the two writes does
not matter. Annotations are written before the records in the serial path, whose records
carry no `WAL_FLAG_ANNOTATED`, and in parallel with them in the group path, whose records
carry the flag so that replay treats a flagged tail commit without its annotation as torn.
`wal::encode_txn` takes the flags from its caller and changes neither contract. The
shared tail runs only after the write that makes the commit durable has returned, so no
snapshot is published for a commit that a crash could lose, and `send_replace` follows
`current.store` so a waiter never sees a commit number whose snapshot is not yet current.

The gates are `scripts/nsc-writes.sh` with and without `--no-group-commit`,
`bench-indexed-commit.py`, `power-loss.sh`, `tests/power_loss.rs`, `group_runs.rs` and
`bench:longwrite`, with no commit path slower than the screen's 10% rule. This is
durability-sensitive work and its planner implements it. The encoder extraction is
routine once the signature is fixed.

### Phase 3. Budgeted readers through a zero-cost Control trait (item 830)

One trait replaces the four carriers.

```rust
pub(crate) trait Control {
    const CHECKS: bool;
    fn check(&mut self, bytes: u64) -> Result<()>;
    fn reserve(&mut self, bytes: u64) -> Result<()>;
    fn release(&mut self, bytes: u64);
}
```

`Unlimited` implements it with `CHECKS` false and empty inlined methods, so a reader
generic over `C: Control` compiles to the ordinary reader when called with `Unlimited`,
as `Meter for ()` does in `keyset.rs`. `ReadLimit`, `ColdBudget`, `Preparation` and
`WriteOptions` implement it, and the 92 `|| Ok(())` closures disappear. `Unlimited`
replaces only the call sites that pass no control at all today. A read whose `ReadLimit`
has no memory cap still checks cancellation and the deadline, so it keeps a `ReadLimit`
whose memory member is absent, and the regression tests of each stage include a
cancelled and a timed-out read with no memory cap. A `Stride<N>` helper replaces the
hard-coded 16, 1,024, 4,096, 65,536 and 64 KiB intervals with one place per reader that
says how often it checks, and the stride is a const generic so that `Unlimited` removes
the counter too.

The stacks move in three stages, each a lane of its own with the same gate.

1. History, WAL replay and changelog. `history_limited.rs`, `changelog/readonly.rs`
   and the `_limited` twins of `wal.rs` fold into their ordinary readers. The fifteen
   `memory.is_some()` forks of `snapshot_at_inner` and the `Option` of
   `WalCursor::next_inner` become the trait's constant. The read-only twins of
   `branching/readonly.rs` keep their read-only open, which is a different behaviour,
   but share the reader bodies.
2. Vocabulary scanners. The `_checked`, `_controlled` and `_fallible` forms of
   `vocab.rs`, `vocab/delta.rs`, `vocab/fsst.rs`, `vocab/cold.rs` and `vocab/numeric.rs`
   fold into the const-generic readers that already exist there. This stage waits for
   item 409, which settles the cost of controlled preparation, and runs under
   `mise run bench:insn`.
3. Cold open, commit, index and the remaining readers in `cold.rs`, `cold/linked.rs`,
   `commit.rs`, `index.rs`, `managed.rs`, `preview.rs`, `lifecycle.rs` and `backup.rs`.

The gate of every stage is that `bench:insn` shows no increase in instructions for
vocabulary scans and WAL replay, the X14 forge gate holds for query paths, and the
controlled tests that exist for each twin still pass against the unified reader with the
same budgets. Binary size and the compile time of `sparkles-core` are recorded before and
after each stage, since monomorphization can grow both, and a growth above 5% gets a
follow-up item. The trait and the first stage need judgment, since they decide the
signature every later reader takes. Stages 2 and 3 are routine once the first has landed,
and a cheaper worker can take them one file at a time with the controlled tests as the
check.

### Phase 4. One load plan (item 831)

`LoadPlan::choose` in `store.rs` takes the inputs the four rules read today and returns
`Bulk` or `Transactional`. The inputs are the head's emptiness, the size as a quad
estimate from `estimated_quads`, a batch length or a stream probe against a byte bound,
whether every source is a small reader, whether the write is restricted to some graphs
through `WriteOptions::graphs`, whether the transaction checks triple-level write
protections, which `requested` records, whether it carries a managed admission, whether
the patch has met a delete row, and `bulk_threshold`. The table below is the current
behaviour, which the first commit of the phase encodes as a table test before any rule
changes, and the second commit unifies the rules by the decisions it records.

| Entry point | Bulk when | Threshold test | Cases it lacks |
|---|---|---|---|
| `load_with` | The write is unrestricted, the sources are not small streams, and the head is empty or the estimate is above the threshold. Small streams means an empty head, at least one source, every source a reader, and the readers holding at most `SMALL_STREAM_BYTES`, 8 MiB, of decompressed bytes in total when read ahead. | The estimate is strictly above. | It has no probe of large streams against the threshold. |
| `replace_with` | The write is unrestricted and the estimate is above the threshold or the streams exceed eighty bytes per threshold quad. | The estimate is strictly above. | It has no empty-head case. |
| `insert_bulk` | The batch reaches the threshold, and the transaction checks no triple protections and carries no managed admission. | The length is at or above. | It has no size estimate and no small-reader case. |
| `patch_apply` | The patch has met no delete row, whether or not the row deleted anything, and its additions reach the threshold, after which `insert_bulk` applies its own rule. | The length is at or above. | It has no estimate and no restriction input of its own. |

The unified rule takes the strict test everywhere, gives `replace_with` the empty-head
case and the small-stream exception of `load_with`, gives `load_with` the large-stream
probe of `replace_with`, and treats a graph restriction, a triple protection, a managed
admission and a delete row as reasons for the transactional path in every entry point. The
table test includes a patch whose delete row removes nothing and still takes the
transactional path. The group admission's
`Fallback::Bulk` reads the same plan. SPARQL `LOAD` keeps inserting per quad unless the
[open decisions](#open-decisions) say otherwise.

The gate is the load benchmark of [docs/benchmarks](../../benchmarks/README.md) at 1.05M
and 10.5M with no load slower, and the table test, which records the plan each entry
point chooses for every row and boundary above, so a later change to the rule is visible
in one place. The phase is routine, with the table as its specification.

### Phase 5. Smaller storage duplicates (item 832)

* The zig-zag delta varint encoder of `index.rs` `encode_column` and `builder/runs.rs`
  `encode_block`, with their matching decoders, share one delta-coding step. The
  compression framing stays separate, since the index writes LZ4 frames with a prepended
  size and the builder's runs go through `builder::codec`. This is codegen-sensitive and
  needs an Intel measurement of the builder and of block decoding before and after.
* `Delta::predicates` drops the last `imbl::OrdMap`, and the `imbl::Vector` uses of the
  spatial index follow if the measurement of `geo/index.rs` allows, after which the
  dependency leaves `Cargo.toml`.
* Test scaffolding leaves production code. The ungated `#[doc(hidden)]` hooks
  `set_clock`, `wal_direct_active`, `vocab_direct_active` and the vector pause hooks
  move behind `cfg(test)` or the `failpoints` feature. The `ordinary_sync_probe` points
  inside `publish_log` are already behind `cfg(test)` and observe three distinct moments,
  the choice of the direct or buffered branch, the capture after synchronization and the
  observation after the writer is released, so they stay at those boundaries and only
  their scaffolding is shared.
* `parse_source` and `parse_source_into` in `io.rs` share one ladder, since the second
  is a prefix of the first.
* History reconstruction waits for items 23 and 52 and stays out of this phase.

Each item is routine and small, and the phase lands as separate commits. Its gate is
`scripts/ci.sh --fast --changed` plus the measurements the first two bullets name.

### Phase 6. A group of one is the ordinary commit (item 833)

The long-term half of the publish tail removes the serial path. Every commit encodes
through `wal::encode_txn`, enqueues, and is written and published by the leader of its
fence, which is itself when the queue is otherwise idle. The leader already writes the
prefix's records and terms through the write-through handles when they fit, as item 446
built, and falls back to buffered writes with `fdatasync` otherwise.

The durability contract of the result is the group's, not the serial path's, and the
phase says so rather than claiming equivalence. The fence synchronizes the same three
files, the log, `delta.vocab` and `annotations.bin`, that the serial path synchronizes,
but it writes the annotation in parallel with the records instead of before them, and its
records carry `WAL_FLAG_ANNOTATED` so that replay treats a flagged tail commit whose
annotation is missing as torn, as `replay_wal` does today for grouped commits. A commit
that today takes the serial path, with its annotation durable first and no flag, would
after this phase recover under the flag rule instead. That rule is already the one
`crash_images_keep_every_acknowledged_commit` and `power-loss.sh` prove for grouped
writers, and the phase adds the direct and buffered cases of a group of one with a
missing annotation to those tests before it removes the serial path. The `unfenced` count
of a group of one is the speculative head less the published head, which is zero only
when no earlier commit is still unpublished, so the phase keeps the count as the group
computes it and does not assume zero. Publication stays in commit order because only the
leader publishes and the next fence publishes after the last one has. The in-memory
store, which has no log, keeps a short path that builds the snapshot and calls
`publish_committed`.

The latency hypothesis is that serial cost falls, because the idle-path admission that
item 448 measured at about 1,000 user instructions goes and the leader keeps
`commit_direct`. It is a hypothesis until the writes benchmark measures it, and a
measured slowdown that the screen accepts gets a follow-up item in the same change.

The gates are those of Phase 2 with the writes benchmark of
[docs/benchmarks/writes](../../benchmarks/writes/README.md) on a benchmark worker before
the phase reports, since its goal is a single path at no cost in serial latency. Items
446, 447, 448 and Phase 2 land first. This is concurrency and WAL-ordering work, and its
planner implements it.

### Phase 7. Partitioned vocabulary and per-range generations

This phase waits for item 14's per-block located deltas and new terms without a full
build, and for items 160 and 343. Its design has two parts, both of which change on-disk
formats under the rules of X11 §7 and X12 §4.3.

The vocabulary becomes partitioned. Each partition holds a sorted key range with its own
sparse index, FSST table and numeric column, and a partition table in `meta.json` lists
the ranges. A compaction that adds terms rewrites only the partitions whose range the new
terms fall in, and the spaced ids of the C13 sketch keep every untouched index block
valid. A lookup from id to key uses the per-partition id array the sketch names.

The generations become leveled. Partial compaction in `store/partial.rs` already rewrites
only the touched blocks when the delta adds no term, but it copies every untouched block
into a new generation and falls back to a full build as soon as the delta names a new
term. The leveled layout lets a compaction leave untouched ranges where they are.
`meta.json` lists the segments of each permutation with the generation that wrote them,
so a reader opens a permutation as a sequence of segments from several builds, and the
partitioned vocabulary removes the full-build fallback for new terms. This is the layout
X17 uploads.

Both are kind 3 changes, read and rewritten at compaction, so `READABLE_INDEX_FORMATS`
gains format 6 and `builder::FORMAT_VERSION` moves to it, and the first release's format
5 stays readable for one window. The phase begins with a measurement item, since the C13
sketch asks for the lookup cost of spaced ids before any decision, and the design is
settled in its own spec section before implementation. Its gates are the X14 forge gate,
the two-hop-count query at 5,000 changes falling from 8.6 ms toward its compacted 5.1 ms,
the write cost of the located deltas within the 5% that item 14 allows, and the format
fixtures. The whole phase needs judgment.

### Phase 8. Bounded-memory index build

`BuildOptions::sort_mem_quads` already bounds the quads sorted in memory, store callers
set it through `StoreOptions::build`, and runs beyond the budget are merged into the
permutations by `merge_runs`. The peak of a bulk load is nevertheless 2.9 times QLever's
on the LUBM closure, so some allocations escape the bound. The phase begins by measuring
which ones, with the vocabulary batches, the hot-key table, the run merge buffers and the
decoded columns of the catch-up as the candidates, and then bounds the ones that matter
under one build budget that the caller sets. Its acceptance criterion is a measured peak
RSS within a stated factor of that budget at 1.05M and 10.5M, not a recorded RSS alone.
It waits for item 40 and is independent of the other phases. It is judgment work in the
builder, with routine option plumbing.

### Parallelism

Phases 1, 2, 4 and 5 are independent and can run as four lanes at once. Phase 1 deletes
reader code in `wal.rs`, `history_limited.rs`, `index.rs` and `vocab/delta.rs`, Phase 2
changes the writer side of `store.rs`, `group.rs` and `compaction.rs`, Phase 4 changes
the loader entry points of `store.rs`, and Phase 5 touches `index.rs`, `builder/runs.rs`
and `io.rs`. Phases 1 and 2 both edit `store.rs` and `compaction.rs`, so the first to
land merges forward and the second rebases its tests. Phase 3 follows Phase 1, since the
cutoff removes the legacy branches that its readers would otherwise carry. Phase 6
follows Phase 2 and item 448. Phase 7 follows item 14 and Phase 1. Phase 8 follows item
40 and nothing in this spec.

### Routine and judgment

Routine work, which a cheaper worker takes with the named tests as its check, is the
deletion of the removed forms in Phase 1 after the table is agreed, the encoder
extraction in Phase 2, stages 2 and 3 of Phase 3, Phase 4 and Phase 5 apart from the
encoder measurement. Judgment work, which its planner implements, is the replay and
probe changes of Phase 1, the shared tail and guard order of Phase 2, the trait and first
stage of Phase 3, Phase 6 and Phase 7.

## Candidate phases

These phases keep their earlier descriptions and get a number when their prerequisites
land.

**Write forwarding and linearizable replica reads.** Add `minCommit=latest`, write
forwarding from a replica with the caller's identity over mutual TLS, and `minCommit` as
a write precondition. F10 Phase 3 names them, while items 66 and 280 do not. It depends
on items 63, 66 and 280.

**Clone fast path for sources with a delta.** Share base index files and carry the
source's delta so a busy dataset clones in seconds, and record reflink timings. Today
`clone.rs` shares files only when the delta is empty and the generation is not linked,
and any commit since compaction forces a full rebuild of 38.5 s and 1.44 GB at 10.5M.

**Remaining format-policy work.** Add the release fixture job, background rebuild of
vector and spatial indexes with unknown file versions, and a WAL header migration plan.
Only full-text honours the rule that derived files never need migration. It depends on
items 24, 34 and 59.

**Replica scope.** Follow every dataset of a primary, keep replica-local text and vector
indexes, bootstrap in-memory replicas and stream bulk commits. It depends on items 52, 66
and 280.

**Read-only open of a held data directory.** Let a second process open a store
read-only without the exclusive lock, for checks, inspection and embedded readers. Every
local command fails on the lock while a server runs.

**Sharding, distributed query and distributed reasoning.** Hash-partition permutations
and the vocabulary, run exchange-based distributed joins through a coordinator, and add
distributed semi-naive evaluation. Sparkles runs as a single node. Reasoning above 10B
triples and data beyond one host need it. It depends on items 66 and 280. This phase is
deferred until the condition under Design constraints holds.

**Prebuild remaining derived structures before the switch.** Prebuild the ShEx typing
cache and full-text checkpoint between build and catch-up. C13 names the hook, and
spatial and vector already use it. It depends on item 14.

**Direct tabular-to-loader streaming.** Feed the CSV converter straight into the loader
instead of spooling and reparsing N-Triples, and speed the CONSTRUCT-template path. C05
deferred this until measured, and the measurement shows a quarter of load time and five
times the table size on disk.

**Multi-region replicas and region-aware routing.** Run cross-region async replicas with
lag objectives, region-aware read routing and WAN write forwarding. F10 has no region
concept. Phase 3 write forwarding is the nearest piece. It depends on item 66.

## Open decisions

These go to the maintainer. The plan recommends an answer for each. Where a decision
would change the roadmap's order or scope, the roadmap's current order stands until the
maintainer takes it.

1. **Dump and reload for every pre-release form.** The alternative is one `migrate`
   step that compacts a pre-release generation into the first-release format, as X12
   §4.4 allows for the index. The recommendation is dump and reload, because there is no
   release and no database outside the repository to carry forward, and the step would
   keep every legacy reader alive to read its input. The existing `delta-vocab` step goes
   with its reader.
2. **Phase 1 before item 343.** The roadmap orders item 343 before item 479 so that the
   release writes the dataset-local codec settings in their final form. The deletion of
   pre-release readers does not depend on that, and a later settings change follows the
   ordinary format rules. The recommendation is to start Phase 1 now with a fixture
   labelled by its commit, and to keep the dependency only for the release fixture job
   of the format-policy phase, which writes the `0.1.0` fixture after item 343. The
   roadmap's Release and Sequencing sections keep the current order until this is taken.
3. **One index format.** The first release could keep writing format 3 for a generation
   without features and 4 with them, as X11 §7.2 chose to keep pre-release readers
   working, and keep the sparse index optional. The recommendation is one format, 5,
   which is format 4 with the `features` list always present and `vocab.idx` required,
   since a second write form and an optional file serve nobody after the cutoff and the
   mandatory index then has a version of its own.
4. **SPARQL `LOAD` and the load plan.** `LOAD` inserts per quad today whatever its size.
   Letting it take the bulk plan when it is the only operation of its request would make
   a large `LOAD` as fast as `sparkles load`, but changes which path an update takes and
   therefore its memory and lock profile. The recommendation is to leave `LOAD` alone in
   Phase 4 and record the choice in the load plan's table.
5. **The import transport's reader.** The unwired import transport writes `minimumReader`
   9 for an imported destination while the build reads up to 8. The recommendation is to
   leave it to the lane that wires the import, which raises `DATASET_READER` and the
   registry together once the capability has a contract, and not to widen the registry
   to make staged bytes open before then.

## Dependencies

* Items 446 and 447 have landed. Item 448 and Phase 2 land before the group-of-one
  commit of item 833.
* Items 4 and 14 settle cold reads and per-block deltas before the layout changes of
  Phase 7.
* Item 409 settles the cost of controlled preparation before the vocabulary scanners move
  to the Control trait in stage 2 of item 830.
* Item 343 lands before the format cutoff, item 479, which lands before the release
  artifacts of items 58 and 59. Open decision 2 proposes to narrow the first dependency
  to the release fixture.
* Fragment composition in [X18](../X18-fragment-composition/README.md) follows this
  workstream.

## Acceptance gates

* Writes keep their durability and crash recovery. The guards are `scripts/nsc-writes.sh`
  with and without `--no-group-commit`, `bench-indexed-commit.py`, `power-loss.sh`,
  `tests/power_loss.rs`, `group_runs.rs` and `bench:longwrite`.
* The instruction-count screen of item 473, `mise run bench:insn`, shows no increase for
  vocabulary scans and WAL replay when a reader moves to the Control trait or the cutoff
  removes a branch from replay.
* Query paths meet the forge gate of X14. Load paths meet the load benchmark at 1.05M and
  10.5M, and write paths meet the writes benchmark.
* Every on-disk format change has a version, and a reader refuses a version it does not
  know with a message that names the dump-and-reload path. The cutoff's fixtures prove the
  refusal of each pre-release form, and the fixture labelled with the cutoff commit opens,
  checks clean, answers its recorded queries and takes a write and a compaction.
* The plain and encrypted spill matrices of `test:w3c` run for the phases that touch
  `index.rs` or the cursor readers, which are Phases 3 and 5.
* Binary size and the compile time of `sparkles-core` are recorded for each stage of
  Phase 3, and a growth above 5% gets a follow-up item.
* Every measured slowdown that a gate or screen accepts, including one judged to be
  noise, gets a roadmap follow-up item in the change that accepts it, as the engineering
  rules require. Effects under about 5% and the codegen-sensitive changes of Phases 3 and
  5 are measured on an Intel host.

## Non-goals

* In-place downgrade, which X12 keeps out of scope.
* Consensus replication through Raft or quorum writes, which waits with sharding.

## Design constraints

Sharding, distributed query and distributed reasoning stay deferred. They are
reconsidered only if the billion-scale benchmarks of item 452 show a single-node ceiling.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the
published papers, standards and public documentation of these sources: Zero-cost
abstraction pattern in store/keyset.rs, LSM leveled compaction, QLever LocatedTriples,
HDT, RDF-3X dictionary split, QLever index builder, TriAD, Trinity.RDF, distributed RDFox
(Ajileye, Motik and Horrocks ISWC 2021), Aurora global databases and CockroachDB regions.
No code from another project is copied.

## Outcome

Phase 2 is built. `wal::encode_txn` in `store/wal.rs` encodes a transaction's records for
the serial, group and compaction writers, which pass their own flags and unfenced count
through a `Seal`, and the compaction writer passes translated ids. `SnapshotBuilder` in
`store/snapshot.rs` replaced the twelve full `Snapshot` literals with two starts, an
opened generation and the previous snapshot, and the seven struct-update literals stay.
`Store::publish_committed` in `store/publish.rs` takes a logical commit from the serial
path, the group leader and the bulk rebuild, and a generation switch from the compaction
switch, which calls `guard.rekey` between `current.store` and `send_replace`. The tail
notifies the guard before `current.store` on every path, and a regression test holds a
serial committer right after its publication while a waiter woken through `commits`
reads what the guard knows. The compaction tap stays under the writer lock in each path,
and the quota accounting stays with each path's write, since the serial path counts the
growth of its files and the group leader settles a prefix's reservations. The bulk path
adds the generation's prefixes before the tail appends its catalog entry, so a failed
prefix write leaves the catalog and the snapshot in step. No write moved, and no on-disk
format changed.

[Item 455](../../ROADMAP.md) tracks the remaining work. The plan above replaced
the earlier candidate list for the first eight phases on 2026-11-09 after an audit of the
code, which found about 114 twin readers rather than 80, three publish paths with a
compaction writer rather than three paths alone, a 17-field snapshot rather than 20, and
readable windows wider than the earlier text named.
