import { EventEmitter } from 'node:events';
import {
  Administration,
  adminResult,
  type AdminCall,
  type DatasetInfo,
  type Configuration,
  type CommitChanges,
} from './admin.js';
export * from './admin.js';
export * from './utilities.js';
export type { BackupPolicy, PolicyRun, RetentionReport, RetentionBackup } from './policy.js';
import { policyResult, type BackupPolicy, type PolicyRun, type RetentionReport } from './policy.js';
import { AsyncLocalStorage } from 'node:async_hooks';
import { Readable } from 'node:stream';
import type * as RDF from '@rdfjs/types';
import {
  Bindings,
  LazyBindings,
  factory,
  encodeTerm,
  decodeTerm,
  nativeError,
  InvalidInputError,
  UnsupportedError,
  CancelledError,
  BudgetExceededError,
  ConflictError,
  QueryTimeoutError,
  receiptOf,
  unsigned,
  type OperationOptions,
  type QueryOptions,
  type UpdateOptions,
  type UpdateResult,
  type CommitReceipt,
  type SparqlDataset,
  type QueryResult,
  type BindingsResult,
  type QuadsResult,
} from '@sparkles-rdf/common';
export * from '@sparkles-rdf/common';

import { native } from './native.js';
const transactionContext = new AsyncLocalStorage<ReadonlySet<string>>();
const defaults = { batchSize: 1024, batchBytes: 1 << 20 };
export function configure(options: { batchSize?: number; batchBytes?: number }) {
  for (const key of ['batchSize', 'batchBytes'] as const)
    if (options[key] !== undefined) {
      if (!Number.isSafeInteger(options[key]) || options[key]! <= 0)
        throw new InvalidInputError(`${key} must be positive`);
      defaults[key] = options[key]!;
    }
}
export interface DatasetOptions {
  readOnly?: boolean;
  unionDefaultGraph?: boolean;
  cacheBytes?: number;
}
export interface LoadOptions extends UpdateOptions {
  format?: string;
  baseIri?: string;
  toGraph?: RDF.NamedNode;
  compression?: string;
  lenient?: boolean;
}
export interface DumpOptions extends OperationOptions {
  format?: string;
  fromGraph?: RDF.NamedNode;
  compression?: string;
}
import type { RdfInput } from './rdf.js';
export type { RdfInput } from './rdf.js';
function wireOptions(options: QueryOptions | UpdateOptions = {}) {
  const { signal, ...rest } = options;
  const object: any = { ...rest };
  delete object.factory;
  if ('bindings' in options && options.bindings)
    object.bindings = Object.fromEntries(
      options.bindings instanceof Bindings
        ? Array.from(options.bindings, ([k, v]) => [k.value, encodeTerm(v)])
        : Object.entries(options.bindings).map(([k, v]) => [k, encodeTerm(v)]),
    );
  if ('ifHead' in options && options.ifHead !== undefined)
    object.ifHead = unsigned(options.ifHead, 'ifHead');
  if ('at' in options && options.at !== undefined)
    object.at =
      options.at instanceof Date
        ? 'time:' + options.at.toISOString()
        : typeof options.at === 'number' || typeof options.at === 'bigint'
          ? 'commit:' + unsigned(options.at, 'at')
          : options.at;
  if (
    options.timeout !== undefined &&
    (!Number.isSafeInteger(options.timeout) || options.timeout < 0)
  )
    throw new InvalidInputError('timeout must be a nonnegative integer');
  return JSON.stringify(object);
}
function updateResult(text: string): UpdateResult {
  const v = JSON.parse(text);
  return {
    ...v,
    inserted: BigInt(v.inserted),
    deleted: BigInt(v.deleted),
    receipt: receiptOf(v.receipt),
  };
}
const pattern = (
  s?: RDF.Term | null,
  p?: RDF.Term | null,
  o?: RDF.Term | null,
  g?: RDF.Term | null,
) => JSON.stringify([s, p, o, g].map((v) => (v == null ? null : encodeTerm(v))));

class PullResult<T> implements AsyncIterable<T>, AsyncIterator<T> {
  readonly type: 'bindings' | 'quads';
  readonly variables: RDF.Variable[];
  readonly size?: number;
  readonly timing: unknown;
  readonly plan: unknown;
  private batch: T[] = [];
  private closed = false;
  private queue: Promise<unknown> = Promise.resolve();
  private abort = () => {
    void this.close();
  };
  constructor(
    private handle: any,
    private options: QueryOptions,
    private onClose: () => void = () => {},
  ) {
    const info = JSON.parse(handle.info());
    this.type = info.type;
    this.variables = (info.variables ?? []).map(factory.variable!);
    this.size = info.size;
    this.timing = info.timing;
    this.plan = info.plan;
    options.signal?.addEventListener('abort', this.abort, { once: true });
    if (options.signal?.aborted) this.abort();
  }
  [Symbol.asyncIterator]() {
    return this;
  }
  next(): Promise<IteratorResult<T>> {
    const task = this.queue.then(async () => {
      this.options.signal?.throwIfAborted();
      if (this.closed) return { done: true, value: undefined } as IteratorResult<T>;
      if (!this.batch.length) {
        let value: string;
        try {
          value = await this.handle.nextBatch(
            this.options.batchSize ?? defaults.batchSize,
            this.options.batchBytes ?? defaults.batchBytes,
          );
        } catch (e) {
          await this.close();
          throw nativeError(e);
        }
        this.options.signal?.throwIfAborted();
        if (this.closed) return { done: true, value: undefined } as IteratorResult<T>;
        const batch = JSON.parse(value);
        if (!batch) {
          await this.close();
          return { done: true, value: undefined } as IteratorResult<T>;
        }
        const cache: (RDF.Term | undefined)[] = [];
        const decode = (i: number) =>
          (cache[i] ??= decodeTerm(batch.terms[i], this.options.factory));
        this.batch = batch.rows.map((cells: number[]) =>
          this.type === 'bindings'
            ? new LazyBindings(
                this.variables.map((v) => v.value),
                cells,
                decode,
              )
            : decode(cells[0]),
        ) as T[];
      }
      return { done: false, value: this.batch.shift()! } as IteratorResult<T>;
    });
    this.queue = task.catch(() => {});
    return task;
  }
  async return(): Promise<IteratorResult<T>> {
    await this.close();
    return { done: true, value: undefined };
  }
  async close() {
    if (this.closed) return;
    this.closed = true;
    this.batch = [];
    this.options.signal?.removeEventListener('abort', this.abort);
    try {
      await this.handle.close();
    } finally {
      this.onClose();
    }
  }
  async [Symbol.asyncDispose]() {
    await this.close();
  }
  async toArray() {
    const result: T[] = [];
    for await (const value of this) result.push(value);
    return result;
  }
  toStream(): Readable {
    return Readable.from(this);
  }
}

