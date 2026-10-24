// `POST /$/reason/{ds}/explain` (docs/specs/U10-canvas-lenses.md §3.3, docs/API.md
// #explanations) for the mock: derivation trees of the mock reasoner's rules, found by
// iterative deepening over the dataset's store, in the answer shape of the server. The
// mock materializes rdfs9, rdfs2 and rdfs3, and for OWL 2 RL also symmetric and inverse
// properties, so those are the rules a tree names.

import ox from 'oxigraph';

const RDF_TYPE = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';
const RDFS = 'http://www.w3.org/2000/01/rdf-schema#';
const OWL = 'http://www.w3.org/2002/07/owl#';
const INFERRED = 'urn:x-sparkles:inferred';
const MAX_SEARCH = 12;

function toTerm(j) {
  if (!j || typeof j !== 'object' || typeof j.value !== 'string') return null;
  if (j.type === 'uri') return ox.namedNode(j.value);
  if (j.type === 'bnode') return ox.blankNode(j.value);
  if (j.type === 'literal') {
    if (j['xml:lang']) return ox.literal(j.value, j['xml:lang']);
    return ox.literal(j.value, j.datatype ? ox.namedNode(j.datatype) : undefined);
  }
  return null;
}

function termJson(t) {
  if (t.termType === 'NamedNode') return { type: 'uri', value: t.value };
  if (t.termType === 'BlankNode') return { type: 'bnode', value: t.value };
  const o = { type: 'literal', value: t.value };
  if (t.language) o['xml:lang'] = t.language;
  else if (t.datatype && t.datatype.value !== 'http://www.w3.org/2001/XMLSchema#string')
    o.datatype = t.datatype.value;
  return o;
}

const key = (t) => `${t[0]} ${t[1]} ${t[2]}`;

/** The explainer of one dataset's store. */
function explainer(store) {
  const nn = (v) => ox.namedNode(v);
  const quads = (s, p, o) => store.match(s ?? null, p ?? null, o ?? null, null);
  /** The graph that asserts a triple, `default` or an IRI; null when only inferred. */
  const asserted = (t) => {
    for (const q of quads(t[0], t[1], t[2])) {
      if (q.graph.value === INFERRED) continue;
      return q.graph.termType === 'DefaultGraph' ? 'default' : q.graph.value;
    }
    return null;
  };
  const holds = (t) => quads(t[0], t[1], t[2]).length > 0;
  /** The rule instances whose head is `t` and whose premises hold. */
  const instances = (t) => {
    const [s, p, o] = t;
    const out = [];
    if (p.value === RDF_TYPE && o.termType === 'NamedNode') {
      for (const q of quads(null, nn(`${RDFS}subClassOf`), o)) {
        const typed = [s, p, q.subject];
        if (holds(typed))
          out.push({ rule: 'rdfs9', premises: [typed, [q.subject, q.predicate, o]] });
      }
      for (const q of quads(null, nn(`${RDFS}domain`), o)) {
        const use = quads(s, q.subject, null)[0];
        if (use)
          out.push({
            rule: 'rdfs2',
            premises: [
              [s, q.subject, use.object],
              [q.subject, q.predicate, o],
            ],
          });
      }
      for (const q of quads(null, nn(`${RDFS}range`), o)) {
        const use = quads(null, q.subject, s)[0];
        if (use)
          out.push({
            rule: 'rdfs3',
            premises: [
              [use.subject, q.subject, s],
              [q.subject, q.predicate, o],
            ],
          });
      }
    }
    if (p.termType === 'NamedNode' && o.termType !== 'Literal') {
      const sym = [p, nn(RDF_TYPE), nn(`${OWL}SymmetricProperty`)];
      if (holds(sym) && holds([o, p, s]))
        out.push({ rule: 'prp-symp', premises: [sym, [o, p, s]] });
      for (const q of quads(null, nn(`${OWL}inverseOf`), p))
        if (holds([o, q.subject, s]))
          out.push({
            rule: 'prp-inv1',
            premises: [
              [q.subject, q.predicate, p],
              [o, q.subject, s],
            ],
          });
    }
    return out.filter((i) => !i.premises.some((x) => key(x) === key(t)));
  };
  // depth-bounded proofs, memoized per depth: a premise counts at d only when it is
  // asserted or has an instance whose premises count at d - 1
  const memo = new Map();
  const prove = (t, d) => {
    if (asserted(t)) return { kind: 'asserted', t };
    if (d <= 0) return null;
    const k = `${key(t)}@${d}`;
    if (memo.has(k)) return memo.get(k);
    memo.set(k, null);
    let best = null;
    for (const i of instances(t)) {
      const ps = i.premises.map((x) => prove(x, d - 1));
      if (ps.some((x) => !x)) continue;
      best = { kind: 'derived', t, rule: i.rule, premises: ps };
      break;
    }
    memo.set(k, best);
    return best;
  };
  return { asserted, instances, prove };
}

