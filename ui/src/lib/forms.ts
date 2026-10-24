// Editing forms generated from SHACL shapes (docs/specs/U02-shape-driven-forms.md): the
// form model the server answers, the checks that need no data (§2.4), the widget of each
// field (§2.3) and the change set a save sends (§3.2). Everything here is pure, so the
// checks run on each change without a request.

import type { Term } from './api';
import { termKey, type PrefixMap } from './rdf';
import { RDF_LANG_STRING, XSD, datatypeName, rangeValue, validLexical, xsdLocal } from './xsd';

export type Widget =
  | 'radio'
  | 'select'
  | 'checkbox'
  | 'boolean-select'
  | 'date'
  | 'datetime'
  | 'number'
  | 'text-lang'
  | 'textarea'
  | 'text'
  | 'entity'
  | 'iri'
  | 'nested'
  | 'term';

export type Constraints = {
  minCount?: number;
  maxCount?: number;
  datatype?: string;
  nodeKind?: string;
  class?: string[];
  in?: Term[];
  hasValue?: Term[];
  minInclusive?: Term;
  minExclusive?: Term;
  maxInclusive?: Term;
  maxExclusive?: Term;
  minLength?: number;
  maxLength?: number;
  pattern?: { pattern: string; flags?: string }[];
  languageIn?: string[];
  uniqueLang?: boolean;
  singleLine?: boolean;
  defaultValue?: Term;
  node?: string[];
  /** Components only the server checks. */
  server?: string[];
};

export type FormValue = { term: Term; graph: string | null; label?: string; readOnly?: boolean };

export type ReadOnlyReason =
  | 'readOnly'
  | 'protected'
  | 'graph'
  | 'complex-path'
  | 'too-many'
  | 'state';

export type FormField = {
  id: string;
  path: string;
  inverse: boolean;
  editable: boolean;
  reason?: ReadOnlyReason;
  label: string;
  description?: string;
  group: string | null;
  order: number | null;
  widget: Widget;
  editor?: string;
  viewer?: string;
  constraints: Constraints;
  sources: string[];
  values: FormValue[];
  truncated?: boolean;
};

export type FormModel = {
  formFormat: 1;
  focus: string | null;
  types: string[];
  commit: number;
  branch: string;
  editable: boolean;
  shapes: {
    source: 'guard' | 'graphs' | 'drafted';
    nodeShapes: string[];
    missing?: string;
    unknownHints: string[];
  };
  groups: { iri: string | null; label: string; order: number | null }[];
  fields: FormField[];
  other: { predicate: string; values: FormValue[] }[];
  graph: string | null;
  closed: boolean;
  save: { to: 'main' | 'branch'; branch: string; exists: boolean; mainProtected: boolean };
  guard: { language: 'shacl' | 'shex'; mode: 'reject' | 'warn' | 'off' } | null;
};

/** A validation result of the server check, with the field it belongs to. */
export type FieldResult = {
  field: string | null;
  focusNode: Term;
  resultPath: Term | { type: 'path'; value: string } | null;
  value: Term | null;
  sourceShape: Term;
  sourceConstraintComponent: Term;
  severity: Term;
  messages: string[];
};

/** The words a read-only field shows for its reason. */
export const REASONS: Record<ReadOnlyReason, string> = {
  readOnly: 'The shape marks this field read-only (dash:readOnly).',
  protected: 'Your access protects this property from changes.',
  graph: 'New values would go to a graph you cannot write.',
  'complex-path': 'The path of this field has no single triple to write.',
  'too-many': 'The field holds more values than a form lists.',
  state: 'This form shows a past state, or you may not save changes.',
};

const DASH = 'http://datashapes.org/dash#';

/** The widgets of the DASH editors (§2.3), which override the constraints' choice. */
const EDITORS: Record<string, Widget> = {
  TextFieldEditor: 'text',
  TextAreaEditor: 'textarea',
  TextFieldWithLangEditor: 'text-lang',
  DatePickerEditor: 'date',
  DateTimePickerEditor: 'datetime',
  BooleanSelectEditor: 'boolean-select',
  EnumSelectEditor: 'select',
  InstancesSelectEditor: 'entity',
  AutoCompleteEditor: 'entity',
  URIEditor: 'iri',
  DetailsEditor: 'nested',
};

