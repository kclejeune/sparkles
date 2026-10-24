// Layered settings (spec C19): the client of `/$/settings/{ds}/{kind}`, and the logic the
// settings forms share. A kind's answer says where each field comes from, which fields the
// operator locked and which runtime values a lock ignores. The helpers here turn that into
// what a form row shows (its source, a lock, a reset), and turn a form's edits into the
// RFC 7396 merge patch a save sends. The routes of server-wide kinds answer in the same
// shape, so the helpers take the kind's URL rather than a dataset.

import { ApiError, errorMessage, json } from './api';

const enc = encodeURIComponent;

/** Where a field's value comes from. */
export type FieldSource = 'default' | 'declared' | 'runtime' | 'locked';

/** A JSON object, as the settings routes hold them. */
export type JsonObject = { [k: string]: unknown };

/** A runtime value used in place of a different value of the server's configuration. */
export type SettingsOverride = { path: string; declared: unknown; runtime: unknown };

/** `GET /$/settings/{ds}/{kind}`, and `GET /$/server/settings/{kind}` with `scope`. */
export type SettingsKind = {
  dataset?: string;
  scope?: 'server';
  kind: string;
  effective: JsonObject;
  declared: JsonObject;
  runtime: JsonObject;
  /** The source of every leaf of `effective`, by dotted path. */
  sources: Record<string, FieldSource>;
  locked: string[];
  overridden: string[];
  /** The runtime values that replace a declared value (absent from older servers). */
  overrides?: SettingsOverride[];
  status: { valid: boolean; error?: string };
  etag: string;
};

/** `GET /$/settings/{ds}`. */
export type DatasetSettings = { dataset: string; kinds: Record<string, SettingsKind> };

/** The kinds of a dataset, in the order the Settings tab shows them. */
export const DATASET_KINDS = [
  'assistant',
  'memory',
  'ingest',
  'graphrag',
  'paths',
  'forms',
] as const;
export type DatasetKind = (typeof DATASET_KINDS)[number];

/** The URL of a dataset's kind. */
export const datasetKindUrl = (ds: string, kind: string) => `/$/settings/${enc(ds)}/${enc(kind)}`;

/** The headers of a write: JSON, and `If-Match` when the caller read an ETag. */
function writeInit(method: string, etag?: string | null, body?: unknown): RequestInit {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  if (etag) headers['If-Match'] = etag;
  return {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  };
}

/** Read a kind. */
export const readKind = (url: string, signal?: AbortSignal) =>
  json<SettingsKind>(url, { signal, cache: 'no-store' });

/** Merge `patch` into the runtime layer (RFC 7396): `null` removes a runtime value. */
export const patchKind = (url: string, patch: JsonObject, etag?: string | null) =>
  json<SettingsKind>(url, writeInit('PATCH', etag, patch));

/** Make the effective object equal to `body`. */
export const putKind = (url: string, body: JsonObject, etag?: string | null) =>
  json<SettingsKind>(url, writeInit('PUT', etag, body));

/** Clear the runtime layer, or the runtime value of one field. */
export const resetKind = (url: string, field?: string | null, etag?: string | null) =>
  json<SettingsKind>(field ? `${url}?field=${enc(field)}` : url, writeInit('DELETE', etag));

/** Every kind of a dataset. */
export const datasetSettings = (ds: string, signal?: AbortSignal) =>
  json<DatasetSettings>(`/$/settings/${enc(ds)}`, { signal, cache: 'no-store' });

// --- paths -------------------------------------------------------------------------

/** A dotted path as its members; a member that holds a dot escapes it with `\`. */
export function parsePath(s: string): string[] {
  const out: string[] = [];
  let cur = '';
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (c === '\\' && i + 1 < s.length) cur += s[++i];
    else if (c === '.') {
      out.push(cur);
      cur = '';
    } else cur += c;
  }
  out.push(cur);
  return out;
}

/** Members as a dotted path, the inverse of `parsePath`. */
export const pathString = (p: readonly string[]) =>
  p.map((k) => k.replace(/\\/g, '\\\\').replace(/\./g, '\\.')).join('.');

