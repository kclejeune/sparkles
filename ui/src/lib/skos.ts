import { preferredLabel } from './rendering';
// The thesaurus editor (docs/specs/U12-ontology-workbench/SPEC.md, "SKOS and SKOS-XL
// thesaurus editor"): the SPARQL that reads a graph's concept schemes, concepts and
// collections one level of the hierarchy at a time, the details of one resource, label
// search in any language, the updates of every edit with both directions of the
// hierarchy kept, and the label checks the editor shows before an edit. The integrity
// conditions that a write guard can enforce live in the built-in SKOS shapes
// (`GET /$/skos/shapes`, crates/sparkles/src/skos/shapes.ttl). The label checks here repeat
// S13 and S14 for the one resource the editor shows, so that a clash shows as it is typed.
import { request, type Term } from './api';
import { dataUpdate, type HeaderTriple, type Lit } from './ontology';
import { sparqlIri, sparqlString, WELL_KNOWN } from './rdf';

export const SKOS = WELL_KNOWN.skos;
export const XL = 'http://www.w3.org/2008/05/skos-xl#';
const RDF = WELL_KNOWN.rdf;
const RDF_TYPE = RDF + 'type';

export const CONCEPT = SKOS + 'Concept';
export const SCHEME = SKOS + 'ConceptScheme';
export const COLLECTION = SKOS + 'Collection';
export const ORDERED = SKOS + 'OrderedCollection';
export const XL_LABEL = XL + 'Label';
export const LITERAL_FORM = XL + 'literalForm';

const s = (local: string) => sparqlIri(SKOS + local);
const x = (local: string) => sparqlIri(XL + local);
const iri = (v: string) => sparqlIri(v);

/** The most children of one concept the tree reads. */
export const CHILD_CAP = 2000;
/** The most roots of a scheme the tree reads. */
export const ROOT_CAP = 5000;

// --- labels --------------------------------------------------------------------------

export type LabelKind = 'pref' | 'alt' | 'hidden';
export const LABEL_KINDS: LabelKind[] = ['pref', 'alt', 'hidden'];
export const KIND_NAME: Record<LabelKind, string> = {
  pref: 'Preferred',
  alt: 'Alternative',
  hidden: 'Hidden',
};
export const labelProperty = (k: LabelKind) => SKOS + `${k}Label`;
export const xlProperty = (k: LabelKind) => XL + `${k}Label`;

/** A label: its text and its language tag, lower case, empty for none. */
export type Label = { value: string; lang: string };

/** A plain label of a resource, or the literal form of one of its SKOS-XL labels. */
export type LabelEntry = {
  kind: LabelKind;
  label: Label;
  /** The SKOS-XL label resource whose literal form this is, null for a plain label. */
  xl: Term | null;
};

export const labelLit = (l: Label): Lit =>
  l.lang ? { '@value': l.value, '@language': l.lang } : { '@value': l.value };

export function labelOf(t: Term | undefined): Label | null {
  if (!t || t.type !== 'literal') return null;
  return { value: t.value, lang: (t['xml:lang'] ?? '').toLowerCase() };
}

export const sameLabel = (a: Label, b: Label) =>
  a.value === b.value && a.lang.toLowerCase() === b.lang.toLowerCase();

const litSparql = (l: Label) =>
  l.lang ? `${sparqlString(l.value)}@${l.lang}` : sparqlString(l.value);

/** A clash between labels of one resource, by the SKOS integrity condition it breaks. */
export type LabelProblem = {
  rule: 'S13' | 'S14';
  message: string;
  labels: LabelEntry[];
};

/**
 * The S13 and S14 clashes among the labels of one resource. The literal forms of SKOS-XL
 * labels count as labels of their kind (S55 to S57). The same label held both plainly and
 * as an XL label of the same kind is one label, as S55 makes it, and does not clash.
 */
export function labelProblems(entries: LabelEntry[]): LabelProblem[] {
  const out: LabelProblem[] = [];
  const key = (l: Label) => `${l.lang.toLowerCase()}\u0000${l.value}`;
  // S14: at most one preferred label per language tag
  const prefs = new Map<string, LabelEntry[]>();
  for (const e of entries.filter((e) => e.kind === 'pref')) {
    const l = prefs.get(e.label.lang.toLowerCase()) ?? [];
    if (!l.some((o) => sameLabel(o.label, e.label))) l.push(e);
    prefs.set(e.label.lang.toLowerCase(), l);
  }
  for (const [lang, l] of prefs)
    if (l.length > 1)
      out.push({
        rule: 'S14',
        message: `More than one preferred label ${lang ? `in ${lang}` : 'without a language tag'}.`,
        labels: l,
      });
  // S13: preferred, alternative and hidden labels are pairwise disjoint
  const byLabel = new Map<string, LabelEntry[]>();
  for (const e of entries) byLabel.set(key(e.label), [...(byLabel.get(key(e.label)) ?? []), e]);
  for (const l of byLabel.values()) {
    const kinds = new Set(l.map((e) => e.kind));
    if (kinds.size > 1)
      out.push({
        rule: 'S13',
        message: `“${l[0].label.value}”${l[0].label.lang ? `@${l[0].label.lang}` : ''} is both ${[
          ...kinds,
        ]
          .map((k) => KIND_NAME[k].toLowerCase())
          .join(' and ')}.`,
        labels: l,
      });
  }
  return out;
}

