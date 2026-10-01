#!/usr/bin/env node
// Sparkles mock backend — implements enough of docs/API.md for UI development.
// SPARQL is evaluated for real by oxigraph (WASM, dev dependency); query plans,
// timings split, cache stats and disk sizes are fabricated.
//
//   node mock/server.mjs            # listens on :3030 (PORT env to override)
//   MOCK_LATENCY=300 node mock/...  # add artificial latency (ms) to every request

import { randomUUID } from 'node:crypto';
import http from 'node:http';
import { performance } from 'node:perf_hooks';
import ox from 'oxigraph';
import { handleBackups } from './backups.mjs';
import { PREFIXES, buildTurtle, provenanceTrig, scratchTurtle, vectorTurtle } from './data.mjs';

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
    id: randomUUID(),
    store: new ox.Store(),
    prefixes: { ...PREFIXES },
    reasoning: null,
    baseQuads: 0,
    deltaInserts: 0,
    deltaDeletes: 0,
    cache: { entries: 0, bytes: 0, hits: 0, misses: 0 },
    /** Commit catalog, oldest first (docs/API.md "Commits"). */
    commits: [],
    firstRetained: 0,
    /** Full-text configuration (null: disabled) and whether a rebuild task runs. */
    text: null,
    textBuilding: false,
    textRebuilt: null,
  };
  addCommit(ds, 'create', 0, 0);
  datasets.set(name, ds);
  return ds;
}

// ---------------------------------------------------------------------------
// commits

function addCommit(ds, kind, inserted, deleted, opts = {}) {
  const head = ds.commits[ds.commits.length - 1];
  const seq = head ? head.seq + 1 : 0;
  const ts = Math.max(opts.at ?? Date.now(), head ? Date.parse(head.timestamp) : 0);
  const c = {
    seq,
    parent: seq ? seq - 1 : null,
    ref: `commit:${seq}`,
    timestamp: new Date(ts).toISOString(),
    kind,
    inserted,
    deleted,
    quads: opts.quads ?? ds.store.size,
    generation: ds.type === 'mem' ? 'mem' : 'gen-0001',
    bulk: !!opts.bulk,
    exact: opts.exact ?? true,
  };
  ds.commits.push(c);
  return c;
}

const headCommit = (ds) => ds.commits[ds.commits.length - 1];

/** Quads of the store as N-Quads lines, to diff a write. */
const quadSet = (ds) => new Set(ds.store.match().map(String));

/** Commit a write from the quad sets before and after it; the receipt. */
function commitWrite(ds, kind, before, opts = {}) {
  const after = quadSet(ds);
  let inserted = 0;
  let deleted = 0;
  for (const q of after) if (!before.has(q)) inserted++;
  for (const q of before) if (!after.has(q)) deleted++;
  const committed = inserted + deleted > 0;
  const commit = committed ? addCommit(ds, kind, inserted, deleted, opts) : headCommit(ds);
  return { dataset: ds.name, datasetId: ds.id, committed, commit };
}

const wantsReceipt = (req, p) =>
  ['true', '1', 'yes'].includes(String(p.get('receipt') ?? '').toLowerCase()) ||
  String(req.headers.accept ?? '').includes('application/x-sparkles+json');

/** A plausible history for the demo dataset: a bulk load and many small updates. */
function seedHistory(ds) {
  const size = ds.store.size;
  const now = Date.now();
  const updates = Array.from({ length: 58 }, (_, i) => ({
    inserted: (i * 7) % 5,
    deleted: i % 6 === 0 ? 1 + (i % 3) : 0,
  }));
  const net = updates.reduce((n, u) => n + u.inserted - u.deleted, 0);
  let quads = size - net;
  ds.commits = [];
  const start = now - 12 * 86400_000;
  addCommit(ds, 'create', 0, 0, { at: start, quads: 0 });
  addCommit(ds, 'load', quads, 0, { at: start + 60_000, quads, bulk: true });
  updates.forEach((u, i) => {
    quads += u.inserted - u.deleted;
    const kind = i === 20 ? 'gsp-put' : i === 33 ? 'upload' : 'update';
    const at = start + 3600_000 + ((now - 3600_000 - start) * i) / updates.length;
    addCommit(ds, kind, u.inserted, u.deleted, {
      at,
      quads,
      bulk: kind === 'upload',
      exact: !(kind === 'upload' && u.deleted > 0),
    });
  });
  // the oldest commits are no longer retained
  ds.firstRetained = 3;
  ds.commits = ds.commits.filter((c) => c.seq >= ds.firstRetained);
}

