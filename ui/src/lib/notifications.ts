// The server's outbound notifications (spec C21) for the server page's Notifications
// section: the client of `GET /$/notifications` and of the test send, the form of the
// `notifications` kind built from its schema, and the merge patches that add and remove
// channels and routes. A route is what the section calls a subscription, the channels an
// event type goes to. Credentials are named by secrets and never held here.

import { json } from './api';
import schemas from './notifications-schema.json';
import { deref, fieldsOf, type JsonSchema, type SchemaSet } from './schema-form';
import {
  getAt,
  parsePath,
  pathString,
  type FieldDef,
  type FormValues,
  type JsonObject,
  type SettingsKind,
} from './settings';

const enc = encodeURIComponent;

/** The route of the `notifications` kind. */
export const NOTIFICATIONS_URL = '/$/server/settings/notifications';

/** The channel types, in the order the Add form offers them. */
export const CHANNEL_TYPES = ['webhook', 'ntfy', 'email'] as const;
export type ChannelType = (typeof CHANNEL_TYPES)[number];

/** The most channels the kind holds. */
export const MAX_CHANNELS = 32;

/** A delivery a channel reported, as the last success or failure of its status. */
export type DeliveryOutcome = {
  at: string;
  event: string;
  id?: string;
  status?: number;
  error?: string;
};

/** One channel of `GET /$/notifications`. */
export type ChannelStatus = {
  name: string;
  type: ChannelType;
  /** Where the channel delivers, without credentials. */
  target: string;
  secrets: string[];
  lastSuccess: DeliveryOutcome | null;
  lastFailure: DeliveryOutcome | null;
  sent: number;
  failed: number;
  queued: number;
  inFlight: boolean;
  queueCapacity: number;
};

/** One of the recent deliveries. */
export type RecentDelivery = {
  at: string;
  id: string;
  event: string;
  channel: string;
  result: 'ok' | 'failed' | 'refused' | 'dropped';
  attempts?: number;
  status?: number;
  error?: string;
};

/** A lasting condition that was notified and has not cleared. */
export type ActiveCondition = {
  key: string;
  event: string;
  dataset: string | null;
  first: string;
  last: string;
  count: number;
};

/** `GET /$/notifications`. */
export type NotificationStatus = {
  enabled: boolean;
  channels: ChannelStatus[];
  routes: Record<string, string[]>;
  queued: number;
  active: ActiveCondition[];
  recent: RecentDelivery[];
};

/** `POST /$/notifications/test/{channel}`, when the channel accepted the test. */
export type TestResult = {
  channel: string;
  type: ChannelType;
  result: 'ok';
  status: number;
  id: string;
  latencyMs: number;
};

export const notificationStatus = (signal?: AbortSignal) =>
  json<NotificationStatus>('/$/notifications', { signal, cache: 'no-store' });

/** Send a test notification to a channel. A failed delivery is an error with the reason. */
export const testChannel = (name: string) =>
  json<TestResult>(`/$/notifications/test/${enc(name)}`, { method: 'POST' });

/** The newest deliveries first. */
export const newestFirst = (recent: readonly RecentDelivery[]) =>
  [...recent].sort((a, b) => (a.at < b.at ? 1 : a.at > b.at ? -1 : 0));

/** The badge class of a delivery's result. */
export const resultClass = (r: string) => (r === 'ok' ? 'ok' : r === 'dropped' ? 'warn' : 'danger');

/** What a delivery's result says beyond its word: the status or the error. */
export function outcomeText(d: { status?: number; error?: string }): string {
  if (d.error) return d.status ? `${d.status}: ${d.error}` : d.error;
  return d.status ? String(d.status) : '';
}

// --- the kind --------------------------------------------------------------------------

/** The schemas of the kind, from docs/openapi.json (`pnpm models-schema`). */
export const NOTIFICATIONS_SCHEMA = schemas as unknown as SchemaSet;

const root = NOTIFICATIONS_SCHEMA.NotificationsSettings ?? {};
const CHANNEL_SCHEMA = NOTIFICATIONS_SCHEMA.NotificationChannelSettings ?? {};

const channelsOf = (k: SettingsKind | null) =>
  (k?.effective.channels as Record<string, JsonObject | null> | undefined) ?? {};

const routesOf = (effective: JsonObject | undefined) =>
  (effective?.routes as Record<string, unknown> | undefined) ?? {};

/** The channels of a `notifications` answer, by name. */
export const channelNames = (k: SettingsKind | null) => Object.keys(channelsOf(k)).sort();