const KNOWN: Set<string> = new Set([
  'radio',
  'select',
  'checkbox',
  'boolean-select',
  'date',
  'datetime',
  'number',
  'text-lang',
  'textarea',
  'text',
  'entity',
  'iri',
  'nested',
  'term',
]);

/** The widget of a field: its DASH editor's when the UI knows it, else the server's. */
export function widgetOf(f: Pick<FormField, 'widget' | 'editor' | 'constraints'>): Widget {
  const ed = f.editor?.startsWith(DASH) ? EDITORS[f.editor.slice(DASH.length)] : undefined;
  // an enum editor needs values to choose from
  if (ed === 'select' && !f.constraints.in?.length) return KNOWN.has(f.widget) ? f.widget : 'term';
  if (ed) return ed;
  return KNOWN.has(f.widget) ? f.widget : 'term';
}

/** How a read-only value shows (§2.3): the DASH viewer, or a label for an IRI by default. */
export type Viewer = 'label' | 'uri' | 'literal' | 'term';

/** The viewer of a field: its known `dash:viewer`, else `label`, as DASH's default for IRIs. */
export function viewerOf(f: Pick<FormField, 'viewer'>): Viewer {
  switch (f.viewer?.startsWith(DASH) ? f.viewer.slice(DASH.length) : '') {
    case 'URIViewer':
      return 'uri';
    case 'LiteralViewer':
      return 'literal';
    case 'LabelViewer':
      return 'label';
    default:
      return 'label';
  }
}

/** Whether a field takes several values (no `sh:maxCount`, or one above 1). */
export const multiValued = (f: FormField) => (f.constraints.maxCount ?? Infinity) > 1;

/** Whether a field is required (`sh:minCount` 1 or more). */
export const required = (f: FormField) => (f.constraints.minCount ?? 0) >= 1;

/** Two terms are the same RDF term (simple literals are `xsd:string`, tags in any case). */
export function sameTerm(a: Term, b: Term): boolean {
  if (a.type !== b.type) return false;
  if (a.type === 'literal' && b.type === 'literal') {
    const dt = (t: typeof a) => (t['xml:lang'] ? RDF_LANG_STRING : (t.datatype ?? `${XSD}string`));
    return (
      a.value === b.value &&
      dt(a) === dt(b) &&
      (a['xml:lang'] ?? '').toLowerCase() === (b['xml:lang'] ?? '').toLowerCase()
    );
  }
  return termKey(a) === termKey(b);
}

/** A term's string value, as `str()` gives it. */
const str = (t: Term): string => (t.type === 'triple' ? '' : t.value);

/** SHACL's language matching of `sh:languageIn`: the tag or a subtag of it. */
export function langMatches(tag: string, range: string): boolean {
  const t = tag.toLowerCase();
  const r = range.toLowerCase();
  return t === r || t.startsWith(`${r}-`);
}

/** A problem of one value (`index`) or of the whole field (`index: null`). */
export type Problem = { index: number | null; message: string };

/** Compile a `sh:pattern` the way the browser checks it, or null when it cannot. */
export function compilePattern(p: { pattern: string; flags?: string }): RegExp | null {
  const flags = [...new Set(`u${(p.flags ?? '').replace(/[^imsx]/g, '')}`)]
    .filter((f) => f !== 'x')
    .join('');
  try {
    return new RegExp(p.pattern, flags);
  } catch {
    return null;
  }
}

/** The patterns of a field that JavaScript cannot compile: the server checks them. */
export function serverPatterns(f: FormField): string[] {
  return (f.constraints.pattern ?? [])
    .filter((p) => compilePattern(p) === null)
    .map((p) => p.pattern);
}

function boundText(t: Term): string {
  return t.type === 'literal' ? t.value : str(t);
}

