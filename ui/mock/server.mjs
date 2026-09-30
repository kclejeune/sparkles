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
    endpoints: { query: `${base}/sparql`, update: `${base}/update`, gsp: `${base}/data`, upload: `${base}/upload` },
    quads: ds.store.size,
    reasoning: ds.reasoning,
  };
}

function send(res, status, body, type = 'application/json') {
  const data = typeof body === 'string' || Buffer.isBuffer(body) ? body : JSON.stringify(body);
  res.writeHead(status, { 'Content-Type': type, 'Access-Control-Allow-Origin': '*' });
  res.end(data);
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
      else if (t.datatype && t.datatype.value !== 'http://www.w3.org/2001/XMLSchema#string') o.datatype = t.datatype.value;
      return o;
    }
    case 'Quad':
      return { type: 'triple', value: { subject: termJson(t.subject), predicate: termJson(t.predicate), object: termJson(t.object) } };
    default:
      return { type: 'literal', value: String(t.value) };
  }
}

function parseErr(e) {
  const msg = String(e?.message ?? e);
  const m = /(?:at|line)\s+(\d+):(\d+)/.exec(msg);
  return m ? { error: 'Parse error', detail: msg, line: Number(m[1]), column: Number(m[2]) } : { error: 'Parse error', detail: msg };
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
    .replace(/("""[\s\S]*?"""|"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*')|(<[^<>"{}|^`\\\s]*>)|(#[^\n]*)/g, (m, str, iri) =>
      str ? '"…"' : iri ? iri : '',
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
    const perm = !isVar(s) ? 'SPO' : !isVar(p) && !isVar(o) ? 'POS' : !isVar(p) ? 'PSO' : !isVar(o) ? 'OSP' : 'SPO';
    const est = !isVar(s) ? 8 : !isVar(o) ? 40 : !isVar(p) ? 400 : 5000;
    const rows = Math.max(totalRows, 1) * (1 + ((i * 37) % 5));
    return {
      operator: 'IndexScan',
      description: `${perm} ${s} ${p} ${o}`,
      columns: [s, p, o].filter(isVar),
      sortedOn: [perm === 'SPO' ? s : perm === 'POS' || perm === 'PSO' ? (perm === 'POS' ? o : s) : o].filter(isVar),
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
      ? { operator: 'Values', description: 'single empty row', columns: [], sortedOn: [], estimatedRows: 1, estimatedCost: 1, actualRows: n(1), timeMs: 0, cached: false, children: [] }
      : scans[0];
  for (let i = 1; i < scans.length; i++) {
    const right = scans[i];
    const shared = node.columns.filter((c) => right.columns.includes(c));
    const merge = shared.length && node.sortedOn[0] === shared[0] && right.sortedOn[0] === shared[0];
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
  const vars = /SELECT\s+(?:DISTINCT\s+)?(.*?)\s*(?:WHERE|FROM|\{)/is.exec(q)?.[1]?.match(/[?$]\w+/g);
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
    const isGraph = Array.isArray(result) && (result.length === 0 ? /^\s*(CONSTRUCT|DESCRIBE)/im.test(stripNoise(query)) : !(result[0] instanceof Map));
    if (isGraph) {
      const fmt = GRAPH_FORMATS[fmtParam] ?? pickFormat(accept, Object.values(GRAPH_FORMATS)) ?? 'text/turtle';
      const out = ds.store.query(query, { ...queryOptions(ds, p), results_format: fmt });
      return send(res, 200, out, fmt);
    }
    const fmt = SELECT_FORMATS[fmtParam] ?? pickFormat(accept, Object.values(SELECT_FORMATS)) ?? 'application/sparql-results+json';
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
    const triples = result.map((q) => [termJson(q.subject), termJson(q.predicate), termJson(q.object)]);
    body = {
      queryType: /^\s*DESCRIBE/im.test(stripNoise(query)) ? 'DESCRIBE' : 'CONSTRUCT',
      triples: triples.slice(0, cap),
      total: triples.length,
    };
  } else if (/^\s*(CONSTRUCT|DESCRIBE)/im.test(stripNoise(query))) {
    body = { queryType: /^\s*DESCRIBE/im.test(stripNoise(query)) ? 'DESCRIBE' : 'CONSTRUCT', triples: [], total: 0 };
  } else {
    // Use the JSON serializer for projection order, then reshape into rows.
    const json = JSON.parse(ds.store.query(query, { ...queryOptions(ds, p), results_format: 'application/sparql-results+json' }));
    const vars = json.head.vars;
    const rows = json.results.bindings.map((b) => vars.map((v) => b[v] ?? null));
    body = { queryType: 'SELECT', vars, rows: rows.slice(0, cap), total: rows.length };
  }
  const serializeMs = performance.now() - t1;
  const { total, ...rest } = body;
  const sent = rest.rows?.length ?? rest.triples?.length ?? 1;
  const parseMs = 0.05 + query.length / 20000;
  const planMs = 0.1 + extractPatterns(query).length * 0.04;
  send(res, 200, {
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
  }, 'application/x-sparkles+json');
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
  ttl: 'text/turtle', nt: 'application/n-triples', nq: 'application/n-quads', trig: 'application/trig',
  rdf: 'application/rdf+xml', owl: 'application/rdf+xml', xml: 'application/rdf+xml', n3: 'text/n3',
};

async function handleUpload(req, res, ds) {
  const body = await readBody(req);
  const parts = parseMultipart(body, req.headers['content-type'] ?? '');
  const graph = parts.find((x) => x.name === 'graph' && !x.filename)?.data.toString('utf8').trim();
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
    for (const [, pfx, iri] of text.matchAll(/@prefix\s+([\w-]*):\s*<([^>]+)>/gi)) ds.prefixes[pfx] = iri;
    count += ds.store.size - before;
    ds.deltaInserts += ds.store.size - before;
  }
  send(res, 200, { count, tripleCount: count, quadCount: count });
}

// ---------------------------------------------------------------------------
// tasks

function startTask(kind, ds, work, durationMs = 2500) {
  const task = { id: String(taskSeq++), kind, dataset: ds.name, state: 'running', startedAt: new Date().toISOString(), progress: 0 };
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
  const graphs = [{ name: null, quads: ds.store.match(null, null, null, ox.defaultGraph()).length }];
  for (const b of ds.store.query('SELECT ?g (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g ORDER BY DESC(?n)')) {
    graphs.push({ name: b.get('g').value, quads: num(b.get('n')) });
  }
  const predicates = q(
    'SELECT ?p (COUNT(*) AS ?n) (COUNT(DISTINCT ?s) AS ?ds) (COUNT(DISTINCT ?o) AS ?do) WHERE { ?s ?p ?o } GROUP BY ?p ORDER BY DESC(?n) LIMIT 100',
  ).map((b) => ({ iri: b.get('p').value, count: num(b.get('n')), distinctSubjects: num(b.get('ds')), distinctObjects: num(b.get('do')) }));
  const classes = q('SELECT ?c (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s a ?c FILTER(isIRI(?c)) } GROUP BY ?c ORDER BY DESC(?n) LIMIT 100').map(
    (b) => ({ iri: b.get('c').value, instances: num(b.get('n')) }),
  );
  const terms = num(q('SELECT (COUNT(DISTINCT ?t) AS ?n) WHERE { { ?t ?p ?o } UNION { ?s ?t ?o } UNION { ?s ?p ?t } }')[0]?.get('n'));
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
// router

const server = http.createServer(async (req, res) => {
  if (LATENCY) await new Promise((r) => setTimeout(r, LATENCY));
  const url = new URL(req.url ?? '/', 'http://localhost');
  const path = decodeURIComponent(url.pathname);
  const seg = path.split('/').filter(Boolean);
  const log = () => console.log(`${new Date().toISOString().slice(11, 19)} ${req.method} ${req.url?.slice(0, 120)} → ${res.statusCode}`);
  res.on('finish', log);
  try {
    if (req.method === 'OPTIONS') {
      res.writeHead(204, { 'Access-Control-Allow-Origin': '*', 'Access-Control-Allow-Headers': '*', 'Access-Control-Allow-Methods': '*' });
      return res.end();
    }
    if (seg[0] === '$') {
      const [, what, name, extra] = seg;
      const ds = name ? datasets.get(name) : undefined;
      switch (what) {
        case 'ping':
          return send(res, 200, new Date().toISOString(), 'text/plain');
        case 'server':
          return send(res, 200, {
            version: VERSION,
            startedAt: startedAt.toISOString(),
            uptimeSeconds: Math.round((Date.now() - startedAt.getTime()) / 1000),
            datasets: [...datasets.values()].map(info),
          });
        case 'datasets': {
          if (!name) {
            if (req.method === 'GET') return send(res, 200, { datasets: [...datasets.values()].map(info) });
            if (req.method === 'POST') {
              const p = await params(req, url);
              const dbName = String(p.get('dbName') ?? '').replace(/^\//, '');
              const dbType = String(p.get('dbType') ?? 'mem');
              if (!/^[A-Za-z0-9_.-]+$/.test(dbName)) return fail(res, 400, 'Invalid dataset name', { detail: 'Use letters, digits, "_", "-" or "."' });
              if (datasets.has(dbName)) return fail(res, 409, `Dataset "${dbName}" already exists`);
              if (!['mem', 'persistent'].includes(dbType)) return fail(res, 400, 'dbType must be "mem" or "persistent"');
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
        case 'prefixes':
          if (!ds) return fail(res, 404, `No such dataset: ${name}`);
          return send(res, 200, { prefixes: ds.prefixes });
        case 'compact':
          if (!ds) return fail(res, 404, `No such dataset: ${name}`);
          return send(res, 200, startTask('compact', ds, () => {
            ds.baseQuads = ds.store.size;
            ds.deltaInserts = 0;
            ds.deltaDeletes = 0;
            return `Merged delta into base (${ds.baseQuads} quads)`;
          }));
        case 'backup':
          if (!ds) return fail(res, 404, `No such dataset: ${name}`);
          return send(res, 200, startTask('backup', ds, () => `Wrote backups/${ds.name}_${new Date().toISOString().slice(0, 10)}.nq.gz`, 1500));
        case 'reason': {
          if (!ds) return fail(res, 404, `No such dataset: ${name}`);
          if (req.method === 'DELETE') {
            ds.store.update('DROP SILENT GRAPH <urn:sparkles:inferred>');
            ds.reasoning = null;
            return send(res, 200, { ok: true });
          }
          const p = await params(req, url);
          const profile = String(p.get('profile') ?? 'rdfs');
          if (!['rdfs', 'owl-rl', 'rules'].includes(profile)) return fail(res, 400, `Unknown profile "${profile}"`);
          if (profile === 'rules' && !String(p.get('rules') ?? '').trim()) return fail(res, 400, 'Custom rules are empty');
          return send(res, 200, startTask('reason', ds, () => {
            const rules = profile === 'rules' ? [String(p.get('rules'))] : profile === 'owl-rl' ? [...RDFS_RULES, ...OWL_RULES] : RDFS_RULES;
            for (let pass = 0; pass < 3; pass++) for (const r of rules) ds.store.update(r);
            ds.reasoning = { profile, inferred: inferredCount(ds), at: new Date().toISOString() };
            return `Inferred ${ds.reasoning.inferred} triples (${profile})`;
          }, 3000));
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
        ds.store.query(query.replace(/\bLIMIT\s+\d+/i, '') + (/\bLIMIT\b/i.test(query) ? '' : ''), { use_default_graph_as_union: true });
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
          return send(res, 200, ds.store.dump({ format: 'text/turtle', from_graph_name: from }), 'text/turtle');
        }
        return send(res, 200, ds.store.dump({ format: 'application/n-quads' }), 'application/n-quads');
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
  console.log(`sparkles mock listening on http://localhost:${PORT}  (datasets: ${[...datasets.keys()].join(', ')})`);
});
