// Settings form fields from a JSON Schema, as docs/openapi.json describes a settings
// kind. Each member becomes a field whose control follows its type: a toggle for a
// boolean, a select for an enum, a number input with its bounds, a list editor for an
// array of strings, rows for an array of objects, cards for an object of objects and
// name and value pairs for an object of strings. Nested objects of known members become
// one field per member. A member with no known shape stays JSON. The labels and help
// come from the schema's `title` and `description`, so a member added to the schema shows
// in the form without code here. The `x-sparkles-*` extensions of
// crates/sparkles-server/src/openapi/schemas/model_settings.rs say the order of the
// members, the provider kinds a member applies to and which strings name a secret.

import { pathString, type FieldDef, type FieldType } from './settings';

/** The part of JSON Schema the settings schemas use, with the extensions. */
export type JsonSchema = {
  $ref?: string;
  type?: string | string[];
  title?: string;
  description?: string;
  enum?: readonly string[];
  properties?: Record<string, JsonSchema>;
  additionalProperties?: boolean | JsonSchema;
  propertyNames?: { pattern?: string };
  items?: JsonSchema;
  required?: string[];
  minimum?: number;
  maximum?: number;
  exclusiveMinimum?: number;
  minLength?: number;
  maxLength?: number;
  pattern?: string;
  format?: string;
  uniqueItems?: boolean;
  default?: unknown;
  'x-sparkles-order'?: string[];
  'x-sparkles-kinds'?: string[];
  'x-sparkles-secret'?: boolean;
  'x-sparkles-unset'?: string;
  'x-sparkles-unit'?: string;
  'x-sparkles-https'?: boolean;
  'x-sparkles-enum-kinds'?: Record<string, string[]>;
  'x-sparkles-fallback'?: string;
  'x-sparkles-section'?: string;
};

/** Named schemas, as `components.schemas` holds them. */
export type SchemaSet = Record<string, JsonSchema>;

/** What decides which members apply: the provider's kind and whether its endpoint is https. */
export type SchemaContext = {
  set: SchemaSet;
  kind?: string;
  https?: boolean;
  group?: string;
};

/** The schema a `$ref` names, or the schema itself. */
export function deref(set: SchemaSet, s: JsonSchema | undefined): JsonSchema {
  if (!s) return {};
  if (s.$ref) {
    const name = s.$ref.split('/').pop() ?? '';
    return set[name] ?? {};
  }
  return s;
}

const typeOf = (s: JsonSchema) => (Array.isArray(s.type) ? s.type[0] : s.type);

/** An object's members in the order the schema gives, then any it leaves out. */
export function members(s: JsonSchema): [string, JsonSchema][] {
  const props = s.properties ?? {};
  const order = s['x-sparkles-order'] ?? [];
  const names = [
    ...order.filter((n) => n in props),
    ...Object.keys(props).filter((n) => !order.includes(n)),
  ];
  return names.map((n) => [n, props[n]]);
}

/** Whether a member applies to the context's provider. */
export function applies(s: JsonSchema, ctx: SchemaContext): boolean {
  const kinds = s['x-sparkles-kinds'];
  if (kinds && ctx.kind !== undefined && !kinds.includes(ctx.kind)) return false;
  if (s['x-sparkles-https'] && ctx.https === false) return false;
  return true;
}

/** The values of an enum that the context's provider offers. */
export function enumValues(s: JsonSchema, ctx: SchemaContext): string[] {
  const all = [...(s.enum ?? [])];
  const by = s['x-sparkles-enum-kinds'];
  if (!by || ctx.kind === undefined) return all;
  return all.filter((v) => !by[v] || by[v].includes(ctx.kind!));
}

/** An object whose members are any names, each of one schema (a map). */
function mapValue(set: SchemaSet, s: JsonSchema): JsonSchema | null {
  if (typeOf(s) !== 'object' || s.properties) return null;
  const a = s.additionalProperties;
  return a && typeof a === 'object' ? deref(set, a) : null;
}

