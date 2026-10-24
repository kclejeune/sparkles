// Mock of the projection route of a vector index (docs/API.md, "Embedding map"):
// `POST /$/vector/{ds}/{name}/projection` computes a projection, in the request for a small
// PCA and as a `vector-projection` task otherwise, and `GET` answers the kept one in JSON or
// in the binary points format, in a region, sampled to a limit and coloured by a predicate.
//
// The projection of `foaf`'s index is a real PCA of its 8-dimensional vectors. The index
// `shipments` of `world` has no vectors in the store. Its 6,000 freight shipments are
// invented here, placed in clusters by cargo class, so that the README's screenshot shows a
// map of a realistic size.

import { vectorRows } from './vector.mjs';

const POINTS_TYPE = 'application/x-sparkles-points';
const MAGIC = Buffer.from([0x53, 0x50, 0x4b, 0x50, 0x54, 0x53, 0x00, 0x01]);
const RDF_TYPE = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';
const MAX_POINTS = 50000;
const GRID = 256;
const NONE = 0xffffffff;
/** A PCA of at most this many rows runs in the request, as on the server. */
const IN_REQUEST = 2000;

const WORLD = 'http://example.org/world/';
/** `MOCK_SHIPMENTS` changes the count, for measuring the map at its 50,000-point cap. */
export const WORLD_SHIPMENTS = Number(process.env.MOCK_SHIPMENTS ?? 6000);
const CARGO = [
  'Electronics',
  'Textiles',
  'Perishables',
  'Machinery',
  'Chemicals',
  'Pharmaceuticals',
  'Automotive',
  'Furniture',
];

/** A small deterministic generator (mulberry32). */
function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** The splitmix-style priority of an entity: the points of a region shown first. */
function priority(s) {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  h ^= h >>> 16;
  h = Math.imul(h, 0x85ebca6b);
  h ^= h >>> 13;
  return h >>> 0;
}

/** `world`'s invented shipments: an entity, its cargo class and a place in two dimensions. */
function worldShipments() {
  const r = rng(1107);
  const gauss = () => {
    const u = Math.max(r(), 1e-9);
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * r());
  };
  // each class is a cluster of two or three lobes; some classes overlap a little
  const centres = CARGO.map((_, c) => {
    const a = (c / CARGO.length) * Math.PI * 2 + 0.3 * Math.sin(c * 2.1);
    const d = 5 + 1.2 * Math.sin(c * 1.7);
    const lobes = 2 + (c % 2);
    return Array.from({ length: lobes }, (_, l) => ({
      x: d * Math.cos(a) + 0.9 * Math.cos(a + l * 2.2),
      y: d * Math.sin(a) + 0.9 * Math.sin(a + l * 2.2),
      s: 0.35 + 0.2 * ((c + l) % 3),
    }));
  });
  const weights = [1.6, 1.1, 1.3, 0.9, 0.6, 0.5, 0.8, 0.7];
  const total = weights.reduce((a, b) => a + b, 0);
  const out = [];
  for (let i = 0; i < WORLD_SHIPMENTS; i++) {
    let pick = r() * total;
    let c = 0;
    while (pick > weights[c]) pick -= weights[c++];
    const lobe = centres[c][Math.floor(r() * centres[c].length)];
    out.push({
      entity: `${WORLD}shipment/${String(i + 1).padStart(5, '0')}`,
      x: lobe.x + gauss() * lobe.s,
      y: lobe.y + gauss() * lobe.s,
      cls: c,
    });
  }
  return { points: out, classes: CARGO.map((c) => `${WORLD}${c}Cargo`), dimension: 64 };
}

/** Two principal components by power iteration with deflation. */
function pca(rows) {
  const n = rows.length;
  const d = rows[0].length;
  const mean = new Array(d).fill(0);
  for (const v of rows) for (let k = 0; k < d; k++) mean[k] += v[k] / n;
  const c = rows.map((v) => v.map((x, k) => x - mean[k]));
  const cov = Array.from({ length: d }, () => new Array(d).fill(0));
  for (const v of c)
    for (let a = 0; a < d; a++) for (let b = 0; b < d; b++) cov[a][b] += v[a] * v[b];
  const trace = cov.reduce((s, row, a) => s + row[a], 0) || 1;
  const comps = [];
  const values = [];
  for (let p = 0; p < 2; p++) {
    let w = Array.from({ length: d }, (_, k) => 1 / Math.sqrt(d) + (k === p ? 0.5 : 0));
    let lambda = 0;
    for (let it = 0; it < 200; it++) {
      const next = cov.map((row) => row.reduce((s, x, k) => s + x * w[k], 0));
      for (const q of comps) {
        const dot = next.reduce((s, x, k) => s + x * q[k], 0);
        for (let k = 0; k < d; k++) next[k] -= dot * q[k];
      }
      const norm = Math.hypot(...next) || 1;
      lambda = norm;
      w = next.map((x) => x / norm);
    }
    comps.push(w);
    values.push(lambda);
  }
  return {
    xy: c.map((v) => comps.map((q) => v.reduce((s, x, k) => s + x * q[k], 0))),
    explained: values.map((l) => Math.round((l / trace) * 1000) / 1000),
  };
}

