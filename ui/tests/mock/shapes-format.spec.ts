// The Format button and Shift+Alt+F of the SHACL shapes editor on the dataset page,
// against the mock's stand-in formatter (mock/server.mjs: it collapses runs of spaces,
// trims line ends, and reports an unclosed bracket as a syntax error).

import { expect, test, type Page } from '@playwright/test';

const shapes = (page: Page) => page.getByRole('textbox', { name: 'Shapes graph (Turtle)' });

/** Replace the editor's text (typed as one insertion, so brackets are not auto-closed). */
async function setText(page: Page, text: string) {
  await shapes(page).click();
  await page.keyboard.press('ControlOrMeta+A');
  await page.keyboard.press('Delete');
  await page.keyboard.insertText(text);
}

/** The editor's text, line by line (an empty line holds only a `<br>`). */
const textOf = async (page: Page) =>
  (await shapes(page).locator('.cm-line').allInnerTexts())
    .map((l) => l.replace(/\n$/, ''))
    .join('\n');

test('Shift+Alt+F formats the shapes, and one undo restores them', async ({ page }) => {
  await page.goto('/ui/datasets/foaf');
  const original = 'ex:S   a sh:NodeShape ;\n  sh:targetClass   ex:C .   ';
  await setText(page, original);
  await page.keyboard.press('Shift+Alt+F');
  await expect.poll(() => textOf(page)).toBe('ex:S a sh:NodeShape ;\n sh:targetClass ex:C .\n');

  await page.keyboard.press('ControlOrMeta+Z');
  await expect.poll(() => textOf(page)).toBe(original);
});

test('the Format button formats; a syntax error highlights its line', async ({ page }) => {
  await page.goto('/ui/datasets/foaf');
  const panel = page.locator('section.panel').filter({
    has: page.getByRole('heading', { name: 'Validate (SHACL)' }),
  });
  const format = panel.getByRole('button', { name: 'Format', exact: true });
  await expect(format).toHaveAttribute('title', 'Format (Shift+Alt+F)');
  await setText(page, 'ex:S  a  sh:NodeShape .');
  await format.click();
  await expect.poll(() => textOf(page)).toBe('ex:S a sh:NodeShape .\n');

  await setText(page, 'ex:S a sh:NodeShape ;\n  sh:property [ sh:path ex:p .');
  await format.click();
  await expect(page.getByText("Can't format: syntax error at line 2")).toBeVisible();
  await expect(panel.locator('.cm-error-line')).toHaveCount(1);
  await expect(panel.locator('.cm-error-line')).toContainText('sh:property [');
  // the text is left as it was; an edit clears the highlight
  expect(await textOf(page)).toBe('ex:S a sh:NodeShape ;\n  sh:property [ sh:path ex:p .');
  await shapes(page).focus();
  await page.keyboard.type(' ]');
  await expect(panel.locator('.cm-error-line')).toHaveCount(0);
});
