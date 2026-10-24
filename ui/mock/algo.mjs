// F14 in the mock, for the algorithm lenses of U10 Phase 2: `SERVICE algo:` calls with a
// nested pattern over the drawn nodes, which the mock answers by rewriting the query into a
// `VALUES` block of computed rows, and `POST /$/algo/{ds}/run` with the view-only members
// `nodes`, `keep`, `async` and `top`, with a cache of kept results and `algo-view` tasks
// that `DELETE /$/tasks/{id}` cancels. The algorithms are small textbook versions that agree
// with the server's on the acceptance data, not ports of it.

const NS = 'urn:x-sparkles:algo#';
const RDF_TYPE = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';

const OUTPUTS = {
  pageRank: ['score'],
  betweenness: ['score'],
  bfs: ['depth'],
  kCore: ['core'],
  triangleCount: ['triangles'],
  wcc: ['component'],
  labelPropagation: ['community'],
  louvain: ['community', 'level'],
  leiden: ['community', 'level'],
};

/** A seeded random generator (mulberry32). */
function rng(seed) {
  let a = seed >>> 0 || 1;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function graphOf(nodes, edges) {
  const index = new Map(nodes.map((n, i) => [n, i]));
  const out = nodes.map(() => []);
  const und = nodes.map(() => new Set());
  for (const [s, o] of edges) {
    const a = index.get(s);
    const b = index.get(o);
    if (a == null || b == null || a === b) continue;
    out[a].push(b);
    und[a].add(b);
    und[b].add(a);
  }
  return { n: nodes.length, out, und: und.map((s) => [...s]) };
}

function pageRank(g, p) {
  const d = Number(p.dampingFactor ?? 0.85);
  const iters = Number(p.maxIterations ?? 20);
  let r = new Array(g.n).fill(1 / g.n);
  let it = 0;
  let converged = false;
  for (; it < iters; it++) {
    const next = new Array(g.n).fill((1 - d) / g.n);
    let dangling = 0;
    for (let i = 0; i < g.n; i++) {
      if (!g.out[i].length) dangling += r[i];
      else for (const j of g.out[i]) next[j] += (d * r[i]) / g.out[i].length;
    }
    for (let i = 0; i < g.n; i++) next[i] += (d * dangling) / g.n;
    const delta = next.reduce((s, v, i) => s + Math.abs(v - r[i]), 0);
    r = next;
    if (delta < 1e-7) {
      converged = true;
      it++;
      break;
    }
  }
  return { cols: [r], summary: { iterations: it, converged } };
}

function bfsFrom(g, src, adj) {
  const dist = new Array(g.n).fill(-1);
  if (src == null) return dist;
  dist[src] = 0;
  const q = [src];
  for (let k = 0; k < q.length; k++)
    for (const j of adj[q[k]]) if (dist[j] < 0) ((dist[j] = dist[q[k]] + 1), q.push(j));
  return dist;
}

function betweenness(g) {
  const cb = new Array(g.n).fill(0);
  for (let s = 0; s < g.n; s++) {
    const stack = [];
    const pred = g.out.map(() => []);
    const sigma = new Array(g.n).fill(0);
    const dist = new Array(g.n).fill(-1);
    sigma[s] = 1;
    dist[s] = 0;
    const q = [s];
    for (let k = 0; k < q.length; k++) {
      const v = q[k];
      stack.push(v);
      for (const w of g.out[v]) {
        if (dist[w] < 0) ((dist[w] = dist[v] + 1), q.push(w));
        if (dist[w] === dist[v] + 1) ((sigma[w] += sigma[v]), pred[w].push(v));
      }
    }
    const delta = new Array(g.n).fill(0);
    while (stack.length) {
      const w = stack.pop();
      for (const v of pred[w]) delta[v] += (sigma[v] / sigma[w]) * (1 + delta[w]);
      if (w !== s) cb[w] += delta[w];
    }
  }
  return { cols: [cb], summary: {} };
}

function kCore(g) {
  const deg = g.und.map((a) => a.length);
  const core = new Array(g.n).fill(0);
  const removed = new Array(g.n).fill(false);
  let k = 0;
  for (let left = g.n; left > 0;) {
    let i = -1;
    for (let j = 0; j < g.n; j++) if (!removed[j] && (i < 0 || deg[j] < deg[i])) i = j;
    k = Math.max(k, deg[i]);
    core[i] = k;
    removed[i] = true;
    left--;
    for (const j of g.und[i]) if (!removed[j]) deg[j]--;
  }
  return { cols: [core], summary: {} };
}

function triangles(g) {
  const sets = g.und.map((a) => new Set(a));
  const t = new Array(g.n).fill(0);
  for (let i = 0; i < g.n; i++)
    for (const j of g.und[i])
      if (j > i) for (const k of g.und[j]) if (k > j && sets[i].has(k)) (t[i]++, t[j]++, t[k]++);
  return { cols: [t], summary: {} };
}

function wcc(g) {
  const comp = new Array(g.n).fill(-1);
  let c = 0;
  for (let i = 0; i < g.n; i++) {
    if (comp[i] >= 0) continue;
    for (const [j, d] of bfsFrom(g, i, g.und).entries()) if (d >= 0) comp[j] = c;
    c++;
  }
  return { cols: [comp], summary: { components: c } };
}

function labelPropagation(g, p) {
  const label = [...Array(g.n).keys()];
  const iters = Number(p.maxIterations ?? 10);
  let it = 0;
  for (; it < iters; it++) {
    let changed = false;
    for (let i = 0; i < g.n; i++) {
      const counts = new Map();
      for (const j of g.und[i]) counts.set(label[j], (counts.get(label[j]) ?? 0) + 1);
      let best = label[i];
      let n = 0;
      for (const [l, k] of counts) if (k > n || (k === n && l < best)) ((best = l), (n = k));
      if (best !== label[i]) ((label[i] = best), (changed = true));
    }
    if (!changed) break;
  }
  return { cols: [renumber(label)], summary: { iterations: it, converged: it < iters } };
}

const renumber = (labels) => {
  const m = new Map();
  return labels.map((l) => (m.has(l) ? m.get(l) : (m.set(l, m.size), m.size - 1)));
};

/** One level of Louvain's local moving, in a seeded order, then the modularity. */
function louvain(g, p) {
  const res = Number(p.resolution ?? 1);
  const rand = rng(Number(p.seed ?? 0) + 1);
  const m2 = g.und.reduce((s, a) => s + a.length, 0) || 1;
  const k = g.und.map((a) => a.length);
  const comm = [...Array(g.n).keys()];
  const tot = [...k];
  const order = [...Array(g.n).keys()];
  for (let i = order.length - 1; i > 0; i--) {
    const j = Math.floor(rand() * (i + 1));
    [order[i], order[j]] = [order[j], order[i]];
  }
  for (let pass = 0; pass < 20; pass++) {
    let moved = false;
    for (const i of order) {
      const links = new Map();
      for (const j of g.und[i]) links.set(comm[j], (links.get(comm[j]) ?? 0) + 1);
      tot[comm[i]] -= k[i];
      let best = comm[i];
      let gain = (links.get(comm[i]) ?? 0) - (res * tot[comm[i]] * k[i]) / m2;
      for (const [c, l] of links) {
        const gc = l - (res * tot[c] * k[i]) / m2;
        if (gc > gain + 1e-12) ((gain = gc), (best = c));
      }
      tot[best] += k[i];
      if (best !== comm[i]) ((comm[i] = best), (moved = true));
    }
    if (!moved) break;
  }
  const c = renumber(comm);
  let q = 0;
  const totc = new Map();
  for (let i = 0; i < g.n; i++) totc.set(c[i], (totc.get(c[i]) ?? 0) + k[i]);
  for (let i = 0; i < g.n; i++) for (const j of g.und[i]) if (c[i] === c[j]) q += 1;
  for (const t of totc.values()) q -= (res * t * t) / m2;
  q /= m2;
  const count = new Set(c).size;
  return {
    cols: [c, new Array(g.n).fill(0)],
    summary: { levels: 1, modularity: q, communities: count },
  };
}

const ALGOS = {
  pageRank,
  betweenness,
  bfs: (g, p, nodes) => ({ cols: [bfsFrom(g, nodes.indexOf(p.source), g.out)], summary: {} }),
  kCore,
  triangleCount: triangles,
  wcc,
  labelPropagation,
  louvain,
  leiden: louvain,
};

/** Rows `[node, ...outputs]` of an algorithm over the nodes and edges, with its summary. */
export function compute(algo, nodes, edges, params) {
  const fn = ALGOS[algo];
  if (!fn) throw new Error(`unknown algorithm '${algo}'`);
  const g = graphOf(nodes, edges);
  const { cols, summary } = fn(g, params, nodes);
  const rows = [];
  for (let i = 0; i < nodes.length; i++) {
    // BFS leaves out the nodes it does not reach
    if (algo === 'bfs' && cols[0][i] < 0) continue;
    rows.push([nodes[i], ...cols.map((c) => c[i])]);
  }
  return { vars: ['node', ...OUTPUTS[algo]], rows, summary: { nodes: g.n, ...summary } };
}

const iriList = (text) => [...text.matchAll(/<([^<>\s]*)>/g)].map((m) => m[1]);

/**
 * A query with `SERVICE algo:name { [] algo:start ?s ; algo:end ?o ; … . VALUES ?s {…} VALUES
 * ?o {…} ?s ?p ?o . }` as the same query over a `VALUES` block of the computed rows, or null
 * for any other query.
 */
export function rewriteService(store, query, options) {
  const m = /SERVICE\s+algo:(\w+)\s*\{([\s\S]*)\}\s*\}\s*$/.exec(query);
  if (!m || !query.includes(NS)) return null;
  const [, algo, body] = m;
  const vs = /VALUES\s+\?s\s*\{([^}]*)\}/.exec(body);
  const vo = /VALUES\s+\?o\s*\{([^}]*)\}/.exec(body);
  if (!vs || !vo) return null;
  const s = iriList(vs[1]);
  const o = iriList(vo[1]);
  const params = {};
  for (const [, k, v] of body.matchAll(/algo:(\w+)\s+([0-9.eE+-]+|<[^>]*>)/g))
    params[k] = v.startsWith('<') ? v.slice(1, -1) : Number(v);
  const vars = {};
  for (const [, k, v] of body.matchAll(/algo:(\w+)\s+\?(\w+)/g)) vars[k] = v;
  const edges = [];
  if (s.length && o.length) {
    const q = `SELECT DISTINCT ?s ?o WHERE { VALUES ?s { ${s.map((x) => `<${x}>`).join(' ')} } VALUES ?o { ${o.map((x) => `<${x}>`).join(' ')} } ?s ?p ?o }`;
    for (const b of store.query(q, options)) edges.push([b.get('s').value, b.get('o').value]);
  }
  const nodes = [...new Set(edges.flat())];
  const r = compute(algo, nodes, edges, params);
  const summary = JSON.stringify(r.summary);
  const cols = r.vars.filter((v) => vars[v]);
  const head = [...cols.map((c) => vars[c]), ...(vars.summary ? [vars.summary] : [])];
  const lit = (v) =>
    typeof v === 'number'
      ? Number.isInteger(v)
        ? String(v)
        : `"${v}"^^<http://www.w3.org/2001/XMLSchema#double>`
      : `<${v}>`;
  const rows = r.rows
    .map(
      (row) =>
        `(${cols.map((c) => lit(row[r.vars.indexOf(c)])).join(' ')}${vars.summary ? ` ${JSON.stringify(summary)}` : ''})`,
    )
    .join(' ');
  const proj = /SELECT\s+([\s\S]*?)\s+WHERE/i.exec(query)?.[1] ?? '*';
  return `SELECT ${proj} WHERE { VALUES (${head.map((v) => `?${v}`).join(' ')}) { ${rows} } }`;
}

