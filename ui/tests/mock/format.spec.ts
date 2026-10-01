// The Format button and Shift+Alt+F on the query page, against the mock's stand-in
// formatter (mock/server.mjs: it collapses runs of spaces, trims line ends, and reports an
// unclosed bracket as a syntax error).

import { expect, test, type Page } from '@playwright/test';

const editor = (page: Page) => page.locator('.cm-content');

/** Replace the editor's text (typed as one insertion, so brackets are not auto-closed). */
async function setText(page: Page, text: string) {
  await editor(page).click();
  await page.keyboard.press('ControlOrMeta+A');
  await page.keyboard.press('Delete');
  await page.keyboard.insertText(text);
}

/** The editor's text, line by line (an empty line holds only a `<br>`). */
const textOf = async (page: Page) =>
  (await editor(page).locator('.cm-line').allInnerTexts())
    .map((l) => l.replace(/\n$/, ''))
    .join('\n');

test('Shift+Alt+F formats the query, and one undo restores it', async ({ page }) => {
  await page.goto('/ui/query');
  await setText(page, 'SELECT   *  WHERE {\n  ?s   ?p ?o }   ');
  await page.keyboard.press('Shift+Alt+F');
  await expect.poll(() => textOf(page)).toBe('SELECT * WHERE {\n ?s ?p ?o }\n');

  await page.keyboard.press('ControlOrMeta+Z');
  await expect.poll(() => textOf(page)).toBe('SELECT   *  WHERE {\n  ?s   ?p ?o }   ');
});

test('the Format button formats; a syntax error highlights its line', async ({ page }) => {
  await page.goto('/ui/query');
  await setText(page, 'ASK   {}');
  const format = page.getByRole('button', { name: 'Format', exact: true });
  await expect(format).toHaveAttribute('title', 'Format (Shift+Alt+F)');
  await format.click();
  await expect.poll(() => textOf(page)).toBe('ASK {}\n');

  await setText(page, 'ASK {\n  ?s ?p ?o .\n  FILTER(?o > 1))\n}');
  await page.keyboard.press('Shift+Alt+F');
  await expect(page.getByText("Can't format: syntax error at line 3")).toBeVisible();
  await expect(page.locator('.cm-error-line')).toHaveCount(1);
  await expect(page.locator('.cm-error-line')).toContainText('FILTER(?o > 1))');
  // the text is left as it was
  expect(await textOf(page)).toBe('ASK {\n  ?s ?p ?o .\n  FILTER(?o > 1))\n}');
});

test('format on run is off by default, remembered, and formats before running', async ({
  page,
}) => {
  await page.goto('/ui/query');
  await page.getByRole('button', { name: 'Format options' }).click();
  const box = page.getByRole('checkbox', { name: 'Format on run' });
  await expect(box).not.toBeChecked();
  await box.check();
  await page.reload();
  await page.getByRole('button', { name: 'Format options' }).click();
  await expect(page.getByRole('checkbox', { name: 'Format on run' })).toBeChecked();

  await setText(page, 'SELECT   *  WHERE { ?s ?p ?o }   LIMIT 1');
  await page.getByRole('button', { name: /^Run/ }).click();
  await expect.poll(() => textOf(page)).toBe('SELECT * WHERE { ?s ?p ?o } LIMIT 1\n');
});
