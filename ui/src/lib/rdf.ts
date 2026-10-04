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
  geo: 'http://www.opengis.net/ont/geosparql#',
  geof: 'http://www.opengis.net/def/function/geosparql/',
  sf: 'http://www.opengis.net/ont/sf#',
  uom: 'http://www.opengis.net/def/uom/OGC/1.0/',
  spatial: 'http://jena.apache.org/spatial#',
  spatialF: 'http://jena.apache.org/function/spatial#',
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
      return literalText(t);
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
      // RDF 1.2 triple term; the bare `<< … >>` form is reification syntax, not a term
      return `<<( ${toSparql(t.value.subject)} ${toSparql(t.value.predicate)} ${toSparql(t.value.object)} )>>`;
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

// --- vector literals (docs/API.md "Vector similarity") ------------------------------

export const SPK = 'urn:x-sparkles:';
export const VECTOR_DATATYPE = SPK + 'vector';
/** The compact form: base64 of the values as little-endian binary32. */
export const VECTOR_B64_DATATYPE = SPK + 'vectorB64';

export function isVectorLiteral(
  t: Term | null | undefined,
): t is Extract<Term, { type: 'literal' }> & { datatype: string } {
  return (
    t?.type === 'literal' && (t.datatype === VECTOR_DATATYPE || t.datatype === VECTOR_B64_DATATYPE)
  );
}

/**
 * The numbers of a vector literal (a JSON array of 1–16384 finite numbers, or with the
 * compact datatype their base64), or null when it does not parse (the server stores such
 * a literal but never matches it).
 */
export function parseVector(value: string, datatype: string = VECTOR_DATATYPE): number[] | null {
  if (datatype === VECTOR_B64_DATATYPE) return parseVectorB64(value);
  let v: unknown;
  try {
    v = JSON.parse(value);
  } catch {
    return null;
  }
  if (!Array.isArray(v) || v.length === 0 || v.length > 16384) return null;
  return v.every((x) => typeof x === 'number' && Number.isFinite(x)) ? (v as number[]) : null;
}

/** The values of a compact vector literal, or null when it does not parse. */
export function parseVectorB64(value: string): number[] | null {
  if (!value || value.length % 4 !== 0 || !/^[A-Za-z0-9+/]+={0,2}$/.test(value)) return null;
  let bin: string;
  try {
    bin = atob(value);
  } catch {
    return null;
  }
  if (bin.length % 4 !== 0 || bin.length / 4 > 16384) return null;
  const view = new DataView(new ArrayBuffer(bin.length));
  for (let i = 0; i < bin.length; i++) view.setUint8(i, bin.charCodeAt(i));
  const out: number[] = [];
  for (let i = 0; i < bin.length; i += 4) {
    const x = view.getFloat32(i, true);
    if (!Number.isFinite(x)) return null;
    out.push(x);
  }
  return out;
}

/** A component for display: at most 3 decimals (tiny or huge ones in exponent form). */
function fmtComponent(x: number): string {
  const a = Math.abs(x);
  const s = a >= 1e4 || (a !== 0 && a < 0.001) ? x.toExponential(1) : String(+x.toFixed(3));
  return s.replace(/^-/, '−');
}

/**
 * Compact text of a vector literal: `vector(384) [0.12, −0.03, 0.4, …]` showing the first
 * `head` components; `vector(invalid) …` for one that does not parse.
 */
export function abbreviateVector(value: string, head = 3, datatype = VECTOR_DATATYPE): string {
  const v = parseVector(value, datatype);
  if (!v) return `vector(invalid) ${value.length > 24 ? value.slice(0, 21) + '…' : value}`;
  const shown = v.slice(0, head).map(fmtComponent);
  return `vector(${v.length}) [${shown.join(', ')}${v.length > head ? ', …' : ''}]`;
}

/** Display text of a literal's value: vectors abbreviated, everything else unchanged. */
export function literalText(t: Extract<Term, { type: 'literal' }>): string {
  return isVectorLiteral(t) ? abbreviateVector(t.value, 3, t.datatype) : t.value;
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

/**
 * Add missing prefixes to `text`, the value `target.query` had when an execution started
 * (captured before any await), and write the result back only if the user has not edited
 * `target` since. Returns the text to execute.
 */
export function applyMissingPrefixes(
  target: { query: string },
  text: string,
  known: PrefixMap,
): string {
  const fixed = addMissingPrefixes(text, known);
  if (fixed !== text && target.query === text) target.query = fixed;
  return fixed;
}

/** Build a PREFIX header for generated queries. */
export function prefixHeader(prefixes: PrefixMap, only?: string[]): string {
  const names = only ?? Object.keys(prefixes);
  return names
    .filter((p) => prefixes[p] != null)
    .map((p) => `PREFIX ${p}: <${prefixes[p]}>`)
    .join('\n');
}
