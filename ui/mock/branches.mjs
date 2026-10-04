// Mock of branches and merges (docs/specs/F09-branches-and-merges.md) for UI development:
// `/$/branches/{ds}[/{name}]`, `/$/merge/{ds}`, and the branch a request chooses with
// `?branch=` or the path form `/{ds}@{branch}/…`.
//
// A branch is a copy of the dataset object with its own store and commits, so the rest of
// the mock serves it unchanged. It inherits the commits of the branch it started from, and
// it keeps the state of its merge base with its upstream for three-way merges. The mock
// merges a branch with its upstream only, in either direction, with cell conflicts
// (graph, subject, predicate). It cannot read past states, so a branch created at an
// older commit starts from the current data of its source.

import { randomUUID } from 'node:crypto';
import ox from 'oxigraph';

export const MAIN = 'main';

/** Admin routes that take `branch=`; the others refuse a branch other than main. */
const BRANCH_ROUTES = new Set([
  'commits',
  'snapshots',
  'history',
  'compaction',
  'compact',
  'stats',
  'schema',
  'reason',
  'validation',
  'prefixes',
  'describe',
  'text',
  'geo',
  'vector',
  'rdfs',
  'cache',
]);

const validName = (n) =>
  /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(n) && /[A-Za-z]/.test(n) && n !== 'head';

/** The branch fields of a new dataset (its `main`). */
export function initBranches(ds) {
  ds.branchName = MAIN;
  ds.branchId = ds.id;
  ds.ordinal = 0;
  ds.protected = false;
  ds.note = null;
  ds.created = new Date().toISOString();
  ds.inherited = [];
  /** The other branches, by name. */
  ds.branchSet = new Map();
  ds.nextOrdinal = 1;
}

/** Every commit of a branch: those it inherited, then its own, oldest first. */
export const allCommits = (b) => [...(b.inherited ?? []), ...b.commits];

/** A branch of the dataset `ds` by name (`main` is the dataset itself), or undefined. */
export const branchOf = (ds, name) => (!name || name === MAIN ? ds : ds.branchSet?.get(name));

const head = (b) => allCommits(b).at(-1);
const copyStore = (store) => new ox.Store(store.match());

function branchJson(ds, b, upstream = b.upstream ? branchOf(ds, b.upstream) : null) {
  const h = head(b);
  const base = b.base;
  const linked = b !== ds && !b.compacted;
  const own = b.store.size * 38;
  return {
    name: b.branchName,
    id: b.branchId,
    ordinal: b.ordinal,
    head: h.seq,
    modified: h.timestamp,
    from: b.from ?? null,
    upstream: b.upstream ?? null,
    mergeBase: base ? { branch: b.upstream, seq: base.upstreamSeq } : null,
    ahead: base ? h.seq - base.branchSeq : 0,
    behind: base && upstream ? head(upstream).seq - base.upstreamSeq : 0,
    protected: b.protected,
    note: b.note,
    created: b.created,
    storage: {
      linked,
      ownBytes: linked ? b.commits.length * 4096 : own + 65536,
      heldBytes: linked && upstream ? upstream.store.size * 38 : 0,
      generation: linked ? `gen-0001@${b.upstream}` : 'gen-0001',
    },
    broken: false,
  };
}

/**
 * The branch a request chose, from the path form or `branch=`: `{ name, path, seg }`
 * with the path rewritten to `/{ds}/…`, or `{ error: [status, body] }`.
 */
export function chooseBranch(path, seg, url) {
  const chosen = [];
  if (seg[0] !== '$' && seg[0]?.includes('@')) {
    const [ds, b] = seg[0].split('@', 2);
    if (!ds || !b)
      return { error: [400, { error: 'invalid dataset and branch', code: 'invalid-branch' }] };
    chosen.push(b);
    seg = [ds, ...seg.slice(1)];
    path = `/${seg.join('/')}`;
  }
  chosen.push(...url.searchParams.getAll('branch'));
  const distinct = [...new Set(chosen)];
  if (distinct.length > 1)
    return {
      error: [
        400,
        {
          error: `the request names more than one branch: ${distinct.join(', ')}`,
          code: 'invalid-branch',
        },
      ],
    };
  const name = distinct[0] ?? null;
  if (name && name !== MAIN && !validName(name))
    return { error: [400, { error: `invalid branch name '${name}'`, code: 'invalid-branch' }] };
  if (seg[0] === '$' && name && name !== MAIN) {
    const route = seg[1];
    const clone = route === 'datasets' && seg[3] === 'clone';
    if (!clone && !BRANCH_ROUTES.has(route))
      return { error: [400, { error: `${path} does not take a branch`, code: 'invalid-branch' }] };
  }
  return { name: name === MAIN ? null : name, path, seg };
}

