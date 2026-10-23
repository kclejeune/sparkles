// Suggested fixes for validation results (docs/specs/U07-data-quality-and-schema-diagram.md
// §11.4). A suggestion is a SPARQL Update built from the result alone: the result's terms
// are filled in, and what the result does not say is left as a marked placeholder
// (`?value`, `?allowed`, `?class`), so the update never guesses data and does not parse
// until the person fills it in. Components without a safe form get no suggestion.
import type { Term } from './api';
import { displayIri, toSparql, WELL_KNOWN, type PrefixMap } from './rdf';
import { expandIri, type VResult } from './validation-overlay';

const XSD = WELL_KNOWN.xsd;
const RDF_TYPE = WELL_KNOWN.rdf + 'type';

/** Lexical forms of the XSD datatypes a cast may write (XSD 1.1 Part 2). */
const LEXICAL: Record<string, RegExp> = {
  string: /^[\s\S]*$/,
  boolean: /^(true|false|1|0)$/,
  decimal: /^[+-]?(\d+(\.\d*)?|\.\d+)$/,
  integer: /^[+-]?\d+$/,
  long: /^[+-]?\d+$/,
  int: /^[+-]?\d+$/,
  short: /^[+-]?\d+$/,
  byte: /^[+-]?\d+$/,
  nonNegativeInteger: /^\+?\d+$|^-0+$/,
  positiveInteger: /^\+?0*[1-9]\d*$/,
  nonPositiveInteger: /^-\d+$|^\+?0+$/,
  negativeInteger: /^-0*[1-9]\d*$/,
  unsignedLong: /^\+?\d+$/,
  unsignedInt: /^\+?\d+$/,
  double: /^([+-]?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?|[+-]?INF|NaN)$/,
  float: /^([+-]?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?|[+-]?INF|NaN)$/,
  date: /^-?\d{4,}-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])(Z|[+-]\d{2}:\d{2})?$/,
  dateTime:
    /^-?\d{4,}-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])T([01]\d|2[0-3]):[0-5]\d:[0-5]\d(\.\d+)?(Z|[+-]\d{2}:\d{2})?$/,
  time: /^([01]\d|2[0-3]):[0-5]\d:[0-5]\d(\.\d+)?(Z|[+-]\d{2}:\d{2})?$/,
  gYear: /^-?\d{4,}(Z|[+-]\d{2}:\d{2})?$/,
  anyURI: /^\S*$/,
};

const RANGES: Record<string, [bigint, bigint]> = {
  long: [-(2n ** 63n), 2n ** 63n - 1n],
  int: [-(2n ** 31n), 2n ** 31n - 1n],
  short: [-32768n, 32767n],
  byte: [-128n, 127n],
  unsignedLong: [0n, 2n ** 64n - 1n],
  unsignedInt: [0n, 2n ** 32n - 1n],
};

/**
 * Whether `lexical` is a valid lexical form of the datatype `iri`: `null` for a datatype
 * this check does not know, so that no cast is suggested.
 */
export function validLexical(lexical: string, iri: string): boolean | null {
  if (!iri.startsWith(XSD)) return null;
  const local = iri.slice(XSD.length);
  const re = LEXICAL[local];
  if (!re) return null;
  if (!re.test(lexical)) return false;
  const range = RANGES[local];
  if (range) {
    const n = BigInt(lexical.replace(/^\+/, ''));
    return n >= range[0] && n <= range[1];
  }
  return true;
}

/** The parameter of a default SHACL message, such as the `<…>` of `DatatypeConstraint[<…>]`. */
function messageParam(r: VResult, name: string): string | null {
  for (const m of r.messages) {
    const hit = new RegExp(`${name}\\[(.*?)\\](?:[: ]|$)`).exec(m);
    if (hit) return hit[1].trim();
  }
  return null;
}

/** A term that may appear in a DATA block: IRIs and literals, not blank nodes. */
const dataTerm = (t: Term | null | undefined): t is Term =>
  !!t && (t.type === 'uri' || t.type === 'literal');

