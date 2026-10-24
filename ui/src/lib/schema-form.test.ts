import { describe, expect, it } from 'vitest';
import { applies, controlOf, enumValues, fieldsOf, members, type SchemaSet } from './schema-form';
import { checkStructured, checkValue, fromForm, type FieldDef } from './settings';

/** A small schema set in the shape of docs/openapi.json's settings schemas. */
const set: SchemaSet = {
  Options: {
    type: 'object',
    'x-sparkles-order': ['tokens', 'level'],
    properties: {
      level: {
        type: 'string',
        title: 'Level',
        enum: ['auto', 'tool', 'json-object'],
        'x-sparkles-enum-kinds': { tool: ['openai'], 'json-object': ['ollama', 'openai'] },
      },
      tokens: {
        type: 'integer',
        title: 'Tokens',
        minimum: 1,
        default: 8192,
        'x-sparkles-unit': 'tokens',
      },
    },
  },
  Pair: {
    type: 'object',
    required: ['provider', 'model'],
    properties: {
      provider: { type: 'string', title: 'Provider' },
      model: { type: 'string', title: 'Model', minLength: 1 },
      timeout: { type: 'number', title: 'Timeout', exclusiveMinimum: 0 },
    },
  },
  Provider: {
    type: 'object',
    required: ['kind'],
    'x-sparkles-order': ['kind', 'key', 'allowed', 'budget', 'headers', 'tls', 'models', 'extra'],
    properties: {
      kind: { type: 'string', title: 'Protocol', enum: ['ollama', 'openai'] },
      key: {
        type: 'object',
        title: 'API key',
        properties: {
          secret: {
            type: 'string',
            title: 'API key',
            pattern: '^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$',
            'x-sparkles-secret': true,
          },
        },
      },
      allowed: {
        type: 'array',
        title: 'Allowed models',
        items: { type: 'string', minLength: 1 },
        'x-sparkles-unset': 'any model',
      },
      budget: {
        type: 'object',
        title: 'Budget',
        properties: { perDay: { type: 'integer', title: 'Per day', minimum: 0 } },
      },
      headers: {
        type: 'object',
        title: 'Headers',
        additionalProperties: { type: 'string' },
        'x-sparkles-kinds': ['openai'],
      },
      tls: {
        type: 'object',
        title: 'TLS',
        'x-sparkles-https': true,
        properties: { skip: { type: 'boolean', title: 'Skip', default: false } },
      },
      models: {
        type: 'object',
        title: 'Per-model options',
        additionalProperties: { $ref: '#/components/schemas/Options' },
      },
      extra: { type: 'object', title: 'Extra', additionalProperties: true },
    },
  },
  Roles: {
    type: 'object',
    properties: {
      draft: { type: 'array', title: 'draft', items: { $ref: '#/components/schemas/Pair' } },
      repair: {
        type: 'array',
        title: 'repair',
        items: { $ref: '#/components/schemas/Pair' },
        'x-sparkles-fallback': 'draft',
      },
    },
  },
};

const byPath = (defs: FieldDef[]) => Object.fromEntries(defs.map((d) => [d.path, d]));

describe('controls from a schema', () => {
  it('picks a control for each shape, and JSON for an unknown one', () => {
    const p = set.Provider.properties!;
    expect(controlOf(set, p.kind)).toBe('enum');
    expect(controlOf(set, p.allowed)).toBe('list');
    expect(controlOf(set, p.headers)).toBe('pairs');
    expect(controlOf(set, p.models)).toBe('map');
    expect(controlOf(set, p.budget)).toBe('object');
    expect(controlOf(set, p.extra)).toBe('json');
    expect(controlOf(set, set.Roles.properties!.draft)).toBe('records');
    expect(controlOf(set, set.Options.properties!.tokens)).toBe('int');
    expect(controlOf(set, set.Pair.properties!.timeout)).toBe('number');
    expect(controlOf(set, set.Provider.properties!.tls.properties!.skip)).toBe('bool');
    expect(controlOf(set, p.key.properties!.secret)).toBe('secret');
  });

  it('orders members as the schema says, then the rest', () => {
    expect(members(set.Options).map(([n]) => n)).toEqual(['tokens', 'level']);
    expect(members(set.Pair).map(([n]) => n)).toEqual(['provider', 'model', 'timeout']);
  });

  it('keeps members and enum values to the kinds that take them', () => {
    const headers = set.Provider.properties!.headers;
    expect(applies(headers, { set, kind: 'openai' })).toBe(true);
    expect(applies(headers, { set, kind: 'ollama' })).toBe(false);
    expect(applies(set.Provider.properties!.tls, { set, https: false })).toBe(false);
    const level = set.Options.properties!.level;
    expect(enumValues(level, { set, kind: 'ollama' })).toEqual(['auto', 'json-object']);
    expect(enumValues(level, { set })).toEqual(['auto', 'tool', 'json-object']);
  });
});

