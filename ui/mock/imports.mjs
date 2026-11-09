// Import resolution in the mock (ROADMAP item 624, docs/specs/U12-ontology-workbench/SPEC.md
// "Import resolution"): the dataset's import map, the summary's closure, the XML catalog upload
// and the fetch into a graph. The map is kept per dataset name, so every branch shares it. A
// fetch reaches only `http://imports.mock/…`, whose documents are canned; every other location
// is refused as the server's outbound policy would refuse it.
import ox from 'oxigraph';
import { createHash } from 'node:crypto';

const OWL_IMPORTS = 'http://www.w3.org/2002/07/owl#imports';
const MAX_ENTRIES = 4096;

/** The canned documents of the mock's only reachable host. */
export const FETCHABLE = {
  'http://imports.mock/units.ttl':
    '<http://imports.mock/units> a <http://www.w3.org/2002/07/owl#Ontology> .\n' +
    '<http://imports.mock/units#Unit> a <http://www.w3.org/2002/07/owl#Class> .\n',
};

/** dataset name → { entries: Map(iri → entry), seq } */
const maps = new Map();

function mapOf(ds) {
  let m = maps.get(ds.name);
  if (!m) maps.set(ds.name, (m = { entries: new Map(), seq: 0 }));
  return m;
}

const revision = (m) => `"map-${m.seq}"`;

function body(ds) {
  const m = mapOf(ds);
  return {
    dataset: ds.name,
    revision: revision(m),
    admin: true,
    maxEntries: MAX_ENTRIES,
    entries: [...m.entries]
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([iri, e]) => ({ iri, ...e })),
  };
}

const graphTerm = (g) => (!g || g === 'default' ? ox.defaultGraph() : ox.namedNode(g));
const holds = (ds, g) => ds.store.match(null, null, null, graphTerm(g)).length > 0;
const importsIn = (ds, g) =>
  ds.store
    .match(null, ox.namedNode(OWL_IMPORTS), null, graphTerm(g))
    .filter((q) => q.object.termType === 'NamedNode')
    .map((q) => q.object.value);

/** The closure of `root`: one edge per `owl:imports` triple, cycles marked. */
export function closureOf(ds, root) {
  const m = mapOf(ds);
  const edges = [];
  const seen = new Map();
  const graphs = [root];
  for (let i = 0; i < graphs.length; i++) {
    const from = graphs[i];
    for (const iri of importsIn(ds, from)) {
      if (!seen.has(iri)) {
        const entry = m.entries.get(iri);
        const r =
          entry && holds(ds, entry.graph)
            ? { graphs: [entry.graph], via: 'map' }
            : holds(ds, iri)
              ? { graphs: [iri], via: 'graph' }
              : { graphs: [], via: null };
        seen.set(iri, r);
        for (const g of r.graphs) if (!graphs.includes(g)) graphs.push(g);
      }
      edges.push({ from, iri, ...seen.get(iri), cycle: false });
    }
  }
  const next = new Map();
  for (const e of edges)
    for (const g of e.graphs) (next.get(e.from) ?? next.set(e.from, new Set()).get(e.from)).add(g);
  const reaches = (start, goal) => {
    const stack = [start];
    const done = new Set();
    while (stack.length) {
      const n = stack.pop();
      if (n === goal) return true;
      if (done.has(n)) continue;
      done.add(n);
      stack.push(...(next.get(n) ?? []));
    }
    return false;
  };
  for (const e of edges) e.cycle = e.graphs.some((g) => reaches(g, e.from));
  return { edges, graphs };
}

const sha = (text) => `sha256:${createHash('sha256').update(text).digest('hex')}`;

/** Replace graph `g` of `ds` with the Turtle `text`. */
function replaceGraph(ds, g, text) {
  const term = ox.namedNode(g);
  for (const q of ds.store.match(null, null, null, term)) ds.store.delete(q);
  ds.store.load(text, { format: 'text/turtle', to_graph_name: term });
}

/** The `uri` entries of an XML catalog, and those it skips. */
function parseCatalog(xml) {
  if (/<!DOCTYPE/i.test(xml))
    throw new Error('the catalog has a document type declaration, which is refused');
  if (!/<catalog[\s>]/.test(xml)) throw new Error('the catalog has no <catalog> element');
  const entries = [];
  const skipped = [];
  for (const [, tag, attrs] of xml.matchAll(/<([A-Za-z]+)\b([^>]*)\/?>/g)) {
    const a = Object.fromEntries(
      [...attrs.matchAll(/([\w:]+)="([^"]*)"/g)].map((x) => [x[1], x[2]]),
    );
    if (tag === 'catalog' || tag === 'group') continue;
    if (tag === 'uri' && a.name && a.uri) entries.push({ name: a.name, uri: a.uri });
    else
      skipped.push({
        entry: `${tag} ${a.name ?? a.uriStartString ?? a.catalog ?? ''}`.trim(),
        reason: `<${tag}> entries are not supported`,
      });
  }
  return { entries, skipped };
}