const isObject = (v: unknown): v is JsonObject =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** Whether `prefix` is `path` or a path above it. */
export function startsWith(path: readonly string[], prefix: readonly string[]) {
  return prefix.length <= path.length && prefix.every((k, i) => path[i] === k);
}

/** The value at `path`, or undefined. */
export function getAt(v: unknown, path: readonly string[]): unknown {
  let cur = v;
  for (const k of path) {
    if (!isObject(cur) || !(k in cur)) return undefined;
    cur = cur[k];
  }
  return cur;
}

/**
 * Whether a layer sets the field at `path`: the field itself, something below it, or a
 * field above it to something other than an object (as the server decides sources).
 */
export function covers(layer: unknown, path: readonly string[]): boolean {
  let cur = layer;
  for (const k of path) {
    if (!isObject(cur)) return cur !== undefined;
    if (!(k in cur)) return false;
    cur = cur[k];
  }
  return true;
}

/** Set `value` at `path` of `obj`, making the objects on the way. */
export function setAt(obj: JsonObject, path: readonly string[], value: unknown) {
  let cur = obj;
  path.forEach((k, i) => {
    if (i === path.length - 1) cur[k] = value;
    else {
      if (!isObject(cur[k])) cur[k] = {};
      cur = cur[k] as JsonObject;
    }
  });
}

/**
 * A copy of the object `obj` with `value` at `path`, or without the member at `path`
 * when `value` is undefined, in which case objects the removal leaves empty go too.
 */
export function withAt(obj: JsonObject, path: readonly string[], value: unknown): JsonObject {
  const [k, ...rest] = path;
  const out: JsonObject = { ...obj };
  if (rest.length === 0) {
    if (value === undefined) delete out[k];
    else out[k] = value;
    return out;
  }
  const inner = withAt(isObject(out[k]) ? (out[k] as JsonObject) : {}, rest, value);
  if (Object.keys(inner).length === 0 && value === undefined) delete out[k];
  else out[k] = inner;
  return out;
}

export function deepEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (Array.isArray(a) && Array.isArray(b))
    return a.length === b.length && a.every((x, i) => deepEqual(x, b[i]));
  if (isObject(a) && isObject(b)) {
    const ka = Object.keys(a);
    return ka.length === Object.keys(b).length && ka.every((k) => k in b && deepEqual(a[k], b[k]));
  }
  return false;
}

/**
 * The RFC 7396 merge patch that turns `from` into `to`: members `to` leaves out become
 * `null`, objects are compared member by member, and anything else is replaced. Undefined
 * when they are equal.
 */
export function diffPatch(from: unknown, to: unknown): unknown {
  if (deepEqual(from, to)) return undefined;
  if (!isObject(from) || !isObject(to)) return to;
  const out: JsonObject = {};
  for (const k of Object.keys(from)) if (!(k in to)) out[k] = null;
  for (const [k, v] of Object.entries(to)) {
    const d = diffPatch(from[k], v);
    if (d !== undefined) out[k] = d;
  }
  return out;
}

// --- what a form row shows ------------------------------------------------------------

/** What a settings row shows about one field. */
export type FieldInfo = {
  path: string;
  /** Where the value comes from. */
  source: FieldSource;
  /** The operator locked the field, or a field above it. */
  locked: boolean;
  /** The operator locked a field below this one, so part of it is fixed. */
  partlyLocked: boolean;
  /** The runtime layer holds a value for the field (a reset clears it). */
  runtime: boolean;
  /** A lock ignores a runtime value of the field. */
  overridden: boolean;
  /** The runtime value a lock ignores, when the field has one. */
  ignored?: unknown;
  /** The server's configuration sets the field, so a reset brings its value back. */
  hasDeclared: boolean;
  /** The value of the server's configuration that a runtime value overrides. */
  overrides?: unknown;
};

const RANK: Record<FieldSource, number> = { runtime: 3, declared: 2, locked: 1, default: 0 };

