import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  CHANNEL_TYPES,
  channelGroup,
  channelNames,
  emptyChannel,
  groupChannel,
  newChannelPatch,
  newRoutePatch,
  newestFirst,
  notificationFields,
  outcomeText,
  removeChannelPatch,
  removeRoutePatch,
  removedChannels,
  resultClass,
  routeDeclared,
  routeErrors,
  routePath,
  routesTo,
  testChannel,
  type NewChannel,
} from './notifications';
import { formOf, type SettingsKind } from './settings';

/** A `notifications` kind: phone and ops declared, mail added at runtime, old removed. */
const notifications = (over: Partial<SettingsKind> = {}): SettingsKind => ({
  scope: 'server',
  kind: 'notifications',
  effective: {
    enabled: true,
    repeatEvery: '1d',
    channels: {
      phone: { type: 'ntfy', topic: 'ops', token: { secret: 'ntfy-token' } },
      ops: { type: 'webhook', url: 'https://hooks.example.org/s' },
      mail: { type: 'email', host: 'smtp.example.org', from: 'a@b.org', to: ['c@d.org'] },
    },
    routes: { 'backup.*': ['phone', 'ops'], '*': ['ops'], 'models.budget.exceeded': ['mail'] },
    delivery: { attempts: 5, backoffSecs: 30, maxBackoffSecs: 1800, timeoutSecs: 10 },
  },
  declared: {
    channels: {
      phone: { type: 'ntfy', topic: 'ops' },
      ops: { type: 'webhook', url: 'https://hooks.example.org/s' },
      old: { type: 'webhook', url: 'https://old.example.org' },
    },
    routes: { 'backup.*': ['phone', 'ops'], '*': ['ops'] },
  },
  runtime: { channels: { old: null } },
  sources: {},
  locked: [],
  overridden: [],
  overrides: [{ path: 'channels.old', declared: {}, runtime: null }],
  status: { valid: true },
  etag: '"1"',
  ...over,
});

const channel = (over: Partial<NewChannel>): NewChannel => ({ ...emptyChannel(), ...over });

afterEach(() => vi.unstubAllGlobals());

describe('notificationFields', () => {
  const fields = notificationFields(notifications());
  const paths = fields.map((f) => f.path);

  it('gives each channel the members of its type only, without its type', () => {
    expect(paths).toContain('channels.phone.topic');
    expect(paths).toContain('channels.phone.token.secret');
    expect(paths).not.toContain('channels.phone.url');
    expect(paths).toContain('channels.ops.url');
    expect(paths).toContain('channels.ops.signingSecret.secret');
    expect(paths).not.toContain('channels.ops.topic');
    expect(paths).toContain('channels.mail.to');
    expect(paths).toContain('channels.mail.password.secret');
    expect(paths.some((p) => p.endsWith('.type'))).toBe(false);
    expect(fields.find((f) => f.path === 'channels.ops.url')?.group).toBe('Channel ops');
    expect(fields.find((f) => f.path === 'channels.ops.signingSecret.secret')?.type).toBe('secret');
    expect(fields.find((f) => f.path === 'channels.mail.tls')?.options).toEqual([
      'starttls',
      'tls',
    ]);
  });

  it('has the general members, a list per route with its dot escaped, and the delivery', () => {
    expect(paths.slice(0, 4)).toEqual(['enabled', 'server', 'baseUrl', 'repeatEvery']);
    expect(fields.find((f) => f.path === 'enabled')?.type).toBe('bool');
    const route = fields.find((f) => f.path === routePath('backup.*'));
    expect(route?.path).toBe('routes.backup\\.*');
    expect(route).toMatchObject({ type: 'list', group: 'Routes', label: 'backup.*' });
    expect(paths).toContain('delivery.attempts');
    expect(fields.find((f) => f.path === 'delivery.attempts')).toMatchObject({ min: 1, max: 10 });
  });

  it('reads the route lists into the form', () => {
    const form = formOf(fields, notifications().effective);
    expect(form[routePath('backup.*')]).toEqual(['phone', 'ops']);
  });

  it('has only the general members and the delivery without a kind', () => {
    const empty = notificationFields(null).map((f) => f.path);
    expect(empty.every((p) => !p.startsWith('channels.') && !p.startsWith('routes.'))).toBe(true);
  });
});

describe('names and routes', () => {
  it('lists the channels, the groups and the routes to a channel', () => {
    expect(channelNames(notifications())).toEqual(['mail', 'ops', 'phone']);
    expect(groupChannel(channelGroup('a.b'))).toBe('a.b');
    expect(groupChannel('Routes')).toBeNull();
    expect(routesTo(notifications().effective.routes as never, 'ops')).toEqual(['*', 'backup.*']);
    expect(removedChannels(notifications())).toEqual([{ name: 'old', path: 'channels.old' }]);
    expect(routeDeclared(notifications(), 'backup.*')).toBe(true);
    expect(routeDeclared(notifications(), 'models.budget.exceeded')).toBe(false);
  });

  it('flags a route to a channel that does not exist', () => {
    const k = notifications();
    expect(routeErrors(k.effective, { [routePath('*')]: ['ops', 'nope'], enabled: true })).toEqual({
      [routePath('*')]: 'No channel named nope. Add the channel first.',
    });
    expect(routeErrors(k.effective, { [routePath('*')]: ['ops'] })).toEqual({});
  });
});