const key = (path) =>
  path
    .split('/')
    .filter((s) => s && s !== '.')
    .join('/');

export async function handleImports(req, res, url, seg, ctx) {
  if (seg[0] !== '$' || seg[1] !== 'ontology' || seg[3] !== 'imports') return false;
  const { datasets, send, fail, readBody, quadSet, commitWrite } = ctx;
  const ds = datasets.get(seg[2]);
  if (!ds) return (fail(res, 404, `no such dataset: /${seg[2]}`), true);
  const action = seg[4] ?? '';
  const m = mapOf(ds);
  if (req.method === 'GET' && action === '') {
    send(res, 200, body(ds), 'application/json', { ETag: revision(m) });
    return true;
  }
  if (req.method === 'PATCH' && action === '') {
    const ifMatch = req.headers['if-match'];
    if (ifMatch && ifMatch !== revision(m))
      return (
        fail(res, 412, 'If-Match: the import map has changed', { code: 'precondition-failed' }),
        true
      );
    const b = JSON.parse(String(await readBody(req)) || '{}');
    for (const iri of b.remove ?? []) m.entries.delete(iri);
    for (const [iri, graph] of Object.entries(b.set ?? {}))
      m.entries.set(iri, { graph, source: 'manual' });
    m.seq++;
    send(res, 200, body(ds), 'application/json', { ETag: revision(m) });
    return true;
  }
  if (req.method !== 'POST' || !['catalog', 'fetch'].includes(action)) return false;
  if (ds.protected)
    return (
      fail(res, 403, `branch ${ds.branchName} is protected`, { code: 'branch-protected' }), true
    );
  const before = quadSet(ds);
  const receipt = () =>
    commitWrite(ds, 'update', before, { message: req.headers['sparkles-commit-message'] });
  if (action === 'catalog') {
    const raw = await readBody(req);
    const form = await new Request('http://mock/', {
      method: 'POST',
      headers: { 'content-type': req.headers['content-type'] ?? '' },
      body: raw,
    }).formData();
    const catalog = form.get('catalog');
    if (!catalog) return (fail(res, 400, 'the body has no catalog part'), true);
    let parsed;
    try {
      parsed = parseCatalog(await catalog.text());
    } catch (e) {
      return (fail(res, 400, e.message), true);
    }
    const files = new Map();
    for (const [k, v] of form) if (k !== 'catalog' && typeof v !== 'string') files.set(key(k), v);
    const loaded = [];
    const entries = [];
    const skipped = [...parsed.skipped];
    for (const e of parsed.entries) {
      if (/^[a-z][a-z0-9+.-]+:/i.test(e.uri)) {
        m.entries.set(e.name, { graph: e.uri, source: 'catalog' });
        entries.push({ iri: e.name, graph: e.uri, source: 'catalog' });
        continue;
      }
      const f = files.get(key(e.uri));
      if (!f) {
        skipped.push({
          entry: `uri ${e.name}`,
          reason: `the file ${key(e.uri)} is not in the body`,
        });
        continue;
      }
      const text = await f.text();
      try {
        replaceGraph(ds, e.name, text);
      } catch (err) {
        return (fail(res, 400, `${key(e.uri)}: ${err.message}`), true);
      }
      const entry = {
        graph: e.name,
        source: 'catalog',
        file: key(e.uri),
        digest: sha(text),
        branch: ds.branchName,
      };
      m.entries.set(e.name, entry);
      entries.push({ iri: e.name, ...entry });
      loaded.push({
        graph: e.name,
        file: key(e.uri),
        triples: ds.store.match(null, null, null, ox.namedNode(e.name)).length,
      });
    }
    if (!entries.length)
      return (
        fail(res, 400, `the catalog has no entry to load or map (${skipped.length} skipped)`), true
      );
    m.seq++;
    const r = receipt();
    send(res, 200, { ...r, loaded, entries, skipped, changed: r.committed, revision: revision(m) });
    return true;
  }
  const b = JSON.parse(String(await readBody(req)) || '{}');
  const old = m.entries.get(b.iri);
  const location = b.location ?? old?.location ?? b.iri;
  const graph = b.graph ?? old?.graph ?? b.iri;
  if (!/^https?:\/\//.test(location))
    return (fail(res, 400, `the location <${location}> is not an http or https URL`), true);
  const text = FETCHABLE[location];
  if (text == null)
    return (
      fail(res, 403, `GET <${location}>: the server's outbound policy refuses this destination`),
      true
    );
  replaceGraph(ds, graph, text);
  const r = receipt();
  const entry = {
    graph,
    source: 'fetch',
    location,
    branch: ds.branchName,
    commit: r.commit.seq,
    etag: '"mock-1"',
    digest: sha(text),
    fetched: new Date().toISOString(),
  };
  m.entries.set(b.iri, entry);
  m.seq++;
  send(res, 200, {
    ...r,
    entry: { iri: b.iri, ...entry },
    triples: ds.store.match(null, null, null, ox.namedNode(graph)).length,
    changed: r.committed,
    revision: revision(m),
  });
  return true;
}
