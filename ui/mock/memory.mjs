// C18 Phase 1 in the mock: `/{ds}/check`, `/{ds}/sparql/diagnose`, `/{ds}/recall`,
// `/$/memory/{ds}` and the suggested examples of `/$/queries/{ds}/suggestions`, plus the
// `org` dataset with agent memory (C17 §3.2 reifiers) to browse.
//
// The mock runs open, as everyone's admin. A `mock-principal` cookie names a principal
// instead: `admin` stays an admin, any other name reads every dataset and nothing more.

import { randomUUID } from 'node:crypto';
import ox from 'oxigraph';

const RDF = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#';
const RDFS = 'http://www.w3.org/2000/01/rdf-schema#';
const XSD = 'http://www.w3.org/2001/XMLSchema#';
const PROV = 'http://www.w3.org/ns/prov#';
const SPK = 'urn:x-sparkles:';
const FOAF = 'http://xmlns.com/foaf/0.1/';
const INFERRED = 'urn:x-sparkles:inferred';
const nn = (v) => ox.namedNode(v);

// --- the org dataset ---------------------------------------------------------------

export const ORG_PREFIXES = {
  rdf: RDF,
  rdfs: RDFS,
  owl: 'http://www.w3.org/2002/07/owl#',
  xsd: XSD,
  foaf: FOAF,
  prov: PROV,
  spk: SPK,
  ex: 'http://example.org/ontology#',
  res: 'http://example.org/resource/',
};

const header = Object.entries(ORG_PREFIXES)
  .map(([p, iri]) => `PREFIX ${p}: <${iri}>`)
  .join('\n');

/**
 * A small organisation: teams and people in the default graph, an HR graph without
 * reifiers, and two graphs of stand-up notes that agents wrote, with provenance. Ana
 * moved from the platform team to the payments team on 2026-10-08, which superseded the
 * note of 2026-10-01.
 */
export const orgTrig = `${header}
ex:Team a owl:Class ; rdfs:label "Team"@en .
ex:Person a owl:Class ; rdfs:label "Person"@en .
ex:memberOf a owl:ObjectProperty ; rdfs:label "member of"@en .
ex:startDate a owl:DatatypeProperty ; rdfs:label "start date"@en .
ex:worksOn a owl:ObjectProperty ; rdfs:label "works on"@en .
res:payments a ex:Team ; rdfs:label "Payments team"@en .
res:platform a ex:Team ; rdfs:label "Platform team"@en .
res:checkout a ex:Team ; rdfs:label "Checkout team"@en .
res:proj-17 rdfs:label "Checkout redesign"@en .
res:kai a ex:Person ; foaf:name "Kai Ito"@en ; ex:memberOf res:payments .
res:lea a ex:Person ; foaf:name "Lea Brandt"@en ; ex:memberOf res:checkout .
res:ana a ex:Person ; foaf:name "Ana Lima"@en .

GRAPH <https://example.org/hr> {
  res:ana foaf:mbox "ana@example.org" ; ex:startDate "2025-11-14"^^xsd:date .
  res:kai ex:startDate "2026-03-02"^^xsd:date .
}

GRAPH <https://example.org/notes/2026-10-01> {
  <urn:uuid:r1> rdf:reifies <<( res:ana ex:memberOf res:platform )>> ;
    prov:wasGeneratedBy <urn:uuid:a0> ;
    prov:wasDerivedFrom <https://example.org/notes/2026-10-01> ;
    prov:generatedAtTime "2026-10-01T10:02:11Z"^^xsd:dateTime ;
    spk:quote "Ana is on the platform team." ;
    prov:wasInvalidatedBy <urn:uuid:a1> ;
    prov:invalidatedAtTime "2026-10-08T09:14:03Z"^^xsd:dateTime .
  <urn:uuid:a0> a prov:Activity ;
    prov:wasAssociatedWith <urn:x-sparkles:principal:agent-9> ;
    prov:startedAtTime "2026-10-01T10:02:11Z"^^xsd:dateTime ;
    rdfs:label "Stand-up notes of 2026-10-01" .
}

GRAPH <https://example.org/notes/2026-10-08> {
  res:ana ex:memberOf res:payments ~ <urn:uuid:r2> {|
    prov:wasGeneratedBy <urn:uuid:a1> ;
    prov:wasDerivedFrom <https://example.org/notes/2026-10-08> ;
    prov:generatedAtTime "2026-10-08T09:14:03Z"^^xsd:dateTime ;
    prov:wasRevisionOf <urn:uuid:r1> ;
    spk:confidence 0.9 ;
    spk:quote "Ana moved to the payments team this week." |} .
  res:ana ex:worksOn res:proj-17 ~ <urn:uuid:r3> {|
    prov:wasGeneratedBy <urn:uuid:a1> ;
    prov:wasDerivedFrom <https://example.org/notes/2026-10-08> ;
    prov:generatedAtTime "2026-10-08T09:14:03Z"^^xsd:dateTime ;
    spk:quote "She picks up the checkout redesign." |} .
  res:ana ex:startDate "2025-11-17"^^xsd:date ~ <urn:uuid:r4> {|
    prov:wasGeneratedBy <urn:uuid:a1> ;
    prov:wasDerivedFrom <https://example.org/notes/2026-10-08> ;
    prov:generatedAtTime "2026-10-08T09:14:03Z"^^xsd:dateTime ;
    spk:confidence 0.6 ;
    spk:quote "Ana started on 17 November." |} .
  <urn:uuid:a1> a prov:Activity ;
    prov:wasAssociatedWith <urn:x-sparkles:principal:agent-7> ;
    prov:startedAtTime "2026-10-08T09:14:03Z"^^xsd:dateTime ;
    rdfs:label "Stand-up notes of 2026-10-08" .
}
`;

