// The server's model configuration as a layered settings kind (spec C19 §11): the client
// of `GET /$/models` and `/$/server/secrets`, the form of the `models` kind for the
// server page's Models section, and the words for a key's state. Keys are write-only, so
// nothing here ever holds one after it is sent.

import { json } from './api';
import {
  parsePath,
  pathString,
  type FieldDef,
  type JsonObject,
  type SettingsKind,
} from './settings';

const enc = encodeURIComponent;

/** The route of the `models` kind. */
export const MODELS_URL = '/$/server/settings/models';

/** The protocols a provider speaks. */
export const PROVIDER_KINDS = ['ollama', 'openai', 'anthropic'] as const;

/** The steps that call a model, in the order the Models section lists them. */
export const ROLES = ['draft', 'repair', 'summarize', 'extract', 'explain', 'optimize'] as const;

const LEVELS = ['auto', 'json-schema', 'json-object', 'tool', 'text'] as const;

/** One provider of `GET /$/models`. */
export type ProviderStatus = {
  name: string;
  kind: string;
  endpoint: string;
  /** `ok`, or `secret-missing` when its key cannot be read. */
  status: string;
  concurrency?: number;
  apiKey?: { secret: string; source: 'declared' | 'runtime' | 'missing' };
  models: { name: string; status?: { state: string; message?: string } }[];
};

/** `GET /$/models`. */
export type ModelsStatus = {
  configured: boolean;
  providers: ProviderStatus[];
  roles: Record<string, { provider: string; model: string }[]>;
};

export const modelsStatus = (signal?: AbortSignal) =>
  json<ModelsStatus>('/$/models', { signal, cache: 'no-store' });

/** One secret of `GET /$/server/secrets`. */
export type SecretInfo = {
  name: string;
  source: 'declared' | 'runtime' | 'missing';
  /** A `--model-secret` source exists. */
  declared: boolean;
  locked: boolean;
  setAt: string | null;
  /** A stored value that the lock ignores. */
  overridden: boolean;
  providers: string[];
};

export const listSecrets = (signal?: AbortSignal) =>
  json<{ secrets: SecretInfo[] }>('/$/server/secrets', { signal, cache: 'no-store' });