/** The dataset a request names, on the branch it chose: `{ ds }` or `{ error }`. */
export function resolveBranch(main, name) {
  if (!main || !name) return { ds: main };
  if (main.type === 'mem')
    return {
      error: [501, { error: 'branches need a persistent dataset', code: 'branches-unsupported' }],
    };
  const b = main.branchSet.get(name);
  if (!b) return { error: [404, { error: `no such branch: ${name}`, code: 'no-such-branch' }] };
  return { ds: b };
}

/** A branch's quads by their N-Quads line. */
const quadMap = (store) => new Map(store.match().map((q) => [String(q), q]));

/** The cell of a quad: graph, subject and predicate. */
const cellOf = (q) =>
  `${q.graph.termType === 'DefaultGraph' ? '' : q.graph}\u0000${q.subject}\u0000${q.predicate}`;

/** The quads whose presence differs between two quad maps. */
function toggles(a, b) {
  const out = new Map();
  for (const [k, q] of a) if (!b.has(k)) out.set(k, q);
  for (const [k, q] of b) if (!a.has(k)) out.set(k, q);
  return out;
}

const objects = (m, cell) =>
  [...m.values()]
    .filter((q) => cellOf(q) === cell)
    .map((q) => String(q.object))
    .sort();

/** Plan the merge of `source` into `target` (one is the other's upstream). */
function planMerge(ds, source, target) {
  const child =
    source.upstream === target.branchName
      ? source
      : target.upstream === source.branchName
        ? target
        : null;
  if (!child)
    return {
      error: [
        400,
        {
          error: `the mock merges a branch with its upstream only (${source.branchName} and ${target.branchName})`,
          code: 'invalid-merge',
        },
      ],
    };
  const B = quadMap(child.base.store);
  const O = quadMap(target.store);
  const T = quadMap(source.store);
  const ours = toggles(B, O);
  const theirs = toggles(B, T);
  const sh = head(source).seq;
  const th = head(target).seq;
  const baseSeq = child === source ? child.base.upstreamSeq : child.base.branchSeq;
  const baseRef = { branch: target.branchName, seq: baseSeq };
  const upToDate =
    (child === source ? sh === child.base.branchSeq : sh === child.base.upstreamSeq) ||
    theirs.size === 0;
  const both = new Set(
    [...ours.values()].map(cellOf).filter((c) => [...theirs.values()].some((q) => cellOf(q) === c)),
  );
  const cells = [];
  for (const cell of both) {
    const o = objects(O, cell);
    const t = objects(T, cell);
    if (o.join('\n') === t.join('\n')) continue;
    const [g, s, p] = cell.split('\u0000');
    cells.push({
      graph: g || null,
      subject: s,
      predicate: p,
      base: objects(B, cell),
      ours: o,
      theirs: t,
    });
  }
  const apply = [...theirs].filter(([k]) => !ours.has(k));
  const inserted = apply.filter(([k]) => T.has(k)).length;
  return {
    child,
    apply,
    T,
    cells,
    report: {
      upToDate,
      fastForward: !upToDate && ours.size === 0,
      source: { branch: source.branchName, seq: sh },
      target: { branch: target.branchName, seq: th },
      base: baseRef,
      changes: upToDate
        ? { inserted: 0, deleted: 0 }
        : { inserted, deleted: apply.length - inserted },
      conflicts: { found: cells.length, resolved: 0 },
      commit: null,
      inferences: null,
    },
  };
}

function conflictReport(plan) {
  const { report, cells } = plan;
  const n = cells.length;
  const graphs = new Map();
  for (const c of cells) graphs.set(c.graph, (graphs.get(c.graph) ?? 0) + 1);
  return {
    error: `${n} conflict${n === 1 ? '' : 's'} merging ${report.source.branch} (commit ${report.source.seq}) into ${report.target.branch} (commit ${report.target.seq})`,
    code: 'merge-conflict',
    scope: 'cell',
    truncated: false,
    graphs: [...graphs].map(([graph, conflicts]) => ({ graph, conflicts })),
    cells,
  };
}

