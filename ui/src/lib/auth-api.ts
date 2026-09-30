// Typed client for the `/$/whoami` and `/$/auth/*` routes (see docs/API.md).

import { json, request } from './api';
import type { AuthConfig, Level, Whoami } from './auth';

const enc = encodeURIComponent;

const jsonInit = (method: string, body: unknown): RequestInit => ({
  method,
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify(body),
});

export const authConfig = () => json<AuthConfig>('/$/auth/config', { cache: 'no-store' });

export const whoami = () => json<Whoami>('/$/whoami', { cache: 'no-store' });

/** Password or token sign-in: the server answers with a session cookie. */
export async function login(body: { user: string; password: string } | { token: string }) {
  await request('/$/auth/login', jsonInit('POST', body));
}

/** Sign out; `redirect` is the identity provider's or proxy's logout page, if any. */
export async function logout(): Promise<{ redirect: string | null }> {
  return (
    (await json<{ redirect: string | null }>('/$/auth/logout', { method: 'POST' })) ?? {
      redirect: null,
    }
  );
}

export type TokenInfo = {
  id: string;
  name: string;
  scope?: { datasets: Record<string, Level>; server: string[] };
  created?: string;
  expires?: string | null;
  lastUsed?: string | null;
  via?: 'api' | 'ui' | 'cli-loopback' | 'cli-device';
  client?: { label?: string; hostname?: string };
  owner?: string;
  parent?: string;
  /** Static tokens of the configuration file (`?all=true`). */
  static?: boolean;
  grants?: string;
};

export async function listTokens(all = false): Promise<TokenInfo[]> {
  const r = await json<{ tokens: TokenInfo[] }>(`/$/auth/tokens${all ? '?all=true' : ''}`, {
    cache: 'no-store',
  });
  return r?.tokens ?? [];
}

export type MintRequest = {
  name: string;
  datasets: Record<string, Level>;
  server: string[];
  expiresIn: string;
};

/** Mint a token; the response is the only time the token is shown. */
export const mintToken = (m: MintRequest) =>
  json<TokenInfo & { token: string }>('/$/auth/tokens', jsonInit('POST', { ...m, via: 'ui' }));

export async function revokeToken(id: string) {
  await request(`/$/auth/tokens/${enc(id)}`, { method: 'DELETE' });
}

export type DeviceGrant = {
  userCode: string;
  label: string;
  hostname: string;
  expiresIn: number;
  status: 'pending' | 'approved' | 'denied';
};

export const deviceGrant = (code: string) =>
  json<DeviceGrant>(`/$/auth/device/${enc(code)}`, { cache: 'no-store' });

export const approveDevice = (code: string, m: Partial<MintRequest>) =>
  json<{ approved: boolean }>(`/$/auth/device/${enc(code)}/approve`, jsonInit('POST', m));

export const denyDevice = (code: string) =>
  json<{ denied: boolean }>(`/$/auth/device/${enc(code)}/deny`, { method: 'POST' });

/** Approve a CLI waiting on 127.0.0.1: the answer is where to send the browser. */
export const authorizeCli = (body: {
  port: number;
  state: string;
  codeChallenge: string;
  label: string;
  hostname: string;
  name: string;
  datasets: Record<string, Level>;
  server: string[];
  expiresIn: string;
}) => json<{ redirect: string }>('/$/auth/cli/authorize', jsonInit('POST', body));