{
  const foaf = makeDataset('foaf', 'persistent');
  foaf.store.load(buildTurtle(), { format: 'text/turtle' });
  foaf.store.load(provenanceTrig, { format: 'application/trig' });
  foaf.store.load(vectorTurtle(), { format: 'text/turtle' });
  foaf.baseQuads = foaf.store.size;
  foaf.text = { predicates: 'all', graphs: { include: 'all', exclude: [] } };
  foaf.textRebuilt = { at: new Date().toISOString(), ms: 42.5 };
  seedHistory(foaf);
  const scratch = makeDataset('scratch', 'mem');
  scratch.store.load(scratchTurtle, { format: 'text/turtle' });
  scratch.baseQuads = scratch.store.size;
  addCommit(scratch, 'upload', scratch.store.size, 0, { bulk: true });
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
    id: ds.id,
    head: headCommit(ds).seq,
    modified: headCommit(ds).timestamp,
    text: ds.text ? { state: textState(ds), docs: textDocs(ds).length } : null,
    ...(ds.origin ? { forkedFrom: ds.origin.forkedFrom, origin: ds.origin } : {}),
  };
}

function send(res, status, body, type = 'application/json', headers = {}) {
  const data = typeof body === 'string' || Buffer.isBuffer(body) ? body : JSON.stringify(body);
  res.writeHead(status, {
    'Content-Type': type,
    'Access-Control-Allow-Origin': '*',
    'Access-Control-Expose-Headers': 'X-Request-Id, Sparkles-Commit, Sparkles-Dataset-Id',
    'X-Request-Id': res.requestId ?? '',
    ...headers,
  });
  res.end(data);
}

