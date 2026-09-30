#!/usr/bin/env node
// Sparkles mock backend — implements enough of docs/API.md for UI development.
// SPARQL is evaluated for real by oxigraph (WASM, dev dependency); query plans,
// timings split, cache stats and disk sizes are fabricated.
//
//   node mock/server.mjs            # listens on :3030 (PORT env to override)
//   MOCK_LATENCY=300 node mock/...  # add artificial latency (ms) to every request

import http from 'node:http';
import { performance } from 'node:perf_hooks';
import ox from 'oxigraph';
import { PREFIXES, buildTurtle, provenanceTrig, scratchTurtle } from './data.mjs';

const PORT = Number(process.env.PORT ?? 3030);
const LATENCY = Number(process.env.MOCK_LATENCY ?? 0);
const VERSION = '0.1.0-mock';
const startedAt = new Date();
const INFERRED = ox.namedNode('urn:sparkles:inferred');

/** @type {Map<string, any>} */
const datasets = new Map();
/** @type {any[]} */
const tasks = [];
let taskSeq = 1;

function makeDataset(name, type) {
  const ds = {
    name,
    type,
    store: new ox.Store(),
    prefixes: { ...PREFIXES },
    reasoning: null,
    baseQuads: 0,
    deltaInserts: 0,
    deltaDeletes: 0,
    cache: { entries: 0, bytes: 0, hits: 0, misses: 0 },
  };
  datasets.set(name, ds);
  return ds;
}

{
  const foaf = makeDataset('foaf', 'persistent');
  foaf.store.load(buildTurtle(), { format: 'text/turtle' });
  foaf.store.load(provenanceTrig, { format: 'application/trig' });
  foaf.baseQuads = foaf.store.size;
  const scratch = makeDataset('scratch', 'mem');
  scratch.store.load(scratchTurtle, { format: 'text/turtle' });
  scratch.baseQuads = scratch.store.size;
}

// ---------------------------------------------------------------------------
// helpers

function info(ds) {
  const base = `/${ds.name}`;
  return {
    name: ds.name,
    type: ds.type,
    endpoints: {
      query: `${base}/sparql`,
      update: `${base}/update`,
      gsp: `${base}/data`,
      upload: `${base}/upload`,
    },
    quads: ds.store.size,
    reasoning: ds.reasoning,
  };
}

function send(res, status, body, type = 'application/json') {
  const data = typeof body === 'string' || Buffer.isBuffer(body) ? body : JSON.stringify(body);
  res.writeHead(status, {
    'Content-Type': type,
    'Access-Control-Allow-Origin': '*',
    'Access-Control-Expose-Headers': 'X-Request-Id',
    'X-Request-Id': res.requestId ?? '',
  });
  res.end(data);
}

// ---------------------------------------------------------------------------
// observability: request ids, readiness and a metrics registry fed by the mock's own
// traffic (health checks, metrics and the UI are not counted, as on the server)

const BOOT = Math.floor(Math.random() * 0xffffffff)
  .toString(16)
  .padStart(8, '0');
let requestSeq = 0;
const nextRequestId = () => `${BOOT}-${(++requestSeq).toString(16).padStart(12, '0')}`;

const BUCKETS = [
  0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30, 60, 300,
];
const OUTCOMES = ['ok', 'client_error', 'error', 'timeout', 'cancelled', 'budget'];
const LIMITS = {
  timeoutSeconds: 60,
  queryMemoryBytes: 8 * 2 ** 30,
  maxResultBytes: 2 ** 30,
  maxRows: 200_000_000,
};
/** @type {Map<string, any>} */
const series = new Map();
const active = {
  query: 0,
  update: 0,
  gsp: 0,
  upload: 0,
  shacl: 0,
  explain: 0,
  admin: 0,
  other: 0,
};

function classify(seg, method, url) {
  if (seg[0] === '$') {
    if (['ping', 'ready', 'metrics'].includes(seg[1])) return null;
    return { op: 'admin', ds: datasets.has(seg[2]) ? seg[2] : '$none' };
  }
  if (!seg.length || seg[0] === 'ui') return null;
  const ds = datasets.has(seg[0]) ? seg[0] : '$none';
  const op = seg[1] ?? '';
  if (['sparql', 'query'].includes(op)) return { op: 'query', ds };
  if (['update', 'upload', 'shacl', 'explain'].includes(op)) return { op, ds };
  if (['data', 'get'].includes(op)) return { op: 'gsp', ds };
  if (op === '')
    return {
      op: url.searchParams.has('update') ? 'update' : method === 'GET' ? 'query' : 'gsp',
      ds,
    };
  return { op: 'other', ds };
}

function record(c, status, seconds) {
  const key = `${c.ds}\u0000${c.op}`;
  let s = series.get(key);
  if (!s) {
    s = {
      dataset: c.ds,
      operation: c.op,
      outcomes: Object.fromEntries(OUTCOMES.map((o) => [o, 0])),
      buckets: new Array(BUCKETS.length + 1).fill(0),
      sumSeconds: 0,
      responseBytes: 0,
    };
    series.set(key, s);
  }
  const outcome =
    status === 408
      ? 'timeout'
      : status === 507
        ? 'budget'
        : status >= 500
          ? 'error'
          : status >= 400
            ? 'client_error'
            : 'ok';
  s.outcomes[outcome]++;
  const i = BUCKETS.findIndex((b) => seconds <= b);
  s.buckets[i < 0 ? BUCKETS.length : i]++;
  s.sumSeconds += seconds;
}

