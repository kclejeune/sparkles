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

Queries over HTTP, in the local CLI and in Node now run in automatic mode by default. A
plain scan that is expected to return at least a million rows from immutable blocks, and
an uncached OPTIONAL count, stream as a cursor. Every other query runs eagerly as before.
A streamed answer that fails after its first 1 MiB aborts the transfer after a `200`
status, so only a completed transfer is a complete answer. With `send`, the native
metadata reports `stopped` and an unknown total, and a Node result that streams has no
`size`. For Rust callers `ExecutionMode::default()` is now `Auto`, and
`Dataset::query_execution` uses it, while `Dataset::query` stays eager. A request opts
out with `execution=eager`, a server with `serve --default-execution eager`, the CLI
with `--execution eager` and Node with `execution: 'eager'`.

The deadline of a streamed answer now counts the query's own time. The time a query spends
blocked on its reader, which is the response channel over HTTP and the gap between two
pulls in Node, no longer counts against `timeout`, so a slow but steady reader of a large
export is not cut off. Over HTTP a client that reads nothing for longer than the
request's timeout ends the stream as a timeout. This applies to `execution=streaming` as
well. A local `sparkles query --timeout` still counts the time its output is blocked.

`sparkles migrate --check` exits 1 for a database with an interrupted run, which a server
refuses until `sparkles migrate` runs without `--check`, and its JSON report adds `opens`.
`sparkles migrate` keeps the mode and owner of every file it copies, and it must run as
the database's owner or as root.

### Behavior

FastRP follows the meaning of `iterationWeights` in Neo4j GDS. The list has one weight
for each propagation, and the new `nodeSelfInfluence` parameter weights the random
starting vectors. The default `(0.0, 1.0, 1.0)` now propagates three times where it
propagated twice, so FastRP embeddings written by earlier builds differ from new ones and
a default call costs about a third more. A call whose weights are all zero with a zero
`nodeSelfInfluence` is refused. A write-back graph algorithm run now has the update
timeout instead of the query timeout.

### Format

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
