// Questions on the query page against the mock (C18 Phase 1): handoff links from an agent
// open a tab with the question header and never run (A1), an update in a link is refused
// (A2), Save as example proposes a parameter for a constant entity (A5), Suggest as
// example and the admin's list (A6), and the diagnosis of an empty result.
//
// The mock runs open, as an admin. A `mock-principal` cookie signs in as another
// principal, who reads every dataset and administers none.

import { expect, test, type Page } from '@playwright/test';
import { encodeHandoff, type Handoff } from '../../src/lib/handoff';
import { selectAll } from '../phone';

const PREFIXES = `PREFIX ex: <http://example.org/ontology#>
PREFIX res: <http://example.org/resource/>
`;
const TEAM_QUERY = `${PREFIXES}SELECT ?person WHERE { ?person ex:memberOf res:payments }`;

const link = (h: Omit<Handoff, 'dataset'>, ds = 'org') =>
  `/ui/query?ds=${ds}#ask=${encodeHandoff({ dataset: ds, ...h })}`;

async function openLink(page: Page, h: Omit<Handoff, 'dataset'>) {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'org'));
  await page.goto(link(h));
}

/** Signs in as `name` for the mock (null: back to the open admin). */
async function principal(page: Page, name: string | null) {
  await page.context().clearCookies();
  if (name)
    await page
      .context()
      .addCookies([{ name: 'mock-principal', value: name, domain: '127.0.0.1', path: '/' }]);
}

test('a handoff link opens a tab with the question and does not run it (A1)', async ({ page }) => {
  const runs: string[] = [];
  page.on('request', (r) => {
    if (/\/org(@[^/]+)?\/sparql(\?|$)/.test(new URL(r.url()).pathname + '?')) runs.push(r.url());
  });
  await openLink(page, {
    query: TEAM_QUERY,
    question: 'Who is on the payments team?',
    explanation: 'Finds the members of the payments team.',
    assumptions: ['"payments team" = res:payments'],
  });
  const header = page.getByRole('region', { name: 'Question' });
  await expect(header).toContainText('Who is on the payments team?');
  await expect(header).toContainText('Finds the members of the payments team.');
  await expect(header).toContainText('"payments team" = res:payments');
  const terms = header.getByRole('list', { name: 'Terms used' });
  await expect(terms).toContainText('ex:memberOf');
  await expect(terms).toContainText('"member of"');
  await expect(terms).toContainText('property · 3 triples');
  await expect(terms).toContainText('res:payments');
  await expect(terms).toContainText('"Payments team"');
  await expect(terms).toContainText('entity · ex:Team');
  await expect(header).toContainText('no issues · estimated 2 rows');
  // the fragment is read once and dropped
  expect(page.url()).not.toContain('#ask=');
  await expect(page.getByRole('tab', { name: 'Who is on the payments team?' })).toBeVisible();
  await expect(page.locator('.cm-content')).toContainText('ex:memberOf res:payments');
  await expect(page.getByText('Run a query to see results here.')).toBeVisible();
  expect(runs).toEqual([]);

  // an edit marks the query as edited
  await page.locator('.cm-content').click();
  await page.keyboard.press('Control+End');
  await page.keyboard.insertText('\n# narrowed');
  await expect(header.getByText('edited', { exact: true })).toBeVisible();

  // the question stays with the tab after a reload, and is in the Asked list
  await page.reload();
  await expect(page.getByRole('region', { name: 'Question' })).toContainText(
    'Who is on the payments team?',
    { timeout: 20_000 },
  );
  await page.getByRole('button', { name: 'Saved' }).click();
  const asked = page.getByRole('group', { name: 'Asked' });
  await expect(
    asked.getByRole('menuitem', { name: /Who is on the payments team\?/ }),
  ).toBeVisible();
  await asked.getByRole('menuitem', { name: /Who is on the payments team\?/ }).click();
  await expect(page.getByRole('tab', { name: 'Who is on the payments team?' })).toHaveCount(2);
});

test('a link with an update is refused and leaves the editor empty (A2)', async ({ page }) => {
  await openLink(page, {
    query: 'INSERT DATA { <urn:a> <urn:b> <urn:c> }',
    question: 'Add a triple',
  });
  await expect(page.getByRole('alert')).toContainText(
    'This link contains an update. Links can only open queries.',
  );
  await expect(page.getByRole('tab', { name: 'Refused link' })).toHaveAttribute(
    'aria-selected',
    'true',
  );
  await expect(page.locator('.cm-content')).not.toContainText('INSERT');
  // an empty editor shows its placeholder
  await expect(page.locator('.cm-placeholder')).toBeVisible();
  await expect(page.getByRole('region', { name: 'Question' })).toHaveCount(0);
});

