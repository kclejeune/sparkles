// The Clone dialog against the mock: a full persistent clone with hard links, and a
// partial in-memory clone of one graph. The task list, the banner and the clone's own
// page say how each copy was made.

import { expect, test, type Page } from '@playwright/test';

// the first page load compiles the UI in vite dev
test.describe.configure({ timeout: 90_000 });

async function openClone(page: Page, source: string) {
  await page.goto(`/ui/datasets/${source}`);
  await page.getByRole('button', { name: 'Clone', exact: true }).click();
  return page.getByRole('dialog');
}

test('a full clone shares the index files by hard links', async ({ page }) => {
  const dialog = await openClone(page, 'foaf');
  await dialog.getByLabel('New name').fill('foaf-linked');
  await expect(dialog.getByLabel('Persistent')).toBeChecked();
  await expect(dialog.getByLabel('All graphs')).toBeChecked();
  await dialog.getByLabel('Index files').selectOption('link');
  await dialog.getByRole('button', { name: 'Clone', exact: true }).click();
  await expect(dialog).toBeHidden();

  const banner = page.locator('.cloned');
  await expect(banner).toContainText('Cloned into /foaf-linked', { timeout: 15_000 });
  await expect(banner).toContainText('The clone hard-linked the source’s index files.');
  await expect(page.getByRole('listitem').filter({ hasText: 'into /foaf-linked' })).toContainText(
    '(hard-linked the source’s index files)',
  );

  await banner.getByRole('link', { name: 'Open' }).click();
  await expect(page.locator('.origin')).toContainText('cloned from /foaf');
  await expect(page.locator('.origin')).toContainText('index by link');
});

test('a partial in-memory clone copies the chosen graph and rebuilds', async ({ page }) => {
  const dialog = await openClone(page, 'foaf');
  await dialog.getByLabel('New name').fill('foaf-provenance');
  await dialog.getByLabel('In memory').check();
  // an in-memory clone has no index files to share
  await expect(dialog.getByLabel('Index files')).toBeHidden();
  await dialog.getByLabel('Only some').check();
  await expect(dialog).toContainText('Choose at least one graph.');
  await expect(dialog.getByRole('button', { name: 'Clone', exact: true })).toBeDisabled();
  await dialog
    .getByRole('group', { name: 'Graphs to copy' })
    .getByLabel('http://example.org/graph/provenance')
    .check();
  await dialog.getByRole('button', { name: 'Clone', exact: true }).click();
  await expect(dialog).toBeHidden();

  const banner = page.locator('.cloned');
  await expect(banner).toContainText('Cloned into /foaf-provenance', { timeout: 15_000 });
  await expect(banner).toContainText('rebuilt the index: some graphs are left out');

  await banner.getByRole('link', { name: 'Open' }).click();
  await expect(page.locator('.origin')).toContainText('only http://example.org/graph/provenance');
  await expect(page.locator('.origin')).toContainText('index rebuilt');
});
