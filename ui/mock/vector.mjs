// Mock of the vector index endpoints for UI development (docs/API.md, "Vector indexes"):
// `GET /$/vector/{ds}`, `GET`, `PUT` and `DELETE /$/vector/{ds}/{name}`, `POST …/rebuild`
// (a `vector-index` task) and `POST …/recall`. `vectorIndexFor` tells the query endpoint's
// `spk:vectorSearch` emulation which index a search goes through and how it would run.
//
// Seeded on the first request: `foaf` has the index `embedding` over `ex:embedding`
// (8 dimensions, cosine) with an exact threshold of 10 rows, so its searches take the
// HNSW path. `ex:bioEmbedding` has no index and is listed as a packed predicate once it
// has been searched. Builds take about three seconds. With `MOCK_ROLE=dataset-admin` the
// caller is an admin of `foaf` and a reader of `scratch`, as in mock/backups.mjs.

const VECTOR_DT = 'urn:x-sparkles:vector';
const EX = 'http://example.org/ontology#';
const ROLE = process.env.MOCK_ROLE ?? 'server-admin';
const ROLE_DATASETS = { foaf: 'admin', scratch: 'read' };
const NAME = /^[A-Za-z0-9_-][A-Za-z0-9_.-]{0,63}$/;
const METRICS = ['cosine', 'dot', 'euclidean'];
const BUDGET = 4096 * 1024 * 1024;

function parseVec(s) {
  try {
    const v = JSON.parse(s);
    return Array.isArray(v) && v.length && v.every((x) => Number.isFinite(x)) ? v : null;
  } catch {
    return null;
  }
}

/** The vector literals of a predicate: `{ subject, value, vector }`, vector null when malformed. */
export function vectorRows(ds, predicate) {
  return ds.store
    .match()
    .filter(
      (q) =>
        q.predicate.value === predicate &&
        q.object.termType === 'Literal' &&
        q.object.datatype.value === VECTOR_DT,
    )
    .map((q) => ({
      subject: q.subject.value,
      value: q.object.value,
      vector: parseVec(q.object.value),
    }));
}

const level = (ds) => (ROLE === 'dataset-admin' ? (ROLE_DATASETS[ds] ?? null) : 'admin');

function indexes(ds) {
  ds.vectorIndexes ??= new Map();
  return ds.vectorIndexes;
}

/** Packed predicates without an index, remembered from searches (`touchPacked`). */
function packed(ds) {
  ds.vectorPacked ??= new Set();
  return ds.vectorPacked;
}

/** The search of `predicate` packed its vectors (as the server does on a first search). */
export function touchPacked(ds, predicate) {
  if (![...indexes(ds).values()].some((e) => e.config.predicate === predicate))
    packed(ds).add(predicate);
}

function counts(ds, cfg) {
  const rows = vectorRows(ds, cfg.predicate);
  let ok = 0;
  const skipped = { malformed: 0, wrongDimension: 0, zeroNorm: 0 };
  for (const r of rows) {
    if (!r.vector) skipped.malformed++;
    else if (r.vector.length !== cfg.dimension) skipped.wrongDimension++;
    else {
      ok++;
      if (r.vector.every((x) => x === 0)) skipped.zeroNorm++;
    }
  }
  return { ok, skipped };
}

function status(ds, name) {
  const e = indexes(ds).get(name);
  const cfg = e.config;
  const { ok, skipped } = counts(ds, cfg);
  const base = e.building ? 0 : e.baseRows;
  const rows = e.building ? 0 : base;
  const diff = e.building ? 0 : ok - base;
  const hnswBytes = cfg.hnsw ? rows * ((2 * cfg.hnsw.m + 1) * 4 + 4) : 0;
  const persistent = ds.type !== 'mem';
  return {
    name,
    predicate: cfg.predicate,
    dimension: cfg.dimension,
    metric: cfg.metric,
    ...(cfg.model ? { model: cfg.model } : {}),
    state: e.building ? 'building' : 'ready',
    ...(e.building ? { progress: e.building.progress ?? 0, message: 'packing vectors' } : {}),
    generation: persistent ? 'gen-0001' : 'mem',
    rows,
    overlay: { inserts: Math.max(0, diff), deletes: Math.max(0, -diff) },
    skipped,
    memory: {
      segmentBytes: rows * (4 * cfg.dimension + 28),
      hnswBytes,
      residency: persistent && !e.lastBuild ? 'mmap' : 'heap',
    },
    hnsw:
      cfg.hnsw && !e.building
        ? { ...cfg.hnsw, nodes: rows, layers: rows > 16 ? 2 : 1 }
        : cfg.hnsw
          ? { ...cfg.hnsw, nodes: 0, layers: 0 }
          : null,
    exactThreshold: cfg.exactThreshold,
    ...(persistent && !e.building
      ? {
          files: {
            bytes: 4096 + rows * (4 * cfg.dimension + 28) + hnswBytes,
            opened: !e.lastBuild,
          },
        }
      : {}),
    ...(e.lastBuild ? { lastBuild: e.lastBuild } : {}),
  };
}

