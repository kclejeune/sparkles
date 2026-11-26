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
  await task.getByRole('button', { name: 'Continue' }).click();
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

test('when server ingestion is off, panel states so and asks before storing without facts', async ({
  page,
}) => {
  await page.goto('/ui/datasets/foaf');
  const panel = page.getByRole('region', { name: 'Ingest' });
  const status = panel.getByTestId('server-ingest-status');
  await expect(status).toContainText(
    'Server ingestion is off: uploaded documents are stored as sources without facts',
  );
  // Uploading a document asks for confirmation and sends extract=false
  let extractField: string | null = null;
  await page.route('**/$/ingest/foaf', async (route) => {
    if (route.request().method() === 'POST') {
      const body = route.request().postData() ?? '';
      const m = body.match(/name="extract"\r?\n\r?\n([^\r\n]+)/);
      extractField = m ? m[1] : null;
    }
    await route.continue();
  });
  await panel.getByLabel('Document to ingest').setInputFiles({
    name: 'memo.md',
    mimeType: 'text/markdown',
    buffer: Buffer.from('# Memo\n\nJust notes.\n'),
  });
  await panel.getByRole('button', { name: 'Ingest', exact: true }).click();
  const dialog = page.getByRole('dialog', {
    name: 'Store this document as a source without facts?',
  });
  await expect(dialog).toBeVisible();
  await dialog.getByRole('button', { name: 'Store without facts' }).click();
  const task = panel.getByRole('group', { name: 'Ingestion task' });
  await expect(task).toContainText('Registered as a source');
  expect(extractField).toBe('false');

  // Admin can turn server ingestion on from the panel after seeing what changes and which providers receive documents
  await status.getByRole('button', { name: 'Turn on server ingestion' }).click();
  const enableDialog = page.getByRole('dialog', { name: 'Turn on server ingestion' });
  await expect(enableDialog).toContainText('enabled');
  await expect(enableDialog).toContainText('ingest');
  await expect(enableDialog).toContainText('send');
  await expect(enableDialog).toContainText('mock/small');
  await expect(enableDialog).toContainText('query result rows');
  await enableDialog.getByRole('button', { name: 'Turn on server ingestion' }).click();
  await expect(status).toContainText(
    'Server ingestion is on: documents go to mock/small, mock/large',
  );

  // Turning it off patches ingest=false and links back to Settings
  await status.getByRole('button', { name: 'Turn off server ingestion' }).click();
  await expect(status).toContainText('Server ingestion is off');
  await expect(status.getByRole('link', { name: 'Open ingest in Settings' })).toHaveAttribute(
    'href',
    /\/datasets\/foaf\?tab=settings#setting-assistant-ingest$/,
  );
});

test('a locked send setting explains why server ingestion is off and links to Settings', async ({
  page,
}) => {
  await page.goto('/ui/datasets/scratch');
  const panel = page.getByRole('region', { name: 'Ingest' });
  const status = panel.getByTestId('server-ingest-status');
  await expect(status).toContainText('Server ingestion is off');
  await expect(status).toContainText("locked in the server's settings file");
  await expect(status.getByRole('button', { name: 'Turn on server ingestion' })).toHaveCount(0);
  await expect(status.getByRole('link', { name: 'Open send in Settings' })).toHaveAttribute(
    'href',
    /\/datasets\/scratch\?tab=settings#setting-assistant-send$/,
  );
});