/** The type of each channel, which decides its form's fields. */
export function channelTypes(k: SettingsKind | null): Record<string, string> {
  return Object.fromEntries(
    Object.entries(channelsOf(k)).map(([n, c]) => [n, String(c?.type ?? '')]),
  );
}

/** The route keys of a `notifications` answer, in order. */
export const routeKeys = (k: SettingsKind | null) => Object.keys(routesOf(k?.effective)).sort();

/** The group heading of a channel's fields. */
export const channelGroup = (name: string) => `Channel ${name}`;

/** The channel a group heading names, if it names one. */
export const groupChannel = (group: string) =>
  group.startsWith('Channel ') ? group.slice('Channel '.length) : null;

/** The heading of the routes' fields. */
export const ROUTES_GROUP = 'Routes';

/** The path of a route's field. */
export const routePath = (key: string) => pathString(['routes', key]);

/**
 * The form of the `notifications` kind, built from its schema: the general members, a
 * group for each channel with the members of its type, a list of channels for each
 * route, and the delivery's retries. A channel's type is fixed once it is added, so it
 * is not a field.
 */
export function notificationFields(k: SettingsKind | null): FieldDef[] {
  const out: FieldDef[] = [];
  const props = root.properties ?? {};
  const general = { set: NOTIFICATIONS_SCHEMA, group: 'General' };
  const top: JsonSchema = {
    type: 'object',
    properties: Object.fromEntries(
      ['enabled', 'server', 'baseUrl', 'repeatEvery']
        .filter((m) => props[m])
        .map((m) => [m, props[m]]),
    ),
    'x-sparkles-order': ['enabled', 'server', 'baseUrl', 'repeatEvery'],
  };
  out.push(...fieldsOf(top, [], general));
  const types = channelTypes(k);
  for (const name of channelNames(k)) {
    const ctx = {
      set: NOTIFICATIONS_SCHEMA,
      kind: types[name] || undefined,
      group: channelGroup(name),
    };
    for (const d of fieldsOf(CHANNEL_SCHEMA, ['channels', name], ctx)) {
      if (d.path === pathString(['channels', name, 'type'])) continue;
      if (d.type === 'list') {
        d.emptyText = 'None.';
        d.keyLabel = d.path.endsWith('.to') ? 'recipient' : 'tag';
      }
      out.push(d);
    }
  }
  const route = deref(
    NOTIFICATIONS_SCHEMA,
    (props.routes?.additionalProperties as JsonSchema | undefined) ?? {},
  );
  for (const key of routeKeys(k))
    out.push({
      path: routePath(key),
      label: key,
      type: 'list',
      help: route.description,
      group: ROUTES_GROUP,
      emptyText: 'No channels, so this route sends nothing.',
      keyLabel: 'channel',
    });
  if (props.delivery)
    out.push(
      ...fieldsOf(props.delivery, ['delivery'], { set: NOTIFICATIONS_SCHEMA, group: 'Delivery' }),
    );
  return out;
}

/**
 * The checks of the routes that need the rest of the configuration, as the server makes
 * them: each channel a route names is defined. `effective` is the configuration the
 * pending change gives. The answer is keyed by the dotted path of the route.
 */
export function routeErrors(
  effective: JsonObject | undefined,
  form: FormValues,
): Record<string, string> {
  const out: Record<string, string> = {};
  const channels = (effective?.channels as JsonObject | undefined) ?? {};
  for (const [path, v] of Object.entries(form)) {
    const p = parsePath(path);
    if (p.length !== 2 || p[0] !== 'routes' || !Array.isArray(v)) continue;
    const unknown = v.filter((c) => typeof c === 'string' && c && !(c in channels));
    if (unknown.length)
      out[path] = `No channel named ${unknown.join(', ')}. Add the channel first.`;
  }
  return out;
}

/** The members of a channel type of which a channel has exactly one, by the field of each. */
const ALTERNATIVES: Record<string, [string, string][]> = {
  webhook: [
    ['url', 'url'],
    ['urlSecret', pathString(['urlSecret', 'secret'])],
  ],
};

/**
 * The whole configuration to `PUT` when the form switches a channel from one alternative
 * to the other, such as a webhook from a URL the settings file declares to a URL secret.
 * A merge patch cannot remove the declared member, since emptying its field only drops
 * the runtime value. `effective` is the configuration the pending change gives, in which
 * both alternatives are then present, and the member whose field the form emptied goes.
 * Null when no channel needs this, so the form sends its merge patch.
 */
