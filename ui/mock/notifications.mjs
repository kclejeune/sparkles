// Mock of the outbound notifications of a server whose settings file declares two
// channels and two routes (spec C21), for UI development and the mock UI tests:
// `GET /$/notifications`, `POST /$/notifications/test/{channel}` and
// `/$/server/settings/notifications`, in the shapes of docs/openapi.json
// (`NotificationStatus`, `NotificationTest`, `ServerSettingsKind`). The settings file
// locks the `phone` channel. Writes change a runtime layer kept in memory, merged as RFC
// 7396 says, and are checked as the server checks the main rules. A test to a channel
// whose target names `fail` answers 502 as an unreachable channel does, and every test
// joins the recent deliveries. `DELETE /$/server/settings/notifications` clears the
// runtime layer and the deliveries of tests, which the tests use to start again.

const DEFAULTS = {
  enabled: false,
  repeatEvery: '1d',
  channels: {},
  routes: {},
  delivery: { attempts: 5, backoffSecs: 30, maxBackoffSecs: 1800, timeoutSecs: 10 },
};
const DECLARED = {
  enabled: true,
  server: 'sparkles-dev',
  channels: {
    phone: { type: 'ntfy', topic: 'sparkles-ops', token: { secret: 'ntfy-token' } },
    ops: {
      type: 'webhook',
      url: 'https://hooks.example.org/sparkles',
      signingSecret: { secret: 'hook-signing' },
    },
  },
  routes: { 'backup.*': ['phone', 'ops'], '*': ['ops'] },
};
const LOCKED = ['channels.phone'];
const SECRET_MEMBERS = ['token', 'urlSecret', 'signingSecret', 'password'];

/** Deliveries since the mock started, before any test. */
const SEEDED = [
  {
    at: '2026-11-08T21:14:03Z',
    id: 'n-0007',
    event: 'backup.failed',
    channel: 'phone',
    result: 'ok',
    attempts: 1,
    status: 200,
  },
  {
    at: '2026-11-08T21:14:03Z',
    id: 'n-0007',
    event: 'backup.failed',
    channel: 'ops',
    result: 'failed',
    attempts: 5,
    status: 503,
    error: 'the webhook answered 503 Service Unavailable',
  },
  {
    at: '2026-11-09T06:02:41Z',
    id: 'n-0008',
    event: 'backup.verify.failed',
    channel: 'phone',
    result: 'ok',
    attempts: 2,
    status: 200,
  },
  {
    at: '2026-11-09T06:02:41Z',
    id: 'n-0008',
    event: 'backup.verify.failed',
    channel: 'ops',
    result: 'ok',
    attempts: 1,
    status: 204,
  },
];
const ACTIVE = [
  {
    key: 'backup.failed:nightly',
    event: 'backup.failed',
    dataset: null,
    first: '2026-11-08T21:14:03Z',
    last: '2026-11-08T21:14:03Z',
    count: 1,
  },
];

let runtime = {};
let version = 1;
let tests = [];
let testSeq = 100;

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

/** The effective object of a runtime layer: defaults, declared, runtime, then the locks. */
function effectiveOf(rt) {
  const eff = merge(merge(clone(DEFAULTS), DECLARED), rt);
  for (const l of LOCKED) {
    const p = parse(l);
    const v = at(DECLARED, p);
    if (v !== undefined) setAt(eff, p, clone(v));
  }
  return eff;
}

function leaves(v, base = []) {
  if (!isObj(v) || Object.keys(v).length === 0) return [base];
  return Object.entries(v).flatMap(([k, x]) => leaves(x, [...base, k]));
}

class Refused extends Error {
  constructor(field, message) {
    super(message);
    this.field = field;
  }
}