/** Create a branch of `ds` (also used to seed the mock). */
export function createBranch(
  ds,
  { name, from = MAIN, at = 'head', protected: prot = false, note = null },
) {
  const src = branchOf(ds, from);
  if (!src) return { error: [404, { error: `no such branch: ${from}`, code: 'no-such-branch' }] };
  if (!validName(name ?? '') || name === MAIN)
    return { error: [400, { error: `invalid branch name '${name}'`, code: 'invalid-branch' }] };
  if (ds.branchSet.has(name))
    return { error: [409, { error: `branch ${name} already exists`, code: 'branch-exists' }] };
  const commits = allCommits(src);
  const h = commits.at(-1);
  let seq = h.seq;
  const m = /^(?:commit:)?(\d+)$/.exec(String(at));
  if (m) seq = Number(m[1]);
  else if (String(at).startsWith('snapshot:'))
    return { error: [404, { error: `no snapshot ${String(at).slice(9)} on ${from}` }] };
  else if (at !== 'head' && !String(at).startsWith('time:'))
    return { error: [400, { error: `invalid commit selector '${at}'` }] };
  if (seq > h.seq)
    return { error: [404, { error: `no commit ${seq} on ${from} (head is ${h.seq})` }] };
  const b = {
    ...src,
    branchName: name,
    branchId: randomUUID(),
    ordinal: ds.nextOrdinal++,
    store: copyStore(src.store),
    commits: [],
    inherited: commits.filter((c) => c.seq <= seq),
    from: { branch: src.branchName, branchId: src.branchId, seq },
    upstream: src.branchName,
    base: { store: copyStore(src.store), branchSeq: seq, upstreamSeq: seq },
    protected: !!prot,
    note: note || null,
    created: new Date().toISOString(),
    branchSet: undefined,
    compacted: false,
    cache: { entries: 0, bytes: 0, hits: 0, misses: 0 },
    deltaInserts: 0,
    deltaDeletes: 0,
  };
  ds.branchSet.set(name, b);
  return { branch: b };
}

