// The query page's saved queries against the mock: save the editor's query with a typed
// parameter, then open it from the Saved menu and get its parameter form. (The mock keeps
// stored queries but does not run them; the server's own tests cover runs.)

import { expect, test } from '@playwright/test';
import { selectAll } from '../phone';

test('save a query with a parameter and open it again', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'foaf'));
  await page.goto('/ui/query');
  await page.locator('.cm-content').click();
  await selectAll(page);
  await page.keyboard.insertText('SELECT ?s WHERE { ?s ?p ?o FILTER(?o = ?wanted) } LIMIT 10');
  await page.getByRole('button', { name: 'Saved' }).click();
  await page.getByRole('menuitem', { name: /Save this query/ }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('textbox').first().fill('by-value');
  await dialog.getByRole('checkbox', { name: '?wanted' }).check();
  await dialog.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText(/Saved by-value \(version 1\)/)).toBeVisible();
  // the tab is tied to the stored query, with a field per parameter
  const params = page.getByLabel('Saved query parameters');
  await expect(params).toContainText('by-value');
  await expect(params.getByText('?wanted*')).toBeVisible();

  // a fresh tab opens it from the menu
  await page.getByRole('button', { name: 'New query tab' }).click();
  await page.getByRole('button', { name: 'Saved' }).click();
  await page.getByRole('menuitem', { name: /by-value/ }).click();
  await expect(page.getByLabel('Saved query parameters')).toContainText('by-value');
  await expect(page.locator('.cm-content')).toContainText('?wanted');
});
