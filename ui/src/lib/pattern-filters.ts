// The filter forms of the Pattern pane (U08 §2.2, after U03 §2.2): the forms a variable
// offers by the datatypes its values were seen with, and the expression each form writes.
// Every expression is one the reader understands, so a filter made here is drawn as a
// chip on its variable, and a chip opens again in the same form.

import type { FilterForm } from './pattern';

export const XSD = 'http://www.w3.org/2001/XMLSchema#';
export const RDF_LANG = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#langString';

export type Op =
  | '='
  | '!='
  | '<'
  | '<='
  | '>'
  | '>='
  | 'contains'
  | 'strstarts'
  | 'strends'
  | 'regex'
  | 'langmatches'
  | 'bound'
  | 'isIRI'
  | 'isLiteral';

export const OP_LABEL: Record<Op, string> = {
  '=': 'equals',
  '!=': 'differs from',
  '<': 'less than',
  '<=': 'at most',
  '>': 'more than',
  '>=': 'at least',
  contains: 'contains',
  strstarts: 'starts with',
  strends: 'ends with',
  regex: 'matches the pattern',
  langmatches: 'has the language',
  bound: 'is bound',
  isIRI: 'is an IRI',
  isLiteral: 'is a literal',
};

/** The forms that take no value. */
export const NO_VALUE = new Set<Op>(['bound', 'isIRI', 'isLiteral']);

const NUMERIC = [
  'integer',
  'decimal',
  'double',
  'float',
  'int',
  'long',
  'short',
  'byte',
  'nonNegativeInteger',
  'positiveInteger',
  'negativeInteger',
  'nonPositiveInteger',
  'unsignedInt',
  'unsignedLong',
  'unsignedShort',
  'unsignedByte',
].map((t) => XSD + t);
const TEMPORAL = ['date', 'dateTime', 'dateTimeStamp', 'time', 'gYear', 'gYearMonth'].map(
  (t) => XSD + t,
);

/** What kind of values a datatype holds, for the forms it offers. */
export function kindOf(datatype: string): 'number' | 'date' | 'text' | 'lang' | 'other' {
  if (NUMERIC.includes(datatype)) return 'number';
  if (TEMPORAL.includes(datatype)) return 'date';
  if (datatype === RDF_LANG) return 'lang';
  if (datatype === XSD + 'string') return 'text';
  return 'other';
}

/**
 * The forms for a variable whose values were seen with `datatypes` (null when nothing is
 * known, which offers every form): equality always, ranges for numbers and dates,
 * `CONTAINS`, `STRSTARTS`, `STRENDS` and `REGEX` for strings, `LANGMATCHES` for
 * language-tagged strings, and `BOUND`, `isIRI` and `isLiteral`.
 */
export function formsFor(datatypes: string[] | null): Op[] {
  const all: Op[] = [
    '=',
    '!=',
    '<',
    '<=',
    '>',
    '>=',
    'contains',
    'strstarts',
    'strends',
    'regex',
    'langmatches',
    'bound',
    'isIRI',
    'isLiteral',
  ];
  if (!datatypes) return all;
  const kinds = new Set(datatypes.map(kindOf));
  const out: Op[] = ['=', '!='];
  if (kinds.has('number') || kinds.has('date')) out.push('<', '<=', '>', '>=');
  if (kinds.has('text') || kinds.has('lang') || kinds.has('other'))
    out.push('contains', 'strstarts', 'strends', 'regex');
  if (kinds.has('lang')) out.push('langmatches');
  out.push('bound', 'isIRI', 'isLiteral');
  return out;
}

/** A string as a SPARQL literal. */
export function quote(s: string): string {
  return `"${s.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n').replace(/\r/g, '\\r')}"`;
}

/** A prefixed name, such as `ex:alice` or `:a`. */
const PNAME = /^(?:[A-Za-z][\w-]*(?:\.[\w-]+)*)?:[\w-]+(?:\.[\w-]+)*$/u;

/**
 * A value typed into a form as a constant: an IRI or prefixed name as written, a number
 * as written, a date with its datatype, a literal already written as one, else a string.
 */
export function constant(value: string, datatype?: string | null): string {
  const v = value.trim();
  if (/^<[^<>\s]*>$/.test(v)) return v;
  if (/^"(?:[^"\\]|\\.)*"(?:@[a-zA-Z-]+|\^\^\S+)?$/.test(v)) return v;
  if (
    /^[+-]?(\d+\.?\d*([eE][+-]?\d+)?|\.\d+([eE][+-]?\d+)?)$/.test(v) &&
    (!datatype || kindOf(datatype) === 'number' || kindOf(datatype) === 'other')
  )
    return v;
  if (datatype && kindOf(datatype) === 'date') return `${quote(v)}^^<${datatype}>`;
  if ((v === 'true' || v === 'false') && (!datatype || datatype === `${XSD}boolean`)) return v;
  // a prefixed name is an IRI, whose prefix the pane declares when the dataset does
  if (PNAME.test(v)) return v;
  return quote(v);
}

/** The expression a form writes for `?v`. A regex takes `flags`, `"i"` for a new filter,
 * and none when empty. */
export function expression(
  op: Op,
  v: string,
  value: string,
  datatype?: string | null,
  flags = 'i',
): string {
  const x = `?${v.replace(/^[?$]/, '')}`;
  switch (op) {
    case 'contains':
      return `CONTAINS(${x}, ${quote(value)})`;
    case 'strstarts':
      return `STRSTARTS(${x}, ${quote(value)})`;
    case 'strends':
      return `STRENDS(${x}, ${quote(value)})`;
    case 'regex':
      return flags
        ? `REGEX(${x}, ${quote(value)}, ${quote(flags)})`
        : `REGEX(${x}, ${quote(value)})`;
    case 'langmatches':
      return `LANGMATCHES(LANG(${x}), ${quote(value)})`;
    case 'bound':
      return `BOUND(${x})`;
    case 'isIRI':
      return `isIRI(${x})`;
    case 'isLiteral':
      return `isLiteral(${x})`;
    default:
      return `${x} ${op} ${constant(value, datatype)}`;
  }
}

/** A string literal's text, else the constant as written. */
function unquote(c: string): string {
  const m = /^"((?:[^"\\]|\\.)*)"$/.exec(c) ?? /^'((?:[^'\\]|\\.)*)'$/.exec(c);
  return m ? m[1].replace(/\\(.)/g, (_, x) => (x === 'n' ? '\n' : x === 'r' ? '\r' : x)) : c;
}

/** The form of a chip, to open it again: its operator, its value as typed, and a
 * regex's flags. */
export function formOf(f: FilterForm): { op: Op; value: string; flags?: string } {
  const op = f.op as Op;
  const arg = f.args[0] ?? '';
  const value = ['contains', 'strstarts', 'strends', 'regex', 'langmatches'].includes(op)
    ? unquote(arg)
    : arg;
  return op === 'regex' ? { op, value, flags: f.args[1] ? unquote(f.args[1]) : '' } : { op, value };
}
