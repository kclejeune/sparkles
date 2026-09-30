import type { Term } from './api';

export const WELL_KNOWN: Record<string, string> = {
  rdf: 'http://www.w3.org/1999/02/22-rdf-syntax-ns#',
  rdfs: 'http://www.w3.org/2000/01/rdf-schema#',
  owl: 'http://www.w3.org/2002/07/owl#',
  xsd: 'http://www.w3.org/2001/XMLSchema#',
  foaf: 'http://xmlns.com/foaf/0.1/',
  dcterms: 'http://purl.org/dc/terms/',
  dc: 'http://purl.org/dc/elements/1.1/',
  skos: 'http://www.w3.org/2004/02/skos/core#',
  schema: 'http://schema.org/',
  sh: 'http://www.w3.org/ns/shacl#',
  prov: 'http://www.w3.org/ns/prov#',
};

export const RDF_TYPE = WELL_KNOWN.rdf + 'type';
export const RDFS_LABEL = WELL_KNOWN.rdfs + 'label';
export const XSD_STRING = WELL_KNOWN.xsd + 'string';

export type PrefixMap = Record<string, string>;

/** Longest-namespace-first list for shortening. */
function sortedEntries(prefixes: PrefixMap): [string, string][] {
  return Object.entries(prefixes).sort((a, b) => b[1].length - a[1].length);
}

const cache = new WeakMap<PrefixMap, [string, string][]>();

const LOCAL_OK = /^[A-Za-z0-9_À-￿]([A-Za-z0-9_.\-À-￿]*[A-Za-z0-9_\-À-￿])?$|^$/;

/** Shorten an IRI to a prefixed name when a known namespace matches (and the local part is valid). */
export function shorten(iri: string, prefixes: PrefixMap): string | null {
  let entries = cache.get(prefixes);
  if (!entries) {
    entries = sortedEntries(prefixes);
    cache.set(prefixes, entries);
  }
  for (const [p, ns] of entries) {
    if (ns && iri.startsWith(ns)) {
      const local = iri.slice(ns.length);
      if (LOCAL_OK.test(local)) return `${p}:${local}`;
    }
  }
  return null;
}

/** Last path/fragment segment of an IRI, useful as a fallback label. */
export function localName(iri: string): string {
  const m = /[#/:]([^#/:]+)[#/]?$/.exec(iri);
  return m ? decodeSafe(m[1]) : iri;
}

function decodeSafe(s: string) {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

export function displayIri(iri: string, prefixes: PrefixMap): string {
  return shorten(iri, prefixes) ?? iri;
}

/** Compact, human display of any term. */
export function displayTerm(t: Term | null | undefined, prefixes: PrefixMap): string {
  if (!t) return '';
  switch (t.type) {
    case 'uri':
      return displayIri(t.value, prefixes);
    case 'bnode':
      return `_:${t.value}`;
    case 'literal':
      return t.value;
    case 'triple':
      return `<< ${displayTerm(t.value.subject, prefixes)} ${displayTerm(t.value.predicate, prefixes)} ${displayTerm(t.value.object, prefixes)} >>`;
  }
}

/** SPARQL/Turtle syntax for a term (used in generated queries and copy). */
export function toSparql(t: Term): string {
  switch (t.type) {
    case 'uri':
      return `<${t.value}>`;
    case 'bnode':
      return `_:${t.value}`;
    case 'literal': {
      const v = JSON.stringify(t.value);
      if (t['xml:lang']) return `${v}@${t['xml:lang']}`;
      if (t.datatype && t.datatype !== XSD_STRING) return `${v}^^<${t.datatype}>`;
      return v;
    }
    case 'triple':
      return `<< ${toSparql(t.value.subject)} ${toSparql(t.value.predicate)} ${toSparql(t.value.object)} >>`;
  }
}

/** Stable identity key for a term (graph node ids, dedup). */
export function termKey(t: Term): string {
  return toSparql(t);
}

export const iri = (value: string): Term => ({ type: 'uri', value });

export function sparqlIri(value: string): string {
  return `<${value.replace(/[<>"{}|^`\\\s]/g, (c) => '%' + c.charCodeAt(0).toString(16).toUpperCase().padStart(2, '0'))}>`;
}

export function sparqlString(value: string): string {
  return JSON.stringify(value);
}

// --- query text analysis --------------------------------------------------

/** Strip comments, strings and IRIs so keyword detection is not fooled by their contents. */
export function stripQueryNoise(q: string): string {
  return q.replace(
    /("""[\s\S]*?"""|'''[\s\S]*?'''|"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*')|(<[^<>"{}|^`\\\s]*>)|(#[^\n]*)/g,
    (_m, str, iri) => (str ? '""' : iri ? '<>' : ''),
  );
}

const UPDATE_RE = /\b(INSERT|DELETE|LOAD|CLEAR|DROP|CREATE|COPY|MOVE|ADD|WITH)\b/i;
const QUERY_RE = /\b(SELECT|ASK|CONSTRUCT|DESCRIBE)\b/i;

/** True when the text is a SPARQL Update request rather than a query. */
export function isUpdate(q: string): boolean {
  const body = stripQueryNoise(q).replace(/\b(PREFIX\s+[\w.-]*:\s*<>|BASE\s+<>)/gi, '');
  const u = UPDATE_RE.exec(body);
  if (!u) return false;
  const s = QUERY_RE.exec(body);
  // "INSERT { } WHERE { SELECT ... }" is an update; "SELECT ... FILTER NOT EXISTS" is not.
  return !s || u.index < s.index;
}

export function queryKind(
  q: string,
): 'SELECT' | 'ASK' | 'CONSTRUCT' | 'DESCRIBE' | 'UPDATE' | null {
  if (isUpdate(q)) return 'UPDATE';
  const m = QUERY_RE.exec(stripQueryNoise(q));
  return m ? (m[1].toUpperCase() as 'SELECT') : null;
}

/** Prefixes declared in the query text. */
export function declaredPrefixes(q: string): Set<string> {
  const out = new Set<string>();
  for (const m of q.matchAll(/\bPREFIX\s+([\w.-]*):/gi)) out.add(m[1]);
  return out;
}

/** Prefixes used as `pfx:local` outside strings/IRIs/comments. */
export function usedPrefixes(q: string): Set<string> {
  const body = stripQueryNoise(q).replace(/\bPREFIX\s+[\w.-]*:\s*<>/gi, '');
  const out = new Set<string>();
  for (const m of body.matchAll(/(?<![\w?$:.-])([A-Za-z][\w.-]*)?:(?=[\wÀ-￿-]|\s|$)/g)) {
    out.add(m[1] ?? '');
  }
  return out;
}

/** Return the PREFIX lines needed for prefixes used but not declared (and known). */
export function missingPrefixDecls(q: string, known: PrefixMap): string[] {
  const declared = declaredPrefixes(q);
  const lines: string[] = [];
  for (const p of usedPrefixes(q)) {
    if (!declared.has(p) && known[p] != null) lines.push(`PREFIX ${p}: <${known[p]}>`);
  }
  return lines;
}

export function addMissingPrefixes(q: string, known: PrefixMap): string {
  const lines = missingPrefixDecls(q, known);
  return lines.length ? lines.join('\n') + '\n' + q : q;
}

/** Build a PREFIX header for generated queries. */
export function prefixHeader(prefixes: PrefixMap, only?: string[]): string {
  const names = only ?? Object.keys(prefixes);
  return names
    .filter((p) => prefixes[p] != null)
    .map((p) => `PREFIX ${p}: <${prefixes[p]}>`)
    .join('\n');
}
