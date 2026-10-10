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

/** `GET /$/settings/{ds}/{kind}`. */
export type SettingsKind = {
  dataset?: string;
  kind: string;
  effective: JsonObject;
  declared: JsonObject;
  runtime: JsonObject;
  /** The source of every leaf of `effective`, by dotted path. */
  sources: Record<string, FieldSource>;
  locked: string[];
  overridden: string[];
  status: { valid: boolean; error?: string };
  etag: string;
};

/** `GET /$/settings/{ds}`. */
export type DatasetSettings = { dataset: string; kinds: Record<string, SettingsKind> };

/** The kinds of a dataset, in the order the Settings tab shows them. */
export const DATASET_KINDS = ['assistant', 'memory', 'ingest'] as const;
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
  /** The value of the server's settings file that a runtime value overrides. */
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
  const info: FieldInfo = { path, source, locked, partlyLocked, runtime, overridden };
  if (overridden && runtime) info.ignored = getAt(k.runtime, p);
  if (source === 'runtime') {
    const d = getAt(k.declared, p);
    if (d !== undefined) info.overrides = d;
  }
  return info;
}

/** Words for a source, as a row's label and tooltip say it. */
export function sourceText(f: FieldInfo): { label: string; title: string } | null {
  if (f.locked)
    return {
      label: 'locked',
      title:
        "The operator locked this field in the server's settings file, so it cannot be changed here.",
    };
  switch (f.source) {
    case 'declared':
      return {
        label: 'server config',
        title:
          "The value comes from the server's settings file. A change here overrides it until it is reset.",
      };
    case 'runtime':
      return {
        label: 'changed',
        title:
          f.overrides === undefined
            ? 'The value was changed here or through the API. Reset it to go back to the default.'
            : `The value was changed here or through the API, and overrides ${JSON.stringify(f.overrides)} from the server's settings file. Reset it to go back to that value.`,
      };
    default:
      return null;
  }
}

// --- form fields ---------------------------------------------------------------------

export type FieldType = 'bool' | 'enum' | 'int' | 'number' | 'text' | 'lines' | 'json';

/** A field of a settings form. */
export type FieldDef = {
  /** The dotted path of the field. */
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
  /** A field the server leaves unset by default: an empty input removes the value. */
  optional?: boolean;
  placeholder?: string;
  /** The heading of the group of fields the field belongs to. */
  group?: string;
};

/** A form's values: text for inputs, booleans for checkboxes. */
export type FormValues = Record<string, string | boolean>;

/** The form value of a field from a value. */
export function toForm(def: FieldDef, v: unknown): string | boolean {
  const value = v === undefined ? (def.type === 'bool' ? def.fallback : undefined) : v;
  switch (def.type) {
    case 'bool':
      return value === true;
    case 'lines':
      return Array.isArray(value) ? value.map(String).join('\n') : '';
    case 'json':
      return value === undefined || value === null ? '' : JSON.stringify(value, null, 2);
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

/** A field's value from its form value: `undefined` for an empty optional field. */
export function fromForm(def: FieldDef, f: string | boolean): { ok: unknown } | { error: string } {
  if (def.type === 'bool') return { ok: f === true };
  const t = String(f).trim();
  if (t === '') {
    if (def.type === 'lines') return { ok: [] };
    if (def.type === 'json' || def.optional || def.type === 'enum') return { ok: undefined };
    return { error: `${def.label} needs a value` };
  }
  switch (def.type) {
    case 'int':
    case 'number': {
      const n = Number(t);
      if (!Number.isFinite(n) || (def.type === 'int' && !Number.isSafeInteger(n)))
        return { error: `${def.label} must be a ${def.type === 'int' ? 'whole ' : ''}number` };
      if (def.min != null && n < def.min) return { error: `${def.label} is at least ${def.min}` };
      if (def.max != null && n > def.max) return { error: `${def.label} is at most ${def.max}` };
      return { ok: n };
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
    case 'enum':
      return def.options?.includes(t)
        ? { ok: t }
        : { error: `${def.label} is not one of the values` };
    default:
      return { ok: t };
  }
}

/**
 * The merge patch of a form's edits: each field whose form value changed is set, an
 * emptied optional field is `null` (the runtime value goes, and the field falls back to
 * the settings file or the default), and an object field sends only the members that
 * changed. `errors` lists fields whose text does not read.
 */
export function formPatch(
  defs: readonly FieldDef[],
  initial: FormValues,
  form: FormValues,
  effective: unknown,
): { patch: JsonObject; changed: string[]; errors: Record<string, string> } {
  const patch: JsonObject = {};
  const changed: string[] = [];
  const errors: Record<string, string> = {};
  for (const d of defs) {
    if (form[d.path] === initial[d.path]) continue;
    const r = fromForm(d, form[d.path]);
    if ('error' in r) {
      errors[d.path] = r.error;
      continue;
    }
    const p = parsePath(d.path);
    const before = getAt(effective, p);
    let value: unknown = r.ok === undefined ? null : r.ok;
    if (d.type === 'json' && isObject(before) && isObject(r.ok)) value = diffPatch(before, r.ok);
    if (value === undefined || (value === null && before === undefined)) continue;
    setAt(patch, p, value);
    changed.push(d.path);
  }
  return { patch, changed, errors };
}

/** The fields of a form whose value differs from the loaded one. */
export const dirtyFields = (defs: readonly FieldDef[], initial: FormValues, form: FormValues) =>
  defs.filter((d) => form[d.path] !== initial[d.path]).map((d) => d.path);

// --- the runtime layer as JSON --------------------------------------------------------

/** The runtime layer as the JSON editor shows it. */
export const runtimeText = (k: SettingsKind) => JSON.stringify(k.runtime ?? {}, null, 2);

/**
 * The merge patch that makes the runtime layer equal to the editor's text, or why the
 * text does not do. Null when nothing changed.
 */
export function runtimePatch(
  k: SettingsKind,
  text: string,
): { patch: JsonObject | null } | { error: string } {
  let v: unknown;
  try {
    v = JSON.parse(text.trim() || '{}');
  } catch (e) {
    return { error: `Not JSON: ${e instanceof Error ? e.message : e}` };
  }
  if (!isObject(v)) return { error: 'The runtime layer is a JSON object.' };
  const d = diffPatch(k.runtime ?? {}, v);
  return { patch: d === undefined ? null : (d as JsonObject) };
}

// --- failed writes -------------------------------------------------------------------

/** What a failed write means for the form. */
export type WriteFailure =
  | { kind: 'stale'; message: string }
  | { kind: 'locked'; fields: string[]; message: string }
  | { kind: 'invalid'; message: string }
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
  if (e.status === 400) return { kind: 'invalid', message };
  if (e.status === 403) return { kind: 'forbidden', message };
  return { kind: 'other', message };
}

/** Whether a form field is one of the fields a `locked-by-config` answer names. */
export function conflicts(path: string, fields: readonly string[]): boolean {
  const p = parsePath(path);
  return fields.map(parsePath).some((f) => startsWith(f, p) || startsWith(p, f));
}