/** The source, lock and reset state of the field at `path` (dotted) of a kind. */
export function fieldInfo(k: SettingsKind, path: string): FieldInfo {
  const p = parsePath(path);
  const locks = k.locked.map(parsePath);
  const locked = locks.some((l) => startsWith(p, l));
  const partlyLocked = !locked && locks.some((l) => startsWith(l, p));
  const overridden = k.overridden.map(parsePath).some((o) => startsWith(o, p) || startsWith(p, o));
  const runtime = covers(k.runtime, p);
  let source: FieldSource | null = null;
  if (locked) source = 'locked';
  else {
    // the field itself, or the most telling source of the leaves below it
    for (const [key, s] of Object.entries(k.sources)) {
      const kp = parsePath(key);
      if (startsWith(kp, p) || startsWith(p, kp))
        if (source == null || RANK[s] > RANK[source]) source = s;
    }
    if (source == null || source === 'default')
      source = runtime ? 'runtime' : covers(k.declared, p) ? 'declared' : (source ?? 'default');
  }
  const hasDeclared = covers(k.declared, p);
  const info: FieldInfo = { path, source, locked, partlyLocked, runtime, overridden, hasDeclared };
  if (overridden && runtime) info.ignored = getAt(k.runtime, p);
  if (!locked && runtime) {
    const d = overriddenValue(k, p);
    if (d !== undefined) info.overrides = d;
  }
  return info;
}

/**
 * The declared value that the runtime values at or below `p` replace, from the answer's
 * `overrides`. A server without `overrides` is answered from the layers.
 */
function overriddenValue(k: SettingsKind, p: string[]): unknown {
  if (!k.overrides) {
    const d = getAt(k.declared, p);
    return d !== undefined && !deepEqual(d, getAt(k.runtime, p)) ? d : undefined;
  }
  const hits = k.overrides.filter((o) => {
    const op = parsePath(o.path);
    return startsWith(op, p) || startsWith(p, op);
  });
  if (hits.length === 0) return undefined;
  if (hits.length === 1 && deepEqual(parsePath(hits[0].path), p)) return hits[0].declared;
  // the declared object of a field whose members are overridden, or the declared value
  // above the field that a runtime object replaces
  return getAt(k.declared, p) ?? hits[0].declared;
}

/** Words for a source, as a row's label and tooltip say it. */
export function sourceText(f: FieldInfo): { label: string; title: string } | null {
  if (f.locked)
    return {
      label: 'locked',
      title:
        "The operator locked this field in the server's configuration, so it cannot be changed here.",
    };
  switch (f.source) {
    case 'declared':
      return {
        label: 'server config',
        title:
          "The value comes from the server's configuration. A change here overrides it until it is reset.",
      };
    case 'runtime':
      return f.overrides === undefined
        ? {
            label: 'changed',
            title: f.hasDeclared
              ? "The value was changed here or through the API. It equals the server's configuration."
              : 'The value was changed here or through the API. Reset it to go back to the default.',
          }
        : {
            label: 'overrides server config',
            title: `The value was changed here or through the API, so the server's configuration (${valueText(f.overrides)}) is not used. Use the server config to go back to it.`,
          };
    default:
      return null;
  }
}

/** The words of a field's reset button: what the field goes back to. */
export function resetText(f: FieldInfo): { text: string; title: string } {
  if (f.overridden)
    return {
      text: 'Remove change',
      title: 'Remove the runtime value that the lock ignores',
    };
  if (f.hasDeclared)
    return {
      text: 'Use server config',
      title: "Remove the change, so the field takes the value of the server's configuration",
    };
  return {
    text: 'Reset to default',
    title: 'Remove the change, so the field takes its built-in default',
  };
}

/** A value in one line, shortened to `max` characters. */
export function valueText(v: unknown, max = 80): string {
  const s = v === undefined ? 'unset' : JSON.stringify(v);
  return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}

/** The overrides of a kind: none for a server that predates them. */
export const overridesOf = (k: SettingsKind | null | undefined) => k?.overrides ?? [];

/** "2 fields override the server config". */
export function overridesSummary(n: number): string {
  return n === 1 ? '1 field overrides the server config' : `${n} fields override the server config`;
}