const commitHeaders = (ds, seq = headCommit(ds).seq) => ({
  'Sparkles-Commit': String(seq),
  'Sparkles-Dataset-Id': ds.id,
});

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
const OUTCOMES = ['ok', 'client_error', 'error', 'timeout', 'cancelled', 'budget', 'rate_limited'];
const LIMITS = {
  timeoutSeconds: 60,
  maxTimeoutSeconds: 1800,
  queryMemoryBytes: 8 * 2 ** 30,
  maxResultBytes: 2 ** 30,
  maxExportBytes: 0,
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
        budgetExceeded: {
          rows: 0,
          memory: 0,
          'result-bytes': 0,
          'decompressed-bytes': 0,
          'outbound-bytes': 0,
        },
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

/**
 * The mock's stand-in for the formatter: runs of spaces and tabs become one space
 * (outside strings, IRIs and comments), lines lose their trailing whitespace, and the
 * text ends with one newline. The cursor (UTF-16 offset) maps through the same edits. A
 * bracket that does not match is a syntax error at the line and column where it is.
 */
function mockFormat(text, cursor) {
  const pairs = { ')': '(', '}': '{', ']': '[' };
  const stack = [];
  let out = '';
  // out offset for each input offset, to map the cursor
  const map = new Array(text.length + 1);
  let quote = null; // '"', "'", '<' or '#' while inside a string, an IRI or a comment
  let line = 1;
  let col = 1;
  for (let i = 0; i < text.length; i++) {
    map[i] = out.length;
    const c = text[i];
    if (quote) {
      out += c;
      if ((quote === '#' && c === '\n') || (quote !== '#' && c === quote && text[i - 1] !== '\\'))
        quote = null;
    } else if (c === ' ' || c === '\t') {
      const next = text[i + 1];
      // keep one space, none before a line break or another space
      if (next !== ' ' && next !== '\t' && next !== '\n' && next !== undefined) out += ' ';
    } else {
      if (c === '"' || c === "'" || c === '#') quote = c;
      else if (c === '<' && /^<[^\s<>"{}|^`\\]*>/.test(text.slice(i))) quote = '>';
      else if (c === '(' || c === '{' || c === '[') stack.push({ c, line, col });
      else if (pairs[c]) {
        const open = stack.pop();
        if (!open || open.c !== pairs[c])
          return { error: { message: `unexpected '${c}'`, line, column: col } };
      }
      out += c;
    }
    if (c === '\n') {
      line++;
      col = 1;
    } else col++;
  }
  map[text.length] = out.length;
  if (stack.length) {
    const open = stack[stack.length - 1];
    return { error: { message: `'${open.c}' is not closed`, line: open.line, column: open.col } };
  }
  const trimmed = out.replace(/\s+$/, '');
  const text2 = trimmed + '\n';
  const c =
    typeof cursor === 'number'
      ? Math.min(map[Math.min(cursor, text.length)], trimmed.length)
      : null;
  return { text: text2, cursor: c };
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
// full-text search and vector similarity. oxigraph knows neither property function, so
// queries using `text:query` or `spk:vectorSearch` are answered here from the call alone
// (the rest of the WHERE clause is ignored); the result has the subject-list variables.

const TEXT_QUERY_IRI = 'http://jena.apache.org/text#query';
const VECTOR_SEARCH_IRI = 'urn:x-sparkles:vectorSearch';
const VECTOR_DT = 'urn:x-sparkles:vector';
const XSD = 'http://www.w3.org/2001/XMLSchema#';
const RDF_LANGSTRING = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#langString';
const DEFAULT_GRAPH_IRI = 'urn:x-arq:DefaultGraph';
/** Packed-vector budget in bytes (`MOCK_VECTOR_BUDGET=1000` to try the 507). */
const VECTOR_BUDGET = Number(process.env.MOCK_VECTOR_BUDGET ?? Infinity);

class QueryError extends Error {
  constructor(status, message, extra = {}) {
    super(message);
    this.status = status;
    this.extra = extra;
  }
}

function textDocs(ds) {
  const cfg = ds.text;
  if (!cfg) return [];
  const preds = cfg.predicates === 'all' ? null : new Set(cfg.predicates);
  const include = cfg.graphs?.include ?? 'all';
  const exclude = new Set(cfg.graphs?.exclude ?? []);
  return ds.store.match().filter((q) => {
    if (q.object.termType !== 'Literal') return false;
    const dt = q.object.datatype.value;
    if (dt !== XSD + 'string' && dt !== RDF_LANGSTRING) return false;
    if (preds && !preds.has(q.predicate.value)) return false;
    const g = q.graph.termType === 'DefaultGraph' ? DEFAULT_GRAPH_IRI : q.graph.value;
    if (g === INFERRED.value || exclude.has(g)) return false;
    return include === 'all' || include.includes(g);
  });
}

const textState = (ds) => (ds.textBuilding ? 'stale' : 'ready');

function textStatusJson(ds) {
  const docs = textDocs(ds).length;
  const head = headCommit(ds).seq;
  return {
    enabled: true,
    state: textState(ds),
    docs,
    seq: ds.textBuilding ? Math.max(0, head - 1) : head,
    storeSeq: head,
    epoch: 1,
    diskBytes: 4096 + docs * 180,
    segments: 1,
    config: { maxTextBytes: 262144, maxHits: 1000000, ...ds.text },
    formatVersion: 1,
    ...(ds.textRebuilt
      ? { lastRebuild: { at: ds.textRebuilt.at, ms: ds.textRebuilt.ms, docs } }
      : {}),
    ...(ds.textBuilding ? { message: 'rebuild in progress' } : {}),
  };
}

/** Find `(subjects) <fn> (args)` in a query; null when the function is not used. */
function propertyCall(query, prefixed, iri) {
  const fn = `(?:${prefixed.replace(':', '\\:')}|<${iri.replace(/[.#]/g, '\\$&')}>)`;
  const m = new RegExp(`\\(([^()]*)\\)\\s*${fn}\\s*\\(((?:"(?:[^"\\\\]|\\\\.)*"|[^()"])*)\\)`).exec(
    query,
  );
  if (!m) return null;
  return { subjects: m[1].trim().split(/\s+/).filter(Boolean), args: tokenizeArgs(m[2]) };
}

/** Argument list tokens: IRIs, prefixed names, string literals (with @lang / ^^type), numbers. */
function tokenizeArgs(src) {
  const pfx = { ...PREFIXES, spk: 'urn:x-sparkles:' };
  const out = [];
  const re =
    /\s*(?:<([^>]*)>|"((?:[^"\\]|\\.)*)"(?:@([\w-]+)|\^\^(?:<([^>]*)>|([\w-]*):([\w-]*)))?|(-?\d+(?:\.\d+)?)|([\w-]*):([\w.-]*))/y;
  let m;
  while (re.lastIndex < src.length && (m = re.exec(src))) {
    if (m[1] != null) out.push({ kind: 'iri', value: m[1] });
    else if (m[2] != null)
      out.push({
        kind: 'literal',
        value: JSON.parse(`"${m[2].replace(/\\([^"\\/bfnrtu])/g, '\\\\$1')}"`),
        lang: m[3],
        datatype: m[4] ?? (m[5] != null ? pfx[m[5]] + m[6] : undefined),
      });
    else if (m[7] != null) out.push({ kind: 'number', value: Number(m[7]) });
    else out.push({ kind: 'iri', value: (pfx[m[8]] ?? '') + m[9] });
    if (!src.slice(re.lastIndex).trim()) break;
  }
  return out;
}

const fold = (s) =>
  s
    .normalize('NFD')
    .replace(/[\u0300-\u036f]/g, '')
    .toLowerCase();
const tokens = (s) =>
  fold(s)
    .split(/[^a-z0-9]+/)
    .filter(Boolean);

/** A small subset of the query syntax: terms, "phrases"(*), +/-, AND/OR, \-escapes. */
function parseTextQuery(q) {
  const clauses = [];
  let i = 0;
  let pendingAnd = false;
  while (i < q.length) {
    const c = q[i];
    if (/[\s()]/.test(c)) {
      i++;
      continue;
    }
    let occur = 'should';
    if (c === '+' || c === '-') {
      occur = c === '+' ? 'must' : 'not';
      i++;
    }
    let words;
    let prefix = false;
    if (q[i] === '"') {
      const end = q.indexOf('"', i + 1);
      if (end < 0) throw new QueryError(400, `text:query: Syntax Error: ${q}`);
      words = tokens(q.slice(i + 1, end));
      i = end + 1;
      if (q[i] === '*') ((prefix = true), i++);
      const slop = /^~\d+/.exec(q.slice(i));
      if (slop) i += slop[0].length;
    } else {
      let w = '';
      while (i < q.length && !/[\s()"]/.test(q[i])) {
        if (q[i] === '\\' && i + 1 < q.length) {
          w += q[i + 1];
          i += 2;
          continue;
        }
        if (q[i] === ':') throw new QueryError(400, `text:query: Field does not exist: '${w}'`);
        w += q[i++];
      }
      if (w === 'AND' || w === 'OR') {
        if (!clauses.length) throw new QueryError(400, `text:query: Syntax Error: ${q}`);
        if (w === 'AND') {
          if (clauses[clauses.length - 1].occur === 'should')
            clauses[clauses.length - 1].occur = 'must';
          pendingAnd = true;
        }
        continue;
      }
      if (w.endsWith('*')) ((prefix = true), (w = w.slice(0, -1)));
      words = tokens(w);
    }
    if (!words.length) continue;
    if (pendingAnd && occur === 'should') occur = 'must';
    pendingAnd = false;
    clauses.push({ occur, words, prefix });
  }
  if (pendingAnd) throw new QueryError(400, `text:query: Syntax Error: ${q}`);
  return clauses;
}

function clauseMatches(clause, toks) {
  const { words, prefix } = clause;
  let n = 0;
  for (let i = 0; i + words.length <= toks.length; i++) {
    const ok = words.every((w, j) =>
      prefix && j === words.length - 1 ? toks[i + j].startsWith(w) : toks[i + j] === w,
    );
    if (ok) n++;
  }
  return n;
}

function runTextQuery(ds, call) {
  if (!ds.text)
    throw new QueryError(
      400,
      'dataset has no full-text index; enable it with `sparkles text-index` or --text',
    );
  if (ds.textBuilding) {
    const head = headCommit(ds).seq;
    throw new QueryError(
      503,
      `full-text index of 'dataset' is stale (index seq ${head - 1}, data seq ${head}); retry later or rebuild it`,
    );
  }
  const preds = call.args.filter((a) => a.kind === 'iri').map((a) => a.value);
  const query = call.args.find((a) => a.kind === 'literal' && !/^lang:/.test(a.value));
  if (!query) throw new QueryError(400, 'text:query: malformed argument list');
  const limit = call.args.find((a) => a.kind === 'number')?.value ?? Infinity;
  const lang =
    call.args.find((a) => a.kind === 'literal' && /^lang:/.test(a.value))?.value.slice(5) ??
    query.lang;
  for (const p of preds)
    if (ds.text.predicates !== 'all' && !ds.text.predicates.includes(p))
      throw new QueryError(400, `text:query: <${p}> is not text-indexed`);
  const clauses = parseTextQuery(query.value);
  const docs = textDocs(ds).filter(
    (q) =>
      (!preds.length || preds.includes(q.predicate.value)) &&
      (!lang || fold(q.object.language ?? '') === fold(lang)),
  );
  const hits = [];
  for (const q of docs) {
    const toks = tokens(q.object.value);
    let score = 0;
    let ok = true;
    let any = false;
    for (const c of clauses) {
      const n = clauseMatches(c, toks);
      if (c.occur === 'not' && n) ok = false;
      if (c.occur === 'must' && !n) ok = false;
      if (c.occur !== 'not' && n) {
        any = true;
        score += (n * (1 + c.words.length)) / Math.sqrt(toks.length);
      }
    }
    if (ok && any) hits.push({ q, score });
  }
  hits.sort((a, b) => b.score - a.score || String(a.q.subject).localeCompare(String(b.q.subject)));
  const slots = ['s', 'score', 'literal', 'graph', 'predicate'];
  const rows = hits.slice(0, limit).map(({ q, score }) => {
    const g = q.graph.termType === 'DefaultGraph' ? DEFAULT_GRAPH_IRI : q.graph.value;
    const all = [
      termJson(q.subject),
      { type: 'literal', value: String(Math.round(score * 1e6) / 1e6), datatype: XSD + 'float' },
      termJson(q.object),
      { type: 'uri', value: g },
      termJson(q.predicate),
    ];
    return call.subjects.map((_, i) => all[i] ?? null);
  });
  void slots;
  return rows;
}

function parseVec(s) {
  try {
    const v = JSON.parse(s);
    return Array.isArray(v) && v.length && v.every((x) => Number.isFinite(x)) ? v : null;
  } catch {
    return null;
  }
}

const METRIC_FNS = {
  cosine: (a, b) => {
    let d = 0;
    let na = 0;
    let nb = 0;
    for (let i = 0; i < a.length; i++) ((d += a[i] * b[i]), (na += a[i] ** 2), (nb += b[i] ** 2));
    return d / Math.sqrt(na * nb);
  },
  dot: (a, b) => a.reduce((s, x, i) => s + x * b[i], 0),
  euclidean: (a, b) => Math.sqrt(a.reduce((s, x, i) => s + (x - b[i]) ** 2, 0)),
};

function runVectorSearch(ds, call) {
  const [pred, q, ...opts] = call.args;
  if (pred?.kind !== 'iri' || !q)
    throw new QueryError(400, 'spk:vectorSearch: expected (predicate query [k] [options])');
  let k = 10;
  let metric = 'cosine';
  for (const o of opts) {
    if (o.kind === 'number') k = o.value;
    else if (o.kind === 'literal' && o.value.startsWith('metric:')) metric = o.value.slice(7);
  }
  if (!METRIC_FNS[metric])
    throw new QueryError(400, `spk:vectorSearch: unknown metric "${metric}"`);
  const rows = ds.store
    .match(null, ox.namedNode(pred.value), null, null)
    .filter((x) => x.object.termType === 'Literal' && x.object.datatype.value === VECTOR_DT);
  let query;
  if (q.kind === 'literal') {
    query = parseVec(q.value);
    if (!query) throw new QueryError(400, 'spk:vectorSearch: malformed vector literal');
  } else {
    const own = [
      ...new Set(rows.filter((x) => x.subject.value === q.value).map((x) => x.object.value)),
    ];
    if (!own.length) return [];
    if (own.length > 1)
      throw new QueryError(
        400,
        `entity <${q.value}> has ${own.length} vectors for the predicate; pass a vector literal`,
      );
    query = parseVec(own[0]);
    if (!query) throw new QueryError(400, "the entity's vector is not a valid spk:vector");
  }
  const vectors = rows.map((x) => ({ x, v: parseVec(x.object.value) })).filter((r) => r.v);
  const same = vectors.filter((r) => r.v.length === query.length);
  if (vectors.length && !same.length) {
    const dims = [...new Set(vectors.map((r) => r.v.length))].sort((a, b) => a - b);
    throw new QueryError(
      400,
      `dimension mismatch: the predicate has vectors of dimension ${dims.join(', ')}; query has ${query.length}`,
    );
  }
  const bytes = vectors.reduce((n, r) => n + r.v.length * 4 + 16, 0);
  if (bytes > VECTOR_BUDGET)
    throw new QueryError(
      507,
      `query exceeds its memory budget: packed vectors need ${bytes} bytes, limit ${VECTOR_BUDGET}`,
      { budget: 'memory', limit: VECTOR_BUDGET, requested: bytes },
    );
  const lower = metric === 'euclidean';
  const scored = same
    .map((r) => ({ x: r.x, score: METRIC_FNS[metric](query, r.v) }))
    .filter((r) => Number.isFinite(r.score))
    .sort(
      (a, b) =>
        (lower ? a.score - b.score : b.score - a.score) ||
        a.x.subject.value.localeCompare(b.x.subject.value),
    )
    .slice(0, k);
  return scored.map(({ x, score }) => {
    const all = [
      termJson(x.subject),
      { type: 'literal', value: String(score), datatype: XSD + 'double' },
      termJson(x.object),
    ];
    return call.subjects.map((_, i) => all[i] ?? null);
  });
}

/** Answer a property-function query; false when the query uses neither function. */
function handleSearchQuery(req, res, ds, p, query) {
  const text = propertyCall(query, 'text:query', TEXT_QUERY_IRI);
  const vector = text ? null : propertyCall(query, 'spk:vectorSearch', VECTOR_SEARCH_IRI);
  if (!text && !vector) return false;
  const t0 = performance.now();
  const call = text ?? vector;
  let rows;
  try {
    rows = text ? runTextQuery(ds, call) : runVectorSearch(ds, call);
  } catch (e) {
    if (e instanceof QueryError) fail(res, e.status, e.message, e.extra);
    else fail(res, 400, String(e?.message ?? e));
    return true;
  }
  const execMs = performance.now() - t0;
  const vars = call.subjects.map((v) => v.replace(/^[?$]/, ''));
  if (!String(req.headers.accept ?? '').includes('application/x-sparkles+json')) {
    const bindings = rows.map((r) =>
      Object.fromEntries(vars.flatMap((v, i) => (r[i] ? [[v, r[i]]] : []))),
    );
    send(res, 200, { head: { vars }, results: { bindings } }, 'application/sparql-results+json');
    return true;
  }
  const cap = p.has('send') ? Math.max(0, Number(p.get('send'))) : Infinity;
  send(
    res,
    200,
    {
      queryType: 'SELECT',
      vars,
      rows: rows.slice(0, cap),
      meta: {
        totalRows: rows.length,
        sentRows: Math.min(rows.length, cap),
        timing: {
          parseMs: 0.08,
          planMs: 0.05,
          execMs: +execMs.toFixed(3),
          serializeMs: 0.02,
          totalMs: +(execMs + 0.15).toFixed(3),
        },
        plan: {
          operator: text ? 'TextSearch' : 'VectorSearch',
          description: text ? 'text:query' : 'spk:vectorSearch',
          columns: call.subjects,
          sortedOn: [],
          estimatedRows: rows.length,
          estimatedCost: rows.length,
          actualRows: rows.length,
          timeMs: +execMs.toFixed(3),
          cached: false,
          children: [],
        },
        commit: headCommit(ds).seq,
        datasetId: ds.id,
      },
    },
    'application/x-sparkles+json',
    commitHeaders(ds),
  );
  return true;
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
  if (handleSearchQuery(req, res, ds, p, query)) return;
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
        commit: headCommit(ds).seq,
        datasetId: ds.id,
      },
    },
    'application/x-sparkles+json',
    commitHeaders(ds),
  );
}

