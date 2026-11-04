// The ontology editor's class tree on the Gene Ontology, which is larger than the editor's
// capped axiom listing: the tree shows labels, not local names such as GO_0000002. Runs
// against `node scripts/seed-design.mjs` (SPARKLES_DESIGN_URL), with `pnpm design:check`.

import { expect, test } from '@playwright/test';

const BASE = process.env.SPARKLES_DESIGN_URL ?? 'http://127.0.0.1:36320';

test('the GO class tree shows labels', async ({ page }) => {
  test.setTimeout(240_000);
  await page.goto(`${BASE}/ui/workbench/ontology?ds=go`);
  const tree = page.getByRole('tree', { name: 'Classes (asserted)' });
  await expect(tree).toBeVisible({ timeout: 120_000 });
  // the labels arrive after the tree: wait until the first rows have them
  await expect
    .poll(
      async () => {
        const names = await tree
          .getByRole('button')
          .evaluateAll((els) => els.slice(0, 40).map((e) => e.textContent?.trim() ?? ''));
        return names.filter((n) => /^GO_\d+\b/.test(n));
      },
      { timeout: 90_000 },
    )
    .toEqual([]);
  await page.getByRole('searchbox', { name: 'Filter' }).fill('mitochondrion inheritance');
  // a class with several parents shows under each of them
  await expect(
    tree.getByRole('button', { name: 'mitochondrion inheritance' }).first(),
  ).toBeVisible();
});
