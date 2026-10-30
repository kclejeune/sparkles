# Changelog

## Unreleased

### Compatibility

The adopted HTTP, CLI and public library compatibility policy is documented.
HTTP descriptions and the standard CLI command tree have compatibility guards against
a commit-labeled development baseline. No published release baseline exists.
Library and binding checks, broader machine-readable contracts and deprecation
signals remain incomplete.

### Changed

Node graph overviews retain one absolute deadline across their native subqueries
instead of renewing the timeout for each query.

Catalog disposal now refuses raw shared state, Store and BranchSet aliases and preserves
exact owners on refusal. Resident source leases retain real catalog openings without
changing StoreHandle variants or granting managed-write authority. Built-in close links,
member drop witnesses and final family admission protect rename, delete and backup swaps.
Recovery checks existing roots and original identities, and uncertain delete moves retain
their original bytes instead of creating an empty replacement.

A close callback that panics during catalog delete or rename no longer drops the dataset
from `config.json` on the next save. The dataset stays registered and closed until the
catalog reopens. Catalog startup refuses only a registered dataset whose directory or
CURRENT file is missing and serves the others. Retained delete bytes are named
`.kept-<name>-<token>`.

Spilled ORDER BY can retry a smaller sort piece after a transient decoding reservation
fails. Infallible expression failures still stop the query, and memory caps stay enforced.

The successful dataset rename response is described under OpenAPI's `application/json`
media type. Its `name` and `renamedFrom` fields and wire behavior are unchanged.

Persistent datasets remain locked while an embedded caller retains a branch Store
or BranchSet after main closes, including when the branch was removed or retired.
The root lock stays held through those owners' resource destruction.

Dataset and branch administration emit audit events across HTTP, local commands and
MCP. Clone events follow actual registration and preserve the initiating actor and
selected source commit. Successful scratch expiry records a system actor, while failed
authorized operations record bounded codes without request payloads or credentials.

Rust Store snapshots and trusted Dataset history handles accept per-read memory,
cancellation and deadline limits while preserving exact retained branch origins.
Cold inherited reads prepare retained metadata and WAL state without recovery writes.
Bounded WAL replay polls cancellation and deadlines while verifying transaction checksums.
Pruned branch-time reads refuse unresolved states with the original selector and no
invented commit number, including equal timestamps after clock clamping.

History queries report the overlapping part of a recorded gap when the requested
window ends before the gap's final commit. The same inclusive bounds apply to
ascending, descending and filtered requests.

Background tasks release their server handles and task slots before publishing a final
status, including cancelled tasks whose work never ran. Closing the last server handle
after task completion releases the catalog lock without racing the worker's teardown.

The version 1 valid-time resource profile supplies RDF vocabulary, SHACL shapes, an
import normalization contract and typed active-claim and overlapping-claim stored-query
definitions. It preserves uncertain endpoints and temporal verdicts without asserting
the claim's proposition. C27 Phase 1 remains incomplete until server-owned availability
and backdating refusal pass A4; ordinary RDF timestamps do not authenticate that boundary.

Rust query callback bookkeeping remains available while thread-local owners are
destroyed. Late snapshots and callbacks retain the existing family and captured-view
checks, including sticky failures for a transaction retained by an earlier owner.

Dropping the last dataset handle now waits for its incremental views worker to
release the store before returning. Worker acquisition and shutdown are synchronized,
so a temporary host reference cannot leave the dataset directory locked after close.
A handle released by a host callback finishes shutdown after its enclosing view
operation releases its metadata locks. Synchronous callbacks, nested operations and
host replacement follow the same lifetime protocol. View operation tracking remains
available during thread-local object destruction.

Indexed nearest-neighbour ordering can join candidate batches with independent query
components. It continues searching when joins or filters discard an entire batch and
preserves duplicate solutions. Connected filters run before unrelated cross products.
Index-probe filters keep their original binding scope, with generic fallback for volatile
filters or filters that could observe new bindings after a move.

ARQ UUID aliases, unresolved dynamic function calls and effectful ARQ calls are excluded
from result-cache reuse and repeated independent nearest-neighbour inputs. These paths
share expression-purity checks while result caching retains its stricter treatment of
query-clock values.

The full-text integrity check retries segment replacements made by merges even when the
commit stamp stays the same. If the index keeps changing throughout its three attempts,
it requests another check instead of reporting missing files from superseded metadata.

Eager results are now serialized into an 8 KiB buffer of the engine's own and written to
the response in pieces of that size, where the serializers used to write a few bytes at a
time through the response writer's checks. On forge the 300,191 rows of `optional-chain`
at 10.5M reach the client in 150.8 ms instead of 220.4 ms, `order-by-full` and `star-join`
take 19% and 30% less client time, and a plain SPARQL JSON answer of a small query takes
up to a third fewer instructions. The serializers are now compiled
with the engine, so their speed no longer moves with unrelated changes to the server. The
rows reported for an answer that stops partway are still the rows the client received.

The query route no longer names the query's patterns in the plan of an answer that does
not show the plan, which is every format but the Sparkles format, and an in-process
SERVICE call no longer names them in its dropped plan. The explain routes, MCP and the
library keep them. A plain SPARQL JSON request of a small query runs up to 5.8 thousand
fewer instructions.

`scripts/nsc-bench.sh` now records the compilers of each binary and refuses to compare
two binaries built with different ones unless `--allow-mixed-compilers` is given.