/** The memory settings of the org dataset: the notes graphs hold agent memory. */
export const ORG_MEMORY = {
  agentGraphs: ['https://example.org/notes/*'],
  agents: { 'agent-7': { conversationFacts: 'immediate' } },
};

/** Predicates with at most one value, whose disagreeing values are conflicts. */
const SINGLE = new Set(['http://example.org/ontology#startDate']);

// --- principals ----------------------------------------------------------------------

/** The principal a request names with the `mock-principal` cookie, or null (open). */
export function mockPrincipal(req) {
  const m = /(?:^|;\s*)mock-principal=([^;]+)/.exec(String(req.headers.cookie ?? ''));
  return m ? decodeURIComponent(m[1]) : null;
}

export const isAdmin = (req) => {
  const p = mockPrincipal(req);
  return p == null || p === 'admin';
};

/** `/$/whoami` for a cookie principal, or null to keep the open answer. */
export function whoamiFor(req, datasetNames) {
  const name = mockPrincipal(req);
  if (name == null) return null;
  const admin = name === 'admin';
  return {
    authEnabled: true,
    principal: { kind: 'user', name, displayName: name },
    method: 'session',
    server: admin ? ['server-admin'] : [],
    datasets: Object.fromEntries(datasetNames.map((n) => [n, admin ? 'admin' : 'read'])),
    canMintTokens: false,
    logout: false,
  };
}

// --- compact terms -------------------------------------------------------------------

function compactor(prefixes) {
  const used = {};
  const entries = Object.entries(prefixes).sort((a, b) => b[1].length - a[1].length);
  const iri = (v) => {
    for (const [p, ns] of entries) {
      if (v.startsWith(ns) && /^[A-Za-z0-9_][\w-]*$/.test(v.slice(ns.length))) {
        used[p] = ns;
        return `${p}:${v.slice(ns.length)}`;
      }
    }
    return `<${v}>`;
  };
  const term = (t) => {
    switch (t.termType) {
      case 'NamedNode':
        return iri(t.value);
      case 'BlankNode':
        return `_:${t.value}`;
      case 'Literal': {
        const v = JSON.stringify(t.value);
        if (t.language) return `${v}@${t.language}`;
        if (!t.datatype || t.datatype.value === `${XSD}string`) return v;
        return `${v}^^${iri(t.datatype.value)}`;
      }
      case 'Quad':
        return `<<( ${term(t.subject)} ${term(t.predicate)} ${term(t.object)} )>>`;
      default:
        return String(t.value);
    }
  };
  return { iri, term, used };
}

// --- queries -------------------------------------------------------------------------

const UNION = { use_default_graph_as_union: true };