export function alternativesBody(effective: JsonObject, form: FormValues): JsonObject | null {
  const channels = (effective.channels as Record<string, JsonObject | null> | undefined) ?? {};
  let out: JsonObject | null = null;
  for (const [name, c] of Object.entries(channels)) {
    const alts = ALTERNATIVES[String(c?.type ?? '')];
    if (!c || !alts) continue;
    const present = alts.filter(([m]) => c[m] !== undefined);
    if (present.length < 2) continue;
    const emptied = present.filter(([, f]) => {
      const v = form[pathString(['channels', name, ...parsePath(f)])];
      return v === undefined || String(v).trim() === '';
    });
    if (emptied.length !== 1) continue;
    out ??= JSON.parse(JSON.stringify(effective)) as JsonObject;
    delete ((out.channels as JsonObject)[name] as JsonObject)[emptied[0][0]];
  }
  return out;
}

/** Each value once, in the order given, for keyed lists of values that may repeat. */
export const distinct = <T>(xs: readonly T[]) => [...new Set(xs)];

/** Declared channels removed at runtime: overrides whose runtime value is `null`. */
export function removedChannels(k: SettingsKind | null): { name: string; path: string }[] {
  return (k?.overrides ?? [])
    .filter((o) => o.runtime === null)
    .map((o) => ({ path: o.path, p: parsePath(o.path) }))
    .filter(({ p }) => p.length === 2 && p[0] === 'channels')
    .map(({ path, p }) => ({ name: p[1], path }));
}

/** Whether the declared configuration defines a route, which then cannot be removed. */
export const routeDeclared = (k: SettingsKind | null, key: string) =>
  getAt(k?.declared, ['routes', key]) !== undefined;

/** The routes that send to a channel. */
export const routesTo = (routes: Record<string, unknown>, channel: string) =>
  Object.entries(routes)
    .filter(([, v]) => Array.isArray(v) && v.includes(channel))
    .map(([key]) => key)
    .sort();

/** The rule of the server for channel and secret names. */
export const NAME = /^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$/;

/** The rule of the server for route keys. */
export const ROUTE_KEY = /^(\*|[a-z0-9-]+(\.[a-z0-9-]+)*(\.\*)?)$/;

