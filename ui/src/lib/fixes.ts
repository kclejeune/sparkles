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
    /^-?\d{4,}-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])T([01]\d|2[0-4]):[0-5]\d:[0-5]\d(\.\d+)?(Z|[+-]\d{2}:\d{2})?$/,
  time: /^([01]\d|2[0-4]):[0-5]\d:[0-5]\d(\.\d+)?(Z|[+-]\d{2}:\d{2})?$/,
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

const daysIn = (year: number, month: number) =>
  month === 2
    ? (year % 4 === 0 && year % 100 !== 0) || year % 400 === 0
      ? 29
      : 28
    : [4, 6, 9, 11].includes(month)
      ? 30
      : 31;

/** A time zone offset: `Z`, or at most `±14:00` with minutes under 60. */
function zoneOk(z: string | undefined): boolean {
  if (!z || z === 'Z') return true;
  const m = /^[+-](\d{2}):(\d{2})$/.exec(z);
  if (!m) return false;
  const h = Number(m[1]);
  const min = Number(m[2]);
  return min < 60 && (h < 14 || (h === 14 && min === 0));
}

/** The calendar checks that the patterns of `LEXICAL` cannot express. */
function calendarOk(lexical: string, local: string): boolean {
  if (local === 'date' || local === 'dateTime') {
    const m =
      /^(-?\d{4,})-(\d{2})-(\d{2})(?:T(\d{2}):(\d{2}):(\d{2})(\.\d+)?)?(Z|[+-]\d{2}:\d{2})?$/.exec(
        lexical,
      );
    if (!m) return false;
    if (Number(m[3]) > daysIn(Number(m[1]), Number(m[2]))) return false;
    // 24:00:00 is the end of the day, with no fraction
    if (m[4] === '24' && (m[5] !== '00' || m[6] !== '00' || (m[7] && /[1-9]/.test(m[7]))))
      return false;
    return zoneOk(m[8]);
  }
  if (local === 'time' || local === 'gYear') {
    const z = /(Z|[+-]\d{2}:\d{2})$/.exec(lexical)?.[1];
    if (local === 'time' && /^24:/.test(lexical) && !/^24:00:00(\.0+)?(Z|[+-]|$)/.test(lexical))
      return false;
    return zoneOk(z);
  }
  return true;
}

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
  if (!calendarOk(lexical, local)) return false;
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