// ---------------------------------------------------------------------------
// POST /$/algo/{ds}/run with view-only members, and the algo-view tasks

const kept = new Map();
const running = new Map();
const owned = new Set();
const VIEW = ['nodes', 'keep', 'async', 'top'];

function projectionEdges(store, projection, options) {
  const preds = Array.isArray(projection?.predicates) ? projection.predicates : null;
  const filter = preds
    ? `VALUES ?p { ${preds.map((p) => `<${typeof p === 'string' ? p : p.iri}>`).join(' ')} }`
    : `FILTER(?p != <${RDF_TYPE}>)`;
  const q = `SELECT DISTINCT ?s ?o WHERE { ${filter} ?s ?p ?o FILTER(isIRI(?s) && isIRI(?o)) }`;
  return store.query(q, options).map((b) => [b.get('s').value, b.get('o').value]);
}

function answer(result, view, cached, commit) {
  const { vars, rows, summary } = result;
  const groupCol = vars.findIndex((v) => v === 'community' || v === 'component');
  const levelCol = vars.indexOf('level');
  const sizes = new Map();
  if (groupCol >= 0)
    for (const r of rows) {
      const k = `${levelCol >= 0 ? r[levelCol] : ''}|${r[groupCol]}`;
      sizes.set(k, (sizes.get(k) ?? 0) + 1);
    }
  let picked = rows;
  if (view.nodes) {
    const want = new Set(view.nodes);
    picked = rows.filter((r) => want.has(r[0]));
  }
  const numeric = vars.findIndex((v) => ['score', 'triangles', 'core', 'depth'].includes(v));
  if (view.top && numeric >= 0)
    picked = [...picked].sort((a, b) => b[numeric] - a[numeric]).slice(0, view.top);
  const term = (v, i) =>
    i === 0
      ? { type: 'uri', value: v }
      : {
          type: 'literal',
          value: String(v),
          datatype: Number.isInteger(v)
            ? 'http://www.w3.org/2001/XMLSchema#integer'
            : 'http://www.w3.org/2001/XMLSchema#double',
        };
  const bindings = picked.map((r) => {
    const b = Object.fromEntries(vars.map((v, i) => [v, term(r[i], i)]));
    if (groupCol >= 0)
      b.communitySize = {
        type: 'literal',
        value: String(sizes.get(`${levelCol >= 0 ? r[levelCol] : ''}|${r[groupCol]}`)),
        datatype: 'http://www.w3.org/2001/XMLSchema#integer',
      };
    return b;
  });
  return {
    head: { vars: groupCol >= 0 ? [...vars, 'communitySize'] : vars },
    results: { bindings },
    summary,
    run: { commit, cached, rows: rows.length, returned: bindings.length, truncated: false },
  };
}

