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
  parsePath,
  patchKind,
  pathString,
  readKind,
  resetKind,
  runtimePatch,
  sourceText,
  writeFailure,
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
    // a runtime value over a declared one names the value it overrides
    const over = kind({ declared: { historyDays: 30 } });
    expect(fieldInfo(over, 'historyDays').overrides).toBe(30);
    expect(sourceText(fieldInfo(over, 'historyDays'))?.title).toMatch(/overrides 30/);
  });

  it('marks a locked field and the runtime value its lock ignores', () => {
    const f = fieldInfo(kind(), 'send');
    expect(f).toMatchObject({ source: 'locked', locked: true, overridden: true, runtime: true });
    expect(f.ignored).toBe('rows');
    expect(sourceText(f)?.label).toBe('locked');
    expect(sourceText(f)?.title).toMatch(/operator locked/);
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

  it('raises the 409 of a lock with its fields', async () => {
    stub({ error: 'locked', code: 'locked-by-config', fields: ['send'] }, 409);
    const e = await patchKind('/$/settings/d/assistant', { send: 'rows' }).catch((x) => x);
    expect(writeFailure(e)).toEqual({ kind: 'locked', fields: ['send'], message: 'locked' });
  });
});
