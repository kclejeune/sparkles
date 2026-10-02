// Helpers of the query page's saved (stored) queries: names, the variables of a query,
// the parameter form, the values of a run, and the definition a save sends.

import type { StoredDefinition, StoredParam, StoredParamType } from './api';

export const PARAM_TYPES: StoredParamType[] = [
  'string',
  'iri',
  'integer',
  'decimal',
  'double',
  'boolean',
  'date',
  'dateTime',
  'literal',
  'term',
];

const NAME_RE = /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/;

/** Whether a stored query name is valid: letters, digits, `_` and `-`, at most 64. */
export const validName = (name: string) => NAME_RE.test(name);

/** Request parameters of a run that cannot be query parameters. */
export const RESERVED = new Set([
  'query',
  'update',
  'format',
  'output',
  'results',
  'timeout',
  'nocache',
  'reasoning',
  'at',
  'version',
  'send',
  'receipt',
  'default-graph-uri',
  'named-graph-uri',
  'memory-mb',
  'max-result-mb',
  'max-rows',
  'max-rows-produced',
]);

/**
 * The variables of a query text, in order of first appearance, without `?`/`$`. Strings,
 * IRIs and comments are skipped, so `"?x"` or `<http://e/?a=b>` give nothing.
 */
export function queryVariables(text: string): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  let i = 0;
  const n = text.length;
  while (i < n) {
    const c = text[i];
    if (c === '#') {
      while (i < n && text[i] !== '\n') i++;
    } else if (c === '<') {
      // an IRI has no spaces; a `<` comparison is followed by one
      const end = text.indexOf('>', i + 1);
      const iri = end < 0 ? '' : text.slice(i + 1, end);
      i = end > 0 && !/\s/.test(iri) ? end + 1 : i + 1;
    } else if (c === '"' || c === "'") {
      const long = text.startsWith(c.repeat(3), i);
      const close = long ? c.repeat(3) : c;
      i += close.length;
      while (i < n && !text.startsWith(close, i)) i += text[i] === '\\' ? 2 : 1;
      i += close.length;
    } else if (c === '?' || c === '$') {
      const m = /^[A-Za-z0-9_·À-￿]+/.exec(text.slice(i + 1));
      if (m) {
        if (!seen.has(m[0])) {
          seen.add(m[0]);
          out.push(m[0]);
        }
        i += 1 + m[0].length;
      } else i++;
    } else i++;
  }
  return out;
}

export type FormValue = string | boolean;

/** The parameter form, filled with the defaults. */
export function formDefaults(
  params: Record<string, StoredParam> | undefined,
): Record<string, FormValue> {
  const out: Record<string, FormValue> = {};
  for (const [name, p] of Object.entries(params ?? {})) {
    if (p.type === 'boolean') out[name] = p.default === true || p.default === 'true';
    else out[name] = p.default == null ? '' : String(p.default);
  }
  return out;
}

/** Whether a run must give a value: required unless it has a default or says otherwise. */
export const isRequired = (p: StoredParam) => p.required ?? p.default == null;

/**
 * The values a run sends: every filled field, booleans as `true`/`false`; and the
 * required parameters left empty.
 */
export function runValues(
  params: Record<string, StoredParam> | undefined,
  form: Record<string, FormValue>,
): { values: Record<string, string>; missing: string[] } {
  const values: Record<string, string> = {};
  const missing: string[] = [];
  for (const [name, p] of Object.entries(params ?? {})) {
    const v = form[name];
    if (p.type === 'boolean') {
      values[name] = v === true ? 'true' : 'false';
    } else if (typeof v === 'string' && v.trim() !== '') {
      values[name] = v.trim();
    } else if (isRequired(p)) {
      missing.push(name);
    }
  }
  return { values, missing };
}

/** The HTML input type of a parameter's field. */
export function inputType(t: StoredParamType): 'text' | 'number' | 'date' | 'checkbox' {
  switch (t) {
    case 'integer':
    case 'decimal':
    case 'double':
      return 'number';
    case 'date':
      return 'date';
    case 'boolean':
      return 'checkbox';
    default:
      return 'text';
  }
}

/** A hint for a parameter's text field. */
export function placeholder(t: StoredParamType): string {
  switch (t) {
    case 'iri':
      return '<http://…> or prefix:name';
    case 'dateTime':
      return '2026-01-31T12:00:00Z';
    case 'literal':
      return '"text"@en or "5"^^xsd:int';
    case 'term':
      return 'an IRI or a literal';
    default:
      return t;
  }
}

/** One parameter row of the save dialog. */
export type ParamRow = {
  name: string;
  type: StoredParamType;
  default: string;
  description: string;
};

/** The definition to store from the dialog's fields. */
export function buildDefinition(
  query: string,
  description: string,
  rows: ParamRow[],
): StoredDefinition {
  const parameters: Record<string, StoredParam> = {};
  for (const r of rows) {
    if (!r.name) continue;
    const p: StoredParam = { type: r.type };
    const d = r.default.trim();
    if (d !== '') p.default = r.type === 'boolean' ? d === 'true' : d;
    if (r.description.trim()) p.description = r.description.trim();
    parameters[r.name] = p;
  }
  const def: StoredDefinition = { query };
  if (description.trim()) def.description = description.trim();
  if (Object.keys(parameters).length) def.parameters = parameters;
  return def;
}

/** Problems with the dialog's fields, or an empty list. */
export function saveProblems(name: string, rows: ParamRow[]): string[] {
  const out: string[] = [];
  if (!validName(name))
    out.push('The name has 1–64 letters, digits, _ or -, and starts with a letter or digit.');
  const names = rows.map((r) => r.name).filter(Boolean);
  if (new Set(names).size !== names.length) out.push('A variable is a parameter twice.');
  for (const n of names) if (RESERVED.has(n)) out.push(`?${n} has a reserved name.`);
  return out;
}