function handleUpdate(req, res, ds, p) {
  const update = p.get('update');
  if (!update) return fail(res, 400, 'Missing update parameter');
  const t0 = performance.now();
  const before = quadSet(ds);
  try {
    ds.store.update(update);
  } catch (e) {
    return send(res, 400, parseErr(e));
  }
  const receipt = commitWrite(ds, 'update', before);
  ds.deltaInserts += receipt.committed ? receipt.commit.inserted : 0;
  ds.deltaDeletes += receipt.committed ? receipt.commit.deleted : 0;
  ds.cache = { entries: 0, bytes: 0, hits: ds.cache.hits, misses: ds.cache.misses };
  const headers = commitHeaders(ds, receipt.commit.seq);
  if (!wantsReceipt(req, p))
    return send(
      res,
      200,
      `<html><body><p>Update succeeded</p></body></html>`,
      'text/html',
      headers,
    );
  const ms = performance.now() - t0;
  send(
    res,
    200,
    {
      inserted: receipt.committed ? receipt.commit.inserted : 0,
      deleted: receipt.committed ? receipt.commit.deleted : 0,
      operations: 1,
      timing: {
        parseMs: 0.05,
        planMs: 0,
        execMs: +ms.toFixed(3),
        serializeMs: 0,
        totalMs: +ms.toFixed(3),
      },
      memPeakBytes: 0,
      ...receipt,
    },
    'application/x-sparkles+json',
    headers,
  );
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

async function handleUpload(req, res, ds, url) {
  const before = quadSet(ds);
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
    const text = part.data.toString('utf8');
    try {
      ds.store.load(text, { format, ...(graph ? { to_graph_name: ox.namedNode(graph) } : {}) });
    } catch (e) {
      return send(res, 400, { ...parseErr(e), error: `Failed to parse ${part.filename}` });
    }
    for (const [, pfx, iri] of text.matchAll(/@prefix\s+([\w-]*):\s*<([^>]+)>/gi))
      ds.prefixes[pfx] = iri;
  }
  const receipt = commitWrite(ds, 'upload', before, { bulk: true });
  count = receipt.committed ? receipt.commit.inserted : 0;
  ds.deltaInserts += count;
  const out = { count, tripleCount: count, quadCount: count };
  const headers = commitHeaders(ds, receipt.commit.seq);
  if (wantsReceipt(req, url.searchParams))
    return send(res, 200, { ...out, ...receipt }, 'application/x-sparkles+json', headers);
  send(res, 200, out, 'application/json', headers);
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
    // backup repositories, backups, policies and task cancellation (mock/backups.mjs)
    if (
      seg[0] === '$' &&
      (await handleBackups(req, res, url, seg, {
        datasets,
        tasks,
        nextTaskId: () => String(taskSeq++),
        makeDataset,
        addCommit,
        headCommit,
        send,
        readBody,
      }))
    )
      return;
    if (seg[0] === '$') {
      const [, what, name, extra] = seg;
      const ds = name ? datasets.get(name) : undefined;
      switch (what) {
        case 'ping':
          return send(res, 200, new Date().toISOString(), 'text/plain');
        // the formatter, a stand-in: normalizes whitespace (see mockFormat) and reports
        // unbalanced brackets as a syntax error
        case 'format': {
          if (req.method !== 'POST') return fail(res, 405, 'method not allowed');
          let body;
          try {
            body = JSON.parse((await readBody(req)).toString('utf8'));
          } catch {
            return fail(res, 400, 'invalid JSON', { code: 'bad-request' });
          }
          if (typeof body?.text !== 'string')
            return fail(res, 400, 'expected `text`', { code: 'bad-request' });
          const out = mockFormat(body.text, body.cursorOffset);
          if (out.error) {
            return fail(res, 400, out.error.message, {
              code: 'syntax',
              line: out.error.line,
              column: out.error.column,
              language: 'sparql',
            });
          }
          return send(res, 200, {
            text: out.text,
            changed: out.text !== body.text,
            language: body.language ?? 'sparql',
            cursorOffset: out.cursor,
            warnings: [],
          });
        }
        // the mock runs open, like a server without --auth-config
        case 'whoami':
          return send(res, 200, {
            authEnabled: false,
            principal: { kind: 'local' },
            method: 'none',
            server: ['server-admin'],
            datasets: Object.fromEntries([...datasets.keys()].map((n) => [n, 'admin'])),
            canMintTokens: false,
            logout: false,
          });
        case 'auth':
          if (name === 'config') return send(res, 200, { enabled: false });
          return fail(res, 404, 'not found');
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
          if (extra === 'clone' && req.method === 'POST') {
            const p = await params(req, url);
            const target = String(p.get('name') ?? '');
            if (!/^[A-Za-z0-9_-][A-Za-z0-9_.-]*$/.test(target))
              return fail(res, 400, `invalid dataset name '${target}'`);
            if (datasets.has(target)) return fail(res, 409, `dataset /${target} already exists`);
            const task = startTask('clone', ds, () => {
              const c = makeDataset(target, 'persistent');
              for (const q of ds.store.match()) c.store.add(q);
              c.baseQuads = c.store.size;
              c.text = ds.text ? structuredClone(ds.text) : null;
              c.textRebuilt = ds.text ? { at: new Date().toISOString(), ms: 12 } : null;
              c.commits = [];
              addCommit(c, 'create', 0, 0, { quads: c.store.size, bulk: true });
              c.reasoning = p.get('inferences') === 'drop' ? null : ds.reasoning;
              c.origin = {
                originFormat: 1,
                clonedAt: new Date().toISOString(),
                source: { name: ds.name, version: 0, generation: 'gen-0001', quads: ds.store.size },
                forkedFrom: { id: ds.id, seq: headCommit(ds).seq },
                inferences: c.reasoning ? 'copy' : 'drop',
              };
              return `cloned /${ds.name} at commit 0 (${c.store.size} quads) into /${target}`;
            });
            task.target = target;
            return send(res, 202, task);
          }
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
              () => `Wrote backups/${ds.name}_${new Date().toISOString().slice(0, 10)}.nq.zst`,
              1500,
            ),
          );
        case 'reason': {
          if (!ds) return fail(res, 404, `No such dataset: ${name}`);
          if (req.method === 'GET' && extra === 'diagnostics')
            return send(res, 200, {
              diagnosticsFormat: 1,
              dataset: ds.name,
              commit: 0,
              computedAt: new Date().toISOString(),
              scope: {
                graph: 'default',
                inferences: { included: !!ds.reasoning, stale: false, commitsSince: 0 },
                closure: url.searchParams.get('closure') ?? 'subclass',
              },
              status: 'none-found',
              note: "Checks a fixed subset of OWL 2 RL inconsistency rules; 'none-found' does not establish OWL consistency.",
              checks: [],
              findings: [],
            });
          if (req.method === 'GET')
            return send(
              res,
              200,
              ds.reasoning
                ? {
                    ...ds.reasoning,
                    commit: 0,
                    head: 0,
                    stale: false,
                    commitsSince: 0,
                    auto: { enabled: false },
                    warnings: [],
                  }
                : { reasoning: null, head: 0 },
            );
          if (req.method === 'DELETE') {
            const before = quadSet(ds);
            ds.store.update('DROP SILENT GRAPH <urn:sparkles:inferred>');
            ds.reasoning = null;
            commitWrite(ds, 'reason-clear', before);
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
                const before = quadSet(ds);
                for (let pass = 0; pass < 3; pass++) for (const r of rules) ds.store.update(r);
                const receipt = commitWrite(ds, 'reason', before);
                ds.reasoning = {
                  profile,
                  inferred: inferredCount(ds),
                  at: new Date().toISOString(),
                  commit: receipt.commit.seq,
                  stale: false,
                  commitsSince: 0,
                };
                return `Inferred ${ds.reasoning.inferred} triples (${profile})`;
              },
              3000,
            ),
          );
        }
        case 'commits': {
          if (!ds) return fail(res, 404, `No such dataset: ${name}`);
          const head = headCommit(ds);
          const num = (k) => (url.searchParams.has(k) ? Number(url.searchParams.get(k)) : null);
          if (extra) {
            const m = /^(?:commit:)?(\d+)$/.exec(extra);
            const seq = extra === 'head' ? head.seq : m ? Number(m[1]) : NaN;
            if (Number.isNaN(seq)) return fail(res, 400, `invalid commit reference '${extra}'`);
            if (seq > head.seq)
              return fail(res, 404, `no commit ${seq} in dataset ${name} (head is ${head.seq})`);
            const c = ds.commits.find((x) => x.seq === seq);
            if (!c)
              return fail(res, 410, `commit metadata before ${seq + 1} is no longer retained`);
            return send(res, 200, { dataset: name, datasetId: ds.id, commit: c });
          }
          const limit = Math.min(num('limit') ?? 50, 1000);
          if (!(limit > 0)) return fail(res, 400, 'invalid commit range: limit=0');
          const before = num('before');
          const after = num('after');
          if (before != null && after != null)
            return fail(res, 400, 'invalid commit range: use either before or after');
          const list =
            after != null
              ? ds.commits.filter((c) => c.seq > after).slice(0, limit)
              : ds.commits
                  .filter((c) => before == null || c.seq < before)
                  .reverse()
                  .slice(0, limit);
          const last = list[list.length - 1];
          const next = !last
            ? null
            : after != null
              ? last.seq < head.seq
                ? `/$/commits/${name}?after=${last.seq}&limit=${limit}`
                : null
              : last.seq > ds.firstRetained
                ? `/$/commits/${name}?before=${last.seq}&limit=${limit}`
                : null;
          return send(
            res,
            200,
            {
              dataset: name,
              datasetId: ds.id,
              head: head.seq,
              firstRetained: ds.firstRetained,
              complete: true,
              commits: list,
              next,
            },
            'application/json',
            commitHeaders(ds),
          );
        }
        case 'text': {
          if (!ds) return fail(res, 404, `No such dataset: ${name}`);
          if (req.method === 'GET')
            return send(res, 200, ds.text ? textStatusJson(ds) : { enabled: false });
          if (req.method === 'DELETE') {
            ds.text = null;
            ds.textRebuilt = null;
            res.writeHead(204, { 'Access-Control-Allow-Origin': '*' });
            return res.end();
          }
          const building = () => {
            ds.textBuilding = true;
            const t0 = Date.now();
            return startTask(
              'text-rebuild',
              ds,
              () => {
                ds.textBuilding = false;
                ds.textRebuilt = { at: new Date().toISOString(), ms: Date.now() - t0 };
                return `full-text index: ${textDocs(ds).length} documents`;
              },
              4000,
            );
          };
          if (req.method === 'PUT') {
            const body = (await readBody(req)).toString('utf8').trim();
            let cfg = {};
            try {
              cfg = body ? JSON.parse(body) : {};
            } catch (e) {
              return fail(res, 400, `invalid text configuration: ${e.message}`);
            }
            const preds = cfg.predicates ?? 'all';
            if (
              preds !== 'all' &&
              !(Array.isArray(preds) && preds.every((x) => typeof x === 'string'))
            )
              return fail(
                res,
                400,
                `invalid text configuration: predicates: expected "all" or a list of IRIs, got ${JSON.stringify(preds)}`,
              );
            ds.text = {
              predicates: preds,
              graphs: { include: 'all', exclude: [], ...(cfg.graphs ?? {}) },
              ...(cfg.maxTextBytes ? { maxTextBytes: cfg.maxTextBytes } : {}),
              ...(cfg.maxHits ? { maxHits: cfg.maxHits } : {}),
            };
            return send(res, 202, building());
          }
          if (req.method === 'POST' && extra === 'rebuild') {
            if (!ds.text) return fail(res, 400, 'full-text search is not enabled');
            if (ds.textBuilding) return fail(res, 409, 'text index rebuild already running');
            return send(res, 202, building());
          }
          break;
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
    if (op === 'upload' && req.method === 'POST') return handleUpload(req, res, ds, url);
    const p = await params(req, url);
    if (['', 'sparql', 'query'].includes(op)) {
      if (p.has('update')) return handleUpdate(req, res, ds, p);
      return handleQuery(req, res, ds, p);
    }
    if (op === 'update') return handleUpdate(req, res, ds, p);
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
