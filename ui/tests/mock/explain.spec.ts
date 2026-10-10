// The explanation panel against the mock (C18 §6.6.6, A46 and A48): **Explain why** on a
// query that a timeout stopped, links that select nodes in the plan tree, hovering a node
// to highlight the sentences that cite it, and the Explanation switch of the Plan tab.

import { expect, test, type Page } from '@playwright/test';

const QUERY = `PREFIX ex: <http://example.org/ontology#>
SELECT ?p ?team WHERE {
  ?p ex:memberOf ?team .
  ?team a ex:Team
  FILTER(?team != ?p)
}`;

async function run(page: Page, query: string) {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'org'));
  await page.goto('/ui/query');
  const editor = page.locator('.cm-content');
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText(query);
  await page.getByRole('button', { name: /^Run\b/ }).click();
}

test('Explain why opens the partial plan with the budget first and links to nodes', async ({
  page,
}) => {
  await run(page, `${QUERY}\n# mock:timeout`);
  await expect(page.getByText('did not finish within its 2 s timeout')).toBeVisible();
  await page.getByRole('button', { name: 'Explain why' }).click();

  const panel = page.getByRole('complementary', { name: 'Explanation' });
  const why = panel.getByRole('region', { name: 'Why it is slow' });
  const notes = why.getByRole('listitem');
  await expect(notes.first()).toContainText('timeout stopped the query');
  await expect(notes.nth(1)).toContainText('Filter');
  await expect(notes.nth(1)).toContainText('took');
  // partial counts carry a mark that the legend explains
  await expect(page.getByText('partial, counted until the query stopped')).toBeVisible();

  // A48: a link selects its node in the tree and scrolls it into view
  await panel
    .getByRole('region', { name: 'What it asks' })
    .getByRole('button', { name: '‹Filter›' })
    .click();
  const selected = page.locator('[data-node][aria-current="true"]');
  await expect(selected).toHaveAttribute('data-node', '0.0');
  await expect(selected).toContainText('Filter');
  await expect(selected).toBeInViewport();

  // hovering the node highlights the sentences that cite it
  await selected.hover();
  await expect(panel.locator('.sentence.lit')).toContainText('Keeping those where');
  await page.mouse.move(0, 0);
  await expect(panel.locator('.sentence.lit')).toHaveCount(0);
});

test('the Plan tab explains a finished run behind the Explanation switch', async ({ page }) => {
  await run(page, QUERY);
  const results = page.getByRole('region', { name: 'Results' });
  await results.getByRole('tab', { name: 'Plan' }).click();
  const sent = page.waitForRequest((r) => r.url().endsWith('/org/sparql/explain'));
  await page.getByRole('checkbox', { name: 'Explanation' }).check();
  const body = (await sent).postDataJSON();
  expect(body.profile).toBe('given');
  expect(body.plan.operator).toBe('Project');
  const panel = page.getByRole('complementary', { name: 'Explanation' });
  await expect(panel.getByRole('region', { name: 'What it asks' })).toContainText('Finds');
  // each sentence links to nodes of the tree
  const link = panel.getByRole('button', { name: /^‹IndexScan›$/ }).first();
  await link.click();
  await expect(page.locator('[data-node][aria-current="true"]')).toContainText('IndexScan');
});
