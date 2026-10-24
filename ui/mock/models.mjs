// Mock of the model configuration of a server run with `--model-config`, for UI
// development and the mock UI tests: `GET /$/models`, `/$/server/secrets` and
// `/$/server/settings/models`, in the shapes of docs/openapi.json (`ModelProviders`,
// `SecretList`, `ServerSettingsKind`). The declared configuration has three providers and
// locks claude's endpoint. Writes change a runtime layer kept in memory, merged as RFC
// 7396 says, and are checked as the server checks the main rules: a refused write is a
// 400 with `bad-settings` and the `field` it names. `DELETE /$/server/settings/models`
// clears the runtime layer, which the tests use to start from the declared state.

const DECLARED = {
  providers: {
    local: {
      kind: 'ollama',
      endpoint: 'http://127.0.0.1:11434',
      keepAlive: '10m',
      models: { 'qwen3:14b': { contextTokens: 32768 } },
    },
    claude: {
      kind: 'anthropic',
      endpoint: 'https://api.anthropic.com',
      apiKey: { secret: 'anthropic' },
      budget: { tokensPerDay: 2000000 },
      allowedModels: ['claude-haiku-5-5', 'claude-sonnet-5-5'],
      models: {
        'claude-haiku-5-5': { pricing: { inputPerMTok: 0.1, outputPerMTok: 0.5 } },
      },
    },
    gateway: {
      kind: 'openai',
      endpoint: 'https://llm.internal.example/v1',
      apiKey: { secret: 'gateway' },
      structuredOutput: 'json-schema',
      headers: { 'X-Team': 'search' },
    },
  },
  roles: {
    draft: [
      { provider: 'local', model: 'qwen3:14b' },
      { provider: 'claude', model: 'claude-sonnet-5-5', maxOutputTokens: 1024 },
    ],
    summarize: [{ provider: 'claude', model: 'claude-haiku-5-5' }],
  },
};
const LOCKED = ['providers.claude.endpoint'];
/** Models a provider reports, beyond those the configuration names. */
const REPORTED = { local: ['llama3.3:70b', 'qwen3:8b'], gateway: ['gpt-5-mini', 'gpt-5'] };

let runtime = {};
let version = 1;
/** Stored keys by secret name. */
const stored = new Set(['gateway']);

const isObj = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
const clone = (v) => (v === undefined ? v : JSON.parse(JSON.stringify(v)));
const esc = (k) => k.replace(/\\/g, '\\\\').replace(/\./g, '\\.');
const pathOf = (p) => p.map(esc).join('.');
const parse = (s) => {
  const out = [];
  let cur = '';
  for (let i = 0; i < s.length; i++) {
    if (s[i] === '\\' && i + 1 < s.length) cur += s[++i];
    else if (s[i] === '.') (out.push(cur), (cur = ''));
    else cur += s[i];
  }
  out.push(cur);
  return out;
};

function merge(target, patch) {
  if (!isObj(patch)) return clone(patch);
  const out = isObj(target) ? { ...target } : {};
  for (const [k, v] of Object.entries(patch)) {
    if (v === null) delete out[k];
    else out[k] = merge(out[k], v);
  }
  return out;
}

function prune(v) {
  if (!isObj(v)) return v;
  const out = {};
  for (const [k, x] of Object.entries(v)) {
    const p = prune(x);
    if (!(isObj(p) && Object.keys(p).length === 0)) out[k] = p;
  }
  return out;
}

const at = (v, p) => p.reduce((c, k) => (isObj(c) ? c[k] : undefined), v);
function setAt(v, p, x) {
  let c = v;
  p.forEach((k, i) => {
    if (i === p.length - 1) c[k] = x;
    else c = c[k] = isObj(c[k]) ? c[k] : {};
  });
}
function removeAt(v, p) {
  const parent = at(v, p.slice(0, -1));
  if (isObj(parent)) delete parent[p[p.length - 1]];
}
const covers = (layer, p) => {
  let c = layer;
  for (const k of p) {
    if (!isObj(c)) return c !== undefined;
    if (!(k in c)) return false;
    c = c[k];
  }
  return true;
};

/** The effective configuration of a runtime layer: declared, then runtime, then the locks. */
function effectiveOf(rt) {
  const eff = merge({ providers: {}, roles: {}, ...clone(DECLARED) }, rt);
  for (const l of LOCKED) {
    const p = parse(l);
    const v = at(DECLARED, p);
    if (v !== undefined) setAt(eff, p, clone(v));
  }
  return eff;
}

function leaves(v, base = []) {
  if (!isObj(v) || Object.keys(v).length === 0) return [base];
  // a role list is one field
  if (base.length === 2 && base[0] === 'roles') return [base];
  return Object.entries(v).flatMap(([k, x]) => leaves(x, [...base, k]));
}

class Refused extends Error {
  constructor(field, message) {
    super(message);
    this.field = field;
  }
}

