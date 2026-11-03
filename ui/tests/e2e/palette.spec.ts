// The command palette against a real server (ROADMAP item 631): the page's commands with
// the real formatter and query engine, entities through the server's ranking, a saved
// query from `/$/queries`, the dataset's branches, and the phone.

import { expect, server, test } from './fixtures';
import { DATASET } from './data';

type Page = import('@playwright/test').Page;

const search = (page: Page) => page.locator('.scope-search input[role="combobox"]');
const list = (page: Page) => page.getByRole('listbox', { name: 'Search results' });
const titles = (page: Page) => list(page).getByRole('option').locator('.title');
const option = (page: Page, name: string) =>
  list(page).getByRole('option', { name: new RegExp(`^${name}`) });

async function visit(page: Page, url: string) {
  await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), DATASET);
  await page.goto(url);
  await expect(page.locator('main h1')).toBeAttached();
}

async function palette(page: Page, text = '') {
  await page.keyboard.press('ControlOrMeta+k');
  await expect(search(page)).toBeFocused();
  if (text) await page.keyboard.type(text);
}

test.describe('at 1440 pixels', () => {
  test.use({ viewport: { width: 1440, height: 900 } });

  test('formats and runs the query, and the focus goes back to the editor', async ({ page }) => {
    await visit(page, '/ui/query');
    const editor = page.locator('.cm-content');
    await editor.click();
    await page.keyboard.press('ControlOrMeta+a');
    await page.keyboard.press('Delete');
    await page.keyboard.insertText('select ?s where { ?s ?p ?o } limit 2');
    await palette(page, '>form');
    await expect(titles(page)).toHaveText(['Format query', 'Open dataset settings']);
    await page.keyboard.press('Enter');
    await expect(editor).toContainText('SELECT ?s');
    await expect(editor).toBeFocused();
    await palette(page, '>run');
    await page.keyboard.press('Enter');
    await expect(page.getByRole('tab', { name: /^Table/ })).toBeVisible();
  });

  test('`@` finds entities with the server ranking', async ({ page }) => {
    await visit(page, '/ui/admin/server');
    await palette(page, '@pers');
    await expect(list(page).getByRole('group', { name: `Entities in ${DATASET}` })).toBeVisible();
    await expect(option(page, 'Person')).toBeVisible();
    await expect(list(page).getByRole('group', { name: 'Go to' })).toHaveCount(0);
  });

  test('`?` opens a stored query', async ({ page, request }) => {
    const put = await request.put(`${server.url}/$/queries/${DATASET}/palette-people`, {
      headers: { Authorization: `Bearer ${server.token}` },
      data: { query: 'SELECT ?s WHERE { ?s ?p ?o } LIMIT 4', description: 'Some subjects' },
    });
    expect(put.ok(), await put.text()).toBe(true);
    await visit(page, '/ui/datasets');
    await palette(page, '?palette');
    await expect(option(page, 'palette-people')).toContainText('Some subjects');
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/ui\/query/);
    await expect(page.locator('.cm-content')).toContainText('LIMIT 4');
  });

  test('`#` lists the datasets and the page branches', async ({ page }) => {
    await visit(page, '/ui/ontology?ds=' + DATASET);
    await palette(page, '#');
    await expect(list(page).getByRole('group', { name: 'Datasets' })).toBeVisible();
    await expect(list(page).getByRole('group', { name: `Branches of ${DATASET}` })).toBeVisible();
    await expect(option(page, 'main')).toBeVisible();
  });

  test('`/` filters the ontology tree; Ctrl+K stays the palette', async ({ page }) => {
    await visit(page, '/ui/ontology?ds=' + DATASET);
    const filter = page.getByRole('searchbox', { name: 'Filter' });
    await expect(filter).toBeVisible();
    await page.locator('main h1').click();
    await page.keyboard.press('/');
    await expect(filter).toBeFocused();
    await page.locator('main h1').click();
    await palette(page);
    await expect(list(page).getByRole('group', { name: 'This page' })).toBeVisible();
  });
});

test.describe('at 390 pixels', () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

  test('the commands are a tap away', async ({ page }) => {
    await visit(page, '/ui/query');
    await page.getByRole('button', { name: 'Open search' }).tap();
    await expect(list(page).getByRole('group', { name: 'This page' })).toBeVisible();
    await page
      .getByRole('toolbar', { name: 'Show' })
      .getByRole('button', { name: /^Commands/ })
      .tap();
    await option(page, 'Theme: dark').tap();
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  });
});
