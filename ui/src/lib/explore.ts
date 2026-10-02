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
  /** Asserted superclasses after transitive reduction: the edges the tree draws. */
  supers: string[];
  subs: string[];
  instances: number;
  /** Declared with rdf:type rdfs:Class, owl:Class or rdfs:Datatype. */
  declared: boolean;
  /** In the rdf:, rdfs:, owl:, xsd: or sh: namespace. */
  builtin: boolean;
  /** Asserted superclasses, as declared. */
  assertedSupers: string[];
  equivalents: string[];
  disjoint: string[];
  /** The other members of the subClassOf cycle the class is on (empty if none). */
  cycle: string[];
};

export type PropertyInfo = {
  iri: string;
  label?: string;
  comment?: string;
  /** Declared property types (rdf:Property, owl:ObjectProperty, owl:FunctionalProperty, …). */
  kinds: string[];
  domains: string[];
  ranges: string[];
  supers: string[];
  inverseOf: string[];
  builtin: boolean;
  observed: api.SchemaPredicate['observed'];
};

export type Schema = {
  classes: Map<string, ClassInfo>;
  roots: string[];
  properties: PropertyInfo[];
  ontology?: { iri: string; label?: string; version?: string; comment?: string };
  snapshot: api.SchemaSummary['snapshot'];
  selection: api.SchemaSummary['selection'];
  totals: api.SchemaSummary['totals'];
  cycles: string[][];
  /** The SHACL constraints of the report, one line per class and property shape. */
  constraints: ConstraintLine[];
};

/** One property shape of the constraints layer, with the class it applies to. */
export type ConstraintLine = {
  class: string;
  source: api.ConstraintSource['kind'];
  constraint: api.PropertyConstraint;
};

/** The constraints layer as one line per class and property shape. */
export function constraintLines(layer?: api.ConstraintsLayer): ConstraintLine[] {
  return (layer?.sources ?? []).flatMap((src) =>
    src.classes.flatMap((c) =>
      c.properties.map((constraint) => ({ class: c.class, source: src.kind, constraint })),
    ),
  );
}

const ENFORCEMENT: Record<api.Enforcement, { text: string; title: string }> = {
  'reject-on-write': {
    text: 'enforced on write',
    title: 'Write-time validation refuses a write that breaks this constraint.',
  },
  'warn-on-write': {
    text: 'reported on write',
    title: 'Write-time validation commits a write that breaks this constraint and reports it.',
  },
  'validated-on-request': {
    text: 'checked on request',
    title: 'Nothing checks this constraint until the data is validated.',
  },
};

/** How a constraint is enforced, in words. */
export function enforcementText(e: api.Enforcement): { text: string; title: string } {
  return ENFORCEMENT[e];
}

/**
 * `min 1 · max 1 · datatype xsd:string · class ex:Org` for one property shape, with IRIs
 * shortened by `short`.
 */
export function constraintSummary(
  c: api.PropertyConstraint,
  short: (iri: string) => string,
): string {
  const parts: string[] = [];
  if (c.minCount != null) parts.push(`min ${c.minCount}`);
  if (c.maxCount != null) parts.push(`max ${c.maxCount}`);
  if (c.datatype) parts.push(`datatype ${short(c.datatype)}`);
  for (const k of c.class ?? []) parts.push(`class ${short(k)}`);
  if (c.nodeKind) parts.push(`nodeKind ${short(c.nodeKind)}`);
  for (const o of c.other ?? [])
    parts.push(`+${o.replace(/^.*[#/]/, '').replace(/ConstraintComponent$/, '')}`);
  return parts.length ? parts.join(' · ') : 'no constraints';
}

/**
 * The declared SHACL chips of a property: one per class whose shapes constrain it. They
 * are separate from the observed and declared cardinality chips and never merged with
 * them.
 */