export async function handleAlgo(req, res, url, seg, ctx) {
  const { datasets, send, fail, readBody, tasks, nextTaskId, headSeq, queryOptions } = ctx;
  if (seg[0] !== '$') return false;
  if (seg[1] === 'tasks' && seg[2] && req.method === 'DELETE' && owned.has(seg[2])) {
    const t = tasks.find((x) => x.id === seg[2]);
    if (!t || t.state !== 'running')
      return (fail(res, 409, `task ${seg[2]} has finished`, { code: 'not-cancellable' }), true);
    t.cancelled = true;
    return (send(res, 202, t), true);
  }
  if (seg[1] !== 'algo' || !seg[2]) return false;
  const ds = datasets.get(seg[2]);
  if (!ds) return (fail(res, 404, `no such dataset: /${seg[2]}`), true);
  if (seg[3] === 'projections' && req.method === 'GET' && !seg[4])
    return (send(res, 200, { dataset: ds.name, projections: ds.algoProjections ?? [] }), true);
  if (seg[3] !== 'run' || req.method !== 'POST') return false;
  let body;
  try {
    body = JSON.parse((await readBody(req)).toString('utf8') || '{}');
  } catch {
    return (fail(res, 400, 'invalid JSON'), true);
  }
  const view = {};
  for (const k of VIEW) if (k in body) ((view[k] = body[k]), delete body[k]);
  if (!OUTPUTS[body.algorithm])
    return (fail(res, 400, `unknown algorithm '${body.algorithm}'`), true);
  if (view.async && !view.keep) return (fail(res, 400, 'async needs keep'), true);
  if (body.write) return (fail(res, 501, 'the mock does not write algorithm results'), true);
  const projection =
    typeof body.projection === 'string'
      ? (ds.algoProjections ?? []).find((p) => p.name === body.projection)?.definition
      : body.projection;
  if (typeof body.projection === 'string' && !projection)
    return (fail(res, 400, `no projection '${body.projection}' in /${ds.name}`), true);
  const commit = headSeq(ds);
  const key = `${ds.name}|${ds.branchId ?? ''}|${commit}|${JSON.stringify(body)}`;
  const run = () => {
    const edges = projectionEdges(ds.store, projection, queryOptions(ds));
    const nodes = [...new Set(edges.flat())];
    return compute(body.algorithm, nodes, edges, { ...(body.params ?? {}) });
  };
  const hit = kept.get(key);
  if (hit) return (send(res, 200, answer(hit, view, true, commit)), true);
  if (view.async) {
    const id = running.get(key);
    const t = id && tasks.find((x) => x.id === id);
    if (t && t.state === 'running') return (send(res, 202, { ...t, joined: true }), true);
    const task = {
      id: nextTaskId(),
      kind: 'algo-view',
      dataset: ds.name,
      state: 'running',
      startedAt: new Date().toISOString(),
      progress: 0,
      message: 'computing',
      cancellable: true,
    };
    tasks.unshift(task);
    owned.add(task.id);
    running.set(key, task.id);
    // a run that takes a moment, so that its progress and Cancel can be seen
    const steps = 6;
    let i = 0;
    const timer = setInterval(
      () => {
        i++;
        if (task.cancelled) {
          clearInterval(timer);
          running.delete(key);
          Object.assign(task, { state: 'cancelled', finishedAt: new Date().toISOString() });
          delete task.progress;
          return;
        }
        task.progress = i / steps;
        if (i < steps) return;
        clearInterval(timer);
        running.delete(key);
        try {
          const r = run();
          kept.set(key, r);
          Object.assign(task, {
            state: 'done',
            message: `${body.algorithm}: kept a view-only run at commit ${commit}`,
            detail: {
              algorithm: body.algorithm,
              commit,
              rows: r.rows.length,
              kept: true,
              summary: r.summary,
            },
          });
        } catch (e) {
          Object.assign(task, { state: 'failed', message: String(e?.message ?? e) });
        }
        task.finishedAt = new Date().toISOString();
        delete task.progress;
      },
      Number(url.searchParams.get('mockRunMs') ?? 1800) / steps,
    );
    return (send(res, 202, { ...task, joined: false }), true);
  }
  let r;
  try {
    r = run();
  } catch (e) {
    return (fail(res, 400, String(e?.message ?? e)), true);
  }
  if (view.keep) kept.set(key, r);
  return (send(res, 200, answer(r, view, false, commit)), true);
}

/** The memberships of the canned GraphRAG communities (mock/graphrag.mjs). */
export function memberships(communities, nodes, level) {
  const want = new Set(nodes);
  const out = [];
  const used = new Map();
  for (const c of Object.values(communities)) {
    if (level != null && c.level !== level) continue;
    for (const m of c.members) {
      const iri = `http://example.org/resource/${m}`;
      if (!want.has(iri)) continue;
      out.push({ node: iri, community: c.iri, level: c.level });
      used.set(c.iri, c);
    }
  }
  return {
    available: true,
    reportsGraph: 'urn:x-sparkles:graphrag:reports',
    memberships: out,
    communities: [...used.values()].map((c) => ({
      iri: c.iri,
      level: c.level,
      memberCount: c.members.length,
      title: c.report?.title ?? null,
    })),
  };
}
