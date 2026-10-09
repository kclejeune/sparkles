import type { Dataset } from './index.js';
import type { RDF, OperationOptions, UpdateOptions, CommitReceipt } from '@sparkles-rdf/common';
import { decodeTerm, unsigned } from '@sparkles-rdf/common';
export type Configuration = Record<string, unknown>;
export interface CommitInfo {
  seq: bigint;
  parent?: bigint | null;
  kind: string;
  quads: bigint;
  inserted: bigint;
  deleted: bigint;
  timestamp: string;
  message?: string;
}
export interface DatasetInfo {
  id: string;
  branch: string;
  quads: bigint;
  head: CommitInfo;
}
export interface SnapshotInfo {
  name: string;
  seq: bigint;
  commit: CommitInfo | null;
  createdMs: number;
  note: string | null;
  expiresMs: number | null;
  reconstructable: boolean;
  warm: boolean;
}
export interface BranchInfo {
  name: string;
  id: string;
  protected: boolean;
  note?: string;
  head: CommitInfo;
  upstream?: string;
}
/** What relinking a branch to main's index did. */
export interface RelinkResult {
  generation: string;
  quads: bigint;
  baseCommit: bigint;
  caughtUpCommits: bigint;
  abandoned?: string;
  mode: string;
  lockMs: number;
  buildMs: number;
  totalMs: number;
  [key: string]: unknown;
}
export interface MergeResult {
  merged: boolean;
  upToDate?: boolean;
  report?: Configuration;
  conflicts?: Configuration;
}
export interface GuardOutcome {
  installed: boolean;
  removed?: boolean;
  validation?: Configuration;
}
export interface ValidationReport {
  conforms: boolean;
  results: Configuration[];
  turtle?: string;
  warnings?: string[];
}
export interface Change {
  op: 'add' | 'remove';
  quad: RDF.Quad;
}
export interface CommitChanges {
  commit: CommitInfo;
  added: bigint;
  removed: bigint;
  complete: boolean;
  changes: Change[];
}
export interface ChangePage {
  after: bigint;
  next: bigint;
  head: CommitInfo;
  commits: CommitChanges[];
}
export interface HistoryStatus {
  head: bigint;
  reconstructable: [bigint, bigint][];
  bytes: bigint;
  firstCommit: bigint;
  snapshots: number;
  generations: Configuration[];
  [key: string]: unknown;
}
export type AdminCall = <T>(
  op: string,
  args?: Configuration,
  options?: UpdateOptions,
  lock?: boolean,
) => Promise<T>;
export interface Setting<T = Configuration> {
  get(options?: OperationOptions): Promise<T>;
  set(value: T, options?: UpdateOptions): Promise<unknown>;
  reset(options?: UpdateOptions): Promise<unknown>;
}
const at = (v: string | number | bigint = 'head') =>
  typeof v === 'string' ? v : 'commit:' + unsigned(v);