test('Save as example proposes the team parameter (A5)', async ({ page }) => {
  await openLink(page, {
    query: TEAM_QUERY,
    question: 'Who is on the payments team?',
    explanation: 'Finds the members of the payments team.',
  });
  const header = page.getByRole('region', { name: 'Question' });
  await expect(header).toContainText('no issues');
  await header.getByRole('button', { name: 'Save as example' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toContainText('Save as example');
  await expect(dialog.getByRole('textbox').first()).toHaveValue('who-is-on-the-payments-team');
  await expect(dialog.getByRole('textbox').nth(1)).toHaveValue(
    'Finds the members of the payments team.',
  );
  await expect(dialog.getByRole('textbox').nth(2)).toHaveValue('Who is on the payments team?');
  const proposals = dialog.getByLabel('Proposed parameters');
  await expect(proposals.getByRole('checkbox', { name: '?team' })).toBeChecked();
  await expect(proposals).toContainText('res:payments');
  await expect(proposals).toContainText('ex:Team');
  await dialog.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText(/Saved who-is-on-the-payments-team \(version 1\)/)).toBeVisible();

  const stored = await (
    await page.request.get('/$/queries/org/who-is-on-the-payments-team')
  ).json();
  expect(stored.questions).toEqual(['Who is on the payments team?']);
  expect(stored.description).toBe('Finds the members of the payments team.');
  expect(stored.parameters.team).toMatchObject({
    type: 'iri',
    default: 'http://example.org/resource/payments',
  });
  expect(stored.query).toContain('?person ex:memberOf ?team');
  expect(stored.query).not.toMatch(/ex:memberOf res:payments/);
});

test('an unticked proposal keeps its constant', async ({ page }) => {
  await openLink(page, { query: TEAM_QUERY, question: 'Who is in payments, fixed?' });
  const header = page.getByRole('region', { name: 'Question' });
  await expect(header).toContainText('no issues');
  await header.getByRole('button', { name: 'Save as example' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('Proposed parameters').getByRole('checkbox', { name: '?team' }).uncheck();
  await dialog.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText(/Saved who-is-in-payments-fixed/)).toBeVisible();
  const stored = await (await page.request.get('/$/queries/org/who-is-in-payments-fixed')).json();
  expect(stored.parameters).toBeUndefined();
  expect(stored.query).toContain('ex:memberOf res:payments');
});

test('Suggest as example, then an admin promotes or dismisses (A6)', async ({ page }) => {
  await principal(page, 'ana');
  await openLink(page, { query: TEAM_QUERY, question: 'Who works in payments?' });
  const header = page.getByRole('region', { name: 'Question' });
  await expect(header.getByRole('button', { name: 'Save as example' })).toHaveCount(0);
  await header.getByRole('button', { name: 'Suggest as example' }).click();
  await expect(page.getByText('Suggested as an example')).toBeVisible();
  // ana does not see the list, and the server refuses it to her
  await page.getByRole('button', { name: 'Saved' }).click();
  await expect(page.getByRole('group', { name: 'Suggestions' })).toHaveCount(0);
  expect((await page.request.get('/$/queries/org/suggestions')).status()).toBe(403);
  await page.keyboard.press('Escape');

  // a second suggestion, to dismiss
  const second = await page.request.post('/$/queries/org/suggestions', {
    data: { question: 'Which teams exist?', query: `${PREFIXES}SELECT ?t { ?t a ex:Team }` },
  });
  expect(second.status()).toBe(201);
  // an update is never a suggestion
  const update = await page.request.post('/$/queries/org/suggestions', {
    data: { question: 'x', query: 'INSERT DATA { <urn:a> <urn:b> <urn:c> }' },
  });
  expect((await update.json()).code).toBe('not-a-query');

  await principal(page, null);
  await page.goto('/ui/query');
  await page.getByRole('button', { name: 'Saved' }).click();
  const list = page.getByRole('group', { name: 'Suggestions' });
  await expect(list).toContainText('Who works in payments?');
  await expect(list).toContainText('by ana');
  await list.getByRole('button', { name: 'Dismiss' }).first().click();
  await expect(page.getByText('Dismissed the suggestion')).toBeVisible();
  await expect(list).not.toContainText('Which teams exist?');

  await list.getByRole('button', { name: 'Promote' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByRole('textbox').first()).toHaveValue('who-works-in-payments');
  await expect(
    dialog.getByLabel('Proposed parameters').getByRole('checkbox', { name: '?team' }),
  ).toBeChecked();
  await dialog.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText(/Saved who-works-in-payments \(version 1\)/)).toBeVisible();
  const stored = await (await page.request.get('/$/queries/org/who-works-in-payments')).json();
  expect(stored.questions).toEqual(['Who works in payments?']);
  const left = await (await page.request.get('/$/queries/org/suggestions')).json();
  expect(left.suggestions).toEqual([]);
});

test('an empty result says what matched nothing', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'org'));
  await page.goto('/ui/query');
  await page.locator('.cm-content').click();
  await selectAll(page);
  await page.keyboard.insertText(
    `PREFIX foaf: <http://xmlns.com/foaf/0.1/>\n${PREFIXES}SELECT ?p WHERE { ?p a ex:Person ; foaf:name "Ana Lima" }`,
  );
  await page.getByRole('button', { name: /^Run (Ctrl|⌘)/ }).click();
  const why = page.getByRole('note', { name: 'Why the result is empty' });
  await expect(why).toContainText('No data you can read matched.');
  await expect(why).toContainText('?p foaf:name "Ana Lima"');
  await expect(why).toContainText('Not in the data you can read');
  await expect(why).toContainText('only with a language tag');
});
