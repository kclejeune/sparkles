# @sparkles-rdf/client

A streaming TypeScript client for Sparkles and plain SPARQL endpoints, with RDF/JS terms and generated types for the complete Sparkles administrative API. The client uses standard Fetch and Web Streams APIs in Node.js 22.12+ and browsers.

```ts
import { SparklesClient, factory } from '@sparkles-rdf/client';

const client = new SparklesClient('http://localhost:3030', { token: 'your-token' });
const dataset = client.dataset('wiki', 'work');
const result = await dataset.select('SELECT ?s WHERE { ?s ?p ?o }');
try {
  for await (const row of result) console.log(row.get('s')?.value);
} finally {
  await result.close();
}

const update = await dataset.update('INSERT DATA { <urn:s> <urn:p> 1 }', {
  message: 'Add an example',
});
console.log(update.inserted, update.receipt?.commit.seq); // bigint

const response = await dataset.graphs.get(factory.namedNode('urn:graph'));
console.log(await response.text());

const { data, error } = await client.api.GET('/$/datasets/{name}', {
  params: { path: { name: 'wiki' } },
});
```

`query()` returns a bindings stream, a quads stream, or `{ type: 'boolean', value }`. `select()`, `ask()`, and `construct()` check that result kind. SELECT JSON and N-Quads are decoded incrementally, including RDF 1.2 triple terms and directional language literals. Supply `factory` in query options to construct terms with another RDF/JS factory. Break from iteration or call `close()` to cancel the body and release reader ownership; `toArray()` consumes and closes the stream.

Queries accept `baseIri`, `prefixes`, `defaultGraph`, `namedGraphs`, `timeout` in milliseconds, `signal`, `at`, `includeInferred`, `maxRows`, `maxRowsProduced`, `noCache`, and `describe`. The union default graph setting cannot be overridden per remote request. Remote `maxMemoryBytes` must be a whole positive number of MiB, matching the HTTP budget parameter. Unsupported native options, such as pre-bound variables and transfer batch sizes, throw `UnsupportedError` before dispatch. Commit receipts, update counts and the `commits()` history helper use bigint; unsafe JSON numbers are rejected rather than silently rounded. The generated `api` surface retains the wire JSON types and returns `{ data, error, response }`.

Graph Store operations are `graphs.get`, `put`, `post`, and `delete`. A graph is a NamedNode or DefaultGraph; omitted means default. Writes take an RDF body and `contentType`; `put` also accepts `ifMatch` from a prior read's ETag. Sparkles writes forward `message`. `dryRun`, `noWait` and `ifHead` are unsupported remotely and reject before sending a write. The `commits({ limit, before, after, signal })` helper preserves the selected branch in pagination links.

`batch(callback)` combines synchronous `batch.update(text)` calls into one SPARQL Update request. It rejects asynchronous callbacks. Updates are never retried. Queries and safe reads retry 429/502/503/504 responses up to `maxRetries` (default 3), honoring a bounded Retry-After and cancellation. `signal` and local timeout cover request and streamed body consumption, including injected Fetch implementations. A lost response to a write can leave its final outcome unknown; consult history before retrying it.

For browser cookie sessions, set `credentials: 'include'`. Unsafe calls obtain the current session's CSRF token through `/$/whoami`, or use a supplied `csrfToken` string/function. Basic credentials, bearer tokens and injected `fetch` are also supported. The typed administrative client shares the same authentication, credentials and cancellation handling.

```ts
const endpoint = SparklesClient.endpoint('https://example.org/sparql?tenant=a', {
  updateUrl: 'https://example.org/update?tenant=a',
  graphStoreUrl: 'https://example.org/data?tenant=a',
});
console.log(await endpoint.ask('ASK { ?s ?p ?o }'));
```

Plain endpoints preserve existing URL parameters and support protocol graph selection, BASE/PREFIX declarations and local cancellation. Sparkles-specific snapshots, inferred overlays, budgets, DESCRIBE overrides, history and commit messages throw `UnsupportedError`. A successful 204 Update carries no counts: the result sets both counts to zero with `countsReported: false`.