/** What adding `label` of `kind` would break among `entries`, or null. */
export function addProblem(entries: LabelEntry[], kind: LabelKind, label: Label): string | null {
  if (entries.some((e) => e.kind === kind && sameLabel(e.label, label)))
    return 'The resource has this label already.';
  const p = labelProblems([...entries, { kind, label, xl: null }]).find((p) =>
    p.labels.some((e) => e.xl === null && e.kind === kind && sameLabel(e.label, label)),
  );
  return p ? `${p.rule}: ${p.message}` : null;
}

/**
 * The label to show among `entries`' preferred labels: the first in one of `langs` (a
 * primary subtag matches too), then one without a language, then English, then the first.
 */
export function displayLabel(lits: Label[] | undefined, langs: readonly string[]): string | null {
  return preferredLabel(
    lits?.map((l) => ({ '@value': l.value, '@language': l.lang })),
    langs,
  );
}

/** A BCP 47 tag as the editor writes it: lower case, or empty. */
export const normLang = (l: string) => l.trim().toLowerCase();

// --- reads ---------------------------------------------------------------------------

/** A pattern in the editor's graph: a named graph, or the default graph for null. */
export const inGraph = (graph: string | null, body: string) =>
  graph === '*'
    ? `{ ${body} } UNION { GRAPH ?anyGraph { ${body} } }`
    : graph
      ? `GRAPH ${iri(graph)} { ${body} }`
      : body;

const values = (v: string, iris: string[]) => `VALUES ?${v} { ${iris.map(iri).join(' ')} }`;

/** The narrower concepts of `?c` by either direction of the hierarchy, bound to `?k`. */
const NARROWER_OF_C = `{ ?k ${s('broader')} ?c } UNION { ?c ${s('narrower')} ?k }`;
/** The broader concepts of `?c` by either direction, bound to `?up`. */
const BROADER_OF_C = `{ ?c ${s('broader')} ?up } UNION { ?up ${s('narrower')} ?c }`;

/** The concept schemes of a graph: typed, or named by `inScheme` or a top concept. */
export function schemesQuery(graph: string | null): string {
  return `SELECT DISTINCT ?s WHERE { ${inGraph(
    graph,
    `{ ?s a ${s('ConceptScheme')} } UNION { ?c ${s('inScheme')} ?s } UNION { ?s ${s(
      'hasTopConcept',
    )} ?c } UNION { ?c ${s('topConceptOf')} ?s }`,
  )} FILTER(isIRI(?s)) } ORDER BY ?s LIMIT 1000`;
}

/**
 * The roots of the tree with their narrower counts. With a scheme they are its top
 * concepts (`top` 1) and its concepts that have no broader concept (`top` 0). Without one
 * they are the concepts that have no broader concept.
 */
export function rootsQuery(scheme: string | null, graph: string | null): string {
  const noBroader = `FILTER NOT EXISTS { ?c ${s('broader')} ?b } FILTER NOT EXISTS { ?b2 ${s(
    'narrower',
  )} ?c }`;
  const roots = scheme
    ? `{ ${iri(scheme)} ${s('hasTopConcept')} ?c BIND(1 AS ?t) }
    UNION { ?c ${s('topConceptOf')} ${iri(scheme)} BIND(1 AS ?t) }
    UNION { ?c ${s('inScheme')} ${iri(scheme)} ${noBroader} BIND(0 AS ?t) }`
    : `{ ?c a ${s('Concept')} ${noBroader} BIND(0 AS ?t) }`;
  return `SELECT ?c (MAX(?t) AS ?top) (COUNT(DISTINCT ?k) AS ?n) (COUNT(DISTINCT ?up) AS ?ups) WHERE { ${inGraph(
    graph,
    `${roots} OPTIONAL { ${NARROWER_OF_C} } OPTIONAL { ${BROADER_OF_C} }`,
  )} FILTER(isIRI(?c)) } GROUP BY ?c ORDER BY ?c LIMIT ${ROOT_CAP}`;
}