/** The problems of a field's values that need no data (§2.4). */
export function checkField(f: FormField, values: (Term | null)[]): Problem[] {
  const c = f.constraints;
  const out: Problem[] = [];
  const present = values
    .map((v, i) => ({ v, i }))
    .filter((x): x is { v: Term; i: number } => x.v !== null);
  const kind = c.nodeKind?.replace('http://www.w3.org/ns/shacl#', '');
  const patterns = (c.pattern ?? []).map((p) => ({ p, re: compilePattern(p) }));
  const ranges: [Term | undefined, (a: number, b: number) => boolean, string][] = [
    [c.minInclusive, (a, b) => a >= b, 'at least'],
    [c.minExclusive, (a, b) => a > b, 'more than'],
    [c.maxInclusive, (a, b) => a <= b, 'at most'],
    [c.maxExclusive, (a, b) => a < b, 'less than'],
  ];
  for (const { v, i } of values.map((v, i) => ({ v, i }))) {
    if (v === null) continue;
    const problem = (message: string) => out.push({ index: i, message });
    const isLit = v.type === 'literal';
    if (kind === 'IRI' && v.type !== 'uri') problem('must be an IRI');
    else if (kind === 'Literal' && !isLit) problem('must be a literal');
    else if (kind === 'BlankNode') problem('must be a blank node');
    if ((c.class?.length ?? 0) > 0 && v.type !== 'uri')
      problem('must be a resource, not a literal');
    if (c.datatype) {
      if (!isLit) problem(`must be ${datatypeName(c.datatype)}`);
      else {
        const dt = v['xml:lang'] ? RDF_LANG_STRING : (v.datatype ?? `${XSD}string`);
        if (dt !== c.datatype) problem(`must be ${datatypeName(c.datatype)}`);
        else if (!validLexical(v.value, c.datatype)) problem(`must be ${datatypeName(c.datatype)}`);
      }
    }
    const s = str(v);
    const len = [...s].length;
    if (c.minLength !== undefined && len < c.minLength)
      problem(`must be at least ${c.minLength} characters`);
    if (c.maxLength !== undefined && len > c.maxLength)
      problem(`must be at most ${c.maxLength} characters`);
    for (const { p, re } of patterns) if (re && !re.test(s)) problem(`must match ${p.pattern}`);
    if (isLit) {
      for (const [bound, ok, words] of ranges) {
        if (!bound) continue;
        const bdt = bound.type === 'literal' ? bound.datatype : undefined;
        const a = rangeValue(v.value, v.datatype ?? c.datatype);
        const b = rangeValue(boundText(bound), bdt);
        if (a !== null && b !== null && !ok(a, b)) problem(`must be ${words} ${boundText(bound)}`);
      }
    }
    if (c.in && !c.in.some((x) => sameTerm(x, v))) problem('must be one of the listed values');
    if (c.languageIn) {
      const tag = isLit ? v['xml:lang'] : undefined;
      if (!tag || !c.languageIn.some((r) => langMatches(tag, r)))
        problem(`must have a language of ${c.languageIn.join(', ')}`);
    }
  }
  const n = present.length;
  if (c.minCount !== undefined && n < c.minCount)
    out.push({
      index: null,
      message: c.minCount === 1 ? 'is required' : `needs at least ${c.minCount} values`,
    });
  if (c.maxCount !== undefined && n > c.maxCount)
    out.push({
      index: null,
      message: c.maxCount === 1 ? 'takes one value' : `takes at most ${c.maxCount} values`,
    });
  if (c.uniqueLang) {
    const seen = new Set<string>();
    for (const { v } of present) {
      const tag = v.type === 'literal' ? v['xml:lang']?.toLowerCase() : undefined;
      if (!tag) continue;
      if (seen.has(tag)) {
        out.push({ index: null, message: `has two values in language ${tag}` });
        break;
      }
      seen.add(tag);
    }
  }
  for (const hv of c.hasValue ?? [])
    if (!present.some(({ v }) => sameTerm(v, hv)))
      out.push({ index: null, message: `must include ${boundText(hv)}` });
  return out;
}

/** What a person typed into one value of a field. */
export type Entry = {
  /** a stable key for the list */
  key: string;
  /** the term, once the text names one; null while empty or unreadable */
  term: Term | null;
  /** the text of a text, number or date input */
  text: string;
  /** the language of a text-with-language input */
  lang: string;
  /** why the text names no term */
  error?: string;
  /** the label of an IRI value, for the entity picker */
  label?: string;
};

let entryKey = 0;

/** A new entry for a term (or an empty one). */
export function entryOf(term: Term | null, widget: Widget, label?: string): Entry {
  entryKey += 1;
  const key = `e${entryKey}`;
  if (!term) return { key, term: null, text: '', lang: '' };
  let text = term.type === 'triple' ? '' : term.value;
  if (widget === 'datetime' && term.type === 'literal')
    text = text.replace(/(Z|[+-]\d\d:\d\d)$/, '');
  if (widget === 'term' && term.type === 'literal') text = termText(term);
  return {
    key,
    term,
    text,
    lang: term.type === 'literal' ? (term['xml:lang'] ?? '') : '',
    ...(label ? { label } : {}),
  };
}