/** The checks of the server's model configuration that a form can trip. */
function check(cfg) {
  const fail = (p, m) => {
    throw new Refused(pathOf(p), m);
  };
  for (const [name, p] of Object.entries(cfg.providers ?? {})) {
    const base = ['providers', name];
    if (!['ollama', 'openai', 'anthropic', 'local'].includes(p.kind))
      fail([...base, 'kind'], `provider ${name}: unknown variant \`${p.kind}\``);
    if (p.kind !== 'local' && !/^https?:\/\/[^/@?#]+[^?#]*$/i.test(p.endpoint ?? ''))
      fail(
        [...base, 'endpoint'],
        `provider ${name}: endpoint ${JSON.stringify(p.endpoint ?? '')}: use an http or https URL`,
      );
    if (p.apiKey && !/^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$/.test(p.apiKey.secret ?? ''))
      fail(
        [...base, 'apiKey', 'secret'],
        `provider ${name}: apiKey: secret name ${JSON.stringify(p.apiKey.secret)}: use 1 to 128 letters, digits, '_', '-' and '.', not starting with '.' or '-'`,
      );
    for (const h of Object.keys(p.headers ?? {}))
      if (
        ['authorization', 'x-api-key', 'api-key', 'proxy-authorization'].includes(h.toLowerCase())
      )
        fail(
          [...base, 'headers', h],
          `provider ${name}: the header ${h} carries a key; name the key with apiKey and a secret`,
        );
    if (p.keepAlive !== undefined && p.kind !== 'ollama')
      fail([...base, 'keepAlive'], `provider ${name}: keepAlive applies to the ollama kind only`);
    if (p.concurrency === 0)
      fail([...base, 'concurrency'], `provider ${name}: concurrency must be at least 1`);
    const entries = [
      ['(defaults)', p, base],
      ...Object.entries(p.models ?? {}).map(([m, o]) => [m, o, [...base, 'models', m]]),
    ];
    for (const [m, o, b] of entries) {
      const what = `provider ${name}, model ${m}`;
      if (o.contextTokens !== undefined && !(o.contextTokens >= 1))
        fail([...b, 'contextTokens'], `${what}: contextTokens must be at least 1`);
      if (o.maxOutputTokens !== undefined && !(o.maxOutputTokens >= 1))
        fail([...b, 'maxOutputTokens'], `${what}: maxOutputTokens must be at least 1`);
      if (o.temperature !== undefined && !(o.temperature >= 0 && o.temperature <= 2))
        fail([...b, 'temperature'], `${what}: temperature must be between 0 and 2`);
      if (o.requestTimeoutSecs !== undefined && !(o.requestTimeoutSecs > 0))
        fail(
          [...b, 'requestTimeoutSecs'],
          `${what}: requestTimeoutSecs must be a positive number of seconds`,
        );
      if (o.structuredOutput === 'json-object' && p.kind === 'anthropic')
        fail([...b, 'structuredOutput'], `${what}: the anthropic kind offers no json-object level`);
      if (o.structuredOutput === 'tool' && p.kind === 'ollama')
        fail([...b, 'structuredOutput'], `${what}: the ollama kind offers no tool level`);
    }
  }
  for (const [role, list] of Object.entries(cfg.roles ?? {}))
    (list ?? []).forEach((e, i) => {
      const b = ['roles', role, String(i)];
      const p = cfg.providers?.[e.provider];
      if (!p)
        fail(
          [...b, 'provider'],
          `role ${role}: no provider named ${JSON.stringify(e.provider)} is defined`,
        );
      if (!e.model) fail([...b, 'model'], `role ${role}: an entry names no model`);
      if (Array.isArray(p.allowedModels) && !p.allowedModels.includes(e.model))
        fail(
          [...b, 'model'],
          `role ${role}: provider ${e.provider} does not allow the model ${JSON.stringify(e.model)} (allowedModels)`,
        );
    });
}

/** `GET /$/server/settings/models`. */
function modelsKind() {
  const effective = effectiveOf(runtime);
  const sources = {};
  for (const p of leaves(effective)) {
    const key = pathOf(p);
    sources[key] = LOCKED.some((l) => key === l || key.startsWith(`${l}.`))
      ? 'locked'
      : covers(runtime, p)
        ? 'runtime'
        : 'declared';
  }
  const overrides = [];
  for (const p of leaves(runtime)) {
    const key = pathOf(p);
    if (LOCKED.includes(key)) continue;
    const d = at(DECLARED, p);
    const r = at(runtime, p);
    if (d !== undefined && JSON.stringify(d) !== JSON.stringify(r))
      overrides.push({ path: key, declared: d, runtime: r });
  }
  for (const [name, v] of Object.entries(runtime.providers ?? {}))
    if (v === null)
      overrides.push({
        path: pathOf(['providers', name]),
        declared: DECLARED.providers[name],
        runtime: null,
      });
  let status = { valid: true };
  try {
    check(effective);
  } catch (e) {
    status = { valid: false, error: e.message };
  }
  return {
    scope: 'server',
    kind: 'models',
    effective,
    declared: clone(DECLARED),
    runtime: clone(runtime),
    sources,
    locked: LOCKED,
    overridden: [],
    overrides,
    status,
    etag: `"mock-models-${version}"`,
  };
}

/** `GET /$/models`. */
function status() {
  const eff = effectiveOf(runtime);
  const providers = Object.entries(eff.providers).map(([name, p]) => {
    const models = new Set(Object.keys(p.models ?? {}));
    for (const list of Object.values(eff.roles ?? {}))
      for (const e of list ?? []) if (e.provider === name) models.add(e.model);
    for (const m of REPORTED[name] ?? []) models.add(m);
    const secret = p.apiKey?.secret;
    const source = !secret
      ? undefined
      : stored.has(secret)
        ? 'runtime'
        : secret === 'anthropic'
          ? 'declared'
          : 'missing';
    return {
      name,
      kind: p.kind,
      endpoint: p.endpoint ?? '',
      status: source === 'missing' ? 'secret-missing' : 'ok',
      concurrency: p.concurrency ?? 4,
      ...(secret ? { apiKey: { secret, source } } : {}),
      models: [...models].map((m) => ({ name: m, status: { state: 'untested' } })),
    };
  });
  return { configured: true, providers, roles: eff.roles ?? {} };
}

/** `GET /$/server/secrets`. */
function secrets() {
  const eff = effectiveOf(runtime);
  const names = new Map();
  for (const [name, p] of Object.entries(eff.providers))
    if (p.apiKey?.secret) names.set(p.apiKey.secret, [...(names.get(p.apiKey.secret) ?? []), name]);
  for (const s of stored) if (!names.has(s)) names.set(s, []);
  return {
    storage: 'sealed',
    keySource: 'configured',
    keyMemory: 'locked',
    secrets: [...names].map(([name, providers]) => {
      const declared = name === 'anthropic';
      const runtimeSet = stored.has(name);
      return {
        name,
        source: runtimeSet ? 'runtime' : declared ? 'declared' : 'missing',
        declared,
        locked: false,
        setAt: runtimeSet ? '2026-10-24T09:00:00Z' : null,
        overridden: false,
        providers,
      };
    }),
  };
}

function readBody(req) {
  return new Promise((resolve) => {
    let b = '';
    req.on('data', (c) => (b += c));
    req.on('end', () => resolve(b));
  });
}

/** Handles the routes above; false when the request is not one of them. */
export function handleModels(req, res, seg, ctx) {
  const { send, fail } = ctx;
  if (seg[0] !== '$') return false;
  const path = seg.slice(1).join('/');
  if (path === 'models') {
    if (req.method !== 'GET') return (fail(res, 405, 'method not allowed'), true);
    send(res, 200, status());
    return true;
  }
  if (path === 'server/secrets') {
    send(res, 200, secrets());
    return true;
  }
  if (seg[1] === 'server' && seg[2] === 'secrets' && seg.length === 4) {
    const name = decodeURIComponent(seg[3]);
    if (req.method === 'PUT')
      void readBody(req).then(() => {
        stored.add(name);
        res.writeHead(204).end();
      });
    else if (req.method === 'DELETE') {
      stored.delete(name);
      res.writeHead(204).end();
    } else fail(res, 405, 'method not allowed');
    return true;
  }
  if (path !== 'server/settings/models') return false;
  const url = new URL(req.url, 'http://mock');
  if (req.method === 'GET') {
    send(res, 200, modelsKind());
    return true;
  }
  const ifMatch = req.headers['if-match'];
  if (ifMatch && ifMatch !== `"mock-models-${version}"`) {
    fail(res, 412, 'the models settings of the server changed; read them again');
    return true;
  }
  if (req.method === 'DELETE') {
    const field = url.searchParams.get('field');
    if (field) removeAt(runtime, parse(field));
    else runtime = {};
    runtime = prune(runtime);
    version++;
    send(res, 200, modelsKind());
    return true;
  }
  if (req.method !== 'PATCH') return (fail(res, 405, 'method not allowed'), true);
  void readBody(req).then((text) => {
    let patch;
    try {
      patch = JSON.parse(text);
    } catch {
      return send(res, 400, { error: 'the body is not JSON', code: 'bad-settings' });
    }
    let next = merge(runtime, patch);
    // `null` for a declared provider removes it: the layer keeps the null
    for (const [name, v] of Object.entries(patch.providers ?? {}))
      if (v === null && DECLARED.providers[name]) setAt(next, ['providers', name], null);
    next = prune(next);
    for (const l of LOCKED) {
      const p = parse(l);
      if (covers(patch, p) && JSON.stringify(at(next, p)) !== JSON.stringify(at(DECLARED, p))) {
        return send(res, 409, {
          error: `the settings file locks ${l} of models for the server`,
          code: 'locked-by-config',
          fields: [l],
        });
      }
    }
    try {
      check(effectiveOf(next));
    } catch (e) {
      if (!(e instanceof Refused)) throw e;
      return send(res, 400, {
        error: `models: ${e.message}`,
        code: 'bad-settings',
        field: e.field,
      });
    }
    runtime = next;
    version++;
    send(res, 200, modelsKind());
  });
  return true;
}
