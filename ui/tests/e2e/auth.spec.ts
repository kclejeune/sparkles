import { randomBytes } from 'node:crypto';
import { anonymous, expect, server, test } from './fixtures';
import { DATASET, PASSWORD, USER } from './data';

const ask = `/${DATASET}/sparql?query=${encodeURIComponent('ASK { ?s ?p ?o }')}`;

anonymous(
  'an anonymous visitor signs in with a password and returns to the page',
  async ({ page }) => {
    await page.goto('/ui/query');
    await expect(page).toHaveURL(/\/ui\/login\?return_to=%2Fui%2Fquery/);

    await page.getByLabel('User').fill(USER);
    await page.getByLabel('Password').fill('wrong password');
    await page.getByRole('button', { name: 'Sign in', exact: true }).click();
    await expect(page.getByRole('alert')).toContainText('invalid credentials');

    await page.getByLabel('Password').fill(PASSWORD);
    await page.getByRole('button', { name: 'Sign in', exact: true }).click();
    await expect(page).toHaveURL(/\/ui\/query$/);
    await expect(page.locator('.user .who')).toContainText(USER);
    await expect(page.locator('.user .who')).toContainText('password');
  },
);

anonymous('API tokens authenticate SPARQL requests', async ({ request }) => {
  expect((await request.get(ask)).status()).toBe(401);
  expect(
    (
      await request.get(ask, { headers: { Authorization: 'Bearer spk_not-a-real-token' } })
    ).status(),
  ).toBe(401);

  const r = await request.get(ask, {
    headers: { Authorization: `Bearer ${server.token}`, Accept: 'application/sparql-results+json' },
  });
  expect(r.status()).toBe(200);
  expect((await r.json()).boolean).toBe(true);
});

test('a token minted on the tokens page signs in to the UI and calls the API', async ({
  page,
  browser,
}) => {
  // tests share the server (and --repeat-each runs this one several times)
  const name = `e2e-${randomBytes(4).toString('hex')}`;
  await page.goto('/ui/tokens');
  await page.getByRole('button', { name: 'New token' }).click();
  const form = page.getByRole('dialog', { name: 'New API token' });
  await form.getByLabel('Token name').fill(name);
  await form.getByRole('button', { name: 'Create token' }).click();

  const shown = page.getByRole('dialog', { name: 'Your new token' });
  const token = (await shown.locator('code').textContent())?.trim() ?? '';
  expect(token).toMatch(/^spk_[A-Za-z0-9_-]{43}$/);
  await shown.getByRole('button', { name: 'Done' }).click();
  await expect(page.getByRole('row', { name: new RegExp(`^${name} `) })).toBeVisible();

  // the token as a Bearer credential
  const api = await page.request.get(ask, {
    headers: { Authorization: `Bearer ${token}`, Accept: 'application/sparql-results+json' },
  });
  expect(api.status()).toBe(200);

  // and on the login page of a browser context without the session (contexts made in a
  // test inherit the `use` options, the storage state among them)
  const ctx = await browser.newContext({
    baseURL: server.url,
    storageState: { cookies: [], origins: [] },
  });
  try {
    const other = await ctx.newPage();
    await other.goto('/ui/login');
    await other.getByLabel('API token').fill(token);
    await other.getByRole('button', { name: 'Sign in with token' }).click();
    await expect(other).toHaveURL(/\/ui\/$/);
    await expect(other.locator('.user .who')).toContainText(USER);
    await expect(other.locator('.user .who')).toContainText('token');
  } finally {
    await ctx.close();
  }
});
