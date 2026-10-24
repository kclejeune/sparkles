import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from './api';
import {
  conflicts,
  covers,
  datasetKindUrl,
  diffPatch,
  dirtyFields,
  fieldInfo,
  formOf,
  formPatch,
  fromForm,
  overridesOf,
  overridesSummary,
  parsePath,
  patchKind,
  pathString,
  readKind,
  resetFields,
  resetKind,
  resetText,
  runtimePatch,
  sourceText,
  valueText,
  writeFailure,
  applyPatch,
  fieldAt,
  formState,
  layerText,
  parseLayer,
  pendingLayer,
  previewEffective,
  pruned,
  rebase,
  withAt,
  type FieldDef,
  type SettingsKind,
} from './settings';
import { ASSISTANT_FIELDS, MEMORY_FIELDS } from './settings-fields';

/** The assistant kind of docs/API.md's example. */
const kind = (over: Partial<SettingsKind> = {}): SettingsKind => ({
  dataset: 'org',
  kind: 'assistant',
  effective: {
    enabled: true,
    send: 'schema',
    historyDays: 7,
    ask: true,
    budget: { perRequest: 80000 },
    sendByProvider: { claude: 'schema' },
  },
  declared: { enabled: true, send: 'schema' },
  runtime: { historyDays: 7, send: 'rows', budget: { perRequest: 80000 } },
  sources: {
    enabled: 'declared',
    send: 'locked',
    historyDays: 'runtime',
    ask: 'default',
    'budget.perRequest': 'runtime',
    'sendByProvider.claude': 'default',
  },
  locked: ['send'],
  overridden: ['send'],
  status: { valid: true },
  etag: '"4f1c"',
  ...over,
});

describe('paths', () => {
  it('splits on dots and keeps escaped ones', () => {
    expect(parsePath('budget.perRequest')).toEqual(['budget', 'perRequest']);
    expect(parsePath('agents.claude\\.ai')).toEqual(['agents', 'claude.ai']);
    expect(pathString(['agents', 'claude.ai'])).toBe('agents.claude\\.ai');
    expect(parsePath(pathString(['a\\b', 'c.d']))).toEqual(['a\\b', 'c.d']);
  });

  it('says whether a layer sets a field as the server does', () => {
    const layer = { budget: { perRequest: 1 }, send: 'rows' };
    expect(covers(layer, ['budget'])).toBe(true);
    expect(covers(layer, ['budget', 'perRequest'])).toBe(true);
    expect(covers(layer, ['budget', 'perPrincipalPerDay'])).toBe(false);
    // a scalar above the field sets it
    expect(covers(layer, ['send', 'x'])).toBe(true);
    expect(covers({}, ['send'])).toBe(false);
  });
});

