// The Backups area against the stateful mock (mock/backups.mjs). The tests share the
// mock's state and run in order; each uses names of its own.

import { expect, test } from '@playwright/test';

test('the backup table spans repositories and shows lineage, verify results and the manifest', async ({
  page,
}) => {
  await page.goto('/ui/backups');
  await expect(page.getByText('minio-lab is unreachable')).toBeVisible();

  const older = page.getByRole('row', { name: /foaf-before-reload/ });
  await expect(older.getByText('other lineage')).toBeVisible();
  await expect(page.getByRole('row', { name: /nightly-wiki-/ }).first()).toContainText(
    'not on this server',
  );
  await expect(page.getByRole('row', { name: /before the schema change/ })).toContainText('ok');

  await page.getByRole('combobox', { name: 'Repository' }).selectOption('dr-source');
  await expect(page.getByRole('row', { name: /nightly-foaf/ })).toHaveCount(0);
  await page.getByRole('combobox', { name: 'Repository' }).selectOption('');

  await older.getByRole('button', { name: 'foaf-before-reload', exact: true }).click();
  const drawer = page.getByRole('dialog', { name: 'foaf-before-reload' });
  await expect(drawer).toContainText('before the September reload');
  await expect(drawer.getByRole('row', { name: /gen-0001\/vocab\.dat/ })).toBeVisible();
  await drawer.getByRole('button', { name: 'Close' }).click();
  await expect(drawer).toBeHidden();
});

test('adding a repository tests the connection; a failed one can be removed again', async ({
  page,
}) => {
  await page.goto('/ui/backups?tab=repositories');
  await page.getByRole('button', { name: 'Add repository' }).click();
  let dialog = page.getByRole('dialog', { name: 'Add repository' });
  await dialog.getByRole('textbox', { name: 'Name' }).fill('e2e-offline');
  await dialog.getByRole('textbox', { name: 'Path' }).fill('/mnt/offline/e2e');
  await dialog.getByRole('button', { name: 'Add and test' }).click();

  dialog = page.getByRole('dialog', { name: 'Added e2e-offline' });
  await expect(dialog).toContainText('Connection failed');
  await expect(dialog).toContainText('No such file or directory');
  await dialog.getByRole('button', { name: 'Remove it' }).click();
  // back to the form, with the settings kept
  await expect(page.getByRole('dialog', { name: 'Add repository' })).toBeVisible();
  await page.getByRole('textbox', { name: 'Name' }).fill('e2e-nas');
  await page.getByRole('textbox', { name: 'Path' }).fill('/srv/e2e');
  await page.getByRole('button', { name: 'Add and test' }).click();

  dialog = page.getByRole('dialog', { name: 'Added e2e-nas' });
  await expect(dialog).toContainText('Connection works');
  await expect(dialog).toContainText('conditional writes');
  await dialog.getByRole('button', { name: 'Done' }).click();
  await expect(page.getByRole('article', { name: 'e2e-nas' })).toContainText('/srv/e2e');
  await expect(page.getByRole('article', { name: 'e2e-offline' })).toHaveCount(0);
});

test('a backup made on the dataset page restores into a new dataset', async ({ page }) => {
  await page.goto('/ui/datasets/foaf');
  const panel = page.getByRole('region', { name: 'Backups' });
  await panel.getByRole('button', { name: 'Back up now' }).click();
  const dialog = page.getByRole('dialog', { name: 'Back up foaf' });
  await dialog.getByRole('combobox', { name: 'Repository' }).selectOption('local');
  await dialog.getByRole('textbox', { name: 'Name' }).fill('e2e-backup-1');
  await dialog.getByRole('textbox', { name: 'Note' }).fill('made by the e2e test');
  await dialog.getByRole('button', { name: 'Back up' }).click();

  const row = panel.getByRole('row', { name: /e2e-backup-1/ });
  await expect(row).toBeVisible({ timeout: 15_000 });
  await row.getByRole('button', { name: 'Restore…' }).click();

  const restore = page.getByRole('dialog', { name: 'Restore foaf' });
  await expect(restore).toContainText('e2e-backup-1');
  await restore.getByRole('button', { name: 'Next' }).click();
  await restore.getByRole('textbox', { name: 'Name' }).fill('foaf-e2e-copy');
  await restore.getByRole('button', { name: 'Next' }).click();
  await expect(restore).toContainText('Here: a new id');
  await restore.getByRole('button', { name: 'Restore', exact: true }).click();
  await expect(restore).toContainText('/foaf-e2e-copy is ready', { timeout: 15_000 });
  await restore.getByRole('link', { name: 'Open' }).click();
  await expect(page).toHaveURL(/\/ui\/datasets\/foaf-e2e-copy$/);
});

