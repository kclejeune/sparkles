// The ontology browser (docs/specs/U12-ontology-workbench/SPEC.md, "Ontology browser"):
// the client of `GET /$/ontology/{ds}/hierarchy` and `GET /$/ontology/{ds}/term`
// (docs/API.md, "Ontology"), and the pure functions that turn their answers into the
// sections of an entity's page. The page's components take what these return as props,
// so the editor, DL Query and the term pages of item 461 can reuse them.
import { request, select } from './api';
import {
  iriName,
  OBJECT_CHARACTERISTICS,
  sectionOf,
  SECTIONS,
  type AxiomItem,
  type AxiomJson,
  type EntityKind,
  type Lit,
} from './ontology';
import { WELL_KNOWN, type PrefixMap } from './rdf';

const enc = encodeURIComponent;
const RDF = WELL_KNOWN.rdf;
const RDFS = WELL_KNOWN.rdfs;
const OWL = WELL_KNOWN.owl;
const SKOS = WELL_KNOWN.skos;
const DCT = WELL_KNOWN.dcterms;
const DC = WELL_KNOWN.dc;
const OIO = 'http://www.geneontology.org/formats/oboInOwl#';
const OBO = 'http://purl.obolibrary.org/obo/';

// --- the answers ----------------------------------------------------------------------

/** A value of a statement: an IRI as a string, or a literal. */
export type Value = string | Lit;

export type Name = {
  iri: string;
  label: string | null;
  labelLang: string | null;
  shortForm: string | null;
  kinds: string[];
  deprecated: boolean;
};

export type TreeNode = Name & { children: number; parents: number };

export type TreeKind =
  | 'class'
  | 'objectProperty'
  | 'dataProperty'
  | 'annotationProperty'
  | 'concept';

export type Hierarchy = {
  dataset: string;
  commit: number;
  kind: TreeKind;
  node: TreeNode | null;
  children: TreeNode[];
  total: number;
  offset: number;
  limit: number;
  next: number | null;
  order: 'label' | 'iri';
  roots?: 'ontology' | 'scheme' | 'computed';
  deprecatedRoots?: number;
  ancestors?: { nodes: TreeNode[]; edges: [string, string][]; truncated: boolean };
  warnings?: string[];
};

export type Statement = {
  property: string;
  value: Value;
  graph: string;
  annotations: { property: string; value: Value }[];
};

export type Incoming = { property: string; count: number; more: boolean; subjects: string[] };

export type TermPage = Name & {
  dataset: string;
  commit: number;
  graphs: string[];
  types: string[];
  ontologies: { graph: string; ontology: string | null }[];
  statements: Statement[];
  statementsTruncated: boolean;
  /** The literal forms of the entity's SKOS-XL labels, by the SKOS-XL property. */
  xlLabels?: { property: string; value: Lit; graph: string }[];
  axioms: AxiomItem[];
  mentions: { axioms: AxiomItem[]; total: number; truncated: boolean };
  incoming: Incoming[];
  usage: Incoming | null;
  names: Record<string, Omit<Name, 'iri'>>;
  warnings?: string[];
  /** The answer's entity tag, which an edit's `If-Match` names (set by {@link fetchTerm}). */
  etag?: string | null;
};

/** What the browser reads: one graph (null for the default graph), its imports and a state. */
export type BrowseSelection = { graph: string | null; imports?: boolean; at?: string | null };

function params(
  sel: BrowseSelection,
  more: Record<string, string | number | boolean | null | undefined>,
) {
  const p = new URLSearchParams();
  p.set('graph', sel.graph ?? 'default');
  if (sel.imports === false) p.set('imports', 'false');
  if (sel.at) p.set('at', sel.at);
  for (const [k, v] of Object.entries(more)) if (v !== undefined && v !== null) p.set(k, String(v));
  return p;
}

/** `GET /$/ontology/{ds}/hierarchy`: one level of a hierarchy. */
export async function fetchHierarchy(
  target: string,
  sel: BrowseSelection,
  q: {
    kind?: TreeKind;
    iri?: string | null;
    ancestors?: boolean;
    scheme?: string | null;
    deprecated?: boolean;
    limit?: number;
    offset?: number;
    lang?: readonly string[];
  } = {},
  signal?: AbortSignal,
): Promise<Hierarchy> {
  const p = params(sel, {
    kind: q.kind,
    iri: q.iri,
    ancestors: q.ancestors || undefined,
    scheme: q.scheme,
    deprecated: q.deprecated === false ? false : undefined,
    limit: q.limit,
    offset: q.offset || undefined,
    lang: q.lang?.length ? q.lang.join(',') : undefined,
  });
  const res = await request(`/$/ontology/${enc(target)}/hierarchy?${p}`, {
    headers: { Accept: 'application/json' },
    signal,
  });
  return (await res.json()) as Hierarchy;
}

/** `GET /$/ontology/{ds}/term`: what the graphs say about one entity. */
export async function fetchTerm(
  target: string,
  sel: BrowseSelection,
  iri: string,
  q: { lang?: readonly string[]; mentions?: number; incoming?: number } = {},
  signal?: AbortSignal,
): Promise<TermPage> {
  const p = params(sel, {
    iri,
    mentions: q.mentions,
    incoming: q.incoming,
    lang: q.lang?.length ? q.lang.join(',') : undefined,
  });
  const res = await request(`/$/ontology/${enc(target)}/term?${p}`, {
    headers: { Accept: 'application/json' },
    signal,
  });
  return { ...((await res.json()) as TermPage), etag: res.headers.get('ETag') };
}

export const DOWNLOADS: { format: string; label: string }[] = [
  { format: 'ttl', label: 'Turtle' },
  { format: 'rdfxml', label: 'RDF/XML' },
  { format: 'ofn', label: 'OWL functional' },
  { format: 'jsonld', label: 'JSON-LD' },
];