/** An IRI reference with nothing that could end it early (SPARQL IRIREF). */
const IRIREF = /^<[^<>"{}|^`\\\x00-\x20]*>$/;
/** A literal: one quoted string with valid escapes, a language tag or an IRI datatype. */
const LITERAL =
  /^"(?:[^"\\\n\r]|\\[tbnrf"'\\]|\\u[0-9A-Fa-f]{4}|\\U[0-9A-Fa-f]{8})*"(?:@[A-Za-z]+(?:-[A-Za-z0-9]+)*|\^\^<[^<>"{}|^`\\\x00-\x20]*>)?$/;

/**
 * One RDF term in SPARQL syntax taken from message text, or null: the whole text must be
 * one IRI reference or one literal, so message text can never add SPARQL syntax.
 */
export function oneTerm(text: string | null | undefined): string | null {
  const t = (text ?? '').trim();
  return IRIREF.test(t) || LITERAL.test(t) ? t : null;
}

/** An IRI from message or constraint text: one IRI reference, or a prefixed name. */
function oneIri(text: string | null | undefined, prefixes: PrefixMap): string | null {
  const t = (text ?? '').trim();
  if (t.startsWith('<')) return IRIREF.test(t) ? t.slice(1, -1) : null;
  const iri = expandIri(t, prefixes);
  return iri && IRIREF.test(`<${iri}>`) ? iri : null;
}

/** Text for a comment line: line breaks become spaces, so it cannot leave the comment. */
const oneLine = (s: string) => s.replace(/[\r\n\u2028\u2029\u0085]+/g, ' ');

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
  return oneLine(
    `# Suggested fix for ${name(r.focus)} · ${shape} · ${r.lang === 'shacl' ? 'sh:' : 'ShEx '}${r.kind}${path}`,
  );
}

/**
 * Where the result's triples live: `null` for the default graph, a graph IRI, or `choose`
 * when the data graph is several graphs or unknown, which puts the placeholder `?graph` in
 * the update.
 */
export type FixGraph = string | null | 'choose';

export type FixContext = {
  prefixes?: PrefixMap;
  /** The values of the result's path on its focus node, for `sh:maxCount`. */
  values?: Term[];
  /** The graph the update reads and writes (the default graph when absent). */
  graph?: FixGraph;
};

/** A triple pattern in the graph of `ctx`. */
function inGraph(ctx: FixContext, triple: string): string {
  if (ctx.graph == null) return triple;
  const g = ctx.graph === 'choose' ? '?graph' : iriRef(ctx.graph);
  return `GRAPH ${g} { ${triple} }`;
}

/** The comment that asks for the graph, when it is not known. */
const graphNote = (ctx: FixContext): string[] =>
  ctx.graph === 'choose'
    ? [
        '# The data graph is several graphs: replace ?graph with the graph that holds the triple,',
        '# or remove GRAPH ?graph { } to use the default graph.',
      ]
    : [];

/** A DATA block of one triple in the graph of `ctx`. */
const dataBlock = (op: 'INSERT' | 'DELETE', ctx: FixContext, triple: string) => [
  `${op} DATA {`,
  ctx.graph == null ? `  ${triple} .` : `  ${inGraph(ctx, `${triple} .`)}`,
  '}',
];

/** The triple pattern of the focus node, the result's path and `object`. */
function arc(r: VResult, object: string): string {
  const f = toSparql(r.focus);
  const p = iriRef(r.path!);
  return r.inverse ? `${object} ${p} ${f}` : `${f} ${p} ${object}`;
}

function minFix(r: VResult, ctx: FixContext): string {
  const n = r.lang === 'shex' ? r.min : Number(messageParam(r, 'minCount') ?? 1);
  const count = n != null && Number.isFinite(n) ? n : 1;
  return [
    header(r, ctx.prefixes ?? {}),
    `# The shape needs at least ${count} value${count === 1 ? '' : 's'} here.`,
    '# Replace ?value with the value to add, such as "text", 42 or <http://example.org/x>.',
    ...graphNote(ctx),
    ...dataBlock('INSERT', ctx, arc(r, '?value')),
  ].join('\n');
}

function maxFix(r: VResult, ctx: FixContext): string {
  const raw = r.lang === 'shex' ? r.max : Number(messageParam(r, 'maxCount') ?? 1);
  const n = raw != null && Number.isFinite(raw) ? raw : 1;
  const values = (ctx.values ?? []).filter(dataTerm);
  const listed = values.length
    ? values.map((v, i) => oneLine(`#   ${toSparql(v)}${i < n ? '   (kept)' : ''}`))
    : ['#   (read them with: SELECT ?v WHERE { ' + arc(r, '?v') + ' })'];
  return [
    header(r, ctx.prefixes ?? {}),
    `# The shape allows at most ${n} value${n === 1 ? '' : 's'} here. Keep the first and`,
    '# remove the rest: replace ?value with a value to remove, once per value.',
    '# The values are:',
    ...listed,
    ...graphNote(ctx),
    ...dataBlock('DELETE', ctx, arc(r, '?value')),
  ].join('\n');
}

/** A DELETE and INSERT that replaces the value: the path when known, else any predicate. */
function replace(r: VResult, ctx: FixContext, from: Term, to: string): string {
  const f = toSparql(r.focus);
  const p = r.path ? iriRef(r.path) : '?p';
  const v = toSparql(from);
  const pattern = inGraph(ctx, r.inverse ? `${v} ${p} ${f}` : `${f} ${p} ${v}`);
  const fresh = inGraph(ctx, r.inverse ? `${to} ${p} ${f}` : `${f} ${p} ${to}`);
  return [
    ...graphNote(ctx),
    `DELETE { ${pattern} }\nINSERT { ${fresh} }\nWHERE  { ${pattern} }`,
  ].join('\n');
}

function datatypeFix(r: VResult, ctx: FixContext): string | null {
  const v = r.value;
  if (!v || v.type !== 'literal') return null;
  const text = r.lang === 'shex' ? r.constraint : messageParam(r, 'DatatypeConstraint');
  const dt = oneIri(text, ctx.prefixes ?? {});
  if (!dt || validLexical(v.value, dt) !== true) return null;
  const cast = `${JSON.stringify(v.value)}^^<${dt}>`;
  return [
    header(r, ctx.prefixes ?? {}),
    oneLine(
      `# The value has the wrong datatype. Its lexical form is a valid ${displayIri(dt, ctx.prefixes ?? {})},`,
    ),
    '# so this writes the same value with that datatype.',
    replace(r, ctx, v, cast),
  ].join('\n');
}

function classFix(r: VResult, ctx: FixContext): string | null {
  if (r.value?.type !== 'uri') return null;
  const cls = oneIri(messageParam(r, 'ClassConstraint'), ctx.prefixes ?? {});
  return [
    header(r, ctx.prefixes ?? {}),
    '# This gives the value the class the shape expects. The other choice is to change the',
    '# value to a resource that already has the class.',
    ...(cls ? [] : ['# Replace ?class with the class the shape expects.']),
    ...graphNote(ctx),
    ...dataBlock(
      'INSERT',
      ctx,
      `${toSparql(r.value)} <${RDF_TYPE}> ${cls ? iriRef(cls) : '?class'}`,
    ),
  ].join('\n');
}

function allowedFix(r: VResult, ctx: FixContext): string | null {
  const allowed =
    r.lang === 'shex'
      ? r.constraint
      : (messageParam(r, 'InConstraint') ?? messageParam(r, 'HasValue'));
  if (r.kind === 'hasValue') {
    // the required value is missing: add it, when the message names exactly one term
    if (!r.path) return null;
    const v = oneTerm(allowed) ?? '?allowed';
    return [
      header(r, ctx.prefixes ?? {}),
      oneLine(`# The shape requires the value ${allowed ?? 'it names'} here.`),
      ...(v === '?allowed' ? ['# Replace ?allowed with that value.'] : []),
      ...graphNote(ctx),
      ...dataBlock('INSERT', ctx, arc(r, v)),
    ].join('\n');
  }
  if (!dataTerm(r.value)) return null;
  return [
    header(r, ctx.prefixes ?? {}),
    allowed
      ? oneLine(`# The allowed values are: ${allowed}`)
      : '# The shape lists the allowed values.',
    '# Replace ?allowed with one of them.',
    replace(r, ctx, r.value, '?allowed'),
  ].join('\n');
}

function closedFix(r: VResult, ctx: FixContext): string | null {
  if (!r.path || !dataTerm(r.value)) return null;
  return [
    header(r, ctx.prefixes ?? {}),
    '# The shape is closed and does not allow this predicate. This removes the triple.',
    ...graphNote(ctx),
    ...dataBlock('DELETE', ctx, `${toSparql(r.focus)} ${iriRef(r.path)} ${toSparql(r.value)}`),
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
  /\?(value|allowed|class|graph)\b/.test(q.replace(/^#.*$/gm, ''));