function metricsSnapshot() {
  const requests = [...series.values()].map((s) => {
    let acc = 0;
    const buckets = s.buckets.map((n) => (acc += n));
    return { ...s, buckets, count: acc };
  });
  const perDataset = new Map();
  for (const r of requests) {
    if (r.operation === 'query')
      perDataset.set(r.dataset, (perDataset.get(r.dataset) ?? 0) + r.count * 42);
  }
  return {
    formatVersion: 1,
    version: VERSION,
    uptimeSeconds: (Date.now() - startedAt.getTime()) / 1000,
    ready: true,
    processResidentBytes: process.memoryUsage().rss,
    limits: LIMITS,
    bucketBounds: BUCKETS,
    active,
    requests,
    datasets: [...datasets.values()].map((ds) => {
      const st = stats(ds);
      const lookups = ds.cache.hits + ds.cache.misses;
      return {
        name: ds.name,
        quads: ds.store.size,
        deltaInserts: ds.deltaInserts,
        deltaDeletes: ds.deltaDeletes,
        walBytes: ds.type === 'mem' ? 0 : (ds.deltaInserts + ds.deltaDeletes) * 33,
        diskBytes: st.diskBytes,
        resultRows: perDataset.get(ds.name) ?? 0,
        budgetExceeded: { rows: 0, memory: 0, 'result-bytes': 0 },
        blockCache: {
          bytes: Math.min(2 ** 30, st.quads * 24),
          capacityBytes: 2 ** 30,
          entries: Math.ceil(st.quads / 32768) * 4,
          hits: lookups * 9,
          misses: lookups + 12,
        },
        resultCache: {
          enabled: true,
          bytes: ds.cache.bytes,
          capacityBytes: 512 * 2 ** 20,
          entries: ds.cache.entries,
          hits: ds.cache.hits,
          misses: ds.cache.misses,
        },
      };
    }),
  };
}

function readyInfo() {
  return {
    status: 'ready',
    ready: true,
    uptimeSeconds: Math.round((Date.now() - startedAt.getTime()) / 1000),
    datasets: [...datasets.values()].map((ds) => ({
      name: ds.name,
      type: ds.type,
      state: 'open',
      ready: true,
      ...(ds.type === 'persistent' ? { generation: 'gen-0001' } : {}),
      walBytes: ds.type === 'mem' ? 0 : (ds.deltaInserts + ds.deltaDeletes) * 33,
      deltaQuads: ds.deltaInserts + ds.deltaDeletes,
    })),
  };
}

function fail(res, status, error, extra = {}) {
  send(res, status, { error, ...extra });
}

async function readBody(req) {
  const chunks = [];
  for await (const c of req) chunks.push(c);
  return Buffer.concat(chunks);
}

/** Collect protocol params from URL, form bodies and raw sparql bodies. */
async function params(req, url) {
  const p = new Map(url.searchParams);
  if (req.method === 'POST' || req.method === 'PUT') {
    const ct = (req.headers['content-type'] ?? '').split(';')[0].trim();
    const body = await readBody(req);
    if (ct === 'application/x-www-form-urlencoded') {
      for (const [k, v] of new URLSearchParams(body.toString('utf8'))) p.set(k, v);
    } else if (ct === 'application/sparql-query') {
      p.set('query', body.toString('utf8'));
    } else if (ct === 'application/sparql-update') {
      p.set('update', body.toString('utf8'));
    } else if (ct === 'application/json' && body.length) {
      try {
        for (const [k, v] of Object.entries(JSON.parse(body.toString('utf8')))) p.set(k, v);
      } catch {
        /* ignore */
      }
    }
    p.set('__body', body);
    p.set('__ct', req.headers['content-type'] ?? '');
  }
  return p;
}

function termJson(t) {
  switch (t.termType) {
    case 'NamedNode':
      return { type: 'uri', value: t.value };
    case 'BlankNode':
      return { type: 'bnode', value: t.value };
    case 'Literal': {
      const o = { type: 'literal', value: t.value };
      if (t.language) o['xml:lang'] = t.language;
      else if (t.datatype && t.datatype.value !== 'http://www.w3.org/2001/XMLSchema#string')
        o.datatype = t.datatype.value;
      return o;
    }
    case 'Quad':
      return {
        type: 'triple',
        value: {
          subject: termJson(t.subject),
          predicate: termJson(t.predicate),
          object: termJson(t.object),
        },
      };
    default:
      return { type: 'literal', value: String(t.value) };
  }
}

function parseErr(e) {
  const msg = String(e?.message ?? e);
  const m = /(?:at|line)\s+(\d+):(\d+)/.exec(msg);
  return m
    ? { error: 'Parse error', detail: msg, line: Number(m[1]), column: Number(m[2]) }
    : { error: 'Parse error', detail: msg };
}

function queryOptions(ds, p) {
  const reasoning = p.get('reasoning') !== 'false';
  if (!reasoning && ds.reasoning) {
    const graphs = ds.store
      .query('SELECT DISTINCT ?g WHERE { GRAPH ?g {} }')
      .map((b) => b.get('g'))
      .filter((g) => !g.equals(INFERRED));
    return { default_graph: [ox.defaultGraph(), ...graphs] };
  }
  return { use_default_graph_as_union: true };
}

// ---------------------------------------------------------------------------
// fake planner: turns the WHERE clause into a plausible operator tree

