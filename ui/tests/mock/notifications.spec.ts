// The server page's Notifications section against the mock (mock/notifications.mjs): the
// declared channels with their status, secrets and routes, a locked channel that cannot
// be removed, adding a channel and a route, a test that succeeds and one that fails, a
// route list edited in the configuration panel, a route to a missing channel refused in
// place, removing a declared channel and restoring it, the recent deliveries and active
// conditions, and the layout at a phone's width. Each test starts from the declared state.

import { expect, test, type Page } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

const URL = '/$/server/settings/notifications';

test.beforeEach(async ({ request }) => {
  expect((await request.delete(URL)).ok()).toBe(true);
});

async function openSection(page: Page) {
  await page.goto('/ui/admin/server');
  const section = page.getByTestId('notifications-section');
  const channels = section.getByRole('table', { name: 'Notification channels' });
  await expect(channels.locator('[data-channel=ops]')).toBeVisible();
  const panel = page.getByTestId('settings-notification-configuration');
  return {
    section,
    channels,
    routes: section.getByRole('table', { name: 'Notification routes' }),
    panel,
    row: (field: string) => panel.locator(`[data-field="${field}"]`),
  };
}

const runtime = async (page: Page) =>
  (await (await page.request.get(URL)).json()).runtime as Record<string, unknown>;

test('shows the channels, their secrets, routes and recent deliveries', async ({ page }) => {
  const { section, channels, routes } = await openSection(page);
  await expect(
    section.getByTitle('Events are delivered to the channels the routes name'),
  ).toHaveText('on');
  const phone = channels.locator('[data-channel=phone]');
  await expect(phone).toContainText('ntfy');
  await expect(phone).toContainText('https://ntfy.sh/sparkles-ops');
  // the token is named but has no value, the signing secret has one
  await expect(phone).toContainText('ntfy-token not set');
  const ops = channels.locator('[data-channel=ops]');
  await expect(ops).toContainText('hook-signing');
  await expect(ops).not.toContainText('not set');
  await expect(ops).toContainText('*, backup.*');
  await expect(ops).toContainText('1 ok, 1 failed');
  // the settings file locks phone, so it cannot be removed
  await expect(section.getByRole('button', { name: 'Remove channel phone' })).toBeDisabled();
  await expect(section.getByRole('button', { name: 'Remove channel ops' })).toBeEnabled();
  // declared routes cannot be removed, only emptied
  await expect(routes.locator('[data-route="backup.*"]')).toContainText('phone, ops');
  await expect(
    section.getByRole('button', { name: 'Remove the route for backup.*' }),
  ).toBeDisabled();

  const deliveries = page.getByTestId('notification-deliveries');
  await expect(
    deliveries.getByRole('table', { name: 'Active conditions' }).locator('tbody tr'),
  ).toHaveCount(1);
  const recent = deliveries.getByRole('table', { name: 'Recent deliveries' });
  await expect(recent.locator('tbody tr')).toHaveCount(4);
  // the newest first, and a failure with its status and error
  await expect(recent.locator('tbody tr').first()).toContainText('backup.verify.failed');
  await expect(recent.locator('tbody tr').last()).toContainText(
    '503: the webhook answered 503 Service Unavailable',
  );
});