describe('fields from a schema', () => {
  it('gives a field per member, with labels, bounds and units from the schema', () => {
    const defs = fieldsOf(set.Provider, ['providers', 'p.x'], {
      set,
      kind: 'ollama',
      https: false,
      group: 'Provider p.x',
    });
    const f = byPath(defs);
    expect(Object.keys(f)).toEqual([
      'providers.p\\.x.kind',
      'providers.p\\.x.key.secret',
      'providers.p\\.x.allowed',
      'providers.p\\.x.budget.perDay',
      'providers.p\\.x.models',
      'providers.p\\.x.extra',
    ]);
    expect(f['providers.p\\.x.kind']).toMatchObject({ type: 'enum', group: 'Provider p.x' });
    expect(f['providers.p\\.x.kind'].optional).toBeUndefined();
    expect(f['providers.p\\.x.key.secret']).toMatchObject({
      type: 'secret',
      label: 'API key',
      optional: true,
    });
    expect(f['providers.p\\.x.key.secret'].section).toBeUndefined();
    expect(f['providers.p\\.x.allowed']).toMatchObject({ type: 'list', placeholder: 'any model' });
    expect(f['providers.p\\.x.budget.perDay']).toMatchObject({ type: 'int', min: 0 });
    const models = f['providers.p\\.x.models'];
    expect(models.type).toBe('map');
    // entry fields are relative, and follow the provider's kind
    expect(models.entry!.map((e) => [e.path, e.type, e.options])).toEqual([
      ['tokens', 'int', undefined],
      ['level', 'enum', ['auto', 'json-object']],
    ]);
    expect(models.entry![0]).toMatchObject({ unit: 'tokens', fallback: 8192, min: 1 });
    expect(f['providers.p\\.x.extra'].type).toBe('json');
  });

  it('turns a list of objects into rows, with the fallback of an unset list', () => {
    const f = byPath(fieldsOf(set.Roles, ['roles'], { set, group: 'Roles' }));
    expect(f['roles.draft'].entry!.map((e) => [e.path, e.optional ?? false])).toEqual([
      ['provider', false],
      ['model', false],
      ['timeout', true],
    ]);
    expect(f['roles.repair'].unsetText).toBe('the draft list');
    expect(f['roles.draft'].unsetText).toBeUndefined();
  });
});

describe('checks', () => {
  const int: FieldDef = { path: 'n', label: 'N', type: 'int', min: 1, optional: true };
  const secs: FieldDef = { path: 't', label: 'T', type: 'number', above: 0, optional: true };
  const level: FieldDef = { path: 'l', label: 'Level', type: 'enum', options: ['a', 'b'] };
  const name: FieldDef = { path: 's', label: 'Key', type: 'secret', pattern: '^[a-z]+$' };

  it('checks a value against its type and bounds', () => {
    expect(checkValue(int, 3)).toBeNull();
    expect(checkValue(int, undefined)).toBeNull();
    expect(checkValue(int, 0)).toBe('N is at least 1');
    expect(checkValue(int, 1.5)).toBe('N must be a whole number');
    expect(checkValue(int, 'abc')).toBe('N must be a whole number');
    expect(checkValue(secs, 0)).toBe('T is more than 0');
    expect(checkValue(level, 'c')).toBe('Level is not one of the values');
    expect(checkValue(name, 'A')).toBe('Key does not have the right form');
    expect(checkValue(name, undefined)).toBe('Key needs a value');
    expect(fromForm(secs, '0')).toEqual({ error: 'T is more than 0' });
    expect(fromForm(secs, '2.5')).toEqual({ ok: 2.5 });
  });

  it('checks the entries of lists, rows and cards by their paths', () => {
    const list: FieldDef = { path: 'a', label: 'Allowed', type: 'list' };
    expect(checkStructured(list, ['x', '', 'x'])).toEqual({
      '1': 'An entry needs a value',
      '2': 'x is listed twice',
    });
    const rows: FieldDef = {
      path: 'r',
      label: 'draft',
      type: 'records',
      entry: [
        { path: 'provider', label: 'Provider', type: 'enum', options: ['p'] },
        { path: 'model', label: 'Model', type: 'text' },
        secs,
      ],
    };
    expect(
      checkStructured(rows, [
        { provider: 'p', model: 'm' },
        { provider: 'q', t: -1 },
      ]),
    ).toEqual({
      '1.provider': 'Provider is not one of the values',
      '1.model': 'Model needs a value',
      '1.t': 'T is more than 0',
    });
    const cards: FieldDef = {
      path: 'm',
      label: 'Per-model options',
      type: 'map',
      entry: [int, { path: 'price.in', label: 'In', type: 'number', min: 0, optional: true }],
    };
    expect(checkStructured(cards, { 'a.b': { n: 0, price: { in: -1 } }, ok: { n: 2 } })).toEqual({
      'a\\.b.n': 'N is at least 1',
      'a\\.b.price.in': 'In is at least 0',
    });
    expect(fromForm(cards, { a: { n: 0 } })).toEqual({
      error: 'N is at least 1',
      errors: { 'a.n': 'N is at least 1' },
    });
  });
});