abstract class Queryable implements SparqlDataset {
  protected abstract read(text: string, options: QueryOptions): Promise<any>;
  abstract update(text: string, options?: UpdateOptions): Promise<UpdateResult>;
  protected result(handle: any, options: QueryOptions): QueryResult {
    const info = JSON.parse(handle.info());
    if (info.type === 'boolean') {
      handle.close();
      return { type: 'boolean', value: info.value, plan: info.plan, timing: info.timing };
    }
    return new PullResult(handle, options) as unknown as BindingsResult | QuadsResult;
  }
  async query(text: string, options: QueryOptions = {}) {
    return this.result(await this.read(text, options), options);
  }
  async select(text: string, options: QueryOptions = {}): Promise<BindingsResult> {
    const r = await this.query(text, options);
    if (r.type !== 'bindings') {
      if ('close' in r) await r.close();
      throw new InvalidInputError('Expected SELECT query');
    }
    return r;
  }
  async ask(text: string, options: QueryOptions = {}) {
    const r = await this.query(text, options);
    if (r.type !== 'boolean') {
      await r.close();
      throw new InvalidInputError('Expected ASK query');
    }
    return r.value;
  }
  async construct(text: string, options: QueryOptions = {}): Promise<QuadsResult> {
    const r = await this.query(text, options);
    if (r.type !== 'quads') {
      if ('close' in r) await r.close();
      throw new InvalidInputError('Expected CONSTRUCT or DESCRIBE query');
    }
    return r;
  }
  async queryBindings(text: string, options: QueryOptions = {}) {
    return Readable.from(await this.select(text, options));
  }
  async queryQuads(text: string, options: QueryOptions = {}) {
    return Readable.from(await this.construct(text, options));
  }
  queryBoolean(text: string, options: QueryOptions = {}) {
    return this.ask(text, options);
  }
  async queryVoid(text: string, options: UpdateOptions = {}) {
    await this.update(text, options);
  }
}

export class Dataset extends Queryable {
  readonly id: string;
  private closing = false;
  private closed = false;
  private closePromise?: Promise<void>;
  private operations = new Set<Promise<unknown>>();
  private tokens = new Set<any>();
  private transactions = new Set<Transaction>();
  private results = new Set<{ close(): Promise<void> }>();
  private family: string;
  private administration: Administration;
  private constructor(
    private handle: any,
    public readonly path: string | null,
    public readonly options: DatasetOptions,
    family?: string,
  ) {
    super();
    this.id = handle.identity();
    this.family = family ?? this.id;
    this.administration = new Administration(
      this.admin.bind(this),
      (name) => this.branch(name),
      async (name, params, options) =>
        this.result(
          await this.run<any>(options, (cancel) =>
            this.handle.runQuery(
              name,
              JSON.stringify(params),
              options.version === undefined ? undefined : unsigned(options.version),
              wireOptions(options),
              cancel,
            ),
          ),
          options,
        ),
    );
  }
  /** @internal Native handles are never shared across JavaScript environments. */
  static fromNative(handle: any, path: string | null, options: DatasetOptions) {
    return new Dataset(handle, handle.path() ?? path, options);
  }
  get prefixes() {
    return this.administration.prefixes;
  }
  get snapshots() {
    return this.administration.snapshots;
  }
  get history() {
    return this.administration.history;
  }
  get settings() {
    return this.administration.settings;
  }
  get queries() {
    return this.administration.queries;
  }
  get branches() {
    return this.administration.branches;
  }
  get indexes() {
    return this.administration.indexes;
  }
  get reasoning() {
    return this.administration.reasoning;
  }
  get validation() {
    return this.administration.validation;
  }
  private async admin<T>(
    op: string,
    args: Configuration = {},
    options: UpdateOptions = {},
    lock = true,
  ): Promise<T> {
    if (options.dryRun)
      throw new UnsupportedError(
        'Administration dryRun is unsupported; use branch previews or update dryRun',
      );
    if (options.ifHead !== undefined && op !== 'applyPatch')
      throw new UnsupportedError('ifHead is unsupported for this administration operation');
    const value = JSON.parse(
      await this.run<string>(
        options,
        (cancel) =>
          this.handle.admin(
            op,
            JSON.stringify(args, (_, v) => (typeof v === 'bigint' ? v.toString() : v)),
            wireOptions(options),
            cancel,
          ),
        lock,
      ),
    );
    if (op === 'graphql.execute') return value as T;
    if (op === 'history.prune' || op === 'reasoning.clear') return BigInt(value) as T;
    return adminResult(value) as T;
  }