describe('newChannelPatch', () => {
  it('adds a webhook with its signing secret', () => {
    expect(
      newChannelPatch([], channel({ name: 'hook', url: 'https://h.example/x', secret: 'sig' })),
    ).toEqual({
      patch: {
        channels: {
          hook: { type: 'webhook', url: 'https://h.example/x', signingSecret: { secret: 'sig' } },
        },
      },
    });
  });

  it('adds ntfy on the default server and email with its recipients', () => {
    expect(newChannelPatch([], channel({ name: 'n', type: 'ntfy', topic: 't_1' }))).toEqual({
      patch: { channels: { n: { type: 'ntfy', topic: 't_1' } } },
    });
    expect(
      newChannelPatch(
        [],
        channel({
          name: 'm',
          type: 'email',
          host: 'smtp.example.org',
          tls: 'tls',
          from: 's@example.org',
          to: 'a@example.org, b@example.org',
          username: 'u',
          secret: 'smtp',
        }),
      ),
    ).toEqual({
      patch: {
        channels: {
          m: {
            type: 'email',
            host: 'smtp.example.org',
            tls: 'tls',
            from: 's@example.org',
            to: ['a@example.org', 'b@example.org'],
            username: 'u',
            password: { secret: 'smtp' },
          },
        },
      },
    });
  });

  it('refuses what the server refuses', () => {
    const err = (c: Partial<NewChannel>, existing: string[] = []) => {
      const r = newChannelPatch(existing, channel({ name: 'c', ...c }));
      return 'error' in r ? r.error : null;
    };
    expect(err({ name: '' })).toMatch(/Enter a channel name/);
    expect(err({ name: '-x' })).toMatch(/letters, digits/);
    expect(err({ url: 'https://h.example' }, ['c'])).toMatch(/already exists/);
    expect(
      err(
        { url: 'https://h.example' },
        Array.from({ length: 32 }, (_, i) => `c${i}`),
      ),
    ).toMatch(/at most 32/);
    expect(err({ url: 'ftp://h.example' })).toMatch(/http or https/);
    expect(err({ url: 'https://h.example/x?token=1' })).toMatch(/URL secret/);
    expect(err({ url: 'https://u:p@h.example/' })).toMatch(/URL secret/);
    expect(err({ type: 'ntfy', topic: 'a b' })).toMatch(/topic/);
    expect(err({ type: 'email', host: 'h', from: 'nope', to: 'a@b.org' })).toMatch(/sender/);
    expect(err({ type: 'email', host: 'h', from: 'a@b.org', to: '' })).toMatch(/1 to 20/);
    expect(err({ type: 'email', host: 'h', from: 'a@b.org', to: 'x' })).toMatch(/x is not/);
    expect(
      err({ type: 'email', host: 'h', from: 'a@b.org', to: 'c@d.org', username: 'u' }),
    ).toMatch(/go together/);
    expect(err({ url: 'https://h.example', secret: '.bad' })).toMatch(/Secret names/);
    expect(err({ type: 'pager' })).toMatch(/type/);
    expect(CHANNEL_TYPES).toEqual(['webhook', 'ntfy', 'email']);
  });
});

describe('removing and adding routes', () => {
  it('takes a removed channel out of its routes', () => {
    // ops is the only channel of the declared `*`, which keeps an empty list, and the
    // runtime route to mail alone goes
    expect(removeChannelPatch(notifications(), 'ops')).toEqual({
      channels: { ops: null },
      routes: { 'backup.*': ['phone'], '*': [] },
    });
    expect(removeChannelPatch(notifications(), 'mail')).toEqual({
      channels: { mail: null },
      routes: { 'models.budget.exceeded': null },
    });
    expect(
      removeChannelPatch(notifications({ effective: { channels: { x: {} }, routes: {} } }), 'x'),
    ).toEqual({ channels: { x: null } });
  });

  it('adds a route to known channels and removes one', () => {
    const k = notifications();
    expect(newRoutePatch(k, ' backup.failed ', ['mail'])).toEqual({
      patch: { routes: { 'backup.failed': ['mail'] } },
    });
    const err = (key: string, chans: string[]) => {
      const r = newRoutePatch(k, key, chans);
      return 'error' in r ? r.error : null;
    };
    expect(err('Backup', ['mail'])).toMatch(/event type/);
    expect(err('backup.*', ['mail'])).toMatch(/exists/);
    expect(err('memory.*', [])).toMatch(/at least one/);
    expect(err('memory.*', ['nope'])).toMatch(/No channel named nope/);
    expect(removeRoutePatch('backup.*')).toEqual({ routes: { 'backup.*': null } });
  });
});

describe('deliveries', () => {
  it('orders, classes and describes outcomes', () => {
    const d = (at: string) => ({ at, id: at, event: 'e', channel: 'c', result: 'ok' as const });
    expect(newestFirst([d('1'), d('3'), d('2')]).map((x) => x.at)).toEqual(['3', '2', '1']);
    expect(resultClass('ok')).toBe('ok');
    expect(resultClass('dropped')).toBe('warn');
    expect(resultClass('refused')).toBe('danger');
    expect(outcomeText({ status: 503, error: 'busy' })).toBe('503: busy');
    expect(outcomeText({ status: 204 })).toBe('204');
    expect(outcomeText({})).toBe('');
  });

  it('posts a test to the channel, escaping its name', async () => {
    const fetch = vi.fn(
      async () =>
        new Response(JSON.stringify({ channel: 'a b', result: 'ok' }), {
          headers: { 'Content-Type': 'application/json' },
        }),
    );
    vi.stubGlobal('fetch', fetch);
    await testChannel('a b');
    const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
    expect(String(url)).toContain('/$/notifications/test/a%20b');
    expect(init.method).toBe('POST');
  });
});
