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
  | 'list'
  | 'term';

/** One option of a field's `sh:or` or `sh:xone` over value shapes. */
export type Alternative = {
  shape: string;
  label: string;
  widget: Widget;
  constraints: Constraints;
};

/** SHACL 1.2 list constraints: each value of the field is an `rdf:List`. */
export type ListConstraints = {
  minLength?: number;
  maxLength?: number;
  unique?: boolean;
  memberShapes: string[];
  /** The value constraints every member meets. */
  member: Constraints;
  memberWidget: Widget;
};

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
  /** The options of `sh:or` (one must hold) or `sh:xone` (exactly one must hold). */
  or?: { kind: 'or' | 'xone'; options: Alternative[] };
  list?: ListConstraints;
};

export type FormValue = {
  term: Term;
  graph: string | null;
  label?: string;
  readOnly?: boolean;
  /** The members of an `rdf:List` value of a list field, in order. */
  items?: FormValue[];
};

export type ReadOnlyReason =
  | 'readOnly'
  | 'protected'
  | 'graph'
  | 'complex-path'
  | 'too-many'
  | 'list'
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

/** The fields of a nested node: a pooled child, or the template of a new one. */
export type NodeModel = Pick<FormModel, 'fields' | 'groups' | 'types' | 'graph' | 'other'>;

/** A stored blank-node child of the form, with the visible edges it hangs from. */
export type NestedNode = NodeModel & {
  id: string;
  term: Term;
  anchors: { parent: string; field: string; graph: string | null; inverse: boolean }[];
};

/** The empty form of a new child under a field whose `sh:node` shapes are `shapes`. */
/** The empty form of a new child under a field whose `constraints.node` is `node`. */
export type NestedTemplate = NodeModel & { node: string[] };