/**
 * Clear the runtime values of `fields`, one `DELETE ?field=` after another, each with the
 * `ETag` of the answer before it. The answer is the kind after the last one.
 */
export async function resetFields(
  url: string,
  fields: readonly string[],
  etag?: string | null,
): Promise<SettingsKind> {
  let k: SettingsKind | null = null;
  let tag = etag;
  for (const f of fields) {
    k = await resetKind(url, f, tag);
    tag = k.etag;
  }
  return k ?? readKind(url);
}

// --- form fields ---------------------------------------------------------------------

export type FieldType =
  | 'bool'
  | 'enum'
  | 'int'
  | 'number'
  | 'text'
  | 'secret'
  | 'lines'
  | 'json'
  | 'list'
  | 'records'
  | 'map'
  | 'pairs';

/** The types whose form value is JSON (an array or an object) rather than text. */
export const STRUCTURED: readonly FieldType[] = ['list', 'records', 'map', 'pairs'];

export const isStructured = (d: FieldDef) => STRUCTURED.includes(d.type);

/** A field of a settings form. */
export type FieldDef = {
  /** The dotted path of the field, or of a member of an entry for an entry's fields. */
  path: string;
  label: string;
  type: FieldType;
  /** What the field does, in a line. */
  help?: string;
  /** `enum`: the values. */
  options?: readonly string[];
  /** The value the server uses when the field is not set, shown as a placeholder. */
  fallback?: unknown;
  /** `int` and `number`: the smallest and largest value. */
  min?: number;
  max?: number;
  /** `number`: a value must be greater than this. */
  above?: number;
  /** `text`, `secret` and the entries of `list`: a regular expression the value matches. */
  pattern?: string;
  /** What a value that does not match `pattern` is told. */
  patternText?: string;
  /** `text`: the longest value. */
  maxLength?: number;
  /** A unit shown after the input, such as `seconds` or `tokens`. */
  unit?: string;
  /** A field the server leaves unset by default: an empty input removes the value. */
  optional?: boolean;
  /** `text`: the value is kept as typed, with its leading and trailing spaces. */
  literal?: boolean;
  /** `enum`: the options are the known values, and other values are checked elsewhere. */
  openOptions?: boolean;
  placeholder?: string;
  /** The heading of the group of fields the field belongs to. */
  group?: string;
  /** A heading inside the group, shown before the first field of a run that has it. */
  section?: string;
  /** `records` and `map`: the fields of one entry, with paths inside the entry. */
  entry?: FieldDef[];
  /** `map` and `pairs`: a regular expression an entry's name matches. */
  keyPattern?: string;
  /** `map` and `pairs`: what an entry's name is called, such as `model` or `header`. */
  keyLabel?: string;
  /** `records`: the list used while the field is unset, in words. */
  unsetText?: string;
  /** `list` and `records`: what an empty list means, in words. */
  emptyText?: string;
};

/** A form's values: text for inputs, booleans for checkboxes, JSON for structured fields. */
export type FormValues = Record<string, unknown>;

/** A JSON value's copy. */
export const clone = <T>(v: T): T => (v === undefined ? v : (JSON.parse(JSON.stringify(v)) as T));

/** The form value of a field from a value. */
export function toForm(def: FieldDef, v: unknown): unknown {
  const value = v === undefined ? (def.type === 'bool' ? def.fallback : undefined) : v;
  switch (def.type) {
    case 'bool':
      return value === true;
    case 'lines':
      return Array.isArray(value) ? value.map(String).join('\n') : '';
    case 'json':
      return value === undefined || value === null ? '' : JSON.stringify(value, null, 2);
    case 'list':
    case 'records':
    case 'map':
    case 'pairs':
      return value === null ? undefined : clone(value);
    default:
      return value === undefined || value === null ? '' : String(value);
  }
}

/** The form of a kind's effective object. */
export function formOf(defs: readonly FieldDef[], effective: unknown): FormValues {
  return Object.fromEntries(
    defs.map((d) => [d.path, toForm(d, getAt(effective, parsePath(d.path)))]),
  );
}

