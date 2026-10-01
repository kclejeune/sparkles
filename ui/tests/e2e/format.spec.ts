// The query page's Format action against the real formatter (POST /$/format).

import { expect, test } from './fixtures';
import type { Page } from '@playwright/test';

const editor = (page: Page) => page.locator('.cm-content');

async function setText(page: Page, text: string) {
  await editor(page).click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.press('Delete');
  await page.keyboard.insertText(text);
}

/** The editor's text, line by line (an empty line holds only a `<br>`). */
const textOf = async (page: Page) =>
  (await editor(page).locator('.cm-line').allInnerTexts())
    .map((l) => l.replace(/\n$/, ''))
    .join('\n');

test('Shift+Alt+F formats with the server, one undo restores, a syntax error is shown', async ({
  page,
}) => {
  await page.goto('/ui/query');
  const original = 'select ?s where { ?s ?p ?o filter(?o>1&&?o<10) } limit 5';
  await setText(page, original);
  await page.keyboard.press('Shift+Alt+F');
  await expect
    .poll(() => textOf(page))
    .toBe('SELECT ?s\nWHERE {\n  ?s ?p ?o .\n  FILTER(?o > 1 && ?o < 10)\n}\nLIMIT 5\n');

  await page.keyboard.press('ControlOrMeta+z');
  await expect.poll(() => textOf(page)).toBe(original);

  // the reference parser reports where it got furthest: here, the last line
  await setText(page, 'PREFIX ex: <http://example.org/>\nSELECT * WHERE { ?s ?p ?o ) }');
  await page.keyboard.press('Shift+Alt+F');
  await expect(page.getByText("Can't format: syntax error at line 2")).toBeVisible();
  await expect(page.locator('.cm-error-line')).toContainText('SELECT * WHERE { ?s ?p ?o ) }');
});
