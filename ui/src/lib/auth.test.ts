import { describe, expect, it } from 'vitest';
import {
  can,
  deniedCallback,
  displayName,
  fmtExpires,
  hasServer,
  interactive,
  loginErrorMessage,
  loginHref,
  methodBadge,
  minLevel,
  needsCsrf,
  normalizeUserCode,
  parseCliAuthorize,
  safeReturnTo,
  scopeFrom,
  scopeSummary,
  signedIn,
  ttlOptions,
  type Whoami,
} from './auth';

const bob: Whoami = {
  authEnabled: true,
  principal: { kind: 'user', name: 'bob' },
  method: 'session',
  csrfToken: 'c',
  server: [],
  datasets: { wiki: 'write', 'team-a': 'read' },
  canMintTokens: true,
  logout: true,
};

const open: Whoami = {
  authEnabled: false,
  principal: { kind: 'local' },
  method: 'none',
  server: ['server-admin'],
  datasets: {},
  canMintTokens: false,
  logout: false,
};

describe('permissions', () => {
  it('checks dataset levels', () => {
    expect(can(bob, 'wiki', 'read')).toBe(true);
    expect(can(bob, 'wiki', 'write')).toBe(true);
    expect(can(bob, 'wiki', 'admin')).toBe(false);
    expect(can(bob, 'team-a', 'write')).toBe(false);
    expect(can(bob, 'secret', 'read')).toBe(false);
    expect(can(bob, null, 'read')).toBe(false);
  });
  it('allows everything without auth', () => {
    expect(can(open, 'x', 'admin')).toBe(true);
    expect(can(null, 'x', 'admin')).toBe(true);
    expect(hasServer(open, 'metrics')).toBe(true);
  });
  it('treats server-admin as everything', () => {
    const alice = { ...bob, server: ['server-admin' as const], datasets: {} };
    expect(can(alice, 'anything', 'admin')).toBe(true);
    expect(hasServer(alice, 'metrics')).toBe(true);
    expect(hasServer(bob, 'metrics')).toBe(false);
  });
  it('orders levels', () => {
    expect(minLevel('admin', 'read')).toBe('read');
    expect(minLevel('write', 'admin')).toBe('write');
  });
});

describe('return paths and login links', () => {
  it('keeps /ui/ paths only', () => {
    expect(safeReturnTo('/ui/datasets')).toBe('/ui/datasets');
    expect(safeReturnTo('https://evil.example')).toBe('/ui/');
    expect(safeReturnTo('//evil.example/ui/')).toBe('/ui/');
    expect(safeReturnTo('/ui/../$/metrics')).toBe('/ui/');
    expect(safeReturnTo(null)).toBe('/ui/');
  });
  it('builds login links', () => {
    expect(loginHref('/ui/datasets')).toBe('/ui/login?return_to=%2Fui%2Fdatasets');
    expect(loginHref('https://x')).toBe('/ui/login?return_to=%2Fui%2F');
  });
  it('explains login errors', () => {
    expect(loginErrorMessage('not_allowed')).toMatch(/not allowed/);
    expect(loginErrorMessage(null)).toBeNull();
    expect(loginErrorMessage('weird')).toBe('Sign-in failed.');
  });
});

describe('principals', () => {
  it('needs CSRF on unsafe methods', () => {
    expect(needsCsrf('POST')).toBe(true);
    expect(needsCsrf('delete')).toBe(true);
    expect(needsCsrf('GET')).toBe(false);
    expect(needsCsrf(undefined)).toBe(false);
  });
  it('describes the caller', () => {
    expect(displayName(bob)).toBe('bob');
    expect(methodBadge(bob)).toBe('password');
    const sso = { ...bob, principal: { kind: 'oidc' as const, name: 'a@x', displayName: 'Alice' } };
    expect(displayName(sso)).toBe('Alice');
    expect(methodBadge(sso)).toBe('SSO');
    const tok = { ...bob, principal: { kind: 'token' as const, name: 'tok_x', owner: 'oidc:a@x' } };
    expect(displayName(tok)).toBe('a@x');
    expect(methodBadge(open)).toBeNull();
    expect(signedIn(bob)).toBe(true);
    expect(signedIn(open)).toBe(false);
    expect(signedIn({ ...bob, principal: { kind: 'anonymous' } })).toBe(false);
    expect(interactive(bob)).toBe(true);
    expect(interactive({ ...bob, method: 'bearer' })).toBe(false);
  });
});

describe('token forms', () => {
  it('offers lifetimes up to the maximum', () => {
    expect(ttlOptions(90 * 86400).map((o) => o.value)).toEqual(['7d', '30d', '90d']);
    expect(ttlOptions(30 * 86400).map((o) => o.value)).toEqual(['7d', '30d']);
    expect(ttlOptions(86400).length).toBe(1);
  });
  it('caps scopes at the user', () => {
    const s = scopeFrom(bob, { wiki: 'admin', 'team-a': 'read', secret: 'read', x: '' }, [
      'metrics',
    ]);
    expect(s).toEqual({ datasets: { wiki: 'write', 'team-a': 'read' }, server: [] });
    expect(scopeSummary({ datasets: { wiki: 'read' }, server: ['*'] })).toBe(
      'wiki=read; all server',
    );
    expect(scopeSummary({})).toBe('no datasets');
  });
  it('says when tokens expire', () => {
    const now = Date.parse('2026-01-01T00:00:00Z');
    expect(fmtExpires('2026-01-08T00:00:00Z', now)).toBe('in 7 days');
    expect(fmtExpires('2026-01-01T05:00:00Z', now)).toBe('in 5 hours');
    expect(fmtExpires('2025-12-31T00:00:00Z', now)).toBe('expired');
    expect(fmtExpires(null, now)).toBe('never');
  });
  it('normalizes user codes', () => {
    expect(normalizeUserCode('wdjb-mjht')).toBe('WDJB-MJHT');
    expect(normalizeUserCode('WDJBMJHT')).toBe('WDJB-MJHT');
    expect(normalizeUserCode('abc')).toBe('ABC');
  });
});

describe('CLI loopback parameters', () => {
  const ok = 'port=50123&state=s1&code_challenge=' + 'a'.repeat(43) + '&hostname=h';
  it('validates', () => {
    const a = parseCliAuthorize(new URLSearchParams(ok))!;
    expect(a.port).toBe(50123);
    expect(a.label).toBe('sparkles CLI');
    expect(deniedCallback(a)).toBe('http://127.0.0.1:50123/callback?error=access_denied&state=s1');
    expect(parseCliAuthorize(new URLSearchParams(ok.replace('50123', '80')))).toBeNull();
    expect(parseCliAuthorize(new URLSearchParams(ok.replace('a'.repeat(43), 'short')))).toBeNull();
    expect(parseCliAuthorize(new URLSearchParams(ok + '&code_challenge_method=plain'))).toBeNull();
    expect(parseCliAuthorize(new URLSearchParams(ok.replace('state=s1', 'state=')))).toBeNull();
  });
});
