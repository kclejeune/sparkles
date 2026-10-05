# @sparkles-rdf/engine

Sparkles embedded in Node.js, with typed RDF/JS terms, persistent datasets and no server. Node-API engine work runs on Rust threads. SELECT and quad results are pulled in bounded batches; loading and dumping support Node.js and Web streams with backpressure.

```ts
import { Dataset, factory } from '@sparkles-rdf/engine';

const ds = await Dataset.open('./example');
try {
  await ds.load('<urn:s> <urn:p> "hello" .', { format: 'nt' });
  const rows = await ds.select('SELECT ?o WHERE { <urn:s> <urn:p> ?o }');
  for await (const row of rows) console.log(row.get('o')?.value);
  const { receipt } = await ds.transaction(
    async (tx) => {
      await tx.add(
        factory.quad(
          factory.namedNode('urn:a'),
          factory.namedNode('urn:p'),
          factory.literal('value'),
        ),
      );
    },
    { timeout: 1000, message: 'add value' },
  );
  console.log(receipt.commit.seq); // bigint
} finally {
  await ds.close();
}
```

`Dataset.memory()` makes an isolated in-memory dataset. `Catalog.open(path)` and `Catalog.memory()` manage named datasets; branches live under each dataset, accessed with `ds.branch(name)` or `ds.branches.open(name)`. Prefixes, snapshots, history, stored queries, settings, indexes, reasoning, validation, schema and GraphQL are property handles. `Repository.open(config)` opens a backup repository; `ds.backups(repository)` captures, lists, verifies and deletes this dataset's backups.

Query results expose `plan`, `timing`, `variables` and an async iterator with `toArray()`, `toStream()` and `close()`. A retained row remains valid after closing its result. Stop early with `break`, or call `close()` explicitly. `ds.source()` implements RDF/JS Source with native pattern counts for Comunica. `ds.store()` additionally implements import/remove/removeMatches/deleteGraph. Use Sparkles SPARQL directly for joins executed in the native engine.

Counts and commit numbers use `bigint`; integer arguments also accept safe JavaScript numbers. RDF terms from other factories are accepted. Blank-node labels created externally identify new nodes within each write; use terms read from the dataset to refer to existing stored nodes. Directional literals and triple terms round-trip, with triple terms allowed in object position.

Queries, updates, loads and transactions accept `signal` and millisecond `timeout`; writes accept `message`, `ifHead` and `noWait`. A transaction callback sees pending changes through its `tx` parameter. Dataset writes and snapshot-capturing administration inside that callback reject instead of waiting on the callback's writer lock. A timeout rolls back even if the callback never settles. Escaped transactions reject after ending. `update(text, { dryRun: true })` reports the intended changes without creating a commit.

`load` accepts a string, bytes, a Node.js Readable, a Web ReadableStream, an async iterable of byte chunks, or `{ path }`. `loadFiles` loads several paths atomically. Paths infer RDF format/compression from their extension. Streams require an explicit format. `dump` returns a Web ReadableStream; `dumpToFile` and `dumpToString` are convenience methods. `queryToStream` serializes results on a Rust thread; pass `accept` for TSV, SPARQL JSON or an RDF result format.

`ds.changes({ after, signal })` yields recorded commits and their RDF/JS changes, waiting for engine notifications between commits. Its `return()` and `close()` cancel a pending wait; closing the dataset closes its streams and transactions. History gaps fail explicitly. If a commit exceeds the configured `maxQuads`, increase that ceiling before resuming from the last complete cursor.

The package targets Node.js 22.12 and later, Linux glibc/musl on x64/arm64, macOS x64/arm64 and Windows x64. Bun and Deno use their Node-API compatibility layers; Deno needs local `node_modules` and permission to read the addon and call FFI. CI builds and installs archives on those platforms. Builds are not published until the npm environment and gated release workflow are enabled.

For development, run `mise run node:build` and `mise run node:test`. The local build defaults to release; set `NODE_PROFILE=dev` for a faster smoke build. `SPARKLES_NODE_NATIVE` may point to a compiled addon for development. Bundlers must preserve the native `.node` package as an external file.

`parse(input, {format: 'ttl', baseIri, compression, signal, timeout, factory})`
returns an async RDF/JS quad iterator. `serialize(quads, {format, compression,
signal, timeout})` returns a Web byte stream. Both use bounded native queues
without a temporary dataset; iterator return and stream cancellation release
owned producers and readers, including stalled input. The default format is
N-Quads. A single encoded quad may use at most 16 MiB; RDF parsers and codecs may
also buffer according to their syntax. The vector embedding executor accepts
`waitMs` to bound its worker, and does not advertise an AbortSignal control.

Administration dry runs, transaction dry runs and load dry runs are unsupported;
use `update(..., {dryRun: ...})` or explicit branch preview methods. Conditional
`ifHead` applies to ordinary writes and patches; administration operations reject
it when their engine facade cannot honor it. Set `ifHead` when beginning a
transaction; `tx.update` rejects `dryRun` and `ifHead` before mutation.