/** A `vector-index` task that follows a build of a few seconds. */
function build(ds, name, ctx, message) {
  const e = indexes(ds).get(name);
  const task = {
    id: ctx.nextTaskId(),
    kind: 'vector-index',
    dataset: ds.name,
    state: 'running',
    startedAt: new Date().toISOString(),
    progress: 0,
    message,
  };
  ctx.tasks.unshift(task);
  e.building = task;
  const t0 = Date.now();
  const timer = setInterval(() => {
    task.progress = Math.min(1, task.progress + 0.2);
    if (task.progress < 1) return;
    clearInterval(timer);
    if (indexes(ds).get(name) !== e) {
      task.state = 'failed';
      task.message = `vector index ${name} was dropped`;
    } else {
      e.building = null;
      e.baseRows = counts(ds, e.config).ok;
      e.lastBuild = { at: new Date().toISOString(), ms: Date.now() - t0, rows: e.baseRows };
      task.state = 'done';
      task.message = `vector index ${name}: ${e.baseRows} rows, ready`;
    }
    task.finishedAt = new Date().toISOString();
    delete task.progress;
  }, 600);
  return task;
}

/** Give `foaf` its index (called once the datasets are loaded). */
export function seedVectors(datasets) {
  const foaf = datasets.get('foaf');
  if (!foaf) return;
  const config = {
    predicate: `${EX}embedding`,
    dimension: 8,
    metric: 'cosine',
    model: 'mock-embed-8',
    hnsw: { m: 16, efConstruction: 128, efSearch: 128 },
    exactThreshold: 10,
  };
  indexes(foaf).set('embedding', {
    config,
    building: null,
    baseRows: counts(foaf, config).ok,
    lastBuild: null,
  });
}

/** A configuration from a `PUT` body, with the defaults filled in, or an error message. */
function readConfig(body) {
  const known = ['predicate', 'dimension', 'metric', 'model', 'hnsw', 'exactThreshold'];
  const unknown = Object.keys(body).find((k) => !known.includes(k));
  if (unknown) return `unknown field \`${unknown}\``;
  if (typeof body.predicate !== 'string' || !/^[a-z][a-z0-9+.-]*:\S+$/i.test(body.predicate))
    return `predicate: ${JSON.stringify(body.predicate)} is not an IRI`;
  if (!Number.isInteger(body.dimension) || body.dimension < 1 || body.dimension > 16384)
    return 'dimension: must be 1..=16384';
  const metric = body.metric ?? 'cosine';
  if (!METRICS.includes(metric)) return `metric: unknown variant \`${metric}\``;
  let hnsw = { m: 16, efConstruction: 128, efSearch: 128 };
  if (body.hnsw === false || body.hnsw === null) hnsw = null;
  else if (body.hnsw && typeof body.hnsw === 'object') hnsw = { ...hnsw, ...body.hnsw };
  if (hnsw) {
    if (!(hnsw.m >= 2 && hnsw.m <= 128)) return 'hnsw.m: must be 2..=128';
    if (!(hnsw.efConstruction >= 1 && hnsw.efConstruction <= 4096))
      return 'hnsw.efConstruction: must be 1..=4096';
    if (!(hnsw.efSearch >= 1 && hnsw.efSearch <= 4096)) return 'hnsw.efSearch: must be 1..=4096';
  }
  if (typeof body.model === 'string' && body.model.length > 256) return 'model: at most 256 bytes';
  return {
    predicate: body.predicate,
    dimension: body.dimension,
    metric,
    ...(typeof body.model === 'string' && body.model ? { model: body.model } : {}),
    hnsw,
    exactThreshold: body.exactThreshold ?? 10000,
  };
}

