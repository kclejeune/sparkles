import { expect, test } from '@playwright/test';
import { expectNoSidewaysScroll, phone } from '../phone';

for (const width of [1024, 1440, 390]) {
  test(`at ${width} px the lens options leave Fit and layout controls reachable`, async ({
    browser,
    request,
  }) => {
    const context = await browser.newContext({
      ...(width === 390 ? phone : {}),
      viewport: { width, height: 900 },
    });
    try {
      const page = await context.newPage();
      const rows = await request.post('/foaf/sparql', {
        headers: { 'Content-Type': 'application/sparql-query' },
        data: 'SELECT ?p (COUNT(?o) AS ?n) WHERE { ?p <http://xmlns.com/foaf/0.1/knows> ?o } GROUP BY ?p ORDER BY DESC(?n) ?p LIMIT 1',
      });
      expect(rows.ok()).toBe(true);
      const person = (await rows.json()).results.bindings[0].p.value;
      await page.goto(`/ui/explore?ds=foaf&iri=${encodeURIComponent(person)}`);
      await expect(page.locator('.wrap[data-graph]')).toBeVisible();
      await page.getByRole('button', { name: /^Lenses/ }).click();
      const menu = page.getByRole('dialog', { name: 'Lenses' });
      await menu.getByRole('combobox', { name: 'Size' }).selectOption('pageRank');
      await menu.getByRole('combobox', { name: 'Colour' }).selectOption('leiden');
      await expect(page.getByTestId('lens-status-colour')).toContainText('communit');
      await page.evaluate(() => document.fonts.ready);
      for (const name of ['Fit to view', 'Re-run layout']) {
        const button = page.getByRole('button', { name, exact: true });
        // Trial actionability checks keep the menu open while checking both targets.
        await button.click({ trial: true, timeout: 5000 });
        const target = (await button.boundingBox())!;
        const panel = (await menu.boundingBox())!;
        expect(
          panel.x >= target.x + target.width ||
            panel.x + panel.width <= target.x ||
            panel.y >= target.y + target.height ||
            panel.y + panel.height <= target.y,
          `${name} must stay outside the open lens menu`,
        ).toBe(true);
      }
      await expectNoSidewaysScroll(page, 'Explore with algorithm lens options');
      const capture = test.info().outputPath('lens-menu.png');
      await page.screenshot({ path: capture });
      await test.info().attach('open-lens-menu', {
        path: capture,
        contentType: 'image/png',
      });
      // A resize can wrap the toolbar even after the menu has opened.
      await page.setViewportSize({ width: width === 390 ? 420 : width - 100, height: 844 });
      await page.getByRole('button', { name: 'Fit to view', exact: true }).click();
      await expect(menu).toBeHidden();
    } finally {
      await context.close();
    }
  });
}
