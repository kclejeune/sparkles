// The Ask bar against the mock (C18 Phase 2): preview first and run then show, the step
// indicator, a summary whose citations select rows, feedback and Try harder, an edit that
// clears the summary and Summarize again, clarification choices, graph variables,
// follow-ups, failure states, the server-side Asked list and the Explore hint.
//
// mock/assistant.mjs scripts the pipeline by words in the question. Only `org` has an
// assistant.

import { expect, test, type Page } from '@playwright/test';

async function openQuery(page: Page, mode?: 'preview' | 'run') {
  await page.addInitScript((m) => {
    localStorage.setItem('sparkles.dataset', 'org');
    localStorage.removeItem('sparkles.queryTabs');
    if (m) localStorage.setItem('sparkles.askPrefs.local', JSON.stringify({ mode: m }));
  }, mode ?? null);
  await page.goto('/ui/query?ds=org');
  await expect(page.getByRole('region', { name: 'Ask bar' })).toBeVisible({
    timeout: 20_000,
  });
}

async function ask(page: Page, question: string) {
  const bar = page.getByRole('region', { name: 'Ask bar' });
  await bar.getByRole('textbox').fill(question);
  await bar.getByRole('button', { name: 'Ask' }).click();
}

test('preview first shows the checked query without running it (A22)', async ({ page }) => {
  const runs: string[] = [];
  page.on('request', (r) => {
    if (/\/org\/sparql$/.test(new URL(r.url()).pathname)) runs.push(r.url());
  });
  await openQuery(page);
  await expect(page.getByRole('radio', { name: 'Preview first' })).toBeChecked();
  await ask(page, 'Who is in the payments team?');
  await expect(page.getByRole('tab', { name: 'Who is in the payments team?' })).toHaveAttribute(
    'aria-selected',
    'true',
  );
  const header = page.getByRole('region', { name: 'Question' });
  await expect(header).toContainText('Finds the members of the payments team.');
  await expect(page.locator('.cm-content')).toContainText('ex:memberOf res:payments');
  await expect(page.getByText('Run a query to see results here.')).toBeVisible();
  expect(runs).toEqual([]);
  // Run runs the editor's query through the normal path
  await page.getByRole('button', { name: /^Run/ }).click();
  await expect(page.getByRole('grid')).toContainText('Kai Ito');
  expect(runs.length).toBe(1);
});

test('run then show answers with rows, a cited summary and feedback (A23, A25)', async ({
  page,
}) => {
  await openQuery(page, 'run');
  await expect(page.getByRole('radio', { name: 'Run, then show' })).toBeChecked();
  await ask(page, 'Who is in the payments team?');
  const answer = page.getByRole('region', { name: 'Answer' });
  await expect(answer).toContainText('is in the answer');
  await expect(answer).toContainText(/generated · cites 2 of 2 rows/);
  const grid = page.getByRole('grid');
  await expect(grid).toContainText('Kai Ito');
  // a citation selects its row
  await answer.getByRole('button', { name: '[1]' }).click();
  await expect(grid.locator('[aria-selected="true"]')).toHaveCount(1);
  // the model that answered, and feedback
  const bar = page.getByRole('group', { name: 'Answer feedback' });
  await expect(bar).toContainText('mock · small');
  await expect(bar.getByRole('button', { name: 'Try harder' })).toBeVisible();
  await bar.getByRole('button', { name: 'Correct', exact: true }).click();
  await expect(bar).toContainText('Feedback sent: accepted');
  // the summary can be collapsed, and stays collapsed
  await answer.getByRole('button', { name: 'Collapse the summary' }).click();
  await expect(answer).toContainText('The summary is collapsed.');
  await answer.getByRole('button', { name: 'Show the summary' }).click();

  // an edit clears the summary, and Summarize again asks for a new one
  await page.locator('.cm-content').click();
  await page.keyboard.press('Control+End');
  await page.keyboard.insertText('\n# narrowed');
  await expect(answer).toHaveCount(0);
  await expect(page.getByRole('region', { name: 'Question' }).getByText('edited')).toBeVisible();
  await bar.getByRole('button', { name: 'Summarize again' }).click();
  await expect(page.getByRole('region', { name: 'Answer' })).toContainText('is in the answer');
});

test('Try harder moves to the large model and then hides (A40)', async ({ page }) => {
  await openQuery(page, 'run');
  await ask(page, 'Who works on payments?');
  const bar = page.getByRole('group', { name: 'Answer feedback' });
  await expect(bar).toContainText('mock · small');
  await bar.getByRole('button', { name: 'Try harder' }).click();
  await expect(bar).toContainText('mock · large');
  await expect(bar.getByRole('button', { name: 'Try harder' })).toHaveCount(0);
});

