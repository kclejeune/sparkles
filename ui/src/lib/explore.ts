// SPARQL used by the Explore page (resource graph + ontology browser).
import * as api from './api';
import type { Term } from './api';
import { sparqlIri, sparqlString } from './rdf';

const P = `PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX owl: <http://www.w3.org/2002/07/owl#>
PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
PREFIX dcterms: <http://purl.org/dc/terms/>
PREFIX schema: <http://schema.org/>
`;

/** Label-ish properties, in order of preference. */
const LABEL_PATH = 'rdfs:label|skos:prefLabel|foaf:name|dcterms:title|schema:name';
export const LABEL_IRIS = [
  'http://www.w3.org/2000/01/rdf-schema#label',
  'http://www.w3.org/2004/02/skos/core#prefLabel',
  'http://xmlns.com/foaf/0.1/name',
  'http://purl.org/dc/terms/title',
  'http://schema.org/name',
];

const opts = { send: 5000 };

/** String value of a term (undefined for unbound or RDF-star triple terms). */
function v(t: Term | undefined): string | undefined {
  return t && t.type !== 'triple' ? t.value : undefined;
}

/** Choose the best label among literals: English or untagged first. */
export function pickLabel(labels: (Term | undefined)[]): string | undefined {
  const lits = labels.filter((l): l is Extract<Term, { type: 'literal' }> => l?.type === 'literal');
  const score = (l: Extract<Term, { type: 'literal' }>) => {
    const lang = l['xml:lang'] ?? '';
    return lang === 'en' || lang.startsWith('en-') ? 0 : lang === '' ? 1 : 2;
  };
  lits.sort((a, b) => score(a) - score(b));
  return lits[0]?.value;
}

export type SearchHit = { iri: string; label?: string };

export async function search(ds: string, text: string, signal?: AbortSignal): Promise<SearchHit[]> {
  const q = sparqlString(text.toLowerCase());
  const rows = await api.select(
    ds,
    `${P}
SELECT ?s (SAMPLE(?l) AS ?label) WHERE {
  {
    ?s ${LABEL_PATH} ?l .
    FILTER(CONTAINS(LCASE(STR(?l)), ${q}))
  } UNION {
    ?s ?p ?o .
    FILTER(isIRI(?s) && CONTAINS(LCASE(STR(?s)), ${q}))
    OPTIONAL { ?s rdfs:label ?l }
  }
  FILTER(isIRI(?s))
}
GROUP BY ?s
LIMIT 25`,
    { ...opts, signal },
  );
  return rows.map((r) => ({ iri: v(r.s)!, label: v(r.label) })).filter((h) => h.iri);
}

export type Neighbour = { predicate: string; term: Term; label?: string; incoming: boolean };

/** Outgoing and incoming edges of a resource (LIMIT 100 each). */
export async function neighbours(
  ds: string,
  iri: string,
): Promise<{ out: Neighbour[]; inc: Neighbour[]; label?: string }> {
  const node = sparqlIri(iri);
  const [outRows, incRows] = await Promise.all([
    api.select(
      ds,
      `${P}
SELECT ?p ?o (SAMPLE(?l) AS ?label) WHERE {
  ${node} ?p ?o .
  OPTIONAL { ?o ${LABEL_PATH} ?l }
}
GROUP BY ?p ?o
LIMIT 100`,
      opts,
    ),
    api.select(
      ds,
      `${P}
SELECT ?s ?p (SAMPLE(?l) AS ?label) WHERE {
  ?s ?p ${node} .
  OPTIONAL { ?s ${LABEL_PATH} ?l }
}
GROUP BY ?s ?p
LIMIT 100`,
      opts,
    ),
  ]);
  const out = outRows
    .filter((r) => r.p && r.o)
    .map((r) => ({ predicate: v(r.p)!, term: r.o!, label: v(r.label), incoming: false }));
  const inc = incRows
    .filter((r) => r.p && r.s)
    .map((r) => ({ predicate: v(r.p)!, term: r.s!, label: v(r.label), incoming: true }));
  const label = pickLabel(out.filter((n) => LABEL_IRIS.includes(n.predicate)).map((n) => n.term));
  return { out, inc, label };
}

export type Details = {
  props: { p: string; o: Term }[];
  incoming: { s: Term; p: string }[];
  incomingTotal: number;
};

export async function details(ds: string, iri: string): Promise<Details> {
  const node = sparqlIri(iri);
  const [props, incoming, count] = await Promise.all([
    api.select(ds, `SELECT ?p ?o WHERE { ${node} ?p ?o } LIMIT 1000`, opts),
    api.select(ds, `SELECT ?s ?p WHERE { ?s ?p ${node} } LIMIT 200`, opts),
    api.select(ds, `SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ${node} }`, opts),
  ]);
  return {
    props: props.filter((r) => r.p && r.o).map((r) => ({ p: v(r.p)!, o: r.o! })),
    incoming: incoming.filter((r) => r.p && r.s).map((r) => ({ s: r.s!, p: v(r.p)! })),
    incomingTotal: Number(v(count[0]?.n) ?? incoming.length),
  };
}