/**
 * The value an input's text stands for in a structured field's entry: nothing for an
 * empty input, a number for a numeric field whose text reads as one, and the text itself
 * otherwise, which the checks then refuse for a numeric field.
 */
export function inputValue(def: FieldDef, text: string): unknown {
  const t = text.trim();
  if (t === '') return undefined;
  if (def.type === 'int' || def.type === 'number') {
    const n = Number(t);
    return Number.isFinite(n) ? n : t;
  }
  return def.type === 'text' || def.type === 'secret' ? text : t;
}

/** What is wrong with a value of a scalar field, or null. */
export function checkValue(def: FieldDef, v: unknown): string | null {
  if (v === undefined || v === null) {
    if (def.optional || def.type === 'bool' || def.type === 'json' || def.type === 'enum')
      return null;
    return `${def.label} needs a value`;
  }
  switch (def.type) {
    case 'bool':
      return typeof v === 'boolean' ? null : `${def.label} is on or off`;
    case 'int':
    case 'number': {
      const whole = def.type === 'int';
      if (typeof v !== 'number' || !Number.isFinite(v) || (whole && !Number.isSafeInteger(v)))
        return `${def.label} must be a ${whole ? 'whole ' : ''}number`;
      if (def.min != null && v < def.min) return `${def.label} is at least ${def.min}`;
      if (def.above != null && v <= def.above) return `${def.label} is more than ${def.above}`;
      if (def.max != null && v > def.max) return `${def.label} is at most ${def.max}`;
      return null;
    }
    case 'enum':
      return typeof v === 'string' && (def.openOptions || (def.options ?? []).includes(v))
        ? null
        : `${def.label} is not one of the values`;
    case 'text':
    case 'secret': {
      if (typeof v !== 'string') return `${def.label} is text`;
      if (!def.optional && v.trim() === '') return `${def.label} needs a value`;
      if (def.maxLength != null && v.length > def.maxLength)
        return `${def.label} has at most ${def.maxLength} characters`;
      if (def.pattern && !new RegExp(def.pattern, 'u').test(v))
        return def.patternText ?? `${def.label} does not have the right form`;
      return null;
    }
    default:
      return null;
  }
}

const sub = (base: string, ...members: string[]) => pathString([...parsePath(base), ...members]);

/**
 * What is wrong with a structured field's value, by the dotted path below the field: an
 * entry of a list by its index, a member of an entry by `index.member` or `name.member`,
 * and the field itself by `''`.
 */
export function checkStructured(def: FieldDef, v: unknown): Record<string, string> {
  const out: Record<string, string> = {};
  if (v === undefined || v === null) return out;
  const entryFields = (at: string[], entry: unknown) => {
    if (!isObject(entry)) {
      out[pathString(at)] = `Each ${def.keyLabel ?? 'entry'} is an object`;
      return;
    }
    for (const f of def.entry ?? []) {
      const e = checkValue(f, getAt(entry, parsePath(f.path)));
      if (e) out[pathString([...at, ...parsePath(f.path)])] = e;
    }
  };
  switch (def.type) {
    case 'list': {
      if (!Array.isArray(v)) return { '': `${def.label} is a list` };
      const seen = new Set<string>();
      v.forEach((x, i) => {
        const at = String(i);
        if (typeof x !== 'string' || !x.trim()) out[at] = 'An entry needs a value';
        else if (seen.has(x.trim())) out[at] = `${x.trim()} is listed twice`;
        else if (def.pattern && !new RegExp(def.pattern, 'u').test(x))
          out[at] = def.patternText ?? `${x} does not have the right form`;
        if (typeof x === 'string') seen.add(x.trim());
      });
      return out;
    }
    case 'records':
      if (!Array.isArray(v)) return { '': `${def.label} is a list` };
      v.forEach((x, i) => entryFields([String(i)], x));
      return out;
    case 'map':
    case 'pairs': {
      if (!isObject(v)) return { '': `${def.label} is an object` };
      for (const [name, x] of Object.entries(v)) {
        if (def.keyPattern && !new RegExp(def.keyPattern, 'u').test(name))
          out[pathString([name])] =
            `${name} is not a ${def.keyLabel ?? 'name'} that the server accepts`;
        else if (def.type === 'pairs') {
          if (typeof x !== 'string') out[pathString([name])] = `The value of ${name} is text`;
        } else entryFields([name], x);
      }
      return out;
    }
    default:
      return out;
  }
}