const iriRef = (iri: string) => `<${iri}>`;

/** The comment line that names the result. */
function header(r: VResult, prefixes: PrefixMap): string {
  const name = (t: Term) => (t.type === 'uri' ? displayIri(t.value, prefixes) : toSparql(t));
  const shape =
    r.shape.startsWith('_:') || r.shape === 'START' ? r.shape : displayIri(r.shape, prefixes);
  const path = r.path ? ` · path ${displayIri(r.path, prefixes)}` : '';
  return `# Suggested fix for ${name(r.focus)} · ${shape} · ${r.lang === 'shacl' ? 'sh:' : 'ShEx '}${r.kind}${path}`;
}

export type FixContext = {
  prefixes?: PrefixMap;
  /** The values of the result's path on its focus node, for `sh:maxCount`. */
  values?: Term[];
};

/** The triple pattern of the focus node, the result's path and `object`. */
function arc(r: VResult, object: string): string {
  const f = toSparql(r.focus);
  const p = iriRef(r.path!);
  return r.inverse ? `${object} ${p} ${f}` : `${f} ${p} ${object}`;
}

function minFix(r: VResult, ctx: FixContext): string {
  const n = r.lang === 'shex' ? r.min : Number(messageParam(r, 'minCount') ?? 1);
  return [
    header(r, ctx.prefixes ?? {}),
    `# The shape needs at least ${n ?? 1} value${n === 1 ? '' : 's'} here.`,
    '# Replace ?value with the value to add, such as "text", 42 or <http://example.org/x>.',
    'INSERT DATA {',
    `  ${arc(r, '?value')} .`,
    '}',
  ].join('\n');
}

function maxFix(r: VResult, ctx: FixContext): string {
  const n = r.lang === 'shex' ? r.max : Number(messageParam(r, 'maxCount') ?? 1);
  const values = (ctx.values ?? []).filter(dataTerm);
  const listed = values.length
    ? values.map((v, i) => `#   ${toSparql(v)}${i < (n ?? 1) ? '   (kept)' : ''}`)
    : ['#   (read them with: SELECT ?v WHERE { ' + arc(r, '?v') + ' })'];
  return [
    header(r, ctx.prefixes ?? {}),
    `# The shape allows at most ${n ?? 0} value${n === 1 ? '' : 's'} here. Keep the first and`,
    '# remove the rest: replace ?value with a value to remove, once per value.',
    '# The values are:',
    ...listed,
    'DELETE DATA {',
    `  ${arc(r, '?value')} .`,
    '}',
  ].join('\n');
}

/** A DELETE and INSERT that replaces the value: the path when known, else any predicate. */
function replace(r: VResult, from: Term, to: string): string {
  const f = toSparql(r.focus);
  const p = r.path ? iriRef(r.path) : '?p';
  const v = toSparql(from);
  const pattern = r.inverse ? `${v} ${p} ${f}` : `${f} ${p} ${v}`;
  const fresh = r.inverse ? `${to} ${p} ${f}` : `${f} ${p} ${to}`;
  return `DELETE { ${pattern} }\nINSERT { ${fresh} }\nWHERE  { ${pattern} }`;
}

function datatypeFix(r: VResult, ctx: FixContext): string | null {
  const v = r.value;
  if (!v || v.type !== 'literal') return null;
  const dt =
    r.lang === 'shex'
      ? expandIri(r.constraint ?? '', ctx.prefixes ?? {})
      : expandIri(messageParam(r, 'DatatypeConstraint') ?? '', ctx.prefixes ?? {});
  if (!dt || validLexical(v.value, dt) !== true) return null;
  const cast = `${JSON.stringify(v.value)}^^<${dt}>`;
  return [
    header(r, ctx.prefixes ?? {}),
    `# The value has the wrong datatype. Its lexical form is a valid ${displayIri(dt, ctx.prefixes ?? {})},`,
    '# so this writes the same value with that datatype.',
    replace(r, v, cast),
  ].join('\n');
}

