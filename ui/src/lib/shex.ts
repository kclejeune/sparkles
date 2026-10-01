// Helpers of the ShEx side of the dataset page's Validate panel: the example schema and
// shape map built from the dataset's classes, labels of shapes and failures, the stored
// drafts, and where a syntax error is.

import { ApiError, type SchemaSummary, type ShexFailure, type ShexResult, type Term } from './api';
import { displayIri, shorten, type PrefixMap } from './rdf';

export type ValidateLang = 'shacl' | 'shex';

/** The schema and shape map a dataset's ShEx side was last left with. */
export type ShexDraft = { schema: string; map: string };

/** localStorage key of a dataset's ShEx draft (`{schema, map}`). */
export const shexDraftKey = (ds: string) => `sparkles.shex.${ds}`;
/** localStorage key of a dataset's Validate language (SHACL or ShEx). */
export const validateLangKey = (ds: string) => `sparkles.validate.lang.${ds}`;

/** The stored draft, if it has the right shape. */
export function readDraft(stored: unknown): ShexDraft | null {
  if (!stored || typeof stored !== 'object') return null;
  const { schema, map } = stored as Record<string, unknown>;
  return typeof schema === 'string' && typeof map === 'string' ? { schema, map } : null;
}

/** The stored language, SHACL unless it reads `shex`. */
export const readLang = (stored: unknown): ValidateLang => (stored === 'shex' ? 'shex' : 'shacl');

/** The classes with the most instances (vocabulary classes left out), at most `n`. */
export function topClasses(summary: SchemaSummary | null | undefined, n = 3): string[] {
  return (summary?.classes.items ?? [])
    .filter((c) => !c.builtin && c.observed.instances > 0)
    .sort((a, b) => b.observed.instances - a.observed.instances || a.iri.localeCompare(b.iri))
    .slice(0, n)
    .map((c) => c.iri);
}

/** The schema of the example when the dataset has no typed nodes (the people of the docs). */
const FALLBACK: ShexDraft = {
  schema: `PREFIX ex: <http://example.org/>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>

# A person has one string name, at most one age up to 150, and knows other persons.
ex:PersonShape EXTRA a {
  a [ex:Person] ;
  foaf:name xsd:string ;
  foaf:age xsd:integer MAXINCLUSIVE 150 ? ;
  foaf:knows @ex:PersonShape *
}
`,
  map: '{FOCUS a ex:Person}@ex:PersonShape',
};

/** An IRI as a prefixed name, or `<iri>`. */
function compact(iri: string, prefixes: PrefixMap): { text: string; prefix?: string } {
  const short = shorten(iri, prefixes);
  if (short == null) return { text: `<${iri}>` };
  return { text: short, prefix: short.slice(0, short.indexOf(':')) };
}

/** The shape label of a class: `ex:Person` → `ex:PersonShape`, `<…/Person>` → `<…/PersonShape>`. */
export function shapeFor(cls: string, prefixes: PrefixMap): string {
  const c = compact(cls, prefixes);
  // a prefixed name gains the suffix after its local part; an IRI before its `>`
  return c.prefix == null ? `<${cls}Shape>` : `${c.text}Shape`;
}

/**
 * An example schema and shape map for a dataset: one shape per class (the classes with
 * the most instances), each checking the type of the nodes the map selects by type.
 */
export function exampleDraft(classes: string[], prefixes: PrefixMap): ShexDraft {
  if (!classes.length) return FALLBACK;
  const used = new Set<string>();
  const shapes: string[] = [];
  const map: string[] = [];
  for (const cls of classes) {
    const c = compact(cls, prefixes);
    if (c.prefix != null) used.add(c.prefix);
    const label = shapeFor(cls, prefixes);
    shapes.push(`${label} EXTRA a {\n  a [${c.text}]\n}`);
    map.push(`{FOCUS a ${c.text}}@${label}`);
  }
  const header = [...used]
    .sort()
    .map((p) => `PREFIX ${p}: <${prefixes[p]}>`)
    .join('\n');
  const intro = `# One shape per class with the most instances. Add triple constraints such as
#   rdfs:label xsd:string ;
# (with their PREFIXes) to check more than the type.`;
  return {
    schema: `${header ? `${header}\n\n` : ''}${intro}\n${shapes.join('\n\n')}\n`,
    map: map.join(',\n'),
  };
}