/** Fetch labels for many IRIs at once. */
export async function labels(ds: string, iris: string[]): Promise<Map<string, string>> {
  const out = new Map<string, string>();
  if (!iris.length) return out;
  const chunks: string[][] = [];
  for (let i = 0; i < iris.length; i += 80) chunks.push(iris.slice(i, i + 80));
  for (const chunk of chunks) {
    const rows = await api.select(
      ds,
      `${P}
SELECT ?s ?l WHERE {
  VALUES ?s { ${chunk.map(sparqlIri).join(' ')} }
  ?s ${LABEL_PATH} ?l .
}`,
      opts,
    );
    const by = new Map<string, Term[]>();
    for (const r of rows) if (r.s && r.l) by.set(v(r.s)!, [...(by.get(v(r.s)!) ?? []), r.l]);
    for (const [s, ls] of by) {
      const l = pickLabel(ls);
      if (l) out.set(s, l);
    }
  }
  return out;
}

// --- schema ------------------------------------------------------------------

export type ClassInfo = {
  iri: string;
  label?: string;
  comment?: string;
  supers: string[];
  subs: string[];
  instances: number;
  declared: boolean;
};

export type PropertyInfo = {
  iri: string;
  label?: string;
  kinds: string[];
  domains: string[];
  ranges: string[];
  supers: string[];
};

export type Schema = {
  classes: Map<string, ClassInfo>;
  roots: string[];
  properties: PropertyInfo[];
  ontology?: { iri: string; label?: string; version?: string; comment?: string };
};

/**
 * Drop super classes implied by other super classes (A ⊂ B ⊂ C and A ⊂ C: keep only B),
 * so a hierarchy that is transitively closed (for example by a reasoner, or by data
 * that asserts every ancestor) still draws as a tree. Equivalent classes (cycles) are kept.
 */
export function reduceSupers(supers: Map<string, string[]>): Map<string, string[]> {
  const memo = new Map<string, Set<string>>();
  const ancestors = (c: string): Set<string> => {
    const hit = memo.get(c);
    if (hit) return hit;
    const out = new Set<string>();
    const stack = [...(supers.get(c) ?? [])];
    while (stack.length) {
      const s = stack.pop()!;
      if (out.has(s)) continue;
      out.add(s);
      for (const x of supers.get(s) ?? []) stack.push(x);
    }
    memo.set(c, out);
    return out;
  };
  const reduced = new Map<string, string[]>();
  for (const [c, ss] of supers) {
    reduced.set(
      c,
      ss.filter((s) => !ss.some((s2) => s2 !== s && ancestors(s2).has(s) && !ancestors(s).has(s2))),
    );
  }
  return reduced;
}

/**
 * Ontology browser data. The hierarchy and property declarations come from asserted
 * triples only (`reasoning=false`): materialized RDFS/OWL inferences would add the
 * transitive closure of rdfs:subClassOf plus rdfs:Resource / owl:Thing everywhere.
 * Instance counts include inferences, so a superclass counts its subclasses' members.
 */