/** The narrower concepts of each of `parents`, with their own narrower counts. */
export function childrenQuery(parents: string[], graph: string | null): string {
  return `SELECT ?p ?c (COUNT(DISTINCT ?k) AS ?n) (COUNT(DISTINCT ?up) AS ?ups) WHERE { ${inGraph(
    graph,
    `${values('p', parents)} { ?c ${s('broader')} ?p } UNION { ?p ${s(
      'narrower',
    )} ?c } OPTIONAL { ${NARROWER_OF_C} } OPTIONAL { ${BROADER_OF_C} }`,
  )} FILTER(isIRI(?c)) } GROUP BY ?p ?c ORDER BY ?p ?c LIMIT ${CHILD_CAP * Math.max(1, parents.length)}`;
}

/** The preferred labels of `iris`, plain and SKOS-XL. */
export function labelsQuery(iris: string[], graph: string | null): string {
  return `SELECT ?r ?l WHERE { ${inGraph(
    graph,
    `${values('r', iris)} { ?r ${s('prefLabel')} ?l } UNION { ?r ${x('prefLabel')}/${x(
      'literalForm',
    )} ?l }`,
  )} FILTER(isLiteral(?l)) }`;
}

/** The broader concepts of `iri`, transitively, so that the tree can open its paths. */
export function ancestorsQuery(c: string, graph: string | null): string {
  return `SELECT DISTINCT ?a WHERE { ${inGraph(
    graph,
    `${iri(c)} (${s('broader')}|^${s('narrower')})+ ?a`,
  )} FILTER(isIRI(?a)) } LIMIT 500`;
}

/** Whether `c` is `target` or one of its broader concepts, which a move under it would cycle. */
export function aboveQuery(c: string, target: string, graph: string | null): string {
  return `ASK { ${inGraph(graph, `${iri(target)} (${s('broader')}|^${s('narrower')})* ${iri(c)}`)} }`;
}

/** The collections of a graph, with their kind. */
export function collectionsQuery(graph: string | null): string {
  return `SELECT ?c (MAX(?o) AS ?ordered) WHERE { ${inGraph(
    graph,
    `{ ?c a ${s('Collection')} BIND(0 AS ?o) } UNION { ?c a ${s('OrderedCollection')} BIND(1 AS ?o) }`,
  )} FILTER(isIRI(?c)) } GROUP BY ?c ORDER BY ?c LIMIT 5000`;
}

/** The language tags of a graph's labels. */
export function languagesQuery(graph: string | null): string {
  return `SELECT DISTINCT (LCASE(LANG(?l)) AS ?lang) WHERE { ${inGraph(
    graph,
    `?r ${s('prefLabel')}|${s('altLabel')}|${x('literalForm')} ?l`,
  )} FILTER(LANG(?l) != "") } ORDER BY ?lang LIMIT 200`;
}

/** The most matches a label search answers. */
export const SEARCH_CAP = 100;

/**
 * The resources with a label containing `text`, case-insensitively, in any language or in
 * `lang`, among the plain labels and the literal forms of SKOS-XL labels, in `scheme` when
 * one is given.
 */
export function searchQuery(
  text: string,
  o: { lang?: string | null; scheme?: string | null; graph: string | null },
): string {
  const kinds = LABEL_KINDS.map((k) => `(${s(`${k}Label`)} ${x(`${k}Label`)} "${k}")`).join(' ');
  const filters = [
    `isLiteral(?l)`,
    `CONTAINS(LCASE(STR(?l)), ${sparqlString(text.trim().toLowerCase())})`,
    ...(o.lang ? [`LANGMATCHES(LANG(?l), ${sparqlString(o.lang)})`] : []),
  ];
  const scheme = o.scheme
    ? `{ ?r ${s('inScheme')} ${iri(o.scheme)} } UNION { ?r ${s('topConceptOf')} ${iri(o.scheme)} } UNION { ${iri(o.scheme)} ${s('hasTopConcept')} ?r }`
    : '';
  return `SELECT ?r ?kind ?l WHERE { ${inGraph(
    o.graph,
    `VALUES (?p ?xp ?kind) { ${kinds} } { ?r ?p ?l } UNION { ?r ?xp ?xl . ?xl ${x('literalForm')} ?l } ${scheme}`,
  )} FILTER(${filters.join(' && ')}) } LIMIT ${SEARCH_CAP}`;
}
export type SearchHit = { iri: string; kind: LabelKind; label: Label };

/** The search answer, preferred labels and matches at the start first. */
export function searchHits(rows: Record<string, Term | undefined>[], text: string): SearchHit[] {
  const q = text.trim().toLowerCase();
  const hits: SearchHit[] = [];
  for (const r of rows) {
    const l = labelOf(r.l);
    if (r.r?.type !== 'uri' || !l) continue;
    hits.push({ iri: r.r.value, kind: (r.kind?.value ?? 'pref') as LabelKind, label: l });
  }
  const rank = (h: SearchHit) =>
    (h.label.value.toLowerCase().startsWith(q) ? 0 : 2) + (h.kind === 'pref' ? 0 : 1);
  return hits.sort(
    (a, b) => rank(a) - rank(b) || a.label.value.localeCompare(b.label.value, undefined),
  );
}