  info(options: OperationOptions = {}) {
    return this.admin<DatasetInfo>('info', {}, options, false);
  }
  async branch(name: string) {
    return new Dataset(
      await this.run<any>({}, () => this.handle.branch(name)),
      this.path,
      this.options,
      this.family,
    );
  }
  stats(options: OperationOptions & { at?: string } = {}) {
    return this.admin<Configuration>('stats', { at: options.at }, options, false);
  }
  explain(query: string, options: QueryOptions = {}) {
    return this.admin<{ text: string; plan: Configuration }>('explain', { query }, options, false);
  }
  clearCache() {
    return this.admin<void>('clearCache', {}, {}, false);
  }
  applyPatch(patch: string, options: UpdateOptions = {}) {
    return this.admin<UpdateResult>('applyPatch', { patch }, options, true);
  }
  get schema() {
    const call = this.admin.bind(this);
    return {
      classes: (o: OperationOptions & { at?: string; cursor?: string; limit?: number } = {}) =>
        call<Configuration>('schema.classes', { ...o }, o, false),
      predicates: (o: OperationOptions & { at?: string; cursor?: string; limit?: number } = {}) =>
        call<Configuration>('schema.predicates', { ...o }, o, false),
      diff: (from: string, o: OperationOptions & { at?: string; limit?: number } = {}) =>
        call<Configuration>('schema.diff', { from, ...o }, o, false),
      report: (o: OperationOptions & { limit?: number } = {}) =>
        call<Configuration>('schema.report', { ...o }, o, false),
      profiles: (o: OperationOptions & { classes?: string[] } = {}) =>
        call<Configuration>('schema.profiles', { ...o }, o, false),
      draftShapes: (o: OperationOptions & { support?: number } = {}) =>
        call<Configuration>('schema.draftShapes', { ...o }, o, false),
      constraints: (o?: OperationOptions) =>
        call<Configuration>('schema.constraints', {}, o, false),
    };
  }
  get graphql() {
    const call = this.admin.bind(this);
    return {
      get: (version?: number | bigint, o?: OperationOptions) =>
        call<Configuration | null>(
          'graphql.get',
          { version: version === undefined ? undefined : unsigned(version) },
          o,
          false,
        ),
      versions: (o?: OperationOptions) => call<Configuration[]>('graphql.versions', {}, o, false),
      sdl: (o?: OperationOptions) => call<string | null>('graphql.sdl', {}, o, false),
      put: (config: Configuration, o?: UpdateOptions) =>
        call<Configuration>('graphql.put', { config }, o, true),
      reset: (o: UpdateOptions & { ifVersion?: number | bigint } = {}) =>
        call<boolean>(
          'graphql.reset',
          { ifVersion: o.ifVersion === undefined ? undefined : unsigned(o.ifVersion) },
          o,
          true,
        ),
      execute: (query: string, variables: Configuration = {}, o?: QueryOptions) =>
        call<Configuration>('graphql.execute', { query, variables }, o, true),
      draft: (name = 'dataset', o?: OperationOptions) =>
        call<Configuration>('graphql.draft', { name }, o, false),
    };
  }
  compact(options: UpdateOptions = {}) {
    return this.admin<void>('compact', {}, options, true);
  }
  async cloneToMemory(name = 'clone', options: QueryOptions & { graphs?: string[] } = {}) {
    if ('ifHead' in options || 'dryRun' in options)
      throw new UnsupportedError(
        'Clone accepts read controls and historical at; conditional writes are unsupported',
      );
    return Dataset.fromNative(
      await this.run<any>(
        options,
        (cancel) => this.handle.cloneMemory(name, wireOptions(options), cancel),
        true,
      ),
      null,
      this.options,
    );
  }
  cloneTo(path: string, options: UpdateOptions = {}) {
    return this.admin<void>('cloneTo', { path }, options, true);
  }
  backup(path: string, options: UpdateOptions = {}) {
    return this.admin<string>('backup', { path }, options, true);
  }
  backups(repository: Repository) {
    const ds = this;
    const call = async <T>(
      op: string,
      args: Configuration = {},
      o: UpdateOptions = {},
    ): Promise<T> => {
      if (o.dryRun || o.ifHead !== undefined)
        throw new UnsupportedError('Backup operations do not support dryRun or ifHead');
      const value = await ds.run<string>(
        o,
        (cancel) =>
          ds.handle.backups(
            repository.nativeHandle(),
            op,
            JSON.stringify(args),
            wireOptions(o),
            cancel,
          ),
        !['list', 'get', 'verify'].includes(op) || op === 'create',
      );
      return adminResult(JSON.parse(value));
    };
    return {
      create: (
        name: string,
        options: UpdateOptions & { note?: string; datasetName?: string } = {},
      ) => call<Configuration>('create', { name, ...options }, options),
      list: (o?: OperationOptions) => call<Configuration[]>('list', {}, o),
      get: (name: string, o?: OperationOptions) => call<Configuration | null>('get', { name }, o),
      delete: (name: string, o?: UpdateOptions) => call<boolean>('delete', { name }, o),
      verify: (name: string, o?: OperationOptions) => call<Configuration>('verify', { name }, o),
    };
  }
  source(): RDF.Source & {
    countQuads(
      s?: RDF.Term | null,
      p?: RDF.Term | null,
      o?: RDF.Term | null,
      g?: RDF.Term | null,
    ): Promise<number>;
  } {
    const ds = this;
    return {
      match(s, p, o, g) {
        return ds.match(s, p, o, g).toStream() as RDF.Stream;
      },
      async countQuads(s, p, o, g) {
        const n = await ds.countQuads(s, p, o, g);
        if (n > BigInt(Number.MAX_SAFE_INTEGER))
          throw new InvalidInputError('RDF/JS count exceeds safe integer range');
        return Number(n);
      },
    };
  }
  async countQuads(
    s?: RDF.Term | null,
    p?: RDF.Term | null,
    o?: RDF.Term | null,
    g?: RDF.Term | null,
  ) {
    return BigInt(await this.run<string>({}, () => this.handle.countPattern(pattern(s, p, o, g))));
  }
  store(): RDF.Store & {
    countQuads(
      s?: RDF.Term | null,
      p?: RDF.Term | null,
      o?: RDF.Term | null,
      g?: RDF.Term | null,
    ): Promise<number>;
  } {
    const ds = this;
    const completed = (f: () => Promise<unknown>) => {
      const events = new EventEmitter();
      queueMicrotask(() => {
        void f().then(
          () => events.emit('end'),
          (error) => events.emit('error', error),
        );
      });
      return events;
    };
    return {
      ...this.source(),
      import: (stream: RDF.Stream) =>
        completed(() => ds.addAll(stream as unknown as AsyncIterable<RDF.Quad>)),
      remove: (stream: RDF.Stream) =>
        completed(() =>
          ds.transaction(async (tx) => {
            for await (const q of stream as unknown as AsyncIterable<RDF.Quad>) await tx.delete(q);
          }),
        ),
      removeMatches: (s, p, o, g) =>
        completed(() =>
          ds.transaction(async (tx) => {
            for await (const q of tx.match(s, p, o, g)) await tx.delete(q);
          }),
        ),
      deleteGraph: (graph) =>
        completed(() =>
          ds.clearGraph(typeof graph === 'string' ? factory.namedNode(graph) : graph),
        ),
    };
  }
  waitForCommit(after: number | bigint, options: OperationOptions = {}) {
    return this.run<string>(options, (cancel) =>
      this.handle.waitCommit(unsigned(after), cancel),
    ).then(BigInt);
  }
  changes(
    options: OperationOptions & {
      after?: number | bigint;
      maxCommits?: number;
      maxQuads?: number;
    } = {},
  ) {
    const ds = this;
    ds.check();
    const controller = new AbortController();
    let closed = false;
    let ending: Promise<void> | undefined;
    const signal = options.signal
      ? AbortSignal.any([options.signal, controller.signal])
      : controller.signal;
    const owner = { close: () => close() };
    this.results.add(owner);
    const generator: AsyncGenerator<CommitChanges, void, unknown> = (async function* () {
      let after = BigInt(unsigned(options.after ?? 0));
      try {
        while (true) {
          signal.throwIfAborted();
          const page = await ds.history.changes(after, { ...options, signal });
          for (const commit of page.commits) {
            if (!commit.complete)
              throw new BudgetExceededError(
                'Commit exceeds the change-feed quad limit; increase maxQuads',
                { commit: commit.commit.seq, after, maxQuads: options.maxQuads ?? 10000 },
              );
            signal.throwIfAborted();
            yield commit;
          }
          after = page.next;
          if (page.commits.length === 0) await ds.waitForCommit(after, { ...options, signal });
        }
      } finally {
        ds.results.delete(owner);
      }
    })();
    const close = () =>
      (ending ??= (async () => {
        closed = true;
        controller.abort(new CancelledError('Change feed closed'));
        try {
          await generator.return(undefined);
        } finally {
          ds.results.delete(owner);
        }
      })());
    return {
      [Symbol.asyncIterator]() {
        return this;
      },
      async next() {
        if (closed) return { done: true, value: undefined } as const;
        try {
          return await generator.next();
        } catch (e) {
          if (closed) return { done: true, value: undefined } as const;
          throw e;
        }
      },
      async return() {
        await close();
        return { done: true, value: undefined } as const;
      },
      close,
      async [Symbol.asyncDispose]() {
        await close();
      },
    };
  }

