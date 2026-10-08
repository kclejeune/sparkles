import createOpenApiClient from 'openapi-fetch';
import type { paths, components } from './openapi.js';
import type * as RDF from '@rdfjs/types';
import {
  Bindings,
  factory,
  InvalidInputError,
  UnsupportedError,
  NotFoundError,
  PermissionDeniedError,
  ConflictError,
  BudgetExceededError,
  SparqlSyntaxError,
  RdfSyntaxError,
  QueryTimeoutError,
  SparklesError,
  WriteRejectedError,
  receiptOf,
  unsigned,
  type QueryOptions,
  type UpdateOptions,
  type BindingsResult,
  type QuadsResult,
  type QueryResult,
  type UpdateResult,
  type CommitReceipt,
  type SparqlDataset,
} from '@sparkles-rdf/common';
import { jsonRows, parseNQuad } from './parsers.js';
import {
  BodyReader,
  checkSignal,
  controlled,
  delay,
  fetchControlled,
  operationSignal,
} from './transport.js';
export * from '@sparkles-rdf/common';
export type { paths, components };

export interface RemoteCommit {
  seq: bigint;
  parent?: bigint | null;
  inserted?: bigint;
  deleted?: bigint;
  quads?: bigint;
  [key: string]: unknown;
}
export interface RemoteCommitPage {
  dataset: string;
  datasetId: string;
  head: bigint;
  firstRetained: bigint;
  complete: boolean;
  oldestReconstructable?: bigint | null;
  commits: RemoteCommit[];
  next: string | null;
  [key: string]: unknown;
}