/** A structured field's value as it is sent: list entries trimmed. */
function normalized(def: FieldDef, v: unknown): unknown {
  if (def.type === 'list' && Array.isArray(v)) return v.map((x) => String(x).trim());
  return clone(v);
}

/** A field's value from its form value: `undefined` for an empty optional field. */
export function fromForm(
  def: FieldDef,
  f: unknown,
): { ok: unknown } | { error: string; errors?: Record<string, string> } {
  if (def.type === 'bool') return { ok: f === true };
  if (isStructured(def)) {
    const errors = checkStructured(def, f);
    const keys = Object.keys(errors);
    if (keys.length) return { error: errors[keys[0]], errors };
    return { ok: f === undefined || f === null ? undefined : normalized(def, f) };
  }
  const t = String(f ?? '').trim();
  if (t === '') {
    // an optional list left empty is unset, as `allowedModels` is
    if (def.type === 'lines') return def.optional ? { ok: undefined } : { ok: [] };
    if (def.type === 'json' || def.optional || def.type === 'enum') return { ok: undefined };
    return { error: `${def.label} needs a value` };
  }
  switch (def.type) {
    case 'int':
    case 'number': {
      const n = Number(t);
      const e = checkValue(def, Number.isFinite(n) ? n : t);
      return e ? { error: e } : { ok: n };
    }
    case 'lines':
      return {
        ok: t
          .split('\n')
          .map((x) => x.trim())
          .filter(Boolean),
      };
    case 'json':
      try {
        return { ok: JSON.parse(t) };
      } catch (e) {
        return { error: `${def.label} is not JSON: ${e instanceof Error ? e.message : e}` };
      }
    default: {
      const v = def.literal ? String(f) : t;
      const e = checkValue(def, v);
      return e ? { error: e } : { ok: v };
    }
  }
}

/**
 * The placeholders of a map entry's members: the value each inherits from `inherited`,
 * except for a member of a nested object the entry sets itself, which the entry's object
 * replaces whole on the server, so it takes its own default.
 */
export function inheritedFor(
  fields: readonly FieldDef[],
  entry: unknown,
  inherited: Record<string, unknown>,
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const f of fields) {
    const p = parsePath(f.path);
    if (p.length > 1 && getAt(entry, p.slice(0, -1)) !== undefined) {
      if (f.fallback !== undefined) out[f.path] = f.fallback;
    } else if (inherited[f.path] !== undefined) out[f.path] = inherited[f.path];
  }
  return out;
}

/**
 * The merge patch of a form's edits: each field whose form value changed is set, an
 * emptied optional field is `null` (the runtime value goes, and the field falls back to
 * the settings file or the default), and an object field sends only the members that
 * changed. `errors` lists fields whose value does not read, and `entryErrors` the
 * members of structured fields that do not, by their whole dotted path.
 */
export function formPatch(
  defs: readonly FieldDef[],
  initial: FormValues,
  form: FormValues,
  effective: unknown,
): {
  patch: JsonObject;
  changed: string[];
  errors: Record<string, string>;
  entryErrors: Record<string, string>;
} {
  const patch: JsonObject = {};
  const changed: string[] = [];
  const errors: Record<string, string> = {};
  const entryErrors: Record<string, string> = {};
  for (const d of defs) {
    if (deepEqual(form[d.path], initial[d.path])) continue;
    const r = fromForm(d, form[d.path]);
    if ('error' in r) {
      errors[d.path] = r.error;
      for (const [k, e] of Object.entries(r.errors ?? {}))
        entryErrors[k === '' ? d.path : sub(d.path, ...parsePath(k))] = e;
      continue;
    }
    const p = parsePath(d.path);
    const before = getAt(effective, p);
    let value: unknown = r.ok === undefined ? null : r.ok;
    if (
      (d.type === 'json' || d.type === 'map' || d.type === 'pairs') &&
      isObject(before) &&
      isObject(r.ok)
    )
      value = diffPatch(before, r.ok);
    if (value === undefined || (value === null && before === undefined)) continue;
    setAt(patch, p, value);
    changed.push(d.path);
  }
  return { patch, changed, errors, entryErrors };
}