/** The address of an entity's download in `format`. */
export function downloadHref(target: string, sel: BrowseSelection, iri: string, format: string) {
  return `/$/ontology/${enc(target)}/term?${params(sel, { iri, format })}`;
}

/** The page of `iri` in the browser (the landing page when it is null). */
export function browseHref(
  base: string,
  ds: string,
  iri: string | null,
  extra: Record<string, string | null | undefined> = {},
): string {
  const p = new URLSearchParams({ ds });
  if (iri) p.set('iri', iri);
  for (const [k, v] of Object.entries(extra)) if (v) p.set(k, v);
  return `${base}/browse?${p}`;
}

// --- names ----------------------------------------------------------------------------

/** What a link to `iri` shows: its label, else its short form, else a short IRI. */
export function displayName(
  iri: string,
  names: Record<string, Partial<Omit<Name, 'iri'>>> | undefined,
  prefixes: PrefixMap,
): string {
  const n = names?.[iri];
  return n?.label ?? n?.shortForm ?? iriName(iri, prefixes);
}

const KIND_WORD: Record<string, string> = {
  class: 'class',
  objectProperty: 'object property',
  dataProperty: 'data property',
  annotationProperty: 'annotation property',
  property: 'property',
  individual: 'individual',
  datatype: 'datatype',
  concept: 'concept',
  conceptScheme: 'concept scheme',
  collection: 'collection',
  ontology: 'ontology',
};

/** The kind of an entity in words, from the first kind it has. */
export function kindWord(kinds: readonly string[]): string {
  for (const k of kinds) if (KIND_WORD[k]) return KIND_WORD[k];
  return 'resource';
}

/** The browser's hierarchy for an entity of `kinds`, or null when it has none. */
export function treeKindOf(kinds: readonly string[]): TreeKind | null {
  if (kinds.includes('class')) return 'class';
  if (kinds.includes('concept')) return 'concept';
  if (kinds.includes('objectProperty')) return 'objectProperty';
  if (kinds.includes('dataProperty')) return 'dataProperty';
  if (kinds.includes('annotationProperty')) return 'annotationProperty';
  return null;
}

/** The editor's entity kind of `kinds`, for the frame sections of its axioms. */
export function editorKindOf(kinds: readonly string[]): EntityKind | null {
  if (kinds.includes('class')) return 'Class';
  if (kinds.includes('objectProperty')) return 'ObjectProperty';
  if (kinds.includes('dataProperty')) return 'DataProperty';
  if (kinds.includes('annotationProperty')) return 'AnnotationProperty';
  if (kinds.includes('datatype')) return 'Datatype';
  if (kinds.includes('individual') || kinds.includes('concept')) return 'NamedIndividual';
  return null;
}

/** Where Edit opens an entity: the thesaurus editor for SKOS, the ontology editor otherwise. */
export function editHref(
  base: string,
  ds: string,
  iri: string,
  kinds: readonly string[],
  sel: BrowseSelection,
) {
  const p = new URLSearchParams({ ds, iri });
  if (sel.graph) p.set('graph', sel.graph);
  if (kinds.some((k) => k === 'concept' || k === 'conceptScheme' || k === 'collection'))
    return `${base}/thesaurus?${p}`;
  const tab: Record<string, string> = {
    objectProperty: 'ObjectProperty',
    dataProperty: 'DataProperty',
    annotationProperty: 'AnnotationProperty',
    individual: 'NamedIndividual',
  };
  const t = kinds.find((k) => k in tab);
  if (t && !kinds.includes('class')) p.set('tab', tab[t]);
  return `${base}/ontology?${p}`;
}

// --- the sections of a page -----------------------------------------------------------

export const LABEL_PROPS = [RDFS + 'label', SKOS + 'prefLabel'];
export const DEFINITION_PROPS = [OBO + 'IAO_0000115', SKOS + 'definition'];

/** Synonym properties, in display order, with their kind. */
export const SYNONYM_KINDS: [string, string][] = [
  [OIO + 'hasExactSynonym', 'Exact'],
  [OIO + 'hasRelatedSynonym', 'Related'],
  [OIO + 'hasBroadSynonym', 'Broad'],
  [OIO + 'hasNarrowSynonym', 'Narrow'],
  [OIO + 'hasSynonym', 'Synonym'],
  [SKOS + 'altLabel', 'Alternative'],
  [SKOS + 'hiddenLabel', 'Hidden'],
];

export const NOTE_KINDS: [string, string][] = [
  [RDFS + 'comment', 'Comment'],
  [SKOS + 'scopeNote', 'Scope note'],
  [SKOS + 'note', 'Note'],
  [SKOS + 'example', 'Example'],
  [SKOS + 'historyNote', 'History note'],
  [SKOS + 'editorialNote', 'Editorial note'],
  [SKOS + 'changeNote', 'Change note'],
  [DCT + 'description', 'Description'],
  [DC + 'description', 'Description'],
];

export const MAPPING_KINDS: [string, string][] = [
  [SKOS + 'exactMatch', 'Exact match'],
  [SKOS + 'closeMatch', 'Close match'],
  [SKOS + 'broadMatch', 'Broad match'],
  [SKOS + 'narrowMatch', 'Narrow match'],
  [SKOS + 'relatedMatch', 'Related match'],
];

export const XREF_PROPS = [OIO + 'hasDbXref'];
export const SEE_ALSO_PROPS = [RDFS + 'seeAlso', RDFS + 'isDefinedBy'];

/** The facts an infobox shows, in order. */
export const INFO_KINDS: [string, string][] = [
  [OIO + 'id', 'ID'],
  [SKOS + 'notation', 'Notation'],
  [OIO + 'hasOBONamespace', 'Namespace'],
  [SKOS + 'inScheme', 'Scheme'],
  [SKOS + 'topConceptOf', 'Top concept of'],
  [OIO + 'inSubset', 'Subsets'],
  [OBO + 'IAO_0100001', 'Replaced by'],
  [OIO + 'consider', 'Consider'],
  [OIO + 'created_by', 'Created by'],
  [OIO + 'creation_date', 'Created'],
  [DCT + 'created', 'Created'],
  [DCT + 'modified', 'Modified'],
  [OWL + 'versionInfo', 'Version'],
];

