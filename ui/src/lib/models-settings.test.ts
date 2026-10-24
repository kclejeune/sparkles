import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  MODELS_URL,
  PROVIDER_KINDS,
  PROVIDER_PRESETS,
  deleteSecret,
  enablesInsecure,
  groupProvider,
  httpsProviders,
  insecurePath,
  modelFields,
  modelSuggestions,
  newProviderPatch,
  presetForm,
  providerGroup,
  providerNames,
  putSecret,
  removedProviders,
  roleErrors,
  secretState,
  type SecretInfo,
} from './models-settings';
import {
  checkStructured,
  fieldInfo,
  formOf,
  formPatch,
  fromForm,
  inheritedFor,
  type SettingsKind,
} from './settings';

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
    {
      path: 'providers.claude.budget.tokensPerDay',
      declared: 1000,
      runtime: 7,
    },
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
    expect(paths).toContain('roles.community');
    expect(paths).toContain('routing.exampleScore');
    expect(groupProvider(providerGroup('gw.internal'))).toBe('gw.internal');
    expect(groupProvider('Roles')).toBeNull();
    expect(modelFields(null).filter((d) => d.group?.startsWith('Provider'))).toEqual([]);
  });

  it('explains community role inheritance and explicit opt-out', () => {
    expect(modelFields(models()).find((d) => d.path === 'roles.community')).toMatchObject({
      type: 'records',
      unsetText: 'the summarize list',
      help: 'The reports of GraphRAG communities. It uses the summarize list when unset, and an empty list turns it off.',
    });
  });

  it('builds typed controls from the schema', () => {
    const f = Object.fromEntries(modelFields(models()).map((d) => [d.path, d]));
    expect(f['providers.claude.allowedModels']).toMatchObject({
      type: 'list',
      section: 'Models',
      placeholder: 'any model',
    });
    expect(f['providers.claude.apiKey.secret']).toMatchObject({ type: 'secret', label: 'API key' });
    expect(f['providers.claude.temperature']).toMatchObject({
      type: 'number',
      min: 0,
      max: 2,
      section: 'Model defaults',
    });
    expect(f['providers.claude.requestTimeoutSecs']).toMatchObject({ above: 0, unit: 'seconds' });
    // the anthropic kind offers no json-object level, the ollama kind no tool level
    expect(f['providers.claude.structuredOutput'].options).not.toContain('json-object');
    expect(f['providers.gw\\.internal.structuredOutput'].options).not.toContain('tool');
    // headers are for the openai kind only
    expect(f['providers.claude.headers']).toBeUndefined();
    const perModel = f['providers.claude.models'];
    expect(perModel.type).toBe('map');
    expect(perModel.entry!.map((e) => e.path)).toEqual([
      'contextTokens',
      'maxOutputTokens',
      'temperature',
      'structuredOutput',
      'requestTimeoutSecs',
      'pricing.inputPerMTok',
      'pricing.outputPerMTok',
    ]);
    // a role entry picks among the providers that answer prompts
    expect(f['roles.draft'].entry!.find((e) => e.path === 'provider')).toMatchObject({
      type: 'enum',
      options: ['claude', 'gw.internal'],
    });
  });

  it('checks role entries against the providers and their allowed models', () => {
    const k = models();
    const form = {
      'roles.draft': [
        { provider: 'claude', model: 'c' },
        { provider: 'nope', model: 'm' },
        { provider: 'claude', model: 'a' },
        { provider: 'q', model: 'm' },
      ],
    };
    const providers = k.effective.providers as Record<string, Record<string, unknown>>;
    const pending = {
      ...k.effective,
      providers: {
        ...providers,
        claude: { ...providers.claude, allowedModels: ['a'] },
        // a provider that the pending change adds, as an Advanced JSON edit can
        q: { kind: 'openai', endpoint: 'http://q/v1' },
      },
    };
    expect(roleErrors(pending, form)).toEqual({
      'roles.draft.0.model': 'claude does not allow c (allowed models)',
      'roles.draft.1.provider': 'No provider named nope is defined',
    });
    // the provider select lists the loaded providers, but a new one is not refused there
    const draft = modelFields(k).find((d) => d.path === 'roles.draft')!;
    expect(checkStructured(draft, [{ provider: 'q', model: 'm' }])).toEqual({});
  });

  it('keeps the spaces of free text, and trims formatted text', () => {
    const k = models();
    k.effective.providers = { emb: { kind: 'local' } };
    const f = Object.fromEntries(modelFields(k).map((d) => [d.path, d]));
    expect(f['providers.emb.queryPrefix'].literal).toBe(true);
    expect(fromForm(f['providers.emb.queryPrefix'], 'query: ')).toEqual({ ok: 'query: ' });
    expect(fromForm(f['providers.emb.queryPrefix'], '   ')).toEqual({ ok: undefined });
    const claude = Object.fromEntries(modelFields(models()).map((d) => [d.path, d]));
    expect(claude['providers.claude.endpoint'].literal).toBeUndefined();
    expect(fromForm(claude['providers.claude.endpoint'], ' https://x ')).toEqual({
      ok: 'https://x',
    });
  });

  it('lets an optional key reference go', () => {
    const k = models();
    const defs = modelFields(k);
    const key = defs.find((d) => d.path === 'providers.claude.apiKey.secret')!;
    expect(key.optional).toBe(true);
    const initial = formOf(defs, k.effective);
    const form = { ...initial, 'providers.claude.apiKey.secret': '' };
    const r = formPatch(defs, initial, form, k.effective);
    expect(r.errors).toEqual({});
    expect(r.patch).toEqual({ providers: { claude: { apiKey: { secret: null } } } });
  });

  it('shows what a model inherits, and the default of a price its entry replaces', () => {
    const perModel = modelFields(models()).find((d) => d.path === 'providers.claude.models')!;
    const inherited = { temperature: 0.4, 'pricing.inputPerMTok': 1, 'pricing.outputPerMTok': 10 };
    expect(inheritedFor(perModel.entry!, {}, inherited)).toMatchObject(inherited);
    const own = inheritedFor(perModel.entry!, { pricing: { inputPerMTok: 2 } }, inherited);
    expect(own).toMatchObject({ temperature: 0.4, 'pricing.outputPerMTok': 0 });
  });

  it('suggests the models a provider names anywhere', () => {
    const k = models();
    const status = {
      configured: true,
      providers: [
        {
          name: 'claude',
          kind: 'anthropic',
          endpoint: '',
          status: 'ok',
          models: [{ name: 'r' }, { name: 'c' }],
        },
      ],
      roles: {},
    };
    expect(modelSuggestions(k, status, 'claude')).toEqual(['c', 'r']);
    expect(modelSuggestions(k, null, 'gw.internal')).toEqual([]);
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
    expect(fromForm(allowed, undefined)).toEqual({ ok: undefined });
    expect(fromForm(allowed, [' a', 'b'])).toEqual({ ok: ['a', 'b'] });
    expect(fromForm(allowed, ['a', 'a'])).toHaveProperty('error', 'a is listed twice');
    const cleared = { ...initial, 'providers.claude.allowedModels': undefined };
    expect(formPatch(defs, initial, cleared, k.effective).patch).toEqual({});
  });
});

