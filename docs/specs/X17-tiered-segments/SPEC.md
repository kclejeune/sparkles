# X17: Tiered segments

> **Status:** planned.
>
> **Roadmap:** [item 455](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

Immutable generations uploaded to S3-compatible storage after compaction, fetched as whole segments into a local disk cache and memory-mapped from there, with lazy dataset open and idle unload.

## Problem and goal

S3-compatible storage holds backups only, so a dataset must fit on local disk and
cannot share a base generation with other servers. Every opened dataset and branch stays
resident until the server stops.

The goal is a cold tier for immutable generations that keeps the existing read path,
together with lazy opening and idle unloading for servers with many datasets.

## Scope

After compaction, a generation's segments are uploaded to object storage. A cold
generation is fetched as whole segments into a local disk cache and memory-mapped from
there, so reads after the fetch use the existing memory-mapped path. The WAL, the delta
and the vocabulary stay local. Lazy opening and idle unloading reopen
[P06 open question 2](../P06-library-admin-api.md), which found no need at the scale then
measured, for many-dataset and serverless deployments.

## Candidate phases

**Tiered storage with object-store-backed cold generations.** Upload immutable generations to S3-compatible storage after compaction, fetch a cold generation as whole segments into a local disk cache and memory-map it from there, and keep WAL, delta and vocabulary local. There are no range reads and no second block cache beside memory mapping. S3 serves backups only, item 4 covers cold local reads and item 237 covers Parquet sources. Datasets larger than local disk and shared bases need it. It depends on items 4 and 61.

**Scale-to-zero with lazy dataset open and idle unload.** Open datasets on first use, unload idle datasets and idle branch stores and reopen them lazily, and start cold from object storage for a serverless shape. Every opened branch stays resident until server stop today (F09 §10). P06 open question 2 measured lazy opening and found no justification at current scale, so this reopens that question for many-dataset and serverless deployments. It depends on item 402.

## Dependencies

* The consolidated layout of [X16](../X16-storage-consolidation/README.md) and
  [item 455](../../ROADMAP.md).
* Item 4 for cold local reads and item 61 for the object-store backends that backups use.
* Item 402 for startup states, which lazy opening extends.

## Acceptance gates

* A query over a fetched generation meets the cold-read targets of
  [X07](../X07-cold-sparse-reads.md) once its segments are local.
* An interrupted fetch leaves no partial segment in use, and a crash during upload loses
  no committed data.
* The cache directory records a format version and refuses one it does not know.

## Non-goals

* Range reads from object storage.
* A second block cache beside memory mapping.

## Design constraints

Fetching whole segments to a local disk cache, with memory mapping kept, is the
chosen design. It keeps one read path and avoids a second cache whose sizing and eviction
would compete with the page cache.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: Neon, Stardog cloud, ClickHouse and DuckDB object-store tables and serverless databases. No code from another project is copied.

## Outcome

Nothing is built. [Item 455](../../ROADMAP.md) tracks the work.