/** The first rdf:type of each subject. */
function classesOf(ds, subjects) {
  const want = new Set(subjects);
  const first = new Map();
  for (const q of ds.store.match()) {
    if (q.predicate.value !== RDF_TYPE || q.object.termType !== 'NamedNode') continue;
    if (!want.has(q.subject.value)) continue;
    const prev = first.get(q.subject.value);
    if (!prev || q.object.value < prev) first.set(q.subject.value, q.object.value);
  }
  return first;
}

/** Build a projection of the index `name` of `ds`. */
function build(ds, name, e, method) {
  const t0 = Date.now();
  if (e.config.synthetic) {
    const w = worldShipments();
    const counts = new Array(w.classes.length).fill(0);
    for (const p of w.points) counts[p.cls]++;
    return finish(ds, name, method, t0, {
      points: w.points,
      classes: w.classes.map((iri, i) => ({ iri, count: counts[i] })),
      vectors: w.points.length,
      dimension: w.dimension,
      explained: method === 'pca' ? [0.312, 0.184] : undefined,
    });
  }
  const rows = vectorRows(ds, e.config.predicate).filter(
    (r) => r.vector && r.vector.length === e.config.dimension,
  );
  const unit =
    e.config.metric === 'cosine'
      ? rows.map((r) => {
          const n = Math.hypot(...r.vector) || 1;
          return r.vector.map((x) => x / n);
        })
      : rows.map((r) => r.vector);
  const { xy, explained } = rows.length ? pca(unit) : { xy: [], explained: [0, 0] };
  // an entity with several vectors sits at their mean
  const at = new Map();
  rows.forEach((r, i) => {
    const s = at.get(r.subject) ?? { x: 0, y: 0, n: 0 };
    s.x += xy[i][0];
    s.y += xy[i][1];
    s.n++;
    at.set(r.subject, s);
  });
  const subjects = [...at.keys()].sort();
  const cls = classesOf(ds, subjects);
  const classIris = [...new Set(cls.values())].sort();
  const index = new Map(classIris.map((c, i) => [c, i]));
  const counts = new Array(classIris.length).fill(0);
  const points = subjects.map((s) => {
    const p = at.get(s);
    const c = cls.has(s) ? index.get(cls.get(s)) : NONE;
    if (c !== NONE) counts[c]++;
    return { entity: s, x: p.x / p.n, y: p.y / p.n, cls: c };
  });
  return finish(ds, name, method, t0, {
    points,
    classes: classIris.map((iri, i) => ({ iri, count: counts[i] })),
    vectors: rows.length,
    dimension: e.config.dimension,
    explained: method === 'pca' ? explained : undefined,
  });
}

function finish(ds, name, method, t0, p) {
  let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
  for (const q of p.points) {
    q.priority = priority(q.entity);
    x0 = Math.min(x0, q.x);
    y0 = Math.min(y0, q.y);
    x1 = Math.max(x1, q.x);
    y1 = Math.max(y1, q.y);
  }
  if (!p.points.length) [x0, y0, x1, y1] = [0, 0, 0, 0];
  return {
    ...p,
    index: name,
    method,
    bounds: [x0, y0, x1, y1],
    size: ds.store.size,
    commit: ds.commits.at(-1)?.seq ?? 0,
    builtMs: Math.max(1, Date.now() - t0),
  };
}

function kept(ds) {
  ds.projections ??= new Map();
  return ds.projections;
}

function summary(ds, p) {
  return {
    index: p.index,
    method: p.method,
    commit: p.commit,
    generation: ds.type === 'mem' ? 'mem' : 'gen-0001',
    stale: p.size !== ds.store.size,
    vectors: p.vectors,
    dimension: p.dimension,
    entities: p.points.length,
    bounds: p.bounds,
    ...(p.explained ? { explained: p.explained } : {}),
    builtMs: p.builtMs,
    bytes: p.points.length * 24,
  };
}