export function constraintChips(
  lines: ConstraintLine[],
  predicate: string,
  short: (iri: string) => string,
): { text: string; title: string; enforcement: api.Enforcement }[] {
  return lines
    .filter((l) => l.constraint.path === predicate)
    .map((l) => {
      const e = enforcementText(l.constraint.enforcement);
      return {
        text: `SHACL ${constraintSummary(l.constraint, short)}`,
        title: `Declared by a SHACL shape of ${short(l.class)}, ${e.text}. ${e.title}`,
        enforcement: l.constraint.enforcement,
      };
    });
}

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
 * Roots of the class tree, most subclasses first, then by label or IRI: `seeds` (the
 * server's roots) or, without seeds, the classes without superclasses; followed by one
 * representative of every group of classes that is not reachable from them. Classes on
 * a subClassOf cycle (A ⊑ B ⊑ A, or A ⊑ A) all have superclasses, so a cycle with no
 * ordinary root above it would otherwise never be drawn. Expects `subs` to be the
 * inverse of `supers`.
 */
export function hierarchyRoots(classes: Map<string, ClassInfo>, seeds?: string[]): string[] {
  const name = (c: ClassInfo) => (c.label ?? c.iri).toLowerCase();
  const byRank = (a: ClassInfo, b: ClassInfo) =>
    b.subs.length - a.subs.length || name(a).localeCompare(name(b));
  const initial = seeds
    ? [...new Set(seeds)].flatMap((iri) => classes.get(iri) ?? [])
    : [...classes.values()].filter((c) => c.supers.length === 0);
  const roots = initial.sort(byRank).map((c) => c.iri);
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
  return roots;
}

/** Best label among literals (English or untagged first). */
export function pickLit(lits: api.Lit[]): string | undefined {
  return pickLabel(
    lits.map((l) => ({
      type: 'literal',
      value: l.value,
      ...(l.lang ? { 'xml:lang': l.lang } : {}),
    })),
  );
}

/**
 * The explorer's schema model from a complete schema report. Built-in classes (rdf:,
 * rdfs:, owl:, …) are kept only when they take part in the declared hierarchy, so
 * `owl:Class` used as an rdf:type target does not show up as a class of the data while
 * `owl:Thing` as a superclass does.
 */
export function schemaFromSummary(s: api.SchemaSummary): Schema {
  const superOf = new Set(s.classes.items.flatMap((c) => c.declared.superClasses));
  const kept = s.classes.items.filter(
    (c) => !c.builtin || c.declared.superClasses.length > 0 || superOf.has(c.iri),
  );
  const cycleOf = new Map<string, string[]>();
  for (const cycle of s.hierarchy.cycles)
    for (const m of cycle)
      cycleOf.set(
        m,
        cycle.filter((x) => x !== m),
      );
  const classes = new Map<string, ClassInfo>();
  for (const c of kept) {
    classes.set(c.iri, {
      iri: c.iri,
      label: pickLit(c.declared.labels),
      comment: pickLit(c.declared.comments),
      supers: [],
      subs: [],
      instances: c.observed.instances,
      declared: c.declared.types.length > 0,
      builtin: c.builtin,
      assertedSupers: c.declared.superClasses,
      equivalents: c.declared.equivalentClasses,
      disjoint: c.declared.disjointWith,
      cycle: cycleOf.get(c.iri) ?? [],
    });
  }
  const reduced = reduceSupers(
    new Map(
      [...classes.values()].map((c) => [c.iri, c.assertedSupers.filter((x) => classes.has(x))]),
    ),
  );
  for (const c of classes.values()) {
    c.supers = reduced.get(c.iri) ?? [];
    for (const sup of c.supers) classes.get(sup)!.subs.push(c.iri);
  }
  const roots = hierarchyRoots(classes, s.hierarchy.roots);
  const name = (c: ClassInfo) => (c.label ?? c.iri).toLowerCase();
  for (const c of classes.values())
    c.subs.sort((a, b) => name(classes.get(a)!).localeCompare(name(classes.get(b)!)));

  const properties = s.predicates.items
    .map((p): PropertyInfo => ({
      iri: p.iri,
      label: pickLit(p.declared.labels),
      comment: pickLit(p.declared.comments),
      kinds: p.declared.types,
      domains: p.declared.domains,
      ranges: p.declared.ranges,
      supers: p.declared.superProperties,
      inverseOf: p.declared.inverseOf,
      builtin: p.builtin,
      observed: p.observed,
    }))
    .sort((a, b) => (a.label ?? a.iri).localeCompare(b.label ?? b.iri));

  const o = s.ontology[0];
  return {
    classes,
    roots,
    properties,
    ontology: o
      ? {
          iri: o.iri,
          label: pickLit(o.labels),
          version: o.versionInfo[0]?.value,
          comment: pickLit(o.comments),
        }
      : undefined,
    snapshot: s.snapshot,
    selection: s.selection,
    totals: s.totals,
    cycles: s.hierarchy.cycles,
    constraints: constraintLines(s.constraints),
  };
}

/**
 * Ontology browser data from the server's schema report (`/$/schema/{ds}`, all pages of
 * one snapshot). Declarations are asserted ones only (the server's default): materialized
 * RDFS/OWL would add the transitive closure of rdfs:subClassOf plus rdfs:Resource /
 * owl:Thing everywhere. Instance counts include inferences unless `reasoning` is false.
 */
export async function loadSchema(
  ds: string,
  opts: { graph?: string; reasoning?: boolean } = {},
): Promise<Schema> {
  return schemaFromSummary(await api.schema(ds, { ...opts, limit: 5000 }));
}

/** One segment of a property's object-kind bar. */
export type ObjectSegment = {
  kind: 'iri' | 'blank' | 'triple' | 'literal';
  /** Datatype IRI for literals. */
  datatype?: string;
  triples: number;
  /** Share of the property's triples, 0–1. */
  share: number;
  /** Language tags (with direction) of language-tagged literals. */
  languages: string[];
};

/** Object kinds and literal datatypes of a property, largest first. */
export function objectSegments(o: api.SchemaPredicate['observed']): ObjectSegment[] {
  const total = o.triples || 1;
  const segs: ObjectSegment[] = [];
  const kinds = [
    ['iri', o.objects.iri],
    ['blank', o.objects.blank],
    ['triple', o.objects.tripleTerm],
  ] as const;
  for (const [kind, k] of kinds)
    if (k?.triples)
      segs.push({ kind, triples: k.triples, share: k.triples / total, languages: [] });
  for (const l of o.objects.literals)
    segs.push({
      kind: 'literal',
      datatype: l.datatype,
      triples: l.triples,
      share: l.triples / total,
      languages: (l.languages ?? []).map((x) =>
        x.direction ? `${x.lang}--${x.direction}` : x.lang,
      ),
    });
  return segs.sort((a, b) => b.triples - a.triples);
}

export const OWL_FUNCTIONAL = 'http://www.w3.org/2002/07/owl#FunctionalProperty';

/**
 * Cardinality chips of a property. What was observed and what is declared are separate
 * chips and never merged: at most one value per subject in this snapshot says nothing
 * about the next write, and a declared owl:FunctionalProperty says nothing about the data.
 */
export function cardinalityChips(
  p: PropertyInfo,
): { kind: 'observed' | 'declared'; text: string; title: string }[] {
  const chips: { kind: 'observed' | 'declared'; text: string; title: string }[] = [];
  if (p.observed.triples > 0 && p.observed.maxPerSubject === 1)
    chips.push({
      kind: 'observed',
      text: '≤1 per subject (observed)',
      title: 'No constraint enforces this; a future write may add a second value.',
    });
  if (p.kinds.includes(OWL_FUNCTIONAL))
    chips.push({
      kind: 'declared',
      text: 'functional (declared)',
      title: 'Declared owl:FunctionalProperty in the data; not validated on write.',
    });
  return chips;
}

/** "as of version N · generation G · computed HH:MM" for the schema header. */
export function snapshotLine(s: api.SchemaSummary['snapshot']): string {
  const d = new Date(s.computedAt);
  const at = Number.isNaN(d.getTime())
    ? s.computedAt
    : d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
  return `as of version ${s.version} · generation ${s.generation} · computed ${at}`;
}
