// The pages against the real server at a phone's width (320 CSS pixels): the page never
// scrolls sideways, only the wide content inside it does (tests/mock/phone.spec.ts covers
// every dialog and the pages the mock alone has data for).

import { anonymous, expect, test } from './fixtures';
import { DATASET, EX } from './data';
import { expectNoSidewaysScroll, phone, selectAll } from '../phone';

test.use(phone);
test.describe.configure({ timeout: 90_000 });

test('the query page and its result views', async ({ page }) => {
  await page.goto('/ui/query');
  await page.locator('.cm-content').click();
  await selectAll(page);
  await page.keyboard.insertText(`SELECT * WHERE { ?s ?p ?o }`);
  await page.getByRole('button', { name: /^Run\b/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('grid')).toBeVisible();
  await expectNoSidewaysScroll(page, 'the query page');
  for (const view of [/Graph/, /Plan/, /Raw/]) {
    await results.getByRole('tab', { name: view }).click();
    await expectNoSidewaysScroll(page, `the query page's ${view.source} view`);
  }
});

test('the explorer, the datasets, the server and the tokens', async ({ page }) => {
  for (const url of [
    `/ui/explore?ds=${DATASET}&iri=${encodeURIComponent(`${EX}ada`)}`,
    `/ui/explore?ds=${DATASET}&tab=schema`,
    '/ui/datasets',
    `/ui/datasets/${DATASET}`,
    '/ui/backups',
    '/ui/server',
    '/ui/tokens',
  ]) {
    await page.goto(url);
    await page.waitForLoadState('networkidle');
    await expect(page.locator('main .spinner')).toHaveCount(0, { timeout: 15_000 });
    await expectNoSidewaysScroll(page, url);
  }
  await page.goto(`/ui/datasets/${DATASET}`);
  await page.getByRole('tab', { name: 'ShEx' }).click();
  await expect(page.getByRole('textbox', { name: 'ShEx schema' })).toBeVisible();
  await expectNoSidewaysScroll(page, 'the ShEx tab');
});

anonymous('the sign-in page', async ({ page }) => {
  await page.goto('/ui/login');
  await expect(page.getByRole('button', { name: /Sign in/ }).first()).toBeVisible();
  await expectNoSidewaysScroll(page, 'the sign-in page');
});