test('a failed check escalates the repair, and the steps show it (A36)', async ({ page }) => {
  await openQuery(page, 'run');
  await ask(page, 'Who is on payments, needing a repair?');
  const bar = page.getByRole('group', { name: 'Answer feedback' });
  await expect(bar).toContainText('mock · large');
  await expect(bar).toContainText('1 escalation');
  await expect(page.locator('.cm-content')).toContainText('ex:memberOf res:payments');
  await expect(page.locator('.cm-content')).not.toContainText('ex:Teem');
});

test('an ambiguous mention asks which one is meant (A14)', async ({ page }) => {
  await openQuery(page, 'run');
  await ask(page, 'What does Ana work on?');
  const form = page.getByRole('form', { name: 'Clarify the question' });
  await expect(form).toContainText('Which "Ana" do you mean?');
  await form.getByRole('radio', { name: /Ana Lima/ }).check();
  await form.getByRole('button', { name: 'Continue' }).click();
  await expect(form).toHaveCount(0);
  await expect(page.getByRole('grid')).toContainText('Kai Ito');
});

test('graph variables open the graph with the pickers set (A39 UI)', async ({ page }) => {
  await openQuery(page, 'run');
  await ask(page, 'How are people and teams connected?');
  await expect(page.getByRole('tab', { name: 'Graph' })).toHaveAttribute('aria-selected', 'true');
  await expect(page.getByRole('combobox').filter({ hasText: '?person' }).first()).toHaveValue(
    'person',
  );
});

test('a follow-up carries the earlier turn, and New question drops it', async ({ page }) => {
  await openQuery(page, 'run');
  await ask(page, 'Who is in the payments team?');
  await expect(page.getByRole('group', { name: 'Answer feedback' })).toBeVisible();
  const bar = page.getByRole('region', { name: 'Ask bar' });
  await expect(bar).toContainText('Follow-up of “Who is in the payments team?”');
  const sent = page.waitForRequest((r) => r.url().endsWith('/org/ask'));
  await ask(page, 'and their start dates?');
  expect((await sent).postDataJSON().context).toEqual([
    {
      question: 'Who is in the payments team?',
      query: expect.stringContaining('res:payments'),
    },
  ]);
  await expect(page.getByRole('group', { name: 'Answer feedback' })).toBeVisible();
  await bar.getByRole('button', { name: 'New question' }).click();
  await expect(bar).not.toContainText('Follow-up of');
});

test('failure states say what went wrong', async ({ page }) => {
  await openQuery(page, 'run');
  await ask(page, 'Ask the provider something');
  await expect(page.getByRole('alert')).toContainText('The model provider is not responding.');
  await ask(page, 'Spend the budget');
  await expect(page.getByRole('alert')).toContainText(
    "This dataset's question budget for today is used up.",
  );
});

test('the Asked list comes from the server history (A34)', async ({ page }) => {
  await openQuery(page, 'run');
  await ask(page, 'Who belongs to payments, for the history?');
  await expect(page.getByRole('group', { name: 'Answer feedback' })).toBeVisible();
  await page.getByRole('button', { name: 'Saved' }).click();
  const asked = page.getByRole('group', { name: 'Asked' });
  await expect(asked.getByRole('menuitem', { name: /for the history\?/ })).toBeVisible();
  await asked.getByRole('button', { name: 'Forget this question' }).first().click();
  await expect(asked.getByRole('menuitem', { name: /for the history\?/ })).toHaveCount(0);
});

test('Explore offers to ask the search text', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'org'));
  await page.goto('/ui/explore?ds=org');
  const search = page.getByRole('combobox');
  await search.fill('who leads payments');
  await page.getByRole('option', { name: /Ask a question/ }).click();
  await expect(page).toHaveURL(/\/ui\/query/);
  await expect(page.getByRole('region', { name: 'Ask bar' }).getByRole('textbox')).toHaveValue(
    'who leads payments',
  );
});

test('a dataset without an assistant has no Ask bar (A19 UI)', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'foaf'));
  await page.goto('/ui/query?ds=foaf');
  await expect(page.getByRole('region', { name: 'Results' })).toBeVisible({
    timeout: 20_000,
  });
  await expect(page.getByRole('region', { name: 'Ask bar' })).toHaveCount(0);
});