const sameBuild = (a, b) =>
  a.predicate === b.predicate &&
  a.dimension === b.dimension &&
  a.metric === b.metric &&
  !a.hnsw === !b.hnsw &&
  (!a.hnsw || (a.hnsw.m === b.hnsw.m && a.hnsw.efConstruction === b.hnsw.efConstruction));

/**
 * The index of `predicate` for a search of dimension `dim`, and how the search runs
 * (`method`, `exactBecause`, `ef`), as the plan counters report it. Throws a message for
 * a query of another dimension than the index's.
 */
export function vectorIndexFor(ds, predicate, dim, opts) {
  const found = [...indexes(ds)].find(([, e]) => e.config.predicate === predicate);
  if (!found) return null;
  const [name, e] = found;
  const cfg = e.config;
  if (dim != null && dim !== cfg.dimension)
    return {
      error: `dimension mismatch: <${predicate}> is indexed with dimension ${cfg.dimension} (index ${name}); query has ${dim}`,
    };
  const metric = opts.metric ?? cfg.metric;
  const rows = e.building ? 0 : e.baseRows;
  // the reasons of the server's plan counters (crates/sparkles/src/vector/search.rs)
  let exactBecause = null;
  if (!cfg.hnsw) exactBecause = null;
  else if (opts.exact) exactBecause = 'exact:true';
  else if (e.building) exactBecause = 'the graph is being built';
  else if (metric !== cfg.metric) exactBecause = "the query's metric is not the index's";
  else if (rows <= cfg.exactThreshold) exactBecause = 'few rows';
  const ef = Math.max(opts.ef ?? cfg.hnsw?.efSearch ?? 0, opts.k ?? 0);
  const ann = cfg.hnsw && !exactBecause;
  return {
    name,
    metric,
    counters: {
      method: ann ? 'hnsw' : 'exact',
      ...(exactBecause ? { exactBecause } : {}),
      index: name,
      ...(ann ? { ef } : {}),
      rows,
      scored: ann ? Math.min(rows, ef) : rows,
      overlayInserts: 0,
      overlayDeletes: 0,
    },
  };
}

async function readJson(req, ctx) {
  const body = (await ctx.readBody(req)).toString('utf8').trim();
  return body ? JSON.parse(body) : {};
}

/**
 * Answer a vector index request; false when it is not one. `ctx` holds the server's
 * state and helpers (datasets, tasks, nextTaskId, send, readBody).
 */