/** Store a runtime value for a secret. The value is sent once and kept nowhere. */
export const putSecret = (name: string, value: string) =>
  json<void>(`/$/server/secrets/${enc(name)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ value }),
  });

/** Remove a secret's runtime value, so the declared source applies again. */
export const deleteSecret = (name: string) =>
  json<void>(`/$/server/secrets/${enc(name)}`, { method: 'DELETE' });

/** The rule of the server for secret names. */
export const SECRET_NAME = /^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$/;

/** What a key's row says: whether a key is set, and where it comes from. */
export function secretState(s: SecretInfo): {
  set: boolean;
  label: string;
  title: string;
  overrides: boolean;
} {
  if (s.source === 'runtime')
    return s.declared
      ? {
          set: true,
          label: 'overrides server config',
          title:
            "A key stored here is used in place of the key of the server's configuration (--model-secret).",
          overrides: true,
        }
      : {
          set: true,
          label: 'set here',
          title: 'A key stored here or through the API.',
          overrides: false,
        };
  if (s.source === 'declared')
    return {
      set: true,
      label: 'server config',
      title: "The key comes from the server's configuration (--model-secret).",
      overrides: false,
    };
  return {
    set: false,
    label: 'not set',
    title: 'No key: the providers that use this secret cannot be called.',
    overrides: false,
  };
}

/** The providers of a `models` answer, by name. */
export const providerNames = (k: SettingsKind | null) =>
  Object.keys((k?.effective.providers as JsonObject | undefined) ?? {}).sort();

/** The kind of each provider, which decides its form's fields. */
export function providerKinds(k: SettingsKind | null): Record<string, string> {
  const ps = (k?.effective.providers as Record<string, JsonObject> | undefined) ?? {};
  return Object.fromEntries(Object.entries(ps).map(([n, p]) => [n, String(p?.kind ?? '')]));
}

/** Declared providers removed at runtime: overrides whose runtime value is `null`. */
export function removedProviders(k: SettingsKind | null): { name: string; path: string }[] {
  return (k?.overrides ?? [])
    .filter((o) => o.runtime === null)
    .map((o) => ({ path: o.path, p: parsePath(o.path) }))
    .filter(({ p }) => p.length === 2 && p[0] === 'providers')
    .map(({ path, p }) => ({ name: p[1], path }));
}

/** The group heading of a provider's fields. */
export const providerGroup = (name: string) => `Provider ${name}`;

/** The provider a group heading names, if it names one. */
export const groupProvider = (group: string) =>
  group.startsWith('Provider ') ? group.slice('Provider '.length) : null;

/**
 * The form of the `models` kind: the common fields of each provider and its per-model
 * options, the role lists and the routing settings. Headers and other members the form
 * leaves out are edited in the runtime layer's JSON.
 */
export function modelFields(k: SettingsKind | null): FieldDef[] {
  const out: FieldDef[] = [];
  const kinds = providerKinds(k);
  for (const name of providerNames(k)) {
    const at = (m: string) => pathString(['providers', name, ...m.split('.')]);
    const group = providerGroup(name);
    const kind = kinds[name];
    out.push(
      { path: at('kind'), group, label: 'Protocol', type: 'enum', options: PROVIDER_KINDS },
      {
        path: at('endpoint'),
        group,
        label: 'Endpoint',
        type: 'text',
        help: 'The base URL. A private address needs --outbound-allow-private.',
      },
      {
        path: at('apiKey.secret'),
        group,
        label: 'Key (secret name)',
        type: 'text',
        optional: true,
        placeholder: 'no key',
        help: 'The secret that holds the key. The key itself is set under API keys.',
      },
      {
        path: at('budget.tokensPerDay'),
        group,
        label: 'Tokens per day',
        type: 'int',
        min: 1,
        optional: true,
        placeholder: 'no cap',
      },
      {
        path: at('concurrency'),
        group,
        label: 'Concurrent calls',
        type: 'int',
        min: 1,
        optional: true,
        fallback: 4,
      },
      {
        path: at('requestsPerMinute'),
        group,
        label: 'Requests per minute',
        type: 'int',
        min: 1,
        optional: true,
        placeholder: 'no limit',
      },
      {
        path: at('allowedModels'),
        group,
        label: 'Allowed models',
        type: 'lines',
        optional: true,
        placeholder: 'any model, one per line',
      },
      {
        path: at('contextTokens'),
        group,
        label: 'Context tokens',
        type: 'int',
        min: 1,
        optional: true,
        fallback: 8192,
      },
      {
        path: at('maxOutputTokens'),
        group,
        label: 'Output tokens',
        type: 'int',
        min: 1,
        optional: true,
        fallback: 2048,
      },
      {
        path: at('structuredOutput'),
        group,
        label: 'Structured output',
        type: 'enum',
        options: LEVELS,
        optional: true,
      },
      {
        path: at('requestTimeoutSecs'),
        group,
        label: 'Request timeout (s)',
        type: 'number',
        min: 0.001,
        optional: true,
        fallback: 60,
      },
    );
    if (kind === 'ollama')
      out.push(
        {
          path: at('numCtx'),
          group,
          label: 'Context (num_ctx)',
          type: 'int',
          min: 1,
          optional: true,
        },
        { path: at('keepAlive'), group, label: 'Keep alive', type: 'text', optional: true },
      );
    if (kind === 'anthropic')
      out.push({
        path: at('version'),
        group,
        label: 'API version',
        type: 'text',
        optional: true,
        placeholder: '2023-06-01',
      });
    out.push({
      path: at('models'),
      group,
      label: 'Per-model options',
      type: 'json',
      placeholder: '{ "qwen3:8b": { "contextTokens": 32768 } }',
      help: 'contextTokens, maxOutputTokens, temperature, structuredOutput, pricing, requestTimeoutSecs and numCtx by model.',
    });
  }
  for (const role of ROLES)
    out.push({
      path: pathString(['roles', role]),
      group: 'Roles',
      label: role,
      type: 'json',
      placeholder:
        role === 'repair' ? 'the draft list' : '[{ "provider": "local", "model": "qwen3:8b" }]',
      help:
        role === 'draft'
          ? 'Provider and model pairs, from the cheapest to the strongest. A role without a list is off.'
          : undefined,
    });
  out.push(
    {
      path: 'routing.complexityThreshold',
      group: 'Routing',
      label: 'Complexity threshold',
      type: 'int',
      min: 0,
      optional: true,
    },
    {
      path: 'routing.exampleScore',
      group: 'Routing',
      label: 'Example score',
      type: 'number',
      min: 0,
      max: 1,
      optional: true,
    },
  );
  return out;
}

/** The merge patch that adds a provider from the Add form, or why it cannot. */
export function newProviderPatch(
  existing: readonly string[],
  input: { name: string; kind: string; endpoint: string; secret: string },
): { patch: JsonObject } | { error: string } {
  const name = input.name.trim();
  const endpoint = input.endpoint.trim();
  const secret = input.secret.trim();
  if (!name) return { error: 'The provider needs a name.' };
  if (!/^[A-Za-z0-9_][A-Za-z0-9_.-]*$/.test(name))
    return { error: 'A provider name has letters, digits, _, - and . only.' };
  if (existing.includes(name)) return { error: `A provider named ${name} exists.` };
  if (!(PROVIDER_KINDS as readonly string[]).includes(input.kind))
    return { error: 'Pick the protocol the provider speaks.' };
  let url: URL;
  try {
    url = new URL(endpoint);
  } catch {
    return { error: 'The endpoint is not a URL.' };
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:')
    return { error: 'The endpoint is an http or https URL.' };
  if (secret && !SECRET_NAME.test(secret))
    return {
      error:
        'A secret name has 1 to 128 letters, digits, _, - and ., and does not start with . or -.',
    };
  const provider: JsonObject = { kind: input.kind, endpoint };
  if (secret) provider.apiKey = { secret };
  return { patch: { providers: { [name]: provider } } };
}