/** Number the trees' nodes as the server does: a repeated triple is shared by id within a tree. */
function render(proofs, profile, maxDepth) {
  const ids = new Map();
  let next = 0;
  const depthOf = (p) => (p.kind === 'asserted' ? 0 : 1 + Math.max(0, ...p.premises.map(depthOf)));
  const node = (p, level, root) => {
    const k = key(p.t);
    const triple = { s: termJson(p.t[0]), p: termJson(p.t[1]), o: termJson(p.t[2]) };
    if (!root && ids.has(k)) {
      const n = { id: ids.get(k), triple, kind: p.kind, depth: depthOf(p), shared: true };
      if (p.rule) n.rule = { name: p.rule, profile };
      return n;
    }
    const id = ++next;
    // a node cut at the depth limit is no target for sharing, as on the server
    const cut = p.kind !== 'asserted' && level >= maxDepth;
    if (!root && !cut) ids.set(k, id);
    if (p.kind === 'asserted') return { id, triple, kind: 'asserted', depth: 0, graph: p.graph };
    if (cut) return { id, triple, kind: 'ref', depth: depthOf(p) };
    const rule = { name: p.rule, profile };
    if (profile === 'owl-rl' && /^(prp|cax|scm|eq|cls)-/.test(p.rule)) rule.owl2rl = p.rule;
    return {
      id,
      triple,
      kind: 'derived',
      depth: depthOf(p),
      rule,
      premises: p.premises.map((x) => node(x, level + 1, false)),
    };
  };
  // a tree shares repeated triples within itself only, as the server's trees do
  return proofs.map((p) => {
    ids.clear();
    return node(p, 0, true);
  });
}

export async function handleReason(req, res, seg, ctx) {
  const { datasets, send, fail, readBody, headSeq } = ctx;
  if (seg[0] !== '$' || seg[1] !== 'reason' || seg[3] !== 'explain') return false;
  if (req.method !== 'POST') return (fail(res, 405, 'POST only'), true);
  const ds = datasets.get(seg[2]);
  if (!ds) return (fail(res, 404, `No such dataset: ${seg[2]}`), true);
  let body;
  try {
    body = JSON.parse((await readBody(req)).toString('utf8') || '{}');
  } catch {
    return (fail(res, 400, 'invalid JSON'), true);
  }
  if (body.mode === 'whyNot')
    return (
      fail(res, 501, 'whyNot explanations are not available', { code: 'not-implemented' }), true
    );
  const t = body.triple ?? {};
  const triple = [toTerm(t.s), toTerm(t.p), toTerm(t.o)];
  if (triple.some((x) => !x)) return (fail(res, 400, 'triple must have s, p and o'), true);
  const maxTrees = Math.min(Math.max(Number(body.maxTrees ?? 1), 1), 10);
  const maxDepth = Math.min(Math.max(Number(body.maxDepth ?? 32), 1), 64);
  const none = () =>
    fail(res, 404, 'no derivation from facts you can read was found', {
      code: 'no-visible-derivation',
    });
  const x = explainer(ds.store);
  const withGraph = (p) =>
    p.kind === 'asserted'
      ? { ...p, graph: x.asserted(p.t) }
      : { ...p, premises: p.premises.map(withGraph) };
  const proofs = [];
  let more = false;
  if (x.asserted(triple)) proofs.push(withGraph({ kind: 'asserted', t: triple }));
  else {
    if (!ds.reasoning) return (none(), true);
    // one proof per top rule, the shallowest first
    const rules = new Map();
    for (const i of x.instances(triple)) {
      if (rules.has(i.rule)) continue;
      for (let d = 1; d <= MAX_SEARCH; d++) {
        const ps = i.premises.map((p) => x.prove(p, d - 1));
        if (ps.every(Boolean)) {
          rules.set(i.rule, withGraph({ kind: 'derived', t: triple, rule: i.rule, premises: ps }));
          break;
        }
      }
    }
    const all = [...rules.values()];
    const depth = (p) => (p.kind === 'asserted' ? 0 : 1 + Math.max(0, ...p.premises.map(depth)));
    all.sort((a, b) => depth(a) - depth(b));
    more = all.length > maxTrees;
    proofs.push(...all.slice(0, maxTrees));
  }
  if (!proofs.length) return (none(), true);
  const profile = ds.reasoning?.profile ?? 'rdfs';
  const commit = ds.reasoning?.commit ?? headSeq(ds);
  const head = headSeq(ds);
  const out = {
    mode: 'why',
    entailed: true,
    commit,
    source: 'materialized',
    trees: render(proofs, profile, maxDepth),
    more,
  };
  if (ds.reasoning) {
    out.stale = head !== commit;
    out.commitsSince = Math.max(0, head - commit);
  }
  return (send(res, 200, out), true);
}