/** The first object of `predicate` for each entity, as JSON terms. */
function colourValues(ds, predicate, entities) {
  const want = new Set(entities);
  const first = new Map();
  for (const q of ds.store.match()) {
    if (q.predicate.value !== predicate || !want.has(q.subject.value)) continue;
    if (first.has(q.subject.value)) continue;
    const o = q.object;
    first.set(
      q.subject.value,
      o.termType === 'NamedNode'
        ? { type: 'uri', value: o.value }
        : o.termType === 'BlankNode'
          ? { type: 'bnode', value: o.value }
          : {
              type: 'literal',
              value: o.value,
              ...(o.language
                ? { 'xml:lang': o.language }
                : o.datatype.value !== 'http://www.w3.org/2001/XMLSchema#string'
                  ? { datatype: o.datatype.value }
                  : {}),
            },
    );
  }
  return first;
}

function answer(req, res, url, ds, p, send) {
  const q = url.searchParams;
  let bbox = null;
  if (q.has('bbox')) {
    bbox = q.get('bbox').split(',').map(Number);
    if (
      bbox.length !== 4 ||
      bbox.some((x) => !Number.isFinite(x)) ||
      bbox[0] > bbox[2] ||
      bbox[1] > bbox[3]
    )
      return send(res, 400, { error: 'bbox: four numbers, minX,minY,maxX,maxY' });
  }
  const limit = q.has('limit') ? Number(q.get('limit')) : MAX_POINTS;
  if (!Number.isInteger(limit) || limit < 0 || limit > MAX_POINTS)
    return send(res, 400, { error: `limit: 0 to ${MAX_POINTS}` });
  const inRegion = bbox
    ? p.points.filter((r) => r.x >= bbox[0] && r.x <= bbox[2] && r.y >= bbox[1] && r.y <= bbox[3])
    : p.points;
  const shown = [...inRegion]
    .sort((a, b) => a.priority - b.priority || (a.entity < b.entity ? -1 : 1))
    .slice(0, limit)
    .sort((a, b) => (a.entity < b.entity ? -1 : 1));
  const box = bbox ?? p.bounds;
  const counts = new Uint32Array(GRID * GRID);
  const w = box[2] - box[0] || 1;
  const h = box[3] - box[1] || 1;
  let max = 0;
  for (const r of inRegion) {
    const c = Math.min(GRID - 1, Math.floor(((r.x - box[0]) / w) * GRID));
    const k = Math.min(GRID - 1, Math.floor(((r.y - box[1]) / h) * GRID));
    max = Math.max(max, ++counts[k * GRID + c]);
  }
  const head = {
    ...summary(ds, p),
    region: {
      bbox: box,
      total: inRegion.length,
      shown: shown.length,
      sampled: shown.length < inRegion.length,
    },
    density: {
      size: GRID,
      bbox: box,
      max,
      counts: Buffer.from(counts.buffer).toString('base64'),
    },
    classes: p.classes,
  };
  const colour = q.get('colour');
  let colours = null;
  if (colour) {
    const values = colourValues(
      ds,
      colour,
      shown.map((r) => r.entity),
    );
    const dict = new Map();
    const list = [];
    colours = shown.map((r) => {
      const t = values.get(r.entity);
      if (!t) return NONE;
      const key = JSON.stringify(t);
      if (!dict.has(key)) {
        dict.set(key, list.length);
        list.push({ value: t, count: 0 });
      }
      const i = dict.get(key);
      list[i].count++;
      return i;
    });
    head.colour = { predicate: colour, values: list };
  }
  const binary = (req.headers.accept ?? '').includes(POINTS_TYPE);
  if (!binary) {
    head.points = shown.map((r, i) => ({
      entity: r.entity,
      x: r.x,
      y: r.y,
      ...(r.cls !== NONE ? { class: r.cls } : {}),
      ...(colours && colours[i] !== NONE ? { colour: colours[i] } : {}),
    }));
    return send(res, 200, head);
  }
  const terms = Buffer.from(shown.map((r) => r.entity).join('\n'), 'utf8');
  const hj = Buffer.from(
    JSON.stringify({
      ...head,
      count: shown.length,
      termsBytes: terms.length,
      hasColour: !!colours,
    }),
    'utf8',
  );
  const n = shown.length;
  let at = 12 + hj.length + terms.length;
  const pad = (4 - (at % 4)) % 4;
  at += pad;
  const buf = Buffer.alloc(at + n * 12 + (colours ? n * 4 : 0));
  MAGIC.copy(buf, 0);
  buf.writeUInt32LE(hj.length, 8);
  hj.copy(buf, 12);
  terms.copy(buf, 12 + hj.length);
  shown.forEach((r, i) => {
    buf.writeFloatLE(r.x, at + i * 8);
    buf.writeFloatLE(r.y, at + i * 8 + 4);
  });
  at += n * 8;
  shown.forEach((r, i) => buf.writeUInt32LE(r.cls >>> 0, at + i * 4));
  at += n * 4;
  if (colours) colours.forEach((c, i) => buf.writeUInt32LE(c >>> 0, at + i * 4));
  return send(res, 200, buf, POINTS_TYPE);
}

