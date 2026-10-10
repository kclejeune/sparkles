// The review inbox, the branch review page and the ingest settings against the mock
// (C18 §7.10, §8.9). The mock's `org` dataset has a session of unreviewed facts and an
// ingest branch with proposals, a retraction and a possible duplicate.

import { expect, test } from '@playwright/test';

const BRANCH = 'ingest.standup-2026-10-08-1';

test('the inbox selects the facts that pass, rejects and promotes', async ({ page }) => {
  await page.goto('/ui/memory?ds=org&tab=inbox');
  await expect(page.getByRole('tab', { name: /Inbox/ })).toHaveAttribute('aria-selected', 'true');
  const inbox = page.getByRole('region', { name: 'Inbox' });
  const session = inbox.getByRole('region', { name: /^Session/ });
  await expect(session).toContainText('Kai Ito');
  await expect(session).toContainText('link: 2 candidates');
  await expect(session).toContainText('no span');
  await expect(inbox.getByRole('region', { name: `Ingest ${BRANCH}` })).toBeVisible();

  // Accept all that pass selects only the fact whose span, link and guard pass
  const actions = inbox.getByRole('group', { name: 'Inbox actions' });
  await actions.getByRole('button', { name: 'Accept all that pass' }).click();
  await expect(actions).toContainText('1 selected');
  await expect(inbox.getByRole('checkbox', { name: /^Select Kai Ito/ })).toBeChecked();
  // requiring corroboration selects nothing here
  await actions.getByRole('checkbox', { name: 'require corroboration' }).check();
  await actions.getByRole('button', { name: 'Accept all that pass' }).click();
  await expect(actions).toContainText('0 selected');

  // reject the fact without a span, with a reason
  await inbox.getByRole('checkbox', { name: /^Select Ana Lima/ }).check();
  await actions.getByRole('textbox', { name: 'Reason' }).fill('no source');
  await actions.getByRole('button', { name: 'Reject selected' }).click();
  await expect(session).not.toContainText('Ana Lima');

  // promote the passing fact and land on the merge page of the review branch
  await inbox.getByRole('checkbox', { name: /^Select Kai Ito/ }).check();
  await expect(actions.getByRole('textbox', { name: 'Promote into' })).toHaveValue(
    'https://example.org/memory/consolidated',
  );
  await actions.getByRole('button', { name: 'Promote selected' }).click();
  await expect(page).toHaveURL(/\/datasets\/org\/merge\?source=review\.local\.20261010-\d+/);
});

test('the review page highlights spans, relinks, edits and rejects', async ({ page }) => {
  await page.goto(`/ui/datasets/org/review/${BRANCH}`);
  await expect(page.getByRole('heading', { level: 1 })).toContainText(BRANCH);
  await expect(page.getByRole('link', { name: 'Merge' })).toHaveAttribute(
    'href',
    new RegExp(`/datasets/org/merge\\?source=${BRANCH.replace(/\./g, '\\.')}$`),
  );

  const source = page.getByRole('region', { name: /^Source Stand-up notes/ });
  await expect(source.locator('mark')).toHaveCount(2);
  await expect(source.locator('mark').first()).toHaveText(
    'Ana moved to the payments team this week.',
  );
  const facts = page.getByRole('region', { name: 'Proposed facts' });
  await facts.getByRole('button', { name: /^Show the passage of Ana Lima/ }).click();
  await expect(source.locator('mark.focus')).toHaveText(
    'Ana moved to the payments team this week.',
  );

  await expect(page.getByRole('region', { name: 'Retracted facts' })).toContainText(
    'Platform team',
  );

  // Use existing makes the branch name the existing team
  const entities = page.getByRole('region', { name: 'New entities' });
  await entities.getByRole('button', { name: 'Use existing Payments team for Payments' }).click();
  await expect(entities).toHaveCount(0);
  await expect(facts).toContainText('Payments team');

  // Edit value fixes the date the guard refused
  const due = facts.getByRole('listitem').filter({ hasText: 'ex:dueDate' });
  await expect(due).toContainText('guard ✗');
  await due.getByRole('button', { name: 'Edit value' }).click();
  await due.getByRole('textbox', { name: 'New value' }).fill('"2026-10-14"^^xsd:date');
  await due.getByRole('button', { name: 'Save' }).click();
  await expect(due).toContainText('2026-10-14');
  await expect(due).toContainText('guard ✓');

  // Reject retracts the fact on the branch
  await due.getByRole('button', { name: 'Reject' }).click();
  await expect(facts.getByRole('listitem')).toHaveCount(1);
  await expect(page.getByText('1 fact was proposed and rejected on this branch.')).toBeVisible();
});

test('the Memory page edits the ingest settings', async ({ page }) => {
  await page.goto('/ui/memory?ds=org');
  const ingest = page.getByRole('region', { name: 'Ingest settings' });
  await expect(ingest).toContainText('people');
  await ingest.getByRole('checkbox', { name: 'Keep the text of sources' }).uncheck();
  await expect(ingest).toContainText('every fact from them needs a quote');
  await ingest.getByRole('checkbox', { name: 'Keep the text of sources' }).check();

  await ingest.getByRole('button', { name: 'New profile' }).click();
  const form = ingest.getByRole('form', { name: 'Ingest profile' });
  await form.getByRole('textbox', { name: 'Name' }).fill('projects');
  await form.getByRole('textbox', { name: 'Classes' }).fill('http://example.org/ontology#Project');
  await form.getByRole('button', { name: 'Save profile' }).click();
  await expect(ingest).toContainText('projects');
  await expect(ingest).toContainText('1 classes');
  await ingest.getByRole('button', { name: 'Remove the profile projects' }).click();
  await expect(ingest).not.toContainText('projects');
});
