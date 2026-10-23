# Changelog

## Unreleased

### Changed

Automatic execution now streams groupings and counts over an estimated 20,000 input rows
or more whose EXISTS and NOT EXISTS are FILTER conjuncts that a key set answers. On a
Namespace instance at 10.5M quads these ran 0.68 to 0.99 times as long as eagerly. An
EXISTS in BIND, an OPTIONAL condition or a larger expression keeps the plan eager.

A finished eager SELECT or ASK now keeps only its result's estimate in the query memory
pool instead of its query's peak, so a held result, a Python or Node result object or a
cached result no longer reserves the memory of the tables it was built from. CONSTRUCT and
DESCRIBE results keep their peak as before.

Group commit now covers datasets with a full-text, spatial or embedding index, which used
to take the ordinary path. On a Namespace instance whose synchronizations were slowed to
4 ms each, four writers on such a dataset committed about twice as fast and sixteen
writers about eight times as fast. On the instance's own synchronization of about 0.1 ms,
four writers gained nothing and sixteen writers about 1.6 times. These are approximate
screens, and the gates on `forge` have not run. A commit falls back only while a
full-text index recovers or rebuilds or while a spatial index builds or after its build
failed, and `sparkles_group_commit_fallbacks_total` reports these as `text-recovering`,
`text-rebuilding` and `geo-unavailable` in place of the old `text`, `geo` and `embed`
reasons. A full-text search on a transaction that started from a commit still in flight
waits for that commit's fence. `--no-group-commit` and `SPARKLES_GROUP_COMMIT=off` still
turn grouping off.

On a store of more than ten million quads, the schema reports of the MCP tools,
`check_query` and `sparkles ask` take the counts of predicates and classes from the
statistics of the index build, corrected exactly for the changes since and for graphs
outside the selection, and say `"basis": "statistics"`. A selection that leaves out
graphs with more than a million quads is still scanned.
The kinds of a predicate's objects are measured when a tool needs them, from a sample of
8,192 stored literals above 200,000 quads. On DBpedia's billion quads a `check_query`
took 52.7 seconds and 11.1 GiB and now takes 0.3 seconds and under 500 MiB, and an
Ask takes about two seconds instead of 100. `--schema-statistics-above QUADS|off` on
`serve`, `mcp` and `ask` sets the threshold. `/{ds}/schema` and `sparkles schema` still
scan. A dataset keeps up to four schema reports, one per selection, instead of one.
`describe_schema` leaves `maxPerSubject` out of a report from the statistics and adds
`objectsSampled` to a predicate whose objects were sampled.

The planner moves a VALUES table that shares variables with a UNION of the same join
into each branch, so each branch looks its rows up by index instead of reading every
quad of its pattern. It moves at most 10,000 rows, keeps the table outside a BIND or
FILTER that calls a non-deterministic function, and leaves a UNION with a SERVICE, a
LATERAL or a property function call as written. `link_entities` and the terms list of `check_query` failed on the
2 GiB query budget on DBpedia for this reason. `sparkles ask` gained
`--query-memory-mb`, 2048 by default as in `sparkles mcp`.

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

`sparkles serve --spill on` enables disk spill. It remains off by default until all
Phase 2b acceptance gates pass. A streamed sort without LIMIT, a grouping, a
DISTINCT, a hash join and a long run of equal keys in a merge join write what does not
fit in their memory budget to `--spill-dir` (default `<data>/.spill`) and finish. The disk is limited by `--spill-max-mb` for the
server and `--query-spill-mb` for one query, a request may lower the latter with
`spill-mb`, and a query over it fails with the new `spill` budget and `507`. With spill
on, automatic execution also streams a query that eager execution would hold in more
than half its budget, and runs an eager SELECT that exceeds its memory budget once more
as a cursor unless it calls SERVICE anywhere. In `SELECT DISTINCT … ORDER BY` the
DISTINCT runs below the sort, numbering its input rows so that the sort keeps the order,
and may spill. A spilled hash join keeps its probe input's order only when an operator
above it may rely on that order, and otherwise emits its partitions in turn.
`--spill encrypted` seals every spill frame with AES-256-GCM under a random key for each
file held only in memory, so a changed, moved, reordered or truncated frame fails the
query, and `--spill off` turns spill off. When the spill directory cannot be used, sits
on tmpfs or ramfs, is not owned by the server's user or is writable by others, the
server fails to start when spill is enabled. An ordinary start leaves the spill
directory unopened. `--spill-max-mb 0` is refused. `/$/server/memory` adds a `spill` object and
`/$/metrics` the `sparkles_spill_*` series, `sparkles_spill_eager_retries_total`
among them. A streamed grouping over a join can concatenate a `GROUP_CONCAT` in another
order than an eager one, which SPARQL allows, and an ordered subquery fixes the order in
both. Rust callers set
`QueryOptions::spill` and `max_spill_bytes`, and `BudgetKind::Spill` is a new variant.

