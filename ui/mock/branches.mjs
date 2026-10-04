// Mock of branches and merges (docs/specs/F09-branches-and-merges.md) for UI development:
// `/$/branches/{ds}[/{name}]`, `/$/merge/{ds}`, `/$/commit-graph/{ds}`, and the branch a
// request chooses with `?branch=` or the path form `/{ds}@{branch}/…`.
//
// A branch is a copy of the dataset object with its own store and commits, so the rest of
// the mock serves it unchanged. It inherits the commits of the branch it started from, and
// it keeps the state of its merge base with its upstream for three-way merges. The mock
// merges a branch with its upstream only, in either direction, with cell conflicts
// (graph, subject, predicate), resolutions and dry runs. A merge passes a small write
// guard that `PUT /$/validation/{ds}` sets (see `putGuard`). It cannot read past states, so a branch created at an
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

/** A quad from N-Triples terms (`graph` null for the default graph). */
function quadOf(s, p, o, g) {
  const st = new ox.Store();
  st.load(`${s} ${p} ${o}${g ? ` ${g}` : ''} .\n`, { format: 'application/n-quads' });
  return st.match()[0];
}

const SH = 'http://www.w3.org/ns/shacl#';

/**
 * The mock's write guard: the shapes of `PUT /$/validation/{ds}` with `mode: "reject"`,
 * read as "every instance of `sh:targetClass` has a value for each `sh:path` with a
 * `sh:minCount` of at least 1". Only merges check it.
 */
function guardRules(text) {
  const st = new ox.Store();
  st.load(text, { format: 'text/turtle' });
  return st
    .query(
      `SELECT ?shape ?cls ?path WHERE { ?shape <${SH}targetClass> ?cls ; <${SH}property> ?ps .
       ?ps <${SH}path> ?path ; <${SH}minCount> ?m FILTER(?m >= 1) }`,
    )
    .map((b) => ({ shape: b.get('shape'), cls: b.get('cls'), path: b.get('path') }));
}

const termJson = (t) =>
  t.termType === 'BlankNode' ? { type: 'bnode', value: t.value } : { type: 'uri', value: t.value };

/** The guard's report on `store`, or null when it conforms or there is no guard. */
function guardReport(b, store, kind) {
  if (!b.guard) return null;
  const results = [];
  for (const r of b.guard.rules) {
    const rows = store.query(
      `SELECT ?x WHERE { ?x a <${r.cls.value}> FILTER NOT EXISTS { ?x <${r.path.value}> ?v } }`,
    );
    for (const row of rows)
      results.push({
        focusNode: termJson(row.get('x')),
        resultPath: termJson(r.path),
        value: null,
        sourceShape: termJson(r.shape),
        sourceConstraintComponent: { type: 'uri', value: `${SH}MinCountConstraintComponent` },
        severity: { type: 'uri', value: `${SH}Violation` },
        messages: ['Less than 1 values'],
      });
  }
  if (!results.length) return null;
  return {
    language: 'shacl',
    status: 'rejected',
    blocking: results.length,
    total: results.length,
    limit: 100,
    truncated: false,
    results,
    head: head(b).seq,
    kind,
  };
}

/** `PUT /$/validation/{ds}` in the mock: `{mode, shapes: {inline}}` sets the guard. */
export async function putGuard(b, raw) {
  let o;
  try {
    o = JSON.parse(raw || '{}');
  } catch {
    return [400, { error: 'invalid JSON' }];
  }
  if (o.mode === 'off') {
    b.guard = null;
    return [200, { mode: 'off', shapeCount: 0 }];
  }
  const text = o.shapes?.inline;
  if (typeof text !== 'string') return [400, { error: 'shapes.inline is required' }];
  try {
    const rules = guardRules(text);
    b.guard = { mode: o.mode ?? 'reject', rules };
    return [200, { mode: b.guard.mode, shapeCount: rules.length }];
  } catch (e) {
    return [400, { error: `the shapes do not parse: ${e}` }];
  }
}

/**
 * Plan the merge of `source` into `target` (one is the other's upstream) with the
 * request's `resolutions` and `onConflict`.
 */