/** A mail address that SMTP carries without SMTPUTF8, as the server checks it. */
const MAIL = /^(?:(?![<>,"();:\\[\]@])[\x21-\x7e]){1,64}@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*$/;

/** The Add channel form. Only the members of the chosen type are read. */
export type NewChannel = {
  name: string;
  type: string;
  /** webhook */
  url: string;
  /** ntfy, empty for ntfy.sh */
  server: string;
  topic: string;
  /** email */
  host: string;
  tls: string;
  from: string;
  /** recipients separated by commas or spaces */
  to: string;
  /** the signing secret of a webhook, the token of ntfy, the password of email */
  secret: string;
  /** email: the user name the password goes with */
  username: string;
};

export const emptyChannel = (): NewChannel => ({
  name: '',
  type: 'webhook',
  url: '',
  server: '',
  topic: '',
  host: '',
  tls: 'starttls',
  from: '',
  to: '',
  secret: '',
  username: '',
});

/** What the Add form calls the secret of a channel type. */
export const secretLabel = (type: string) =>
  type === 'webhook' ? 'Signing secret' : type === 'ntfy' ? 'Token secret' : 'Password secret';

function httpUrl(s: string, what: string): string | null {
  let u: URL;
  try {
    u = new URL(s);
  } catch {
    return `Enter a valid ${what}.`;
  }
  if (u.protocol !== 'http:' && u.protocol !== 'https:')
    return `The ${what} must be an http or https URL.`;
  if (u.username || u.password || u.search || u.hash)
    return `The ${what} must not carry a user name, a password, a query or a fragment. Name such a URL with a URL secret in the configuration below.`;
  return null;
}

/** The merge patch that adds a channel from the Add form, or why it cannot. */
export function newChannelPatch(
  existing: readonly string[],
  input: NewChannel,
): { patch: JsonObject } | { error: string } {
  const name = input.name.trim();
  const secret = input.secret.trim();
  if (!name) return { error: 'Enter a channel name.' };
  if (!NAME.test(name))
    return { error: 'Channel names can contain only letters, digits, _, - and .' };
  if (existing.includes(name)) return { error: `A channel named ${name} already exists.` };
  if (existing.length >= MAX_CHANNELS)
    return { error: `The server holds at most ${MAX_CHANNELS} channels.` };
  if (!(CHANNEL_TYPES as readonly string[]).includes(input.type))
    return { error: 'Choose a channel type.' };
  if (secret && !NAME.test(secret))
    return {
      error: 'Secret names have 1 to 128 letters, digits, _, - or ., and cannot start with . or -.',
    };
  const ref = secret ? { secret } : undefined;
  let channel: JsonObject;
  if (input.type === 'webhook') {
    const url = input.url.trim();
    const bad = httpUrl(url, 'URL');
    if (bad) return { error: bad };
    channel = { type: 'webhook', url, ...(ref ? { signingSecret: ref } : {}) };
  } else if (input.type === 'ntfy') {
    const server = input.server.trim();
    if (server) {
      const bad = httpUrl(server, 'ntfy server');
      if (bad) return { error: bad };
    }
    const topic = input.topic.trim();
    if (!/^[A-Za-z0-9_-]{1,64}$/.test(topic))
      return { error: 'A topic has 1 to 64 letters, digits, - and _.' };
    channel = {
      type: 'ntfy',
      topic,
      ...(server ? { server } : {}),
      ...(ref ? { token: ref } : {}),
    };
  } else {
    const host = input.host.trim();
    if (!host || !/^[A-Za-z0-9.:-]{1,253}$/.test(host))
      return { error: 'Enter the SMTP host name or IP address.' };
    if (input.tls !== 'starttls' && input.tls !== 'tls')
      return { error: 'Choose STARTTLS or TLS.' };
    const from = input.from.trim();
    if (!MAIL.test(from))
      return { error: 'Enter the sender address, such as sparkles@example.org.' };
    const to = input.to.split(/[\s,]+/).filter(Boolean);
    if (to.length === 0 || to.length > 20) return { error: 'Enter 1 to 20 recipient addresses.' };
    const bad = to.find((t) => !MAIL.test(t));
    if (bad) return { error: `${bad} is not a mail address such as ops@example.org.` };
    const username = input.username.trim();
    if (!!username !== !!ref)
      return { error: 'A user name and a password secret go together. Set both or neither.' };
    channel = {
      type: 'email',
      host,
      tls: input.tls,
      from,
      to,
      ...(username ? { username, password: ref } : {}),
    };
  }
  return { patch: { channels: { [name]: channel } } };
}

/**
 * The merge patch that removes a channel, and takes it out of every route that names it,
 * since the server refuses a route to a missing channel. A runtime route left with no
 * channels goes, and a declared one keeps an empty list.
 */
export function removeChannelPatch(k: SettingsKind | null, name: string): JsonObject {
  const routes: JsonObject = {};
  for (const [key, v] of Object.entries(routesOf(k?.effective))) {
    if (!Array.isArray(v) || !v.includes(name)) continue;
    const rest = v.filter((c) => c !== name);
    routes[key] = rest.length || routeDeclared(k, key) ? rest : null;
  }
  return Object.keys(routes).length
    ? { channels: { [name]: null }, routes }
    : { channels: { [name]: null } };
}

/** The merge patch that adds a route from the Add form, or why it cannot. */
export function newRoutePatch(
  k: SettingsKind | null,
  key: string,
  channels: readonly string[],
): { patch: JsonObject } | { error: string } {
  const at = key.trim();
  if (!ROUTE_KEY.test(at))
    return {
      error: 'Enter an event type such as backup.failed, a prefix such as backup.* or *.',
    };
  if (at in routesOf(k?.effective))
    return { error: `A route for ${at} exists. Change its channels below.` };
  if (!channels.length) return { error: 'Choose at least one channel.' };
  const known = channelNames(k);
  const unknown = channels.filter((c) => !known.includes(c));
  if (unknown.length) return { error: `No channel named ${unknown.join(', ')}.` };
  return { patch: { routes: { [at]: [...channels] } } };
}

/** The merge patch that removes a route added at runtime. */
export const removeRoutePatch = (key: string): JsonObject => ({ routes: { [key]: null } });

/** The event types of spec C21 §3.1, which the Add route form suggests. */
export const EVENT_TYPES = [
  'backup.*',
  'backup.failed',
  'backup.verify.failed',
  'backup.gc.failed',
  'backup.overdue',
  'memory.review.pending',
  'models.budget.exceeded',
  '*',
] as const;