/** The fields of a form whose value differs from the loaded one. */
export const dirtyFields = (defs: readonly FieldDef[], initial: FormValues, form: FormValues) =>
  defs.filter((d) => !deepEqual(form[d.path], initial[d.path])).map((d) => d.path);

/** The field of `defs` that holds the dotted `path`: the field itself or one above it. */
export function fieldAt(defs: readonly FieldDef[], path: string): FieldDef | undefined {
  const p = parsePath(path);
  let best: FieldDef | undefined;
  for (const d of defs) {
    const dp = parsePath(d.path);
    if (startsWith(p, dp) && (!best || dp.length > parsePath(best.path).length)) best = d;
  }
  return best;
}

// --- the runtime layer as JSON --------------------------------------------------------

/** The runtime layer as the JSON editor shows it. */
export const runtimeText = (k: SettingsKind) => JSON.stringify(k.runtime ?? {}, null, 2);

/** A runtime layer as the JSON editor shows it. */
export const layerText = (layer: unknown) => JSON.stringify(layer ?? {}, null, 2);

/**
 * `patch` applied to `target` as RFC 7396 says, in a copy: `null` removes a member, an
 * object merges member by member, and anything else replaces. Members keep their order,
 * and new ones come last.
 */
export function applyPatch(target: unknown, patch: unknown): unknown {
  if (!isObject(patch)) return clone(patch);
  const out: JsonObject = isObject(target) ? { ...target } : {};
  for (const [k, v] of Object.entries(patch)) {
    if (v === null) delete out[k];
    else out[k] = applyPatch(out[k], v);
  }
  return out;
}

/** `v` without empty objects, which the server drops from a runtime layer as well. */
export function pruned(v: unknown): unknown {
  if (!isObject(v)) return v;
  const out: JsonObject = {};
  for (const [k, x] of Object.entries(v)) {
    const p = pruned(x);
    if (!(isObject(p) && Object.keys(p).length === 0)) out[k] = p;
  }
  return out;
}

/**
 * The effective object once the runtime layer is `layer`, as the form shows it before a
 * save: the members the layer changes take its values, a runtime value it removes falls
 * back to the declared one, a `null` it holds removes the declared member, and locked
 * fields keep their value. The server's answer to the save is the exact one.
 */
export function previewEffective(k: SettingsKind, layer: JsonObject): JsonObject {
  const patch = diffPatch(k.runtime ?? {}, layer);
  if (patch === undefined) return k.effective;
  const walk = (eff: unknown, p: unknown, declared: unknown, l: unknown): unknown => {
    if (!isObject(p)) return clone(p);
    const out: JsonObject = isObject(eff) ? { ...eff } : {};
    for (const [key, v] of Object.entries(p)) {
      const d = isObject(declared) ? declared[key] : undefined;
      const lv = isObject(l) ? l[key] : undefined;
      if (v === null) {
        if (lv === null || d === undefined || d === null) delete out[key];
        else out[key] = clone(d);
      } else out[key] = walk(out[key], v, d, lv);
    }
    return out;
  };
  let out = walk(k.effective, patch, k.declared, layer) as JsonObject;
  // copies along the path, since the members the patch leaves alone are the kind's own
  for (const l of k.locked) {
    const p = parsePath(l);
    const v = getAt(k.effective, p);
    if (v !== undefined) out = withAt(out, p, clone(v));
  }
  return out;
}

/**
 * The editor's text as a runtime layer, or why it does not do. The layer is what the
 * form's edits then apply to.
 */