test('replacing a dataset shows the lost commits and needs its name typed', async ({ page }) => {
  await page.goto('/ui/datasets/foaf');
  const panel = page.getByRole('region', { name: 'Backups' });
  // a policy backup from before the head commit
  await panel
    .getByRole('row', { name: /foaf-6h-foaf-/ })
    .first()
    .getByRole('button', { name: 'Restore…' })
    .click();

  const restore = page.getByRole('dialog', { name: 'Restore foaf' });
  await restore.getByRole('button', { name: 'Next' }).click();
  await restore.getByRole('radio', { name: /Replace/ }).check();
  await expect(restore).toContainText(/Commits? \d+(–\d+)? will be lost/);
  const next = restore.getByRole('button', { name: 'Next' });
  await expect(next).toBeDisabled();
  await restore.getByRole('textbox', { name: 'Type foaf to confirm' }).fill('fo');
  await expect(next).toBeDisabled();
  await restore.getByRole('textbox', { name: 'Type foaf to confirm' }).fill('foaf');
  await next.click();
  // keeping the id would issue the lost commit numbers again
  await expect(restore.getByRole('radio', { name: /Keep the id/ })).toBeDisabled();
  await restore.getByRole('button', { name: 'Replace foaf' }).click();
  await expect(restore).toContainText('/foaf is ready', { timeout: 15_000 });
});

test('the policy editor previews runs, the name template and retention', async ({ page }) => {
  await page.goto('/ui/backups?tab=policies');
  await page.getByRole('button', { name: 'New policy' }).click();
  const dialog = page.getByRole('dialog', { name: 'New backup policy' });
  await dialog.getByRole('textbox', { name: 'Name', exact: true }).fill('e2e-weekly');
  await dialog.getByRole('textbox', { name: /Name patterns/ }).fill('foaf*');
  await expect(dialog).toContainText('Matches now: foaf');
  await dialog.getByRole('radio', { name: 'Weekly' }).check();
  await dialog.getByRole('combobox', { name: 'Time zone' }).selectOption('UTC');

  const runs = dialog.getByRole('list', { name: 'Next runs' }).getByRole('listitem');
  await expect(dialog).toContainText('Every Sunday at 02:30 (UTC)');
  await expect(runs).toHaveCount(5);
  await expect(dialog).toContainText(/Next: e2e-weekly-foaf-\d{8}t023000z/);

  await dialog.getByRole('textbox', { name: /^Backup names/ }).fill('{dataset}-{nope}');
  await expect(dialog).toContainText('unknown placeholder {nope}');
  await dialog.getByRole('textbox', { name: /^Backup names/ }).fill('{dataset}-w{date:%V}');

  await dialog.getByRole('textbox', { name: 'Expire after' }).fill('30d');
  await dialog.getByRole('spinbutton', { name: 'Keep at least' }).fill('7');
  await dialog.getByRole('spinbutton', { name: 'Keep at most' }).fill('60');
  await expect(dialog).toContainText(
    'Keeps at least 7; deletes backups older than 30 days, and any beyond 60.',
  );

  await dialog.getByRole('radio', { name: 'Custom' }).check();
  await dialog.getByRole('textbox', { name: 'Cron or interval' }).fill('61 * * * *');
  await expect(dialog).toContainText('minute value “61” is out of range');
  await expect(dialog.getByRole('button', { name: 'Create policy' })).toBeDisabled();
  await dialog.getByRole('textbox', { name: 'Cron or interval' }).fill('every 12h');
  await expect(dialog).toContainText('Every 12 hours, counted from 00:00 UTC');

  await dialog.getByRole('button', { name: 'Create policy' }).click();
  await expect(page.getByRole('row', { name: /e2e-weekly/ })).toContainText('every 12h');
});

test('garbage collection starts with a dry run', async ({ page }) => {
  await page.goto('/ui/backups?tab=repositories');
  await page
    .getByRole('article', { name: 'local' })
    .getByRole('button', { name: 'Run GC' })
    .click();
  const dialog = page.getByRole('dialog', { name: 'Garbage collection: local' });
  await dialog.getByRole('button', { name: 'Dry run' }).click();
  await expect(dialog).toContainText('Would delete', { timeout: 15_000 });
  const run = dialog.getByRole('button', { name: /^Delete \d+ blobs/ });
  await expect(run).toBeEnabled();
  await run.click();
  await expect(dialog.getByText('Freed', { exact: true })).toBeVisible({ timeout: 15_000 });
});

test('a running backup task can be cancelled from Activity', async ({ page, request }) => {
  const res = await request.post('/$/repositories/local/verify', { data: { level: 'data' } });
  expect(res.status()).toBe(202);
  const task = await res.json();

  await page.goto('/ui/backups?tab=activity');
  await page.getByRole('button', { name: `Cancel task ${task.id} (backup-verify)` }).click();
  await expect(
    page.getByRole('listitem').filter({ hasText: 'cancelled at' }).first(),
  ).toBeVisible();
  const after = await (await request.get(`/$/tasks/${task.id}`)).json();
  expect(after.state).toBe('cancelled');
});
