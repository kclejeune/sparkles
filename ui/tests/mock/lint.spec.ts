// The query editor's lint against the mock (whose stand-in knows one rule,
// `unused-prefix`): a finding is underlined and counted while the query is edited, and
// "Fix lint problems" applies the safe fixes as one change.

import { expect, test } from '@playwright/test';
import { selectAll } from '../phone';

test('lint findings are marked while typing and fixed from the Format menu', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'foaf'));
  await page.goto('/ui/query');
  await page.locator('.cm-content').click();
  await selectAll(page);
  await page.keyboard.insertText(
    'PREFIX ex: <http://example.org/>\nPREFIX foaf: <http://xmlns.com/foaf/0.1/>\nSELECT ?n WHERE { ?s foaf:name ?n }',
  );
  const mark = page.locator('.cm-lint-warning');
  await expect(mark).toHaveCount(1);
  await expect(mark).toHaveAttribute('title', /the prefix ex: is declared but never used/);
  await expect(page.getByRole('status').filter({ hasText: '1 warning' })).toBeVisible();

  await page.getByRole('button', { name: 'Format options' }).click();
  await page.getByRole('button', { name: 'Fix lint problems' }).click();
  await expect(page.getByText('Fixed 1 lint problem')).toBeVisible();
  await expect(page.locator('.cm-content')).not.toContainText('PREFIX ex:');
  await expect(page.locator('.cm-content')).toContainText('PREFIX foaf:');
  await expect(mark).toHaveCount(0);

  // turned off: no marks
  await page.getByRole('button', { name: 'Format options' }).click();
  await page.getByRole('checkbox', { name: 'Lint while typing' }).uncheck();
  await page.locator('.cm-content').click();
  await selectAll(page);
  await page.keyboard.insertText('PREFIX ex: <http://example.org/>\nASK {}');
  await page.waitForTimeout(800);
  await expect(mark).toHaveCount(0);
});