const URL_OK = /^https?:\/\/[^/@?#]+[^?#]*$/i;
const MAIL = /^[^\s<>,"();:\\[\]@]{1,64}@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*$/;

/** The checks of the server's `notifications` kind that a form can trip. */
function check(cfg) {
  const fail = (p, m) => {
    throw new Refused(pathOf(p), m);
  };
  const channels = cfg.channels ?? {};
  if (Object.keys(channels).length > 32) fail(['channels'], 'channels: at most 32 channels');
  for (const [name, c] of Object.entries(channels)) {
    const base = ['channels', name];
    const at = (m, msg) => fail([...base, m], `channels.${name}: ${msg}`);
    for (const m of SECRET_MEMBERS)
      if (c[m] !== undefined && !(isObj(c[m]) && typeof c[m].secret === 'string'))
        at(m, `${m}: a secret reference; the settings never hold a credential`);
    if (c.type === 'webhook') {
      if (!!c.url === !!c.urlSecret) at('url', 'a webhook has either url or urlSecret');
      if (c.url && !URL_OK.test(c.url)) at('url', 'url: not an http or https URL without a query');
    } else if (c.type === 'ntfy') {
      if (!/^[A-Za-z0-9_-]{1,64}$/.test(c.topic ?? ''))
        at('topic', 'topic: 1 to 64 letters, digits, - and _, as ntfy requires');
      if (c.priority !== undefined && !(c.priority >= 1 && c.priority <= 5))
        at('priority', 'priority: a number from 1 to 5');
    } else if (c.type === 'email') {
      if (!c.host) at('host', 'host: a host name or an IP address');
      if (c.tls !== undefined && !['starttls', 'tls'].includes(c.tls))
        at('tls', 'tls: starttls or tls; an email channel never connects without TLS');
      if (!MAIL.test(c.from ?? ''))
        at('from', `from: ${JSON.stringify(c.from)} is not a mail address`);
      if (!Array.isArray(c.to) || c.to.length === 0 || c.to.length > 20)
        at('to', 'to: 1 to 20 mail addresses');
      for (const t of c.to)
        if (!MAIL.test(t)) at('to', `to: ${JSON.stringify(t)} is not a mail address`);
      if (!!c.username !== !!c.password)
        at(c.username ? 'username' : 'password', 'a user name and a password go together');
    } else fail([...base, 'type'], `channels.${name}: unknown variant \`${c.type}\``);
  }
  for (const [key, list] of Object.entries(cfg.routes ?? {})) {
    if (!/^(\*|[a-z0-9-]+(\.[a-z0-9-]+)*(\.\*)?)$/.test(key))
      fail(['routes', key], `routes: ${JSON.stringify(key)} is not an event type, a prefix or *`);
    for (const c of list ?? [])
      if (!channels[c])
        fail(['routes', key], `routes.${key}: no channel named ${JSON.stringify(c)}`);
  }
  const d = cfg.delivery ?? {};
  if (!(d.attempts >= 1 && d.attempts <= 10))
    fail(['delivery', 'attempts'], 'delivery.attempts: a number from 1 to 10');
  if (d.maxBackoffSecs < d.backoffSecs)
    fail(['delivery', 'maxBackoffSecs'], 'delivery.maxBackoffSecs is at least backoffSecs');
}

/** `GET /$/server/settings/notifications`. */
function kind() {
  const effective = effectiveOf(runtime);
  const sources = {};
  for (const p of leaves(effective)) {
    const key = pathOf(p);
    sources[key] = LOCKED.some((l) => key === l || key.startsWith(`${l}.`))
      ? 'locked'
      : covers(runtime, p)
        ? 'runtime'
        : at(DECLARED, p) !== undefined
          ? 'declared'
          : 'default';
  }
  const overrides = [];
  for (const p of leaves(runtime)) {
    const key = pathOf(p);
    const d = at(DECLARED, p);
    const r = at(runtime, p);
    // a removed channel is listed once, below
    if (r === null) continue;
    if (d !== undefined && JSON.stringify(d) !== JSON.stringify(r))
      overrides.push({ path: key, declared: d, runtime: r });
  }
  for (const [name, v] of Object.entries(runtime.channels ?? {}))
    if (v === null)
      overrides.push({
        path: pathOf(['channels', name]),
        declared: DECLARED.channels[name],
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
    kind: 'notifications',
    effective,
    declared: clone(DECLARED),
    runtime: clone(runtime),
    sources,
    locked: LOCKED,
    overridden: [],
    overrides,
    status,
    etag: `"mock-notifications-${version}"`,
  };
}

const secretsOf = (c) =>
  SECRET_MEMBERS.map((m) => c[m]?.secret).filter((s) => typeof s === 'string');

function target(c) {
  if (c.type === 'webhook') return c.url ?? `the URL in secret ${c.urlSecret?.secret}`;
  if (c.type === 'ntfy') return `${(c.server ?? 'https://ntfy.sh').replace(/\/$/, '')}/${c.topic}`;
  const tls = c.tls ?? 'starttls';
  return `${c.host} port ${c.port ?? (tls === 'tls' ? 465 : 587)} (${tls}) to ${(c.to ?? []).join(', ')}`;
}

/** The deliveries of the seed and the tests, newest last. */
const deliveries = () => [...SEEDED, ...tests].slice(-50);

/** `GET /$/notifications`. */
function status() {
  const eff = effectiveOf(runtime);
  const all = deliveries();
  const channels = Object.entries(eff.channels).map(([name, c]) => {
    const mine = all.filter((d) => d.channel === name);
    const last = (ok) => {
      const d = mine.filter((x) => (x.result === 'ok') === ok).at(-1);
      return d
        ? {
            at: d.at,
            event: d.event,
            id: d.id,
            ...(d.status ? { status: d.status } : {}),
            ...(d.error ? { error: d.error } : {}),
          }
        : null;
    };
    return {
      name,
      type: c.type,
      target: target(c),
      secrets: secretsOf(c),
      lastSuccess: last(true),
      lastFailure: last(false),
      sent: mine.filter((d) => d.result === 'ok').length,
      failed: mine.filter((d) => d.result !== 'ok').length,
      queued: 0,
      inFlight: false,
      queueCapacity: Math.max(Math.floor(1000 / Math.max(Object.keys(eff.channels).length, 1)), 50),
    };
  });
  return {
    enabled: eff.enabled,
    channels,
    routes: eff.routes,
    queued: 0,
    active: ACTIVE,
    recent: all,
  };
}

/** The channels of the effective object that name each secret, for `/$/server/secrets`. */
export function channelSecretUsers() {
  const out = new Map();
  for (const [name, c] of Object.entries(effectiveOf(runtime).channels))
    for (const s of secretsOf(c)) out.set(s, [...(out.get(s) ?? []), name]);
  return out;
}

function readBody(req) {
  return new Promise((resolve) => {
    let b = '';
    req.on('data', (c) => (b += c));
    req.on('end', () => resolve(b));
  });
}

/** Handles the routes above; false when the request is not one of them. */
export function handleNotifications(req, res, seg, ctx) {
  const { send, fail } = ctx;
  if (seg[0] !== '$') return false;
  const path = seg.slice(1).join('/');
  if (path === 'notifications') {
    if (req.method !== 'GET') return (fail(res, 405, 'method not allowed'), true);
    send(res, 200, status());
    return true;
  }
  if (seg[1] === 'notifications' && seg[2] === 'test' && seg.length === 4) {
    if (req.method !== 'POST') return (fail(res, 405, 'method not allowed'), true);
    const name = decodeURIComponent(seg[3]);
    const c = effectiveOf(runtime).channels[name];
    if (!c) {
      send(res, 404, { error: `no channel named ${name}`, code: 'unknown-channel' });
      return true;
    }
    const id = `n-test-${testSeq++}`;
    const now = new Date().toISOString();
    const broken = target(c).includes('fail');
    const code = c.type === 'email' ? 250 : 200;
    tests.push({
      at: now,
      id,
      event: 'notification.test',
      channel: name,
      result: broken ? 'failed' : 'ok',
      attempts: 1,
      ...(broken ? { error: 'connecting: connection refused' } : { status: code }),
    });
    if (broken)
      send(res, 502, {
        error: `the test to ${name} failed: connecting: connection refused`,
        code: 'delivery-failed',
      });
    else
      send(res, 200, {
        channel: name,
        type: c.type,
        result: 'ok',
        status: code,
        id,
        latencyMs: 42,
      });
    return true;
  }
  if (path !== 'server/settings/notifications') return false;
  const url = new URL(req.url, 'http://mock');
  if (req.method === 'GET') {
    send(res, 200, kind());
    return true;
  }
  const ifMatch = req.headers['if-match'];
  if (ifMatch && ifMatch !== `"mock-notifications-${version}"`) {
    fail(res, 412, 'the notifications settings of the server changed; read them again');
    return true;
  }
  if (req.method === 'DELETE') {
    const field = url.searchParams.get('field');
    if (field) removeAt(runtime, parse(field));
    else {
      runtime = {};
      tests = [];
    }
    runtime = prune(runtime);
    version++;
    send(res, 200, kind());
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
    // `null` for a declared channel removes it: the layer keeps the null
    for (const [name, v] of Object.entries(patch.channels ?? {}))
      if (v === null && DECLARED.channels[name]) setAt(next, ['channels', name], null);
    next = prune(next);
    for (const l of LOCKED) {
      const p = parse(l);
      if (covers(patch, p) && JSON.stringify(at(next, p)) !== JSON.stringify(at(DECLARED, p)))
        return send(res, 409, {
          error: `the settings file locks ${l} of notifications for the server`,
          code: 'locked-by-config',
          fields: [l],
        });
    }
    try {
      check(effectiveOf(next));
    } catch (e) {
      if (!(e instanceof Refused)) throw e;
      return send(res, 400, {
        error: `notifications: ${e.message}`,
        code: 'bad-settings',
        field: e.field,
      });
    }
    runtime = next;
    version++;
    send(res, 200, kind());
  });
  return true;
}
