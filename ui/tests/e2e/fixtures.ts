// `test` runs signed in as the local user (the session made by global-setup.ts);
// `anonymous` starts without cookies. Both point at the server with auth that
// global-setup started; `open` points at the one without auth. Every test fails on a
// Content Security Policy violation of the page.

import { test as plain, expect } from '@playwright/test';

export { expect };

/** Fails the test if the page reports a Content Security Policy violation. */
const base = plain.extend<{ cspViolations: string[] }>({
  cspViolations: [
    async ({ page }, use) => {
      const seen: string[] = [];
      page.on('console', (m) => {
        if (/Content Security Policy/i.test(m.text())) seen.push(m.text());
      });
      await use(seen);
      expect(seen, 'Content Security Policy violations').toEqual([]);
    },
    { auto: true },
  ],
});

function env(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`${name} is not set: run the tests with \`pnpm e2e\``);
  return v;
}

/** The server's base URL and the static API token (server-admin). */
export const server = {
  get url() {
    return env('SPARKLES_E2E_URL');
  },
  get token() {
    return env('SPARKLES_E2E_TOKEN');
  },
};

export const anonymous = base.extend({
  baseURL: async ({}, use) => use(server.url),
});

export const test = anonymous.extend({
  storageState: async ({}, use) => use(env('SPARKLES_E2E_STATE')),
});

/** The server without `--auth-config`: every caller is the local principal. */
export const open = base.extend({
  baseURL: async ({}, use) => use(env('SPARKLES_E2E_OPEN_URL')),
});