/** The vocabularies whose properties state axioms, which the logical definition shows. */
const LOGICAL = [RDF, RDFS, OWL];

/** Properties whose values are shown in a section of their own, or not at all. */
const SHOWN = new Set([
  RDF + 'type',
  OWL + 'deprecated',
  ...LABEL_PROPS,
  ...DEFINITION_PROPS,
  ...SYNONYM_KINDS.map(([p]) => p),
  ...NOTE_KINDS.map(([p]) => p),
  ...MAPPING_KINDS.map(([p]) => p),
  ...XREF_PROPS,
  ...SEE_ALSO_PROPS,
  ...INFO_KINDS.map(([p]) => p),
  SKOS + 'related',
  SKOS + 'broader',
  SKOS + 'narrower',
  SKOS + 'member',
  SKOS + 'hasTopConcept',
  'http://www.w3.org/2008/05/skos-xl#prefLabel',
  'http://www.w3.org/2008/05/skos-xl#altLabel',
  'http://www.w3.org/2008/05/skos-xl#hiddenLabel',
]);

export const isLiteral = (v: Value): v is Lit => typeof v === 'object' && v !== null;

/** The text of a value: a literal's lexical form, or the IRI. */
export const valueText = (v: Value) => (isLiteral(v) ? v['@value'] : v);

/** A value with the sources its annotations name (`oboInOwl:hasDbXref` or `dc:source`). */
export type Sourced = { value: Value; lang: string | null; sources: string[]; property: string };

const SOURCE_PROPS = new Set([OIO + 'hasDbXref', DC + 'source', DCT + 'source', OIO + 'source']);

function sourced(s: Statement): Sourced {
  return {
    value: s.value,
    property: s.property,
    lang: isLiteral(s.value) ? (s.value['@language'] ?? null) : null,
    sources: s.annotations
      .filter((a) => SOURCE_PROPS.has(a.property))
      .map((a) => valueText(a.value)),
  };
}

/** Rank of a language among the caller's: theirs first, then none, then English, then the rest. */
export function langRank(lang: string | null, langs: readonly string[]): number {
  const l = (lang ?? '').toLowerCase();
  const i = langs.findIndex((x) => {
    const w = x.toLowerCase();
    return w === l || l.startsWith(w + '-') || w.split('-')[0] === l;
  });
  if (i >= 0) return i;
  if (!l) return langs.length;
  if (l === 'en' || l.startsWith('en-')) return langs.length + 1;
  return langs.length + 2;
}

const byLang = (langs: readonly string[]) => (a: Sourced, b: Sourced) =>
  langRank(a.lang, langs) - langRank(b.lang, langs) ||
  valueText(a.value).localeCompare(valueText(b.value));

export type Sections = {
  /** the definition in the best language, with its sources */
  definition: Sourced | null;
  /** the definitions in the other languages */
  otherDefinitions: Sourced[];
  synonyms: { kind: string; values: Sourced[] }[];
  notes: { kind: string; values: Sourced[] }[];
  /** preferred, alternative and hidden labels by language, for SKOS resources */
  labels: { lang: string; preferred: string[]; alternative: string[]; hidden: string[] }[];
  xrefs: { database: string; id: string; href: string | null; value: string }[];
  seeAlso: { property: string; value: Value }[];
  mappings: { kind: string; values: string[] }[];
  related: string[];
  members: string[];
  info: { kind: string; values: Value[] }[];
  /** every other statement, by property */
  other: { property: string; values: Value[] }[];
};

/** Each value once, by its text and language, keeping the sources of every copy. */
function uniqueSourced(list: Sourced[]): Sourced[] {
  const by = new Map<string, Sourced>();
  for (const v of list) {
    const k = `${isLiteral(v.value) ? 'l' : 'i'}\u0000${valueText(v.value)}\u0000${v.lang ?? ''}`;
    const was = by.get(k);
    if (!was) by.set(k, { ...v, sources: [...v.sources] });
    else for (const src of v.sources) if (!was.sources.includes(src)) was.sources.push(src);
  }
  return [...by.values()];
}

/** Each value once, by its kind, text, language and datatype. */
function uniqueValues(list: Value[]): Value[] {
  const seen = new Set<string>();
  return list.filter((v) => {
    const k = isLiteral(v)
      ? `l\u0000${v['@value']}\u0000${v['@language'] ?? ''}\u0000${v['@type'] ?? ''}`
      : `i\u0000${v}`;
    if (seen.has(k)) return false;
    seen.add(k);
    return true;
  });
}

const unique = <T>(list: T[]) => [...new Set(list)];

const groupsOf = (kinds: [string, string][], stmts: Statement[], langs: readonly string[]) =>
  kinds
    .map(([p, kind]) => ({
      kind,
      values: uniqueSourced(stmts.filter((s) => s.property === p).map(sourced)).sort(byLang(langs)),
    }))
    .filter((g) => g.values.length > 0);

const SKOSXL = 'http://www.w3.org/2008/05/skos-xl#';
/** The plain SKOS property that each SKOS-XL label property stands for in the sections. */
const XL_PLAIN: Record<string, string> = {
  [SKOSXL + 'prefLabel']: SKOS + 'prefLabel',
  [SKOSXL + 'altLabel']: SKOS + 'altLabel',
  [SKOSXL + 'hiddenLabel']: SKOS + 'hiddenLabel',
};

/**
 * Sort a page's statements into its sections, in the caller's languages. A value that
 * several graphs state comes once, and the literal forms of SKOS-XL labels count as the
 * plain SKOS labels they stand for.
 */