export interface ClientOptions {
  token?: string;
  basic?: { username: string; password: string };
  credentials?: RequestCredentials;
  fetch?: typeof fetch;
  csrfToken?: string | (() => string | undefined | Promise<string | undefined>);
  maxRetries?: number;
}
async function error(response: Response): Promise<Error> {
  let body: Record<string, any> = {};
  try {
    const json: unknown = await response.json();
    if (json && typeof json === 'object' && !Array.isArray(json)) body = json;
  } catch {
    /* Empty or non-JSON error body. */
  }
  const message =
    typeof body.detail === 'string'
      ? body.detail
      : typeof body.error === 'string'
        ? body.error
        : `${response.status} ${response.statusText}`;
  const details = {
    ...body,
    status: response.status,
    requestId: body.requestId ?? response.headers.get('x-request-id'),
  };
  const classes: Record<number, new (message: string, details?: Record<string, unknown>) => Error> =
    {
      401: PermissionDeniedError,
      403: PermissionDeniedError,
      404: NotFoundError,
      409: ConflictError,
      412: ConflictError,
      422: WriteRejectedError,
      408: QueryTimeoutError,
      504: QueryTimeoutError,
      507: BudgetExceededError,
    };
  if (response.status === 400)
    return body.line !== undefined
      ? new SparqlSyntaxError(message, details)
      : new InvalidInputError(message, details);
  return classes[response.status]
    ? new classes[response.status](message, details)
    : new SparklesError(message, body.code ?? 'ERR_SPARKLES_HTTP', details);
}
function auth(options: ClientOptions): Headers {
  const headers = new Headers();
  if (options.token) headers.set('authorization', 'Bearer ' + options.token);
  if (options.basic) {
    const bytes = new TextEncoder().encode(`${options.basic.username}:${options.basic.password}`);
    let text = '';
    for (const byte of bytes) text += String.fromCharCode(byte);
    headers.set('authorization', 'Basic ' + btoa(text));
  }
  return headers;
}
function httpUrl(value: string): URL {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new InvalidInputError('Expected an absolute HTTP URL');
  }
  if (!['http:', 'https:'].includes(url.protocol) || url.hash || url.username || url.password)
    throw new InvalidInputError('Expected an HTTP URL without credentials or a fragment');
  return url;
}
function absoluteIri(value: string): string {
  if (
    typeof value !== 'string' ||
    !/^[A-Za-z][A-Za-z0-9+.-]*:/.test(value) ||
    /[\u0000-\u0020<>"{}|\\^`]/u.test(value) ||
    /%(?![\da-f]{2})/i.test(value)
  )
    throw new InvalidInputError('Expected an absolute IRI');
  for (const c of value) {
    const n = c.codePointAt(0)!;
    if (n >= 0xd800 && n <= 0xdfff)
      throw new InvalidInputError('IRI contains an invalid Unicode scalar');
  }
  return value;
}
function withParams(path: string, params: URLSearchParams): string {
  const absolute = /^https?:\/\//.test(path);
  const url = new URL(path, 'http://sparkles.invalid');
  for (const key of new Set(params.keys())) url.searchParams.delete(key);
  for (const [key, value] of params) url.searchParams.append(key, value);
  return absolute ? url.href : url.pathname + url.search;
}
function prologue(text: string, options: QueryOptions | UpdateOptions): string {
  if (typeof text !== 'string') throw new InvalidInputError('SPARQL text must be a string');
  const lines: string[] = [];
  const base =
    'A-Za-z\\u00C0-\\u00D6\\u00D8-\\u00F6\\u00F8-\\u02FF\\u0370-\\u037D\\u037F-\\u1FFF\\u200C-\\u200D\\u2070-\\u218F\\u2C00-\\u2FEF\\u3001-\\uD7FF\\uF900-\\uFDCF\\uFDF0-\\uFFFD\\u{10000}-\\u{EFFFF}';
  const prefixName = new RegExp(
    `^[${base}](?:[${base}_0-9\\-\\u00b7\\u0300-\\u036f\\u203f\\u2040.]*[${base}_0-9\\-\\u00b7\\u0300-\\u036f\\u203f\\u2040])?$`,
    'u',
  );
  if (options.baseIri !== undefined) lines.push(`BASE <${absoluteIri(options.baseIri)}>`);
  for (const [prefix, iri] of Object.entries(options.prefixes ?? {})) {
    // SPARQL PN_PREFIX. Its final character cannot be a dot.
    if (prefix !== '' && !prefixName.test(prefix))
      throw new InvalidInputError('Invalid SPARQL prefix name');
    lines.push(`PREFIX ${prefix}: <${absoluteIri(iri)}>`);
  }
  return lines.length ? lines.join('\n') + '\n' + text : text;
}
function positive(value: number, name: string): string {
  if (!Number.isSafeInteger(value) || value <= 0)
    throw new InvalidInputError(`${name} must be a positive safe integer`);
  return unsigned(value, name);
}
function params(options: QueryOptions | UpdateOptions, sparkles: boolean): URLSearchParams {
  const p = new URLSearchParams();
  if (options.timeout !== undefined && sparkles && options.timeout > 0)
    p.set('timeout', String(options.timeout / 1000));
  for (const key of [
    'at',
    'includeInferred',
    'maxRows',
    'maxMemoryBytes',
    'maxRowsProduced',
    'describe',
    'noCache',
  ] as const) {
    if (key in options && (options as QueryOptions)[key] !== undefined && !sparkles)
      throw new UnsupportedError(`Plain SPARQL endpoints do not support ${key}`);
  }
  const q = options as QueryOptions;
  if (q.execution !== undefined) {
    if (!sparkles && q.execution !== 'eager')
      throw new InvalidInputError('Streaming execution requires a Sparkles endpoint');
    if (sparkles) p.set('execution', q.execution);
  }
  if (q.unionDefaultGraph !== undefined)
    throw new UnsupportedError('Remote queries cannot override the union default graph setting');
  if (q.noCache !== undefined) {
    if (typeof q.noCache !== 'boolean') throw new InvalidInputError('noCache must be boolean');
    p.set('nocache', String(q.noCache));
  }
  if (q.at !== undefined) {
    const at =
      q.at instanceof Date
        ? 'time:' + q.at.toISOString()
        : typeof q.at === 'string'
          ? q.at
          : 'commit:' + unsigned(q.at);
    p.set('at', at);
  }
  if (q.includeInferred !== undefined) p.set('reasoning', String(q.includeInferred));
  if (q.maxRows !== undefined) p.set('max-rows', positive(q.maxRows, 'maxRows'));
  if (q.maxRowsProduced !== undefined)
    p.set('max-rows-produced', positive(q.maxRowsProduced, 'maxRowsProduced'));
  if (q.maxMemoryBytes !== undefined) {
    positive(q.maxMemoryBytes, 'maxMemoryBytes');
    if (q.maxMemoryBytes % (1 << 20))
      throw new UnsupportedError('Remote memory budgets must be a whole number of MiB');
    p.set('memory-mb', String(q.maxMemoryBytes / (1 << 20)));
  }
  if (q.describe !== undefined) {
    if (typeof q.describe === 'string') p.set('describe', q.describe);
    else
      for (const [key, value] of Object.entries(q.describe)) {
        const parameter: Record<string, string> = {
          mode: 'describe',
          labels: 'describe-labels',
          reifiers: 'describe-reifiers',
          maxTriples: 'describe-max-triples',
          maxDepth: 'describe-max-depth',
        };
        if (!parameter[key]) throw new UnsupportedError(`Remote DESCRIBE does not support ${key}`);
        if (['labels', 'reifiers'].includes(key) && typeof value !== 'boolean')
          throw new InvalidInputError(`${key} must be boolean`);
        if (['maxTriples', 'maxDepth'].includes(key)) positive(value as number, key);
        if (key === 'mode' && typeof value !== 'string')
          throw new InvalidInputError('DESCRIBE mode must be a string');
        p.set(parameter[key], String(value));
      }
  }
  if (q.batchSize !== undefined || q.batchBytes !== undefined)
    throw new UnsupportedError('Remote results do not use native transfer batches');
  return p;
}
function writeHeaders(options: UpdateOptions, sparkles: boolean): Headers {
  if (options.dryRun) throw new UnsupportedError('Remote writes do not support dryRun');
  if (options.noWait !== undefined && typeof options.noWait !== 'boolean')
    throw new InvalidInputError('noWait must be boolean');
  if (options.noWait === true) throw new UnsupportedError('Remote writes do not support noWait');
  if (options.ifHead !== undefined)
    throw new UnsupportedError('Remote ifHead is unsupported; use graphs.put with an ETag');
  const h = new Headers();
  if (options.message !== undefined) {
    if (!sparkles)
      throw new UnsupportedError('Plain SPARQL endpoints do not support commit messages');
    if (typeof options.message !== 'string')
      throw new InvalidInputError('message must be a string');
    try {
      h.set('Sparkles-Commit-Message', "UTF-8''" + encodeURIComponent(options.message));
    } catch {
      throw new InvalidInputError('message contains an invalid Unicode scalar');
    }
  }
  return h;
}

export class SparklesClient {
  readonly api;
  private transport: typeof fetch;
  readonly baseUrl: string;
  /** The origins that receive the configured credentials: the server's and the endpoint URLs'. */
  private trusted = new Set<string>();
  constructor(
    baseUrl: string,
    private options: ClientOptions = {},
  ) {
    const base = httpUrl(baseUrl);
    if (base.search)
      throw new InvalidInputError(
        'The server base URL cannot contain query parameters; use endpoint() for a plain SPARQL URL',
      );
    if (options.basic && options.token)
      throw new InvalidInputError('Choose bearer or basic authentication');
    if (
      options.maxRetries !== undefined &&
      (!Number.isSafeInteger(options.maxRetries) ||
        options.maxRetries < 0 ||
        options.maxRetries > 10)
    )
      throw new InvalidInputError('maxRetries must be between 0 and 10');
    this.baseUrl = base.href.replace(/\/$/, '');
    this.trusted.add(base.origin);
    const fetcher = options.fetch ?? globalThis.fetch;
    this.transport = async (input, init) => {
      const requestInit: (RequestInit & { duplex?: 'half' }) | undefined =
        init?.body instanceof ReadableStream ? { ...init, duplex: 'half' } : init;
      const request = new Request(input, requestInit);
      checkSignal(request.signal);
      const headers = new Headers(request.headers);
      // Credentials go only to configured origins, never to an absolute URL that a server
      // response (such as a page's next link) or a caller supplied for another host.
      const trusted = this.trusted.has(new URL(request.url).origin);
      if (trusted) auth(options).forEach((v, k) => headers.set(k, v));
      if (
        trusted &&
        options.credentials === 'include' &&
        !['GET', 'HEAD', 'OPTIONS'].includes(request.method)
      ) {
        const provider = options.csrfToken;
        const supplied =
          typeof provider === 'function'
            ? await controlled(Promise.resolve().then(provider), request.signal)
            : provider;
        // Re-fetch rather than caching a session-bound token across login/logout.
        let token = supplied;
        if (token === undefined && new URL(request.url).origin === new URL(this.baseUrl).origin) {
          const who = await fetchControlled(
            fetcher,
            new Request(this.baseUrl + '/$/whoami', {
              credentials: 'include',
              headers: auth(options),
              signal: request.signal,
            }),
          );
          if (who.ok) token = ((await who.json()) as { csrfToken?: string }).csrfToken;
          else await who.body?.cancel();
        }
        if (token) headers.set('X-Sparkles-CSRF', token);
      }
      return fetchControlled(
        fetcher,
        new Request(request, { headers, credentials: options.credentials ?? request.credentials }),
      );
    };
    this.api = createOpenApiClient<paths>({ baseUrl: this.baseUrl, fetch: this.transport });
  }
  dataset(name: string, branch?: string): RemoteDataset {
    if (!name || /[\u0000-\u001f]/.test(name))
      throw new InvalidInputError('Dataset name is required');
    // A dot segment would normalize the request onto another path of the server.
    if (name === '.' || name === '..') throw new InvalidInputError('Invalid dataset name');
    if (branch !== undefined && !branch) throw new InvalidInputError('Branch name is required');
    return new RemoteDataset(this, name, branch);
  }
  static endpoint(
    url: string,
    options: ClientOptions & { updateUrl?: string; graphStoreUrl?: string } = {},
  ): RemoteDataset {
    const endpoint = httpUrl(url);
    const client = new SparklesClient(endpoint.origin, options);
    if (options.updateUrl) client.trusted.add(httpUrl(options.updateUrl).origin);
    if (options.graphStoreUrl) client.trusted.add(httpUrl(options.graphStoreUrl).origin);
    return new RemoteDataset(
      client,
      undefined,
      undefined,
      endpoint.href,
      options.updateUrl,
      options.graphStoreUrl,
    );
  }
  async request(path: string, init: RequestInit = {}, safe = false): Promise<Response> {
    const url = /^https?:\/\//.test(path) ? path : this.baseUrl + path;
    const retries = safe ? (this.options.maxRetries ?? 3) : 0;
    for (let attempt = 0; ; attempt++) {
      checkSignal(init.signal);
      const response = await this.transport(url, init);
      if (safe && [429, 502, 503, 504].includes(response.status) && attempt < retries) {
        const after = response.headers.get('retry-after');
        const wait = after
          ? /^\d+$/.test(after)
            ? Number(after) * 1000
            : Math.max(0, Date.parse(after) - Date.now())
          : Math.random() * (250 * 2 ** attempt);
        if (!Number.isFinite(wait) || wait > 60_000) {
          const failure = await error(response);
          checkSignal(init.signal);
          throw failure;
        }
        await response.body?.cancel();
        await delay(wait, init.signal);
        continue;
      }
      if (!response.ok && response.status !== 304) {
        const failure = await error(response);
        checkSignal(init.signal);
        throw failure;
      }
      return response;
    }
  }
}

class RemoteResult<T> implements AsyncIterable<T>, AsyncIterator<T> {
  private stopped = false;
  private queue: Promise<unknown> = Promise.resolve();
  private closing?: Promise<void>;
  constructor(
    readonly type: 'bindings' | 'quads',
    private iterator: AsyncIterator<T>,
    private reader: BodyReader,
    readonly variables: RDF.Variable[] = [],
  ) {}
  [Symbol.asyncIterator]() {
    return this;
  }
  next(): Promise<IteratorResult<T>> {
    const next = this.queue.then(async () => {
      if (this.stopped) return { done: true, value: undefined } as IteratorResult<T>;
      try {
        const result = await this.iterator.next();
        if (this.stopped || result.done) {
          await this.close();
          return { done: true, value: undefined } as IteratorResult<T>;
        }
        return result;
      } catch (e) {
        const closed = this.stopped;
        await this.close();
        if (closed) return { done: true, value: undefined } as IteratorResult<T>;
        throw e;
      }
    });
    this.queue = next.catch(() => {});
    return next;
  }
  async return(): Promise<IteratorResult<T>> {
    await this.close();
    return { done: true, value: undefined };
  }
  close(): Promise<void> {
    if (this.closing) return this.closing;
    this.stopped = true;
    return (this.closing = (async () => {
      await this.reader.cancel();
      try {
        await this.iterator.return?.();
      } finally {
        this.reader.release();
      }
    })());
  }
  async toArray() {
    const rows: T[] = [];
    for await (const row of this) rows.push(row);
    return rows;
  }
  async [Symbol.asyncDispose]() {
    await this.close();
  }
}

export class RemoteDataset implements SparqlDataset {
  readonly graphs;
  constructor(
    private client: SparklesClient,
    readonly name?: string,
    readonly branch?: string,
    private queryUrl?: string,
    private updateUrl?: string,
    private gspUrl?: string,
  ) {
    this.graphs = {
      get: (graph?: RDF.Term, options: QueryOptions = {}) =>
        this.graph('GET', graph, undefined, {}, options),
      put: (
        graph: RDF.Term | undefined,
        data: BodyInit,
        options: UpdateOptions & { contentType?: string; ifMatch?: string } = {},
      ) => this.graph('PUT', graph, data, options, options),
      post: (
        graph: RDF.Term | undefined,
        data: BodyInit,
        options: UpdateOptions & { contentType?: string } = {},
      ) => this.graph('POST', graph, data, options, options),
      delete: (graph?: RDF.Term, options: UpdateOptions = {}) =>
        this.graph('DELETE', graph, undefined, {}, options),
    };
  }
  private path(endpoint: string): string {
    if (this.name === undefined) {
      const url =
        endpoint === 'sparql'
          ? this.queryUrl
          : endpoint === 'update'
            ? this.updateUrl
            : this.gspUrl;
      if (!url) throw new UnsupportedError(`Plain endpoint has no ${endpoint} URL`);
      return url;
    }
    return `/${encodeURIComponent(this.name)}/${endpoint}${this.branch ? '?branch=' + encodeURIComponent(this.branch) : ''}`;
  }
  async query(text: string, options: QueryOptions = {}): Promise<QueryResult> {
    if (options.bindings !== undefined)
      throw new UnsupportedError('Remote pre-bound variables require stored-query parameters');
    const signal = operationSignal(options),
      p = params(options, this.name !== undefined);
    for (const graph of options.defaultGraph ?? [])
      p.append('default-graph-uri', absoluteIri(graph));
    for (const graph of options.namedGraphs ?? []) p.append('named-graph-uri', absoluteIri(graph));
    const response = await this.client.request(
      withParams(this.path('sparql'), p),
      {
        signal,
        method: 'POST',
        headers: {
          'content-type': 'application/sparql-query',
          accept: 'application/sparql-results+json, application/n-quads;q=0.9',
        },
        body: prologue(text, options),
      },
      true,
    );
    const contentType = response.headers.get('content-type')?.split(';')[0].trim().toLowerCase();
    if (!response.body) throw new RdfSyntaxError('Empty SPARQL response');
    if (contentType === 'application/sparql-results+json' || contentType === 'application/json') {
      const reader = new BodyReader(response.body, signal),
        variables: RDF.Variable[] = [];
      const iterator = jsonRows(reader, variables, options.factory ?? factory);
      try {
        // Prime through the head to one row. ASK is fully validated before return.
        const first = await iterator.next();
        if (first.done && typeof first.value === 'boolean') {
          await reader.cancel();
          reader.release();
          return { type: 'boolean', value: first.value };
        }
        const feed = (async function* () {
          if (!first.done) yield first.value;
          yield* iterator;
        })();
        return new RemoteResult<Bindings>('bindings', feed, reader, variables) as BindingsResult;
      } catch (e) {
        await reader.cancel();
        reader.release();
        throw e;
      }
    }
    if (contentType !== 'application/n-quads' && contentType !== 'application/n-triples') {
      await response.body.cancel();
      throw new UnsupportedError(
        `Expected SPARQL JSON or N-Quads, received ${contentType ?? 'no content type'}`,
      );
    }
    return this.rdfResult(response, options, signal);
  }
  private rdfResult(response: Response, options: QueryOptions, signal?: AbortSignal): QuadsResult {
    const reader = new BodyReader(response.body!, signal),
      f = options.factory ?? factory;
    const iterator = (async function* () {
      const decoder = new TextDecoder('utf-8', { fatal: true });
      let text = '';
      while (true) {
        const r = await reader.read();
        try {
          text += decoder.decode(r.value, { stream: !r.done });
        } catch {
          throw new RdfSyntaxError('Invalid UTF-8 in N-Quads');
        }
        let newline: number;
        while ((newline = text.indexOf('\n')) >= 0) {
          if (newline > 64 << 20) throw new BudgetExceededError('N-Quads line exceeds 64 MiB');
          const line = text.slice(0, newline);
          text = text.slice(newline + 1);
          const quad = parseNQuad(line, f);
          if (quad) yield quad;
        }
        if (text.length > 64 << 20) throw new BudgetExceededError('N-Quads line exceeds 64 MiB');
        if (r.done) {
          const quad = parseNQuad(text, f);
          if (quad) yield quad;
          break;
        }
      }
    })();
    return new RemoteResult<RDF.Quad>('quads', iterator, reader) as QuadsResult;
  }
  async select(text: string, options: QueryOptions = {}) {
    const r = await this.query(text, options);
    if (r.type !== 'bindings') {
      if ('close' in r) await r.close();
      throw new InvalidInputError('Expected SELECT result');
    }
    return r;
  }
  async ask(text: string, options: QueryOptions = {}) {
    const r = await this.query(text, options);
    if (r.type !== 'boolean') {
      await r.close();
      throw new InvalidInputError('Expected ASK result');
    }
    return r.value;
  }
  async construct(text: string, options: QueryOptions = {}) {
    const r = await this.query(text, options);
    if (r.type !== 'quads') {
      if ('close' in r) await r.close();
      throw new InvalidInputError('Expected graph result');
    }
    return r;
  }
  async update(text: string, options: UpdateOptions = {}): Promise<UpdateResult> {
    const signal = operationSignal(options),
      p = params(options, this.name !== undefined),
      headers = writeHeaders(options, this.name !== undefined);
    if (this.name !== undefined) p.set('receipt', 'true');
    headers.set('content-type', 'application/sparql-update');
    headers.set('accept', 'application/json');
    const response = await this.client.request(withParams(this.path('update'), p), {
      signal,
      method: 'POST',
      headers,
      body: prologue(text, options),
    });
    if (response.status === 204) return { inserted: 0n, deleted: 0n, countsReported: false };
    const body = (await response.json()) as any;
    const receipt = receiptOf(body);
    return {
      ...body,
      inserted: BigInt(unsigned(body.inserted ?? receipt?.commit.inserted ?? 0, 'Remote inserted')),
      deleted: BigInt(unsigned(body.deleted ?? receipt?.commit.deleted ?? 0, 'Remote deleted')),
      receipt,
    };
  }
  async batch(
    callback: (batch: { update(text: string): void }) => void,
    options: UpdateOptions = {},
  ) {
    if (options.dryRun) throw new UnsupportedError('Remote writes do not support dryRun');
    const updates: string[] = [];
    const result: unknown = callback({ update: (text) => updates.push(text) });
    if (result && typeof (result as PromiseLike<unknown>).then === 'function') {
      void Promise.resolve(result).catch(() => {});
      throw new UnsupportedError('Remote batch callbacks must be synchronous');
    }
    if (!updates.length) throw new InvalidInputError('A batch needs at least one update');
    return this.update(updates.join(';\n'), options);
  }
  private graph(
    method: 'GET',
    graph: RDF.Term | undefined,
    body: undefined,
    metadata: {},
    options: QueryOptions,
  ): Promise<Response>;
  private graph(
    method: 'PUT' | 'POST' | 'DELETE',
    graph: RDF.Term | undefined,
    body: BodyInit | undefined,
    metadata: { contentType?: string; ifMatch?: string },
    options: UpdateOptions,
  ): Promise<CommitReceipt | undefined>;
  private async graph(
    method: string,
    graph: RDF.Term | undefined,
    body: BodyInit | undefined,
    metadata: { contentType?: string; ifMatch?: string },
    options: QueryOptions | UpdateOptions,
  ) {
    const q = options as QueryOptions;
    for (const key of [
      'bindings',
      'defaultGraph',
      'namedGraphs',
      'describe',
      'batchSize',
      'batchBytes',
      'includeInferred',
      'maxRows',
      'maxRowsProduced',
      'maxMemoryBytes',
      'factory',
      'noCache',
      'unionDefaultGraph',
    ] as const)
      if (q[key] !== undefined) throw new UnsupportedError(`Graph Store does not support ${key}`);
    if (options.baseIri !== undefined || options.prefixes !== undefined)
      throw new UnsupportedError('Graph Store data must carry its own base IRI and prefixes');
    const signal = operationSignal(options),
      p = params(options, this.name !== undefined);
    if (!graph || graph.termType === 'DefaultGraph') p.set('default', '');
    else {
      if (graph.termType !== 'NamedNode')
        throw new InvalidInputError('A graph must be a NamedNode or DefaultGraph');
      p.set('graph', absoluteIri(graph.value));
    }
    const headers =
      method === 'GET'
        ? new Headers()
        : writeHeaders(options as UpdateOptions, this.name !== undefined);
    if (method !== 'GET' && this.name !== undefined) p.set('receipt', 'true');
    if (metadata.contentType) headers.set('content-type', metadata.contentType);
    if (metadata.ifMatch) headers.set('if-match', metadata.ifMatch);
    if (method === 'GET') headers.set('accept', 'application/n-quads, application/n-triples;q=0.9');
    const response = await this.client.request(
      withParams(this.path('data'), p),
      { signal, method, body, headers },
      method === 'GET',
    );
    if (method === 'GET') return response;
    return response.status === 204 ? undefined : receiptOf(await response.json());
  }
  async commits(
    options: {
      limit?: number;
      before?: number | bigint;
      after?: number | bigint;
      signal?: AbortSignal;
    } = {},
  ) {
    if (this.name === undefined) throw new UnsupportedError('Plain endpoint has no history');
    if (options.before !== undefined && options.after !== undefined)
      throw new InvalidInputError('Choose before or after');
    const p = new URLSearchParams({ limit: positive(options.limit ?? 100, 'limit') });
    if (options.limit !== undefined && options.limit > 1000)
      throw new InvalidInputError('Commit page limit must not exceed 1000');
    if (this.branch !== undefined) p.set('branch', this.branch);
    if (options.before !== undefined) p.set('before', unsigned(options.before));
    if (options.after !== undefined) p.set('after', unsigned(options.after));
    const body = (await (
      await this.client.request(
        withParams(`/\$/commits/${encodeURIComponent(this.name)}`, p),
        { signal: options.signal },
        true,
      )
    ).json()) as Record<string, any>;
    const integerFields = (value: Record<string, any>, fields: string[]) => {
      const out = { ...value };
      for (const field of fields)
        if (out[field] != null) out[field] = BigInt(unsigned(out[field], `Remote ${field}`));
      return out;
    };
    const page = integerFields(body, ['head', 'firstRetained', 'oldestReconstructable']);
    if (!Array.isArray(body.commits)) throw new RdfSyntaxError('Invalid commit page');
    page.commits = body.commits.map((commit: Record<string, any>) => {
      const value = integerFields(commit, ['seq', 'parent', 'inserted', 'deleted', 'quads']);
      for (const field of ['mergedFrom', 'replayedFrom'])
        if (value[field]) value[field] = integerFields(value[field], ['seq']);
      if (Array.isArray(value.parents))
        value.parents = value.parents.map((parent: Record<string, any>) =>
          integerFields(parent, ['seq']),
        );
      return value;
    });
    if (this.branch !== undefined && typeof page.next === 'string')
      page.next = withParams(page.next, new URLSearchParams({ branch: this.branch }));
    return page as RemoteCommitPage;
  }
}
