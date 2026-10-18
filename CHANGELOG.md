# Changelog

## Unreleased

### Changed

The server's query memory pool is on by default. It holds half of the memory the process
may use, less the configured caches, and at least 256 MiB. Each query and update request
reserves 16 MiB when it is admitted, so a host whose pool sits at the 256 MiB floor serves
at most 16 concurrent queries and updates. Further requests wait for
`--query-admission-wait` and then answer `503` with `Retry-After`.
`--total-query-memory-mb 0` turns the pool off.

`--cache-mb`, `--result-cache-mb`, `--service-cache-mb` and `--history-cache-mb` now size
one cache for the whole server instead of one per dataset. The default result cache fell
from 512 MiB to 256 MiB and the default history cache from 1 GiB to 512 MiB.
`StoreOptions::default` takes the same result and history sizes, which changes the
defaults for library users. `--memory-profile small` sets smaller sizes for constrained
hosts.

A plain query takes memory from the query pool in steps of 64 KiB, so it can hold up to
64 KiB of the pool beyond its estimate. That removed most of the pool's cost on queries of
a few microseconds. The server and the Python, JVM and Node.js libraries call mimalloc
through its plain entry points, so a full decoded block column takes 256 KiB of heap
instead of 320 KiB. A server with ten datasets at `--cache-mb 256` used 493 MiB of
anonymous memory after the suite instead of 573 MiB.

Queries over HTTP, in the local CLI, in Node and in the JVM library now run in automatic
mode by default. A plain scan that is expected to return at least a thousand rows from
immutable blocks streams as a cursor, and so does a grouping that reads an estimated
20,000 rows or more, counts over OPTIONAL and MINUS included, since the cursor's column
kernels measured 0.50 to 0.91 times the eager time on such queries. Every other query
runs eagerly as before. A streamed
answer that fails after its first 1 MiB aborts the transfer after a `200` status, so
only a completed transfer is a complete answer. A streamed answer reports no total in
advance. The native metadata of a scan of a thousand rows or more therefore reports an
unknown total with `send`, and a Node result of such a scan has no `size`, where both
used to appear for every answer under a million rows. For Rust callers
`ExecutionMode::default()` is now `Auto`, and `Dataset::query_execution` uses it, while
`Dataset::query` stays eager. A request opts out with `execution=eager`, a server with
`serve --default-execution eager`, the CLI with `--execution eager`, Node with
`execution: 'eager'` and Jena with `Sparkles.STREAMING_EXECUTION` set to false. A server
remembers a streamed answer that completed in one body in under 10 ms as a quick query,
as it does an eager one, and runs its next request eagerly in place.

A Jena result that streams can throw from `hasNext()` partway through, and Jena queries in
a write transaction run eagerly. A Jena query iterator now throws
`JenaTransactionException` when it is read after its transaction has ended, eager or
streamed, where an eager result used to hand out its remaining rows. This covers an
iterator created in a read transaction and first read after the transaction ended, and
a final `hasNext()` after the last row when it comes after the end of the transaction.

The deadline of a streamed answer now counts the query's own time. The time a query spends
blocked on its reader, which is the response channel over HTTP and the gap between two
pulls in Node, no longer counts against `timeout`, so a slow but steady reader of a large
export is not cut off. Over HTTP a client that reads nothing for longer than the
request's timeout ends the stream as a timeout. This applies to `execution=streaming` as
well. A local `sparkles query --timeout` still counts the time its output is blocked.