A streamed FILTER over an index scan reads the scan in morsels of whole index blocks and
filters each morsel in parallel when every conjunct is a key test on the scan's key
column, and a STRSTARTS filter reads only the blocks its prefix can match. At 10.5M with
eight threads on an 8-vCPU AMD Namespace instance,
CONTAINS, STRSTARTS and LANGMATCHES filters over names and titles now take 0.91 to 0.95
times their eager time, where they took 1.48 to 7.99 times. More threads hold more morsel
scratch at once, 13.5 MB in the ledger at eight threads against 11.8 MB before.

A streamed range top-k no longer reads the parts of its range whose ids do not follow
value order into memory. It ranks them in parallel morsels of one index block and keeps
only its best rows, so its memory no longer grows with the range. A default graph made of
several graphs still holds each triple once across the morsels. At 10.5M with eight
threads on 8-vCPU AMD Namespace instances it takes 0.89 to 1.06 times its eager time over
several runs, where it took 1.33 times.

A streamed answer's term cache sorts its vocabulary ids with a stable sort that merges
the columns already in index order. With the parallel filter, a STRSTARTS filter over
names in native JSON at one thread takes 1.01 to 1.03 times its eager time, where it
took 1.30 times.

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

The Ask evaluation harness converts Text2SPARQL'25 (`scripts/text2sparql-convert`, the
DBpedia set and the CK25 corporate set) and LC-QuAD 2.0 (`scripts/lcquad2-convert`).
Every converter that recomputes gold answers on a database now also compares them with
the answers the source publishes and records the load manifest of that database. The
paid wrapper `scripts/eval-ask-anthropic` lists the ledger's charges that stand at an
upper bound with `--held` and replaces one with the billed amount and its evidence with
`--reconcile`. That is how the $1.002560 that the 2026-10-15 pilot left unresolved can
be settled once the billed amount is known. An amount above the held charge needs
`--allow-raise`, and the evidence is 10 to 1000 printable characters. The wrapper
decodes a reply that comes back gzip or deflate encoded, settles any fault of a request
at its bound, and charges Sonnet 5.5 cache reads at the published $0.20 per million
tokens. The DBpedia gold rewrites leave literals and comments alone, never reuse a
variable of the query and also rewrite `COUNT(*)`. A sampled LC-QuAD 2.0 set records how
many questions it sampled.

The Ask evaluation scores under `rdf-term-set-projected-columns-v2`. An answer with more
columns than the gold, such as a name beside each IRI, is projected onto the gold's
columns and its rows compared as a set, where the first policy scored any extra column
as wrong. Each row also reports the accuracy without projection and how many correct
answers needed one, and `--scoring-policy rdf-term-set-global-columns-v1` scores under
the first policy. `scripts/eval-ask-rescore` scores a run's saved answers again under the
current policy without calling a model, and the paid wrapper records every request and
reply, key redacted, in `replies.jsonl`.

### Behavior

`sparkles ask` decides whether a draft calls SERVICE from the parsed query, including
the patterns inside EXISTS and subqueries, instead of scanning its text. A query whose
class, variable or prefix is named like the keyword, such as `pv:Service` or `?service`,
now runs, and a query that hides the keyword from a text scan is still left unrun.

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

The Ask evaluation cost ledger is in version 2. Reserve and settle lines carry a time,
and a reconcile line replaces a charge held at its bound. A reconcile line that raises
a charge carries `"raise": true`. A reader refuses a ledger of version 1, which no paid
run wrote, and of any later version. Replay also refuses a negative or non-integer
amount, a request ID that is not a positive integer, a settle without its reserve, a
carryover line anywhere but right after the header, and a reconcile that takes the
ledger past USD 25.

Spill files are registered as the derived format `spill` version 2. A file has the magic
`SPKSPL01`, the version as a u16, the operator kind, the codec, the encryption, the
column count and the file's number, then frames of up to 4,096 rows with a CRC-32 over
each frame's header and columns. An encrypted frame holds its columns sealed with
AES-256-GCM, with the file and frame numbers as the nonce and the frame's kind, place and
header as associated data, and the CRC covers the sealed bytes. Only the process that
wrote a file reads it, and a reader refuses another version, kind, codec or encryption.
Version 1 files are refused, which costs nothing, since spill files never outlive their
process. A spill directory holds a `SPARKLES-SPILL`
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