/** A literal in SPARQL syntax for the term input. */
export function termText(t: Term): string {
  if (t.type === 'uri') return t.value;
  if (t.type !== 'literal') return '';
  const v = JSON.stringify(t.value);
  if (t['xml:lang']) return `${v}@${t['xml:lang']}`;
  if (t.datatype && t.datatype !== `${XSD}string`) return `${v}^^<${t.datatype}>`;
  return v;
}

/**
 * The term the text of a term input names: an IRI in angle brackets or bare, a prefixed
 * name, a literal in SPARQL syntax, a number or a boolean.
 */
export function parseTermText(text: string, prefixes: PrefixMap): Term | { error: string } {
  const t = text.trim();
  if (!t) return { error: 'empty' };
  const iri = /^<([^<>"{}|^`\\\s]*)>$/.exec(t);
  if (iri) return { type: 'uri', value: iri[1] };
  if (/^(https?|urn|mailto|file|tag|did):[^\s<>"]*$/i.test(t)) return { type: 'uri', value: t };
  const lit =
    /^"((?:[^"\\]|\\.)*)"(?:@([a-zA-Z]+(?:-[a-zA-Z0-9]+)*)|\^\^(?:<([^>]*)>|([A-Za-z][\w.-]*)?:(\S*)))?$/.exec(
      t,
    );
  if (lit) {
    let value: string;
    try {
      value = JSON.parse(`"${lit[1]}"`);
    } catch {
      return { error: 'the literal has an invalid escape' };
    }
    if (lit[2]) return { type: 'literal', value, 'xml:lang': lit[2] };
    let dt = lit[3];
    if (dt === undefined && lit[5] !== undefined) {
      const ns = prefixes[lit[4] ?? ''];
      if (ns === undefined) return { error: `unknown prefix ${lit[4] ?? ''}:` };
      dt = ns + lit[5];
    }
    return dt && dt !== `${XSD}string`
      ? { type: 'literal', value, datatype: dt }
      : { type: 'literal', value };
  }
  if (/^[+-]?\d+$/.test(t)) return { type: 'literal', value: t, datatype: `${XSD}integer` };
  if (/^[+-]?(\d+\.\d*|\.\d+)$/.test(t))
    return { type: 'literal', value: t, datatype: `${XSD}decimal` };
  if (/^[+-]?(\d+(\.\d*)?|\.\d+)[eE][+-]?\d+$/.test(t))
    return { type: 'literal', value: t, datatype: `${XSD}double` };
  if (t === 'true' || t === 'false')
    return { type: 'literal', value: t, datatype: `${XSD}boolean` };
  const pn = /^([A-Za-z][\w.-]*)?:(\S*)$/.exec(t);
  if (pn) {
    const ns = prefixes[pn[1] ?? ''];
    if (ns === undefined) return { error: `unknown prefix ${pn[1] ?? ''}:` };
    return { type: 'uri', value: ns + pn[2] };
  }
  return { error: 'write an IRI, a prefixed name or a literal such as "text"@en' };
}

/**
 * The term an entry's text names under a field's widget and constraints: a literal of
 * the field's datatype (a language-tagged one for text with a language), or for the term
 * input whatever the text says.
 */
export function entryTerm(
  f: FormField,
  widget: Widget,
  text: string,
  lang: string,
  prefixes: PrefixMap,
): { term: Term | null; error?: string } {
  if (widget === 'term' || widget === 'iri') {
    if (!text.trim()) return { term: null };
    const r = parseTermText(text, prefixes);
    if ('error' in r) return { term: null, error: r.error };
    if (widget === 'iri' && r.type !== 'uri') return { term: null, error: 'write an IRI' };
    return { term: r };
  }
  if (text === '') return { term: null };
  const dt = f.constraints.datatype;
  if (widget === 'text-lang' || dt === RDF_LANG_STRING) {
    if (!lang.trim()) return { term: { type: 'literal', value: text } };
    return { term: { type: 'literal', value: text, 'xml:lang': lang.trim() } };
  }
  let value = text;
  if (widget === 'datetime' && /T\d\d:\d\d$/.test(value)) value += ':00';
  if (!dt || dt === `${XSD}string`) return { term: { type: 'literal', value } };
  return { term: { type: 'literal', value, datatype: dt } };
}