`sparkles serve --spill on` lets a streamed sort without LIMIT, a grouping and a DISTINCT
over unordered input write what does not fit in their memory budget to `--spill-dir`
(default `<data>/.spill`) and finish. The disk is limited by `--spill-max-mb` for the
server and `--query-spill-mb` for one query, a request may lower the latter with
`spill-mb`, and a query over it fails with the new `spill` budget and `507`. With spill
on, automatic execution also streams a query that eager execution would hold in more
than half its budget, and runs an eager SELECT that exceeds its memory budget once more
as a cursor unless it calls SERVICE anywhere. Spill is off by default until hash joins
spill too. A DISTINCT above an ORDER BY keeps its rows in memory so that their order
holds. `serve --spill on` fails to start when the spill directory cannot be used, sits
on tmpfs or ramfs, is not owned by the server's user or is writable by others, and
`--spill-max-mb 0` is refused. `/$/server/memory` adds a `spill` object and
`/$/metrics` the `sparkles_spill_*` series, `sparkles_spill_eager_retries_total`
among them. A streamed grouping over a join can concatenate a `GROUP_CONCAT` in another
order than an eager one, which SPARQL allows, and an ordered subquery fixes the order in
both. Rust callers set
`QueryOptions::spill` and `max_spill_bytes`, and `BudgetKind::Spill` is a new variant.

`sparkles migrate --check` exits 1 for a database with an interrupted run, which a server
refuses until `sparkles migrate` runs without `--check`, and its JSON report adds `opens`.
`sparkles migrate` keeps the mode and owner of every file it copies, and it must run as
the database's owner or as root.

Group commit is on by default for persistent stores. Concurrent commits of a dataset share
one synchronization of the log, and each commit is still acknowledged only once it is
durable. On `forge` four writers commit about 1.8 times as fast as before and sixteen
writers about 5.4 times as fast, while a single writer's latency is unchanged. A dataset
with a full-text, spatial or embedding index keeps committing on its own. The global flag
`--no-group-commit` or `SPARKLES_GROUP_COMMIT=off`, `false`, `no` or `0` turns grouping
off, and an unrecognised value of the variable logs a warning and keeps it on. Node's
`Dataset.open` and `Catalog.open` take `groupCommit`, and Python's `Dataset`,
`Dataset.open` and `Catalog` take `group_commit`, both `true` by default. The JVM library
follows the default and has no option. Embedded Rust sets `StoreOptions::group_commit`,
which now defaults to `true`. Grouping costs memory. Each persistent dataset may hold up
to 64 queued commits or 4 MiB of log records, plus the snapshots of commits in flight,
whose size was not measured. The hidden `--experimental-group-commit` flag is removed, and
passing it is a usage error.

The closure a dataset keeps for incremental reasoning runs holds only the derived triples
and reads the input graphs from the store's indexes. On LUBM-style data with 13.1 million
triples it took 918 MiB instead of 2,267 MiB for RDFS and 1,226 MiB instead of 2,654 MiB
for OWL 2 RL, and a run that reads the closure back from a persistent dataset took 40% to
73% of its previous time. This trades speed for memory. A change of 1,000 triples with a
kept closure took 1.3 to 4.9 times longer at 16 threads, up to 142 ms on that data,
because it reads the input graphs from compressed blocks. The kept closure now remembers
the answers of its lookups of the input graphs between runs, up to 64 MiB charged to the
memory budget, and the searches of a deletion round run in parallel. That brings such a
change to 12 to 31 ms for RDFS and 51 to 88 ms for OWL 2 RL at 16 threads on that data, 1.0
to 1.6 and 1.5 to 2.4 times its time before the closure dropped the explicit facts, for 10
to 18 MiB more kept memory. The goal of roadmap item 87 is 1.2 times, so the item stays
open. A read that the remembered answers serve counts in `baseScans` as the scan it
saves. `serve --reason-cache-triples` now counts derived triples only, so the
same limit keeps the closure of a larger input, and `baseScans` of an incremental run
counts every read of the input graphs.

`literal-not-in-datatype` finds ill-typed literals by a scan of the vocabulary that it
remembers per generation, instead of a query over every literal value, which took 11 to 12 s
on 13.1 million triples and now takes under 20 ms. The generation keeps the ids of at most
262,145 ill-typed literals, 2 MiB, and frees them with itself. With the inferences included,
it also reports the literal typings among the generalized triples of an RDFS or OWL 2 RL
materialization, read from the closure the dataset keeps or from the generalized triples a
persistent dataset saved, with the basis `uses-inferences`. They are reported for the
literals that the checked graphs hold. For Rust callers,
`sparkles_reasoner::diagnostics::diagnose_with` takes these typings, and
`sparkles_reasoner::literal_typings` reads them.