/** Example shape maps for the map field: by type, one node, START, a SPARQL selector. */
export function exampleMaps(classes: string[], prefixes: PrefixMap): string[] {
  const cls = classes[0] ?? 'http://example.org/Person';
  const c = compact(cls, prefixes).text;
  const label = shapeFor(cls, prefixes);
  return [
    `{FOCUS a ${c}}@${label}`,
    `<http://example.org/node>@${label}`,
    `{FOCUS a ${c}}@START`,
    `SPARQL """SELECT ?focus WHERE { ?focus a <${cls}> } LIMIT 100"""@${label}`,
  ];
}

/** `PREFIX p: <ns>` declarations of a ShExC schema (for showing results). */
export function schemaPrefixes(schema: string): PrefixMap {
  const p: PrefixMap = {};
  for (const m of schema.matchAll(/(?:^|\s)PREFIX\s+([A-Za-z][\w.-]*|):\s*<([^>\s]*)>/gi))
    p[m[1]] ??= m[2];
  return p;
}

/** A result's shape: `START`, a prefixed name, `<iri>` or `_:label`. */
export function shapeText(shape: ShexResult['shape'], prefixes: PrefixMap): string {
  if (shape.type === 'start') return 'START';
  if (shape.type === 'uri') return displayIri(shape.value, prefixes);
  if (shape.type === 'bnode') return `_:${shape.value}`;
  return String((shape as Term).value);
}

const KIND_NAMES: Record<ShexFailure['kind'], string> = {
  nodeKind: 'node kind',
  datatype: 'datatype',
  facet: 'facet',
  valueSet: 'value set',
  cardinality: 'cardinality',
  closed: 'closed',
  extra: 'extra',
  noMatch: 'no match',
  reference: 'reference',
  not: 'NOT',
  semAct: 'semantic action',
  external: 'external',
};

/** The cardinality a triple constraint asks for, in words. */
export function expected(min: number, max: number | null): string {
  if (max === min) return `exactly ${min}`;
  if (max == null) return `at least ${min}`;
  if (min === 0) return `at most ${max}`;
  return `${min} to ${max}`;
}

/** One failure as table fields: what kind, on which arc, which value, and why. */
export type FailureRow = {
  kind: string;
  /** The predicate (`^p` for an inverse arc), shortened. */
  predicate?: string;
  value?: Term;
  detail: string;
};

export function failureRow(f: ShexFailure, prefixes: PrefixMap): FailureRow {
  const kind = KIND_NAMES[f.kind] ?? f.kind;
  const iri = (i: string) => displayIri(i, prefixes);
  switch (f.kind) {
    case 'nodeKind':
    case 'datatype':
    case 'facet':
    case 'valueSet':
      return { kind, value: f.value, detail: `fails ${f.constraint}` };
    case 'cardinality':
      return {
        kind,
        predicate: `${f.inverse ? '^' : ''}${iri(f.predicate)}`,
        detail: `${f.count} found, ${expected(f.min, f.max)} expected`,
      };
    case 'closed':
      return { kind, predicate: iri(f.predicate), value: f.value, detail: 'not allowed (CLOSED)' };
    case 'extra':
      return {
        kind,
        predicate: iri(f.predicate),
        value: f.value,
        detail: 'matches no triple constraint',
      };
    case 'noMatch':
      return { kind, detail: f.detail };
    case 'reference':
      return { kind, value: f.value, detail: `does not conform to ${f.shape}` };
    case 'not':
      return { kind, detail: `conforms to ${f.shape} under NOT` };
    case 'semAct':
      return { kind, detail: `<${f.extension}>: ${f.message}` };
    case 'external':
      return { kind, detail: `${f.shape} has no definition` };
  }
  return { kind, detail: JSON.stringify(f) };
}

/** `N conformant · M nonconformant · X ms` (the server's time). */
export function countsLine(
  counts: { conformant: number; nonconformant: number },
  millis: number,
): string {
  const n = (x: number) => x.toLocaleString('en-US');
  const ms = millis < 1000 ? `${Math.round(millis)} ms` : `${(millis / 1000).toFixed(2)} s`;
  return `${n(counts.conformant)} conformant · ${n(counts.nonconformant)} nonconformant · ${ms}`;
}

/**
 * Where a 400's syntax error is: the line and column, and whether it is in the schema
 * (else the shape map or the externs). Null for other errors.
 */
export function syntaxError(e: unknown): { line: number; column?: number; schema: boolean } | null {
  if (!(e instanceof ApiError) || e.status !== 400 || e.line == null) return null;
  return { line: e.line, column: e.column, schema: /^schema\b/i.test(e.message) };
}
