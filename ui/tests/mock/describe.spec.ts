// The DESCRIBE setting on the dataset page against the mock: change the mode, a flag and
// a limit, see a bad limit refused, then go back to the defaults.

import { expect, test } from '@playwright/test';

// the first page load compiles the UI in vite dev
test.describe.configure({ timeout: 90_000 });

test('the dataset page changes and resets the DESCRIBE setting', async ({ page }) => {
  await page.goto('/ui/datasets/foaf');
  const panel = page.getByTestId('describe-setting');
  await expect(panel.getByText('defaults', { exact: true })).toBeVisible();
  const save = panel.getByRole('button', { name: 'Save' });
  await expect(save).toBeDisabled();

  await panel.getByLabel('Mode').selectOption('scbd');
  await panel.getByRole('checkbox', { name: /^Labels/ }).check();
  await panel.getByLabel('Max triples').fill('x');
  await expect(panel.getByText('Max triples must be a whole number')).toBeVisible();
  await expect(save).toBeDisabled();
  await panel.getByLabel('Max triples').fill('500');
  await save.click();
  await expect(panel.getByText('dataset setting', { exact: true })).toBeVisible();

  const stored = await page.request.get('/$/describe/foaf').then((r) => r.json());
  expect(stored).toMatchObject({
    mode: 'scbd',
    labels: true,
    reifiers: false,
    maxTriples: 500,
    maxDepth: null,
    source: 'dataset',
  });

  // the setting survives a reload
  await page.reload();
  await expect(panel.getByLabel('Mode')).toHaveValue('scbd');
  await expect(panel.getByLabel('Max triples')).toHaveValue('500');

  await panel.getByRole('button', { name: 'Use the defaults' }).click();
  await expect(panel.getByText('defaults', { exact: true })).toBeVisible();
  await expect(panel.getByLabel('Mode')).toHaveValue('cbd');
  await expect(panel.getByLabel('Max triples')).toHaveValue('');
});