The first run of a query text that the server does not remember now runs on the thread
that received it under the request's timeout when the server has been idle for 10 ms,
instead of on the blocking pool. On forge that took about 1.8 ms off a fresh server's
first request, and the BSBM explore mix, whose texts are all new, kept its speed. Such a run cannot be cancelled by a client disconnect, only by its timeout, and a
text whose run is slow or fails, or that contains `SERVICE`, runs on the pool. A SERVICE
IRI now names this server only when it is written in the URL parser's own spelling, so
forms such as `127.1` or a path with `..` are ordinary HTTP calls.

Automatic execution now streams groupings and counts over an estimated 20,000 input rows
or more whose EXISTS and NOT EXISTS are FILTER conjuncts that a key set answers. On a
Namespace instance at 10.5M quads these ran 0.68 to 0.99 times as long as eagerly. An
EXISTS in BIND, an OPTIONAL condition or a larger expression keeps the plan eager.

A finished eager SELECT or ASK now keeps only its result's estimate in the query memory
pool instead of its query's peak, so a held result or a Python or Node result object no
longer reserves the memory of the tables it was built from. CONSTRUCT and
DESCRIBE results keep their peak as before.

MCP tool calls and resource reads now wait for the query memory pool like other queries
and answer `503` with `Retry-After` when the admission wait passes, where they used to fail
with `server-memory`. The queries of `sparkles ask`, the GraphQL draft, reasoner
diagnostics, the Graph Store DELETE and tabular uploads now charge the grant their request
was admitted with instead of taking a second one or none.

With the query memory pool on, the server now admits a query or update after its body has
arrived, so a client that sends slowly no longer holds 16 MiB of the pool while it sends.
Uploads, patches and Graph Store writes no longer take an initial grant or wait for the
pool, and a release of memory wakes only the waiters that fit.

A SERVICE call from the SPARQL query or update endpoint to this server now runs in
process under the caller's principal, grant and deadline, where it used to wait for the
memory pool behind its caller and fail with `503`. Only an IRI that names the configured
public URL or the bound listener exactly counts as this server, and such calls nest at
most eight deep.

Automatic execution now streams a plan without spill when its eager output is expected
to outgrow the memory the query may use while the cursor's own state fits. BSBM BI's q4
with ProductType6, a `FILTER NOT EXISTS` over a Cartesian product, now answers at 10M in
15.1 s with 282 MB of server memory under the default pool, where the pool refused it. A
request that needs more than the whole query memory pool now answers `507` without
`Retry-After`, since a retry cannot succeed.

In the JVM library, `abort()` or a timeout that raced the end of a query could throw
`IllegalStateException` because the query's native object had just been released. Such a
cancel is now ignored, since the query has already finished.

The reasoner's limit of 50 million inferred triples is now a setting. `sparkles infer
--max-inferred N`, `serve --reason-max-inferred N`, the `maxInferred` field, form field
or query parameter of `POST /$/reason/{ds}`, `ReasonRequest::max_inferred`, Python's
`max_inferred=`, Node's `maxInferred` and the JVM's `ReasonOptions.maxInferred` set it,
and 0 removes it. A request may only lower the server's limit, and a higher one is
refused with `400`. A run past its limit now fails with the error code `inferred-limit`
instead of an invalid-input error, so Python raises `SparklesError` with that code where
it raised `InvalidInputError`, and the message names the rule that produced the most
candidates and the settings that raise the limit.

The `small` memory profile no longer slows cold lookups. A prefetch request may add an
eighth of the block cache, but at least 64 MiB or a quarter of the cache, whichever is
smaller, so the profile's 256 MiB cache admits the parallel decode that a cold
`star-lookup` at 10.5M needs. Caches of 512 MiB and more keep the eighth.

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

Index generations are written in index format 3. Each column of a permutation block is a
run of independent LZ4 frames of 2,048 rows, and a new file `<perm>.seg` beside each
permutation holds every frame's first key and compressed lengths, under the magic
`SPKSEG01` and the frame size. A point lookup, a short range scan or an index join then
decodes the frames that can hold its rows instead of whole 32,768-row columns. This build
opens format 2 generations as before, and the next compaction rewrites them in format 3
in a new generation directory that replaces the old one in one step. Releases that read
only format 2 refuse a format 3 store with an error that names both versions. At 10.5M
triples a generation grows by 3.0%, from 300.6 MB to 309.7 MB.

A bulk load or a full compaction run with the new global flag `--vocab-codec fsst`
writes index format 4. Its `meta.json` lists the feature `fsst-vocab`, and the new file
`vocab.fsst`, the primary kind `vocab-fsst` version 1 with the magic `SPKFSST\0`, holds
the symbol tables that code the vocabulary's key suffixes. `vocab.dat` shrinks by 29% on
the DBpedia 50M slice and 32% at 10.5M, and warm queries that decode many terms run up to
13% slower, so a new database stays front-coded and in format 3. This build refuses a
generation that lists an unknown feature and names it, and releases that read only
formats 2 and 3 refuse format 4 as newer. A compaction keeps the codec of the generation
it replaces unless the flag asks for another, and then it rebuilds in full. Backup
manifests carry the generation's features as `indexFeatures`, and a restore refuses an
unknown one. `sparkles stats` names the vocabulary codec, and `sparkles check` verifies the
tables.

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
