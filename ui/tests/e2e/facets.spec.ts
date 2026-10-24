// Facet histograms against a real server (docs/specs/U11-linked-views.md, acceptance
// examples A3 and A5): 400 people with ages from 18 to 90, of whom every tenth has none,
// and a country each. The route's age bins sum to the people with an age and its missing
// count to those without (A3), and the query page's Apply to query writes the triple and
// FILTER whose query counts what the panel's `matching` says, leaving the rest of the text
// as it was (A5).

import type { APIRequestContext } from '@playwright/test';
import { expect, open } from './fixtures';

open.describe.configure({ mode: 'serial' });

const EX = 'http://example.org/facets/';
const NAME = 'facets-people';
const COUNTRIES = ['FR', 'DE', 'IT', 'ES', 'PT'];

function data(): string {
  let t = `PREFIX ex: <${EX}>\nINSERT DATA {\n`;
  for (let i = 0; i < 400; i++) {
    t += `  ex:p${i} a ex:Person ; ex:country "${COUNTRIES[i % 5]}"`;
    if (i % 10 !== 0) t += ` ; ex:age ${18 + ((i * 37) % 73)}`;
    t += ' .\n';
  }
  return `${t}}`;
}

async function count(request: APIRequestContext, q: string): Promise<number> {
  const r = await request.post(`/${NAME}/sparql`, {
    headers: {
      'Content-Type': 'application/sparql-query',
      Accept: 'application/sparql-results+json',
    },
    data: q,
  });
  expect(r.ok(), await r.text()).toBe(true);
  const j = await r.json();
  return Number(j.results.bindings[0].n.value);
}

open.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: NAME, dbType: 'mem' } });
  expect(r.ok() || r.status() === 409, await r.text()).toBe(true);
  const u = await request.post(`/${NAME}/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: data(),
  });
  expect(u.ok(), await u.text()).toBe(true);
});

open(
  'A3. the age bins sum to the people with an age, and missing counts the rest',
  async ({ request }) => {
    const r = await request.post(`/$/graph/${NAME}/facets`, {
      data: {
        within: { class: `${EX}Person` },
        facets: [{ property: `${EX}age` }, { property: `${EX}country` }],
      },
    });
    expect(r.ok(), await r.text()).toBe(true);
    const a = await r.json();
    expect(a.size).toBe(400);
    const age = a.facets[0];
    expect(age.kind).toBe('numeric');
    const total = age.bins.reduce((n: number, b: { count: number }) => n + b.count, 0);
    expect(total).toBe(360);
    expect(age.missing).toBe(40);
    expect(age.bins[0].from).toBeLessThanOrEqual(18);
    expect(age.bins.at(-1).to).toBeGreaterThan(90);
    for (const b of age.bins.slice(0, 3))
      expect(b.count).toBe(
        await count(
          request,
          `SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s a <${EX}Person> ; <${EX}age> ?a FILTER(?a >= ${b.from} && ?a < ${b.to}) }`,
        ),
      );
    const country = a.facets[1];
    expect(country.values).toHaveLength(5);
    expect(country.values.every((v: { count: number }) => v.count === 80)).toBe(true);
  },
);

open(
  'A5. Apply to query on the query page counts what the panel says',
  async ({ page, request }) => {
    await page.addInitScript((n) => localStorage.setItem('sparkles.dataset', n), NAME);
    await page.goto('/ui/query');
    const editor = page.locator('.cm-content');
    await editor.click();
    await page.keyboard.press('ControlOrMeta+a');
    const text = `PREFIX ex: <${EX}>\n# every person\nSELECT ?person WHERE {\n  ?person a ex:Person .\n}\n`;
    await page.keyboard.insertText(text);
    await page.getByRole('button', { name: /^Run\b/ }).click();
    const results = page.getByRole('region', { name: 'Results' });
    await expect(results.getByRole('grid')).toBeVisible();
    await results.getByRole('tab', { name: 'Facets' }).click();
    const panel = page.getByTestId('facet-panel');
    await expect(panel.getByRole('heading', { name: /400 values of \?person/ })).toBeVisible();
    const age = panel.locator(`[data-testid="facet"][data-property="${EX}age"]`);
    const country = panel.locator(`[data-testid="facet"][data-property="${EX}country"]`);
    await expect(age.locator('.bin').first()).toBeVisible();
    // ages from the third bin to the sixth, and two countries
    await age.getByRole('group', { name: /age histogram/ }).focus();
    await page.keyboard.press('Home');
    await page.keyboard.press('ArrowRight');
    await page.keyboard.press('ArrowRight');
    for (let i = 0; i < 3; i++) await page.keyboard.press('Shift+ArrowRight');
    await country.getByRole('button', { name: /^FR/ }).click();
    await country.getByRole('button', { name: /^DE/ }).click();
    const line = panel.getByTestId('facet-count');
    await expect(line).toContainText(/of 400 match/);
    const matching = Number((await line.locator('strong').innerText()).replace(/,/g, ''));
    expect(matching).toBeGreaterThan(0);
    await panel.getByRole('button', { name: 'Apply to query' }).click();
    await expect(editor).toContainText('?person ex:age ?age .');
    await expect(editor).toContainText('FILTER(?country IN ("FR", "DE"))');
    const now = await page.evaluate(() =>
      [...document.querySelectorAll('.cm-content .cm-line')].map((l) => l.textContent).join('\n'),
    );
    // the text before and after the edits is byte-identical
    expect(
      now.startsWith(
        `PREFIX ex: <${EX}>\n# every person\nSELECT ?person WHERE {\n  ?person a ex:Person .`,
      ),
    ).toBe(true);
    expect(now.trimEnd().endsWith('}')).toBe(true);
    // the applied query's distinct count equals the panel's matching
    const where = now.slice(now.indexOf('WHERE'));
    expect(
      await count(request, `PREFIX ex: <${EX}>\nSELECT (COUNT(DISTINCT ?person) AS ?n) ${where}`),
    ).toBe(matching);
    await page.getByRole('button', { name: /^Run\b/ }).click();
    await expect(results.getByRole('tab', { name: /Table/ })).toContainText(String(matching));
  },
);