  static memory(options: DatasetOptions = {}) {
    return new Dataset(native.NativeDataset.memory(JSON.stringify(options)), null, options);
  }
  static async open(path: string, options: DatasetOptions = {}) {
    try {
      return new Dataset(
        await native.NativeDataset.open(path, JSON.stringify(options)),
        path,
        options,
      );
    } catch (e) {
      throw nativeError(e);
    }
  }
  check(write = false) {
    if (this.closing || this.closed) throw new InvalidInputError('Dataset is closed');
    if (write && transactionContext.getStore()?.has(this.family))
      throw new ConflictError('Finish the open transaction or use tx for this operation');
  }
  run<T>(
    options: OperationOptions,
    operation: (cancel: any, signal: AbortSignal) => Promise<T>,
    write = false,
  ): Promise<T> {
    this.check(write);
    options.signal?.throwIfAborted();
    const cancel = new native.Cancellation();
    const controller = new AbortController();
    const owner = {
      cancel() {
        cancel.cancel();
        controller.abort(new InvalidInputError('Dataset is closing'));
      },
    };
    this.tokens.add(owner);
    const abort = () => {
      cancel.cancel();
      controller.abort(options.signal?.reason);
    };
    options.signal?.addEventListener('abort', abort, { once: true });
    let timedOut = false;
    const timer =
      options.timeout === undefined
        ? undefined
        : setTimeout(() => {
            timedOut = true;
            cancel.cancel();
            controller.abort(new QueryTimeoutError());
          }, options.timeout);
    const task = (async () => {
      try {
        const value = await operation(cancel, controller.signal);
        if (controller.signal.aborted) {
          const handle = value as any;
          if (handle && typeof handle.end === 'function') await handle.end(false);
          else if (handle && typeof handle.close === 'function') await handle.close();
          throw controller.signal.reason;
        }
        return value;
      } catch (e) {
        if (options.signal?.aborted) throw options.signal.reason;
        if (timedOut) throw new QueryTimeoutError();
        throw nativeError(e);
      } finally {
        if (timer) clearTimeout(timer);
        options.signal?.removeEventListener('abort', abort);
        this.tokens.delete(owner);
      }
    })();
    this.operations.add(task);
    void task.finally(() => this.operations.delete(task)).catch(() => {});
    return task;
  }
  protected read(text: string, options: QueryOptions) {
    return this.run(options, (cancel) => this.handle.query(text, wireOptions(options), cancel));
  }
  protected override result(handle: any, options: QueryOptions): QueryResult {
    const info = JSON.parse(handle.info());
    if (info.type === 'boolean') {
      handle.close();
      return { type: 'boolean', value: info.value, plan: info.plan, timing: info.timing };
    }
    const r = new PullResult<unknown>(handle, options, () => this.results.delete(r));
    this.results.add(r);
    return r as unknown as BindingsResult | QuadsResult;
  }
  async update(text: string, options: UpdateOptions = {}) {
    return updateResult(
      await this.run<string>(
        options,
        (cancel) => this.handle.update(text, wireOptions(options), cancel),
        true,
      ),
    );
  }
  async count() {
    return BigInt(await this.run<string>({}, () => this.handle.count()));
  }
  match(
    s?: RDF.Term | null,
    p?: RDF.Term | null,
    o?: RDF.Term | null,
    g?: RDF.Term | null,
  ): AsyncIterable<RDF.Quad> & { toArray(): Promise<RDF.Quad[]>; toStream(): Readable } {
    const future = this.run<any>({}, () => this.handle.matched(pattern(s, p, o, g))).then(
      (handle) => this.result(handle, {}) as QuadsResult,
    );
    return deferredQuads(future);
  }
  async has(quad: RDF.Quad) {
    for await (const _ of this.match(quad.subject, quad.predicate, quad.object, quad.graph))
      return true;
    return false;
  }
  async add(quad: RDF.Quad) {
    const { value } = await this.transaction(async (tx) => tx.add(quad));
    return value;
  }
  async delete(quad: RDF.Quad) {
    const { value } = await this.transaction(async (tx) => tx.delete(quad));
    return value;
  }
  async addAll(quads: Iterable<RDF.Quad> | AsyncIterable<RDF.Quad>, options: UpdateOptions = {}) {
    return this.transaction(async (tx) => {
      let inserted = 0n;
      let batch: RDF.Quad[] = [];
      for await (const q of quads) {
        batch.push(q);
        if (batch.length >= 4096) {
          inserted += await tx.addBatch(batch);
          batch = [];
        }
      }
      if (batch.length) inserted += await tx.addBatch(batch);
      return inserted;
    }, options).then(({ value, receipt }) => ({ inserted: value, receipt }));
  }
  async replace(quads: Iterable<RDF.Quad> | AsyncIterable<RDF.Quad>, options: UpdateOptions = {}) {
    const { value, receipt } = await this.transaction(async (tx) => {
      let deleted = 0n;
      for await (const q of tx.match()) if (await tx.delete(q)) deleted++;
      let inserted = 0n;
      let batch: RDF.Quad[] = [];
      for await (const q of quads) {
        batch.push(q);
        if (batch.length === 4096) {
          inserted += await tx.addBatch(batch);
          batch = [];
        }
      }
      if (batch.length) inserted += await tx.addBatch(batch);
      return { inserted, deleted };
    }, options);
    return { ...value, receipt };
  }
  async clearGraph(graph: RDF.Term) {
    let deleted = 0n;
    const { receipt } = await this.transaction(async (tx) => {
      for await (const q of tx.match(null, null, null, graph)) if (await tx.delete(q)) deleted++;
    });
    return { deleted, receipt };
  }
  async clear() {
    let deleted = 0n;
    const { receipt } = await this.transaction(async (tx) => {
      for await (const q of tx.match()) if (await tx.delete(q)) deleted++;
    });
    return { deleted, receipt };
  }
  async graphs() {
    const result = new Map<string, RDF.Term>();
    for await (const q of this.match())
      if (q.graph.termType !== 'DefaultGraph')
        result.set(q.graph.termType + '\0' + q.graph.value, q.graph);
    return [...result.values()];
  }
  async beginTransaction(options: UpdateOptions = {}): Promise<Transaction> {
    if (options.dryRun)
      throw new UnsupportedError('Transaction dryRun is unsupported; use update dryRun');
    const started = performance.now();
    const handle = await this.run<any>(
      options,
      (cancel) => this.handle.begin(wireOptions(options), cancel),
      true,
    );
    const remaining = {
      ...options,
      timeout:
        options.timeout === undefined
          ? undefined
          : Math.max(0, options.timeout - Math.ceil(performance.now() - started)),
    };
    const tx = new Transaction(this, handle, remaining, () => this.transactions.delete(tx));
    this.transactions.add(tx);
    return tx;
  }
  async transaction<T>(
    callback: (tx: Transaction) => T | Promise<T>,
    options: UpdateOptions = {},
  ): Promise<{ value: T; receipt: CommitReceipt }> {
    const tx = await this.beginTransaction(options);
    try {
      const value = await Promise.race([
        transactionContext.run(
          new Set([...(transactionContext.getStore() ?? []), this.family]),
          async () => callback(tx),
        ),
        tx.interrupted,
      ]);
      const receipt = await tx.commit();
      return { value, receipt };
    } catch (e) {
      await tx.rollback();
      throw e;
    }
  }
  async load(
    input: RdfInput,
    options: LoadOptions = {},
  ): Promise<{ inserted: bigint; receipt: CommitReceipt }> {
    if (options.dryRun) throw new UnsupportedError('Load dryRun is unsupported; use update dryRun');
    if (typeof input === 'object' && 'path' in input) return this.loadFiles([input.path], options);
    const json = JSON.stringify({
      ...JSON.parse(wireOptions(options)),
      toGraph: options.toGraph?.value,
    });
    return this.run(
      options,
      async (cancel, signal) => {
        const upload = await this.handle.loadStart(json, cancel);
        let reject!: (reason: unknown) => void;
        const stopped = new Promise<never>((_, r) => {
          reject = r;
        });
        void stopped.catch(() => {});
        const abort = () => {
          upload.abort();
          reject(signal.reason);
        };
        signal.addEventListener('abort', abort, { once: true });
        if (signal.aborted) abort();
        const chunks = inputChunks(input)[Symbol.asyncIterator]();
        try {
          while (true) {
            const next = await Promise.race([chunks.next(), stopped]);
            if (next.done) break;
            const chunk = next.value;
            for (let offset = 0; offset < chunk.length; offset += 64 << 10)
              await Promise.race([
                upload.push(Buffer.from(chunk.subarray(offset, offset + (64 << 10)))),
                stopped,
              ]);
          }
          const result = JSON.parse(await upload.finish());
          return { inserted: BigInt(result.inserted), receipt: receiptOf(result.receipt)! };
        } catch (e) {
          upload.abort();
          await upload.finish().catch(() => {});
          throw e;
        } finally {
          signal.removeEventListener('abort', abort);
          void chunks.return?.(undefined).catch(() => {});
          if (signal.aborted && input instanceof Readable) input.destroy();
        }
      },
      true,
    );
  }
  async loadFiles(
    paths: string[],
    options: LoadOptions = {},
  ): Promise<{ inserted: bigint; receipt: CommitReceipt }> {
    const json = JSON.stringify({
      ...JSON.parse(wireOptions(options)),
      toGraph: options.toGraph?.value,
    });
    const result = JSON.parse(
      await this.run<string>(options, (cancel) => this.handle.loadPaths(paths, json, cancel), true),
    );
    if (options.dryRun) throw new UnsupportedError('Load dryRun is unsupported; use update dryRun');
    return { inserted: BigInt(result.inserted), receipt: receiptOf(result.receipt)! };
  }
  private byteStream(options: OperationOptions, create: (cancel: any) => Promise<any> | any) {
    this.check();
    const token = new native.Cancellation();
    let handle: any;
    let stopped = false;
    const active = this;
    let reject!: (reason: unknown) => void;
    const interruption = new Promise<never>((_, r) => {
      reject = r;
    });
    void interruption.catch(() => {});
    const close = async () => {
      if (stopped) return;
      stopped = true;
      reject(new InvalidInputError('Dataset stream is closed'));
      token.cancel();
      if (handle) await handle.close();
      if (timer) clearTimeout(timer);
      options.signal?.removeEventListener('abort', abort);
      active.results.delete(owner);
    };
    const abort = () => {
      reject(options.signal?.reason);
      void close();
    };
    options.signal?.addEventListener('abort', abort, { once: true });
    const timer =
      options.timeout === undefined
        ? undefined
        : setTimeout(() => {
            reject(new QueryTimeoutError());
            void close();
          }, options.timeout);
    const owner = { close };
    this.results.add(owner);
    return new ReadableStream<Uint8Array>(
      {
        async pull(controller) {
          try {
            options.signal?.throwIfAborted();
            if (stopped) {
              controller.close();
              return;
            }
            if (!handle) {
              const creating = Promise.resolve(create(token));
              void creating
                .then(async (late) => {
                  if (stopped) await late.close();
                })
                .catch(() => {});
              handle = await Promise.race([creating, interruption]);
            }
            const chunk = await Promise.race([handle.next(), interruption]);
            if (chunk === null) {
              await close();
              controller.close();
            } else controller.enqueue(new Uint8Array(chunk));
          } catch (e) {
            await close();
            controller.error(nativeError(e));
          }
        },
        async cancel() {
          await close();
        },
      },
      { highWaterMark: 0 },
    );
  }
  dump(options: DumpOptions = {}) {
    return this.byteStream(options, (cancel) =>
      this.handle.dumpStream(
        JSON.stringify({ ...options, signal: undefined, fromGraph: options.fromGraph?.value }),
        cancel,
      ),
    );
  }
  async dumpToFile(path: string, options: DumpOptions = {}) {
    return BigInt(
      await this.run<string>(options, (cancel) =>
        this.handle.dumpFile(
          path,
          JSON.stringify({ ...options, signal: undefined, fromGraph: options.fromGraph?.value }),
          cancel,
        ),
      ),
    );
  }
  async dumpToString(options: DumpOptions = {}) {
    return new Response(this.dump(options)).text();
  }
  async queryToStream(
    text: string,
    options: QueryOptions & { accept?: string } = {},
  ): Promise<ReadableStream<Uint8Array> & { contentType: string }> {
    const stream = this.byteStream(options, (cancel) =>
      this.handle.queryStream(text, wireOptions(options), cancel),
    );
    return Object.assign(stream, {
      contentType: options.accept ?? 'application/sparql-results+json',
    });
  }
  close(): Promise<void> {
    return (this.closePromise ??= (async () => {
      this.closing = true;
      for (const cancel of this.tokens) cancel.cancel();
      await Promise.allSettled([...this.transactions].map((tx) => tx.rollback()));
      await Promise.allSettled([...this.results].map((r) => r.close()));
      await Promise.allSettled([...this.operations]);
      await this.handle.close();
      this.closed = true;
    })());
  }
  async [Symbol.asyncDispose]() {
    await this.close();
  }
}

