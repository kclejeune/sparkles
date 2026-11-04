// The app shell against a real server (ROADMAP item 632): the top bar's search answers
// from `GET /$/ontology/{ds}/search`, the keys focus it, and on a phone it expands over
// the bar and the drawer holds the pages.

import { expect, test } from './fixtures';
import { expectNoSidewaysScroll } from '../phone';
import { DATASET, EX } from './data';

type Page = import('@playwright/test').Page;

const search = (page: Page) => page.getByRole('combobox', { name: /^Search/ });

/** Opens `url` and waits for the app to start, so that its keys answer. */
async function visit(page: Page, url: string) {
  await page.goto(url);
  await expect(page.locator('main h1')).toBeAttached();
}

test.describe('at 1440 pixels', () => {
  test.use({ viewport: { width: 1440, height: 900 } });

  test('Ctrl+K searches the entities of the open dataset', async ({ page }) => {
    await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), DATASET);
    await visit(page, '/ui/datasets');
    await page.keyboard.press('ControlOrMeta+k');
    await expect(search(page)).toBeFocused();
    await page.keyboard.type('pers');
    const hit = page
      .getByRole('listbox', { name: 'Search results' })
      .getByRole('option', { name: /Person/ });
    await expect(hit).toBeVisible();
    await expect(hit).toContainText('class');
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(
      new RegExp(`/ui/browse\\?ds=${DATASET}&iri=${encodeURIComponent(`${EX}Person`)}`),
    );
  });

  test('/ focuses the search and Escape gives the focus back', async ({ page }) => {
    await visit(page, '/ui/admin/server');
    const refresh = page.getByRole('button', { name: 'Refresh' });
    await refresh.focus();
    await page.keyboard.press('/');
    await expect(search(page)).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(refresh).toBeFocused();
  });
});

test.describe('at 390 pixels', () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

  test('the search expands over the bar and the drawer holds the pages', async ({ page }) => {
    await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), DATASET);
    await visit(page, '/ui/datasets');
    await expect(search(page)).toBeHidden();
    await page.getByRole('button', { name: 'Open search' }).click();
    await expect(search(page)).toBeFocused();
    await page.keyboard.type('pers');
    await expect(page.getByRole('option', { name: /Person/ })).toBeVisible();
    await expectNoSidewaysScroll(page, 'the expanded search');
    await page.getByRole('button', { name: 'Close search' }).click();
    await expect(page.getByRole('button', { name: 'Open search' })).toBeFocused();

    await page.getByRole('button', { name: 'Menu' }).click();
    const drawer = page.getByRole('dialog', { name: 'Menu' });
    await expect(drawer.locator('.ds-button')).toContainText(DATASET);
    await drawer.getByRole('link', { name: 'Admin', exact: true }).click();
    await expect(page).toHaveURL(/\/ui\/admin\/server$/);
    await expect(drawer).toBeHidden();
    await expectNoSidewaysScroll(page, 'the server page');
  });
});
