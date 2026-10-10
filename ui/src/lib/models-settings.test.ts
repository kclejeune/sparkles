import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  MODELS_URL,
  deleteSecret,
  groupProvider,
  modelFields,
  newProviderPatch,
  providerGroup,
  providerNames,
  putSecret,
  removedProviders,
  secretState,
  type SecretInfo,
} from './models-settings';
import { fieldInfo, formOf, formPatch, fromForm, type SettingsKind } from './settings';

/** A `models` kind: claude declared with its endpoint locked, local removed at runtime. */
const models = (over: Partial<SettingsKind> = {}): SettingsKind => ({
  scope: 'server',
  kind: 'models',
  effective: {
    providers: {
      claude: {
        kind: 'anthropic',
        endpoint: 'https://api.anthropic.com',
        apiKey: { secret: 'anthropic' },
        budget: { tokensPerDay: 7 },
      },
      'gw.internal': { kind: 'ollama', endpoint: 'http://127.0.0.1:11434' },
    },
    roles: { draft: [{ provider: 'claude', model: 'c' }] },
  },
  declared: {
    providers: {
      claude: {
        kind: 'anthropic',
        endpoint: 'https://api.anthropic.com',
        apiKey: { secret: 'anthropic' },
        budget: { tokensPerDay: 1000 },
      },
      local: { kind: 'openai', endpoint: 'http://127.0.0.1:9/v1' },
    },
    roles: { draft: [{ provider: 'claude', model: 'c' }] },
  },
  runtime: {
    providers: {
      claude: { budget: { tokensPerDay: 7 } },
      local: null,
      'gw.internal': { kind: 'ollama', endpoint: 'http://127.0.0.1:11434' },
    },
  },
  sources: {
    'providers.claude.kind': 'declared',
    'providers.claude.endpoint': 'locked',
    'providers.claude.apiKey.secret': 'declared',
    'providers.claude.budget.tokensPerDay': 'runtime',
    'providers.gw\\.internal.kind': 'runtime',
    'providers.gw\\.internal.endpoint': 'runtime',
    'roles.draft': 'declared',
  },
  locked: ['providers.claude.endpoint'],
  overridden: [],
  overrides: [
    { path: 'providers.claude.budget.tokensPerDay', declared: 1000, runtime: 7 },
    {
      path: 'providers.local',
      declared: { kind: 'openai', endpoint: 'http://127.0.0.1:9/v1' },
      runtime: null,
    },
  ],
  status: { valid: true },
  etag: '"m1"',
  ...over,
});

describe('the form of the models kind', () => {
  it('has a group of fields for each provider, the roles and the routing', () => {
    const k = models();
    expect(providerNames(k)).toEqual(['claude', 'gw.internal']);
    const defs = modelFields(k);
    const paths = defs.map((d) => d.path);
    expect(paths).toContain('providers.claude.endpoint');
    expect(paths).toContain('providers.claude.budget.tokensPerDay');
    expect(paths).toContain('providers.gw\\.internal.numCtx');
    expect(paths).not.toContain('providers.claude.numCtx');
    expect(paths).toContain('providers.claude.version');
    expect(paths).toContain('roles.draft');
    expect(paths).toContain('routing.exampleScore');
    expect(groupProvider(providerGroup('gw.internal'))).toBe('gw.internal');
    expect(groupProvider('Roles')).toBeNull();
    expect(modelFields(null).filter((d) => d.group?.startsWith('Provider'))).toEqual([]);
  });

  it('shows locks and overrides of providers as a dataset kind does', () => {
    const k = models();
    expect(fieldInfo(k, 'providers.claude.endpoint').locked).toBe(true);
    expect(fieldInfo(k, 'providers.claude').partlyLocked).toBe(true);
    expect(fieldInfo(k, 'providers.claude.budget.tokensPerDay')).toMatchObject({
      source: 'runtime',
      overrides: 1000,
      hasDeclared: true,
    });
    expect(fieldInfo(k, 'providers.gw\\.internal.endpoint')).toMatchObject({
      source: 'runtime',
      hasDeclared: false,
    });
    expect(fieldInfo(k, 'providers.gw\\.internal.endpoint').overrides).toBeUndefined();
    expect(removedProviders(k)).toEqual([{ name: 'local', path: 'providers.local' }]);
  });

  it('patches a budget, and unsets an emptied list of allowed models', () => {
    const k = models();
    const defs = modelFields(k);
    const initial = formOf(defs, k.effective);
    const form = { ...initial, 'providers.claude.budget.tokensPerDay': '500' };
    expect(formPatch(defs, initial, form, k.effective).patch).toEqual({
      providers: { claude: { budget: { tokensPerDay: 500 } } },
    });
    const allowed = defs.find((d) => d.path === 'providers.claude.allowedModels')!;
    expect(fromForm(allowed, '')).toEqual({ ok: undefined });
    expect(fromForm(allowed, 'a\nb')).toEqual({ ok: ['a', 'b'] });
  });
});

