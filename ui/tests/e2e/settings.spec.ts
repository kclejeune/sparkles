// The dataset page's Settings tab (C19 §10) against a real server: sources, locks and
// resets of the declared settings file, a write that someone else's change makes stale,
// the read-only tab of a user without admin, memory maintenance, and declared datasets
// that offer no delete.

import { DATASET, DECLARED_DATASET, SETTINGS_DATASET } from './data';
import { expect, open, test } from './fixtures';

const row = (page: import('@playwright/test').Page, field: string) =>
  page.locator(`[data-testid=settings-assistant] [data-field="${field}"]`);

open('the Settings tab shows sources, locks and resets', async ({ page, request }) => {
  const created = await request.post('/$/datasets', {
    data: { dbName: SETTINGS_DATASET, dbType: 'mem' },
  });
  expect(created.ok()).toBe(true);
  await page.goto(`/ui/datasets/${SETTINGS_DATASET}`);
  await page.getByRole('tab', { name: 'Settings' }).click();
  await expect(page).toHaveURL(/tab=settings/);

  // the settings file locks send and declares historyDays
  const send = row(page, 'send');
  await expect(send).toHaveAttribute('data-source', 'locked');
  await expect(send.getByRole('combobox')).toBeDisabled();
  const history = row(page, 'historyDays');
  await expect(history).toHaveAttribute('data-source', 'declared');
  await expect(history.getByText('server config')).toBeVisible();
  await expect(history.getByRole('textbox')).toHaveValue('30');

  // a change becomes a runtime value with a reset
  await history.getByRole('textbox').fill('7');
  await page
    .getByTestId('settings-assistant')
    .getByRole('button', { name: 'Save', exact: true })
    .click();
  await expect(page.getByText('Saved the assistant settings')).toBeVisible();
  await expect(history).toHaveAttribute('data-source', 'runtime');
  const k = await (await request.get(`/$/settings/${SETTINGS_DATASET}/assistant`)).json();
  expect(k.runtime).toEqual({ historyDays: 7 });
  expect(k.overrides).toEqual([{ path: 'historyDays', declared: 30, runtime: 7 }]);
  // the change overrides the declared value, which the row and the section name
  await expect(history.getByText('overrides server config')).toBeVisible();
  await expect(history.getByText('server config: 30')).toBeVisible();
  await expect(page.getByTestId('overrides-assistant')).toContainText(
    '1 field overrides the server config',
  );
  await page.setViewportSize({ width: 1280, height: 2000 });
  await page
    .getByTestId('settings-assistant')
    .screenshot({ path: '../target/settings-overrides.png' });
  await page.setViewportSize({ width: 1280, height: 720 });

  // a change someone else made in the meantime: the save is refused and the section reloads
  await row(page, 'rowsForSummary').getByRole('textbox').fill('20');
  const other = await request.patch(`/$/settings/${SETTINGS_DATASET}/assistant`, {
    data: { deadlineSecs: 60 },
  });
  expect(other.ok()).toBe(true);
  await page
    .getByTestId('settings-assistant')
    .getByRole('button', { name: 'Save', exact: true })
    .click();
  await expect(page.getByText('Someone else changed the assistant settings')).toBeVisible();
  await expect(row(page, 'deadlineSecs').getByRole('textbox')).toHaveValue('60');
  await expect(row(page, 'rowsForSummary').getByRole('textbox')).toHaveValue('');

  // a field without a declared value resets to its default
  await expect(row(page, 'deadlineSecs').getByRole('button', { name: /to default$/ })).toHaveText(
    'Reset to default',
  );
  // the reset brings back the declared value
  await history
    .getByRole('button', { name: 'Use server config for Keep questions (days)' })
    .click();
  await expect(history).toHaveAttribute('data-source', 'declared');
  await expect(history.getByRole('textbox')).toHaveValue('30');
  await expect(page.getByTestId('overrides-assistant')).toHaveCount(0);

  // Use server config for all clears the overrides and keeps the other changes
  const both = await request.patch(`/$/settings/${SETTINGS_DATASET}/assistant`, {
    data: { historyDays: 5 },
  });
  expect(both.ok()).toBe(true);
  await page
    .getByTestId('settings-assistant')
    .getByRole('button', { name: 'Reload Assistant' })
    .click();
  await page.getByRole('button', { name: 'Use server config for all' }).click();
  const dialog = page.getByRole('dialog', { name: 'Use the server config for these fields?' });
  await expect(dialog).toContainText('historyDays');
  await dialog.getByRole('button', { name: 'Use server config' }).click();
  await expect(history).toHaveAttribute('data-source', 'declared');
  const after = await (await request.get(`/$/settings/${SETTINGS_DATASET}/assistant`)).json();
  expect(after.runtime).toEqual({ deadlineSecs: 60 });

  // the runtime layer as JSON: a change of the locked field is refused and highlighted
  const panel = page.getByTestId('settings-assistant');
  await panel.getByText('Advanced: the runtime layer as JSON').click();
  await panel.getByRole('textbox', { name: 'Assistant runtime layer' }).fill('{"send": "rows"}');
  await panel.getByRole('button', { name: 'Save JSON' }).click();
  await expect(panel.getByText('The assistant settings were not saved.')).toBeVisible();
  await expect(send).toHaveClass(/conflict/);

  // memory maintenance: no retention is set, and a consolidation runs on request
  const memory = page.getByTestId('settings-memory');
  await expect(memory.getByTestId('maintenance-retention').getByRole('button')).toBeDisabled();
  await memory
    .getByTestId('maintenance-consolidation')
    .getByRole('button', { name: 'Run now' })
    .click();
  await expect(page.getByText('Consolidation finished')).toBeVisible({ timeout: 30_000 });
  await expect(memory.getByTestId('maintenance-consolidation')).toContainText('Last run');
});

open('declared datasets offer no delete', async ({ page }) => {
  await page.goto('/ui/datasets');
  const del = page.getByRole('button', { name: `Delete ${DECLARED_DATASET}` });
  await expect(del).toBeDisabled();
  await expect(del).toHaveAttribute('title', /declared this dataset/);
  await page.goto(`/ui/datasets/${DECLARED_DATASET}`);
  await expect(page.getByRole('button', { name: 'Delete' })).toBeDisabled();
});

test('a user without admin reads the settings', async ({ page }) => {
  await page.goto(`/ui/datasets/${DATASET}?tab=settings`);
  await expect(page.getByText('Changing them needs admin on the dataset.')).toBeVisible();
  const panel = page.getByTestId('settings-assistant');
  await expect(panel.locator('[data-field="enabled"] input')).toBeDisabled();
  await expect(panel.getByRole('button', { name: 'Save', exact: true })).toHaveCount(0);
  await expect(
    page.getByTestId('maintenance-consolidation').getByRole('button', { name: 'Run now' }),
  ).toHaveCount(0);
});