export function termSections(
  t: Pick<TermPage, 'statements'> & Partial<Pick<TermPage, 'names' | 'xlLabels'>>,
  langs: readonly string[],
): Sections {
  const stmts: Statement[] = [
    ...t.statements,
    ...(t.xlLabels ?? []).flatMap((x) =>
      XL_PLAIN[x.property]
        ? [{ property: XL_PLAIN[x.property], value: x.value, graph: x.graph, annotations: [] }]
        : [],
    ),
  ];
  const defs = uniqueSourced(
    stmts.filter((s) => DEFINITION_PROPS.includes(s.property)).map(sourced),
  ).sort(byLang(langs));
  const labelRows = new Map<
    string,
    { preferred: string[]; alternative: string[]; hidden: string[] }
  >();
  const row = (lang: string) => {
    let r = labelRows.get(lang);
    if (!r) labelRows.set(lang, (r = { preferred: [], alternative: [], hidden: [] }));
    return r;
  };
  const push = (l: string[], v: string) => l.includes(v) || l.push(v);
  for (const s of stmts) {
    if (!isLiteral(s.value)) continue;
    const lang = s.value['@language'] ?? '';
    if (s.property === SKOS + 'prefLabel') push(row(lang).preferred, s.value['@value']);
    else if (s.property === SKOS + 'altLabel') push(row(lang).alternative, s.value['@value']);
    else if (s.property === SKOS + 'hiddenLabel') push(row(lang).hidden, s.value['@value']);
  }
  const labels = [...labelRows]
    .filter(([, r]) => r.preferred.length || r.alternative.length || r.hidden.length)
    .map(([lang, r]) => ({ lang, ...r }))
    .sort(
      (a, b) =>
        langRank(a.lang || null, langs) - langRank(b.lang || null, langs) ||
        a.lang.localeCompare(b.lang),
    );
  const xrefs = unique(
    stmts.filter((s) => XREF_PROPS.includes(s.property)).map((s) => valueText(s.value)),
  )
    .map((v) => {
      const i = v.indexOf(':');
      const database = i > 0 && !/^https?:/.test(v) ? v.slice(0, i) : '';
      const id = database ? v.slice(i + 1) : v;
      return { database, id, href: xrefHref(v), value: v };
    })
    .sort((a, b) => a.database.localeCompare(b.database) || a.id.localeCompare(b.id));
  const iris = (p: string) =>
    unique(
      stmts.filter((s) => s.property === p && !isLiteral(s.value)).map((s) => s.value as string),
    );
  const otherMap = new Map<string, Value[]>();
  const facts = new Set(['objectProperty', 'dataProperty']);
  for (const s of stmts) {
    if (SHOWN.has(s.property) || LOGICAL.some((ns) => s.property.startsWith(ns))) continue;
    // the facts of an individual are axioms, which the logical definition shows
    if (t.names?.[s.property]?.kinds.some((k) => facts.has(k))) continue;
    const l = otherMap.get(s.property) ?? [];
    l.push(s.value);
    otherMap.set(s.property, l);
  }
  const seeAlso = new Map<string, { property: string; value: Value }>();
  for (const s of stmts)
    if (SEE_ALSO_PROPS.includes(s.property))
      seeAlso.set(`${s.property}\u0000${valueText(s.value)}`, {
        property: s.property,
        value: s.value,
      });
  return {
    definition: defs[0] ?? null,
    otherDefinitions: defs.slice(1),
    synonyms: groupsOf(SYNONYM_KINDS, stmts, langs),
    notes: groupsOf(NOTE_KINDS, stmts, langs),
    labels,
    xrefs,
    seeAlso: [...seeAlso.values()],
    mappings: MAPPING_KINDS.map(([p, kind]) => ({ kind, values: iris(p) })).filter(
      (g) => g.values.length > 0,
    ),
    related: iris(SKOS + 'related'),
    members: iris(SKOS + 'member'),
    info: INFO_KINDS.map(([p, kind]) => ({
      kind,
      values: uniqueValues(stmts.filter((s) => s.property === p).map((s) => s.value)),
    })).filter((g) => g.values.length > 0),
    other: [...otherMap].map(([property, values]) => ({ property, values: uniqueValues(values) })),
  };
}

/** Where a database cross-reference such as `Wikipedia:Mitochondrion` resolves, if known. */
export function xrefHref(xref: string): string | null {
  if (/^https?:\/\/\S+$/.test(xref)) return xref;
  const i = xref.indexOf(':');
  if (i <= 0) return null;
  const db = xref.slice(0, i).toLowerCase();
  const id = xref.slice(i + 1);
  if (!id) return null;
  const at: Record<string, (id: string) => string> = {
    wikipedia: (x) => `https://en.wikipedia.org/wiki/${enc(x)}`,
    pmid: (x) => `https://pubmed.ncbi.nlm.nih.gov/${enc(x)}/`,
    doi: (x) => `https://doi.org/${x}`,
    pmcid: (x) => `https://www.ncbi.nlm.nih.gov/pmc/articles/${enc(x)}/`,
    wikidata: (x) => `https://www.wikidata.org/wiki/${enc(x)}`,
    orcid: (x) => `https://orcid.org/${enc(x)}`,
  };
  return at[db]?.(id) ?? null;
}