export async function handleVector(req, res, url, seg, ctx) {
  if (seg[0] !== '$' || seg[1] !== 'vector') return false;
  const fail = (status, error) => (ctx.send(res, status, { error }), true);
  const [, , dsName, name, extra] = seg;
  const ds = dsName ? ctx.datasets.get(dsName) : undefined;
  if (!ds || !level(dsName)) return fail(404, `No such dataset: ${dsName ?? ''}`);
  const admin = () => level(dsName) === 'admin';

  if (!name) {
    if (req.method !== 'GET') return fail(405, 'method not allowed');
    const list = [...indexes(ds).keys()].sort().map((n) => status(ds, n));
    const predicates = [...packed(ds)].sort().map((p) => {
      const rows = vectorRows(ds, p);
      const dims = new Map();
      for (const r of rows)
        if (r.vector) dims.set(r.vector.length, (dims.get(r.vector.length) ?? 0) + 1);
      return {
        predicate: p,
        bytes: rows.reduce((n, r) => n + (r.vector ? 4 * r.vector.length + 28 : 0), 0),
        malformed: rows.filter((r) => !r.vector).length,
        dimensions: [...dims]
          .sort((a, b) => a[0] - b[0])
          .map(([dimension, vectors]) => ({ dimension, vectors })),
      };
    });
    const used =
      list.reduce((n, s) => n + s.memory.segmentBytes + s.memory.hnswBytes, 0) +
      predicates.reduce((n, p) => n + p.bytes, 0);
    ctx.send(res, 200, {
      budgetBytes: BUDGET,
      usedBytes: used,
      generation: ds.type === 'mem' ? 'mem' : 'gen-0001',
      indexes: list,
      predicates,
    });
    return true;
  }

  if (!NAME.test(name))
    return fail(
      400,
      `index name ${JSON.stringify(name)}: 1 to 64 letters, digits, '_', '-' or '.', not starting with '.'`,
    );
  const e = indexes(ds).get(name);

  if (extra === 'rebuild') {
    if (req.method !== 'POST') return fail(405, 'method not allowed');
    if (!admin()) return fail(403, `requires admin on ${dsName}`);
    if (!e) return fail(404, `no vector index ${name}`);
    ctx.send(res, 202, build(ds, name, ctx, 'rebuilding the vector index'));
    return true;
  }
  if (extra === 'recall') {
    if (req.method !== 'POST') return fail(405, 'method not allowed');
    if (!e) return fail(404, `no vector index ${name}`);
    if (!e.config.hnsw || e.building) return fail(400, `vector index ${name} has no graph ready`);
    const p = url.searchParams;
    const num = (k, d, max) => {
      if (!p.has(k)) return d;
      const n = Number(p.get(k));
      return Number.isInteger(n) && n >= 1 && n <= max ? n : NaN;
    };
    const samples = num('samples', 100, 10000);
    const k = num('k', 10, 10000);
    const ef = num('ef', e.config.hnsw.efSearch, 4096);
    for (const [key, v, max] of [
      ['samples', samples, 10000],
      ['k', k, 10000],
      ['ef', ef, 4096],
    ])
      if (Number.isNaN(v)) return fail(400, `${key}: 1 to ${max}`);
    const rows = e.baseRows;
    if (!rows) return fail(400, 'the index is empty, or samples or k is 0');
    // a small graph finds everything; a low ef on a large k misses a little
    const recall = Math.min(1, 0.9 + (0.1 * ef) / Math.max(ef, 4 * k));
    ctx.send(res, 200, {
      k,
      samples: Math.min(samples, rows),
      ef: Math.max(ef, k),
      recall: Math.round(recall * 10000) / 10000,
      hnswMs: 0.04 + ef / 20000,
      exactMs: 0.09 + rows / 50000,
    });
    return true;
  }
  if (extra) return fail(404, `Unknown endpoint ${url.pathname}`);

  switch (req.method) {
    case 'GET':
      return e
        ? (ctx.send(res, 200, status(ds, name)), true)
        : fail(404, `no vector index ${name}`);
    case 'PUT': {
      if (!admin()) return fail(403, `requires admin on ${dsName}`);
      let cfg;
      try {
        cfg = readConfig(await readJson(req, ctx));
      } catch (err) {
        return fail(400, `invalid vector index configuration: ${err.message}`);
      }
      if (typeof cfg === 'string') return fail(400, `invalid vector index configuration: ${cfg}`);
      const other = [...indexes(ds)].find(
        ([n, x]) => n !== name && x.config.predicate === cfg.predicate,
      );
      if (other)
        return fail(409, `<${cfg.predicate}> is already indexed by vector index ${other[0]}`);
      if (!e && indexes(ds).size >= 64) return fail(400, 'a dataset has at most 64 vector indexes');
      packed(ds).delete(cfg.predicate);
      const keep = e && !e.building && sameBuild(e.config, cfg);
      const entry = keep
        ? { ...e, config: cfg }
        : { config: cfg, building: null, baseRows: 0, lastBuild: null };
      indexes(ds).set(name, entry);
      let task;
      if (keep) {
        task = {
          id: ctx.nextTaskId(),
          kind: 'vector-index',
          dataset: ds.name,
          state: 'done',
          startedAt: new Date().toISOString(),
          finishedAt: new Date().toISOString(),
          message: `vector index ${name}: ${entry.baseRows} rows, ready`,
        };
        ctx.tasks.unshift(task);
      } else task = build(ds, name, ctx, 'building the vector index');
      ctx.send(res, e ? 200 : 201, { index: status(ds, name), task });
      return true;
    }
    case 'DELETE':
      if (!admin()) return fail(403, `requires admin on ${dsName}`);
      if (!e) return fail(404, `no vector index ${name}`);
      indexes(ds).delete(name);
      res.writeHead(204, { 'Access-Control-Allow-Origin': '*' });
      res.end();
      return true;
  }
  return fail(405, 'method not allowed');
}