function planMerge(ds, source, target, o = {}) {
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
  const found = [];
  for (const cell of both) {
    const o2 = objects(O, cell);
    const t = objects(T, cell);
    if (o2.join('\n') === t.join('\n')) continue;
    const [g, s, p] = cell.split('\u0000');
    found.push({
      cell,
      graph: g || null,
      subject: s,
      predicate: p,
      base: objects(B, cell),
      ours: o2,
      theirs: t,
    });
  }
  // resolutions: the most specific one that covers a cell wins
  const res = Array.isArray(o.resolutions) ? o.resolutions : [];
  const covers = (r, c) =>
    (r.graph ?? null) === c.graph &&
    (!r.subject || r.subject === c.subject) &&
    (!r.predicate || r.predicate === c.predicate);
  for (const r of res)
    if (!found.some((c) => covers(r, c)))
      return {
        error: [
          400,
          {
            error: `a resolution covers no conflict: ${JSON.stringify(r)}`,
            code: 'invalid-merge',
          },
        ],
      };
  const rule = o.onConflict && o.onConflict !== 'fail' ? o.onConflict : null;
  const remaining = [];
  const decided = [];
  for (const c of found) {
    const r = res
      .filter((x) => covers(x, c))
      .sort(
        (a, b) =>
          Number(!!b.subject) + Number(!!b.predicate) - Number(!!a.subject) - Number(!!a.predicate),
      )[0];
    const take = r?.take ?? rule;
    if (!take) {
      remaining.push(c);
      continue;
    }
    const objs =
      take === 'ours'
        ? c.ours
        : take === 'theirs'
          ? c.theirs
          : take === 'base'
            ? c.base
            : take === 'union'
              ? [...new Set([...c.ours, ...c.theirs])].sort()
              : (r?.objects ?? []);
    decided.push([c, objs]);
  }
  // the changes: the source's outside the conflicting cells, then the decided cells
  const conflicting = new Set(found.map((c) => c.cell));
  const changes = [];
  for (const [k, q] of theirs) {
    if (ours.has(k) || conflicting.has(cellOf(q))) continue;
    changes.push({ op: T.has(k) ? '+' : '-', quad: q });
  }
  for (const [c, objs] of decided) {
    const keep = new Set(objs);
    for (const q of O.values())
      if (cellOf(q) === c.cell && !keep.has(String(q.object))) changes.push({ op: '-', quad: q });
    const have = new Set(objects(O, c.cell));
    for (const ob of objs)
      if (!have.has(ob))
        changes.push({ op: '+', quad: quadOf(c.subject, c.predicate, ob, c.graph) });
  }
  const inserted = changes.filter((x) => x.op === '+').length;
  return {
    child,
    changes: upToDate ? [] : changes,
    cells: remaining.map(({ cell, ...c }) => (void cell, c)),
    report: {
      upToDate,
      fastForward: !upToDate && ours.size === 0,
      source: { branch: source.branchName, seq: sh },
      target: { branch: target.branchName, seq: th },
      base: baseRef,
      changes: upToDate
        ? { inserted: 0, deleted: 0 }
        : { inserted, deleted: changes.length - inserted },
      conflicts: { found: found.length, resolved: decided.length },
      commit: null,
      inferences: null,
    },
  };
}

/** A change of a merge as the diff JSON lists it. */
const changeJson = ({ op, quad: q }) => ({
  op,
  subject: String(q.subject),
  predicate: String(q.predicate),
  object: String(q.object),
  graph: q.graph.termType === 'DefaultGraph' ? null : String(q.graph),
});