/** The values of a field as a set of terms, without repeats. */
function distinct(terms: Term[]): Term[] {
  const out: Term[] = [];
  for (const t of terms) if (!out.some((x) => sameTerm(x, t))) out.push(t);
  return out;
}

/** The values to remove and to add to turn `loaded` into `current`. */
export function diffValues(loaded: Term[], current: Term[]): { remove: Term[]; add: Term[] } {
  const a = distinct(loaded);
  const b = distinct(current);
  return {
    remove: a.filter((t) => !b.some((x) => sameTerm(x, t))),
    add: b.filter((t) => !a.some((x) => sameTerm(x, t))),
  };
}

export type FieldChange = { field: string; remove: Term[]; add: Term[] };

export type ChangeSet = {
  changes: FieldChange[];
  /** the values the form loaded, by changed field */
  expect: Record<string, Term[]>;
};

/** The change set of a form: the changed editable fields and their loaded values. */
export function changeSet(model: FormModel, current: Record<string, (Term | null)[]>): ChangeSet {
  const changes: FieldChange[] = [];
  const expect: Record<string, Term[]> = {};
  for (const f of model.fields) {
    if (!f.editable) continue;
    const now = (current[f.id] ?? []).filter((t): t is Term => t !== null);
    const loaded = f.values.map((v) => v.term);
    const d = diffValues(loaded, now);
    if (!d.remove.length && !d.add.length) continue;
    changes.push({ field: f.id, ...d });
    expect[f.id] = distinct(loaded);
  }
  return { changes, expect };
}

/** The number of changed values: a replaced value counts once, as do an added and a removed one. */
export const changeCount = (s: ChangeSet) =>
  s.changes.reduce((n, c) => n + Math.max(c.remove.length, c.add.length), 0);

/** What pressing Save does (§2.4). */
export type SavePlan = 'save' | 'check' | 'confirm' | 'blocked';

/**
 * What pressing Save does. With a SHACL guard in `reject` mode a save waits for a server
 * check of the same changes: it runs one first (`check`) and is `blocked` while that check
 * says the guard would reject the write, since the write would fail anyway. Otherwise it
 * saves, asking first (`confirm`) when there are problems.
 */
export function savePlan(
  guard: FormModel['guard'],
  problems: number,
  check: { conforms: boolean | null; rejected: boolean; current: boolean } | null,
): SavePlan {
  if (guard?.language === 'shacl' && guard.mode === 'reject') {
    if (!check?.current) return 'check';
    return check.rejected ? 'blocked' : 'save';
  }
  const current = check?.current ? check : null;
  return problems > 0 || current?.conforms === false ? 'confirm' : 'save';
}

/** The groups of a model with their fields, ungrouped fields first. */
export function grouped(
  model: FormModel,
): { group: FormModel['groups'][number] | null; fields: FormField[] }[] {
  const out: { group: FormModel['groups'][number] | null; fields: FormField[] }[] = [];
  const free = model.fields.filter((f) => !f.group);
  if (free.length) out.push({ group: null, fields: free });
  for (const g of model.groups) {
    const fields = model.fields.filter((f) => f.group === g.iri);
    if (fields.length) out.push({ group: g, fields });
  }
  return out;
}

/** The text of a result: its first message, or the component's name. */
export function resultText(r: FieldResult): string {
  if (r.messages?.length) return r.messages[0];
  const c = r.sourceConstraintComponent?.type === 'uri' ? r.sourceConstraintComponent.value : '';
  return (
    c.replace('http://www.w3.org/ns/shacl#', '').replace(/ConstraintComponent$/, '') ||
    'violates a shape'
  );
}

/** Whether a result blocks (a violation) rather than warns. */
export const blocking = (r: FieldResult) =>
  r.severity?.type === 'uri' && r.severity.value.endsWith('#Violation');

/** The DOM id of a field's first input, for the error summary's links. */
export const fieldDomId = (id: string) => `field-${hash(id)}`;

/** A short stable hash of a string (FNV-1a), for DOM ids. */
function hash(s: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0).toString(36);
}

/** The XSD local name of a field's datatype, for inputs. */
export const fieldType = (f: FormField) => xsdLocal(f.constraints.datatype);