/** The CURIEs like `GO:0032047` in a text, with the IRIs they stand for when `prefixes` know them. */
export function linkCuries(
  text: string,
  curieIri: (curie: string) => string | null,
): { text: string; iri?: string }[] {
  const out: { text: string; iri?: string }[] = [];
  const re = /\b([A-Za-z][A-Za-z0-9_]*):(\d{3,})\b/g;
  let last = 0;
  for (const m of text.matchAll(re)) {
    const iri = curieIri(m[0]);
    if (!iri) continue;
    if (m.index > last) out.push({ text: text.slice(last, m.index) });
    out.push({ text: m[0], iri });
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push({ text: text.slice(last) });
  return out;
}

/** The OBO PURL of a CURIE such as `GO:0005739`. */
export const oboIri = (curie: string) => {
  const m = /^([A-Za-z][A-Za-z0-9_]*):(\d+)$/.exec(curie);
  return m ? `${OBO}${m[1]}_${m[2]}` : null;
};

// --- axioms ---------------------------------------------------------------------------

export type AxiomGroup = {
  id: string;
  title: string;
  /** each axiom with what it says of the entity, or the word of a characteristic */
  items: { values: unknown[]; axiom: AxiomItem; word?: string }[];
};

const CHARACTERISTIC_WORDS = new Map<string, string>([
  ...OBJECT_CHARACTERISTICS,
  ['FunctionalDataProperty', 'Functional'],
]);

/** Axiom types that the page shows through its statements instead. */
const STATEMENT_AXIOMS = new Set(['Declaration', 'AnnotationAssertion']);

/**
 * The axioms about the entity in the sections of the editor's frame, in its order, then
 * those that fit no section. Sections the hierarchy tree already shows downwards
 * ("Superclass Of") are left to the tree.
 */
export function axiomGroups(
  iri: string,
  kinds: readonly string[],
  axioms: AxiomItem[],
): AxiomGroup[] {
  const kind = editorKindOf(kinds);
  const titles = new Map((kind ? SECTIONS[kind] : []).map(([id, title]) => [id, title]));
  const groups = new Map<string, AxiomGroup>();
  const other: AxiomGroup = { id: 'other', title: 'Other axioms', items: [] };
  const traits: AxiomGroup = { id: 'characteristics', title: 'Characteristics', items: [] };
  for (const it of axioms) {
    if (STATEMENT_AXIOMS.has(it.type)) continue;
    const word = CHARACTERISTIC_WORDS.get(it.type);
    if (word && (it.axiom as AxiomJson).property === iri) {
      traits.items.push({ values: [], axiom: it, word });
      continue;
    }
    const hit = kind ? sectionOf(kind, iri, it.axiom as AxiomJson) : null;
    if (!hit) {
      other.items.push({ values: [it.axiom], axiom: it });
      continue;
    }
    const [id, values] = hit;
    if (id === 'superOf' || id === 'subProperties' || id === 'instances') continue;
    let g = groups.get(id);
    if (!g) groups.set(id, (g = { id, title: sentenceCase(titles.get(id) ?? id), items: [] }));
    g.items.push({ values, axiom: it });
  }
  const order = kind ? SECTIONS[kind].map(([id]) => id) : [];
  const out = [...groups.values()].sort((a, b) => order.indexOf(a.id) - order.indexOf(b.id));
  if (traits.items.length) out.push(traits);
  if (other.items.length) out.push(other);
  return out;
}

/** "SubClass Of" as "Subclass of". */
export function sentenceCase(s: string): string {
  const lower = s.toLowerCase();
  return lower.charAt(0).toUpperCase() + lower.slice(1);
}

export type MentionGroup = {
  /** the property of the restriction that holds the entity, when one does */
  property: string | null;
  /** the restriction's keyword, such as `some` */
  word: string | null;
  /** the entities whose axiom it is, for a restriction group */
  subjects: string[];
  /** the whole axioms, for those that fit no restriction group */
  axioms: AxiomItem[];
};

const RESTRICTION_WORD: Record<string, string> = {
  ObjectSomeValuesFrom: 'some',
  ObjectAllValuesFrom: 'only',
  ObjectHasValue: 'value',
};

/**
 * The axioms of other entities that mention `iri`, grouped by the restriction that holds
 * it: "part of some mitochondrion" lists every class that is a subclass of it. Axioms of
 * any other form are listed whole in a last group.
 */
export function mentionGroups(iri: string, mentions: AxiomItem[]): MentionGroup[] {
  const groups = new Map<string, MentionGroup>();
  const rest: AxiomItem[] = [];
  for (const it of mentions) {
    const a = it.axiom as AxiomJson;
    const sup = a.superClass as Record<string, unknown> | undefined;
    const word = sup && RESTRICTION_WORD[String(sup.type)];
    const filler = sup?.class ?? sup?.individual;
    if (
      a.type === 'SubClassOf' &&
      typeof a.subClass === 'string' &&
      word &&
      filler === iri &&
      typeof sup.property === 'string'
    ) {
      const key = `${sup.property}|${word}`;
      let g = groups.get(key);
      if (!g) groups.set(key, (g = { property: sup.property, word, subjects: [], axioms: [] }));
      if (!g.subjects.includes(a.subClass)) g.subjects.push(a.subClass);
      g.axioms.push(it);
    } else rest.push(it);
  }
  const out = [...groups.values()];
  if (rest.length) out.push({ property: null, word: null, subjects: [], axioms: rest });
  return out;
}

// --- the hierarchy --------------------------------------------------------------------

const nodeOrder = (a: TreeNode, b: TreeNode) =>
  Number(a.deprecated) - Number(b.deprecated) ||
  (a.label ?? a.shortForm ?? a.iri).localeCompare(b.label ?? b.shortForm ?? b.iri);

/**
 * The lineage of `iri` from its ancestors: one path from a root down to its parent,
 * following at each step the first parent in label order (deprecated ones last), and the
 * entity's other direct parents. A cycle ends the path.
 */
export function lineage(
  iri: string,
  anc: { nodes: TreeNode[]; edges: [string, string][] } | undefined,
): { spine: TreeNode[]; otherParents: TreeNode[] } {
  if (!anc) return { spine: [], otherParents: [] };
  const byIri = new Map(anc.nodes.map((n) => [n.iri, n]));
  const parents = new Map<string, TreeNode[]>();
  for (const [c, p] of anc.edges) {
    const n = byIri.get(p);
    if (!n) continue;
    const l = parents.get(c) ?? [];
    if (!l.some((x) => x.iri === p)) l.push(n);
    parents.set(c, l);
  }
  for (const l of parents.values()) l.sort(nodeOrder);
  const direct = parents.get(iri) ?? [];
  const path: TreeNode[] = [];
  const seen = new Set([iri]);
  let at = direct[0];
  while (at && !seen.has(at.iri)) {
    path.push(at);
    seen.add(at.iri);
    at = (parents.get(at.iri) ?? []).find((p) => !seen.has(p.iri)) as TreeNode;
  }
  return { spine: path.reverse(), otherParents: direct.slice(1) };
}

// --- the graph view -------------------------------------------------------------------

export type GraphPiece = {
  nodes: { id: string; label: string; focus?: boolean }[];
  edges: { id: string; source: string; target: string; label: string }[];
};

/**
 * The neighbourhood of an entity for the graph view: its named superclasses and the
 * fillers of its restrictions, the entities whose restrictions hold it, its first
 * subclasses and the parents in its lineage, as nodes and labelled edges.
 */
export function neighbourhood(
  t: Pick<TermPage, 'iri' | 'axioms' | 'mentions' | 'incoming' | 'names'> &
    Partial<Pick<TermPage, 'label' | 'shortForm'>>,
  prefixes: PrefixMap,
  maxChildren = 20,
): GraphPiece {
  const nodes = new Map<string, { id: string; label: string; focus?: boolean }>();
  const edges = new Map<string, { id: string; source: string; target: string; label: string }>();
  const name = (iri: string) =>
    iri === t.iri && (t.label || t.shortForm)
      ? (t.label ?? t.shortForm)!
      : displayName(iri, t.names, prefixes);
  const node = (iri: string, focus = false) => {
    if (!nodes.has(iri)) nodes.set(iri, { id: iri, label: name(iri), focus: focus || undefined });
  };
  const edge = (s: string, o: string, label: string) => {
    node(s);
    node(o);
    const id = `${s} ${label} ${o}`;
    edges.set(id, { id, source: s, target: o, label });
  };
  node(t.iri, true);
  const relate = (sub: string, sup: unknown) => {
    if (typeof sup === 'string') return edge(sub, sup, 'subclass of');
    const r = sup as Record<string, unknown> | null;
    const word = r && RESTRICTION_WORD[String(r.type)];
    const filler = r?.class ?? r?.individual;
    if (word && typeof r.property === 'string' && typeof filler === 'string')
      edge(sub, filler, name(r.property));
  };
  for (const it of t.axioms) {
    const a = it.axiom as AxiomJson;
    if (a.type === 'SubClassOf' && a.subClass === t.iri) relate(t.iri, a.superClass);
    if (a.type === 'EquivalentClasses')
      for (const c of (a.classes as unknown[]) ?? []) {
        if (typeof c === 'string' && c !== t.iri) edge(t.iri, c, 'equivalent to');
        const parts = (c as Record<string, unknown>)?.classes as unknown[] | undefined;
        for (const p of parts ?? []) relate(t.iri, p);
      }
  }
  for (const it of t.mentions.axioms) {
    const a = it.axiom as AxiomJson;
    if (a.type === 'SubClassOf' && typeof a.subClass === 'string') relate(a.subClass, a.superClass);
  }
  const subs = t.incoming.find((i) => i.property === RDFS + 'subClassOf');
  for (const s of (subs?.subjects ?? []).slice(0, maxChildren)) edge(s, t.iri, 'subclass of');
  const broader = t.incoming.find((i) => i.property === SKOS + 'broader');
  for (const s of (broader?.subjects ?? []).slice(0, maxChildren)) edge(s, t.iri, 'broader');
  return { nodes: [...nodes.values()], edges: [...edges.values()] };
}

/** Merge `b` into `a`, keeping `a`'s focus. */
export function mergePieces(a: GraphPiece, b: GraphPiece): GraphPiece {
  const nodes = new Map(a.nodes.map((n) => [n.id, n]));
  for (const n of b.nodes) if (!nodes.has(n.id)) nodes.set(n.id, { ...n, focus: undefined });
  const edges = new Map(a.edges.map((e) => [e.id, e]));
  for (const e of b.edges) edges.set(e.id, e);
  return { nodes: [...nodes.values()], edges: [...edges.values()] };
}

// --- the landing page and other datasets -----------------------------------------------

export type OntologyHead = {
  iri: string | null;
  title: string | null;
  description: string | null;
  version: string | null;
  versionIri: string | null;
  license: string | null;
  /** the other annotations of the header, by property */
  annotations: { property: string; value: Value }[];
};

const TITLE_PROPS = [DCT + 'title', DC + 'title', RDFS + 'label', SKOS + 'prefLabel'];
const DESCRIPTION_PROPS = [
  DCT + 'description',
  DC + 'description',
  RDFS + 'comment',
  SKOS + 'definition',
];
const LICENSE_PROPS = [DCT + 'license', 'http://creativecommons.org/ns#license'];

/** The SPARQL of the headers of a graph: its ontologies and concept schemes with their annotations. */
export function headQuery(graph: string | null): string {
  const body = `{ ?o a ?t . VALUES ?t { <${OWL}Ontology> <${SKOS}ConceptScheme> } ?o ?p ?v }`;
  const g = graph ? `GRAPH <${graph}> ${body}` : body;
  return `SELECT ?o ?t ?p ?v WHERE { ${g} } LIMIT 500`;
}

type Row = Record<
  string,
  { type: string; value: string; datatype?: string; 'xml:lang'?: string } | undefined
>;

const rowValue = (t: NonNullable<Row[string]>): Value =>
  t.type === 'literal'
    ? {
        '@value': t.value,
        ...(t['xml:lang'] ? { '@language': t['xml:lang'] } : {}),
        ...(t.datatype ? { '@type': t.datatype } : {}),
      }
    : t.value;

/** The header of the first ontology in `rows`, else of the first concept scheme. */
export function parseHead(
  rows: Row[],
  langs: readonly string[],
): { head: OntologyHead; scheme: boolean } | null {
  const subjects = new Map<string, { type: string; stmts: { property: string; value: Value }[] }>();
  for (const r of rows) {
    if (!r.o || !r.p || !r.v || !r.t) continue;
    let s = subjects.get(r.o.value);
    if (!s) subjects.set(r.o.value, (s = { type: r.t.value, stmts: [] }));
    if (r.t.value === OWL + 'Ontology') s.type = r.t.value;
    s.stmts.push({ property: r.p.value, value: rowValue(r.v) });
  }
  const all = [...subjects];
  const pick =
    all.find(([, s]) => s.type === OWL + 'Ontology') ??
    all.find(([, s]) => s.type === SKOS + 'ConceptScheme');
  if (!pick) return null;
  const [iri, s] = pick;
  const best = (props: string[]) => {
    for (const p of props) {
      const vs = s.stmts.filter((x) => x.property === p);
      if (!vs.length) continue;
      vs.sort(
        (a, b) =>
          langRank(isLiteral(a.value) ? (a.value['@language'] ?? null) : null, langs) -
          langRank(isLiteral(b.value) ? (b.value['@language'] ?? null) : null, langs),
      );
      return valueText(vs[0].value);
    }
    return null;
  };
  const versionIri = s.stmts.find((x) => x.property === OWL + 'versionIRI');
  const used = new Set([
    RDF + 'type',
    ...TITLE_PROPS,
    ...DESCRIPTION_PROPS,
    ...LICENSE_PROPS,
    OWL + 'versionIRI',
    OWL + 'versionInfo',
  ]);
  return {
    scheme: s.type !== OWL + 'Ontology',
    head: {
      iri,
      title: best(TITLE_PROPS),
      description: best(DESCRIPTION_PROPS),
      version: best([OWL + 'versionInfo']),
      versionIri: versionIri ? valueText(versionIri.value) : null,
      license: best(LICENSE_PROPS),
      annotations: s.stmts.filter((x) => !used.has(x.property)),
    },
  };
}

/** The ontology or scheme header of a graph, by SPARQL. */
export async function ontologyHead(
  target: string,
  sel: BrowseSelection,
  langs: readonly string[],
  signal?: AbortSignal,
) {
  const rows = (await select(target, headQuery(sel.graph), {
    at: sel.at ?? undefined,
    signal,
  })) as Row[];
  return parseHead(rows, langs);
}

/** The SPARQL that counts the entities of each kind in a graph, and the deprecated classes. */
export function countsQuery(graph: string | null): string {
  const types = [
    `<${OWL}Class>`,
    `<${OWL}ObjectProperty>`,
    `<${OWL}DatatypeProperty>`,
    `<${OWL}AnnotationProperty>`,
    `<${OWL}NamedIndividual>`,
    `<${SKOS}Concept>`,
    `<${SKOS}ConceptScheme>`,
    `<${SKOS}Collection>`,
  ].join(' ');
  const inner = `?s a ?t . VALUES ?t { ${types} }`;
  const dep = `?s a <${OWL}Class> ; <${OWL}deprecated> true .`;
  const w = (b: string) => (graph ? `GRAPH <${graph}> { ${b} }` : b);
  return `SELECT ?t (COUNT(DISTINCT ?s) AS ?n) WHERE { { ${w(inner)} } UNION { ${w(dep)} BIND("deprecated" AS ?t) } } GROUP BY ?t`;
}

export const COUNT_NAMES: [string, string][] = [
  [OWL + 'Class', 'Classes'],
  ['deprecated', 'of them deprecated'],
  [OWL + 'ObjectProperty', 'Object properties'],
  [OWL + 'DatatypeProperty', 'Data properties'],
  [OWL + 'AnnotationProperty', 'Annotation properties'],
  [OWL + 'NamedIndividual', 'Individuals'],
  [SKOS + 'ConceptScheme', 'Concept schemes'],
  [SKOS + 'Concept', 'Concepts'],
  [SKOS + 'Collection', 'Collections'],
];

/** The counts of `countsQuery`'s rows, in display order, leaving out kinds with none. */
export function parseCounts(rows: Row[]): { kind: string; label: string; count: number }[] {
  const by = new Map<string, number>();
  for (const r of rows) if (r.t && r.n) by.set(r.t.value, Number(r.n.value));
  return COUNT_NAMES.filter(([k]) => (by.get(k) ?? 0) > 0).map(([k, label]) => ({
    kind: k,
    label,
    count: by.get(k) ?? 0,
  }));
}

/** The SPARQL that finds where a dataset mentions `iri`: per graph, as subject or object. */
export function reuseQuery(iri: string): string {
  const x = `<${iri}>`;
  return `SELECT ?g (COUNT(*) AS ?n) (SAMPLE(?l) AS ?label) WHERE {
  { { ${x} ?p ?o } UNION { ?s ?p ${x} } BIND("" AS ?g) }
  UNION { GRAPH ?g { { ${x} ?p ?o } UNION { ?s ?p ${x} } } }
  OPTIONAL { ${x} <${RDFS}label>|<${SKOS}prefLabel> ?l }
} GROUP BY ?g LIMIT 50`;
}

export type Reuse = {
  dataset: string;
  graphs: { graph: string; count: number }[];
  label: string | null;
};

/** Where `reuseQuery`'s rows say a dataset mentions the IRI, or null when it does not. */
export function parseReuse(dataset: string, rows: Row[]): Reuse | null {
  const graphs = rows
    .filter((r) => r.n && Number(r.n.value) > 0)
    .map((r) => ({ graph: r.g?.value ?? '', count: Number(r.n!.value) }));
  if (!graphs.length) return null;
  const label = rows.find((r) => r.label)?.label?.value ?? null;
  return { dataset, graphs, label };
}

// --- links and further reads -----------------------------------------------------------

/** How a component names and links the IRIs it shows; `href` is null for no page. */
export type Linker = {
  href: (iri: string) => string | null;
  name: (iri: string) => string;
  short: (iri: string) => string | null;
};

/** The namespaces of the W3C vocabularies, whose terms have no page of their own. */
const BUILT_IN = [RDF, RDFS, OWL, WELL_KNOWN.xsd, SKOS, 'http://www.w3.org/2008/05/skos-xl#'];

/** Whether `iri` is a term of RDF, RDFS, OWL, XSD or SKOS. */
export const builtIn = (iri: string) => BUILT_IN.some((ns) => iri.startsWith(ns));

/** Wrap a pattern in the page's graph, or leave it in the default graph. */
const inGraph = (graph: string | null, body: string) =>
  graph ? `GRAPH <${graph}> { ${body} }` : body;

/**
 * A pattern over the graphs a term answer names, where `default` is the stored default
 * graph. It names that graph as Jena does, so a server with a union default graph still
 * reads the graphs the term route read.
 */
function overGraphs(graphs: readonly string[], body: string) {
  const named = graphs.filter((g) => g !== 'default');
  const parts = [
    ...(graphs.includes('default') ? [`{ GRAPH <urn:x-arq:DefaultGraph> { ${body} } }`] : []),
    ...(named.length
      ? [`{ VALUES ?g { ${named.map((g) => `<${g}>`).join(' ')} } GRAPH ?g { ${body} } }`]
      : []),
  ];
  return parts.length ? parts.join(' UNION ') : `{ GRAPH <urn:x-arq:DefaultGraph> { ${body} } }`;
}

/** Where a page of named subjects continues: the page size of `incomingQuery` and `usageQuery`. */
export const PAGE = 50;

/**
 * The SPARQL of a page of the named subjects that point at `iri` through `property`, in the
 * graphs the term answer read (its `graphs`, which include the imports), in IRI order.
 */
export function incomingQuery(
  iri: string,
  property: string,
  graphs: readonly string[],
  offset: number,
  limit = PAGE,
) {
  return `SELECT DISTINCT ?s WHERE { ${overGraphs(graphs, `?s <${property}> <${iri}> FILTER(isIRI(?s))`)} } ORDER BY ?s LIMIT ${limit} OFFSET ${offset}`;
}

/** The SPARQL of a page of the named subjects that use the property `iri`, in IRI order. */
export function usageQuery(iri: string, graphs: readonly string[], offset: number, limit = PAGE) {
  return `SELECT DISTINCT ?s WHERE { ${overGraphs(graphs, `?s <${iri}> ?o FILTER(isIRI(?s))`)} } ORDER BY ?s LIMIT ${limit} OFFSET ${offset}`;
}

/** The SPARQL of the definitions and labels of some IRIs, for lists such as the top classes. */
export function definitionsQuery(iris: readonly string[], graph: string | null) {
  const values = iris.map((i) => `<${i}>`).join(' ');
  const props = [...DEFINITION_PROPS, RDFS + 'comment'].map((p) => `<${p}>`).join('|');
  return `SELECT ?s ?d WHERE { VALUES ?s { ${values} } ${inGraph(graph, `?s ${props} ?d`)} }`;
}

/** The best definition of each subject in `definitionsQuery`'s rows, in the caller's languages. */
export function parseDefinitions(rows: Row[], langs: readonly string[]): Map<string, string> {
  const best = new Map<string, { text: string; rank: number }>();
  for (const r of rows) {
    if (!r.s || !r.d) continue;
    const rank = langRank(r.d['xml:lang'] ?? null, langs);
    const was = best.get(r.s.value);
    if (!was || rank < was.rank) best.set(r.s.value, { text: r.d.value, rank });
  }
  return new Map([...best].map(([k, v]) => [k, v.text]));
}

/** The words for an entity's children and parents in a hierarchy of `kind`. */
export function treeWords(kind: TreeKind): { child: string; children: string; parent: string } {
  if (kind === 'concept')
    return { child: 'narrower concept', children: 'narrower concepts', parent: 'broader concept' };
  if (kind === 'class') return { child: 'subclass', children: 'subclasses', parent: 'superclass' };
  return { child: 'subproperty', children: 'subproperties', parent: 'superproperty' };
}

/** "1 subclass", "3 subclasses". */
export function countOf(n: number, one: string, many: string) {
  return `${n.toLocaleString('en-US')} ${n === 1 ? one : many}`;
}

/** A row of an infobox: a label and its values or text. */
export type InfoRow = {
  label: string;
  values?: Value[];
  /** text in place of values, or before them */
  text?: string;
  /** a fainter line under the values */
  note?: string;
  mono?: boolean;
  badge?: 'ok' | 'danger';
};

/** Headings for the statements that point at an entity, by property, where words read better. */
export const INCOMING_WORDS: Record<string, string> = {
  [RDF + 'type']: 'Instances',
  [RDFS + 'subClassOf']: 'Subclasses',
  [RDFS + 'subPropertyOf']: 'Subproperties',
  [RDFS + 'domain']: 'Domain of',
  [RDFS + 'range']: 'Range of',
  [OWL + 'equivalentClass']: 'Equivalent to',
  [OWL + 'disjointWith']: 'Disjoint with',
  [OWL + 'inverseOf']: 'Inverse of',
  [OWL + 'sameAs']: 'Same as',
  [SKOS + 'broader']: 'Narrower concepts',
  [SKOS + 'narrower']: 'Broader concepts',
  [SKOS + 'related']: 'Related from',
  [SKOS + 'member']: 'In collections',
  [SKOS + 'inScheme']: 'Concepts in this scheme',
  [SKOS + 'topConceptOf']: 'Top concepts',
  [SKOS + 'hasTopConcept']: 'Top concept of',
  [OBO + 'IAO_0100001']: 'Replaces',
};

/** The SPARQL of the named graphs that hold an ontology or a concept scheme. */
export function headGraphsQuery(): string {
  return `SELECT DISTINCT ?g WHERE { GRAPH ?g { ?o a ?t . VALUES ?t { <${OWL}Ontology> <${SKOS}ConceptScheme> } } } ORDER BY ?g LIMIT 200`;
}