export async function handleBranches(req, res, url, seg, ctx) {
  if (seg[0] !== '$' || (seg[1] !== 'branches' && seg[1] !== 'merge')) return false;
  const { send, fail } = ctx;
  const ds = seg[2] ? ctx.datasets.get(seg[2]) : undefined;
  if (!ds) return (fail(res, 404, `No such dataset: ${seg[2] ?? ''}`), true);
  if (ds.type === 'mem')
    return (
      fail(res, 501, 'branches need a persistent dataset', { code: 'branches-unsupported' }), true
    );
  const err = ([status, body]) => (send(res, status, body), true);
  const body = async () => {
    const raw = (await ctx.readBody(req)).toString('utf8').trim();
    try {
      return raw ? JSON.parse(raw) : {};
    } catch {
      return null;
    }
  };

  if (seg[1] === 'branches') {
    const name = seg[3];
    if (!name) {
      if (req.method === 'GET') {
        const list = [
          ds,
          ...[...ds.branchSet.values()].sort((a, b) => a.branchName.localeCompare(b.branchName)),
        ];
        return (
          send(res, 200, {
            dataset: ds.name,
            datasetId: ds.id,
            branches: list.map((b) => branchJson(ds, b)),
          }),
          true
        );
      }
      if (req.method === 'POST') {
        const o = await body();
        if (!o || typeof o.name !== 'string')
          return err([
            400,
            { error: 'expected a JSON object with a name', code: 'invalid-branch' },
          ]);
        const r = createBranch(ds, o);
        if (r.error) return err(r.error);
        return (
          send(res, 201, branchJson(ds, r.branch), 'application/json', {
            Location: `/$/branches/${ds.name}/${o.name}`,
          }),
          true
        );
      }
      return (fail(res, 405, 'method not allowed'), true);
    }
    const b = branchOf(ds, name);
    if (!b) return err([404, { error: `no such branch: ${name}`, code: 'no-such-branch' }]);
    if (req.method === 'GET') return (send(res, 200, branchJson(ds, b)), true);
    if (req.method === 'PATCH') {
      const o = await body();
      if (!o) return err([400, { error: 'invalid JSON' }]);
      if (typeof o.protected === 'boolean') b.protected = o.protected;
      if ('note' in o) b.note = o.note || null;
      return (send(res, 200, branchJson(ds, b)), true);
    }
    if (req.method === 'DELETE') {
      if (b === ds) return err([400, { error: 'main cannot be deleted', code: 'invalid-branch' }]);
      const children = [...ds.branchSet.values()].filter((x) => x.upstream === name);
      if (children.length)
        return err([
          409,
          {
            error: `branch ${name} has branches created from it: ${children.map((c) => c.branchName).join(', ')}`,
            code: 'has-children',
          },
        ]);
      const ahead = head(b).seq - b.base.branchSeq;
      if (ahead > 0 && url.searchParams.get('force') !== 'true')
        return err([
          409,
          {
            error: `branch ${name} has ${ahead} commit${ahead === 1 ? '' : 's'} that ${b.upstream} does not have; delete with force=true`,
            code: 'unmerged',
          },
        ]);
      ds.branchSet.delete(name);
      // the commits it made name no branch any more
      const gone = (c) => (c.branchId === b.branchId ? { ...c, branch: null } : c);
      for (const x of [ds, ...ds.branchSet.values()]) {
        x.inherited = (x.inherited ?? []).map(gone);
        for (const c of x.commits)
          if (c.mergedFrom?.branchId === b.branchId)
            c.mergedFrom = { ...c.mergedFrom, branch: null };
      }
      res.writeHead(204, { 'Access-Control-Allow-Origin': '*' });
      res.end();
      return true;
    }
    return (fail(res, 405, 'method not allowed'), true);
  }

  // merges
  const o = req.method === 'POST' ? await body() : Object.fromEntries(url.searchParams);
  if (!o || typeof o.source !== 'string' || !o.source)
    return err([400, { error: 'a merge needs a source branch', code: 'invalid-merge' }]);
  const source = branchOf(ds, o.source);
  const target = branchOf(ds, o.target || MAIN);
  if (!source) return err([404, { error: `no such branch: ${o.source}`, code: 'no-such-branch' }]);
  if (!target) return err([404, { error: `no such branch: ${o.target}`, code: 'no-such-branch' }]);
  if (source === target)
    return err([400, { error: 'a branch cannot be merged into itself', code: 'invalid-merge' }]);
  const plan = planMerge(ds, source, target);
  if (plan.error) return err(plan.error);
  if (req.method === 'GET') {
    const preview = { merged: false, ...plan.report };
    if (plan.cells.length)
      Object.assign(preview, { conflictCount: plan.cells.length }, conflictReport(plan));
    return (send(res, 200, preview), true);
  }
  if (req.method !== 'POST') return (fail(res, 405, 'method not allowed'), true);
  const { report } = plan;
  const ex = o.expect ?? {};
  if (
    (ex.source != null && ex.source !== report.source.seq) ||
    (ex.target != null && ex.target !== report.target.seq)
  )
    return err([
      409,
      { error: 'a branch moved since the heads in expect were read', code: 'head-moved' },
    ]);
  if (report.upToDate) return (send(res, 200, { merged: false, ...report }), true);
  if (plan.cells.length) {
    const r = conflictReport(plan);
    return err([
      409,
      {
        ...r,
        source: report.source,
        target: report.target,
        base: report.base,
        conflicts: plan.cells.length,
      },
    ]);
  }
  for (const [k, q] of plan.apply) {
    if (plan.T.has(k)) target.store.add(q);
    else target.store.delete(q);
  }
  const commit = ctx.addCommit(target, 'merge', report.changes.inserted, report.changes.deleted);
  commit.mergedFrom = {
    branch: source.branchName,
    branchId: source.branchId,
    seq: report.source.seq,
  };
  commit.message =
    o.message ||
    `merge ${source.branchName} (commit ${report.source.seq}) into ${target.branchName}`;
  target.deltaInserts += report.changes.inserted;
  target.deltaDeletes += report.changes.deleted;
  // the merged states are the new merge base
  const child = plan.child;
  child.base = {
    store: copyStore(source.store),
    branchSeq: child === source ? report.source.seq : commit.seq,
    upstreamSeq: child === source ? commit.seq : report.source.seq,
  };
  send(res, 200, { merged: true, ...report, commit });
  return true;
}
