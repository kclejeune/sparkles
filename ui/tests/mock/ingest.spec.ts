// The dataset page's Ingest section against the mock (C18 §7.10, Phase 4): a document
// becomes proposals on a review branch, a large estimate waits for a confirmation, a
// scanned page is refused until the readable pages are ingested, a preview waits for an
// approval, and a table gets a mapping draft. The review page shows each fact's page.

import { expect, test, type Page } from '@playwright/test';

async function ingest(page: Page, name: string, mimeType: string, mode?: string) {
  const panel = page.getByRole('region', { name: 'Ingest' });
  await panel.getByLabel('Document to ingest').setInputFiles({
    name,
    mimeType,
    buffer: Buffer.from(mimeType === 'application/pdf' ? '%PDF-1.4\n' : '# Notes\n\nAna.\n'),
  });
  if (mode) await panel.getByLabel('Review mode').selectOption(mode);
  await panel.getByRole('button', { name: 'Ingest', exact: true }).click();
  return panel.getByRole('group', { name: 'Ingestion task' });
}

test('a document becomes proposals on a review branch', async ({ page }) => {
  await page.goto('/ui/datasets/org');
  const task = await ingest(page, 'notes.md', 'text/markdown');
  await expect(task).toContainText('Done');
  await expect(task).toContainText('2 facts proposed, 1 linked and 1 new entities');
  await task.getByRole('link', { name: 'Review proposals' }).click();
  await expect(page).toHaveURL(/\/datasets\/org\/review\/ingest\.standup-2026-10-08-1$/);
});

test('a large estimate waits for a confirmation', async ({ page }) => {
  await page.goto('/ui/datasets/org');
  const task = await ingest(page, 'big-notes.md', 'text/markdown');
  await expect(task).toContainText('Waiting for your confirmation');
  await expect(task).toContainText('about 270,000 tokens');
  await task.getByRole('button', { name: 'Go on' }).click();
  await expect(task).toContainText('2 facts proposed');
});

test('a scanned page needs OCR, and the readable pages show their page', async ({ page }) => {
  await page.goto('/ui/datasets/org');
  const task = await ingest(page, 'scanned-report.pdf', 'application/pdf');
  await expect(task.getByRole('alert')).toContainText('needs-ocr');
  await expect(task.getByRole('alert')).toContainText('Page 3: scanned');
  await task.getByRole('button', { name: 'Ingest the readable pages' }).click();
  await expect(task).toContainText('page 3 left out');
  await task.getByRole('link', { name: 'Review proposals' }).click();
  await expect(page).toHaveURL(/\/datasets\/org\/review\/ingest\.report-\d+$/);
  const source = page.getByRole('region', { name: /^Source report\.pdf/ });
  await expect(source).toContainText('Page 3 could not be read without OCR');
  const facts = page.getByRole('region', { name: 'Proposed facts' });
  await expect(facts).toContainText('page 2');
});

test('a preview waits for its approval', async ({ page }) => {
  await page.goto('/ui/datasets/org');
  const task = await ingest(page, 'notes.md', 'text/markdown', 'preview');
  await expect(task).toContainText('Waiting for your approval');
  await task.getByRole('button', { name: 'Approve and write to main' }).click();
  await expect(task).toContainText('Written to main');
});

test('a table gets a mapping draft', async ({ page }) => {
  await page.goto('/ui/datasets/org');
  const task = await ingest(page, 'people.csv', 'text/csv');
  await expect(task).toContainText('Mapping draft (by the model): 3 rows become 6 triples');
  await expect(task.getByLabel('Mapping')).toHaveValue(/tableSchema/);
});