function stripNoise(q) {
  return q
    .replace(
      /("""[\s\S]*?"""|"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*')|(<[^<>"{}|^`\\\s]*>)|(#[^\n]*)/g,
      (m, str, iri) => (str ? '"…"' : iri ? iri : ''),
    )
    .replace(/\b(PREFIX\s+[\w-]*:\s*<[^>]*>|BASE\s+<[^>]*>)/gi, '')
    .trim();
}

function extractPatterns(q) {
  const body = stripNoise(q);
  const start = body.indexOf('{');
  if (start < 0) return [];
  const inner = body.slice(start + 1, body.lastIndexOf('}'));
  const cleaned = inner
    .replace(/\b(FILTER|BIND|VALUES)\s*\([^)]*\)/gi, ' . ')
    .replace(/\b(OPTIONAL|MINUS|UNION|GRAPH\s+\S+|SERVICE\s+\S+)\b/gi, ' . ')
    .replace(/[{}]/g, ' . ');
  const out = [];
  for (const stmt of cleaned.split(/\s\.(?=\s|$)/)) {
    const parts = stmt.trim().split(/\s*;\s*/);
    const subj = parts[0]?.trim().split(/\s+/)[0];
    if (!subj) continue;
    for (let i = 0; i < parts.length; i++) {
      const toks = parts[i].trim().split(/\s+/);
      const [p, ...os] = i === 0 ? toks.slice(1) : toks;
      if (!p) continue;
      for (const o of os.join(' ').split(/\s*,\s*/)) if (o) out.push([subj, p, o.trim()]);
    }
  }
  return out.slice(0, 12);
}

const isVar = (t) => /^[?$]/.test(t);

function plan(query, totalRows, execMs, executed) {
  const pats = extractPatterns(query);
  const scale = executed ? 1 : -1;
  const n = (x) => (executed ? Math.max(0, Math.round(x)) : -1);
  let budget = Math.max(execMs, 0.05);
  const scans = pats.map(([s, p, o], i) => {
    const perm = !isVar(s)
      ? 'SPO'
      : !isVar(p) && !isVar(o)
        ? 'POS'
        : !isVar(p)
          ? 'PSO'
          : !isVar(o)
            ? 'OSP'
            : 'SPO';
    const est = !isVar(s) ? 8 : !isVar(o) ? 40 : !isVar(p) ? 400 : 5000;
    const rows = Math.max(totalRows, 1) * (1 + ((i * 37) % 5));
    return {
      operator: 'IndexScan',
      description: `${perm} ${s} ${p} ${o}`,
      columns: [s, p, o].filter(isVar),
      sortedOn: [
        perm === 'SPO' ? s : perm === 'POS' || perm === 'PSO' ? (perm === 'POS' ? o : s) : o,
      ].filter(isVar),
      estimatedRows: est,
      estimatedCost: est,
      actualRows: n(Math.min(rows, est * 3)),
      timeMs: executed ? +(budget * 0.12 * (1 + (i % 3))).toFixed(3) : 0,
      cached: i === 1 && executed,
      children: [],
    };
  });
  let node =
    scans.length === 0
      ? {
          operator: 'Values',
          description: 'single empty row',
          columns: [],
          sortedOn: [],
          estimatedRows: 1,
          estimatedCost: 1,
          actualRows: n(1),
          timeMs: 0,
          cached: false,
          children: [],
        }
      : scans[0];
  for (let i = 1; i < scans.length; i++) {
    const right = scans[i];
    const shared = node.columns.filter((c) => right.columns.includes(c));
    const merge =
      shared.length && node.sortedOn[0] === shared[0] && right.sortedOn[0] === shared[0];
    node = {
      operator: shared.length ? (merge ? 'MergeJoin' : 'HashJoin') : 'CartesianProduct',
      description: shared.length ? `on ${shared.join(', ')}` : 'no shared variables',
      columns: [...new Set([...node.columns, ...right.columns])],
      sortedOn: merge ? [shared[0]] : [],
      estimatedRows: Math.max(1, Math.round(Math.sqrt(node.estimatedRows * right.estimatedRows))),
      estimatedCost: node.estimatedCost + right.estimatedCost,
      actualRows: n(totalRows * (1 + (scans.length - i) * 0.6)),
      timeMs: executed ? +(node.timeMs + right.timeMs + budget * 0.08).toFixed(3) : 0,
      cached: false,
      children: [node, right],
    };
  }
  const wrap = (operator, description, factor = 1) => {
    node = {
      operator,
      description,
      columns: node.columns,
      sortedOn: node.sortedOn,
      estimatedRows: node.estimatedRows,
      estimatedCost: node.estimatedCost + node.estimatedRows,
      actualRows: n(totalRows * factor),
      timeMs: executed ? +(node.timeMs + budget * 0.05).toFixed(3) : 0,
      cached: false,
      children: [node],
    };
  };
  const q = stripNoise(query);
  const filter = /FILTER\s*\(([^)]*)\)/i.exec(q);
  if (filter) wrap('Filter', filter[1].trim().slice(0, 80), 1);
  if (/\bGROUP\s+BY\b/i.test(q)) wrap('GroupBy', /GROUP\s+BY\s+([^\n}]*)/i.exec(q)[1].trim(), 1);
  const order = /ORDER\s+BY\s+([^\n}]*?)(?:LIMIT|OFFSET|$)/i.exec(q);
  if (order) wrap('Sort', order[1].trim());
  if (/SELECT\s+DISTINCT/i.test(q)) wrap('Distinct', node.columns.join(' '));
  const limit = /\bLIMIT\s+(\d+)/i.exec(q);
  if (limit) wrap('Limit', `LIMIT ${limit[1]}`);
  if (/^\s*SELECT/im.test(q)) wrap('Project', node.columns.join(' '));
  if (/^\s*(CONSTRUCT|DESCRIBE)/im.test(q)) wrap('Construct', 'template instantiation');
  void scale;
  node.timeMs = executed ? +Math.max(node.timeMs, execMs).toFixed(3) : 0;
  node.actualRows = n(totalRows);
  return node;
}

function sse(query) {
  const pats = extractPatterns(query);
  const triples = pats.map(([s, p, o]) => `    (triple ${s} ${p} ${o})`).join('\n');
  let out = `(bgp\n${triples})`;
  const q = stripNoise(query);
  const filter = /FILTER\s*\(([^)]*)\)/i.exec(q);
  if (filter) out = `(filter (${filter[1].trim()})\n  ${out.replace(/\n/g, '\n  ')})`;
  const vars = /SELECT\s+(?:DISTINCT\s+)?(.*?)\s*(?:WHERE|FROM|\{)/is
    .exec(q)?.[1]
    ?.match(/[?$]\w+/g);
  if (vars) out = `(project (${vars.join(' ')})\n  ${out.replace(/\n/g, '\n  ')})`;
  const limit = /\bLIMIT\s+(\d+)/i.exec(q);
  if (limit) out = `(slice _ ${limit[1]}\n  ${out.replace(/\n/g, '\n  ')})`;
  return out;
}

// ---------------------------------------------------------------------------
// SPARQL query endpoint

const SELECT_FORMATS = {
  json: 'application/sparql-results+json',
  xml: 'application/sparql-results+xml',
  csv: 'text/csv',
  tsv: 'text/tab-separated-values',
};
const GRAPH_FORMATS = {
  ttl: 'text/turtle',
  turtle: 'text/turtle',
  nt: 'application/n-triples',
  nq: 'application/n-quads',
  trig: 'application/trig',
  jsonld: 'application/ld+json',
  rdfxml: 'application/rdf+xml',
  xml: 'application/rdf+xml',
};

function pickFormat(accept, candidates) {
  for (const part of (accept ?? '').split(',')) {
    const mt = part.split(';')[0].trim();
    if (candidates.includes(mt)) return mt;
  }
  return null;
}

async function handleQuery(req, res, ds, p) {
  const query = p.get('query');
  if (!query) return fail(res, 400, 'Missing query parameter');
  const accept = String(req.headers.accept ?? '');
  const t0 = performance.now();
  let result;
  try {
    result = ds.store.query(query, queryOptions(ds, p));
  } catch (e) {
    return send(res, 400, parseErr(e));
  }
  const execMs = performance.now() - t0;
  ds.cache.misses++;
  ds.cache.entries = Math.min(ds.cache.entries + 1, 256);
  ds.cache.bytes += 1024 + query.length * 3;

  const fmtParam = p.get('format');
  const sparkles = accept.includes('application/x-sparkles+json') || fmtParam === 'sparkles';
  if (!sparkles) {
    // Standard content negotiation.
    const isGraph =
      Array.isArray(result) &&
      (result.length === 0
        ? /^\s*(CONSTRUCT|DESCRIBE)/im.test(stripNoise(query))
        : !(result[0] instanceof Map));
    if (isGraph) {
      const fmt =
        GRAPH_FORMATS[fmtParam] ??
        pickFormat(accept, Object.values(GRAPH_FORMATS)) ??
        'text/turtle';
      const out = ds.store.query(query, { ...queryOptions(ds, p), results_format: fmt });
      return send(res, 200, out, fmt);
    }
    const fmt =
      SELECT_FORMATS[fmtParam] ??
      pickFormat(accept, Object.values(SELECT_FORMATS)) ??
      'application/sparql-results+json';
    const out = ds.store.query(query, { ...queryOptions(ds, p), results_format: fmt });
    return send(res, 200, out, fmt);
  }

  const cap = p.has('send') ? Math.max(0, Number(p.get('send'))) : Infinity;
  const t1 = performance.now();
  /** @type {any} */
  let body;
  if (typeof result === 'boolean') {
    body = { queryType: 'ASK', boolean: result, total: 1 };
  } else if (result.length && !(result[0] instanceof Map)) {
    const triples = result.map((q) => [
      termJson(q.subject),
      termJson(q.predicate),
      termJson(q.object),
    ]);
    body = {
      queryType: /^\s*DESCRIBE/im.test(stripNoise(query)) ? 'DESCRIBE' : 'CONSTRUCT',
      triples: triples.slice(0, cap),
      total: triples.length,
    };
  } else if (/^\s*(CONSTRUCT|DESCRIBE)/im.test(stripNoise(query))) {
    body = {
      queryType: /^\s*DESCRIBE/im.test(stripNoise(query)) ? 'DESCRIBE' : 'CONSTRUCT',
      triples: [],
      total: 0,
    };
  } else {
    // Use the JSON serializer for projection order, then reshape into rows.
    const json = JSON.parse(
      ds.store.query(query, {
        ...queryOptions(ds, p),
        results_format: 'application/sparql-results+json',
      }),
    );
    const vars = json.head.vars;
    const rows = json.results.bindings.map((b) => vars.map((v) => b[v] ?? null));
    body = { queryType: 'SELECT', vars, rows: rows.slice(0, cap), total: rows.length };
  }
  const serializeMs = performance.now() - t1;
  const { total, ...rest } = body;
  const sent = rest.rows?.length ?? rest.triples?.length ?? 1;
  const parseMs = 0.05 + query.length / 20000;
  const planMs = 0.1 + extractPatterns(query).length * 0.04;
  send(
    res,
    200,
    {
      ...rest,
      meta: {
        totalRows: total,
        sentRows: sent,
        timing: {
          parseMs: +parseMs.toFixed(3),
          planMs: +planMs.toFixed(3),
          execMs: +execMs.toFixed(3),
          serializeMs: +serializeMs.toFixed(3),
          totalMs: +(parseMs + planMs + execMs + serializeMs).toFixed(3),
        },
        plan: plan(query, total, execMs, true),
      },
    },
    'application/x-sparkles+json',
  );
}

function handleUpdate(res, ds, p) {
  const update = p.get('update');
  if (!update) return fail(res, 400, 'Missing update parameter');
  const before = ds.store.size;
  try {
    ds.store.update(update);
  } catch (e) {
    return send(res, 400, parseErr(e));
  }
  const diff = ds.store.size - before;
  if (diff > 0) ds.deltaInserts += diff;
  else ds.deltaDeletes -= diff;
  ds.cache = { entries: 0, bytes: 0, hits: ds.cache.hits, misses: ds.cache.misses };
  send(res, 200, `<html><body><p>Update succeeded</p></body></html>`, 'text/html');
}

// ---------------------------------------------------------------------------
// multipart upload

function parseMultipart(body, contentType) {
  const m = /boundary=(?:"([^"]+)"|([^;]+))/i.exec(contentType);
  if (!m) return [];
  const boundary = Buffer.from('--' + (m[1] ?? m[2]));
  const parts = [];
  let idx = body.indexOf(boundary);
  while (idx >= 0) {
    const next = body.indexOf(boundary, idx + boundary.length);
    if (next < 0) break;
    const chunk = body.subarray(idx + boundary.length + 2, next - 2);
    const sep = chunk.indexOf('\r\n\r\n');
    if (sep >= 0) {
      const headers = chunk.subarray(0, sep).toString('utf8');
      const name = /name="([^"]*)"/i.exec(headers)?.[1];
      const filename = /filename="([^"]*)"/i.exec(headers)?.[1];
      const type = /content-type:\s*([^\r\n]+)/i.exec(headers)?.[1];
      parts.push({ name, filename, type, data: chunk.subarray(sep + 4) });
    }
    idx = next;
  }
  return parts;
}

const EXT_FORMATS = {
  ttl: 'text/turtle',
  nt: 'application/n-triples',
  nq: 'application/n-quads',
  trig: 'application/trig',
  rdf: 'application/rdf+xml',
  owl: 'application/rdf+xml',
  xml: 'application/rdf+xml',
  n3: 'text/n3',
};

async function handleUpload(req, res, ds) {
  const body = await readBody(req);
  const parts = parseMultipart(body, req.headers['content-type'] ?? '');
  const graph = parts
    .find((x) => x.name === 'graph' && !x.filename)
    ?.data.toString('utf8')
    .trim();
  let count = 0;
  for (const part of parts.filter((x) => x.filename)) {
    const ext = part.filename.split('.').pop()?.toLowerCase() ?? '';
    const format = EXT_FORMATS[ext] ?? part.type;
    if (!format) return fail(res, 400, `Unknown RDF format for ${part.filename}`);
    const before = ds.store.size;
    const text = part.data.toString('utf8');
    try {
      ds.store.load(text, { format, ...(graph ? { to_graph_name: ox.namedNode(graph) } : {}) });
    } catch (e) {
      return send(res, 400, { ...parseErr(e), error: `Failed to parse ${part.filename}` });
    }
    for (const [, pfx, iri] of text.matchAll(/@prefix\s+([\w-]*):\s*<([^>]+)>/gi))
      ds.prefixes[pfx] = iri;
    count += ds.store.size - before;
    ds.deltaInserts += ds.store.size - before;
  }
  send(res, 200, { count, tripleCount: count, quadCount: count });
}

// ---------------------------------------------------------------------------
// tasks

function startTask(kind, ds, work, durationMs = 2500) {
  const task = {
    id: String(taskSeq++),
    kind,
    dataset: ds.name,
    state: 'running',
    startedAt: new Date().toISOString(),
    progress: 0,
  };
  tasks.unshift(task);
  const steps = 10;
  let i = 0;
  const timer = setInterval(() => {
    i++;
    task.progress = i / steps;
    if (i >= steps) {
      clearInterval(timer);
      try {
        task.message = work() ?? 'ok';
        task.state = 'done';
      } catch (e) {
        task.state = 'failed';
        task.message = String(e?.message ?? e);
      }
      task.finishedAt = new Date().toISOString();
      delete task.progress;
    }
  }, durationMs / steps);
  return task;
}

const RDFS_RULES = [
  // rdfs9 — subclass membership (transitive via property path)
  `INSERT { GRAPH <urn:sparkles:inferred> { ?x a ?super } } WHERE { ?x a ?c . ?c <http://www.w3.org/2000/01/rdf-schema#subClassOf>+ ?super . FILTER NOT EXISTS { ?x a ?super } }`,
  // rdfs2 / rdfs3 — domain and range
  `INSERT { GRAPH <urn:sparkles:inferred> { ?x a ?c } } WHERE { ?p <http://www.w3.org/2000/01/rdf-schema#domain> ?c . ?x ?p ?y . FILTER NOT EXISTS { ?x a ?c } }`,
  `INSERT { GRAPH <urn:sparkles:inferred> { ?y a ?c } } WHERE { ?p <http://www.w3.org/2000/01/rdf-schema#range> ?c . ?x ?p ?y . FILTER(isIRI(?y)) FILTER NOT EXISTS { ?y a ?c } }`,
];
const OWL_RULES = [
  `INSERT { GRAPH <urn:sparkles:inferred> { ?y ?p ?x } } WHERE { ?p a <http://www.w3.org/2002/07/owl#SymmetricProperty> . ?x ?p ?y . FILTER NOT EXISTS { ?y ?p ?x } }`,
  `INSERT { GRAPH <urn:sparkles:inferred> { ?y ?q ?x } } WHERE { ?p <http://www.w3.org/2002/07/owl#inverseOf> ?q . ?x ?p ?y . FILTER NOT EXISTS { ?y ?q ?x } }`,
];

function inferredCount(ds) {
  return ds.store.match(null, null, null, INFERRED).length;
}

// ---------------------------------------------------------------------------
// stats

function stats(ds) {
  const q = (s) => ds.store.query(s, { use_default_graph_as_union: true });
  const num = (t) => Number(t?.value ?? 0);
  const graphs = [
    { name: null, quads: ds.store.match(null, null, null, ox.defaultGraph()).length },
  ];
  for (const b of ds.store.query(
    'SELECT ?g (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g ORDER BY DESC(?n)',
  )) {
    graphs.push({ name: b.get('g').value, quads: num(b.get('n')) });
  }
  const predicates = q(
    'SELECT ?p (COUNT(*) AS ?n) (COUNT(DISTINCT ?s) AS ?ds) (COUNT(DISTINCT ?o) AS ?do) WHERE { ?s ?p ?o } GROUP BY ?p ORDER BY DESC(?n) LIMIT 100',
  ).map((b) => ({
    iri: b.get('p').value,
    count: num(b.get('n')),
    distinctSubjects: num(b.get('ds')),
    distinctObjects: num(b.get('do')),
  }));
  const classes = q(
    'SELECT ?c (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s a ?c FILTER(isIRI(?c)) } GROUP BY ?c ORDER BY DESC(?n) LIMIT 100',
  ).map((b) => ({ iri: b.get('c').value, instances: num(b.get('n')) }));
  const terms = num(
    q(
      'SELECT (COUNT(DISTINCT ?t) AS ?n) WHERE { { ?t ?p ?o } UNION { ?s ?t ?o } UNION { ?s ?p ?t } }',
    )[0]?.get('n'),
  );
  return {
    name: ds.name,
    quads: ds.store.size,
    baseQuads: ds.baseQuads,
    deltaInserts: ds.deltaInserts,
    deltaDeletes: ds.deltaDeletes,
    terms,
    graphs,
    predicates,
    classes,
    diskBytes: ds.type === 'mem' ? 0 : ds.store.size * 38 + terms * 21 + 65536,
    cache: ds.cache,
    resultCache: { enabled: true, ...ds.cache },
  };
}

// ---------------------------------------------------------------------------
// schema discovery (/$/schema/{ds}[/classes|/predicates]), computed in JS over the quads

const NS = {
  rdf: 'http://www.w3.org/1999/02/22-rdf-syntax-ns#',
  rdfs: 'http://www.w3.org/2000/01/rdf-schema#',
  owl: 'http://www.w3.org/2002/07/owl#',
  xsd: 'http://www.w3.org/2001/XMLSchema#',
  sh: 'http://www.w3.org/ns/shacl#',
};
const CLASS_TYPES = ['rdfs:Class', 'owl:Class', 'rdfs:Datatype'].map(expand);
const PROPERTY_TYPES = [
  'rdf:Property',
  'owl:ObjectProperty',
  'owl:DatatypeProperty',
  'owl:AnnotationProperty',
  'owl:FunctionalProperty',
  'owl:InverseFunctionalProperty',
  'owl:TransitiveProperty',
  'owl:SymmetricProperty',
].map(expand);

function expand(pn) {
  const [p, l] = pn.split(':');
  return NS[p] + l;
}

const builtin = (iri) => Object.values(NS).some((ns) => iri.startsWith(ns));

/** The schema report of a dataset (all items; the router paginates). */
function schemaReport(ds, p) {
  const graph = p.get('graph') || 'default';
  const reasoning = p.has('reasoning') ? p.get('reasoning') === 'true' : !!ds.reasoning;
  const declaredAll = p.get('declared') === 'all';
  const accepts = (q, withInferred) => {
    const g = q.graph;
    const inferred = g.termType === 'NamedNode' && g.value === INFERRED.value;
    if (graph === 'default') return g.termType === 'DefaultGraph' || (inferred && withInferred);
    if (graph === 'union') return !inferred || withInferred;
    return g.termType === 'NamedNode' && g.value === graph;
  };
  const all = ds.store.match(null, null, null, null);
  if (!['default', 'union'].includes(graph) && !all.some((q) => accepts(q, true)))
    return { status: 404, error: `no such graph: <${graph}>` };
  /** Distinct triples of the selection. */
  const triples = (withInferred) => {
    const seen = new Map();
    for (const q of all) {
      if (!accepts(q, withInferred)) continue;
      const k = `${q.subject} ${q.predicate} ${q.object}`;
      if (!seen.has(k)) seen.set(k, q);
    }
    return [...seen.values()];
  };
  const obs = triples(reasoning);
  const decl = triples(declaredAll);
  const lit = (t) => ({ value: t.value, ...(t.language ? { lang: t.language } : {}) });

  // observed
  const preds = new Map();
  const instances = new Map();
  const anonTypes = new Set();
  for (const q of obs) {
    const p = q.predicate.value;
    let e = preds.get(p);
    if (!e) preds.set(p, (e = { subjects: new Map(), objects: new Map(), triples: 0 }));
    e.triples++;
    const s = String(q.subject);
    e.subjects.set(s, (e.subjects.get(s) ?? 0) + 1);
    const o = String(q.object);
    const ob = e.objects.get(o) ?? { term: q.object, n: 0 };
    ob.n++;
    e.objects.set(o, ob);
    if (p === RDF_TYPE_IRI) {
      if (q.object.termType === 'NamedNode') {
        const set = instances.get(q.object.value) ?? new Set();
        set.add(s);
        instances.set(q.object.value, set);
      } else if (q.object.termType === 'BlankNode') anonTypes.add(o);
    }
  }

  // declared
  const classes = new Map();
  const props = new Map();
  const cls = (iri) => {
    let c = classes.get(iri);
    if (!c)
      classes.set(
        iri,
        (c = { types: [], superClasses: [], equivalentClasses: [], disjointWith: [] }),
      );
    return c;
  };
  const prop = (iri) => {
    let e = props.get(iri);
    if (!e)
      props.set(
        iri,
        (e = { types: [], domains: [], ranges: [], superProperties: [], inverseOf: [] }),
      );
    return e;
  };
  const add = (arr, v) => arr.includes(v) || arr.push(v);
  const bySubject = new Map();
  const anonExprs = new Set();
  for (const q of decl) {
    const s = q.subject.value;
    const p = q.predicate.value;
    const o = q.object;
    if (q.subject.termType === 'NamedNode') {
      const list = bySubject.get(s) ?? [];
      list.push(q);
      bySubject.set(s, list);
    }
    if (q.subject.termType !== 'NamedNode') continue;
    const oIri = o.termType === 'NamedNode';
    if (
      o.termType === 'BlankNode' &&
      /#(subClassOf|equivalentClass|disjointWith|domain|range)$/.test(p)
    )
      anonExprs.add(String(o));
    if (p === RDF_TYPE_IRI && oIri && CLASS_TYPES.includes(o.value)) add(cls(s).types, o.value);
    if (p === RDF_TYPE_IRI && oIri && PROPERTY_TYPES.includes(o.value)) add(prop(s).types, o.value);
    const classRel = {
      [NS.rdfs + 'subClassOf']: 'superClasses',
      [NS.owl + 'equivalentClass']: 'equivalentClasses',
      [NS.owl + 'disjointWith']: 'disjointWith',
    }[p];
    if (classRel) {
      const c = cls(s);
      if (oIri) {
        cls(o.value);
        if (!(classRel === 'superClasses' && o.value === s)) add(c[classRel], o.value);
      }
    }
    const propRel = {
      [NS.rdfs + 'domain']: 'domains',
      [NS.rdfs + 'range']: 'ranges',
      [NS.rdfs + 'subPropertyOf']: 'superProperties',
      [NS.owl + 'inverseOf']: 'inverseOf',
    }[p];
    if (propRel) {
      const e = prop(s);
      if (oIri) {
        if (propRel === 'superProperties' || propRel === 'inverseOf') prop(o.value);
        add(e[propRel], o.value);
      }
    }
  }
  for (const c of instances.keys()) cls(c);
  for (const p of preds.keys()) prop(p);
  const literals = (iri, p) =>
    (bySubject.get(iri) ?? [])
      .filter((q) => q.predicate.value === p && q.object.termType === 'Literal')
      .map((q) => lit(q.object));

  const classItems = [...classes.entries()]
    .map(([iri, d]) => ({
      iri,
      builtin: builtin(iri),
      observed: { instances: instances.get(iri)?.size ?? 0 },
      declared: {
        ...d,
        labels: literals(iri, NS.rdfs + 'label'),
        comments: literals(iri, NS.rdfs + 'comment'),
      },
    }))
    .sort((a, b) => (a.iri < b.iri ? -1 : 1));
  const predItems = [...props.entries()]
    .map(([iri, d]) => {
      const e = preds.get(iri);
      const perSubject = e ? [...e.subjects.values()] : [];
      const objects = { literals: [] };
      const groups = new Map();
      for (const { term, n } of e?.objects.values() ?? []) {
        const kind = { NamedNode: 'iri', BlankNode: 'blank', Quad: 'tripleTerm' }[term.termType];
        if (kind) {
          objects[kind] ??= { triples: 0, distinct: 0 };
          objects[kind].triples += n;
          objects[kind].distinct++;
          continue;
        }
        const dt = term.language ? NS.rdf + 'langString' : term.datatype.value;
        const g = groups.get(dt) ?? { datatype: dt, triples: 0, distinct: 0 };
        g.triples += n;
        g.distinct++;
        if (term.language) {
          g.languages ??= [];
          const l = g.languages.find((x) => x.lang === term.language);
          if (l) l.triples += n;
          else g.languages.push({ lang: term.language, triples: n });
        }
        groups.set(dt, g);
      }
      objects.literals = [...groups.values()].sort((a, b) => (a.datatype < b.datatype ? -1 : 1));
      return {
        iri,
        builtin: builtin(iri),
        observed: {
          triples: e?.triples ?? 0,
          distinctSubjects: perSubject.length,
          distinctObjects: e?.objects.size ?? 0,
          maxPerSubject: Math.max(0, ...perSubject),
          subjectsWithMultiple: perSubject.filter((n) => n >= 2).length,
          objects,
        },
        declared: {
          ...d,
          labels: literals(iri, NS.rdfs + 'label'),
          comments: literals(iri, NS.rdfs + 'comment'),
        },
      };
    })
    .sort((a, b) => (a.iri < b.iri ? -1 : 1));

  // hierarchy: components of mutually reachable classes (small data: plain DFS)
  const supers = (c) => classes.get(c)?.superClasses.filter((s) => classes.has(s)) ?? [];
  const reach = new Map();
  for (const c of classes.keys()) {
    const seen = new Set();
    const stack = [...supers(c)];
    while (stack.length) {
      const x = stack.pop();
      if (seen.has(x)) continue;
      seen.add(x);
      stack.push(...supers(x));
    }
    reach.set(c, seen);
  }
  const comp = (c) =>
    [...classes.keys()]
      .filter((x) => x === c || (reach.get(c).has(x) && reach.get(x).has(c)))
      .sort();
  const subCount = (c) =>
    classItems.filter((x) => x.declared.superClasses.includes(c) && x.iri !== c).length;
  const cycles = [];
  const roots = [];
  for (const c of classes.keys()) {
    const members = comp(c);
    if (members[0] !== c) continue;
    if (members.length > 1) cycles.push(members);
    if (members.every((m) => supers(m).every((s) => members.includes(s)))) roots.push(c);
  }
  roots.sort((a, b) => subCount(b) - subCount(a) || (a < b ? -1 : 1));
  cycles.sort((a, b) => (a[0] < b[0] ? -1 : 1));

  const ontologies = decl
    .filter(
      (q) =>
        q.predicate.value === RDF_TYPE_IRI &&
        q.object.value === NS.owl + 'Ontology' &&
        q.subject.termType === 'NamedNode',
    )
    .map((q) => q.subject.value);
  return {
    report: {
      schemaFormat: 1,
      dataset: ds.name,
      snapshot: {
        version: ds.baseQuads + ds.deltaInserts + ds.deltaDeletes,
        generation: 'mock',
        computedAt: new Date().toISOString(),
      },
      selection: {
        graph,
        declaredGraph: graph,
        reasoning,
        declared: declaredAll ? 'all' : 'asserted',
      },
      totals: {
        triples: obs.length,
        classes: classItems.length,
        predicates: predItems.length,
        anonymousTypeTargets: anonTypes.size,
        anonymousClassExpressions: anonExprs.size,
      },
      ontology: [...new Set(ontologies)].sort().map((iri) => ({
        iri,
        labels: literals(iri, NS.rdfs + 'label'),
        versionInfo: literals(iri, NS.owl + 'versionInfo'),
        comments: literals(iri, NS.rdfs + 'comment'),
      })),
      hierarchy: { roots, cycles },
      classes: classItems,
      predicates: predItems,
    },
  };
}
const RDF_TYPE_IRI = NS.rdf + 'type';

/** A page of an IRI-sorted list after the cursor (base64url JSON `{ a: lastIri }`). */
function schemaPage(items, limit, cursor) {
  let after = null;
  if (cursor) after = JSON.parse(Buffer.from(cursor, 'base64url').toString('utf8')).a;
  const start = after == null ? 0 : items.findIndex((x) => x.iri > after);
  const from = start < 0 ? items.length : start;
  const page = items.slice(from, from + limit);
  const more = from + page.length < items.length;
  return {
    items: page,
    total: items.length,
    next: more
      ? Buffer.from(JSON.stringify({ a: page[page.length - 1].iri })).toString('base64url')
      : null,
  };
}

// ---------------------------------------------------------------------------
// router

const server = http.createServer(async (req, res) => {
  if (LATENCY) await new Promise((r) => setTimeout(r, LATENCY));
  const url = new URL(req.url ?? '/', 'http://localhost');
  const path = decodeURIComponent(url.pathname);
  const seg = path.split('/').filter(Boolean);
  const incoming = String(req.headers['x-request-id'] ?? '');
  res.requestId = /^[A-Za-z0-9._:-]{1,128}$/.test(incoming) ? incoming : nextRequestId();
  const counted = req.method === 'OPTIONS' ? null : classify(seg, req.method, url);
  const t0 = performance.now();
  if (counted) active[counted.op]++;
  const log = () => {
    console.log(
      `${new Date().toISOString().slice(11, 19)} ${req.method} ${req.url?.slice(0, 120)} → ${res.statusCode}`,
    );
    if (counted) {
      active[counted.op]--;
      record(counted, res.statusCode, (performance.now() - t0) / 1000);
    }
  };
  res.on('finish', log);
  try {
    if (req.method === 'OPTIONS') {
      res.writeHead(204, {
        'Access-Control-Allow-Origin': '*',
        'Access-Control-Allow-Headers': '*',
        'Access-Control-Allow-Methods': '*',
      });
      return res.end();
    }
    if (seg[0] === '$') {
      const [, what, name, extra] = seg;
      const ds = name ? datasets.get(name) : undefined;
      switch (what) {
        case 'ping':
          return send(res, 200, new Date().toISOString(), 'text/plain');
        case 'ready': {
          const r = readyInfo();
          if (!name) return send(res, 200, r);
          const d = r.datasets.find((x) => x.name === name);
          return d
            ? send(res, 200, { ...r, datasets: [d] })
            : fail(res, 404, `No such dataset: ${name}`);
        }
        case 'metrics':
          if (url.searchParams.get('format') === 'json') return send(res, 200, metricsSnapshot());
          return send(res, 200, '# metrics: see ?format=json in the mock\n', 'text/plain');
        case 'server':
          return send(res, 200, {
            version: VERSION,
            startedAt: startedAt.toISOString(),
            uptimeSeconds: Math.round((Date.now() - startedAt.getTime()) / 1000),
            datasets: [...datasets.values()].map(info),
            limits: LIMITS,
          });
        case 'datasets': {
          if (!name) {
            if (req.method === 'GET')
              return send(res, 200, { datasets: [...datasets.values()].map(info) });
            if (req.method === 'POST') {
              const p = await params(req, url);
              const dbName = String(p.get('dbName') ?? '').replace(/^\//, '');
              const dbType = String(p.get('dbType') ?? 'mem');
              if (!/^[A-Za-z0-9_.-]+$/.test(dbName))
                return fail(res, 400, 'Invalid dataset name', {
                  detail: 'Use letters, digits, "_", "-" or "."',
                });
              if (datasets.has(dbName)) return fail(res, 409, `Dataset "${dbName}" already exists`);
              if (!['mem', 'persistent'].includes(dbType))
                return fail(res, 400, 'dbType must be "mem" or "persistent"');
              return send(res, 201, info(makeDataset(dbName, dbType)));
            }
            break;
          }
          if (!ds) return fail(res, 404, `No such dataset: ${name}`);
          if (req.method === 'GET') return send(res, 200, info(ds));
          if (req.method === 'DELETE') {
            datasets.delete(name);
            return send(res, 200, { ok: true });
          }
          break;
        }
        case 'stats':
          if (!ds) return fail(res, 404, `No such dataset: ${name}`);
          return send(res, 200, stats(ds));
        case 'schema': {
          if (!ds) return fail(res, 404, `No such dataset: ${name}`);
          const limit = Number(url.searchParams.get('limit') ?? 1000);
          if (!Number.isInteger(limit) || limit < 1 || limit > 10000)
            return fail(res, 400, 'limit must be an integer from 1 to 10000');
          const r = schemaReport(ds, url.searchParams);
          if (r.error) return fail(res, r.status, r.error);
          const cursor = url.searchParams.get('cursor');
          if (extra === 'classes' || extra === 'predicates')
            return send(res, 200, schemaPage(r.report[extra], limit, cursor));
          return send(res, 200, {
            ...r.report,
            classes: schemaPage(r.report.classes, limit, null),
            predicates: schemaPage(r.report.predicates, limit, null),
          });
        }
        case 'prefixes':
          if (!ds) return fail(res, 404, `No such dataset: ${name}`);
          return send(res, 200, { prefixes: ds.prefixes });
        case 'compact':
          if (!ds) return fail(res, 404, `No such dataset: ${name}`);
          return send(
            res,
            200,
            startTask('compact', ds, () => {
              ds.baseQuads = ds.store.size;
              ds.deltaInserts = 0;
              ds.deltaDeletes = 0;
              return `Merged delta into base (${ds.baseQuads} quads)`;
            }),
          );
        case 'backup':
          if (!ds) return fail(res, 404, `No such dataset: ${name}`);
          return send(
            res,
            200,
            startTask(
              'backup',
              ds,
              () => `Wrote backups/${ds.name}_${new Date().toISOString().slice(0, 10)}.nq.gz`,
              1500,
            ),
          );
        case 'reason': {
          if (!ds) return fail(res, 404, `No such dataset: ${name}`);
          if (req.method === 'DELETE') {
            ds.store.update('DROP SILENT GRAPH <urn:sparkles:inferred>');
            ds.reasoning = null;
            return send(res, 200, { ok: true });
          }
          const p = await params(req, url);
          const profile = String(p.get('profile') ?? 'rdfs');
          if (!['rdfs', 'owl-rl', 'rules'].includes(profile))
            return fail(res, 400, `Unknown profile "${profile}"`);
          if (profile === 'rules' && !String(p.get('rules') ?? '').trim())
            return fail(res, 400, 'Custom rules are empty');
          return send(
            res,
            200,
            startTask(
              'reason',
              ds,
              () => {
                const rules =
                  profile === 'rules'
                    ? [String(p.get('rules'))]
                    : profile === 'owl-rl'
                      ? [...RDFS_RULES, ...OWL_RULES]
                      : RDFS_RULES;
                for (let pass = 0; pass < 3; pass++) for (const r of rules) ds.store.update(r);
                ds.reasoning = {
                  profile,
                  inferred: inferredCount(ds),
                  at: new Date().toISOString(),
                };
                return `Inferred ${ds.reasoning.inferred} triples (${profile})`;
              },
              3000,
            ),
          );
        }
        case 'tasks': {
          if (name) {
            const t = tasks.find((x) => x.id === name);
            return t ? send(res, 200, t) : fail(res, 404, `No such task: ${name}`);
          }
          return send(res, 200, tasks.slice(0, 50));
        }
      }
      void extra;
      return fail(res, 404, `Unknown admin endpoint ${path}`);
    }

    // per-dataset protocol
    const ds = datasets.get(seg[0] ?? '');
    if (!ds) return fail(res, 404, `No such dataset: ${seg[0] ?? ''}`);
    const op = seg[1] ?? '';
    if (op === 'upload' && req.method === 'POST') return handleUpload(req, res, ds);
    const p = await params(req, url);
    if (['', 'sparql', 'query'].includes(op)) {
      if (p.has('update')) return handleUpdate(res, ds, p);
      return handleQuery(req, res, ds, p);
    }
    if (op === 'update') return handleUpdate(res, ds, p);
    if (op === 'explain') {
      const query = p.get('query');
      if (!query) return fail(res, 400, 'Missing query parameter');
      try {
        ds.store.query(query.replace(/\bLIMIT\s+\d+/i, '') + (/\bLIMIT\b/i.test(query) ? '' : ''), {
          use_default_graph_as_union: true,
        });
      } catch (e) {
        return send(res, 400, parseErr(e));
      }
      return send(res, 200, { algebra: sse(query), plan: plan(query, 0, 0, false) });
    }
    if (op === 'data' || op === 'get') {
      if (req.method === 'GET' || req.method === 'HEAD') {
        const graph = url.searchParams.get('graph');
        if (graph || url.searchParams.has('default')) {
          const from = graph ? ox.namedNode(graph) : ox.defaultGraph();
          return send(
            res,
            200,
            ds.store.dump({ format: 'text/turtle', from_graph_name: from }),
            'text/turtle',
          );
        }
        return send(
          res,
          200,
          ds.store.dump({ format: 'application/n-quads' }),
          'application/n-quads',
        );
      }
      return fail(res, 405, 'Only GET is mocked for the Graph Store Protocol');
    }
    return fail(res, 404, `Unknown endpoint ${path}`);
  } catch (e) {
    console.error(e);
    fail(res, 500, 'Internal mock error', { detail: String(e?.message ?? e) });
  }
});

server.listen(PORT, () => {
  console.log(
    `sparkles mock listening on http://localhost:${PORT}  (datasets: ${[...datasets.keys()].join(', ')})`,
  );
});