export type FormModel = {
  formFormat: 1;
  focus: string | null;
  types: string[];
  commit: number;
  branch: string;
  editable: boolean;
  shapes: {
    source: 'guard' | 'graphs' | 'drafted' | 'skos';
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
  /** With `nested=true`: the stored blank-node children and the templates of new ones. */
  nestedNodes?: NestedNode[];
  templates?: NestedTemplate[];
  /** The nested model refused past its allowances, so nested values are read-only. */
  nested?: { refused: string };
};

/** A validation result of the server check, with the field it belongs to. */
export type FieldResult = {
  field: string | null;
  /** The nested node whose field the result is on. */
  node?: Term;
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
  protected: 'You do not have permission to change this property.',
  graph: 'New values would go to a graph you cannot write to.',
  'complex-path': "This field's path is too complex to edit in a form.",
  'too-many': 'This field has more values than a form can list.',
  list: 'This field does not hold exactly one well-formed list.',
  state: 'This form shows a past state, or you do not have permission to save.',
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
  'list',
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

/**
 * How a field with a list of allowed values renders them all at once: checkboxes when
 * the shapes allow several values (`sh:maxCount` above 1) or the resource has several,
 * else a radio group for the radio widget. A select and every other widget render one
 * input per value instead (`null`).
 */
export function choiceGroup(f: FormField, widget: Widget): 'checks' | 'radio' | null {
  if (!(f.constraints.in?.length ?? 0)) return null;
  const max = f.constraints.maxCount;
  const several = (max !== undefined && max > 1) || f.values.length > 1;
  if ((widget === 'radio' || widget === 'select') && several) return 'checks';
  return widget === 'radio' ? 'radio' : null;
}

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
  // `x` drops whitespace from the pattern and `q` quotes it, which JavaScript cannot do
  // with the same meaning: the server checks such patterns
  if (/[xq]/.test(p.flags ?? '')) return null;
  const flags = [...new Set(`u${(p.flags ?? '').replace(/[^ims]/g, '')}`)].join('');
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

/** A field with only the value constraints `c`: one option, or a list's members. */
export function valueField(f: FormField, c: Constraints, widget: Widget): FormField {
  return { ...f, widget, constraints: { ...c, minCount: undefined, maxCount: undefined } };
}

/** The pseudo-fields of a field's options, which check and edit one value each. */
export const optionFields = (f: FormField): FormField[] =>
  (f.constraints.or?.options ?? []).map((o) => valueField(f, o.constraints, o.widget));

/** The option a value takes: the first whose checks it passes, else the first. */
export function optionOf(f: FormField, t: Term | null): number {
  if (!t) return 0;
  const i = optionFields(f).findIndex((o) => checkField(o, [t]).length === 0);
  return Math.max(i, 0);
}

/** The pseudo-field of a list field's members. */
export const memberField = (f: FormField): FormField | null =>
  f.constraints.list
    ? valueField(f, f.constraints.list.member, f.constraints.list.memberWidget)
    : null;

/** The members of a list field's stored list, in order. */
export const listItems = (f: FormField): FormValue[] => f.values[0]?.items ?? [];

/**
 * The problems of a list field's members (`values`) that need no data: the members'
 * constraints, the list lengths and repeated members.
 */
function checkList(f: FormField, values: (Term | null)[]): Problem[] {
  const l = f.constraints.list!;
  const m = memberField(f)!;
  const out: Problem[] = [];
  values.forEach((v, i) => {
    if (v) for (const p of checkField(m, [v])) out.push({ index: i, message: p.message });
  });
  const present = values.filter((v): v is Term => v !== null);
  const n = present.length;
  if (l.minLength !== undefined && n < l.minLength)
    out.push({ index: null, message: `needs at least ${l.minLength} members` });
  if (l.maxLength !== undefined && n > l.maxLength)
    out.push({ index: null, message: `takes at most ${l.maxLength} members` });
  if (l.unique && present.some((v, i) => present.findIndex((x) => sameTerm(x, v)) !== i))
    out.push({ index: null, message: 'has a member twice' });
  const had = f.values.length > 0;
  if ((f.constraints.minCount ?? 0) >= 1 && !had && n === 0)
    out.push({ index: null, message: 'is required' });
  return out;
}

/** The problems of a field's values that need no data (§2.4). */
export function checkField(f: FormField, values: (Term | null)[]): Problem[] {
  if (f.constraints.list) return checkList(f, values);
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
    else if (kind === 'BlankNode' && v.type !== 'bnode') problem('must be a blank node');
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
  if (c.or?.options.length) {
    const opts = optionFields(f);
    const names = c.or.options.map((o) => o.label).join(', ');
    // an option with constraints only the server checks may match or not: the browser
    // proves a match only for an option it checks entirely
    const decided = (o: FormField) =>
      !o.constraints.class?.length && !o.constraints.node?.length && !o.constraints.server?.length;
    for (const { v, i } of present) {
      const pass = opts.filter((o) => checkField(o, [v]).length === 0);
      if (pass.length === 0) out.push({ index: i, message: `must be one of: ${names}` });
      else if (c.or.kind === 'xone' && pass.filter(decided).length > 1)
        out.push({ index: i, message: `must be exactly one of: ${names}` });
    }
  }
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
  /** the timezone of a date-time value, which the input does not show */
  zone?: string;
  /** the value sits in a graph the person cannot write, so it is shown and kept */
  readOnly?: boolean;
  /** the option of an `sh:or` or `sh:xone` field the value takes */
  option?: number;
};

let entryKey = 0;

/** A new entry for a term (or an empty one). */
export function entryOf(
  term: Term | null,
  widget: Widget,
  label?: string,
  readOnly?: boolean,
): Entry {
  entryKey += 1;
  const key = `e${entryKey}`;
  if (!term) return { key, term: null, text: '', lang: '' };
  let text = term.type === 'triple' ? '' : term.value;
  let zone: string | undefined;
  if (widget === 'datetime' && term.type === 'literal') {
    zone = /(Z|[+-]\d\d:\d\d)$/.exec(text)?.[1];
    if (zone) text = text.slice(0, -zone.length);
  }
  if (widget === 'term' && term.type === 'literal') text = termText(term);
  return {
    key,
    term,
    text,
    lang: term.type === 'literal' ? (term['xml:lang'] ?? '') : '',
    ...(label ? { label } : {}),
    ...(zone ? { zone } : {}),
    ...(readOnly ? { readOnly } : {}),
  };
}

/** The offset of the browser's timezone at a local date-time, as `+hh:mm` or `Z`. */
export function localZone(text: string): string {
  const d = new Date(text);
  const m = Number.isNaN(d.getTime()) ? new Date().getTimezoneOffset() : d.getTimezoneOffset();
  if (m === 0) return 'Z';
  const a = Math.abs(m);
  const hh = String(Math.floor(a / 60)).padStart(2, '0');
  const mm = String(a % 60).padStart(2, '0');
  return `${m > 0 ? '-' : '+'}${hh}:${mm}`;
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
  zone?: string,
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
  if (widget === 'datetime') {
    if (/T\d\d:\d\d$/.test(value)) value += ':00';
    // the value keeps its timezone; a new time stamp takes the browser's
    const z = zone ?? (dt === `${XSD}dateTimeStamp` ? localZone(value) : undefined);
    if (z) value += z;
  }
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

/**
 * A change to one field: values to remove and to add, or for a list field the members
 * its list takes (`list`).
 */
export type FieldChange = { field: string; remove: Term[]; add: Term[]; list?: Term[] };

/** A change to a nested child (`node`), which hangs from `parent`'s `field`. */
export type NodeChange = {
  node: Term;
  /** absent for the root */
  parent?: Term;
  field: string;
  changes: FieldChange[];
  expect?: Record<string, Term[]>;
};

export type ChangeSet = {
  changes: FieldChange[];
  /** the values the form loaded, by changed field */
  expect: Record<string, Term[]>;
  /** changes to nested children, which commit with the root's */
  nested?: NodeChange[];
};

/** The state key of a field: its id on the root, `{node} {id}` on a nested node. */
export const fieldKey = (node: string | null, id: string) => (node ? `${node} ${id}` : id);

/** The changes of one node's fields, read from `current` by their keys. */
function fieldChanges(
  fields: FormField[],
  node: string | null,
  current: Record<string, (Term | null)[]>,
  loadedToo: boolean,
): { changes: FieldChange[]; expect: Record<string, Term[]> } {
  const changes: FieldChange[] = [];
  const expect: Record<string, Term[]> = {};
  for (const f of fields) {
    if (!f.editable) continue;
    const now = (current[fieldKey(node, f.id)] ?? []).filter((t): t is Term => t !== null);
    if (f.constraints.list) {
      const loaded = listItems(f).map((v) => v.term);
      const same = loaded.length === now.length && loaded.every((t, i) => sameTerm(t, now[i]));
      if (same && (f.values.length > 0 || !now.length)) continue;
      changes.push({ field: f.id, remove: [], add: [], list: now });
      if (loadedToo) expect[f.id] = loaded;
      continue;
    }
    const loaded = f.values.map((v) => v.term);
    const d = diffValues(loaded, now);
    if (!d.remove.length && !d.add.length) continue;
    changes.push({ field: f.id, ...d });
    if (loadedToo) expect[f.id] = distinct(loaded);
  }
  return { changes, expect };
}

/** A nested node the form shows: a stored child, or a new one from a template. */
export type FormNode = {
  /** the blank label: stored, or `new…` for a new child */
  key: string;
  term: Term;
  fields: FormField[];
  isNew: boolean;
};

/** The nested nodes reachable from the root through the current values, breadth first. */
export function reachable(
  model: Pick<FormModel, 'fields'>,
  current: Record<string, (Term | null)[]>,
  nodes: Record<string, FormNode>,
): { node: FormNode; parent: string | null; field: string }[] {
  const out: { node: FormNode; parent: string | null; field: string }[] = [];
  const seen = new Set<string>();
  const queue: { owner: string | null; fields: FormField[] }[] = [
    { owner: null, fields: model.fields },
  ];
  while (queue.length) {
    const { owner, fields } = queue.shift()!;
    for (const f of fields) {
      if (!f.constraints.node?.length) continue;
      for (const t of current[fieldKey(owner, f.id)] ?? []) {
        if (t?.type !== 'bnode' || seen.has(t.value) || !nodes[t.value]) continue;
        seen.add(t.value);
        const node = nodes[t.value];
        out.push({ node, parent: owner, field: f.id });
        queue.push({ owner: node.key, fields: node.fields });
      }
    }
  }
  return out;
}

/**
 * Turn stored nested nodes that no longer hang from their parent into new ones, with
 * the stored nodes reachable from them: their fields lose their loaded values, so a save
 * writes every value the form shows again under new nodes. A person who keeps their own
 * values of a field whose child another commit removed gets the whole child back, not
 * only the fields they changed. `null` when the form cannot rebuild the children whole:
 * a value it cannot write (a read-only field or a value in another graph), a node
 * reached twice (a cycle or a shared node), or a node that a node outside them also
 * holds through another field than `edge`, the field whose conflict is resolved.
 */
export function detachLost(
  nodes: Record<string, FormNode>,
  current: Record<string, (Term | null)[]>,
  lost: string[],
  edge: string,
): Record<string, FormNode> | null {
  const closure = new Set<string>();
  const queue = [...lost];
  while (queue.length) {
    const key = queue.shift()!;
    const n = nodes[key];
    if (!n || n.isNew) continue;
    if (closure.has(key)) return null;
    closure.add(key);
    for (const f of n.fields) {
      const now = current[fieldKey(key, f.id)] ?? [];
      if ((!f.editable && f.values.length) || f.values.some((v) => v.readOnly)) return null;
      if (!f.constraints.node?.length) continue;
      for (const t of now)
        if (t?.type === 'bnode' && nodes[t.value] && !nodes[t.value].isNew) queue.push(t.value);
    }
  }
  // a node of the closure that a node outside it holds stays stored for that one
  for (const [k, es] of Object.entries(current)) {
    const owner = k.includes(' ') ? k.slice(0, k.indexOf(' ')) : null;
    if (owner !== null && closure.has(owner)) continue;
    if (owner === null || k === edge) continue;
    if (es.some((t) => t?.type === 'bnode' && closure.has(t.value))) return null;
  }
  const out = { ...nodes };
  for (const key of closure) {
    const n = nodes[key];
    out[key] = { ...n, fields: n.fields.map((f) => ({ ...f, values: [] })), isNew: true };
  }
  return out;
}

/** Whether a template's shapes are a field's `sh:node` shapes. */
export const templateFits = (t: NestedTemplate, f: FormField) =>
  t.node.length === (f.constraints.node?.length ?? 0) &&
  t.node.every((s) => f.constraints.node?.includes(s));

/** Whether a field shows its blank-node values as nested forms. */
export const nestedField = (f: FormField) =>
  !!f.constraints.node?.length &&
  !f.inverse &&
  (widgetOf(f) === 'nested' || f.values.some((v) => v.term.type === 'bnode'));

/**
 * The change set of a form: the changed editable fields and their loaded values, and the
 * changes of the nested children still reachable from the root through the current
 * values, with the entries of the nodes on the way to a changed or new one.
 */
export function changeSet(
  model: FormModel,
  current: Record<string, (Term | null)[]>,
  nodes: Record<string, FormNode> = {},
): ChangeSet {
  const root = fieldChanges(model.fields, null, current, true);
  const nested: NodeChange[] = [];
  // the reachable nodes, with the edge each was reached through
  const reached = reachable(model, current, nodes);
  const entries = new Map<string, NodeChange>();
  const needed = new Set<string>();
  for (const r of reached) {
    const c = fieldChanges(r.node.fields, r.node.key, current, !r.node.isNew);
    const e: NodeChange = {
      node: r.node.term,
      ...(r.parent ? { parent: nodes[r.parent].term } : {}),
      field: r.field,
      changes: c.changes,
      ...(r.node.isNew ? {} : { expect: c.expect }),
    };
    entries.set(r.node.key, e);
    if (r.node.isNew || c.changes.length) {
      // the node and every node on its way from the root
      let k: string | null = r.node.key;
      while (k && !needed.has(k)) {
        needed.add(k);
        k = reached.find((x) => x.node.key === k)?.parent ?? null;
      }
    }
  }
  for (const r of reached) if (needed.has(r.node.key)) nested.push(entries.get(r.node.key)!);
  return { changes: root.changes, expect: root.expect, ...(nested.length ? { nested } : {}) };
}

/** The number of changed values: a replaced value counts once, as do an added and a removed one. */
export const changeCount = (s: ChangeSet) => {
  const of = (cs: FieldChange[]) =>
    cs.reduce((n, c) => n + (c.list ? 1 : Math.max(c.remove.length, c.add.length)), 0);
  return of(s.changes) + (s.nested ?? []).reduce((n, e) => n + of(e.changes), 0);
};

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
