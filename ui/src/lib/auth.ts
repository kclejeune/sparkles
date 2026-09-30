// Authentication in the UI: pure helpers (permissions, return paths, token forms).
// The UI holds no secrets: it authenticates with the server's HttpOnly session cookie
// (or a forward-auth proxy), and keeps only the non-secret CSRF token in memory.

export type Level = 'read' | 'write' | 'admin';
export type ServerPerm = 'metrics' | 'federate' | 'server-admin';
export type AuthMethod = 'oidc' | 'token' | 'password' | 'proxy';

/** `GET /$/auth/config`: the login methods of the server. */
export type AuthConfig = {
  enabled: boolean;
  methods?: AuthMethod[];
  oidc?: { loginUrl: string; displayName: string };
  cli?: {
    authorizeUrl: string;
    deviceAuthorizationEndpoint: string;
    tokenEndpoint: string;
    deviceVerificationUri: string;
  };
};

/** `GET /$/whoami`: the caller and what it may do. */
export type Whoami = {
  authEnabled: boolean;
  principal: {
    kind: 'local' | 'anonymous' | 'user' | 'token' | 'oidc' | 'proxy';
    name?: string;
    displayName?: string;
    groups?: string[];
    /** Tokens: the identity they act for, e.g. `oidc:alice@example.org`. */
    owner?: string;
  };
  method: 'none' | 'basic' | 'bearer' | 'session' | 'proxy';
  expires?: string;
  /** Present for session and proxy principals; sent as `X-Sparkles-CSRF`. */
  csrfToken?: string;
  tokenId?: string;
  server: ServerPerm[];
  /** Existing datasets the caller can see, with its level on each. */
  datasets: Record<string, Level>;
  canMintTokens: boolean;
  logout: boolean;
  tokensPolicy?: { defaultTtlSeconds: number; maxTtlSeconds: number };
};

export const LEVELS: Level[] = ['read', 'write', 'admin'];
export const SERVER_PERMS: ServerPerm[] = ['metrics', 'federate', 'server-admin'];

/** The header that carries the CSRF token of ambient (cookie or proxy) principals. */
export const CSRF_HEADER = 'X-Sparkles-CSRF';

export function levelAtLeast(have: Level | undefined | null, need: Level): boolean {
  return have != null && LEVELS.indexOf(have) >= LEVELS.indexOf(need);
}

/** The lower of two levels (a token scope never exceeds its owner). */
export function minLevel(a: Level, b: Level): Level {
  return LEVELS[Math.min(LEVELS.indexOf(a), LEVELS.indexOf(b))];
}

/** Whether the caller has `need` on dataset `ds`. Without auth everything is allowed. */
export function can(who: Whoami | null | undefined, ds: string | null | undefined, need: Level) {
  if (!who || !who.authEnabled) return true;
  if (!ds) return false;
  if (who.server.includes('server-admin')) return true;
  return levelAtLeast(who.datasets[ds], need);
}

/** Whether the caller holds a server permission (`server-admin` implies all). */
export function hasServer(who: Whoami | null | undefined, perm: ServerPerm) {
  if (!who || !who.authEnabled) return true;
  return who.server.includes('server-admin') || who.server.includes(perm);
}

/** A `/ui/…` path to return to after signing in; anything else becomes `/ui/`. */
export function safeReturnTo(r: string | null | undefined): string {
  if (!r || !r.startsWith('/ui/') || r.startsWith('//') || /[\\\r\n]/.test(r) || r.includes('/..'))
    return '/ui/';
  return r;
}

/** The login page, asked to come back to `returnTo` afterwards. */
export function loginHref(returnTo: string): string {
  const back = safeReturnTo(returnTo);
  return `/ui/login?return_to=${encodeURIComponent(back)}`;
}

/** Whether a request with this method needs the CSRF header. */
export function needsCsrf(method: string | undefined): boolean {
  const m = (method ?? 'GET').toUpperCase();
  return m !== 'GET' && m !== 'HEAD' && m !== 'OPTIONS';
}

/** A signed-in person (not anonymous, not the open local mode). */
export function signedIn(who: Whoami | null | undefined): boolean {
  return !!who && who.authEnabled && who.principal.kind !== 'anonymous';
}

/** A browser principal that may approve CLI logins. */
export function interactive(who: Whoami | null | undefined): boolean {
  return !!who && (who.method === 'session' || who.method === 'proxy');
}

/** The name shown for the caller. */
export function displayName(who: Whoami | null | undefined): string {
  if (!who) return '';
  const p = who.principal;
  if (p.kind === 'token' && p.owner) return p.owner.replace(/^[a-z]+:/, '');
  return p.displayName ?? p.name ?? p.kind;
}

/** The badge of how the caller signed in. */
export function methodBadge(who: Whoami | null | undefined): string | null {
  if (!who || !who.authEnabled) return null;
  switch (who.principal.kind) {
    case 'oidc':
      return 'SSO';
    case 'proxy':
      return 'proxy';
    case 'token':
      return 'token';
    case 'user':
      return 'password';
    default:
      return null;
  }
}

