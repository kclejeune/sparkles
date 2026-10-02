// Background requests: the UI polls only while the page is visible (lib/poll.ts), and an
// expired session sends the user to sign in once instead of bouncing between pages.

import { expect, test, type Page } from '@playwright/test';

/** Lets the test hide and show the page (`document.visibilityState`). */
async function controlVisibility(page: Page) {
  await page.addInitScript(() => {
    let hidden = false;
    Object.defineProperty(Document.prototype, 'visibilityState', {
      configurable: true,
      get: () => (hidden ? 'hidden' : 'visible'),
    });
    Object.defineProperty(Document.prototype, 'hidden', {
      configurable: true,
      get: () => hidden,
    });
    (window as unknown as { setHidden: (h: boolean) => void }).setHidden = (h) => {
      hidden = h;
      document.dispatchEvent(new Event('visibilitychange'));
    };
  });
}

const setHidden = (page: Page, hidden: boolean) =>
  page.evaluate(
    (h) => (window as unknown as { setHidden: (h: boolean) => void }).setHidden(h),
    hidden,
  );

/** Counts the API requests (`/$/…`) the page makes, by method and path. */
function countRequests(page: Page) {
  const counts = new Map<string, number>();
  page.on('request', (r) => {
    const path = new URL(r.url()).pathname;
    if (!path.startsWith('/$/')) return;
    const key = `${r.method()} ${path}`;
    counts.set(key, (counts.get(key) ?? 0) + 1);
  });
  return {
    total: () => [...counts.values()].reduce((a, b) => a + b, 0),
    of: (key: string) => counts.get(key) ?? 0,
    reset: () => counts.clear(),
  };
}

test('a hidden page makes no requests, and refreshes once it is shown again', async ({ page }) => {
  await page.clock.install();
  await controlVisibility(page);
  const requests = countRequests(page);
  await page.goto('/ui/server');
  await expect(page.getByRole('heading', { name: 'Tasks' })).toBeVisible();
  await page.clock.runFor(2000);

  await setHidden(page, true);
  requests.reset();
  await page.clock.runFor(10 * 60_000);
  expect(requests.total()).toBe(0);

  await setHidden(page, false);
  await expect.poll(() => requests.of('GET /$/ready')).toBe(1);
  await expect.poll(() => requests.of('GET /$/tasks')).toBe(1);
  await expect.poll(() => requests.of('GET /$/ping')).toBe(1);
});

test('an idle Backups page does not poll repositories or policies', async ({ page }) => {
  await page.clock.install();
  const requests = countRequests(page);
  await page.goto('/ui/backups');
  await expect(page.getByRole('tab', { name: /Backups/ })).toBeVisible();
  await page.clock.runFor(2000);
  requests.reset();
  await page.clock.runFor(10 * 60_000);
  expect(requests.of('GET /$/repositories')).toBe(0);
  expect(requests.of('GET /$/backup-policies')).toBe(0);
  expect(requests.of('GET /$/server')).toBe(0);
  // the shared task poller backs off to once a minute while no task runs
  expect(requests.of('GET /$/tasks')).toBeLessThanOrEqual(11);
});

test('an expired session goes to sign-in once, without bouncing back', async ({ page }) => {
  // The session ends right after the page loaded the caller: the UI still holds a signed-in
  // server admin, while the server answers like for an anonymous caller.
  const admin = {
    authEnabled: true,
    principal: { kind: 'oidc', name: 'e2e' },
    method: 'session',
    csrfToken: 'csrf',
    server: ['server-admin'],
    datasets: { foaf: 'admin' },
    canMintTokens: false,
    logout: true,
  };
  const anonymous = {
    authEnabled: true,
    principal: { kind: 'anonymous' },
    method: 'none',
    server: [],
    datasets: { foaf: 'read' },
    canMintTokens: false,
    logout: false,
  };
  let whoamis = 0;
  await page.route('**/$/whoami', (r) => r.fulfill({ json: whoamis++ === 0 ? admin : anonymous }));
  await page.route('**/$/auth/config', (r) =>
    r.fulfill({ json: { enabled: true, methods: ['token'] } }),
  );
  await page.route('**/$/backup-policies', (r) =>
    r.fulfill({ status: 401, json: { error: 'unauthenticated', message: 'Sign in' } }),
  );
  const requests = countRequests(page);

  await page.goto('/ui/backups');
  await expect(page).toHaveURL(/\/ui\/login\?return_to=/);
  await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();
  await page.waitForTimeout(1500);
  await expect(page).toHaveURL(/\/ui\/login\?return_to=/);
  // one refused request, one reload of the caller, and no trip back to the Backups page
  expect(requests.of('GET /$/backup-policies')).toBe(1);
  expect(requests.of('GET /$/whoami')).toBe(2);
});