const setting = <T>(call: AdminCall, name: string): Setting<T> => ({
  get: (o) => call(`settings.${name}.get`, {}, o, false),
  set: (value, o) => call(`settings.${name}.set`, value as Configuration, o, true),
  reset: (o) => call(`settings.${name}.reset`, {}, o, true),
});
export class Administration {
  readonly snapshots;
  readonly history;
  readonly settings;
  readonly prefixes;
  readonly queries;
  readonly branches;
  readonly indexes;
  readonly reasoning;
  readonly validation;
  constructor(
    private call: AdminCall,
    getBranch: (name: string) => Promise<Dataset>,
    runQuery: (
      name: string,
      params: Configuration,
      options: any,
    ) => Promise<import('@sparkles-rdf/common').QueryResult>,
  ) {
    this.prefixes = {
      list: (o?: OperationOptions) => call<Record<string, string>>('prefixes.list', {}, o, false),
      set: (prefix: string, iri: string, o?: UpdateOptions) =>
        call<void>('prefixes.set', { prefix, iri }, o, true),
      delete: (prefix: string, o?: UpdateOptions) =>
        call<boolean>('prefixes.delete', { prefix }, o, true),
    };
    this.snapshots = {
      list: (o?: OperationOptions) => call<SnapshotInfo[]>('snapshots.list', {}, o, false),
      get: (name: string, o?: OperationOptions) =>
        call<SnapshotInfo | null>('snapshots.get', { name }, o, false),
      create: (
        name: string,
        options: {
          at?: string | number | bigint;
          note?: string;
          expiresMs?: number;
          warm?: boolean;
        } & UpdateOptions = {},
      ) =>
        call<{ snapshot: SnapshotInfo; created: boolean }>(
          'snapshots.create',
          { ...options, name, at: at(options.at) },
          options,
          true,
        ),
      delete: (name: string, o?: UpdateOptions) =>
        call<boolean>('snapshots.delete', { name }, o, true),
    };
    this.history = {
      tick: (o?: UpdateOptions) => call<Configuration>('history.tick', {}, o, true),
      query: async (
        options: OperationOptions & {
          from?: string | number | bigint;
          to?: string | number | bigint;
          limit?: number;
        } = {},
      ) => {
        const p = await call<any>(
          'history.query',
          {
            ...options,
            from: options.from === undefined ? undefined : at(options.from),
            to: options.to === undefined ? undefined : at(options.to),
          },
          options,
          false,
        );
        for (const change of p.changes) change.quad = decodeTerm(change.quad);
        return p as Configuration;
      },
      status: (o?: OperationOptions) => call<HistoryStatus>('history.status', {}, o, false),
      commit: (reference: string | number | bigint = 'head', o?: OperationOptions) =>
        call<CommitInfo | null>('history.commit', { reference: at(reference) }, o, false),
      commits: (
        options: {
          before?: number | bigint;
          after?: number | bigint;
          limit?: number;
        } & OperationOptions = {},
      ) =>
        call<Configuration>(
          'history.commits',
          {
            ...options,
            before: options.before === undefined ? undefined : unsigned(options.before),
            after: options.after === undefined ? undefined : unsigned(options.after),
          },
          options,
          false,
        ),
      changes: async (
        after: number | bigint,
        options: { maxCommits?: number; maxQuads?: number } & OperationOptions = {},
      ) => {
        const p = await call<any>(
          'history.changes',
          { ...options, after: unsigned(after) },
          options,
          false,
        );
        for (const c of p.commits)
          for (const change of c.changes) change.quad = decodeTerm(change.quad);
        return p as ChangePage;
      },
      diff: async (
        from: string | number | bigint,
        to: string | number | bigint = 'head',
        options: OperationOptions = {},
      ) => {
        const p = await call<{ changes: any[] }>(
          'history.diff',
          { from: at(from), to: at(to) },
          options,
          false,
        );
        return p.changes.map((c) => ({ ...c, quad: decodeTerm(c.quad) })) as Change[];
      },
      prune: (o?: UpdateOptions) => call<bigint>('history.prune', {}, o, true),
    };
    this.settings = {
      describe: setting(call, 'describe'),
      compaction: setting(call, 'compaction'),
      quota: {
        get: (o?: OperationOptions) => call<Configuration>('settings.quota.get', {}, o, false),
        set: (maxBytes: number | bigint, o?: UpdateOptions) =>
          call<Configuration>('settings.quota.set', { maxBytes: unsigned(maxBytes) }, o, true),
        reset: (o?: UpdateOptions) => call<Configuration>('settings.quota.reset', {}, o, true),
      },
      retention: setting(call, 'retention'),
      changeLog: setting(call, 'changeLog'),
    };
    this.queries = {
      run: (
        name: string,
        params: Configuration = {},
        options: import('@sparkles-rdf/common').QueryOptions & { version?: number | bigint } = {},
      ) => runQuery(name, params, options),
      list: (o?: OperationOptions) => call<[string, Configuration][]>('queries.list', {}, o, false),
      get: (name: string, version?: number | bigint, o?: OperationOptions) =>
        call<Configuration | null>(
          'queries.get',
          { name, version: version === undefined ? undefined : unsigned(version) },
          o,
          false,
        ),
      versions: (name: string, o?: OperationOptions) =>
        call<Configuration[] | null>('queries.versions', { name }, o, false),
      put: (
        name: string,
        definition: Configuration,
        options: UpdateOptions & { author?: string; ifVersion?: number | bigint } = {},
      ) =>
        call<Configuration>(
          'queries.put',
          {
            ...options,
            name,
            definition,
            ifVersion: options.ifVersion === undefined ? undefined : unsigned(options.ifVersion),
          },
          options,
          true,
        ),
      delete: (name: string, options: UpdateOptions & { ifVersion?: number | bigint } = {}) =>
        call<boolean>(
          'queries.delete',
          {
            name,
            ifVersion: options.ifVersion === undefined ? undefined : unsigned(options.ifVersion),
          },
          options,
          true,
        ),
    };
    this.branches = {
      commitGraph: (
        options: OperationOptions & { branches?: string[]; before?: string; limit?: number } = {},
      ) => call<Configuration>('branches.commitGraph', { ...options }, options, false),
      list: (o?: OperationOptions) => call<BranchInfo[]>('branches.list', {}, o, false),
      get: (name: string, o?: OperationOptions) =>
        call<BranchInfo>('branches.get', { name }, o, false),
      open: getBranch,
      create: (
        name: string,
        options: UpdateOptions & {
          from?: string;
          at?: string | number | bigint;
          protected?: boolean;
          note?: string;
        } = {},
      ) =>
        call<BranchInfo>(
          'branches.create',
          { ...options, name, at: at(options.at) },
          options,
          true,
        ),
      delete: (
        name: string,
        options: UpdateOptions & { force?: boolean; reparent?: boolean } = {},
      ) => call<void>('branches.delete', { ...options, name }, options, true),
      rename: (name: string, to: string, o?: UpdateOptions) =>
        call<BranchInfo>('branches.rename', { name, to }, o, true),
      protect: (name: string, protect = true, o?: UpdateOptions) =>
        call<BranchInfo>('branches.protect', { name, protected: protect }, o, true),
      note: (name: string, note: string | null, o?: UpdateOptions) =>
        call<BranchInfo>('branches.note', { name, note }, o, true),
      relink: (name: string, o?: UpdateOptions) =>
        call<RelinkResult>('branches.relink', { name }, o, true),
      merge: (source: string, target = 'main', options: Configuration & UpdateOptions = {}) =>
        call<MergeResult>('branches.merge', { ...options, source, target }, options, true),
      previewMerge: (
        source: string,
        target = 'main',
        options: Configuration & UpdateOptions = {},
      ) =>
        call<Configuration>('branches.previewMerge', { ...options, source, target }, options, true),
      revert: (branch: string, commit: number | bigint, o: UpdateOptions = {}) =>
        call<MergeResult>('branches.revert', { branch, commit: unsigned(commit) }, o, true),
      previewRevert: (branch: string, commit: number | bigint, o: UpdateOptions = {}) =>
        call<Configuration>(
          'branches.previewRevert',
          { branch, commit: unsigned(commit) },
          o,
          true,
        ),
      cherryPick: (
        source: string,
        commit: number | bigint,
        target = 'main',
        o: UpdateOptions = {},
      ) =>
        call<MergeResult>(
          'branches.cherryPick',
          { source, target, commit: unsigned(commit) },
          o,
          true,
        ),
      previewCherryPick: (
        source: string,
        commit: number | bigint,
        target = 'main',
        o: UpdateOptions = {},
      ) =>
        call<Configuration>(
          'branches.previewCherryPick',
          { source, target, commit: unsigned(commit) },
          o,
          true,
        ),
      exempt: {
        get: (o?: OperationOptions) => call<string[]>('branches.exempt.get', {}, o, false),
        set: (predicates: string[], o?: UpdateOptions) =>
          call<string[]>('branches.exempt.set', { predicates }, o, true),
      },
    };
    const index = (name: string) => ({
      status: (o?: OperationOptions) =>
        call<Configuration | null>(`indexes.${name}.status`, {}, o, false),
      enable: (config: Configuration = {}, o?: UpdateOptions) =>
        call<Configuration>(`indexes.${name}.enable`, config, o, true),
      disable: (o?: UpdateOptions) => call<void>(`indexes.${name}.disable`, {}, o, true),
      rebuild: (o?: UpdateOptions) => call<Configuration>(`indexes.${name}.rebuild`, {}, o, true),
    });
    this.indexes = {
      text: {
        ...index('text'),
        search: (
          query: string,
          options: OperationOptions & { limit?: number; lang?: string; highlight?: boolean } = {},
        ) => call<Configuration>('indexes.text.search', { ...options, query }, options, false),
      },
      geo: {
        ...index('geo'),
        features: (
          bbox: [number, number, number, number],
          options: OperationOptions & {
            graph?: string;
            predicate?: string;
            limit?: number;
            tolerance?: number;
          } = {},
        ) => call<Configuration>('indexes.geo.features', { ...options, bbox }, options, false),
      },
      vector: {
        recall: (
          name: string,
          options: OperationOptions & { samples?: number; k?: number; ef?: number } = {},
        ) => call<Configuration>('indexes.vector.recall', { name, ...options }, options, false),
        reembed: (name: string, o?: UpdateOptions) =>
          call<void>('indexes.vector.reembed', { name }, o, true),
        /** Executes embedding work until idle; waitMs bounds the engine worker. */
        embedUntilIdle: (waitMs = 30000) => {
          if (!Number.isSafeInteger(waitMs) || waitMs <= 0 || waitMs > 600000)
            throw new RangeError('waitMs must be in 1..600000');
          return call<void>('indexes.vector.embedUntilIdle', { waitMs }, {}, true);
        },
        list: (o?: OperationOptions) => call<Configuration[]>('indexes.vector.list', {}, o, false),
        get: (name: string, o?: OperationOptions) =>
          call<Configuration | null>('indexes.vector.get', { name }, o, false),
        put: (name: string, config: Configuration, o?: UpdateOptions) =>
          call<boolean>('indexes.vector.put', { name, config }, o, true),
        drop: (name: string, o?: UpdateOptions) =>
          call<void>('indexes.vector.drop', { name }, o, true),
        rebuild: (name: string, o?: UpdateOptions) =>
          call<void>('indexes.vector.rebuild', { name }, o, true),
        wait: (name: string, o?: UpdateOptions) =>
          call<Configuration | null>('indexes.vector.wait', { name }, o, false),
      },
    };
    this.reasoning = {
      diagnostics: (
        options: OperationOptions & {
          checks?: string[];
          graphs?: string[];
          limit?: number;
          inferences?: boolean;
        } = {},
      ) => call<Configuration>('reasoning.diagnostics', { ...options }, options, false),
      status: (o?: OperationOptions) =>
        call<Configuration | null>('reasoning.status', {}, o, false),
      run: (options: UpdateOptions & { profile?: string; incremental?: boolean } = {}) =>
        call<Configuration>('reasoning.run', { profile: 'rdfs', ...options }, options, true),
      clear: (o?: UpdateOptions) => call<bigint>('reasoning.clear', {}, o, true),
      rdfs: {
        get: (o?: OperationOptions) =>
          call<{ enabled: boolean }>('reasoning.rdfs.get', {}, o, false),
        set: (graph: string, o?: UpdateOptions) =>
          call<void>('reasoning.rdfs.set', { graph }, o, true),
        reset: (o?: UpdateOptions) => call<void>('reasoning.rdfs.reset', {}, o, true),
      },
    };
    this.validation = {
      shacl: (
        shapes: string,
        options: OperationOptions & { format?: string; baseIri?: string } = {},
      ) =>
        call<ValidationReport>(
          'validation.shacl',
          { format: 'ttl', ...options, shapes },
          options,
          false,
        ),
      shex: (schema: string, shapeMap: string, o?: OperationOptions) =>
        call<ValidationReport>('validation.shex', { schema, shapeMap }, o, false),
      guard: {
        get: (o?: OperationOptions) =>
          call<Configuration | null>('validation.guard.get', {}, o, false),
        setShacl: (config: Configuration, o?: UpdateOptions) =>
          call<GuardOutcome>('validation.guard.setShacl', config, o, true),
        setShex: (config: Configuration, o?: UpdateOptions) =>
          call<GuardOutcome>('validation.guard.setShex', config, o, true),
        reset: (o?: UpdateOptions) => call<void>('validation.guard.reset', {}, o, true),
      },
    };
  }
}
// Native numeric strings avoid lossy JSON parsing. Convert only numeric schema fields,
// leaving user literals, prefixes, notes and stored query text untouched.
const counts = new Set([
  'seq',
  'parent',
  'quads',
  'inserted',
  'deleted',
  'head',
  'after',
  'next',
  'added',
  'removed',
  'commit',
  'firstCommit',
  'baseSeq',
  'endSeq',
  'bytes',
  'cacheBytes',
  'hits',
  'misses',
  'materializations',
  'maxBytes',
  'usedBytes',
  'inferred',
  'version',
  'ifVersion',
  'minDeltaQuads',
  'maxDeltaQuads',
  'maxDeltaMb',
  'maxWalMb',
  'idleSeconds',
  'maxAgeSeconds',
  'minIntervalSeconds',
  'keepCommits',
  'keepAgeMs',
  'segmentBytes',
  'defaultMaxBytes',
  'bulkMaxQuads',
]);
const numbers = new Set([
  'createdMs',
  'expiresMs',
  'snapshots',
  'cacheEntries',
  'format',
  'dimension',
  'iterations',
  'millis',
  'limit',
  'reportLimit',
  'maxDepth',
  'version',
  'ifVersion',
]);
export function adminResult(v: any, key = ''): any {
  if (['definition', 'config', 'variables', 'data', 'default'].includes(key)) return v;
  if (typeof v === 'string' && /^\d+$/.test(v)) {
    if (counts.has(key)) return BigInt(v);
    if (numbers.has(key)) return Number(v);
  }
  if (Array.isArray(v) && key === 'reconstructable') return v.map((pair) => pair.map(BigInt));
  if (Array.isArray(v)) return v.map((item) => adminResult(item, key));
  if (v && typeof v === 'object') {
    const out: Record<string, unknown> = {};
    for (const [k, value] of Object.entries(v)) out[k] = adminResult(value, k);
    return out;
  }
  return v;
}