describe('field sources', () => {
  it('labels declared, runtime and default fields', () => {
    const k = kind();
    expect(fieldInfo(k, 'enabled')).toMatchObject({ source: 'declared', runtime: false });
    expect(fieldInfo(k, 'historyDays')).toMatchObject({ source: 'runtime', runtime: true });
    expect(fieldInfo(k, 'ask')).toMatchObject({ source: 'default', runtime: false });
    expect(sourceText(fieldInfo(k, 'enabled'))?.label).toBe('server config');
    expect(sourceText(fieldInfo(k, 'historyDays'))?.label).toBe('changed');
    expect(sourceText(fieldInfo(k, 'ask'))).toBeNull();
    // a runtime value over a declared one names the value it overrides (from the layers,
    // for a server without `overrides`)
    const over = kind({ declared: { historyDays: 30 } });
    expect(fieldInfo(over, 'historyDays').overrides).toBe(30);
    expect(sourceText(fieldInfo(over, 'historyDays'))).toMatchObject({
      label: 'overrides server config',
      title: expect.stringMatching(/overriding the server's configuration \(30\)/),
    });
    // a runtime value equal to the declared one overrides nothing
    const same = kind({ declared: { historyDays: 7 } });
    expect(fieldInfo(same, 'historyDays').overrides).toBeUndefined();
    expect(sourceText(fieldInfo(same, 'historyDays'))?.label).toBe('changed');
  });

  it('reads the overrides of the answer, at the field and below it', () => {
    const k = kind({
      declared: { historyDays: 30, sendByProvider: { claude: 'rows' } },
      runtime: { historyDays: 7, sendByProvider: { claude: 'schema' } },
      sources: { historyDays: 'runtime', 'sendByProvider.claude': 'runtime' },
      overrides: [
        { path: 'historyDays', declared: 30, runtime: 7 },
        { path: 'sendByProvider.claude', declared: 'rows', runtime: 'schema' },
      ],
      locked: [],
      overridden: [],
    });
    expect(fieldInfo(k, 'historyDays')).toMatchObject({ overrides: 30, hasDeclared: true });
    expect(fieldInfo(k, 'sendByProvider').overrides).toEqual({ claude: 'rows' });
    // `overrides` is what counts: an empty list overrides nothing
    const none = kind({ declared: { historyDays: 30 }, overrides: [] });
    expect(fieldInfo(none, 'historyDays').overrides).toBeUndefined();
  });

  it('names the reset by what the field goes back to', () => {
    const k = kind({ declared: { historyDays: 30 }, overridden: [], locked: [] });
    expect(resetText(fieldInfo(k, 'historyDays')).text).toBe('Use server config');
    expect(resetText(fieldInfo(k, 'budget.perRequest')).text).toBe('Reset to default');
    expect(resetText(fieldInfo(kind(), 'send')).text).toBe('Remove change');
    expect(overridesSummary(1)).toBe('1 field overrides the server config');
    expect(overridesSummary(2)).toBe('2 fields override the server config');
    expect(valueText('schema')).toBe('"schema"');
    expect(valueText({ a: 'x'.repeat(100) }, 20)).toHaveLength(20);
    expect(overridesOf(null)).toEqual([]);
  });

  it('marks a locked field and the runtime value its lock ignores', () => {
    const f = fieldInfo(kind(), 'send');
    expect(f).toMatchObject({ source: 'locked', locked: true, overridden: true, runtime: true });
    expect(f.ignored).toBe('rows');
    expect(sourceText(f)?.label).toBe('locked');
    expect(sourceText(f)?.title).toMatch(/locked in the server's configuration/);
  });

  it('locks the fields below a locked object, and notes a lock below a field', () => {
    const k = kind({ locked: ['budget'], overridden: [] });
    expect(fieldInfo(k, 'budget.perRequest').locked).toBe(true);
    const partly = kind({ locked: ['sendByProvider.claude'], overridden: [] });
    expect(fieldInfo(partly, 'sendByProvider')).toMatchObject({
      locked: false,
      partlyLocked: true,
    });
  });

  it('takes the most telling source of the leaves below an object field', () => {
    const k = kind({
      sources: { 'sendByProvider.claude': 'declared', 'sendByProvider.local': 'runtime' },
      runtime: { sendByProvider: { local: 'rows' } },
    });
    expect(fieldInfo(k, 'sendByProvider')).toMatchObject({ source: 'runtime', runtime: true });
  });

  it('falls back to the layers for a field the effective object leaves out', () => {
    const k = kind({
      runtime: { rowsForSummary: null },
      declared: { budget: { perDatasetPerDay: 5 } },
    });
    expect(fieldInfo(k, 'rowsForSummary')).toMatchObject({ source: 'runtime', runtime: true });
    expect(fieldInfo(k, 'budget.perDatasetPerDay').source).toBe('declared');
    expect(fieldInfo(k, 'deadlineSecs').source).toBe('default');
  });
});

describe('form values', () => {
  it('shows the effective object, with fallbacks for unset booleans', () => {
    const f = formOf(ASSISTANT_FIELDS, kind().effective);
    expect(f.enabled).toBe(true);
    expect(f.explain).toBe(false);
    expect(f.historyDays).toBe('7');
    expect(f.rowsForSummary).toBe('');
    expect(f['budget.perRequest']).toBe('80000');
    expect(JSON.parse(String(f.sendByProvider))).toEqual({ claude: 'schema' });
    const m = formOf(MEMORY_FIELDS, { agentGraphs: ['urn:a/*', 'urn:b/*'], agents: {} });
    expect(m.agentGraphs).toBe('urn:a/*\nurn:b/*');
    expect(m['retention.requireConsolidated']).toBe(true);
  });

  it('reads numbers, lists, JSON and empty optional fields', () => {
    const int: FieldDef = { path: 'n', label: 'N', type: 'int', min: 0, optional: true };
    expect(fromForm(int, '12')).toEqual({ ok: 12 });
    expect(fromForm(int, '')).toEqual({ ok: undefined });
    expect(fromForm(int, '1.5')).toHaveProperty('error');
    expect(fromForm(int, '-1')).toEqual({ error: 'N is at least 0' });
    const required: FieldDef = { path: 'n', label: 'N', type: 'number' };
    expect(fromForm(required, ' ')).toEqual({ error: 'N needs a value' });
    const lines: FieldDef = { path: 'l', label: 'L', type: 'lines' };
    expect(fromForm(lines, ' a \n\n b ')).toEqual({ ok: ['a', 'b'] });
    const json: FieldDef = { path: 'j', label: 'J', type: 'json' };
    expect(fromForm(json, '{"a":1}')).toEqual({ ok: { a: 1 } });
    expect(fromForm(json, '{a')).toHaveProperty('error');
  });

  it('patches only the fields that changed', () => {
    const k = kind();
    const initial = formOf(ASSISTANT_FIELDS, k.effective);
    const form = {
      ...initial,
      historyDays: '14',
      'budget.perRequest': '',
      explain: true,
      sendByProvider: '{"local": "rows"}',
    };
    expect(dirtyFields(ASSISTANT_FIELDS, initial, form).sort()).toEqual(
      ['budget.perRequest', 'explain', 'historyDays', 'sendByProvider'].sort(),
    );
    const r = formPatch(ASSISTANT_FIELDS, initial, form, k.effective);
    expect(r.errors).toEqual({});
    expect(r.patch).toEqual({
      historyDays: 14,
      explain: true,
      // an emptied field drops its runtime value
      budget: { perRequest: null },
      // a map sends the members that changed, and null for the one removed
      sendByProvider: { claude: null, local: 'rows' },
    });
  });

  it('reports fields that do not read and sends nothing for them', () => {
    const k = kind();
    const initial = formOf(ASSISTANT_FIELDS, k.effective);
    const r = formPatch(
      ASSISTANT_FIELDS,
      initial,
      { ...initial, historyDays: 'soon' },
      k.effective,
    );
    expect(r.patch).toEqual({});
    expect(r.errors.historyDays).toMatch(/whole number/);
  });

  it('skips clearing a field that was never set', () => {
    const def: FieldDef = { path: 'a.b', label: 'B', type: 'text', optional: true };
    const r = formPatch([def], { 'a.b': 'x' }, { 'a.b': '' }, {});
    expect(r.patch).toEqual({});
  });
});

describe('the runtime layer as JSON', () => {
  it('makes a merge patch from the old layer to the new one', () => {
    expect(diffPatch({ a: 1, b: { c: 2, d: 3 } }, { b: { c: 2, d: 4 }, e: [1] })).toEqual({
      a: null,
      b: { d: 4 },
      e: [1],
    });
    expect(diffPatch({ a: [1] }, { a: [1] })).toBeUndefined();
    const k = kind();
    expect(runtimePatch(k, JSON.stringify(k.runtime))).toEqual({ patch: null });
    expect(runtimePatch(k, '{"historyDays": 7}')).toEqual({
      patch: { send: null, budget: null },
    });
    expect(runtimePatch(k, '[]')).toHaveProperty('error');
    expect(runtimePatch(k, '{')).toHaveProperty('error');
  });
});

describe('failed writes', () => {
  it('tells a stale write, a lock, a bad body and a refusal apart', () => {
    expect(writeFailure(new ApiError(412, 'changed', { code: 'precondition-failed' })).kind).toBe(
      'stale',
    );
    const locked = writeFailure(
      new ApiError(409, 'locked', {
        code: 'locked-by-config',
        body: { error: 'locked', code: 'locked-by-config', fields: ['send'] },
      }),
    );
    expect(locked).toEqual({ kind: 'locked', fields: ['send'], message: 'locked' });
    expect(writeFailure(new ApiError(400, 'bad', { code: 'bad-settings' })).kind).toBe('invalid');
    expect(writeFailure(new ApiError(403, 'no')).kind).toBe('forbidden');
    expect(writeFailure(new Error('x')).kind).toBe('other');
  });

  it('highlights the fields a lock names, and the fields below or above them', () => {
    expect(conflicts('send', ['send'])).toBe(true);
    expect(conflicts('budget.perRequest', ['budget'])).toBe(true);
    expect(conflicts('sendByProvider', ['sendByProvider.claude'])).toBe(true);
    expect(conflicts('ask', ['send'])).toBe(false);
  });
});

describe('the settings routes', () => {
  afterEach(() => vi.unstubAllGlobals());

  function stub(answer: unknown, status = 200) {
    const calls: { url: string; init: RequestInit }[] = [];
    vi.stubGlobal('fetch', async (url: string, init: RequestInit = {}) => {
      calls.push({ url, init });
      return new Response(JSON.stringify(answer), {
        status,
        headers: { 'Content-Type': 'application/json' },
      });
    });
    return calls;
  }

  it('reads a kind and writes with If-Match', async () => {
    const calls = stub(kind());
    const url = datasetKindUrl('my ds', 'assistant');
    expect(url).toBe('/$/settings/my%20ds/assistant');
    expect((await readKind(url)).etag).toBe('"4f1c"');
    await patchKind(url, { historyDays: 14 }, '"4f1c"');
    const patch = calls[1];
    expect(patch.init.method).toBe('PATCH');
    expect(new Headers(patch.init.headers).get('If-Match')).toBe('"4f1c"');
    expect(JSON.parse(String(patch.init.body))).toEqual({ historyDays: 14 });
    await resetKind(url, 'agents.claude\\.ai', '"4f1c"');
    expect(calls[2].url).toBe('/$/settings/my%20ds/assistant?field=agents.claude%5C.ai');
    expect(calls[2].init.method).toBe('DELETE');
    await resetKind(url);
    expect(calls[3].url).toBe(url);
    expect(new Headers(calls[3].init.headers).has('If-Match')).toBe(false);
  });

  it('resets fields one after another, each with the ETag of the answer before', async () => {
    let n = 0;
    const calls: { url: string; init: RequestInit }[] = [];
    vi.stubGlobal('fetch', async (url: string, init: RequestInit = {}) => {
      calls.push({ url, init });
      n++;
      return new Response(JSON.stringify(kind({ etag: `"e${n}"` })), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      });
    });
    const url = '/$/server/settings/models';
    const k = await resetFields(url, ['roles.draft', 'providers.local'], '"e0"');
    expect(k.etag).toBe('"e2"');
    expect(calls.map((c) => c.url)).toEqual([
      `${url}?field=roles.draft`,
      `${url}?field=providers.local`,
    ]);
    expect(calls.map((c) => new Headers(c.init.headers).get('If-Match'))).toEqual(['"e0"', '"e1"']);
  });

  it('raises the 409 of a lock with its fields', async () => {
    stub({ error: 'locked', code: 'locked-by-config', fields: ['send'] }, 409);
    const e = await patchKind('/$/settings/d/assistant', { send: 'rows' }).catch((x) => x);
    expect(writeFailure(e)).toEqual({ kind: 'locked', fields: ['send'], message: 'locked' });
  });
});

describe('the form and the JSON editor share one change', () => {
  /** A models-like kind: a declared provider with a runtime change, and members no form knows. */
  const k = (): SettingsKind => ({
    scope: 'server',
    kind: 'models',
    effective: {
      providers: {
        p: {
          kind: 'openai',
          endpoint: 'http://h/v1',
          budget: { tokensPerDay: 7 },
          future: { a: 1 },
          models: { m: { temperature: 0.5, later: true } },
        },
        q: { kind: 'ollama', endpoint: 'http://q' },
      },
      roles: {
        draft: [
          { provider: 'p', model: 'm' },
          { provider: 'q', model: 'x' },
        ],
      },
      zzz: 3,
    },
    declared: {
      providers: {
        p: { kind: 'openai', endpoint: 'http://h/v1', budget: { tokensPerDay: 1000 } },
        r: { kind: 'openai', endpoint: 'http://r' },
      },
    },
    runtime: {
      providers: {
        p: {
          budget: { tokensPerDay: 7 },
          future: { a: 1 },
          models: { m: { temperature: 0.5, later: true } },
        },
        q: { kind: 'ollama', endpoint: 'http://q' },
        r: null,
      },
      roles: {
        draft: [
          { provider: 'p', model: 'm' },
          { provider: 'q', model: 'x' },
        ],
      },
      zzz: 3,
    },
    sources: {},
    locked: ['providers.p.endpoint'],
    overridden: [],
    status: { valid: true },
    etag: '"e"',
  });
  const defs: FieldDef[] = [
    { path: 'providers.p.endpoint', label: 'Endpoint', type: 'text' },
    {
      path: 'providers.p.budget.tokensPerDay',
      label: 'Tokens per day',
      type: 'int',
      min: 0,
      optional: true,
    },
    {
      path: 'providers.p.models',
      label: 'Per-model options',
      type: 'map',
      entry: [
        {
          path: 'temperature',
          label: 'Temperature',
          type: 'number',
          min: 0,
          max: 2,
          optional: true,
        },
      ],
    },
    {
      path: 'roles.draft',
      label: 'draft',
      type: 'records',
      entry: [
        { path: 'provider', label: 'Provider', type: 'enum', options: ['p', 'q'] },
        { path: 'model', label: 'Model', type: 'text' },
      ],
    },
  ];

  it('sends nothing until something changes', () => {
    const r = pendingLayer(defs, k(), formState(defs, k()));
    expect(r.patch).toBeNull();
    expect(r.layer).toEqual(k().runtime);
  });

  it('keeps members the form does not know, and their order, when the form changes', () => {
    const s = formState(defs, k());
    s.form['providers.p.budget.tokensPerDay'] = '500';
    s.form['providers.p.models'] = { m: { temperature: 1.5, later: true } };
    const r = pendingLayer(defs, k(), s);
    expect(r.errors).toEqual({});
    expect(r.patch).toEqual({
      providers: { p: { budget: { tokensPerDay: 500 }, models: { m: { temperature: 1.5 } } } },
    });
    // the unknown members stay, in their places
    expect(Object.keys(r.layer)).toEqual(['providers', 'roles', 'zzz']);
    expect(Object.keys((r.layer.providers as Record<string, unknown>).p as object)).toEqual([
      'budget',
      'future',
      'models',
    ]);
    expect(r.layer.zzz).toBe(3);
    expect((r.layer.providers as Record<string, unknown>).r).toBeNull();
    expect(applyPatch(k().runtime, r.patch)).toEqual(r.layer);
  });

  it('round-trips through the JSON text without losing or reordering anything', () => {
    const s = formState(defs, k());
    s.form['providers.p.budget.tokensPerDay'] = '500';
    s.form['roles.draft'] = [
      { provider: 'q', model: 'x' },
      { provider: 'p', model: 'm' },
    ];
    const first = pendingLayer(defs, k(), s);
    const text = layerText(first.layer);
    const parsed = parseLayer(text);
    if ('error' in parsed) throw new Error(parsed.error);
    const again = rebase(defs, k(), parsed.layer);
    // the form shows the edits the text holds
    expect(again.form['providers.p.budget.tokensPerDay']).toBe('500');
    expect(again.form['roles.draft']).toEqual([
      { provider: 'q', model: 'x' },
      { provider: 'p', model: 'm' },
    ]);
    const second = pendingLayer(defs, k(), again);
    expect(second.layer).toEqual(first.layer);
    expect(second.patch).toEqual(first.patch);
    expect(layerText(second.layer)).toBe(text);
    // and a form edit after the JSON edit keeps what the JSON set
    again.form['providers.p.models'] = { m: { temperature: 2, later: true } };
    const third = pendingLayer(defs, k(), again);
    expect(third.layer.zzz).toBe(3);
    expect(third.patch).toMatchObject({
      providers: { p: { budget: { tokensPerDay: 500 }, models: { m: { temperature: 2 } } } },
    });
  });

  it('applies a JSON edit of a member the form does not know', () => {
    const layer = { ...k().runtime, zzz: 4, added: { x: [1, 2] } };
    const s = rebase(defs, k(), layer);
    const r = pendingLayer(defs, k(), s);
    expect(r.patch).toEqual({ zzz: 4, added: { x: [1, 2] } });
  });

  it('reports what does not read, and leaves it out of the layer', () => {
    const s = formState(defs, k());
    s.form['providers.p.budget.tokensPerDay'] = 'lots';
    s.form['providers.p.models'] = { m: { temperature: 3 } };
    s.form['roles.draft'] = [{ provider: 'z', model: '' }];
    const r = pendingLayer(defs, k(), s);
    expect(r.errors).toMatchObject({
      'providers.p.budget.tokensPerDay': 'Tokens per day must be a whole number',
    });
    expect(r.entryErrors).toEqual({
      'providers.p.models.m.temperature': 'Temperature is at most 2',
      'roles.draft.0.provider': 'Provider is not one of the values',
      'roles.draft.0.model': 'Model needs a value',
    });
    expect(r.patch).toBeNull();
  });

  it('previews the effective object of a layer, with declared values and locks', () => {
    const kk = k();
    // a runtime value removed falls back to the declared one
    const back = previewEffective(kk, withAt(kk.runtime, ['providers', 'p', 'budget'], undefined));
    expect(back.providers).toMatchObject({ p: { budget: { tokensPerDay: 1000 } } });
    // a locked field keeps its value, whatever the layer says
    const locked = previewEffective(
      kk,
      withAt(kk.runtime, ['providers', 'p', 'endpoint'], 'http://x'),
    );
    expect((locked.providers as Record<string, Record<string, unknown>>).p.endpoint).toBe(
      'http://h/v1',
    );
    // a null removes the declared member, and taking it away brings it back
    const restored = previewEffective(kk, withAt(kk.runtime, ['providers', 'r'], undefined));
    expect((restored.providers as Record<string, unknown>).r).toEqual({
      kind: 'openai',
      endpoint: 'http://r',
    });
    expect(previewEffective(kk, kk.runtime)).toBe(kk.effective);
    // the preview never changes the kind it starts from
    const before = JSON.stringify(kk.effective);
    const freeze = (v: unknown): unknown =>
      v && typeof v === 'object' ? Object.freeze(Object.values(v).forEach(freeze) ?? v) : v;
    freeze(kk.effective);
    previewEffective(kk, withAt(kk.runtime, ['zzz'], 9));
    expect(JSON.stringify(kk.effective)).toBe(before);
  });

  it('applies merge patches and drops empty objects as the server does', () => {
    expect(applyPatch({ a: 1, b: { c: 2, d: 3 } }, { b: { c: null, e: 4 }, f: [1] })).toEqual({
      a: 1,
      b: { d: 3, e: 4 },
      f: [1],
    });
    expect(pruned({ a: {}, b: { c: {} }, d: [], e: 0 })).toEqual({ d: [], e: 0 });
    expect(withAt({ a: { b: 1 } }, ['a', 'b'], undefined)).toEqual({});
    expect(withAt({ a: { b: 1 } }, ['a', 'c'], 2)).toEqual({ a: { b: 1, c: 2 } });
    expect(parseLayer('[1]')).toEqual({ error: 'The runtime layer must be a JSON object.' });
  });

  it('finds the field a server error names, from the field or a member below it', () => {
    expect(fieldAt(defs, 'providers.p.models.m.temperature')?.path).toBe('providers.p.models');
    expect(fieldAt(defs, 'roles.draft.1.model')?.path).toBe('roles.draft');
    expect(fieldAt(defs, 'providers.p.endpoint')?.path).toBe('providers.p.endpoint');
    expect(fieldAt(defs, 'providers.p.headers.X')).toBeUndefined();
  });

  it('reads the field a refused write names', () => {
    const e = new ApiError(400, 'models: temperature must be between 0 and 2', {
      code: 'bad-settings',
      body: { field: 'providers.p.models.m.temperature' },
    });
    expect(writeFailure(e)).toEqual({
      kind: 'invalid',
      message: 'models: temperature must be between 0 and 2',
      field: 'providers.p.models.m.temperature',
    });
  });
});