/** Everything about one resource that the details pane shows. */
export function detailsQuery(r: string, graph: string | null): string {
  const inverse = [
    'broader',
    'narrower',
    'related',
    'hasTopConcept',
    'topConceptOf',
    'member',
    'exactMatch',
    'closeMatch',
    'relatedMatch',
    'broadMatch',
    'narrowMatch',
  ]
    .map(s)
    .join(' ');
  return `SELECT ?p ?o ?dir ?xf WHERE { ${inGraph(
    graph,
    `{ ${iri(r)} ?p ?o BIND("out" AS ?dir) OPTIONAL { ${iri(r)} ${x('prefLabel')}|${x('altLabel')}|${x(
      'hiddenLabel',
    )} ?o . ?o ${x('literalForm')} ?xf } }
    UNION { VALUES ?p { ${inverse} } ?o ?p ${iri(r)} BIND("in" AS ?dir) }`,
  )} } LIMIT 20000`;
}

/** A link of the resource the details pane shows: forward, inverse, or both. */
export type Link = { iri: string; forward: boolean; inverse: boolean };

export type Details = {
  iri: string;
  types: string[];
  labels: LabelEntry[];
  /** The SKOS-XL label resources, with every literal form each has. */
  xl: { node: Term; kind: LabelKind; forms: Label[] }[];
  notations: string[];
  broader: Link[];
  narrower: Link[];
  related: Link[];
  inScheme: string[];
  topConceptOf: Link[];
  /** For a scheme, its top concepts. */
  topConcepts: Link[];
  members: string[];
  /** The mappings by property local name. */
  mappings: { property: string; target: string }[];
  /** The collections the resource is a member of. */
  memberOf: string[];
  /** How many other triples the resource is the subject of. */
  other: number;
};

export const MAPPINGS = ['exactMatch', 'closeMatch', 'broadMatch', 'narrowMatch', 'relatedMatch'];
export const MAPPING_NAME: Record<string, string> = {
  exactMatch: 'Exact match',
  closeMatch: 'Close match',
  broadMatch: 'Broad match',
  narrowMatch: 'Narrow match',
  relatedMatch: 'Related match',
};

const termId = (t: Term) => (t.type === 'uri' ? t.value : t.type === 'bnode' ? `_:${t.value}` : '');

export function parseDetails(r: string, rows: Record<string, Term | undefined>[]): Details {
  const d: Details = {
    iri: r,
    types: [],
    labels: [],
    xl: [],
    notations: [],
    broader: [],
    narrower: [],
    related: [],
    inScheme: [],
    topConceptOf: [],
    topConcepts: [],
    members: [],
    mappings: [],
    memberOf: [],
    other: 0,
  };
  const link = (l: Link[], v: string, forward: boolean) => {
    const e = l.find((x) => x.iri === v);
    if (e) {
      if (forward) e.forward = true;
      else e.inverse = true;
    } else l.push({ iri: v, forward, inverse: !forward });
  };
  const xlOf = new Map<string, { node: Term; kind: LabelKind; forms: Label[] }>();
  for (const row of rows) {
    const p = row.p?.type === 'uri' ? row.p.value : '';
    const o = row.o;
    if (!p || !o) continue;
    const out = row.dir?.value === 'out';
    const v = o.type === 'uri' ? o.value : null;
    const local = p.startsWith(SKOS) ? p.slice(SKOS.length) : null;
    if (out && p === RDF_TYPE && v) {
      if (!d.types.includes(v)) d.types.push(v);
      continue;
    }
    if (out && p.startsWith(XL) && (o.type === 'uri' || o.type === 'bnode')) {
      const kind = p.slice(XL.length).replace('Label', '') as LabelKind;
      if (!LABEL_KINDS.includes(kind)) {
        d.other++;
        continue;
      }
      const k = `${kind} ${termId(o)}`;
      const e = xlOf.get(k) ?? { node: o, kind, forms: [] };
      const f = labelOf(row.xf);
      if (f && !e.forms.some((x) => sameLabel(x, f))) e.forms.push(f);
      xlOf.set(k, e);
      continue;
    }
    if (out && local && ['prefLabel', 'altLabel', 'hiddenLabel'].includes(local)) {
      const l = labelOf(o);
      if (l) d.labels.push({ kind: local.replace('Label', '') as LabelKind, label: l, xl: null });
      continue;
    }
    if (out && local === 'notation' && o.type === 'literal') {
      d.notations.push(o.value);
      continue;
    }
    if (!v || !local) {
      if (out) d.other++;
      continue;
    }
    switch (local) {
      case 'broader':
        link(out ? d.broader : d.narrower, v, out);
        break;
      case 'narrower':
        link(out ? d.narrower : d.broader, v, out);
        break;
      case 'related':
        link(d.related, v, out);
        break;
      case 'inScheme':
        if (out && !d.inScheme.includes(v)) d.inScheme.push(v);
        break;
      case 'topConceptOf':
        link(out ? d.topConceptOf : d.topConcepts, v, out);
        break;
      case 'hasTopConcept':
        link(out ? d.topConcepts : d.topConceptOf, v, out);
        break;
      case 'member':
        if (out) d.members.push(v);
        else if (!d.memberOf.includes(v)) d.memberOf.push(v);
        break;
      default:
        if (MAPPINGS.includes(local) && out) d.mappings.push({ property: local, target: v });
        else if (out) d.other++;
    }
  }
  d.xl = [...xlOf.values()];
  for (const e of d.xl)
    for (const f of e.forms) d.labels.push({ kind: e.kind, label: f, xl: e.node });
  return d;
}

