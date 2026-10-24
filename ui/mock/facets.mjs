// `POST /$/graph/{ds}/facets` (docs/specs/U11-linked-views.md §3.2) for the mock: the
// same counts as the server's, from SPARQL run through oxigraph. The queries follow
// `crates/sparkles/src/overview/facets.rs`: ranges of round width over the whole set,
// counts over the set filtered by every brush but the facet's own, and `matching` under
// every brush. A query set is run once and its values become a VALUES set, which the
// mock's small datasets allow.

const XSD = 'http://www.w3.org/2001/XMLSchema#';
const RDF_TYPE = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';
const LABELS = [
  'http://www.w3.org/2000/01/rdf-schema#label',
  'http://www.w3.org/2004/02/skos/core#prefLabel',
  'http://xmlns.com/foaf/0.1/name',
  'http://purl.org/dc/terms/title',
  'http://schema.org/name',
];
const NUMERIC = new Set(
  [
    'integer',
    'decimal',
    'double',
    'float',
    'int',
    'long',
    'short',
    'byte',
    'nonNegativeInteger',
    'positiveInteger',
    'negativeInteger',
    'nonPositiveInteger',
    'unsignedInt',
    'unsignedLong',
    'unsignedShort',
    'unsignedByte',
  ].map((l) => XSD + l),
);
const INTEGER = new Set([...NUMERIC].filter((d) => !/#(decimal|double|float)$/.test(d)));
const DATES = new Set(['date', 'dateTime', 'dateTimeStamp'].map((l) => XSD + l));

class Bad extends Error {}

function termJson(t) {
  if (t.termType === 'NamedNode') return { type: 'uri', value: t.value };
  if (t.termType === 'BlankNode') return { type: 'bnode', value: t.value };
  const o = { type: 'literal', value: t.value };
  if (t.language) o['xml:lang'] = t.language;
  else if (t.datatype && t.datatype.value !== `${XSD}string`) o.datatype = t.datatype.value;
  return o;
}

/** A term of the SPARQL JSON results format as SPARQL text. */
function termText(j) {
  if (!j || typeof j !== 'object' || typeof j.value !== 'string') throw new Bad('a term is needed');
  if (j.type === 'uri') return `<${j.value}>`;
  if (j.type !== 'literal') throw new Bad('blank nodes cannot be named in a request');
  const v = JSON.stringify(j.value);
  if (j['xml:lang']) return `${v}@${j['xml:lang']}`;
  if (j.datatype && j.datatype !== `${XSD}string`) return `${v}^^<${j.datatype}>`;
  return v;
}

const ntText = (t) => termText(termJson(t));

function iri(v, what) {
  if (typeof v !== 'string' || !/^[a-z][a-z0-9+.-]*:[^<>"{}|^`\\\s]*$/i.test(v))
    throw new Bad(`${what} must be an IRI string`);
  return v.replace(/^<|>$/g, '');
}

function niceWidth(raw, integral) {
  if (!Number.isFinite(raw) || raw <= 0) return 1;
  const mag = 10 ** Math.floor(Math.log10(raw));
  let w = [1, 2, 2.5, 5, 10].map((m) => m * mag).find((x) => x >= raw * (1 - 1e-9)) ?? 10 * mag;
  return integral ? Math.max(1, Math.ceil(w)) : w;
}

export function roundEdges(lo, hi, bins, integral) {
  if (!Number.isFinite(lo) || !Number.isFinite(hi) || hi < lo) return null;
  const span = integral ? hi - lo + 1 : hi - lo;
  let w = niceWidth(hi > lo ? span / bins : 1, integral);
  let start;
  let n;
  for (;;) {
    start = Math.floor(lo / w) * w;
    n = Math.floor((hi - start) / w) + 1;
    if (n <= bins) break;
    const wider = niceWidth(w * (1 + 1e-6), integral);
    if (wider <= w) return null;
    w = wider;
  }
  const decimals = integral ? 0 : Math.min(12, Math.max(0, -Math.floor(Math.log10(w)) + 1));
  const edges = [];
  for (let i = 0; i <= n; i++) {
    const x = Number((start + i * w).toFixed(decimals));
    if (edges[edges.length - 1] !== x) edges.push(x);
  }
  if (edges.length < 2 || edges[edges.length - 1] <= hi || edges[0] > lo) return null;
  return edges;
}

const num = (x) => String(x);

function condition(b, v) {
  if (b.values) return `${v} IN (${b.values.join(', ')})`;
  const [a, z] = b.range;
  return b.kind === 'date'
    ? `YEAR(${v}) >= ${num(a)} && YEAR(${v}) < ${num(z)}`
    : `${v} >= ${num(a)} && ${v} < ${num(z)}`;
}

function parse(body) {
  if (!body || typeof body !== 'object') throw new Bad('the request must be a JSON object');
  for (const k of Object.keys(body))
    if (!['within', 'facets', 'filters', 'candidates', 'at'].includes(k))
      throw new Bad(`unknown member '${k}': within, facets, filters, candidates or at`);
  const w = body.within;
  let within;
  if (w?.class) within = { class: iri(w.class, 'within.class') };
  else if (Array.isArray(w?.nodes)) {
    if (!w.nodes.length || w.nodes.length > 20000)
      throw new Bad('within.nodes must hold from 1 to 20000 IRIs');
    within = { nodes: w.nodes.map((n) => iri(n, 'within.nodes[]')) };
  } else if (typeof w?.query === 'string' && typeof w?.variable === 'string')
    within = { query: w.query, variable: w.variable.replace(/^[?$]/, '') };
  else throw new Bad('within must be {"class": IRI}, {"nodes": [IRI]} or {"query", "variable"}');
  const facets = (body.facets ?? []).map((f) => ({
    property: iri(f.property, 'facets[].property'),
    bins: f.bins ?? 20,
    top: f.top ?? 12,
    kind: f.kind,
    edges: f.edges,
    bars: f.bars ? f.bars.map(termText) : undefined,
  }));
  if (facets.length > 6) throw new Bad('facets must be an array of at most 6 facets');
  for (const f of facets)
    if (f.bins < 2 || f.bins > 40) throw new Bad('bins must be an integer from 2 to 40');
  const filters = (body.filters ?? []).map((f) => {
    const property = iri(f.property, 'filters[].property');
    if (Array.isArray(f.range)) {
      if (!(f.range[0] < f.range[1])) throw new Bad('filters[].range must have from below to');
      return { property, kind: f.kind === 'date' ? 'date' : 'numeric', range: f.range };
    }
    if (Array.isArray(f.values) && f.values.length)
      return { property, kind: 'values', values: f.values.map(termText) };
    throw new Bad('a filter has either range [from, to] or values [term, …]');
  });
  if (!facets.length && !body.candidates)
    throw new Bad('name at least one facet, or ask for the candidates');
  return { within, facets, filters, candidates: !!body.candidates };
}

function compute(store, req, opts) {
  const select = (q) => store.query(q, opts);
  let set;
  if (req.within.class) set = `?s <${RDF_TYPE}> <${req.within.class}> .\n`;
  else if (req.within.nodes)
    set = `VALUES ?s { ${req.within.nodes.map((n) => `<${n}>`).join(' ')} }\n`;
  else {
    let rows;
    try {
      rows = store.query(req.within.query, opts);
    } catch (e) {
      throw new Bad(`within.query: ${e.message ?? e}`);
    }
    if (!Array.isArray(rows)) throw new Bad('within.query must be a SELECT query');
    const vals = new Map();
    for (const r of rows) {
      const t = r.get(req.within.variable);
      if (t && t.termType !== 'BlankNode') vals.set(ntText(t), true);
    }
    set = vals.size ? `VALUES ?s { ${[...vals.keys()].join(' ')} }\n` : `FILTER(false)\n`;
  }
  const countOf = (filters) => {
    const r = select(`SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {\n${set}${filters}}`);
    return Number(r[0]?.get('n')?.value ?? 0);
  };
  const brush = (b, i) =>
    `FILTER EXISTS { ?s <${b.property}> ?f${i} . FILTER(${condition(b, `?f${i}`)}) }\n`;
  const size = countOf('');
  const all = req.filters.map(brush).join('');
  const matching = req.filters.length ? countOf(all) : size;
  const facets = req.facets.map((spec) => {
    const others = req.filters
      .map((b, i) => (b.property === spec.property ? '' : brush(b, i)))
      .join('');
    const p = spec.property;
    let kind = spec.kind;
    let sample = [];
    if (!kind || (!spec.edges && !spec.bars)) {
      sample = select(`SELECT ?v WHERE {\n${set}?s <${p}> ?v .\n} LIMIT 500`).map((r) =>
        r.get('v'),
      );
      if (!kind) {
        const typed = (s) =>
          sample.filter((t) => t.termType === 'Literal' && s.has(t.datatype.value)).length >=
          0.9 * sample.length;
        kind =
          sample.length && typed(NUMERIC)
            ? 'numeric'
            : sample.length && typed(DATES)
              ? 'date'
              : 'values';
      }
    }
    if (kind === 'values') {
      let bars = spec.bars;
      let terms = [];
      if (!bars) {
        terms = select(
          `SELECT ?v (COUNT(DISTINCT ?s) AS ?n) WHERE {\n${set}?s <${p}> ?v .\nFILTER(isIRI(?v) || isLiteral(?v))\n} GROUP BY ?v ORDER BY DESC(?n) ?v LIMIT ${spec.top}`,
        ).map((r) => r.get('v'));
        bars = terms.map(ntText);
      }
      const missing = Number(
        select(
          `SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {\n${set}${others}FILTER NOT EXISTS { ?s <${p}> ?any }\n}`,
        )[0]?.get('n')?.value ?? 0,
      );
      const counts = new Map();
      let unbound = 0;
      if (bars.length) {
        for (const r of select(
          `SELECT ?k (COUNT(DISTINCT ?s) AS ?n) WHERE {\n${set}${others}OPTIONAL { VALUES ?k { ${bars.join(' ')} } ?s <${p}> ?k }\n} GROUP BY ?k`,
        )) {
          const k = r.get('k');
          const n = Number(r.get('n').value);
          if (k) counts.set(ntText(k), { t: k, n });
          else unbound += n;
        }
      } else unbound = countOf(others);
      const labels = new Map();
      const iris = bars.filter((b) => b.startsWith('<'));
      if (iris.length)
        for (const r of select(
          `SELECT ?s (SAMPLE(?l) AS ?label) WHERE { VALUES ?s { ${iris.join(' ')} } ?s ${`(${LABELS.map((l) => `<${l}>`).join('|')})`} ?l . FILTER(isLiteral(?l)) } GROUP BY ?s`,
        ))
          labels.set(r.get('s').value, r.get('label').value);
      const values = bars
        .map((b, i) => {
          const c = counts.get(b);
          const t = c?.t ?? terms[i];
          if (!t) return null;
          const o = { value: termJson(t), count: c?.n ?? 0 };
          if (t.termType === 'NamedNode' && labels.has(t.value)) o.label = labels.get(t.value);
          return o;
        })
        .filter(Boolean);
      return {
        property: p,
        kind: 'values',
        values,
        other: Math.max(0, unbound - missing),
        missing,
      };
    }
    const binned = kind === 'date' ? 'YEAR(?v)' : '?v';
    const check =
      kind === 'date'
        ? `DATATYPE(?v) IN (<${XSD}date>, <${XSD}dateTime>, <${XSD}dateTimeStamp>)`
        : 'isNumeric(?v)';
    let edges = spec.edges;
    if (!edges) {
      const r = select(
        `SELECT (MIN(${binned}) AS ?lo) (MAX(${binned}) AS ?hi) WHERE {\n${set}?s <${p}> ?v .\nFILTER(${check})\n}`,
      )[0];
      const lo = Number(r?.get('lo')?.value);
      const hi = Number(r?.get('hi')?.value);
      const integral =
        kind === 'date' ||
        sample
          .filter((t) => t.termType === 'Literal' && NUMERIC.has(t.datatype.value))
          .every((t) => INTEGER.has(t.datatype.value));
      edges = r?.get('lo') ? roundEdges(lo, hi, spec.bins, integral) : [];
      if (!edges) edges = [];
    }
    if (edges.length < 2) return { property: p, kind, bins: [], missing: countOf(others) };
    const n = edges.length - 1;
    const key =
      n === 1
        ? '0'
        : edges
            .slice(1, n)
            .map((e) => `IF(?x >= ${num(e)}, 1, 0)`)
            .join(' + ');
    const counts = new Array(n).fill(0);
    let missing = 0;
    for (const r of select(
      `SELECT ?k (COUNT(DISTINCT ?s) AS ?n) WHERE {\n${set}${others}OPTIONAL { ?s <${p}> ?v . BIND(${binned} AS ?x) FILTER(${check} && ?x >= ${num(edges[0])} && ?x < ${num(edges[n])}) BIND(${key} AS ?k) }\n} GROUP BY ?k`,
    )) {
      const k = r.get('k');
      const c = Number(r.get('n').value);
      if (k && Number(k.value) >= 0 && Number(k.value) < n) counts[Number(k.value)] += c;
      else missing += c;
    }
    return {
      property: p,
      kind,
      bins: counts.map((count, i) => ({ from: edges[i], to: edges[i + 1], count })),
      missing,
    };
  });
  const answer = { size, matching, facets, truncated: false };
  if (req.candidates) {
    const rows = select(
      `SELECT ?s ?p ?o WHERE { { SELECT DISTINCT ?s WHERE {\n${set}} LIMIT 1000 } ?s ?p ?o . }`,
    );
    const subjects = new Set();
    const byP = new Map();
    for (const r of rows) {
      const s = r.get('s').value;
      const pp = r.get('p').value;
      const o = r.get('o');
      subjects.add(s);
      if (!byP.has(pp)) byP.set(pp, { subs: new Set(), vals: [] });
      byP.get(pp).subs.add(s);
      if (o.termType === 'NamedNode' || o.termType === 'Literal') byP.get(pp).vals.push(o);
    }
    const list = [];
    for (const [pp, { subs, vals }] of byP) {
      if (LABELS.includes(pp) || (pp === RDF_TYPE && req.within.class) || !vals.length) continue;
      const typed = (s) =>
        vals.filter((t) => t.termType === 'Literal' && s.has(t.datatype.value)).length >=
        0.9 * vals.length;
      const kind = typed(NUMERIC) ? 'numeric' : typed(DATES) ? 'date' : 'values';
      if (kind === 'values') {
        const distinct = new Set(vals.map(ntText)).size;
        if (distinct > 12 && distinct * 2 > vals.length) continue;
      }
      list.push({
        property: pp,
        share: Math.round((subs.size / Math.max(1, subjects.size)) * 1000) / 1000,
        kind,
      });
    }
    list.sort(
      (a, b) =>
        b.share - a.share ||
        Number(a.kind === 'values') - Number(b.kind === 'values') ||
        (a.property < b.property ? -1 : 1),
    );
    answer.candidates = list.slice(0, 40);
    answer.truncated = list.length > 40 || size > subjects.size;
  }
  return answer;
}

export async function handleFacets(req, res, seg, ctx) {
  if (seg[0] !== '$' || seg[1] !== 'graph' || seg[3] !== 'facets') return false;
  const { datasets, send, fail, readBody } = ctx;
  if (req.method !== 'POST') return (fail(res, 405, 'POST only'), true);
  const name = decodeURIComponent(seg[2] ?? '').split('@')[0];
  const ds = datasets.get(name);
  if (!ds) return (fail(res, 404, `No such dataset: ${name}`), true);
  let body;
  try {
    body = JSON.parse((await readBody(req)).toString('utf8') || '{}');
  } catch {
    return (fail(res, 400, 'invalid JSON'), true);
  }
  try {
    const parsed = parse(body);
    const answer = compute(ds.store, parsed, { use_default_graph_as_union: true });
    send(res, 200, { commit: ds.commits?.length ?? 1, ...answer });
  } catch (e) {
    if (e instanceof Bad) fail(res, 400, e.message);
    else fail(res, 500, String(e?.message ?? e));
  }
  return true;
}