/** The control a schema takes. */
export function controlOf(set: SchemaSet, raw: JsonSchema): FieldType | 'object' {
  const s = deref(set, raw);
  const t = typeOf(s);
  if (s.enum) return 'enum';
  switch (t) {
    case 'boolean':
      return 'bool';
    case 'integer':
      return 'int';
    case 'number':
      return 'number';
    case 'string':
      return s['x-sparkles-secret'] ? 'secret' : 'text';
    case 'array': {
      const item = deref(set, s.items);
      if (typeOf(item) === 'string' && !item.enum) return 'list';
      if (typeOf(item) === 'object' && item.properties) return 'records';
      return 'json';
    }
    case 'object': {
      if (s.properties) return 'object';
      const v = mapValue(set, s);
      if (!v) return 'json';
      if (typeOf(v) === 'string' && !v.enum) return 'pairs';
      if (typeOf(v) === 'object' && v.properties) return 'map';
      return 'json';
    }
    default:
      return 'json';
  }
}

/** The words for a pattern's refusal, for the patterns the settings schemas use. */
function patternText(label: string, s: JsonSchema): string | undefined {
  if (s['x-sparkles-secret'])
    return 'A secret name has 1 to 128 letters, digits, _, - and ., and does not start with . or -.';
  if (s.pattern) return `${label} does not have the right form`;
  return undefined;
}

/**
 * The fields of the members of the object `schema` at `base` (an empty base gives paths
 * inside an entry). Nested objects of known members give a field per member, under a
 * section named by `x-sparkles-section` or by the title of the outermost object of
 * several members.
 */
export function fieldsOf(
  raw: JsonSchema,
  base: readonly string[],
  ctx: SchemaContext,
  section?: string,
): FieldDef[] {
  const s = deref(ctx.set, raw);
  const required = new Set(s.required ?? []);
  const out: FieldDef[] = [];
  for (const [name, m] of members(s)) {
    if (!applies(m, ctx)) continue;
    const ms = deref(ctx.set, m);
    const path = [...base, name];
    const control = controlOf(ctx.set, ms);
    const at = ms['x-sparkles-section'] ?? section;
    if (control === 'object') {
      // the outermost object of several members names the section of its fields
      const several = members(ms).filter(([, x]) => applies(x, ctx)).length > 1;
      out.push(...fieldsOf(ms, path, ctx, at ?? (several ? ms.title : undefined)));
      continue;
    }
    out.push(fieldOf(ms, path, ctx, !required.has(name), at));
  }
  return out;
}

/** The field of one member. */
export function fieldOf(
  raw: JsonSchema,
  path: readonly string[],
  ctx: SchemaContext,
  optional: boolean,
  section?: string,
): FieldDef {
  const s = deref(ctx.set, raw);
  const control = controlOf(ctx.set, s);
  const name = path[path.length - 1] ?? '';
  const label = s.title ?? name;
  const def: FieldDef = {
    path: pathString(path),
    label,
    type: control === 'object' ? 'json' : control,
  };
  if (s.description) def.help = s.description;
  if (ctx.group) def.group = ctx.group;
  if (section) def.section = section;
  if (optional) def.optional = true;
  if (s.default !== undefined) def.fallback = s.default;
  if (s['x-sparkles-unset']) def.placeholder = s['x-sparkles-unset'];
  if (s['x-sparkles-unit']) def.unit = s['x-sparkles-unit'];
  if (s.minimum !== undefined) def.min = s.minimum;
  if (s.maximum !== undefined) def.max = s.maximum;
  if (s.exclusiveMinimum !== undefined) def.above = s.exclusiveMinimum;
  if (s.maxLength !== undefined) def.maxLength = s.maxLength;
  if (s.pattern) {
    def.pattern = s.pattern;
    def.patternText = patternText(label, s);
  }
  switch (def.type) {
    case 'enum':
      def.options = enumValues(s, ctx);
      break;
    case 'list': {
      const item = deref(ctx.set, s.items);
      if (item.pattern) {
        def.pattern = item.pattern;
        def.patternText = patternText(label, item);
      }
      break;
    }
    case 'records': {
      const item = deref(ctx.set, s.items);
      def.entry = fieldsOf(item, [], { ...ctx, group: undefined });
      if (s['x-sparkles-fallback']) def.unsetText = `the ${s['x-sparkles-fallback']} list`;
      break;
    }
    case 'map':
    case 'pairs': {
      const v = mapValue(ctx.set, s);
      if (s.propertyNames?.pattern) def.keyPattern = s.propertyNames.pattern;
      if (def.type === 'map' && v) def.entry = fieldsOf(v, [], { ...ctx, group: undefined });
      break;
    }
  }
  return def;
}