/** The text without strings, IRIs and comments, to look for keywords. */
const bare = (q) =>
  q
    .replace(/#[^\n]*/g, ' ')
    .replace(/"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'/g, '""')
    .replace(/<[^>\s]*>/g, '<>');

const isUpdate = (q) =>
  /\b(INSERT|DELETE)\b\s*(DATA\b|WHERE\b|\{)|\b(LOAD|CLEAR|DROP|CREATE|ADD|MOVE|COPY)\s+(SILENT\s+)?(GRAPH|DEFAULT|NAMED|ALL|<)|\bWITH\s+</i.test(
    bare(q),
  );

const NOT_A_QUERY = {
  code: 'not-a-query',
  severity: 'error',
  message: 'The text is an update. Only queries can be checked, shared or suggested.',
};

/** The prefixes a query declares. */
function declared(q) {
  const out = {};
  for (const m of q.matchAll(/\bPREFIX\s+([A-Za-z][\w.-]*)?:\s*<([^>\s]*)>/gi))
    out[m[1] ?? ''] = m[2];
  return out;
}

/** The constant IRIs of a query, in order of first use. */
function queryIris(q, prefixes) {
  const known = { ...prefixes, ...declared(q) };
  const body = q
    .replace(/#[^\n]*/g, ' ')
    .replace(/"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'/g, '""')
    .replace(/\bPREFIX\s+[\w.-]*:\s*<[^>]*>/gi, ' ')
    .replace(/\bBASE\s+<[^>]*>/gi, ' ');
  const out = [];
  const seen = new Set();
  const add = (iri) => {
    if (!seen.has(iri)) {
      seen.add(iri);
      out.push(iri);
    }
  };
  for (const m of body.matchAll(
    /<([^>\s]*)>|(?<![\w?$:.-])([A-Za-z][\w.-]*)?:((?:[\w-]|\.(?=[\w-]))*)/g,
  )) {
    if (m[1] != null) add(m[1]);
    else if (known[m[2] ?? ''] != null) add(known[m[2] ?? ''] + m[3]);
  }
  return out;
}

function labelOf(store, iri) {
  for (const p of [`${RDFS}label`, `${FOAF}name`]) {
    const ls = store.match(nn(iri), nn(p), null, null).map((q) => q.object);
    const en = ls.find((l) => l.language === 'en') ?? ls[0];
    if (en) return en.value;
  }
  return undefined;
}

const notInferred = (q) => q.graph.termType !== 'NamedNode' || q.graph.value !== INFERRED;

/** `POST /{ds}/check`. */
function check(ds, body) {
  const query = String(body.query ?? '');
  const prefixes = { ...ds.prefixes, ...declared(query) };
  const c = compactor(prefixes);
  const out = { dataset: ds.name, commit: ds.commits.at(-1)?.seq ?? 0, ok: true, issues: [] };
  if (isUpdate(query)) return { ...out, ok: false, issues: [NOT_A_QUERY], prefixes: {} };
  let result;
  try {
    result = ds.store.query(query, UNION);
    out.estimatedRows = typeof result === 'boolean' ? 1 : result.length;
  } catch (e) {
    const msg = String(e?.message ?? e);
    const m = /(\d+):(\d+)/.exec(msg);
    out.ok = false;
    out.issues.push({
      code: 'syntax',
      severity: 'error',
      message: msg,
      ...(m ? { line: Number(m[1]), column: Number(m[2]) } : {}),
    });
  }
  const terms = [];
  for (const iri of queryIris(query, prefixes)) {
    const s = ds.store;
    const asP = s.match(null, nn(iri), null, null).filter(notInferred).length;
    const instances = s.match(null, nn(`${RDF}type`), nn(iri), null).length;
    const asS = s.match(nn(iri), null, null, null).length;
    const asO = s.match(null, null, nn(iri), null).length;
    const occurs = asP + asS + asO > 0;
    const types = s.match(nn(iri), nn(`${RDF}type`), null, null).map((q) => q.object.value);
    const isClass =
      instances > 0 || types.some((t) => /#Class$/.test(t)) || types.includes(`${RDFS}Datatype`);
    const t = { term: c.iri(iri), iri, occurs };
    const label = labelOf(s, iri);
    if (label) t.label = label;
    if (asP > 0 || types.some((x) => /Property$/.test(x))) {
      Object.assign(t, { kind: 'property', count: asP });
    } else if (isClass) {
      Object.assign(t, { kind: 'class', count: instances });
    } else {
      Object.assign(t, { kind: 'entity' });
      if (types.length) t.types = types.map((x) => c.iri(x));
    }
    terms.push(t);
    if (!occurs)
      out.issues.push({
        code: 'unknown-term',
        severity: 'warning',
        message: `${t.term} does not occur in the data you can read.`,
        term: t.term,
      });
  }
  if (body.terms) out.terms = terms;
  return { ...out, prefixes: c.used };
}

/** The triple patterns of a query's WHERE clause, as written (a rough reading). */
function patterns(q) {
  const body = bare(q).replace(/\bPREFIX\s+[\w.-]*:\s*<[^>]*>/gi, ' ');
  const start = body.indexOf('{');
  if (start < 0) return [];
  // the original text, so that strings keep their content
  const src = q.replace(/#[^\n]*/g, ' ');
  const open = src.indexOf('{', src.search(/\bWHERE\b|\bASK\b|\bSELECT\b/i));
  const inner = src.slice(open + 1, src.lastIndexOf('}'));
  const cleaned = inner
    .replace(/\b(FILTER|BIND)\s*\((?:[^()]|\([^()]*\))*\)/gi, ' . ')
    .replace(/\bVALUES\b[^}]*\}/gi, ' . ')
    .replace(/\b(OPTIONAL|MINUS|UNION|GRAPH\s+\S+|SERVICE\s+\S+)\b/gi, ' . ')
    .replace(/[{}]/g, ' . ');
  const out = [];
  for (const stmt of cleaned.split(/\s\.(?=\s|$)/)) {
    const parts = stmt.trim().split(/\s*;\s*/);
    const toks0 = parts[0]?.trim().match(/"(?:[^"\\]|\\.)*"\S*|\S+/g) ?? [];
    const subj = toks0[0];
    if (!subj) continue;
    for (let i = 0; i < parts.length; i++) {
      const toks =
        i === 0 ? toks0.slice(1) : (parts[i].trim().match(/"(?:[^"\\]|\\.)*"\S*|\S+/g) ?? []);
      const [p, ...os] = toks;
      if (!p) continue;
      for (const o of os.join(' ').split(/\s*,\s*/)) if (o) out.push([subj, p, o.trim()]);
    }
  }
  return out.slice(0, 20);
}

const isVar = (t) => /^[?$]/.test(t);

/** `POST /{ds}/sparql/diagnose`. */
function diagnose(ds, body) {
  const query = String(body.query ?? '');
  const out = { dataset: ds.name, commit: ds.commits.at(-1)?.seq ?? 0 };
  const decl = Object.entries({ ...ds.prefixes, ...declared(query) })
    .map(([p, ns]) => `PREFIX ${p}: <${ns}>`)
    .join('\n');
  const c = compactor({ ...ds.prefixes, ...declared(query) });
  let result;
  try {
    result = ds.store.query(query, UNION);
  } catch (e) {
    return { error: [400, { error: 'Parse error', detail: String(e?.message ?? e) }] };
  }
  const empty = typeof result === 'boolean' ? !result : result.length === 0;
  if (!empty)
    return {
      ...out,
      empty: false,
      steps: [],
      complete: true,
      message: 'The query has solutions.',
      prefixes: {},
    };
  const steps = [];
  let first;
  for (const [s, p, o] of patterns(query)) {
    const text = `${s} ${p} ${o}`;
    let solutions = null;
    try {
      solutions = ds.store.query(`${decl}\nASK { ${text} }`, UNION);
    } catch {
      solutions = null;
    }
    steps.push({ kind: 'pattern', text, solutions });
    if (solutions === false && !first) {
      const constants = [s, p, o]
        .filter((t) => !isVar(t) && t !== 'a')
        .map((t) => {
          let occurs = false;
          try {
            occurs = t.startsWith('"')
              ? ds.store.query(`${decl}\nASK { ?s ?p ${t} }`, UNION)
              : ds.store.query(
                  `${decl}\nASK { { ${t} ?p ?o } UNION { ?s ${t} ?o } UNION { ?s ?p ${t} } }`,
                  UNION,
                );
          } catch {
            occurs = false;
          }
          return { term: t, occurs };
        });
      const issues = [];
      for (const k of constants) {
        const m = /^"((?:[^"\\]|\\.)*)"$/.exec(k.term);
        if (!m || k.occurs) continue;
        const tagged = ds.store.query(
          `ASK { ?s ?p ?o FILTER(isLiteral(?o) && STR(?o) = ${k.term} && LANG(?o) != "") }`,
          UNION,
        );
        if (tagged)
          issues.push({
            code: 'language-tag',
            severity: 'warning',
            message: `The data has ${k.term} only with a language tag, such as ${k.term}@en.`,
            term: k.term,
          });
      }
      first = { kind: 'pattern', text, constants, issues };
    }
  }
  const missing = first?.constants.filter((k) => !k.occurs).map((k) => k.term) ?? [];
  const message = first
    ? `The pattern ${first.text} has no solutions${
        missing.length ? `, and ${missing.join(', ')} does not occur in the data you can read` : ''
      }.`
    : 'No single pattern is empty, so the patterns have no solutions together.';
  void c;
  return { ...out, empty: true, first, steps, complete: true, message, prefixes: {} };
}

// --- recall --------------------------------------------------------------------------

const matchesPattern = (pattern, g) =>
  pattern.endsWith('*') ? g.startsWith(pattern.slice(0, -1)) : g === pattern;

/** `POST /{ds}/recall`. */
function recall(ds, body) {
  const store = ds.store;
  const c = compactor(ds.prefixes);
  const memory = ds.memory ?? { agentGraphs: [], agents: {} };
  const agentGraph = (g) => memory.agentGraphs.some((p) => matchesPattern(p, g));
  const graphs = Array.isArray(body.graphs) && body.graphs.length ? new Set(body.graphs) : null;
  const graphName = (q) => (q.graph.termType === 'DefaultGraph' ? 'default' : q.graph.value);
  const readable = (q) => notInferred(q) && (!graphs || graphs.has(graphName(q)));
  const maxTriples = Math.min(Number(body.maxTriples ?? 150), 1000);
  const statuses = new Set(body.statuses ?? ['reviewed', 'unreviewed']);

  // the seeds: the given IRIs, then labels that contain the words of the text
  const seeds = [...(body.seeds ?? [])];
  if (body.query) {
    const words = String(body.query)
      .toLowerCase()
      .split(/\W+/)
      .filter((w) => w.length > 2);
    const hits = new Map();
    for (const q of store.match(null, null, null, null)) {
      if (q.object.termType !== 'Literal' || q.subject.termType !== 'NamedNode') continue;
      if (![`${RDFS}label`, `${FOAF}name`, `${SPK}quote`].includes(q.predicate.value)) continue;
      const text = q.object.value.toLowerCase();
      const score = words.filter((w) => text.includes(w)).length;
      if (!score) continue;
      // a hit on a reifier's quote counts for the reified triple's subject
      let subject = q.subject.value;
      const reified = store.match(q.subject, nn(`${RDF}reifies`), null, null)[0];
      if (reified?.object.termType === 'Quad') subject = reified.object.subject.value;
      hits.set(subject, Math.max(hits.get(subject) ?? 0, score));
    }
    for (const [iri] of [...hits]
      .sort((a, b) => b[1] - a[1])
      .slice(0, Number(body.seedLimit ?? 10)))
      if (!seeds.includes(iri)) seeds.push(iri);
  }
  if (!seeds.length)
    return { error: [400, { error: 'recall needs `query` or `seeds`', code: 'bad-request' }] };
  const hops = Math.min(Number(body.hops ?? 1), 2);

  const citations = [];
  const citationKey = new Map();
  const citationFor = (g, reifier) => {
    const prov = {};
    if (reifier) {
      const one = (p) => store.match(reifier, nn(p), null, null)[0]?.object;
      const act = one(`${PROV}wasGeneratedBy`);
      const by = act
        ? store.match(act, nn(`${PROV}wasAssociatedWith`), null, null)[0]?.object
        : null;
      const src = one(`${PROV}wasDerivedFrom`);
      const at = one(`${PROV}generatedAtTime`);
      const conf = one(`${SPK}confidence`);
      const quote = one(`${SPK}quote`);
      prov.reifier = c.term(reifier);
      if (src) prov.source = c.term(src);
      if (at) prov.at = at.value;
      if (by) prov.by = by.value;
      if (conf) prov.confidence = Number(conf.value);
      if (quote) prov.quote = quote.value;
    }
    const key = `${g} ${JSON.stringify({ ...prov, reifier: undefined })}`;
    let cit = citationKey.get(key);
    if (!cit) {
      cit = { id: citations.length + 1, graph: g, ...prov };
      if (memory.agentGraphs.length) cit.status = agentGraph(g) ? 'unreviewed' : 'reviewed';
      citationKey.set(key, cit);
      citations.push(cit);
    }
    return cit;
  };

  const entities = [];
  const done = new Set();
  let frontier = seeds.map((iri, i) => ({ iri, seed: i + 1 }));
  let total = 0;
  let truncated = false;
  for (let hop = 0; hop <= hops && frontier.length; hop++) {
    const next = [];
    for (const { iri, seed } of frontier) {
      if (done.has(iri)) continue;
      done.add(iri);
      const subject = nn(iri);
      const quads = store.match(subject, null, null, null).filter(readable);
      const types = quads
        .filter((q) => q.predicate.value === `${RDF}type`)
        .map((q) => c.term(q.object));
      const e = { iri: c.iri(iri), types: [...new Set(types)], hop, facts: [] };
      const label = labelOf(store, iri);
      if (label) e.label = label;
      if (seed) e.seed = seed;
      for (const q of quads) {
        if (q.predicate.value === `${RDF}type` || q.predicate.value === `${RDF}reifies`) continue;
        if (total >= maxTriples) {
          truncated = true;
          break;
        }
        const g = graphName(q);
        const triple = ox.triple(q.subject, q.predicate, q.object);
        const reifiers = store
          .match(null, nn(`${RDF}reifies`), triple, q.graph)
          .map((r) => r.subject);
        let status;
        if (memory.agentGraphs.length) {
          const elsewhere = store
            .match(q.subject, q.predicate, q.object, null)
            .some((x) => readable(x) && !agentGraph(graphName(x)));
          status = elsewhere ? 'reviewed' : 'unreviewed';
          if (!statuses.has(status)) continue;
        }
        for (const r of reifiers.length ? reifiers : [null]) {
          const cit = citationFor(g, r);
          e.facts.push({
            s: c.term(q.subject),
            p: c.term(q.predicate),
            o: c.term(q.object),
            citation: cit.id,
            ...(status ? { status } : {}),
          });
          total++;
        }
        if (q.object.termType === 'NamedNode') next.push({ iri: q.object.value });
      }
      entities.push(e);
    }
    frontier = next;
  }

  const subjects = new Set(entities.map((e) => e.iri));
  const out = {
    dataset: ds.name,
    commit: ds.commits.at(-1)?.seq ?? 0,
    entities,
    citations,
    conflicts: [],
    truncated,
  };
  // conflicts: a single-valued predicate with values from different graphs
  for (const e of entities) {
    const by = new Map();
    for (const f of e.facts) {
      const p = f.p;
      const full = ox.namedNode(
        p.includes(':') && !p.startsWith('<')
          ? (ds.prefixes[p.split(':')[0]] ?? '') + p.split(':')[1]
          : p.slice(1, -1),
      );
      if (!SINGLE.has(full.value)) continue;
      const list = by.get(p) ?? [];
      if (!list.some((v) => v.o === f.o)) list.push({ o: f.o, citation: f.citation });
      by.set(p, list);
    }
    for (const [p, values] of by)
      if (values.length > 1) out.conflicts.push({ s: e.iri, p, values });
  }
  if (body.includeSuperseded) {
    out.superseded = [];
    for (const r of store.match(null, nn(`${RDF}reifies`), null, null)) {
      if (!readable(r) || r.object.termType !== 'Quad') continue;
      const t = r.object;
      if (!subjects.has(c.term(t.subject))) continue;
      const asserted = store.match(t.subject, t.predicate, t.object, r.graph).length > 0;
      const inv = store.match(r.subject, nn(`${PROV}invalidatedAtTime`), null, r.graph)[0];
      if (asserted && !inv) continue;
      const at = store.match(r.subject, nn(`${PROV}generatedAtTime`), null, r.graph)[0];
      const rev = store.match(null, nn(`${PROV}wasRevisionOf`), r.subject, null)[0];
      out.superseded.push({
        s: c.term(t.subject),
        p: c.term(t.predicate),
        o: c.term(t.object),
        graph: graphName(r),
        reifier: c.term(r.subject),
        ...(at ? { at: at.object.value } : {}),
        ...(inv ? { invalidatedAt: inv.object.value } : {}),
        ...(rev ? { replacedBy: c.term(rev.subject) } : {}),
      });
    }
  }
  return { ...out, prefixes: c.used };
}

// --- the handler -----------------------------------------------------------------------

/** Suggested examples per dataset name, newest first. */
const suggestions = {};

async function jsonBody(req, readBody) {
  try {
    return JSON.parse((await readBody(req)).toString('utf8') || '{}');
  } catch {
    return null;
  }
}

/**
 * Handles the C18 endpoints; answers whether it did. Runs before the generic routes, so
 * it sees `/$/queries/{ds}/suggestions` before the stored queries do.
 */
export async function handleAsk(req, res, url, seg, { datasets, send, fail, readBody }) {
  if (seg[0] === '$') {
    const [, what, name, extra] = seg;
    if (what === 'memory' && name) {
      const ds = datasets.get(name);
      if (!ds) return (fail(res, 404, `No such dataset: ${name}`), true);
      if (req.method === 'GET')
        return (send(res, 200, ds.memory ?? { agentGraphs: [], agents: {} }), true);
      if (req.method === 'PUT') {
        if (!isAdmin(req)) return (fail(res, 403, 'admin access needed'), true);
        const body = await jsonBody(req, readBody);
        if (!body || !Array.isArray(body.agentGraphs))
          return (fail(res, 400, 'expected `agentGraphs`', { code: 'bad-request' }), true);
        ds.memory = { agentGraphs: body.agentGraphs, agents: body.agents ?? {}, ...body };
        return (send(res, 200, ds.memory), true);
      }
      return (fail(res, 405, 'method not allowed'), true);
    }
    if (what === 'queries' && name && extra === 'suggestions') {
      const ds = datasets.get(name);
      if (!ds) return (fail(res, 404, `No such dataset: ${name}`), true);
      const list = (suggestions[name] ??= []);
      if (req.method === 'POST') {
        const body = await jsonBody(req, readBody);
        if (!body || typeof body.question !== 'string' || typeof body.query !== 'string')
          return (fail(res, 400, 'expected `question` and `query`', { code: 'bad-request' }), true);
        if (isUpdate(body.query))
          return (fail(res, 400, 'an update cannot be an example', { code: 'not-a-query' }), true);
        if (list.length >= 500)
          return (
            fail(res, 409, 'the dataset has 500 suggestions', { code: 'too-many-suggestions' }),
            true
          );
        const s = {
          id: randomUUID(),
          question: body.question,
          query: body.query,
          ...(body.explanation ? { explanation: body.explanation } : {}),
          by: mockPrincipal(req) ?? 'local',
          at: new Date().toISOString(),
        };
        list.unshift(s);
        return (send(res, 201, s), true);
      }
      if (!isAdmin(req)) return (fail(res, 403, `admin access to /${name} needed`), true);
      if (req.method === 'GET') return (send(res, 200, { dataset: name, suggestions: list }), true);
      if (req.method === 'DELETE') {
        const i = list.findIndex((s) => s.id === url.searchParams.get('id'));
        if (i < 0) return (fail(res, 404, 'no such suggestion'), true);
        list.splice(i, 1);
        return (send(res, 204, ''), true);
      }
      return (fail(res, 405, 'method not allowed'), true);
    }
    return false;
  }
  const op = seg.slice(1).join('/');
  if (!['check', 'sparql/diagnose', 'recall'].includes(op)) return false;
  const ds = datasets.get(seg[0] ?? '');
  if (!ds) return (fail(res, 404, `No such dataset: ${seg[0] ?? ''}`), true);
  if (req.method !== 'POST') return (fail(res, 405, 'method not allowed'), true);
  const body = await jsonBody(req, readBody);
  if (!body) return (fail(res, 400, 'invalid JSON', { code: 'bad-request' }), true);
  if (op !== 'recall' && typeof body.query !== 'string')
    return (fail(res, 400, 'expected `query`', { code: 'bad-request' }), true);
  const out =
    op === 'check' ? check(ds, body) : op === 'recall' ? recall(ds, body) : diagnose(ds, body);
  if (out.error) return (send(res, ...out.error), true);
  return (send(res, 200, out), true);
}