export const isConcept = (d: Details) =>
  d.types.includes(CONCEPT) ||
  (!d.types.length && (d.broader.length > 0 || d.narrower.length > 0 || d.inScheme.length > 0));
export const isScheme = (d: Details) => d.types.includes(SCHEME) || d.topConcepts.length > 0;
export const isCollection = (d: Details) =>
  d.types.includes(COLLECTION) || d.types.includes(ORDERED);

// --- updates -------------------------------------------------------------------------

/** An edit of the thesaurus: the SPARQL update the preview runs and a summary. */
export type SkosEdit = { sparql: string; label: string };

const t = (s0: string, p: string, o: string | Label): HeaderTriple => ({
  s: s0,
  p,
  o: typeof o === 'string' ? { iri: o } : labelLit(o),
});

/** Ground triples removed and added in one graph. */
export function triplesEdit(
  remove: HeaderTriple[],
  add: HeaderTriple[],
  graph: string | null,
): string {
  return dataUpdate(remove, add, graph).sparql;
}

/** The two triples of a hierarchical link: `c skos:broader p` and `p skos:narrower c`. */
export const broaderPair = (c: string, p: string): HeaderTriple[] => [
  t(c, SKOS + 'broader', p),
  t(p, SKOS + 'narrower', c),
];
/** The two triples of a top concept: `c skos:topConceptOf s` and `s skos:hasTopConcept c`. */
export const topPair = (c: string, scheme: string): HeaderTriple[] => [
  t(c, SKOS + 'topConceptOf', scheme),
  t(scheme, SKOS + 'hasTopConcept', c),
];
/** Both directions of `skos:related`, which is symmetric. */
export const relatedPair = (a: string, b: string): HeaderTriple[] => [
  t(a, SKOS + 'related', b),
  t(b, SKOS + 'related', a),
];

/** Where a concept hangs in the tree: under a broader concept, or at the top of a scheme. */
export type Place = { parent: string } | { top: string } | null;

const placeTriples = (c: string, at: Place): HeaderTriple[] =>
  !at ? [] : 'parent' in at ? broaderPair(c, at.parent) : topPair(c, at.top);

/**
 * Move `c` from one place to another: the links of the old place go, in both directions,
 * and those of the new place come. With `copy` the old place stays, which adds a parent.
 */
export function moveEdit(c: string, from: Place, to: Place, copy: boolean, graph: string | null) {
  return triplesEdit(copy ? [] : placeTriples(c, from), placeTriples(c, to), graph);
}

/** A new concept with its preferred label, in a scheme and at a place. */
export function createConceptEdit(
  c: string,
  label: Label,
  o: { scheme: string | null; at: Place },
  graph: string | null,
): string {
  const add = [t(c, RDF_TYPE, CONCEPT), t(c, SKOS + 'prefLabel', label)];
  if (o.scheme) add.push(t(c, SKOS + 'inScheme', o.scheme));
  add.push(...placeTriples(c, o.at));
  return triplesEdit([], add, graph);
}

/** A new concept scheme, or collection, with its preferred label. */
export function createTypedEdit(r: string, type: string, label: Label, graph: string | null) {
  return triplesEdit([], [t(r, RDF_TYPE, type), t(r, SKOS + 'prefLabel', label)], graph);
}

/**
 * The removal of a resource: every triple of the graph that has it as subject or object,
 * except as a member of an `rdf:List` (removing `rdf:first` would break the list), and the
 * triples of the SKOS-XL label resources no other resource of the graph refers to.
 */