### Behavior

A SERVICE inside an EXISTS that the query evaluates per row is called once for each
distinct binding of the variables it shares with the outer rows, and once in all when it
shares none. A streamed query whose memory budget was nearly spent used to call it once
per row.

Open deletes a generation directory newer than the one `CURRENT` names when it is an
unfinished build of the dataset, and the deletion is permanent. A power loss during a
compaction or a bulk load can leave such a directory with an empty build marker, which
earlier releases kept until the next rebuild reused its number. Any other directory above
`CURRENT`, such as one that a `CURRENT` restored from an older copy no longer names, is
renamed to `gen-N.set-aside-<ms>` and kept, and `sparkles check` reports it. A bulk build
that a crash stopped before its switch is always set aside, as a full copy of the indexes.
Set-aside directories count toward the byte quota and appear in `sparkles migrate`'s list
of generations until an operator deletes them or renames one back. Open also
deletes the store's own temporary files, such as a `CURRENT.tmp` left by an interrupted
switch, and leaves every other `.tmp` file alone. A directory that holds generations but
no `CURRENT` is now refused instead of opened as a new, empty database.

A commit whose own log synchronization fails keeps its message, change digest and merge
record. Such a commit can still come back after a restart, and it used to come back
without them, which left a hole in the digest chain of a dataset with `--commit-digests`.
The writer stops taking commits after the failure as before, and open drops the records
of a commit that did not survive.

FastRP follows the meaning of `iterationWeights` in Neo4j GDS. The list has one weight
for each propagation, and the new `nodeSelfInfluence` parameter weights the random
starting vectors. The default `(0.0, 1.0, 1.0)` now propagates three times where it
propagated twice, so FastRP embeddings written by earlier builds differ from new ones and
a default call costs about a third more. A call whose weights are all zero with a zero
`nodeSelfInfluence` is refused. A write-back graph algorithm run now has the update
timeout instead of the query timeout.

A query plan deeper than 256 levels, as `/{ds}/explain`, the Sparkles result format and
the cursor write it, is in plan format 2. Its root has `"planFormat": 2` and its nodes
have no `id`, which a client derives from the shape. Shallower plans are unchanged.
Planning a basic graph pattern near the plan limit is faster by 1.1 to 33 times
depending on its shape, 3 to 4 times for a plain chain or its reverse. A star on one
frequent predicate gains the least. Planning observes the request's timeout and cancellation
throughout.

### Format

Spill files are registered as the derived format `spill` version 1. A file has the magic
`SPKSPL01`, the version as a u16, the operator kind, the codec, an encryption flag and
the column count, then frames of up to 4,096 rows with a CRC-32 over each frame's header
and columns. Only the process that wrote a file reads it, and a reader refuses another
version, kind, codec or encryption flag. A spill directory holds a `SPARKLES-SPILL`
marker that names the format and its version.

The ShEx write guard's persisted typing, `validation-typing.bin`, is registered as the
derived format `validation-typing` version 1. The version is the u32 after the `SPXTYP01`
magic. A file of another version is discarded and the typing is rebuilt by a full
validation on the next write. The file's header also covers a digest of the schema copy.

Checked migration state in `.migrate-checked` uses JSON format 1. It records the cleanup
mode and exact old sibling directory so recovery by a later release preserves unrelated
kept copies. Unknown formats, malformed markers and bare legacy mode markers refuse
automatic recovery without changing the migration copies. Bare legacy state requires
inspection of the original directories and manual recovery or restore.

Runtime model secrets are sealed by default in builds with the default `backup` feature.
The first start seals existing plaintext values and generates `secrets.key` if no key
source was configured. Older releases require offline `sparkles secrets unseal --data
DIR` before downgrade. A data directory owned by another user needs `--secrets-key
SOURCE` or `--secrets-key none` until ownership is corrected. The format registry now
records the existing sealed v1 envelope and raw generated key without changing their
bytes or requiring a dataset migration. Unknown envelope versions are refused.