describe('adding a provider', () => {
  const input = { name: 'gateway', kind: 'openai', endpoint: 'https://llm.example/v1', secret: '' };

  it('makes the merge patch of a new provider', () => {
    expect(newProviderPatch([], input)).toEqual({
      patch: { providers: { gateway: { kind: 'openai', endpoint: 'https://llm.example/v1' } } },
    });
    expect(newProviderPatch([], { ...input, secret: 'gw' })).toEqual({
      patch: {
        providers: {
          gateway: {
            kind: 'openai',
            endpoint: 'https://llm.example/v1',
            apiKey: { secret: 'gw' },
          },
        },
      },
    });
  });

  it('refuses a missing or taken name, a bad endpoint and a bad secret name', () => {
    expect(newProviderPatch([], { ...input, name: '' })).toHaveProperty('error');
    expect(newProviderPatch(['gateway'], input)).toHaveProperty('error');
    expect(newProviderPatch([], { ...input, name: 'a b' })).toHaveProperty('error');
    expect(newProviderPatch([], { ...input, endpoint: 'llm.example' })).toHaveProperty('error');
    expect(newProviderPatch([], { ...input, endpoint: 'ftp://x' })).toHaveProperty('error');
    expect(newProviderPatch([], { ...input, kind: 'other' })).toHaveProperty('error');
    expect(newProviderPatch([], { ...input, secret: '.hidden' })).toHaveProperty('error');
  });
});

describe('keys', () => {
  afterEach(() => vi.unstubAllGlobals());

  const secret = (over: Partial<SecretInfo> = {}): SecretInfo => ({
    name: 'anthropic',
    source: 'declared',
    declared: true,
    locked: false,
    setAt: null,
    overridden: false,
    providers: ['claude'],
    ...over,
  });

  it('says whether a key is set and where it comes from', () => {
    expect(secretState(secret())).toMatchObject({ set: true, label: 'server config' });
    expect(secretState(secret({ source: 'runtime', setAt: '2026-10-10T00:00:00Z' }))).toMatchObject(
      { set: true, label: 'overrides server config', overrides: true },
    );
    expect(secretState(secret({ source: 'runtime', declared: false }))).toMatchObject({
      label: 'set here',
      overrides: false,
    });
    expect(secretState(secret({ source: 'missing', declared: false }))).toMatchObject({
      set: false,
      label: 'not set',
    });
  });

  it('sends a key once, in the body of a PUT, and removes one with DELETE', async () => {
    const calls: { url: string; init: RequestInit }[] = [];
    vi.stubGlobal('fetch', async (url: string, init: RequestInit = {}) => {
      calls.push({ url, init });
      return new Response(null, { status: 204 });
    });
    await putSecret('my key', 'sk-test');
    expect(calls[0].url).toBe('/$/server/secrets/my%20key');
    expect(calls[0].init.method).toBe('PUT');
    expect(JSON.parse(String(calls[0].init.body))).toEqual({ value: 'sk-test' });
    await deleteSecret('anthropic');
    expect(calls[1]).toMatchObject({
      url: '/$/server/secrets/anthropic',
      init: { method: 'DELETE' },
    });
    expect(MODELS_URL).toBe('/$/server/settings/models');
  });
});