export function deleteEdit(r: string, graph: string | null): string {
  const R = iri(r);
  const where = `{ ${R} ?p ?o BIND(${R} AS ?s) }
    UNION { ?s ?p ${R} BIND(${R} AS ?o) FILTER(?p != ${sparqlIri(RDF + 'first')}) }
    UNION { ${R} ${x('prefLabel')}|${x('altLabel')}|${x('hiddenLabel')} ?s . ?s ?p ?o
      FILTER NOT EXISTS { ?other ?q ?s FILTER(!sameTerm(?other, ${R})) } }`;
  return `DELETE { ${inGraph(graph, '?s ?p ?o')} }\nWHERE { ${inGraph(graph, where)} }`;
}

export function labelEdit(
  r: string,
  remove: { kind: LabelKind; label: Label }[],
  add: { kind: LabelKind; label: Label }[],
  graph: string | null,
): string {
  const tr = (e: { kind: LabelKind; label: Label }) => t(r, labelProperty(e.kind), e.label);
  return triplesEdit(remove.map(tr), add.map(tr), graph);
}

/**
 * Make an entry's literal the preferred label of its language. Every alternative and
 * hidden representation of that literal becomes preferred, plain or SKOS-XL as it was, so
 * the literal is left in one kind, and the language's other preferred labels become
 * alternative labels in their own form. A SKOS-XL label keeps its resource and changes
 * the property that links it.
 */
export function makePreferredEdit(
  r: string,
  entry: LabelEntry,
  entries: LabelEntry[],
  graph: string | null,
): string {
  const parts: string[] = [];
  const plainDel: { kind: LabelKind; label: Label }[] = [];
  const plainAdd: { kind: LabelKind; label: Label }[] = [];
  for (const e of entries) {
    const promote = sameLabel(e.label, entry.label) && e.kind !== 'pref';
    const demote =
      e.kind === 'pref' && e.label.lang === entry.label.lang && !sameLabel(e.label, entry.label);
    if (!promote && !demote) continue;
    const to: LabelKind = promote ? 'pref' : 'alt';
    if (e.xl) parts.push(xlKindEdit(r, e.kind, to, e.xl, e.label, graph));
    else {
      plainDel.push({ kind: e.kind, label: e.label });
      plainAdd.push({ kind: to, label: e.label });
    }
  }
  if (plainDel.length) parts.unshift(labelEdit(r, plainDel, plainAdd, graph));
  return parts.join(' ;\n');
}

/** The labels after {@link makePreferredEdit}, to check them before the edit is offered. */
export function preferredResult(entry: LabelEntry, entries: LabelEntry[]): LabelEntry[] {
  return entries.map((e) => {
    if (sameLabel(e.label, entry.label) && e.kind !== 'pref') return { ...e, kind: 'pref' };
    if (e.kind === 'pref' && e.label.lang === entry.label.lang && !sameLabel(e.label, entry.label))
      return { ...e, kind: 'alt' };
    return e;
  });
}

/** Two entries are the same representation: one kind, one literal and one XL resource. */
export function sameEntry(a: LabelEntry, b: LabelEntry): boolean {
  return (
    a.kind === b.kind &&
    sameLabel(a.label, b.label) &&
    (a.xl === null) === (b.xl === null) &&
    (a.xl === null || (a.xl.type === b.xl!.type && a.xl.value === b.xl!.value))
  );
}

/**
 * The SKOS-XL entries that an update cannot tell apart: blank-node labels of one kind with
 * one literal form, which a pattern finds only by that form.
 */
export function ambiguousXl(entries: LabelEntry[], e: LabelEntry): boolean {
  if (e.xl?.type !== 'bnode') return false;
  return entries.some(
    (o) =>
      o !== e &&
      o.xl?.type === 'bnode' &&
      o.xl.value !== e.xl!.value &&
      o.kind === e.kind &&
      sameLabel(o.label, e.label),
  );
}

/** A new IRI for a SKOS-XL label of `r`, unused among `taken`. */
export function xlLabelIri(r: string, kind: LabelKind, label: Label, taken: Set<string>): string {
  const base = `${r}${/[#/]$/.test(r) ? '' : '-'}xl-${kind}${label.lang ? `-${label.lang}` : ''}`;
  if (!taken.has(base)) return base;
  for (let i = 2; ; i++) if (!taken.has(`${base}-${i}`)) return `${base}-${i}`;
}

/** A plain label made a SKOS-XL label resource with that literal form. */
export function toXlEdit(
  r: string,
  kind: LabelKind,
  label: Label,
  node: string,
  graph: string | null,
): string {
  return triplesEdit(
    [t(r, labelProperty(kind), label)],
    [t(r, xlProperty(kind), node), t(node, RDF_TYPE, XL_LABEL), t(node, LITERAL_FORM, label)],
    graph,
  );
}

/**
 * The pattern of one SKOS-XL label of `r`: an IRI, or a blank node found by its literal
 * form, bound to `?xl`.
 */