/** The target's state after a planned merge, in a copy. */
function afterMerge(target, plan) {
  const st = copyStore(target.store);
  for (const { op, quad } of plan.changes) {
    if (op === '+') st.add(quad);
    else st.delete(quad);
  }
  return st;
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
  if (seg[0] !== '$') return false;
  if (seg[1] === 'validation' && req.method === 'PUT') {
    const b = seg[2] ? ctx.datasets.get(seg[2]) : undefined;
    if (!b) return (ctx.fail(res, 404, `No such dataset: ${seg[2] ?? ''}`), true);
    const raw = (await ctx.readBody(req)).toString('utf8');
    return (ctx.send(res, ...(await putGuard(b, raw))), true);
  }
  if (seg[1] !== 'branches' && seg[1] !== 'merge' && seg[1] !== 'commit-graph') return false;
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

  if (seg[1] === 'commit-graph') {
    if (req.method !== 'GET') return (fail(res, 405, 'method not allowed'), true);
    return (send(res, ...commitGraph(ds, url)), true);
  }

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
  const plan = planMerge(ds, source, target, req.method === 'POST' ? o : {});
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
  const validation = report.upToDate
    ? null
    : guardReport(target, afterMerge(target, plan), 'merge');
  if (o.dryRun) {
    const listed = Math.max(0, Math.min(Number(o.changes) || 0, 10000));
    const quads = plan.changes.slice(0, listed).map(changeJson);
    const outcome = validation ? 'rejected' : plan.changes.length ? 'commit' : 'no-change';
    return (
      send(res, 200, {
        dryRun: true,
        dataset: ds.name,
        datasetId: ds.id,
        committed: false,
        wouldCommit: outcome === 'commit',
        outcome,
        head: report.target.seq,
        ...(listed
          ? {
              changes: {
                total: plan.changes.length,
                limit: listed,
                truncated: quads.length < plan.changes.length,
                quads,
              },
            }
          : {}),
        ...(validation ? { validation, error: rejection(validation) } : {}),
        merge: { merged: false, ...report },
      }),
      true
    );
  }
  if (report.upToDate) return (send(res, 200, { merged: false, ...report }), true);
  if (validation) return err([422, { error: rejection(validation), validation }]);
  for (const { op, quad } of plan.changes) {
    if (op === '+') target.store.add(quad);
    else target.store.delete(quad);
  }
  const commit = ctx.addCommit(target, 'merge', report.changes.inserted, report.changes.deleted);
  recordChanges(
    commit,
    plan.changes.filter((c) => c.op === '+').map((c) => String(c.quad)),
    plan.changes.filter((c) => c.op === '-').map((c) => String(c.quad)),
  );
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

/** The error message of a merge the guard refuses. */
const rejection = (v) =>
  `SHACL validation failed: ${v.blocking} blocking result${v.blocking === 1 ? '' : 's'} (threshold violation); nothing was committed`;

/** A cursor of the commit graph: `TIMESTAMP_MS.ORDINAL.SEQ`. */
const cursorOf = (k) => k.join('.');
const before = (a, b) => a[0] - b[0] || a[1] - b[1] || a[2] - b[2];

/** `GET /$/commit-graph/{ds}`: the own commits of the branches, newest first, in pages. */
function commitGraph(ds, url) {
  const all = [
    ds,
    ...[...ds.branchSet.values()].sort((a, b) => a.branchName.localeCompare(b.branchName)),
  ];
  const asked = url.searchParams
    .getAll('branches')
    .flatMap((v) => v.split(','))
    .map((s) => s.trim())
    .filter(Boolean);
  for (const n of asked)
    if (!branchOf(ds, n)) return [404, { error: `no such branch: ${n}`, code: 'no-such-branch' }];
  const list = asked.length ? all.filter((b) => asked.includes(b.branchName)) : all;
  const limitArg = url.searchParams.get('limit');
  const limit = limitArg == null ? 100 : Number(limitArg);
  if (!(limit >= 1 && limit <= 1000))
    return [400, { error: 'limit: 1 to 1000', code: 'bad-request' }];
  const b4 = url.searchParams.get('before');
  const cur = b4 ? b4.split('.').map(Number) : null;
  if (cur && (cur.length !== 3 || cur.some((n) => !Number.isFinite(n))))
    return [400, { error: `invalid commit graph cursor '${b4}'`, code: 'bad-request' }];
  const byId = new Map(all.map((b) => [b.branchId, b]));
  /** A commit on the branch that made it: seqs at or below a branch's start are its upstream's. */
  const normalize = (r) => {
    let c = r;
    for (let i = 0; i < 64; i++) {
      const b = byId.get(c.branchId);
      if (!b?.from || c.seq > b.from.seq) return c;
      c = { ...b.from, seq: c.seq };
    }
    return c;
  };
  const named = (r) => ({
    branch: byId.get(r.branchId)?.branchName ?? null,
    branchId: r.branchId,
    seq: r.seq,
  });
  const keyed = [];
  for (const b of list)
    for (const c of b.commits) {
      const k = [Date.parse(c.timestamp), b.ordinal, c.seq];
      if (!cur || before(k, cur) < 0) keyed.push([k, b, c]);
    }
  keyed.sort((a, b) => before(b[0], a[0]));
  const page = keyed.slice(0, limit);
  const commits = page.map(([, b, c]) => {
    const parents = [];
    if (b.commits.some((x) => x.seq === c.seq - 1) || (!b.from && c.seq > 0))
      parents.push({ branch: b.branchName, branchId: b.branchId, seq: c.seq - 1 });
    else if (b.from) parents.push(named(normalize(b.from)));
    if (c.mergedFrom) parents.push(named(normalize(c.mergedFrom)));
    return { ...c, branch: b.branchName, branchId: b.branchId, parents, reconstructable: true };
  });
  const last = page.at(-1);
  const params = (n) =>
    `before=${n}&limit=${limit}${asked.length ? `&branches=${asked.join(',')}` : ''}`;
  return [
    200,
    {
      dataset: ds.name,
      datasetId: ds.id,
      branches: list.map((b) => ({
        name: b.branchName,
        id: b.branchId,
        ordinal: b.ordinal,
        head: head(b).seq,
        modified: head(b).timestamp,
        from: b.from ?? null,
        upstream: b.upstream ?? null,
        created: b.created,
      })),
      commits,
      next:
        keyed.length > limit && last
          ? `/$/commit-graph/${ds.name}?${params(cursorOf(last[0]))}`
          : null,
    },
  ];
}

// ---------------------------------------------------------------------------
// the changes of commits, for `GET /{ds}/diff`

/** The changes each commit made (`[{op, quad}]`), by commit object. */
const changesOf = new WeakMap();

/** Remember what `commit` changed: the N-Quads lines (without " .") it added and removed. */
export function recordChanges(commit, added, removed) {
  const parse = (lines) => {
    if (!lines.length) return [];
    const st = new ox.Store();
    st.load(lines.map((l) => `${l} .`).join('\n'), { format: 'application/n-quads' });
    return st.match();
  };
  changesOf.set(commit, [
    ...parse(removed).map((quad) => ({ op: '-', quad })),
    ...parse(added).map((quad) => ({ op: '+', quad })),
  ]);
}

/**
 * `GET /{ds}/diff?from=&to=&quads=true`: the net change between two commits of a branch,
 * from the changes the mock recorded (none for the seeded history).
 */
export function handleDiff(req, res, url, seg, ctx) {
  if (seg[0] === '$' || seg[1] !== 'diff' || seg.length !== 2 || req.method !== 'GET') return false;
  const ds = ctx.datasets.get(seg[0]);
  if (!ds) return false;
  const list = allCommits(ds);
  const h = head(ds).seq;
  const sel = (v, d) =>
    v == null || v === '' || v === 'head' ? d : Number(String(v).replace(/^commit:/, ''));
  const to = sel(url.searchParams.get('to'), h);
  const from = sel(url.searchParams.get('from'), to - 1);
  if (!Number.isInteger(from) || !Number.isInteger(to) || from < 0 || to > h)
    return (ctx.fail(res, 400, 'the mock compares commit numbers of the branch only'), true);
  const net = new Map();
  for (const c of list.filter((x) => x.seq > from && x.seq <= to))
    for (const ch of changesOf.get(c) ?? []) {
      const k = String(ch.quad);
      const had = net.get(k);
      if (had && had.op !== ch.op) net.delete(k);
      else net.set(k, ch);
    }
  const changes = [...net.values()].sort((a, b) => (a.op === b.op ? 0 : a.op === '-' ? -1 : 1));
  const at = (seq) => list.find((c) => c.seq === seq) ?? { seq, ref: `commit:${seq}` };
  const limit = Number(url.searchParams.get('limit') ?? 1000);
  const added = changes.filter((c) => c.op === '+').length;
  ctx.send(res, 200, {
    dataset: ds.name,
    datasetId: ds.id,
    from: { selector: String(from), commit: at(from) },
    to: { selector: String(to), commit: at(to) },
    added,
    removed: changes.length - added,
    method: 'log',
    ...(url.searchParams.get('quads') === 'true'
      ? { quads: changes.slice(0, limit).map(changeJson) }
      : {}),
  });
  return true;
}