/**
 * Answer a projection request; false when it is not one. `ctx` holds the vector indexes'
 * lookup, the tasks, `nextTaskId`, `send` and `readBody`.
 */
export async function handleProjection(req, res, url, seg, ctx) {
  // cancelling a projection task (other tasks are mock/backups.mjs's)
  if (seg[0] === '$' && seg[1] === 'tasks' && seg[2] && req.method === 'DELETE') {
    const t = ctx.tasks.find((x) => x.id === seg[2] && x.kind === 'vector-projection');
    if (!t) return false;
    if (t.state !== 'running')
      return (ctx.send(res, 409, { error: `task ${t.id} has finished` }), true);
    t.state = 'cancelled';
    t.message = `cancelled at ${Math.round((t.progress ?? 0) * 100)}%`;
    t.finishedAt = new Date().toISOString();
    ctx.send(res, 202, t);
    return true;
  }
  if (seg[0] !== '$' || seg[1] !== 'vector' || seg[4] !== 'projection' || seg.length !== 5)
    return false;
  const { send } = ctx;
  const [, , dsName, name] = seg;
  const ds = ctx.datasets.get(dsName);
  if (!ds) return (send(res, 404, { error: `No such dataset: ${dsName}` }), true);
  const e = ds.vectorIndexes?.get(name);
  if (!e) return (send(res, 404, { error: `no vector index ${name}` }), true);
  const m = url.searchParams.get('method');
  if (m && !['pca', 'neighbours', 'neighbors'].includes(m))
    return (send(res, 400, { error: `method: pca or neighbours, not ${m}` }), true);
  const method = m === 'neighbors' ? 'neighbours' : m;

  if (req.method === 'GET') {
    const p = method
      ? kept(ds).get(`${name}|${method}`)
      : (kept(ds).get(`${name}|pca`) ?? kept(ds).get(`${name}|neighbours`));
    if (!p)
      return (
        send(res, 404, {
          error: `vector index ${name} has no ${method ? method + ' ' : ''}projection yet: POST to /$/vector/${dsName}/${name}/projection computes one`,
        }),
        true
      );
    answer(req, res, url, ds, p, send);
    return true;
  }
  if (req.method !== 'POST') return (send(res, 405, { error: 'method not allowed' }), true);
  await ctx.readBody(req);
  const how = method ?? 'pca';
  const rows = e.config.synthetic ?? vectorRows(ds, e.config.predicate).length;
  if (how === 'pca' && rows <= IN_REQUEST) {
    const p = build(ds, name, e, how);
    kept(ds).set(`${name}|${how}`, p);
    send(res, 200, summary(ds, p));
    return true;
  }
  const task = {
    id: ctx.nextTaskId(),
    kind: 'vector-projection',
    dataset: dsName,
    state: 'running',
    startedAt: new Date().toISOString(),
    progress: 0,
    message: 'sampling vectors',
    cancellable: true,
  };
  ctx.tasks.unshift(task);
  const steps = ['sampling vectors', 'projecting', 'laying out the neighbours', 'placing entities'];
  let k = 0;
  const timer = setInterval(() => {
    if (task.state === 'cancelled') {
      clearInterval(timer);
      return;
    }
    k++;
    task.progress = Math.min(1, k / steps.length);
    task.message = steps[Math.min(k, steps.length - 1)];
    if (k < steps.length) return;
    clearInterval(timer);
    const p = build(ds, name, e, how);
    kept(ds).set(`${name}|${how}`, p);
    task.state = 'done';
    task.finishedAt = new Date().toISOString();
    task.message = `vector index ${name}: ${how} projection of ${p.points.length} entities`;
    delete task.progress;
  }, 350);
  send(res, 202, task, 'application/json', { Location: `/$/tasks/${task.id}` });
  return true;
}
