// The server page's Notifications section (C21 §11) against a real server: a webhook
// channel added through the form to a receiver this test runs, a route to it, a test that
// reaches the receiver and shows on the channel and in the recent deliveries, a failed
// test to a receiver that is gone, and removing the channel, which takes it out of its
// route. The server allows private outbound addresses
// (global-setup.ts), so it can reach 127.0.0.1.

import { createServer, type IncomingHttpHeaders, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { expect, open } from './fixtures';

const URL = '/$/server/settings/notifications';

type Received = { headers: IncomingHttpHeaders; body: Record<string, unknown> };

/** A webhook receiver on a free port of 127.0.0.1 that keeps what it receives. */
async function receiver(): Promise<{ server: Server; url: string; received: Received[] }> {
  const received: Received[] = [];
  const server = createServer((req, res) => {
    let b = '';
    req.on('data', (c) => (b += c));
    req.on('end', () => {
      received.push({ headers: req.headers, body: JSON.parse(b || '{}') });
      res.writeHead(204).end();
    });
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address() as AddressInfo;
  return { server, url: `http://127.0.0.1:${port}/hook`, received };
}

open.describe.configure({ mode: 'serial' });

open(
  'the Notifications section adds, tests and removes a webhook channel',
  async ({ page, request }) => {
    // a clean runtime layer, also when the test is retried
    expect((await request.delete(URL)).ok()).toBe(true);
    const hook = await receiver();
    try {
      await page.goto('/ui/admin/server');
      const section = page.getByTestId('notifications-section');
      await expect(section.getByText('No channels.')).toBeVisible();
      await expect(section.getByTitle('Nothing is delivered except tests')).toHaveText('off');

      await section.getByRole('button', { name: 'Add channel' }).click();
      const add = section.getByRole('form', { name: 'Add a channel' });
      await add.getByLabel('Name', { exact: true }).fill('e2e-hook');
      await add.getByLabel('URL', { exact: true }).fill(hook.url);
      await add.getByRole('button', { name: 'Add', exact: true }).click();
      await expect(page.getByText('Added channel e2e-hook')).toBeVisible();
      const channels = section.getByRole('table', { name: 'Notification channels' });
      const row = channels.locator('[data-channel="e2e-hook"]');
      await expect(row).toContainText(hook.url);
      await expect(row).toContainText('webhook');

      await section.getByRole('button', { name: 'Add route' }).click();
      const route = section.getByRole('form', { name: 'Add a route' });
      await route.getByLabel('Event type').fill('backup.*');
      await route.getByRole('checkbox', { name: 'e2e-hook' }).check();
      await route.getByRole('button', { name: 'Add', exact: true }).click();
      await expect(page.getByText('Added a route for backup.*')).toBeVisible();
      const routes = section.getByRole('table', { name: 'Notification routes' });
      await expect(routes.locator('[data-route="backup.*"]')).toContainText('e2e-hook');
      // the route's channels are a field of the configuration panel, whose path escapes the
      // dot of the key, and CSS needs that backslash escaped again
      const panel = page.getByTestId('settings-notification-configuration');
      await expect(panel.locator('[data-field="routes.backup\\\\.*"]')).toHaveAttribute(
        'data-source',
        'runtime',
      );

      // a test reaches the receiver, though notifications are off
      await section.getByRole('button', { name: 'Send a test to e2e-hook' }).click();
      await expect(row.getByRole('status')).toContainText('Test: Delivered, 204');
      expect(hook.received).toHaveLength(1);
      expect(hook.received[0].body.type).toBe('notification.test');
      // without a signing secret the request carries no Standard Webhooks signature
      expect(hook.received[0].headers['webhook-signature']).toBeUndefined();
      await expect(row).toContainText('1 ok');
      const recent = page
        .getByTestId('notification-deliveries')
        .getByRole('table', { name: 'Recent deliveries' });
      const first = recent.locator('tbody tr').first();
      await expect(first).toContainText('notification.test');
      await expect(first).toContainText('e2e-hook');
      await expect(first).toContainText('ok');

      // with the receiver gone, the test fails and the error stays on the row
      await new Promise<void>((resolve) => hook.server.close(() => resolve()));
      await section.getByRole('button', { name: 'Send a test to e2e-hook' }).click();
      await expect(page.getByText('The test to e2e-hook failed', { exact: true })).toBeVisible();
      await expect(row.getByRole('status')).not.toContainText('Delivered');
      await expect(row).toContainText('1 failed');

      // removing the channel takes it out of its route, which goes with it
      await section.getByRole('button', { name: 'Remove channel e2e-hook' }).click();
      await page.getByRole('dialog').getByRole('button', { name: 'Remove' }).click();
      await expect(page.getByText('Removed channel e2e-hook')).toBeVisible();
      await expect(row).toHaveCount(0);
      await expect(section.getByText('No routes, so events go nowhere')).toBeVisible();
      const kind = await (await request.get(URL)).json();
      expect(kind.effective.channels).toEqual({});
      expect(kind.effective.routes).toEqual({});
    } finally {
      hook.server.close();
      await request.delete(URL);
    }
  },
);
