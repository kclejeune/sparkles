// Backups against the real server without auth (every caller is the local server
// admin): an `fs` repository under the directory the server's backup config allows, a
// backup made on the dataset page, its restore into a new dataset, and the policy
// editor's preview. The tests build on each other, so they run in order.

import { expect, open, server } from './fixtures';

open.describe.configure({ mode: 'serial' });

const DS = 'backed';
const REPO = 'e2e-local';

open(
  'the API registers fs repositories only where the backup config allows',
  async ({ request }) => {
    // the dataset the next tests back up and restore
    let r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
    expect(r.ok()).toBeTruthy();
    r = await request.post(`/${DS}/data`, {
      headers: { 'Content-Type': 'text/turtle' },
      data: '<urn:e2e:a> <urn:e2e:p> "one" .\n<urn:e2e:b> <urn:e2e:p> "two" .\n',
    });
    expect(r.ok()).toBeTruthy();

    r = await request.post('/$/repositories', {
      data: { name: 'outside', type: 'fs', path: `${server.repos}-elsewhere/r` },
    });
    expect(r.status()).toBe(400);
    const body = await r.json();
    expect(body.code).toBe('invalid-config');
    expect(body.field).toBe('path');
    // credentials must name a source of the backup config
    r = await request.post('/$/repositories', {
      data: {
        name: 'cloud',
        type: 's3',
        bucket: 'b',
        credentials: { source: 'env', accessKeyIdVar: 'HOME', secretAccessKeyVar: 'PATH' },
      },
    });
    expect(r.status()).toBe(400);
    expect((await r.json()).field).toBe('credentials');
  },
);

open('adding an fs repository tests the connection', async ({ page }) => {
  await page.goto('/ui/backups?tab=repositories');
  await page.getByRole('button', { name: 'Add repository' }).click();
  const dialog = page.getByRole('dialog', { name: 'Add repository' });
  await dialog.getByRole('textbox', { name: 'Name' }).fill(REPO);
  await dialog.getByRole('textbox', { name: 'Path' }).fill(`${server.repos}/local`);
  await dialog.getByRole('button', { name: 'Add and test' }).click();

  const added = page.getByRole('dialog', { name: `Added ${REPO}` });
  await expect(added).toContainText('Connection works');
  await expect(added).toContainText('conditional writes');
  await added.getByRole('button', { name: 'Done' }).click();
  await expect(page.getByRole('article', { name: REPO })).toContainText(`${server.repos}/local`);
});

open('a backup made on the dataset page restores into a new dataset', async ({ page, request }) => {
  await page.goto(`/ui/datasets/${DS}`);
  const panel = page.getByRole('region', { name: 'Backups' });
  await panel.getByRole('button', { name: 'Back up now' }).click();
  const dialog = page.getByRole('dialog', { name: `Back up ${DS}` });
  await dialog.getByRole('combobox', { name: 'Repository' }).selectOption(REPO);
  await dialog.getByRole('textbox', { name: 'Name' }).fill('e2e-b1');
  await dialog.getByRole('textbox', { name: 'Note' }).fill('made by the e2e test');
  await dialog.getByRole('button', { name: 'Back up' }).click();

  const row = panel.getByRole('row', { name: /e2e-b1/ });
  await expect(row).toBeVisible({ timeout: 15_000 });
  const backup = await (await request.get(`/$/backups/${DS}/${REPO}/e2e-b1`)).json();
  expect(backup.note).toBe('made by the e2e test');
  expect(backup.commit.quads).toBe(2);
  expect(backup.files.map((f: { path: string }) => f.path)).toContain('CURRENT');

  await row.getByRole('button', { name: 'Restore…' }).click();
  const restore = page.getByRole('dialog', { name: `Restore ${DS}` });
  await expect(restore).toContainText('e2e-b1');
  await restore.getByRole('button', { name: 'Next' }).click();
  await restore.getByRole('textbox', { name: 'Name' }).fill(`${DS}-copy`);
  await restore.getByRole('button', { name: 'Next' }).click();
  await expect(restore).toContainText('Here: a new id');
  await restore.getByRole('button', { name: 'Restore', exact: true }).click();
  await expect(restore).toContainText(`/${DS}-copy is ready`, { timeout: 15_000 });
  await restore.getByRole('link', { name: 'Open' }).click();
  await expect(page).toHaveURL(new RegExp(`/ui/datasets/${DS}-copy$`));

  const info = await (await request.get(`/$/datasets/${DS}-copy`)).json();
  expect(info.restoredFrom).toMatchObject({ repository: REPO, backup: 'e2e-b1' });
  expect(info.forkedFrom.id).toBe(backup.dataset.id);
  expect(info.quads).toBe(2);
});

open('the policy editor previews runs from the server', async ({ page }) => {
  await page.goto('/ui/backups?tab=policies');
  await page.getByRole('button', { name: 'New policy' }).click();
  const dialog = page.getByRole('dialog', { name: 'New backup policy' });
  await dialog.getByRole('textbox', { name: 'Name', exact: true }).fill('e2e-weekly');
  await dialog.getByRole('textbox', { name: /Name patterns/ }).fill(`${DS}*`);
  await expect(dialog).toContainText(`Matches now: ${DS}, ${DS}-copy`);
  await dialog.getByRole('radio', { name: 'Weekly' }).check();
  await dialog.getByRole('combobox', { name: 'Time zone' }).selectOption('UTC');

  // the description and the next runs come from the server's preview
  await expect(dialog).toContainText('Every Sunday at 02:30 (UTC)');
  const runs = dialog.getByRole('list', { name: 'Next runs' }).getByRole('listitem');
  await expect(runs).toHaveCount(5);
  await expect(dialog).toContainText(new RegExp(`Next: e2e-weekly-${DS}-\\d{8}t023000z`));

  await dialog.getByRole('radio', { name: 'Custom' }).check();
  await dialog.getByRole('textbox', { name: 'Cron or interval' }).fill('61 * * * *');
  await expect(dialog.getByRole('button', { name: 'Create policy' })).toBeDisabled();
  await dialog.getByRole('textbox', { name: 'Cron or interval' }).fill('every 12h');
  await expect(dialog).toContainText('Every 12 hours, counted from 00:00 UTC');

  await dialog.getByRole('button', { name: 'Create policy' }).click();
  await expect(page.getByRole('row', { name: /e2e-weekly/ })).toContainText('every 12h');
});
