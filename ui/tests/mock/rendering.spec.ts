import { expect, test, type Page } from '@playwright/test';
const UNI = 'http://example.org/uni#';
async function preferences(page: Page, mode: 'label' | 'prefixed' | 'iri', languages = 'fr, en') {
  await page.getByRole('button', { name: 'Rendering preferences', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Rendering preferences' });
  await dialog.getByRole('combobox', { name: 'Show entities by' }).selectOption(mode);
  await dialog.getByRole('textbox', { name: 'Language order' }).fill(languages);
  await dialog.getByRole('button', { name: 'Apply', exact: true }).click();
}
test('one preference names the browser, editor and query results and survives reload', async ({
  page,
}) => {
  await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Person')}`);
  await expect(page.locator('h1.term-title')).toHaveText('Person');
  await preferences(page, 'label');
  await expect(page.locator('h1.term-title')).toHaveText('Personne');
  await expect(page.locator('.lede')).toContainText('Un être humain');
  await preferences(page, 'prefixed');
  await expect(page.locator('h1.term-title')).toHaveText('uni:Person');
  await page.getByRole('combobox', { name: /^Search/ }).fill('pers');
  await expect(page.getByRole('option', { name: /^uni:Person\b/ }).first()).toBeVisible();
  await page.keyboard.press('Escape');
  await preferences(page, 'iri');
  await expect(page.locator('h1.term-title')).toHaveText(UNI + 'Person');
  await page.reload();
  await expect(page.locator('h1.term-title')).toHaveText(UNI + 'Person');
  await preferences(page, 'label');
  await page.goto(`/ui/workbench/ontology?ds=uni&iri=${encodeURIComponent(UNI + 'Person')}`);
  await expect(page.getByRole('heading', { name: 'Personne', exact: true }).first()).toBeVisible();
  await preferences(page, 'prefixed');
  await expect(
    page.getByRole('heading', { name: 'uni:Person', exact: true }).first(),
  ).toBeVisible();
  const labelReads: string[] = [];
  page.on('request', (r) => {
    if (r.postData()?.startsWith('SELECT DISTINCT ?s ?p ?l WHERE')) labelReads.push(r.url());
  });
  await page.goto('/ui/query?ds=uni');
  const editor = page.locator('.cm-content').first();
  await editor.click();
  await page.keyboard.press('Control+a');
  await page.keyboard.type(`SELECT ?entity WHERE { VALUES ?entity { <${UNI}Person> } }`);
  await page.getByRole('button', { name: /^Run (Ctrl|⌘)/ }).click();
  const table = page.getByRole('grid');
  await expect(table.getByRole('button', { name: 'uni:Person', exact: true })).toBeVisible();
  expect(labelReads).toHaveLength(0);
  await preferences(page, 'label');
  await expect(table.getByRole('button', { name: 'Personne', exact: true })).toBeVisible();
  expect(labelReads).toHaveLength(1);
  expect(new URL(labelReads[0]).searchParams.get('at')).toMatch(/^commit:\d+$/);
  await preferences(page, 'iri');
  await expect(table.getByRole('button', { name: UNI + 'Person', exact: true })).toBeVisible();
});
test('a phone can edit preferences and invalid language tags cannot be saved', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/ui/browse?ds=uni');
  await page.getByRole('button', { name: 'Menu', exact: true }).click();
  await page.getByRole('button', { name: 'Rendering preferences', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Rendering preferences' });
  await dialog.getByRole('textbox', { name: 'Language order' }).fill('not_a_tag');
  await expect(dialog.getByRole('button', { name: 'Apply', exact: true })).toBeDisabled();
  await dialog.getByRole('textbox', { name: 'Language order' }).fill('fr-CA, en');
  await dialog.getByRole('button', { name: 'Apply', exact: true }).click();
  await expect(dialog).toBeHidden();
  await page.getByRole('button', { name: 'Close menu', exact: true }).click();
  await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Person')}`);
  await expect(page.locator('h1.term-title')).toHaveText('Personne');
});
