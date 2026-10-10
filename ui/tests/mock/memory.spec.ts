// The memory browser against the mock (C18 §8.7): the Memory tab of a resource on the
// Explore page, with citations, status badges, conflicts and history, and the Memory
// page's sections. The mock's `org` dataset has agent notes with C17 reifiers.

import { expect, test } from '@playwright/test';

const ANA = 'http://example.org/resource/ana';

test('the Memory tab lists facts with citations, badges and history', async ({ page }) => {
  await page.goto(`/ui/explore?ds=org&iri=${encodeURIComponent(ANA)}&view=memory`);
  await expect(page.getByRole('tab', { name: 'Memory' })).toHaveAttribute('aria-selected', 'true');
  const memory = page.getByLabel('Memory', { exact: true });
  const facts = memory.getByRole('region', { name: /Facts about/ });
  await expect(facts).toContainText('ex:memberOf');
  await expect(facts).toContainText('res:payments');
  await expect(facts).toContainText('ana@example.org');
  await expect(facts.getByText('unreviewed').first()).toBeVisible();
  await expect(facts.getByText('reviewed', { exact: true }).first()).toBeVisible();
  // a conflict shows both values with their citations
  const conflict = facts.getByRole('note').filter({ hasText: 'Conflict' });
  await expect(conflict).toContainText('2025-11-14');
  await expect(conflict).toContainText('2025-11-17');

  // a citation expands to its graph, source, quote, time, principal and confidence
  const row = facts.getByRole('row').filter({ hasText: 'ex:memberOf' });
  await row.getByRole('button', { name: /^\[\d+\]$/ }).click();
  const citation = memory.getByRole('definition').filter({ hasText: 'quote' });
  await expect(citation).toContainText('https://example.org/notes/2026-10-08');
  await expect(citation).toContainText('Ana moved to the payments team this week.');
  await expect(citation).toContainText('2026-10-08T09:14:03Z');
  await expect(citation).toContainText('agent-7');
  await expect(citation).toContainText('0.90');

  // the history shows the superseded team and what replaced it
  const history = memory.getByRole('list', { name: 'History' });
  await expect(history).toContainText('res:platform');
  await expect(history).toContainText(/superseded on 2026-10-08 by \[\d+\]/);
  await memory.getByRole('checkbox', { name: 'show superseded' }).check();
  await expect(facts.getByText('superseded')).toBeVisible();

  // a graph filter keeps that graph's facts
  await memory.getByRole('listbox', { name: 'Graphs' }).selectOption('https://example.org/hr');
  await expect(facts).not.toContainText('res:payments');
  await expect(facts).toContainText('ana@example.org');
});

test('the Memory page shows agents, sources, activity, branches and settings', async ({ page }) => {
  await page.goto('/ui/memory?ds=org');
  await expect(page.getByRole('link', { name: 'Memory' })).toHaveAttribute('aria-current', 'page');
  const agents = page.getByRole('region', { name: 'Agents' });
  await expect(agents).toContainText('agent-7');
  await expect(agents).toContainText('agent-9');
  await expect(agents).toContainText('https://example.org/notes/');
  await expect(agents.getByRole('row', { name: /agent-7/ })).toContainText('3');
  await expect(page.getByRole('region', { name: 'Sources' })).toContainText(
    'https://example.org/notes/2026-10-08',
  );
  const activity = page.getByRole('region', { name: 'Recent activity' });
  await expect(activity.getByRole('listitem').first()).toContainText(
    '"Stand-up notes of 2026-10-08"',
  );
  await expect(page.getByRole('region', { name: 'Review branches' })).toContainText('scratch-s2');
  await expect(page.getByRole('region', { name: 'Memory settings' })).toContainText(
    'https://example.org/notes/*',
  );

  const search = page.getByRole('region', { name: 'Search memory' });
  await search.getByRole('textbox', { name: 'Search memory' }).fill('payments team');
  await search.getByRole('button', { name: 'Recall' }).click();
  await expect(search).toContainText('"Payments team"');
  await expect(search.getByRole('region', { name: /Facts about/ }).first()).toBeVisible();
});