function classFix(r: VResult, ctx: FixContext): string | null {
  if (r.value?.type !== 'uri') return null;
  const param = messageParam(r, 'ClassConstraint');
  const cls = param ? expandIri(param, ctx.prefixes ?? {}) : null;
  return [
    header(r, ctx.prefixes ?? {}),
    '# This gives the value the class the shape expects. The other choice is to change the',
    '# value to a resource that already has the class.',
    ...(cls ? [] : ['# Replace ?class with the class the shape expects.']),
    'INSERT DATA {',
    `  ${toSparql(r.value)} <${RDF_TYPE}> ${cls ? iriRef(cls) : '?class'} .`,
    '}',
  ].join('\n');
}

function allowedFix(r: VResult, ctx: FixContext): string | null {
  const allowed =
    r.lang === 'shex'
      ? r.constraint
      : (messageParam(r, 'InConstraint') ?? messageParam(r, 'HasValue'));
  const lines = [
    header(r, ctx.prefixes ?? {}),
    allowed ? `# The allowed values are: ${allowed}` : '# The shape lists the allowed values.',
    '# Replace ?allowed with one of them.',
  ];
  if (r.kind === 'hasValue') {
    // the required value is missing: add it
    if (!r.path) return null;
    const v = allowed && /^(<[^>]*>|".*")/.test(allowed) ? allowed : '?allowed';
    return [
      header(r, ctx.prefixes ?? {}),
      `# The shape requires the value ${allowed ?? 'it names'} here.`,
      ...(v === '?allowed' ? ['# Replace ?allowed with that value.'] : []),
      'INSERT DATA {',
      `  ${arc(r, v)} .`,
      '}',
    ].join('\n');
  }
  if (!dataTerm(r.value)) return null;
  return [...lines, replace(r, r.value, '?allowed')].join('\n');
}

function closedFix(r: VResult, ctx: FixContext): string | null {
  if (!r.path || !dataTerm(r.value)) return null;
  return [
    header(r, ctx.prefixes ?? {}),
    '# The shape is closed and does not allow this predicate. This removes the triple.',
    'DELETE DATA {',
    `  ${toSparql(r.focus)} ${iriRef(r.path)} ${toSparql(r.value)} .`,
    '}',
  ].join('\n');
}

/**
 * The suggested update for a result, or null when its component has no safe suggestion
 * (§11.4): paths of more than one predicate, `sh:sparql`, `sh:pattern`, `sh:node`,
 * qualified shapes and the other components, a focus node that is a blank node, which an
 * update cannot name, and a cast whose lexical form is not valid for the datatype.
 */
export function suggestFix(r: VResult, ctx: FixContext = {}): string | null {
  if (r.focus.type !== 'uri') return null;
  if (r.pathText) return null;
  if (r.lang === 'shacl') {
    switch (r.kind) {
      case 'minCount':
        return r.path ? minFix(r, ctx) : null;
      case 'maxCount':
        return r.path ? maxFix(r, ctx) : null;
      case 'datatype':
        return datatypeFix(r, ctx);
      case 'class':
        return classFix(r, ctx);
      case 'in':
      case 'hasValue':
        return allowedFix(r, ctx);
      case 'closed':
        return closedFix(r, ctx);
      default:
        return null;
    }
  }
  switch (r.kind) {
    case 'cardinality':
      if (!r.path || r.count == null || r.min == null) return null;
      if (r.count < r.min) return minFix(r, ctx);
      if (r.max != null && r.count > r.max) return maxFix(r, ctx);
      return null;
    case 'datatype':
      return datatypeFix(r, ctx);
    case 'valueSet':
      return allowedFix(r, ctx);
    case 'closed':
    case 'extra':
      return closedFix(r, ctx);
    default:
      return null;
  }
}

/** Whether a suggestion still holds a placeholder the person must fill in. */
export const hasPlaceholder = (q: string) =>
  /\?(value|allowed|class)\b/.test(q.replace(/^#.*$/gm, ''));