function inputChunks(
  input: Exclude<RdfInput, { path: string }>,
): AsyncIterableIterator<Uint8Array> {
  let source: AsyncIterator<string | Uint8Array> | Iterator<string | Uint8Array>;
  if (typeof input === 'string' || input instanceof Uint8Array) source = [input][Symbol.iterator]();
  else if (input instanceof ReadableStream) {
    const reader = input.getReader();
    let released = false;
    const release = () => {
      if (!released) {
        released = true;
        reader.releaseLock();
      }
    };
    source = {
      async next() {
        try {
          const row = await reader.read();
          if (row.done) release();
          return row;
        } catch (e) {
          release();
          throw e;
        }
      },
      return() {
        // Cancel the owned reader directly: generator.return() would queue
        // behind an outstanding read from a stalled source.
        const cancellation = reader.cancel();
        release();
        void cancellation.catch(() => {});
        return Promise.resolve({ done: true, value: undefined });
      },
    };
  } else source = input[Symbol.asyncIterator]();
  return {
    [Symbol.asyncIterator]() {
      return this;
    },
    async next() {
      const row = await source.next();
      return row.done
        ? { done: true, value: undefined }
        : {
            done: false,
            value: typeof row.value === 'string' ? new TextEncoder().encode(row.value) : row.value,
          };
    },
    async return() {
      await source.return?.();
      return { done: true, value: undefined };
    },
  };
}

