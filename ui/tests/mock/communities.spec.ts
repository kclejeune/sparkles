// The Communities tab against the mock (C22 §2.3): the hierarchy as a tree, a community's
// report with its findings and citations, a review, and an edit that supersedes the
// report. The mock's `org` dataset has three canned communities.

import { expect, test } from '@playwright/test';
import { expectNoSidewaysScroll, phone } from '../phone';

async function open(page: import('@playwright/test').Page, team: string) {
  await page.goto('/ui/datasets/org?tab=communities');
  await expect(page.getByRole('tab', { name: 'Communities' })).toHaveAttribute(
    'aria-selected',
    'true',
  );
  const panel = page.getByTestId('communities');
  const tree = panel.getByRole('navigation', { name: 'Community hierarchy' });
  await expect(tree).toContainText('Engineering');
  await tree.getByRole('button', { name: new RegExp(team) }).click();
  await expect(panel.getByRole('heading', { name: team })).toBeVisible();
  return panel;
}

test('the tab shows the tree and a report, and reviews it', async ({ page }) => {
  const panel = await open(page, 'The payments team');
  await expect(panel).toContainText('Ana and Kai ship the checkout redesign.');
  await expect(panel).toContainText('Kai works with the rest of the group every week.');
  await panel.getByText('1 citation').click();
  await expect(panel).toContainText('http://example.org/ontology#memberOf');
  await expect(panel.getByRole('link', { name: 'Ana' })).toBeVisible();

  await panel.getByRole('button', { name: 'Reviewed', exact: true }).click();
  await expect(panel.locator('.status-line .badge')).toHaveText('reviewed');

  await panel.getByRole('button', { name: 'Edit' }).click();
  const dialog = page.getByRole('dialog', { name: 'Edit the report' });
  await dialog.getByLabel('Title').fill('Payments, led by Ana');
  await dialog.getByRole('button', { name: 'Save' }).click();
  await expect(panel.getByRole('heading', { name: 'Payments, led by Ana' })).toBeVisible();
  await expect(panel).toContainText('edited revision');
});

test('off main the tab offers no action on main', async ({ page }) => {
  const graphrag: string[] = [];
  page.on('request', (r) => {
    if (r.url().includes('/$/graphrag/')) graphrag.push(`${r.method()} ${r.url()}`);
  });
  await page.goto('/ui/datasets/org?branch=scratch-s2&tab=communities');
  const panel = page.getByTestId('communities');
  await expect(panel.getByTestId('communities-off-main')).toContainText('scratch-s2');
  await expect(panel.getByRole('button', { name: 'Build' })).toHaveCount(0);
  await expect(panel.getByRole('button', { name: 'Refresh' })).toHaveCount(0);
  await expect(panel.getByRole('navigation', { name: 'Community hierarchy' })).toHaveCount(0);
  expect(graphrag).toEqual([]);
});

test('leaving main ends a waiting run and never confirms it', async ({ page }) => {
  // the mock does not run builds, so the build and its task are stubbed here
  const waiting = {
    id: 'gr-wait',
    kind: 'graphrag',
    dataset: 'org',
    state: 'running',
    startedAt: new Date().toISOString(),
    message: 'Waiting for confirmation',
    cancellable: true,
    detail: {
      phase: 'awaiting-confirmation',
      estimate: {
        communities: 3,
        requests: 3,
        model: 'p/m',
        tokens: 9000,
        tokensPerBuild: 100000,
        tokensPerDay: 1000000,
      },
    },
  };
  const confirms: string[] = [];
  await page.route(/\/\$\/graphrag\/org\/build$/, (route) =>
    route.fulfill({ status: 202, json: waiting }),
  );
  await page.route(/\/\$\/graphrag\/org\/confirm$/, (route) => {
    confirms.push(route.request().postData() ?? '');
    return route.fulfill({ status: 200, json: {} });
  });
  await page.route(/\/\$\/tasks\/gr-wait$/, (route) => route.fulfill({ json: waiting }));
  await page.goto('/ui/datasets/org?tab=communities');
  const panel = page.getByTestId('communities');
  await panel.getByRole('button', { name: 'Build' }).click();
  const dialog = page.getByRole('dialog', { name: 'Send this build to the model?' });
  await expect(dialog).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  await page.getByRole('combobox', { name: 'Branch' }).selectOption('scratch-s2');
  await expect(panel.getByTestId('communities-off-main')).toBeVisible();
  // the follower polls every 700 ms: it must not open the estimate again
  await page.waitForTimeout(2000);
  await expect(dialog).toHaveCount(0);
  await expect(panel.getByTestId('graphrag-run')).toHaveCount(0);
  expect(confirms).toEqual([]);
});

test('a slow answer for an earlier community does not replace a later one', async ({ page }) => {
  // hold Engineering's answer until the platform team's has arrived
  let release: () => void = () => {};
  const held = new Promise<void>((r) => (release = r));
  await page.route(/\/\$\/graphrag\/org\/community\?/, async (route) => {
    if (decodeURIComponent(route.request().url()).includes('0:org')) await held;
    await route.continue();
  });
  await page.goto('/ui/datasets/org?tab=communities');
  const panel = page.getByTestId('communities');
  const tree = panel.getByRole('navigation', { name: 'Community hierarchy' });
  await expect(tree).toContainText('Engineering');
  await tree.getByRole('button', { name: /Engineering/ }).click();
  await tree.getByRole('button', { name: /The platform team/ }).click();
  await expect(panel.getByRole('heading', { name: 'The platform team' })).toBeVisible();
  release();
  await page.waitForTimeout(300);
  await expect(panel.getByRole('heading', { name: 'The platform team' })).toBeVisible();
  await expect(panel.getByRole('heading', { name: 'Engineering' })).toHaveCount(0);
});

test.describe('on a phone', () => {
  test.use({ ...phone, viewport: { width: 390, height: 844 } });

  test('the tab fits the screen', async ({ page }) => {
    // the other test edits the payments team on the shared mock
    await open(page, 'The platform team');
    await expectNoSidewaysScroll(page, 'the Communities tab');
  });
});
