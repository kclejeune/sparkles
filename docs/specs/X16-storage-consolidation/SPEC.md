# X16: Storage consolidation

> **Status:** planned.
>
> **Roadmap:** [item 455](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

The design covers a first-release format cutoff, budgeted readers through one Control trait, a shared commit publish tail, one load plan, partitioned vocabularies with per-range generations, and the remaining replica, clone and format-policy work.

## Problem and goal

The workspace has not had a release, yet the store reads index format 2, the headerless
WAL format 0, `delta.vocab` format 1 and `dataset.json` formats 1 to 6. About 80 readers
exist in `_limited`, `_controlled` and read-only twins, so every reader fix is made more
than once. Three publish paths and a compaction writer each encode WAL records and build
the same snapshot structure, and the copies have drifted. The three loader entry points
choose bulk loading by four different rules. Compaction and large updates rebuild the
whole generation, and new terms need a full rebuild to be sorted.

The goal is one implementation per reader, one commit publish path, one load plan and a
storage layout whose maintenance cost follows the size of a change.

## Scope

The format cutoff comes first, because it removes the legacy readers that the
Control trait would otherwise have to unify. The budgeted readers start with the history
and WAL stacks and then move to the vocabulary scanners under the instruction-count
screen of item 473. The shared publish tail has two halves. The short-term half, item
484, extracts the WAL encoder, a snapshot builder and the shared tail now. The long-term
half lets the group leader write through the direct handles once item 446 has landed, so
that a group of one is the ordinary commit. Partitioned vocabularies and per-range
generations follow item 14's per-block located deltas.

The tiered segments of [X17](../X17-tiered-segments/README.md) and the fragment
composition of [X18](../X18-fragment-composition/README.md) build on this layout.

## Candidate phases

**First-release format cutoff and removal of pre-release readers.** Fix a first-release cutoff and refuse older formats with the dump-and-reload message, or provide one `migrate::STEPS` rewrite per format, so segments and vocabulary indexes become mandatory and the legacy WAL machinery goes. The workspace is 0.1.0 with no release (X09, X12 §4.2), yet it reads index format 2, headerless WAL v0, delta.vocab v1 and dataset.json 1 to 6. This is code the twin-reader work would otherwise have to unify. It depends on item 343. Roadmap item 479 holds this phase.

**Budgeted readers through a zero-cost Control trait.** Replace about 80 _limited, _controlled and readonly twin readers with one implementation per reader parameterized by a Control trait whose Unlimited form compiles away, with checks on a stride. Start with the history and WAL stacks, then the vocabulary scanners under the instruction-count screen. The repository already uses this pattern in `keyset.rs` Meter. It depends on item 409.

**Shared commit publish tail and WAL encoding.** Extract `wal::encode_txn`, a SnapshotBuilder and the shared publish tail now, then let the group leader write through the direct handles after item 446 so a group of one is the ordinary commit. Three publish paths and a compaction writer duplicate WAL encoding and a 20-field Snapshot literal about ten times, and the copies have drifted. This is durability-sensitive work. It depends on items 446, 447 and 448. Roadmap item 484 holds this phase.

**Smaller storage duplicates.** Share a `pack_deltas` helper between the builder and index encoding, drop the last `imbl::OrdMap`, move test scaffolding out of production code and simplify the `parse_source` ladder. These are small leftovers. History reconstruction waits for items 52 and 23. It depends on items 23 and 52.

**One load plan for all loader entry points.** Replace the three loader entry points' separate bulk thresholds with one `LoadPlan::choose`. `load_with`, `replace_with` and `insert_bulk` decide on four different rules.

**Partitioned vocabulary and per-range generations.** Replace full-generation compaction and full vocabulary rebuilds with a partitioned or hierarchical vocabulary that merges per block and leveled generations that rewrite only touched predicate or id ranges. Compaction and large updates rebuild the whole generation, the delta overlay raises query cost (two-hop-count 5.1 to 8.6 ms at 5k changes) and new terms need a full rebuild to be sorted. It depends on items 14, 160 and 343.

**Bounded-memory index build.** Bound the external-sort budget and stream permutation construction so bulk-load peak memory is configurable. Bulk-load peak is 2.9 times QLever's on the LUBM closure. Item 40 covers idle and process RSS, not builds. It depends on item 40.

**Write forwarding and linearizable replica reads.** Add `minCommit=latest`, write forwarding from a replica with the caller's identity over mutual TLS, and `minCommit` as a write precondition. F10 Phase 3 names them, while items 66 and 280 do not. It depends on items 63, 66 and 280.

**Clone fast path for sources with a delta.** Share base index files and carry the source's delta so a busy dataset clones in seconds, and record reflink timings. Any commit since compaction forces a full rebuild of 38.5 s and 1.44 GB at 10.5M.

**Remaining format-policy work.** Add the release fixture job, background rebuild of vector and spatial indexes with unknown file versions, and a WAL header migration plan. Only full-text honours the rule that derived files never need migration. It depends on items 24, 34 and 59.

**Replica scope.** Follow every dataset of a primary, keep replica-local text and vector indexes, bootstrap in-memory replicas and stream bulk commits. It depends on items 52, 66 and 280.

**Read-only open of a held data directory.** Let a second process open a store read-only without the exclusive lock, for checks, inspection and embedded readers. Every local command fails on the lock while a server runs.

**Sharding, distributed query and distributed reasoning.** Hash-partition permutations and the vocabulary, run exchange-based distributed joins through a coordinator, and add distributed semi-naive evaluation. Sparkles runs as a single node. Reasoning above 10B triples and data beyond one host need it. It depends on items 66 and 280. This phase is deferred until the condition under Design constraints holds.

**Prebuild remaining derived structures before the switch.** Prebuild the ShEx typing cache and full-text checkpoint between build and catch-up. C13 names the hook, and spatial and vector already use it. It depends on item 14.

**Direct tabular-to-loader streaming.** Feed the CSV converter straight into the loader instead of spooling and reparsing N-Triples, and speed the CONSTRUCT-template path. C05 deferred this until measured, and the measurement shows a quarter of load time and five times the table size on disk.

**Multi-region replicas and region-aware routing.** Run cross-region async replicas with lag objectives, region-aware read routing and WAN write forwarding. F10 has no region concept. Phase 3 write forwarding is the nearest piece. It depends on item 66.

## Dependencies

* Items 446 and 447 land before the long-term half of the commit-path merge, since both
  change the commit path.
* Items 4 and 14 settle cold reads and per-block deltas before the layout changes.
* Item 409 settles the cost of controlled preparation before its readers move to the
  Control trait.
* The format cutoff, item 479, lands before the release artifacts of items 58 and 59, and
  item 343's dataset-local settings land before it.
* Fragment composition in [X18](../X18-fragment-composition/README.md) follows this
  workstream.

## Acceptance gates

* Writes keep their durability and crash recovery. The guards are `scripts/nsc-writes.sh`
  with and without `--no-group-commit`, `bench-indexed-commit.py`, `power-loss.sh` and
  `bench:longwrite`.
* The instruction-count screen of item 473 shows no increase for vocabulary scans and WAL
  replay when a reader moves to the Control trait.
* Query paths meet the forge gate of X14.
* Every on-disk format change has a version, and a reader refuses a version it does not
  know with a message that names the dump-and-reload path. The cutoff's fixtures prove the
  refusal of each pre-release format.

## Non-goals

* In-place downgrade, which X12 keeps out of scope.
* Consensus replication through Raft or quorum writes, which waits with sharding.

## Design constraints

Sharding, distributed query and distributed reasoning stay deferred. They are
reconsidered only if the billion-scale benchmarks of item 452 show a single-node ceiling.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: Zero-cost abstraction pattern in store/keyset.rs, LSM leveled compaction, QLever LocatedTriples, HDT, RDF-3X dictionary split, QLever index builder, TriAD, Trinity.RDF, distributed RDFox (Ajileye, Motik and Horrocks ISWC 2021), Aurora global databases and CockroachDB regions. No code from another project is copied.

## Outcome

Nothing is built. [Item 455](../../ROADMAP.md) tracks the work.