export async function loadSchema(ds: string): Promise<Schema> {
  const asserted = { reasoning: false };
  const [classRows, countRows, propRows, ontRows] = await Promise.all([
    api.select(
      ds,
      `${P}
SELECT DISTINCT ?c ?super ?label ?comment ?declared WHERE {
  { ?c a owl:Class BIND(true AS ?declared) } UNION { ?c a rdfs:Class BIND(true AS ?declared) }
  UNION { ?c rdfs:subClassOf ?x } UNION { ?x rdfs:subClassOf ?c }
  UNION { ?x rdf:type ?c FILTER(?c NOT IN (owl:Class, rdfs:Class, owl:ObjectProperty, owl:DatatypeProperty, owl:AnnotationProperty, rdf:Property, owl:Ontology, owl:TransitiveProperty, owl:SymmetricProperty, owl:FunctionalProperty, owl:InverseFunctionalProperty)) }
  FILTER(isIRI(?c))
  OPTIONAL { ?c rdfs:subClassOf ?super FILTER(isIRI(?super) && ?super != ?c) }
  OPTIONAL { ?c rdfs:label ?label }
  OPTIONAL { ?c rdfs:comment ?comment }
}
LIMIT 20000`,
      { send: 20000, ...asserted },
    ),
    api.select(
      ds,
      `SELECT ?c (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s a ?c } GROUP BY ?c LIMIT 5000`,
      { send: 5000 },
    ),
    api.select(
      ds,
      `${P}
SELECT DISTINCT ?p ?kind ?domain ?range ?label ?super WHERE {
  { ?p a ?kind FILTER(?kind IN (owl:ObjectProperty, owl:DatatypeProperty, owl:AnnotationProperty, rdf:Property, owl:TransitiveProperty, owl:SymmetricProperty, owl:FunctionalProperty, owl:InverseFunctionalProperty)) }
  UNION { ?p rdfs:domain ?d0 } UNION { ?p rdfs:range ?r0 } UNION { ?p rdfs:subPropertyOf ?s0 }
  OPTIONAL { ?p a ?kind FILTER(?kind IN (owl:ObjectProperty, owl:DatatypeProperty, owl:AnnotationProperty, rdf:Property, owl:TransitiveProperty, owl:SymmetricProperty, owl:FunctionalProperty, owl:InverseFunctionalProperty)) }
  OPTIONAL { ?p rdfs:domain ?domain }
  OPTIONAL { ?p rdfs:range ?range }
  OPTIONAL { ?p rdfs:subPropertyOf ?super }
  OPTIONAL { ?p rdfs:label ?label }
}
LIMIT 20000`,
      { send: 20000, ...asserted },
    ),
    api.select(
      ds,
      `${P}
SELECT ?o ?label ?version ?comment WHERE {
  ?o a owl:Ontology .
  OPTIONAL { ?o rdfs:label|dcterms:title ?label }
  OPTIONAL { ?o owl:versionInfo ?version }
  OPTIONAL { ?o rdfs:comment|dcterms:description ?comment }
} LIMIT 1`,
      asserted,
    ),
  ]);

  const classes = new Map<string, ClassInfo>();
  const labelsBy = new Map<string, Term[]>();
  const commentsBy = new Map<string, Term[]>();
  const get = (iri: string) => {
    let c = classes.get(iri);
    if (!c) classes.set(iri, (c = { iri, supers: [], subs: [], instances: 0, declared: false }));
    return c;
  };
  for (const r of classRows) {
    const c = get(v(r.c)!);
    if (r.declared) c.declared = true;
    if (r.super && !c.supers.includes(v(r.super)!)) c.supers.push(v(r.super)!);
    if (r.label) labelsBy.set(c.iri, [...(labelsBy.get(c.iri) ?? []), r.label]);
    if (r.comment) commentsBy.set(c.iri, [...(commentsBy.get(c.iri) ?? []), r.comment]);
  }
  const reduced = reduceSupers(new Map([...classes.values()].map((c) => [c.iri, c.supers])));
  for (const c of classes.values()) c.supers = reduced.get(c.iri) ?? c.supers;
  for (const c of [...classes.values()]) {
    for (const s of c.supers) {
      const sup = get(s);
      if (!sup.subs.includes(c.iri)) sup.subs.push(c.iri);
    }
    c.label = pickLabel(labelsBy.get(c.iri) ?? []);
    c.comment = pickLabel(commentsBy.get(c.iri) ?? []);
  }
  for (const r of countRows) {
    const c = r.c && classes.get(v(r.c)!);
    if (c) c.instances = Number(v(r.n) ?? 0);
  }
  const name = (c: ClassInfo) => (c.label ?? c.iri).toLowerCase();
  const byRank = (a: ClassInfo, b: ClassInfo) =>
    b.subs.length - a.subs.length || name(a).localeCompare(name(b));
  const roots = [...classes.values()]
    .filter((c) => c.supers.length === 0)
    .sort(byRank)
    .map((c) => c.iri);
  // Classes on a subClassOf cycle (A ⊑ B ⊑ A, or A ⊑ A) all have superclasses, so a
  // cycle with no ordinary root above it would be unreachable: promote one member of
  // each such group to a root.
  const reached = new Set<string>();
  const visit = (iri: string) => {
    const stack = [iri];
    while (stack.length) {
      const x = stack.pop()!;
      if (reached.has(x)) continue;
      reached.add(x);
      stack.push(...(classes.get(x)?.subs ?? []));
    }
  };
  roots.forEach(visit);
  for (const c of [...classes.values()].sort(byRank)) {
    if (reached.has(c.iri)) continue;
    roots.push(c.iri);
    visit(c.iri);
  }
  for (const c of classes.values())
    c.subs.sort((a, b) => name(classes.get(a)!).localeCompare(name(classes.get(b)!)));

  const props = new Map<string, PropertyInfo & { _labels: Term[] }>();
  for (const r of propRows) {
    if (!r.p) continue;
    let p = props.get(v(r.p)!);
    if (!p)
      props.set(
        v(r.p)!,
        (p = { iri: v(r.p)!, kinds: [], domains: [], ranges: [], supers: [], _labels: [] }),
      );
    const add = (arr: string[], t?: Term) =>
      t && t.type === 'uri' && !arr.includes(t.value) && arr.push(t.value);
    add(p.kinds, r.kind);
    add(p.domains, r.domain);
    add(p.ranges, r.range);
    add(p.supers, r.super);
    if (r.label) p._labels.push(r.label);
  }
  const properties = [...props.values()]
    .map(({ _labels, ...p }) => ({ ...p, label: pickLabel(_labels) }))
    .sort((a, b) => (a.label ?? a.iri).localeCompare(b.label ?? b.iri));

  const o = ontRows[0];
  return {
    classes,
    roots,
    properties,
    ontology: o?.o
      ? { iri: v(o.o)!, label: v(o.label), version: v(o.version), comment: v(o.comment) }
      : undefined,
  };
}
