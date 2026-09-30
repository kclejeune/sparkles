// `test` runs signed in as the local user (the session made by global-setup.ts);
// `anonymous` starts without cookies. Both point at the server that global-setup started.

import { test as base } from '@playwright/test';

export { expect } from '@playwright/test';

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
