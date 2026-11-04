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
  // the tree draws the rows in view, so the end of the level is reached by the keyboard
  await expect(tree(page).getByRole('treeitem', { name: 'wide child 0000' })).toBeVisible({
    timeout: 30_000,
  });
  await tree(page).focus();
  await page.keyboard.press('End');
  const more = tree(page).locator('.more-row');
  await expect(more).toContainText('Showing 1,000 of 1,050', { timeout: 30_000 });
  await expect(more).toBeInViewport();
  await more.getByRole('button', { name: 'Show more' }).click();
  await expect(more).toHaveCount(0);
  // 1,050 children are now rows of the tree, of which a screenful is in the page
  await tree(page).focus();
  await page.keyboard.press('End');
  await expect(tree(page).getByRole('treeitem').last()).toBeInViewport();
  await page.keyboard.type('wide child 1049');
  await expect(tree(page).getByRole('treeitem', { name: 'wide child 1049' })).toHaveCount(1);
});

test('a long level draws the rows in view and the keyboard walks the tree', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto('/ui/workbench/ontology?ds=deep');
  const rows = tree(page).getByRole('treeitem');
  await tree(page).getByRole('button', { name: 'Expand wide parent' }).click();
  await expect(tree(page).getByRole('treeitem', { name: 'wide child 0000' })).toBeVisible({
    timeout: 30_000,
  });
  // a thousand children are read and a screenful of them is in the page
  expect(await rows.count()).toBeLessThan(80);
  const wide = tree(page).getByRole('treeitem', { name: 'wide parent' });
  await expect(wide).toHaveAttribute('aria-expanded', 'true');
  // the tree is one tab stop, and its arrow keys move a cursor
  await tree(page).focus();
  await page.keyboard.press('Home');
  const first = rows.first();
  await expect(first).toHaveAttribute('aria-level', '1');
  const id = await tree(page).evaluate((e) => e.getAttribute('aria-activedescendant'));
  expect(id).toBeTruthy();
  await page.keyboard.press('ArrowDown');
  const next = await tree(page).evaluate((e) => e.getAttribute('aria-activedescendant'));
  expect(next).not.toBe(id);
  // Enter selects the row under the cursor
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/iri=/);
  await expect(page.getByRole('heading', { name: 'wide parent' })).toBeVisible();
  // a row at the end of the level is reached by End, and the page holds the same
  // screenful of rows there. Showing the selection scrolls too, so End is pressed until
  // the end is in view.
  await expect(async () => {
    await tree(page).focus();
    await page.keyboard.press('End');
    expect(await rows.count()).toBeLessThan(80);
    await expect(tree(page).locator('.more-row, li[role="treeitem"]').last()).toBeInViewport({
      timeout: 1_000,
    });
  }).toPass({ timeout: 15_000 });
  // the sticky ancestor names the open parent while its children scroll under it
  await expect(page.getByLabel('Path to the first row')).toContainText('wide parent');
});

test('a link to a class far down a long level scrolls the tree to its row', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 800 });
  await page.goto(url('W1045'));
  const row = tree(page).getByRole('treeitem', { selected: true });
  await expect(row).toBeVisible({ timeout: 30_000 });
  await expect(row).toBeInViewport();
  const scrolled = await page.locator('.trees .tree-scroll').evaluate((e) => e.scrollTop);
  expect(scrolled).toBeGreaterThan(10_000);
});

test('the tree keeps its labels when a slow summary arrives after the first level', async ({
  page,
}) => {
  const summary = /\/\$\/ontology\/deep(\?.*)?$/;
  await page.route(summary, async (route) => {
    await new Promise((r) => setTimeout(r, 2500));
    await route.continue();
  });
  const arrived = page.waitForResponse((r) => summary.test(r.url()), { timeout: 30_000 });
  await page.goto('/ui/workbench/ontology?ds=deep');
  const parent = tree(page).getByRole('button', { name: 'wide parent', exact: true });
  await expect(parent).toBeVisible({ timeout: 30_000 });
  // the summary comes after the first level, and the labels are still there once it has
  await arrived;
  await page.waitForTimeout(500);
  await expect(parent).toBeVisible();
});

test('a refused list is not read again until another list is needed', async ({ page }) => {
  let reads = 0;
  await page.route(/\/\$\/ontology\/deep\/entities/, async (route) => {
    reads++;
    await route.fulfill({ status: 500, body: 'refused' });
  });
  await page.goto(url('C3'));
  await expect(page.getByRole('heading', { name: /level 3/ })).toBeVisible({ timeout: 30_000 });
  await page.waitForTimeout(2000);
  const first = reads;
  expect(first).toBeGreaterThan(0);
  expect(first).toBeLessThanOrEqual(2);
  await page.waitForTimeout(1500);
  expect(reads).toBe(first);
});

test('a click keeps the focus on the tree and the paging row is an item of it', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto('/ui/workbench/ontology?ds=deep');
  await tree(page).getByRole('button', { name: 'Expand wide parent' }).click();
  await expect(tree(page).getByRole('treeitem', { name: 'wide child 0000' })).toBeVisible({
    timeout: 30_000,
  });
  await tree(page).getByRole('button', { name: 'wide child 0003', exact: true }).click();
  // the row of the clicked button leaves the page when the tree scrolls far, and the focus stays
  await expect(tree(page).getByRole('treeitem', { selected: true })).toBeVisible();
  await page.waitForTimeout(800);
  await page.locator('.trees .tree-scroll').evaluate((e) => (e.scrollTop = 20_000));
  await expect(tree(page).getByRole('treeitem', { name: 'wide child 0003' })).toHaveCount(0);
  await expect(tree(page)).toBeFocused();
  await page.keyboard.press('End');
  // the end of a clipped level is the paging row, which is an item of the tree
  const active = await tree(page).getAttribute('aria-activedescendant');
  expect(active).toBeTruthy();
  await expect(page.locator(`[id="${active}"]`)).toHaveAttribute('role', 'treeitem');
  await expect(page.locator(`[id="${active}"]`)).toHaveAccessibleName(/Show more.*1,000 of 1,050/);
});
