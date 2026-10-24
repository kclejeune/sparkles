// The server's model configuration as a layered settings kind (spec C19 §11): the client
// of `GET /$/models` and `/$/server/secrets`, the form of the `models` kind for the
// server page's Models section, and the words for a key's state. Keys are write-only, so
// nothing here ever holds one after it is sent.

import { json } from './api';
import schemas from './models-schema.json';
import presets from './provider-presets.json';
import {
  deref,
  fieldsOf,
  type JsonSchema,
  type SchemaContext,
  type SchemaSet,
} from './schema-form';
import {
  getAt,
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
export const ROLES = [
  'draft',
  'repair',
  'summarize',
  'community',
  'extract',
  'explain',
  'optimize',
] as const;

/**
 * A template for a provider with a well-known endpoint. It fills the Add provider form,
 * and every field it fills stays editable. The table is `provider-presets.json`, which
 * the server's `sparkles settings set --global --preset` reads in Rust as well; a Rust
 * test checks that the two agree.
 */
export type ProviderPreset = {
  name: string;
  label: string;
  kind: (typeof PROVIDER_KINDS)[number];
  endpoint: string;
  /** The secret that holds the key, or null for a provider without one. */
  secret: string | null;
  /** A model to start with. */
  model: string;
};

export const PROVIDER_PRESETS = presets as readonly ProviderPreset[];

/** The Add form's fields for a preset, or empty ones for Custom (any other name). */
export function presetForm(name: string): {
  name: string;
  kind: string;
  endpoint: string;
  secret: string;
  model: string;
} {
  const p = PROVIDER_PRESETS.find((x) => x.name === name);
  if (!p) return { name: '', kind: 'openai', endpoint: '', secret: '', model: '' };
  return {
    name: p.name,
    kind: p.kind,
    endpoint: p.endpoint,
    secret: p.secret ?? '',
    model: p.model,
  };
}

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
  /** Certificate checks are off (`tls.insecureSkipVerify`). */
  unverified?: boolean;
  tls?: {
    verification: 'system' | 'custom-ca' | 'off';
    caCert?: {
      file?: string;
      secret?: string;
      source?: string;
      status: 'ok' | 'unreadable';
      message?: string;
    };
  };
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

/** `GET /$/server/secrets`: how stored values are kept, and the secrets. */
export type SecretList = {
  /** `sealed` by default, `plaintext` (mode 0600) with `serve --secrets-key none`. */
  storage?: 'sealed' | 'plaintext';
  /**
   * `configured` for `serve --secrets-key` or the systemd credential, `generated` for the
   * key the server keeps in `<dataDir>/secrets.key`, `null` for plaintext values.
   */
  keySource?: 'configured' | 'generated' | null;
  /** `locked` or `heap`, where the server keeps the key, `null` until there is one. */
  keyMemory?: 'locked' | 'heap' | null;
  secrets: SecretInfo[];
};

export const listSecrets = (signal?: AbortSignal) =>
  json<SecretList>('/$/server/secrets', {
    signal,
    cache: 'no-store',
  });

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

/** Whether an endpoint is an https URL, the only kind that takes TLS options. */
export const isHttps = (endpoint: unknown) =>
  typeof endpoint === 'string' && /^https:\/\//i.test(endpoint.trim());

/** The providers whose endpoints are https URLs, which the TLS fields are shown for. */
export function httpsProviders(k: SettingsKind | null): string[] {
  const ps = (k?.effective.providers as Record<string, JsonObject> | undefined) ?? {};
  return Object.entries(ps)
    .filter(([, p]) => isHttps(p?.endpoint))
    .map(([n]) => n)
    .sort();
}

/** The path of a provider's `tls.insecureSkipVerify`. */
export const insecurePath = (name: string) =>
  pathString(['providers', name, 'tls', 'insecureSkipVerify']);

/**
 * The providers that a merge patch of the `models` kind turns certificate checks off for:
 * those whose `tls.insecureSkipVerify` it sets to true while the effective object does
 * not already have it. Saving such a patch needs the acknowledgement of what that means.
 */
export function enablesInsecure(patch: unknown, effective: JsonObject | undefined): string[] {
  const ps = getAt(patch, ['providers']);
  if (!ps || typeof ps !== 'object' || Array.isArray(ps)) return [];
  return Object.keys(ps)
    .filter((name) => getAt(patch, ['providers', name, 'tls', 'insecureSkipVerify']) === true)
    .filter((name) => getAt(effective, ['providers', name, 'tls', 'insecureSkipVerify']) !== true)
    .sort();
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

/** The schemas of the model configuration, from docs/openapi.json (`pnpm models-schema`). */
export const MODELS_SCHEMA = schemas as unknown as SchemaSet;

const root = MODELS_SCHEMA.ModelsSettings ?? {};
const PROVIDER_SCHEMA = deref(
  MODELS_SCHEMA,
  (root.properties?.providers?.additionalProperties as JsonSchema | undefined) ?? {},
);

/**
 * The form of the `models` kind, built from its schema: a group for each provider with
 * the members that apply to its kind (TLS for https endpoints only), its allowed models
 * as a list, its per-model options as a card per model and its headers as pairs; then
 * the role lists as rows of provider and model, and the routing. A role entry picks its
 * provider from those that answer prompts.
 */
export function modelFields(k: SettingsKind | null): FieldDef[] {
  const out: FieldDef[] = [];
  const kinds = providerKinds(k);
  const https = httpsProviders(k);
  const names = providerNames(k);
  for (const name of names) {
    const ctx: SchemaContext = {
      set: MODELS_SCHEMA,
      kind: kinds[name] || undefined,
      https: https.includes(name),
      group: providerGroup(name),
    };
    for (const d of fieldsOf(PROVIDER_SCHEMA, ['providers', name], ctx)) {
      if (d.type === 'map') d.keyLabel = 'model';
      if (d.type === 'pairs') d.keyLabel = 'header';
      if (d.type === 'list') d.emptyText = 'No model is allowed.';
      out.push(d);
    }
  }
  const chat = names.filter((n) => kinds[n] !== 'local');
  const roles = root.properties?.roles;
  if (roles)
    for (const d of fieldsOf(roles, ['roles'], { set: MODELS_SCHEMA, group: 'Roles' })) {
      if (d.type === 'records') {
        d.entry = d.entry?.map((f) =>
          f.path === 'provider' ? { ...f, type: 'enum', options: chat } : f,
        );
        d.emptyText = 'Empty list: the role is off.';
        d.keyLabel = 'entry';
      }
      out.push(d);
    }
  const routing = root.properties?.routing;
  if (routing)
    out.push(...fieldsOf(routing, ['routing'], { set: MODELS_SCHEMA, group: 'Routing' }));
  return out;
}

/**
 * The models to suggest for a provider: those its allowed models, its per-model options,
 * the role lists and `GET /$/models` name, in that order, each once.
 */
export function modelSuggestions(
  k: SettingsKind | null,
  status: ModelsStatus | null,
  provider: string,
): string[] {
  const p = (k?.effective.providers as Record<string, JsonObject> | undefined)?.[provider];
  const out: string[] = [];
  const add = (m: unknown) => {
    if (typeof m === 'string' && m && !out.includes(m)) out.push(m);
  };
  if (Array.isArray(p?.allowedModels)) p.allowedModels.forEach(add);
  if (p?.models && typeof p.models === 'object') Object.keys(p.models).forEach(add);
  for (const list of Object.values((k?.effective.roles as JsonObject | undefined) ?? {}))
    if (Array.isArray(list))
      for (const e of list)
        if (e && (e as JsonObject).provider === provider) add((e as JsonObject).model);
  status?.providers.find((x) => x.name === provider)?.models.forEach((m) => add(m.name));
  return out;
}

/**
 * The checks of the role lists that need the rest of the form, as the server makes them:
 * a provider that answers prompts, and a model its allowed models permit. The answer is
 * keyed by the dotted path of the member, such as `roles.draft.1.model`.
 */
export function roleErrors(
  k: SettingsKind | null,
  form: Record<string, unknown>,
): Record<string, string> {
  const out: Record<string, string> = {};
  const kinds = providerKinds(k);
  const allowed = (provider: string): unknown => {
    const path = pathString(['providers', provider, 'allowedModels']);
    return path in form
      ? form[path]
      : getAt(k?.effective, ['providers', provider, 'allowedModels']);
  };
  for (const role of ROLES) {
    const list = form[pathString(['roles', role])];
    if (!Array.isArray(list)) continue;
    list.forEach((e, i) => {
      const entry = (e ?? {}) as JsonObject;
      const provider = typeof entry.provider === 'string' ? entry.provider : '';
      const model = typeof entry.model === 'string' ? entry.model.trim() : '';
      const at = (m: string) => pathString(['roles', role, String(i), m]);
      if (!provider) return;
      if (!(provider in kinds)) out[at('provider')] = `No provider named ${provider} is defined`;
      else if (kinds[provider] === 'local')
        out[at('provider')] = `${provider} computes embeddings and does not answer prompts`;
      else {
        const a = allowed(provider);
        if (model && Array.isArray(a) && !a.includes(model))
          out[at('model')] = `${provider} does not allow ${model} (allowed models)`;
      }
    });
  }
  return out;
}

/** The merge patch that adds a provider from the Add form, or why it cannot. */
export function newProviderPatch(
  existing: readonly string[],
  input: {
    name: string;
    kind: string;
    endpoint: string;
    secret: string;
    model?: string;
  },
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
  const model = (input.model ?? '').trim();
  const provider: JsonObject = { kind: input.kind, endpoint };
  if (secret) provider.apiKey = { secret };
  // an empty entry would vanish in the merge, so it states the default level
  if (model) provider.models = { [model]: { structuredOutput: 'auto' } };
  return { patch: { providers: { [name]: provider } } };
}