test('adds a channel and a route, and sends tests', async ({ page }) => {
  const { section, channels, routes, row } = await openSection(page);
  await section.getByRole('button', { name: 'Add channel' }).click();
  const add = section.getByRole('form', { name: 'Add a channel' });
  await add.getByLabel('Name', { exact: true }).fill('mail');
  await add.getByLabel('Type').selectOption('email');
  // a field of another type is not offered, and a bad address is refused in the form
  await expect(add.getByLabel('URL', { exact: true })).toHaveCount(0);
  await add.getByLabel('SMTP host').fill('smtp.example.org');
  await add.getByLabel('From').fill('sparkles@example.org');
  await add.getByLabel('To', { exact: true }).fill('ops@example.org, nope');
  await add.getByRole('button', { name: 'Add', exact: true }).click();
  await expect(add.getByText('nope is not a mail address')).toBeVisible();
  await add.getByLabel('To', { exact: true }).fill('ops@example.org, oncall@example.org');
  await add.getByRole('button', { name: 'Add', exact: true }).click();
  await expect(page.getByText('Added channel mail')).toBeVisible();
  const mail = channels.locator('[data-channel=mail]');
  await expect(mail).toContainText(
    'smtp.example.org port 587 (starttls) to ops@example.org, oncall@example.org',
  );
  await expect(mail).toContainText('none');
  expect(await runtime(page)).toEqual({
    channels: {
      mail: {
        type: 'email',
        host: 'smtp.example.org',
        tls: 'starttls',
        from: 'sparkles@example.org',
        to: ['ops@example.org', 'oncall@example.org'],
      },
    },
  });
  // its fields are in the configuration panel, under its own group
  await expect(row('channels.mail.host').getByRole('textbox')).toHaveValue('smtp.example.org');

  // a route of backup failures to it
  await section.getByRole('button', { name: 'Add route' }).click();
  const route = section.getByRole('form', { name: 'Add a route' });
  await route.getByLabel('Event type').fill('backup.failed');
  await route.getByRole('button', { name: 'Add', exact: true }).click();
  await expect(route.getByText('Choose at least one channel')).toBeVisible();
  await route.getByRole('checkbox', { name: 'mail' }).check();
  await route.getByRole('button', { name: 'Add', exact: true }).click();
  await expect(page.getByText('Added a route for backup.failed')).toBeVisible();
  await expect(routes.locator('[data-route="backup.failed"]')).toContainText('mail');
  await expect(mail).toContainText('backup.failed');

  // a test that the channel takes, and its delivery in the recent list
  await section.getByRole('button', { name: 'Send a test to mail' }).click();
  await expect(mail.getByRole('status')).toContainText('Test: Delivered, 250 in 42 ms');
  await expect(mail).toContainText('1 ok');
  const recent = page.getByTestId('notification-deliveries').getByRole('table', {
    name: 'Recent deliveries',
  });
  await expect(recent.locator('tbody tr').first()).toContainText('notification.test');
  await expect(recent.locator('tbody tr').first()).toContainText('mail');

  // a runtime route can be removed, which keeps the channel
  await section.getByRole('button', { name: 'Remove the route for backup.failed' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Remove' }).click();
  await expect(page.getByText('Removed the route for backup.failed')).toBeVisible();
  await expect(routes.locator('[data-route="backup.failed"]')).toHaveCount(0);
  await expect(mail).toBeVisible();
});

test('a failed test shows the error on the channel', async ({ page }) => {
  const { section, channels } = await openSection(page);
  await section.getByRole('button', { name: 'Add channel' }).click();
  const add = section.getByRole('form', { name: 'Add a channel' });
  await add.getByLabel('Name', { exact: true }).fill('broken');
  await add.getByLabel('URL', { exact: true }).fill('https://fail.example.org/hook');
  await add.getByRole('button', { name: 'Add', exact: true }).click();
  const broken = channels.locator('[data-channel=broken]');
  await expect(broken).toBeVisible();
  await section.getByRole('button', { name: 'Send a test to broken' }).click();
  await expect(broken.getByRole('status')).toContainText('connection refused');
  await expect(page.getByText('The test to broken failed', { exact: true })).toBeVisible();
  await expect(broken).toContainText('0 ok, 1 failed');
  await expect(broken).toContainText('failed');
});

test('edits a route in the configuration and refuses a missing channel', async ({ page }) => {
  const { routes, panel, row } = await openSection(page);
  const all = row('routes.*');
  await expect(all.getByRole('list', { name: '*' }).getByRole('listitem')).toHaveCount(1);
  // a channel the server does not know is flagged before a save
  const input = all.getByPlaceholder('Add a channel');
  await input.fill('pager');
  await input.press('Enter');
  await expect(all.getByText('No channel named pager')).toBeVisible();
  await all.getByRole('button', { name: 'Remove pager' }).click();
  await expect(all.getByText('No channel named pager')).toHaveCount(0);
  // a known one is suggested and saved
  await all.getByRole('button', { name: 'Add phone' }).click();
  await panel.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByText('Saved the notification configuration settings')).toBeVisible();
  await expect(routes.locator('[data-route="*"]')).toContainText('ops, phone');
  expect(await runtime(page)).toEqual({ routes: { '*': ['ops', 'phone'] } });
  await expect(all).toHaveAttribute('data-source', 'runtime');
});

test('removes a declared channel from its routes and restores it', async ({ page }) => {
  const { section, channels, routes } = await openSection(page);
  await section.getByRole('button', { name: 'Remove channel ops' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toContainText('*, backup.*');
  await dialog.getByRole('button', { name: 'Remove' }).click();
  await expect(page.getByText('Removed channel ops')).toBeVisible();
  await expect(channels.locator('[data-channel=ops]')).toHaveCount(0);
  // the declared routes keep their place without it
  await expect(routes.locator('[data-route="backup.*"]')).toContainText('phone');
  await expect(routes.locator('[data-route="*"]')).toContainText('none');
  expect(await runtime(page)).toEqual({
    channels: { ops: null },
    routes: { 'backup.*': ['phone'], '*': [] },
  });
  await section.getByRole('button', { name: 'Use server config for channel ops' }).click();
  await expect(page.getByText('Channel ops uses the server config again')).toBeVisible();
  await expect(channels.locator('[data-channel=ops]')).toBeVisible();
});

test('fits a phone', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const { section } = await openSection(page);
  await expect(section).toBeVisible();
  await expectNoSidewaysScroll(page, 'the Notifications section at 390 px');
});