describe('adding a provider', () => {
  const input = {
    name: 'gateway',
    kind: 'openai',
    endpoint: 'https://llm.example/v1',
    secret: '',
  };

  it('makes the merge patch of a new provider', () => {
    expect(newProviderPatch([], input)).toEqual({
      patch: {
        providers: {
          gateway: { kind: 'openai', endpoint: 'https://llm.example/v1' },
        },
      },
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

describe('presets', () => {
  it('are the providers with well-known endpoints, and no generic gateway', () => {
    expect(PROVIDER_PRESETS.map((p) => [p.name, p.kind, p.endpoint, p.secret])).toEqual([
      ['anthropic', 'anthropic', 'https://api.anthropic.com', 'anthropic'],
      ['openai', 'openai', 'https://api.openai.com/v1', 'openai'],
      ['ollama', 'ollama', 'http://127.0.0.1:11434', null],
    ]);
    for (const p of PROVIDER_PRESETS) {
      expect(p.model).not.toBe('');
      expect(PROVIDER_KINDS).toContain(p.kind);
    }
  });

  it('fill the Add form, and Custom empties it', () => {
    expect(presetForm('anthropic')).toEqual({
      name: 'anthropic',
      kind: 'anthropic',
      endpoint: 'https://api.anthropic.com',
      secret: 'anthropic',
      model: 'claude-sonnet-5-5',
    });
    expect(presetForm('ollama').secret).toBe('');
    expect(presetForm('')).toEqual({
      name: '',
      kind: 'openai',
      endpoint: '',
      secret: '',
      model: '',
    });
  });

  it('give a provider the server accepts, with the edited fields and its model', () => {
    const f = { ...presetForm('openai'), name: 'oai', secret: 'team-key' };
    expect(newProviderPatch([], f)).toEqual({
      patch: {
        providers: {
          oai: {
            kind: 'openai',
            endpoint: 'https://api.openai.com/v1',
            apiKey: { secret: 'team-key' },
            models: { 'gpt-5-mini': { structuredOutput: 'auto' } },
          },
        },
      },
    });
    const local = presetForm('ollama');
    expect(newProviderPatch([], { ...local, model: '' })).toEqual({
      patch: {
        providers: {
          ollama: { kind: 'ollama', endpoint: 'http://127.0.0.1:11434' },
        },
      },
    });
  });
});

describe('TLS options', () => {
  it('are fields of https providers only', () => {
    const k = models();
    expect(httpsProviders(k)).toEqual(['claude']);
    const paths = modelFields(k).map((d) => d.path);
    expect(paths).toContain('providers.claude.tls.caCert.file');
    expect(paths).toContain('providers.claude.tls.caCert.secret');
    expect(paths).toContain(insecurePath('claude'));
    expect(paths).not.toContain(insecurePath('gw.internal'));
    const skip = modelFields(k).find((d) => d.path === insecurePath('claude'))!;
    expect(skip.type).toBe('bool');
    // unchecked unless the configuration sets it
    expect(formOf([skip], k.effective)[skip.path]).toBe(false);
  });

  it('need the acknowledgement only for a patch that turns verification off', () => {
    const k = models();
    const defs = modelFields(k);
    const initial = formOf(defs, k.effective);
    const on = formPatch(
      defs,
      initial,
      { ...initial, [insecurePath('claude')]: true },
      k.effective,
    );
    expect(on.patch).toEqual({
      providers: { claude: { tls: { insecureSkipVerify: true } } },
    });
    expect(enablesInsecure(on.patch, k.effective)).toEqual(['claude']);
    // the runtime JSON is checked the same way
    expect(
      enablesInsecure(
        { providers: { 'gw.internal': { tls: { insecureSkipVerify: true } } } },
        k.effective,
      ),
    ).toEqual(['gw.internal']);
    // a CA, turning verification back on, and other changes need none
    expect(
      enablesInsecure(
        { providers: { claude: { tls: { caCert: { file: '/etc/ca.pem' } } } } },
        k.effective,
      ),
    ).toEqual([]);
    expect(
      enablesInsecure({ providers: { claude: { tls: { insecureSkipVerify: false } } } }, {}),
    ).toEqual([]);
    expect(enablesInsecure({ roles: {} }, k.effective)).toEqual([]);
    expect(enablesInsecure({ providers: { claude: null } }, k.effective)).toEqual([]);
    // a provider whose verification is off already
    const off = {
      providers: { claude: { tls: { insecureSkipVerify: true } } },
    };
    expect(enablesInsecure(off, off)).toEqual([]);
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
    expect(secretState(secret())).toMatchObject({
      set: true,
      label: 'server config',
    });
    expect(secretState(secret({ source: 'runtime', setAt: '2026-10-10T00:00:00Z' }))).toMatchObject(
      {
        set: true,
        label: 'overrides server config',
        overrides: true,
      },
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
    expect(JSON.parse(String(calls[0].init.body))).toEqual({
      value: 'sk-test',
    });
    await deleteSecret('anthropic');
    expect(calls[1]).toMatchObject({
      url: '/$/server/secrets/anthropic',
      init: { method: 'DELETE' },
    });
    expect(MODELS_URL).toBe('/$/server/settings/models');
  });
});