function deferredQuads(future: Promise<QuadsResult>) {
  return {
    async *[Symbol.asyncIterator]() {
      const r = await future;
      try {
        yield* r;
      } finally {
        await r.close();
      }
    },
    async toArray() {
      return (await future).toArray();
    },
    toStream() {
      return Readable.from(this);
    },
  };
}

export class Transaction extends Queryable {
  private ended = false;
  private ending?: Promise<unknown>;
  private tokens = new Set<any>();
  private requests: Promise<unknown> = Promise.resolve();
  private timer?: ReturnType<typeof setTimeout>;
  private results = new Set<{ close(): Promise<void> }>();
  private interrupt!: (reason: unknown) => void;
  readonly interrupted: Promise<never>;
  private abortListener: () => void;
  constructor(
    private ds: Dataset,
    private handle: any,
    private options: UpdateOptions,
    private release: () => void,
  ) {
    super();
    this.interrupted = new Promise((_, reject) => {
      this.interrupt = reject;
    });
    void this.interrupted.catch(() => {});
    this.abortListener = () => this.cancel(options.signal?.reason);
    options.signal?.addEventListener('abort', this.abortListener, { once: true });
    if (options.signal?.aborted) this.abortListener();
    if (options.timeout !== undefined)
      this.timer = setTimeout(() => this.cancel(new QueryTimeoutError()), options.timeout);
  }
  private cancel(reason: unknown) {
    for (const token of this.tokens) token.cancel();
    this.interrupt(reason);
    void this.rollback().catch(() => {});
  }
  private async call<T>(options: OperationOptions, f: (cancel: any) => Promise<T>): Promise<T> {
    if (this.ended) throw new InvalidInputError('Transaction has ended');
    options.signal?.throwIfAborted();
    const token = new native.Cancellation();
    this.tokens.add(token);
    const abort = () => token.cancel();
    options.signal?.addEventListener('abort', abort, { once: true });
    let timedOut = false;
    const timer =
      options.timeout === undefined
        ? undefined
        : setTimeout(() => {
            timedOut = true;
            abort();
          }, options.timeout);
    const request = this.requests.then(async () => {
      if (this.ended) throw new InvalidInputError('Transaction has ended');
      return f(token);
    });
    this.requests = request.catch(() => {});
    try {
      const value = await request;
      if (options.signal?.aborted) throw options.signal.reason;
      if (timedOut) throw new QueryTimeoutError();
      return value;
    } catch (e) {
      if (options.signal?.aborted) throw options.signal.reason;
      if (timedOut) throw new QueryTimeoutError();
      throw nativeError(e);
    } finally {
      if (timer) clearTimeout(timer);
      options.signal?.removeEventListener('abort', abort);
      this.tokens.delete(token);
    }
  }
  protected override result(handle: any, options: QueryOptions): QueryResult {
    const info = JSON.parse(handle.info());
    if (info.type === 'boolean') {
      void handle.close();
      return { type: 'boolean', value: info.value, plan: info.plan, timing: info.timing };
    }
    const result: PullResult<unknown> = new PullResult(handle, options, () => {
      this.results.delete(result);
    });
    this.results.add(result);
    return result as unknown as BindingsResult | QuadsResult;
  }
  protected read(text: string, options: QueryOptions) {
    return this.call(options, (cancel) => this.handle.query(text, wireOptions(options), cancel));
  }
  async update(text: string, options: UpdateOptions = {}) {
    if (options.dryRun)
      throw new UnsupportedError('Transaction dryRun is unsupported; use dataset update dryRun');
    if (options.ifHead !== undefined)
      throw new UnsupportedError(
        'Transaction update ifHead is unsupported; set it when beginning the transaction',
      );
    return updateResult(
      await this.call<string>(options, (cancel) =>
        this.handle.update(text, wireOptions(options), cancel),
      ),
    );
  }
  async addBatch(quads: RDF.Quad[]) {
    const r = await this.call<string>({}, () =>
      this.handle.apply(JSON.stringify(quads.map((q) => [true, encodeTerm(q)]))),
    );
    return BigInt(JSON.parse(r).inserted);
  }
  async add(quad: RDF.Quad) {
    return (await this.addBatch([quad])) > 0n;
  }
  async delete(quad: RDF.Quad) {
    const r = await this.call<string>({}, () =>
      this.handle.apply(JSON.stringify([[false, encodeTerm(quad)]])),
    );
    return BigInt(JSON.parse(r).deleted) > 0n;
  }
  match(s?: RDF.Term | null, p?: RDF.Term | null, o?: RDF.Term | null, g?: RDF.Term | null) {
    return deferredQuads(
      this.call<any>({}, () => this.handle.matched(pattern(s, p, o, g))).then(
        (handle) => this.result(handle, {}) as QuadsResult,
      ),
    );
  }
  async commit(): Promise<CommitReceipt> {
    await this.requests;
    if (this.ended) throw new InvalidInputError('Transaction has ended');
    return receiptOf(JSON.parse((await this.end(true)) as string))!;
  }
  async rollback() {
    for (const token of this.tokens) token.cancel();
    if (this.ending) {
      await this.ending;
      return;
    }
    if (!this.ended) await this.end(false);
  }
  private end(commit: boolean) {
    this.ended = true;
    if (this.timer) clearTimeout(this.timer);
    this.options.signal?.removeEventListener('abort', this.abortListener);
    return (this.ending = Promise.allSettled([...this.results].map((result) => result.close()))
      .then(() => this.handle.end(commit))
      .catch((e: unknown) => {
        throw nativeError(e);
      })
      .finally(this.release));
  }
  async [Symbol.asyncDispose]() {
    await this.rollback();
  }
}