function xlPattern(r: string, kind: LabelKind, node: Term, form: Label | null): string {
  const link = `${iri(r)} ${x(`${kind}Label`)} ?xl .`;
  if (node.type === 'uri') return `${link} FILTER(sameTerm(?xl, ${iri(node.value)}))`;
  return `${link} FILTER(isBlank(?xl))${form ? ` ?xl ${x('literalForm')} ${litSparql(form)} .` : ''}`;
}

/**
 * A SKOS-XL label made a plain label, or with `add` null removed: the link to the label
 * resource goes, and so does every triple of the resource unless another resource of the
 * graph still refers to it.
 */
export function fromXlEdit(
  r: string,
  kind: LabelKind,
  node: Term,
  form: Label | null,
  add: Label | null,
  graph: string | null,
): string {
  const link = x(`${kind}Label`);
  const del = `${iri(r)} ${link} ?xl . ?xl ?p ?o .`;
  const ins = add
    ? `INSERT { ${inGraph(graph, `${iri(r)} ${s(`${kind}Label`)} ${litSparql(add)} .`)} }\n`
    : '';
  const shared = `FILTER NOT EXISTS { ?other ?q ?xl FILTER(!(sameTerm(?other, ${iri(r)}) && sameTerm(?q, ${link}))) }`;
  return `DELETE { ${inGraph(graph, del)} }\n${ins}WHERE { ${inGraph(
    graph,
    `${xlPattern(r, kind, node, form)} OPTIONAL { ?xl ?p ?o ${shared} }`,
  )} }`;
}

/** A SKOS-XL label of `r` linked by another kind of label property, its resource kept. */
export function xlKindEdit(
  r: string,
  from: LabelKind,
  to: LabelKind,
  node: Term,
  form: Label,
  graph: string | null,
): string {
  if (node.type === 'uri')
    return triplesEdit(
      [t(r, xlProperty(from), node.value)],
      [t(r, xlProperty(to), node.value)],
      graph,
    );
  return `DELETE { ${inGraph(graph, `${iri(r)} ${x(`${from}Label`)} ?xl .`)} }
INSERT { ${inGraph(graph, `${iri(r)} ${x(`${to}Label`)} ?xl .`)} }
WHERE { ${inGraph(graph, xlPattern(r, from, node, form))} }`;
}

/** The literal form of a SKOS-XL label replaced. */
export function xlFormEdit(
  r: string,
  kind: LabelKind,
  node: Term,
  from: Label,
  to: Label,
  graph: string | null,
): string {
  if (node.type === 'uri')
    return triplesEdit(
      [t(node.value, LITERAL_FORM, from)],
      [t(node.value, LITERAL_FORM, to)],
      graph,
    );
  return `DELETE { ${inGraph(graph, `?xl ${x('literalForm')} ${litSparql(from)} .`)} }
INSERT { ${inGraph(graph, `?xl ${x('literalForm')} ${litSparql(to)} .`)} }
WHERE { ${inGraph(graph, xlPattern(r, kind, node, from))} }`;
}

/** A link of `r` by a SKOS property, with the inverse triple for the hierarchy, top concepts and `related`. */
export function linkTriples(r: string, property: string, target: string): HeaderTriple[] {
  switch (property) {
    case 'broader':
      return broaderPair(r, target);
    case 'narrower':
      return broaderPair(target, r);
    case 'related':
      return relatedPair(r, target);
    case 'topConceptOf':
      return topPair(r, target);
    case 'hasTopConcept':
      return topPair(target, r);
    default:
      return [t(r, SKOS + property, target)];
  }
}

export function linkEdit(
  r: string,
  property: string,
  target: string,
  remove: boolean,
  graph: string | null,
): string {
  const ts = linkTriples(r, property, target);
  return remove ? triplesEdit(ts, [], graph) : triplesEdit([], ts, graph);
}

// --- the tree --------------------------------------------------------------------------

/** What the tree has read: the children of each opened concept and the counts of each node. */
export class ConceptTree {
  roots: string[] = [];
  top = new Set<string>();
  children = new Map<string, string[]>();
  counts = new Map<string, number>();
  /** The parents each node was read under. */
  parents = new Map<string, Set<string>>();
  /** How many broader concepts each node has, read or not: the polyhierarchy badge. */
  ups = new Map<string, number>();
  /** Parents whose children reached the cap. */
  capped = new Set<string>();

  setRoots(rows: Record<string, Term | undefined>[]) {
    this.roots = [];
    this.top = new Set();
    for (const r of rows) {
      if (r.c?.type !== 'uri') continue;
      this.roots.push(r.c.value);
      if (Number(r.top?.value ?? 0) > 0) this.top.add(r.c.value);
      this.counts.set(r.c.value, Number(r.n?.value ?? 0));
      this.ups.set(r.c.value, Number(r.ups?.value ?? 0));
    }
  }

