// The Workbench trees at depth and width (ROADMAP item 633): sticky ancestor rows and a
// path above the details for a chain 20 levels deep, labels that keep their room, and a
// level with more children than the hierarchy route reads at once.

import { expect, test, type Page } from '@playwright/test';

test.describe.configure({ timeout: 90_000 });

const DEEP = 'http://example.org/deep#';
const url = (iri: string) => `/ui/workbench/ontology?ds=deep&iri=${encodeURIComponent(DEEP + iri)}`;
const tree = (page: Page) => page.getByRole('tree', { name: 'Classes (asserted)' });

test('a deep chain keeps sticky ancestors, a path and readable labels', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 700 });
  await page.goto(url('C19'));
  const row = tree(page).locator(`li[data-id="${DEEP}C19"] > .node`);
  await expect(row).toBeVisible({ timeout: 30_000 });
  // the deepest row keeps room for its label
  const nameWidth = await row.locator('.name').evaluate((e) => e.getBoundingClientRect().width);
  expect(nameWidth).toBeGreaterThan(200);
  // scrolled so that level 15 is the first row, the rows above it stay at the top
  const box = page.locator('.trees .tree-scroll');
  await box.evaluate((e, id) => {
    const n = e.querySelector(`li[data-id="${id}"] > .node`)!;
    e.scrollTop += n.getBoundingClientRect().top - e.getBoundingClientRect().top;
  }, `${DEEP}C15`);
  const sticky = page.getByLabel('Path to the first row');
  await expect(sticky.getByRole('button').last()).toHaveText(/^level 1[0-4] of/);
  await expect(sticky).toContainText('more above');
  // clicking an ancestor scrolls back up to it
  const before = await box.evaluate((e) => e.scrollTop);
  await sticky.getByRole('button').first().click();
  await expect.poll(() => box.evaluate((e) => e.scrollTop)).toBeLessThan(before);
  // the selection's path from the root, above the details
  const path = page.getByRole('navigation', { name: 'Path' });
  await expect(path).toContainText('level 0 of a deep chain');
  await expect(path.locator('[aria-current="location"]')).toContainText('level 19');
});

test('a level with more children than one read shows the rest on request', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto('/ui/workbench/ontology?ds=deep');
  await tree(page).getByRole('button', { name: 'Expand wide parent' }).click();
  const more = tree(page).locator('.more-row');
  await expect(more).toContainText('Showing 1,000 of 1,050', { timeout: 30_000 });
  await expect(tree(page).getByRole('treeitem', { name: 'wide child 1049' })).toHaveCount(0);
  await more.getByRole('button', { name: 'Show more' }).click();
  await expect(tree(page).getByRole('treeitem', { name: 'wide child 1049' })).toHaveCount(1);
  await expect(more).toHaveCount(0);
});