export function parseLayer(text: string): { layer: JsonObject } | { error: string } {
  let v: unknown;
  try {
    v = JSON.parse(text.trim() || '{}');
  } catch (e) {
    return { error: `Not JSON: ${e instanceof Error ? e.message : e}` };
  }
  if (!isObject(v)) return { error: 'The runtime layer is a JSON object.' };
  return { layer: v };
}

/**
 * The merge patch that makes the runtime layer equal to the editor's text, or why the
 * text does not do. Null when nothing changed.
 */
export function runtimePatch(
  k: SettingsKind,
  text: string,
): { patch: JsonObject | null } | { error: string } {
  const r = parseLayer(text);
  if ('error' in r) return r;
  const d = diffPatch(k.runtime ?? {}, r.layer);
  return { patch: d === undefined ? null : (d as JsonObject) };
}

/**
 * The state of a settings form that the JSON editor shares (C19 §10): the runtime layer
 * the editor last set (`base`), the form of the effective object it gives (`baseForm`),
 * and the form as edited. The layer a save sends is `base` with the form's edits, so the
 * editor and the form show one change, and members the form does not know stay as the
 * editor or the server left them.
 */
export type FormState = {
  base: JsonObject;
  baseForm: FormValues;
  form: FormValues;
};

/** The state of a kind as the server answered it. */
export function formState(defs: readonly FieldDef[], k: SettingsKind): FormState {
  const base = clone(k.runtime ?? {});
  const baseForm = formOf(defs, k.effective);
  return { base, baseForm, form: clone(baseForm) };
}

/** The state once the editor's text is the runtime layer `layer`; edits of the form go. */
export function rebase(defs: readonly FieldDef[], k: SettingsKind, layer: JsonObject): FormState {
  const baseForm = formOf(defs, previewEffective(k, layer));
  return { base: clone(layer), baseForm, form: clone(baseForm) };
}

/**
 * The runtime layer a save would leave, the merge patch it sends (null when it sends
 * nothing), and the fields and members that do not read.
 */
export function pendingLayer(
  defs: readonly FieldDef[],
  k: SettingsKind,
  s: FormState,
): {
  layer: JsonObject;
  patch: JsonObject | null;
  errors: Record<string, string>;
  entryErrors: Record<string, string>;
} {
  const r = formPatch(defs, s.baseForm, s.form, previewEffective(k, s.base));
  const layer = pruned(applyPatch(s.base, r.patch)) as JsonObject;
  const d = diffPatch(k.runtime ?? {}, layer);
  return {
    layer,
    patch: d === undefined ? null : (d as JsonObject),
    errors: r.errors,
    entryErrors: r.entryErrors,
  };
}

// --- failed writes -------------------------------------------------------------------

/** What a failed write means for the form. */
export type WriteFailure =
  | { kind: 'stale'; message: string }
  | { kind: 'locked'; fields: string[]; message: string }
  | { kind: 'invalid'; message: string; field?: string }
  | { kind: 'forbidden'; message: string }
  | { kind: 'other'; message: string };

export function writeFailure(e: unknown): WriteFailure {
  const message = errorMessage(e);
  if (!(e instanceof ApiError)) return { kind: 'other', message };
  if (e.status === 412) return { kind: 'stale', message };
  if (e.status === 409 && e.code === 'locked-by-config') {
    const f = e.body?.fields;
    return {
      kind: 'locked',
      fields: Array.isArray(f) ? f.map(String) : [],
      message,
    };
  }
  if (e.status === 400) {
    const f = e.body?.field;
    return typeof f === 'string'
      ? { kind: 'invalid', message, field: f }
      : { kind: 'invalid', message };
  }
  if (e.status === 403) return { kind: 'forbidden', message };
  return { kind: 'other', message };
}

/** Whether a form field is one of the fields a `locked-by-config` answer names. */
export function conflicts(path: string, fields: readonly string[]): boolean {
  const p = parsePath(path);
  return fields.map(parsePath).some((f) => startsWith(f, p) || startsWith(p, f));
}