  addChildren(parents: string[], rows: Record<string, Term | undefined>[]) {
    for (const p of parents) this.children.set(p, []);
    for (const r of rows) {
      if (r.p?.type !== 'uri' || r.c?.type !== 'uri') continue;
      const p = r.p.value;
      const c = r.c.value;
      const l = this.children.get(p) ?? [];
      if (!l.includes(c)) l.push(c);
      this.children.set(p, l);
      this.counts.set(c, Number(r.n?.value ?? 0));
      this.ups.set(c, Number(r.ups?.value ?? 0));
      this.parents.set(c, new Set([...(this.parents.get(c) ?? []), p]));
    }
    for (const p of parents) {
      const n = this.children.get(p)?.length ?? 0;
      if (n >= CHILD_CAP) this.capped.add(p);
      else this.capped.delete(p);
      // the count the parent's row read may be stale after an edit
      this.counts.set(p, n);
    }
  }

  loaded = (id: string) => this.children.has(id);
  /** How many narrower concepts a node has, read or not. */
  count = (id: string) => this.children.get(id)?.length ?? this.counts.get(id) ?? 0;
}

/** The ids every tree node is shown under, as the key of the open set. */
export function iriSet(rows: Record<string, Term | undefined>[], v: string): string[] {
  const out: string[] = [];
  for (const r of rows) {
    const t0 = r[v];
    if (t0?.type === 'uri' && !out.includes(t0.value)) out.push(t0.value);
  }
  return out;
}

/** The labels of `rows` from `labelsQuery`, by resource. */
export function labelMap(rows: Record<string, Term | undefined>[]): Map<string, Label[]> {
  const m = new Map<string, Label[]>();
  for (const r of rows) {
    const l = labelOf(r.l);
    if (r.r?.type !== 'uri' || !l) continue;
    const a = m.get(r.r.value) ?? [];
    if (!a.some((x0) => sameLabel(x0, l))) a.push(l);
    m.set(r.r.value, a);
  }
  return m;
}

/** The session branch's prefix for the thesaurus editor. */
export const SESSION_PREFIX = 'skos';

// --- requests ----------------------------------------------------------------------------

let shapesText: Promise<string> | null = null;

/** `GET /$/skos/shapes`: the built-in SKOS shapes as Turtle, read once. */
export function skosShapes(): Promise<string> {
  shapesText ??= request('/$/skos/shapes', { headers: { Accept: 'text/turtle' } })
    .then((r) => r.text())
    .catch((e) => {
      shapesText = null;
      throw e;
    });
  return shapesText;
}

/**
 * The head the editor shows: the entity tag an edit's `If-Match` names and the commit,
 * from a Graph Store `HEAD` of the default graph, which sends no data.
 */
export const headUrl = (target: string, graph: string | null) =>
  `/${encodeURIComponent(target)}/data?${graph ? `graph=${encodeURIComponent(graph)}` : 'default'}`;

/**
 * The commit a target's graph is read at and its entity tag, which `If-Match` names. The
 * tag is dataset-wide, so the HEAD of the graph the editor reads serves every write.
 */
export async function readHead(
  target: string,
  graph: string | null,
  signal?: AbortSignal,
): Promise<{ etag: string | null; commit: number }> {
  const res = await request(headUrl(target, graph), {
    method: 'HEAD',
    cache: 'no-store',
    signal,
  });
  return {
    etag: res.headers.get('ETag'),
    commit: Number(res.headers.get('Sparkles-Commit') ?? 0),
  };
}

/** Jena's name of the stored default graph, which a union default graph does not change. */
export const STORED_DEFAULT_GRAPH = 'urn:x-arq:DefaultGraph';

/**
 * Whether a dataset holds any triple outside `graph` (`null`: the default graph). The
 * default graph is read by its Jena name, which is the stored default graph even when the
 * server makes the unnamed default graph the union of the named graphs.
 */
export function otherGraphsQuery(graph: string | null): string {
  const stored = iri(STORED_DEFAULT_GRAPH);
  return graph
    ? `ASK { { GRAPH ?g { ?s ?p ?o } FILTER(?g != ${iri(graph)} && ?g != ${stored}) }
  UNION { GRAPH ${stored} { ?s ?p ?o } } }`
    : `ASK { GRAPH ?g { ?s ?p ?o } FILTER(?g != ${stored}) }`;
}

/** Any graph of a dataset whose graphs the editor does not know: for the picker. */
export const ANY_GRAPH = '*';

/** The named graphs that hold SKOS concepts or schemes. */
export const SKOS_GRAPHS = `SELECT DISTINCT ?g WHERE { GRAPH ?g { { ?c a ${s('Concept')} } UNION { ?c a ${s('ConceptScheme')} } } } ORDER BY ?g LIMIT 200`;