export interface CatalogDatasetInfo {
  name: string;
  id: string;
  kind: 'persistent' | 'mem';
  path: string | null;
  attached: boolean;
  reservedBy: string | null;
}
/** A name claim; close releases it, including when its catalog closes. */
export class CatalogReservation {
  private closed = false;
  /** @internal */ constructor(
    private handle: any,
    public readonly name: string,
    private release: () => void,
  ) {}
  close() {
    if (!this.closed) {
      this.closed = true;
      this.handle.close();
      this.release();
    }
  }
  async [Symbol.asyncDispose]() {
    this.close();
  }
}
export class Catalog {
  private closed = false;
  private tokens = new Set<any>();
  private repositoriesOwned = new Set<Repository>();
  private reservations = new Set<CatalogReservation>();
  private datasets = new Set<Dataset>();
  private operations = new Set<Promise<unknown>>();
  private closePromise?: Promise<void>;
  private constructor(
    private handle: any,
    public readonly path: string | null,
    private options: DatasetOptions,
  ) {}
  static memory(options: DatasetOptions = {}) {
    return new Catalog(native.NativeCatalog.memory(JSON.stringify(options)), null, options);
  }
  static async open(path: string, options: DatasetOptions = {}) {
    try {
      return new Catalog(
        await native.NativeCatalog.open(path, JSON.stringify(options)),
        path,
        options,
      );
    } catch (e) {
      throw nativeError(e);
    }
  }
  private check() {
    if (this.closed) throw new InvalidInputError('Catalog is closed');
  }
  private dataset(op: string, args: Configuration) {
    this.check();
    const task = (async () => {
      try {
        const handle = await this.handle.dataset(op, JSON.stringify(args));
        if (!handle) return null;
        if (this.closed) {
          handle.close();
          throw new InvalidInputError('Catalog is closed');
        }
        const ds = Dataset.fromNative(handle, null, this.options);
        this.datasets.add(ds);
        return ds;
      } catch (e) {
        throw nativeError(e);
      }
    })();
    this.operations.add(task);
    void task.finally(() => this.operations.delete(task)).catch(() => {});
    return task;
  }
  private track<T>(task: Promise<T>) {
    this.operations.add(task);
    void task.finally(() => this.operations.delete(task)).catch(() => {});
    return task;
  }
  private admin<T>(op: string, args: Configuration = {}): Promise<T> {
    this.check();
    return this.track(
      (async () => {
        try {
          return JSON.parse(await this.handle.admin(op, JSON.stringify(args)));
        } catch (e) {
          throw nativeError(e);
        }
      })(),
    );
  }
  list() {
    return this.admin<CatalogDatasetInfo[]>('list');
  }
  /** Collect legacy catalog backup-file paths without reading their contents. */
  backupFiles() {
    return this.admin<{ name: string; path: string }[]>('backupFiles');
  }
  reserve(name: string, kind: 'clone' | 'restore', holder: string) {
    this.check();
    try {
      const reservation = new CatalogReservation(
        this.handle.reserve(name, kind, holder),
        name,
        () => this.reservations.delete(reservation),
      );
      this.reservations.add(reservation);
      return reservation;
    } catch (e) {
      throw nativeError(e);
    }
  }
  /** Offline retention is a blocking repository operation; dryRun defaults true. */
  applyRetention(
    policy: BackupPolicy,
    options: { dryRun?: boolean } = {},
  ): Promise<RetentionReport> {
    this.check();
    for (const key of Object.keys(options))
      if (key !== 'dryRun') throw new UnsupportedError(`Retention does not support ${key}`);
    return this.track(
      (async () => {
        try {
          return policyResult(
            JSON.parse(
              await this.handle.applyRetention(JSON.stringify(policy), options.dryRun ?? true),
            ),
          );
        } catch (e) {
          throw nativeError(e);
        }
      })(),
    );
  }
  /** Execute one manual policy run with cancellation and writer admission. */
  async runPolicy(policy: BackupPolicy, options: OperationOptions = {}): Promise<PolicyRun> {
    this.check();
    for (const key of Object.keys(options))
      if (key !== 'signal' && key !== 'timeout')
        throw new UnsupportedError(`Policy runs do not support ${key}`);
    options.signal?.throwIfAborted();
    wireOptions(options);
    const started = Date.now();
    // Reject same-family transaction owners before capture waits on its writer.
    for (const info of await this.list())
      if (
        (policy.datasets ?? ['*']).some((p) =>
          new RegExp(
            '^' +
              p
                .split('*')
                .map((s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'))
                .join('.*') +
              '$',
          ).test(info.name),
        )
      )
        await this.checkFamily(info.name);
    this.check();
    options.signal?.throwIfAborted();
    const remaining =
      options.timeout === undefined ? undefined : options.timeout - (Date.now() - started);
    if (remaining !== undefined && remaining <= 0) throw new QueryTimeoutError();
    const wire = wireOptions({ ...options, timeout: remaining });
    const token = new native.Cancellation();
    this.tokens.add(token);
    let timedOut = false;
    const abort = () => token.cancel();
    options.signal?.addEventListener('abort', abort, { once: true });
    const timer =
      remaining === undefined
        ? undefined
        : setTimeout(() => {
            timedOut = true;
            abort();
          }, remaining);
    return this.track(
      (async () => {
        try {
          return policyResult(
            JSON.parse(await this.handle.runPolicy(JSON.stringify(policy), wire, token)),
          );
        } catch (e) {
          if (options.signal?.aborted) throw options.signal.reason;
          if (timedOut) throw new QueryTimeoutError();
          throw nativeError(e);
        } finally {
          this.tokens.delete(token);
          if (timer) clearTimeout(timer);
          options.signal?.removeEventListener('abort', abort);
        }
      })(),
    );
  }
  info(name: string) {
    return this.admin<CatalogDatasetInfo | null>('info', { name });
  }
  get(name: string) {
    return this.dataset('get', { name });
  }
  getById(id: string) {
    return this.dataset('getById', { name: id });
  }
  static async inspect(path: string): Promise<CatalogDatasetInfo[]> {
    try {
      return JSON.parse(await native.NativeCatalog.inspect(path));
    } catch (e) {
      throw nativeError(e);
    }
  }
  async create(name: string, options: { kind?: 'persistent' | 'mem' } = {}) {
    return (await this.dataset('create', { name, ...options }))!;
  }
  async attach(name: string, path?: string) {
    return (await this.dataset('attach', { name, path }))!;
  }
  private async checkFamily(name: string) {
    const ds = await this.get(name);
    try {
      ds?.check(true);
    } finally {
      await ds?.close();
    }
  }
  async rename(name: string, to: string) {
    await this.checkFamily(name);
    return (await this.dataset('rename', { name, to }))!;
  }
  async delete(name: string) {
    await this.checkFamily(name);
    return this.admin<boolean>('delete', { name });
  }
  async clone(source: string, name: string, options: OperationOptions = {}) {
    await this.checkFamily(source);
    this.check();
    options.signal?.throwIfAborted();
    const token = new native.Cancellation();
    this.tokens.add(token);
    let timedOut = false;
    const abort = () => token.cancel();
    options.signal?.addEventListener('abort', abort, { once: true });
    const timer =
      options.timeout === undefined
        ? undefined
        : setTimeout(() => {
            timedOut = true;
            abort();
          }, options.timeout);
    return this.track(
      (async () => {
        try {
          const handle = await this.handle.cloneDataset(source, name, wireOptions(options), token);
          if (this.closed || options.signal?.aborted || timedOut) {
            await handle.close();
            options.signal?.throwIfAborted();
            if (timedOut) throw new QueryTimeoutError();
            throw new InvalidInputError('Catalog is closed');
          }
          const ds = Dataset.fromNative(handle, null, this.options);
          this.datasets.add(ds);
          return ds;
        } catch (e) {
          if (options.signal?.aborted) throw options.signal.reason;
          if (timedOut) throw new QueryTimeoutError();
          throw nativeError(e);
        } finally {
          this.tokens.delete(token);
          if (timer) clearTimeout(timer);
          options.signal?.removeEventListener('abort', abort);
        }
      })(),
    );
  }
  get repositories() {
    const cat = this;
    return {
      list: () => cat.admin<Configuration[]>('repositories.list'),
      get: (name: string) => cat.admin<Configuration>('repositories.get', { name }),
      add: (config: Configuration) => cat.admin<Configuration>('repositories.add', { config }),
      update: (name: string, config: Configuration) =>
        cat.admin<Configuration>('repositories.update', { name, config }),
      remove: (name: string) => cat.admin<boolean>('repositories.remove', { name }),
      /** Add immutable operator-defined entries to this catalog's shared registry. */
      withFixed: (entries: Configuration[]) =>
        cat.admin<Configuration[]>('repositories.withFixed', { entries }),
      async open(name: string) {
        cat.check();
        return cat.track(
          (async () => {
            try {
              const repo = Repository.fromNative(await cat.handle.repository(name));
              if (cat.closed) {
                await repo.close();
                throw new InvalidInputError('Catalog is closed');
              }
              cat.repositoriesOwned.add(repo);
              return repo;
            } catch (e) {
              throw nativeError(e);
            }
          })(),
        );
      },
    };
  }
  restore(repository: Repository, name: string, target: string, options: UpdateOptions = {}) {
    this.check();
    return this.track(this.restoreInner(repository, name, target, options));
  }
  private async restoreInner(
    repository: Repository,
    name: string,
    target: string,
    options: UpdateOptions,
  ) {
    if (options.dryRun || options.ifHead !== undefined)
      throw new UnsupportedError('Restore does not support dryRun or ifHead');
    this.check();
    options.signal?.throwIfAborted();
    const cancel = new native.Cancellation();
    this.tokens.add(cancel);
    let timedOut = false;
    const abort = () => cancel.cancel();
    options.signal?.addEventListener('abort', abort, { once: true });
    const timer =
      options.timeout === undefined
        ? undefined
        : setTimeout(() => {
            timedOut = true;
            abort();
          }, options.timeout);
    try {
      const handle = await this.handle.restore(
        repository.nativeHandle(),
        name,
        target,
        wireOptions(options),
        cancel,
      );
      if (this.closed || options.signal?.aborted || timedOut) {
        await handle.close();
        options.signal?.throwIfAborted();
        if (timedOut) throw new QueryTimeoutError();
        throw new InvalidInputError('Catalog is closed');
      }
      const ds = Dataset.fromNative(handle, null, this.options);
      this.datasets.add(ds);
      return ds;
    } catch (e) {
      if (options.signal?.aborted) throw options.signal.reason;
      if (timedOut) throw new QueryTimeoutError();
      throw nativeError(e);
    } finally {
      this.tokens.delete(cancel);
      if (timer) clearTimeout(timer);
      options.signal?.removeEventListener('abort', abort);
    }
  }
  close() {
    return (this.closePromise ??= (async () => {
      this.closed = true;
      for (const reservation of this.reservations) reservation.close();
      this.reservations.clear();
      for (const token of this.tokens) token.cancel();
      await Promise.allSettled([...this.datasets].map((ds) => ds.close()));
      await Promise.allSettled([...this.operations]);
      await Promise.allSettled([...this.repositoriesOwned].map((repo) => repo.close()));
      this.repositoriesOwned.clear();
      await Promise.allSettled([...this.datasets].map((ds) => ds.close()));
      this.datasets.clear();
      this.handle.close();
    })());
  }
  async [Symbol.asyncDispose]() {
    await this.close();
  }
}

export class Repository {
  private closed = false;
  private operations = new Set<Promise<unknown>>();
  private tokens = new Set<any>();
  private closePromise?: Promise<void>;
  private constructor(private handle: any) {}
  /** @internal */ static fromNative(handle: any) {
    return new Repository(handle);
  }
  static async open(config: Configuration, options: { init?: boolean } = {}) {
    try {
      return new Repository(
        await native.NativeRepository.open(JSON.stringify(config), options.init ?? true),
      );
    } catch (e) {
      throw nativeError(e);
    }
  }
  /** @internal */ nativeHandle() {
    if (this.closed) throw new InvalidInputError('Repository is closed');
    return this.handle;
  }
  private call<T>(
    op: string,
    args: Configuration = {},
    options: OperationOptions = {},
  ): Promise<T> {
    const task = this.callInner<T>(op, args, options);
    this.operations.add(task);
    void task.finally(() => this.operations.delete(task)).catch(() => {});
    return task;
  }
  private async callInner<T>(
    op: string,
    args: Configuration = {},
    options: OperationOptions = {},
  ): Promise<T> {
    const handle = this.nativeHandle();
    options.signal?.throwIfAborted();
    const token = new native.Cancellation();
    this.tokens.add(token);
    const abort = () => token.cancel();
    options.signal?.addEventListener('abort', abort, { once: true });
    let timedOut = false;
    const timer =
      options.timeout === undefined
        ? undefined
        : setTimeout(() => {
            timedOut = true;
            abort();
          }, options.timeout);
    try {
      const value = await handle.admin(op, JSON.stringify(args), token);
      options.signal?.throwIfAborted();
      if (timedOut) throw new QueryTimeoutError();
      if (this.closed) throw new InvalidInputError('Repository is closed');
      return adminResult(JSON.parse(value));
    } catch (e) {
      if (options.signal?.aborted) throw options.signal.reason;
      if (timedOut) throw new QueryTimeoutError();
      throw nativeError(e);
    } finally {
      this.tokens.delete(token);
      if (timer) clearTimeout(timer);
      options.signal?.removeEventListener('abort', abort);
    }
  }
  list(options: OperationOptions & { dataset?: string; before?: string; limit?: number } = {}) {
    return this.call<Configuration[]>('list', { ...options }, options);
  }
  stats(o?: OperationOptions) {
    return this.call<Configuration>('stats', {}, o);
  }
  test(o?: OperationOptions) {
    return this.call<Configuration>('test', {}, o);
  }
  verify(names: string[], o?: OperationOptions) {
    return this.call<Configuration>('verify', { names }, o);
  }
  gc(options: OperationOptions & { dryRun?: boolean } = {}) {
    return this.call<Configuration>('gc', { ...options }, options);
  }
  locks(o?: OperationOptions) {
    return this.call<Configuration[]>('locks', {}, o);
  }
  breakLock(id: string, o?: OperationOptions) {
    return this.call<boolean>('breakLock', { id }, o);
  }
  restoreToDir(name: string, path: string, o?: OperationOptions) {
    return this.call<Configuration>('restoreToDir', { name, path }, o);
  }
  close() {
    return (this.closePromise ??= (async () => {
      this.closed = true;
      for (const token of this.tokens) token.cancel();
      await Promise.allSettled([...this.operations]);
      this.handle.close();
    })());
  }
  async [Symbol.asyncDispose]() {
    await this.close();
  }
}

export { parse, serialize, type ParseOptions, type SerializeOptions } from './rdf.js';