/** The message for a `/ui/login?error=` code. */
export function loginErrorMessage(code: string | null | undefined): string | null {
  switch (code) {
    case null:
    case undefined:
    case '':
      return null;
    case 'state':
      return 'The sign-in expired or was started in another tab. Please try again.';
    case 'idp':
      return 'The identity provider refused the sign-in.';
    case 'idp_unavailable':
      return 'The identity provider cannot be reached right now. Try again in a moment.';
    case 'not_allowed':
      return 'Your account is not allowed to use this server.';
    default:
      return 'Sign-in failed.';
  }
}

const DAY = 86_400;

/** Token lifetimes offered: 7, 30 and 90 days, capped at the server's maximum. */
export function ttlOptions(maxTtlSeconds?: number): { label: string; value: string }[] {
  const max = maxTtlSeconds ?? 90 * DAY;
  const out = [7, 30, 90]
    .filter((d) => d * DAY <= max)
    .map((d) => ({ label: `${d} days`, value: `${d}d` }));
  if (out.length === 0) {
    const d = Math.max(1, Math.floor(max / DAY));
    out.push({ label: `${d} day${d === 1 ? '' : 's'}`, value: `${Math.floor(max / 60)}m` });
  }
  return out;
}

/** A token scope request: `{datasets, server}`. */
export type Scope = { datasets: Record<string, Level>; server: string[] };

/** "All my access": every dataset at the owner's level, every server permission. */
export const ALL_ACCESS: Scope = { datasets: { '*': 'admin' }, server: ['*'] };

/**
 * The scope of a token form: the chosen level per dataset (empty choices dropped, each
 * capped at the user's own level) and the chosen server permissions the user holds.
 */
export function scopeFrom(
  who: Whoami | null | undefined,
  levels: Record<string, Level | ''>,
  perms: ServerPerm[],
): Scope {
  const datasets: Record<string, Level> = {};
  for (const [ds, l] of Object.entries(levels)) {
    if (!l) continue;
    const mine = who?.server.includes('server-admin') ? 'admin' : who?.datasets[ds];
    if (!mine) continue;
    datasets[ds] = minLevel(l, mine);
  }
  return { datasets, server: perms.filter((p) => hasServer(who, p)) };
}

/** Human summary of a scope: `wiki=read, *=admin; metrics`. */
export function scopeSummary(s: { datasets?: Record<string, string>; server?: string[] }) {
  const ds = Object.entries(s.datasets ?? {}).map(([k, v]) => `${k}=${v}`);
  const parts = [ds.length ? ds.join(', ') : 'no datasets'];
  if (s.server?.length) parts.push(s.server.map((x) => (x === '*' ? 'all server' : x)).join(', '));
  return parts.join('; ');
}

/** When a token expires: `in 7 days`, `in 3 hours`, `expired`, or `never`. */
export function fmtExpires(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return 'never';
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return iso;
  const s = Math.round((t - now) / 1000);
  if (s <= 0) return 'expired';
  if (s < 3600) return `in ${Math.max(1, Math.round(s / 60))} min`;
  if (s < 2 * DAY) return `in ${Math.round(s / 3600)} hours`;
  return `in ${Math.round(s / DAY)} days`;
}

/** `abcd-efgh` → `ABCD-EFGH` (user codes are case- and dash-insensitive). */
export function normalizeUserCode(c: string): string {
  const s = c.toUpperCase().replace(/[^A-Z0-9]/g, '');
  return s.length === 8 ? `${s.slice(0, 4)}-${s.slice(4)}` : s;
}

/** Parameters of a CLI loopback authorization (`/ui/cli/authorize?…`), validated. */
export type CliAuthorize = {
  port: number;
  state: string;
  codeChallenge: string;
  label: string;
  hostname: string;
};

export function parseCliAuthorize(p: URLSearchParams): CliAuthorize | null {
  const port = Number(p.get('port'));
  const state = p.get('state') ?? '';
  const codeChallenge = p.get('code_challenge') ?? '';
  const method = p.get('code_challenge_method') ?? 'S256';
  if (!Number.isInteger(port) || port < 1024 || port > 65535) return null;
  if (!state || state.length > 256) return null;
  if (method !== 'S256' || !/^[A-Za-z0-9_-]{43}$/.test(codeChallenge)) return null;
  return {
    port,
    state,
    codeChallenge,
    label: (p.get('label') ?? 'sparkles CLI').slice(0, 80),
    hostname: (p.get('hostname') ?? '').slice(0, 80),
  };
}

/** Where a denied loopback login sends the browser (the CLI's own listener). */
export function deniedCallback(a: CliAuthorize): string {
  return `http://127.0.0.1:${a.port}/callback?error=access_denied&state=${encodeURIComponent(a.state)}`;
}
