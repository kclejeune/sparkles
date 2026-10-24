// Facet histograms and Use in query (docs/specs/U11-linked-views.md, Phase 2) against the
// mock's `world` dataset (mock/world.mjs): 41 people with an age, a date joined, a city and
// an employer, whose facets the mock counts with the server's queries (mock/facets.mjs).
// The tests run A4 and the keyboard and phone parts of A10 in Explore, Apply to query from
// Explore and from the query page with the count of the applied query, Use in query from a
// selection made on a result's map, the selection shared by a result's views, and the empty
// state.

import { devices, expect as baseExpect, test, type Locator, type Page } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

test.describe.configure({ timeout: 90_000 });
test.use({ actionTimeout: 15_000 });

const expect = baseExpect.configure({ timeout: 15_000 });

const WORLD = 'http://example.org/world/';
const AMIRA = encodeURIComponent(`${WORLD}amira-haddad`);
const PREFIXES = `PREFIX ex: <${WORLD}>\n`;

async function openFacets(page: Page, iri = AMIRA) {
  await page.goto(`/ui/explore?ds=world&iri=${iri}`);
  await expect(page.getByText(/\d+ nodes, \d+ edges/)).toBeVisible();
  // the class of the selected entity is read with its properties
  await expect(page.getByRole('complementary', { name: 'Resource details' })).toContainText(
    'Person',
  );
  await page.keyboard.press('Shift+F');
  const panel = page.getByTestId('facet-panel');
  await expect(panel).toBeVisible();
  await expect(panel.getByTestId('facet')).toHaveCount(3);
  await expect(panel.getByRole('heading', { name: /Facets · 41 instances of/ })).toBeVisible();
  return panel;
}

const facet = (panel: Locator, local: string) =>
  panel.locator(`[data-testid="facet"][data-property="${WORLD}${local}"]`);

/** The counts of a bar list, by label. */
async function bars(f: Locator): Promise<Record<string, number>> {
  const out: Record<string, number> = {};
  for (const b of await f.locator('button.val').all()) {
    const label = (await b.locator('.vlabel').innerText()).trim();
    out[label] = Number((await b.locator('.num').innerText()).replace(/,/g, ''));
  }
  return out;
}

const binTitles = (f: Locator) =>
  f.locator('.bin').evaluateAll((els) => els.map((e) => e.getAttribute('title')));

test('A4 and A10. Brushing ages with the keyboard filters the other facets, dims the graph and keeps its own counts', async ({
  page,
}) => {
  const panel = await openFacets(page);
  const age = facet(panel, 'age');
  const works = facet(panel, 'worksFor');
  await expect(age.locator('.bin').first()).toBeVisible();
  await expect(works.locator('button.val').first()).toBeVisible();
  const before = await binTitles(age);
  const allWorks = await bars(works);
  expect(Object.values(allWorks).reduce((a, b) => a + b, 0)).toBe(41);

  // the histogram's keys: Home, two bins right, then Shift with arrows brushes three more
  const hist = age.getByRole('group', { name: /age histogram/ });
  await hist.focus();
  await page.keyboard.press('Home');
  await page.keyboard.press('ArrowRight');
  await page.keyboard.press('ArrowRight');
  await page.keyboard.press('Shift+ArrowRight');
  await page.keyboard.press('Shift+ArrowRight');
  await page.keyboard.press('Shift+ArrowRight');
  const count = panel.getByTestId('facet-count');
  await expect(count).toContainText(/of 41 match/);
  const matching = Number((await count.locator('strong').innerText()).replace(/,/g, ''));
  expect(matching).toBeGreaterThan(0);
  expect(matching).toBeLessThan(41);
  // A4: the employer facet counts the brushed people, the age facet keeps its own counts
  await expect
    .poll(async () => Object.values(await bars(works)).reduce((a, b) => a + b, 0))
    .toBe(matching);
  expect(await binTitles(age)).toEqual(before);
  await expect(age.locator('.bin.in')).toHaveCount(4);
  // the handles say what the brush is
  const from = age.getByRole('slider', { name: /age from/ });
  await expect(from).toHaveAttribute('aria-valuenow', '2');
  await expect(from).toHaveAttribute('aria-valuetext', new RegExp(`${matching} entities`));
  // the drawn people outside the brush are dimmed through the shared selection
  const bar = page.getByRole('region', { name: 'Selection' });
  await expect(bar).toContainText('selected by the facets');
  await expect(page.locator('.wrap[data-muted]')).not.toHaveAttribute('data-muted', '0');
  // a handle moves with the arrows and widens the brush by a bin
  await from.focus();
  await page.keyboard.press('ArrowLeft');
  await expect(age.locator('.bin.in')).toHaveCount(5);
  // Escape on the histogram clears its brush, and the selection with it
  await hist.focus();
  await page.keyboard.press('Escape');
  await expect(age.locator('.bin.in')).toHaveCount(0);
  await expect(count).not.toContainText('match');
  await expect(bar).toHaveCount(0);
  await expect.poll(async () => bars(works)).toEqual(allWorks);
});

test('a bar list brushes by values, the table lists the bins, and Apply to query opens the query', async ({
  page,
}) => {
  const panel = await openFacets(page);
  const works = facet(panel, 'worksFor');
  const meridian = works.getByRole('button', { name: /Meridian Logistics/ });
  await meridian.click();
  await expect(meridian).toHaveAttribute('aria-pressed', 'true');
  const count = panel.getByTestId('facet-count');
  await expect(count).toContainText('24 of 41 match');
  // Space toggles a value as well
  const northgate = works.getByRole('button', { name: /Northgate Rail/ });
  await northgate.focus();
  await page.keyboard.press('Space');
  await expect(count).toContainText('28 of 41 match');
  await page.keyboard.press('Space');
  await expect(count).toContainText('24 of 41 match');
  // the table alternative of a histogram
  const age = facet(panel, 'age');
  await age.getByRole('button', { name: 'Table' }).click();
  const table = age.getByRole('table');
  await expect(table.getByRole('row')).not.toHaveCount(0);
  const counted = await table
    .locator('tbody td.num')
    .evaluateAll((els) => els.reduce((a, e) => a + Number(e.textContent!.replace(/,/g, '')), 0));
  expect(counted).toBe(24);
  // Apply to query opens a query for the people of the brush
  await panel.getByRole('button', { name: 'Apply to query' }).click();
  await expect(page).toHaveURL(/\/ui\/query/);
  const editor = page.locator('.cm-content');
  await expect(editor).toContainText('?entity a ');
  await expect(editor).toContainText('FILTER(?worksFor IN (<http://example.org/world/meridian>))');
  await page.getByRole('button', { name: /^Run\b/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('tab', { name: /Table/ })).toContainText('24');
});

test('A5 on the query page: the facets of a variable, Apply to query, and the count of the applied query', async ({
  page,
}) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'world'));
  await page.goto('/ui/query');
  const editor = page.locator('.cm-content');
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  const text = `${PREFIXES}SELECT ?person ?city WHERE {\n  ?person a ex:Person ;\n    ex:livesIn ?city .\n}\nORDER BY ?person\n`;
  await page.keyboard.insertText(text);
  await page.getByRole('button', { name: /^Run\b/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('grid')).toBeVisible();
  await results.getByRole('tab', { name: 'Facets' }).click();
  const panel = page.getByTestId('facet-panel');
  await expect(panel.getByRole('combobox', { name: /variable whose values/ })).toHaveValue(
    'person',
  );
  await expect(panel.getByRole('heading', { name: /41 values of \?person/ })).toBeVisible();
  const works = facet(panel, 'worksFor');
  await works.getByRole('button', { name: /Meridian Logistics/ }).click();
  const age = facet(panel, 'age');
  await age.getByRole('group', { name: /age histogram/ }).focus();
  await page.keyboard.press('Home');
  await page.keyboard.press('Shift+ArrowRight');
  await page.keyboard.press('Shift+ArrowRight');
  await page.keyboard.press('Shift+ArrowRight');
  await page.keyboard.press('Shift+ArrowRight');
  const count = panel.getByTestId('facet-count');
  await expect(count).toContainText(/of 41 match/);
  await page.waitForTimeout(400);
  const matching = Number((await count.locator('strong').innerText()).replace(/,/g, ''));
  // the people of the result the brushes keep become the selection, which the graph dims by
  const bar = page.getByRole('region', { name: 'Selection' });
  await expect(bar).toContainText('selected by the facets');
  await panel.getByRole('button', { name: 'Apply to query' }).click();
  await expect(editor).toContainText('?person ex:worksFor ?worksFor .');
  await expect(editor).toContainText('?person ex:age ?age .');
  await expect(editor).toContainText('FILTER(?age >= ');
  await expect(editor).toContainText('FILTER(?worksFor IN (<http://example.org/world/meridian>))');
  // the rest of the text is as it was
  const now = await page.evaluate(() =>
    [...document.querySelectorAll('.cm-content .cm-line')].map((l) => l.textContent).join('\n'),
  );
  expect(now.startsWith(`${PREFIXES}SELECT ?person ?city WHERE {`)).toBe(true);
  expect(now.trimEnd().endsWith('ORDER BY ?person')).toBe(true);
  // the selection stays when the Graph tab opens, and the graph dims what it does not hold
  await results.getByRole('tab', { name: 'Graph' }).click();
  await expect(bar).toContainText('selected by the facets');
  await expect(page.locator('.wrap[data-muted]')).not.toHaveAttribute('data-muted', '0');
  // the applied query counts what the panel said
  await page.getByRole('button', { name: /^Run\b/ }).click();
  await expect(results.getByRole('tab', { name: /Table/ })).toContainText(String(matching));
});

test('a selection on a result map reaches its graph and becomes a VALUES block with Use in query', async ({
  page,
}) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'world'));
  await page.goto('/ui/query');
  const editor = page.locator('.cm-content');
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText(
    `${PREFIXES}SELECT ?person ?city WHERE { ?person a ex:Person ; ex:livesIn ?city }`,
  );
  await page.getByRole('button', { name: /^Run\b/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('grid')).toBeVisible();
  await results.getByRole('tab', { name: 'Map' }).click();
  const arcmap = page.locator('.arcmap');
  await expect(arcmap).toHaveAttribute('data-loading', 'false');
  await arcmap.locator('details.list summary').click();
  await arcmap.getByRole('list', { name: 'Markers in view' }).getByRole('button').first().click();
  const bar = page.getByRole('region', { name: 'Selection' });
  await expect(bar).toContainText('selected by the map');
  const size = Number(await bar.getAttribute('data-size'));
  // the selection stays when the Graph tab opens, and the graph dims what it does not hold
  await results.getByRole('tab', { name: 'Graph' }).click();
  await expect(bar).toContainText(`${size} selected by the map`);
  // Use in query writes the selected entities as VALUES on the chosen variable
  const as = bar.getByRole('combobox', { name: /variable of the VALUES block/ });
  // the variable follows the column that holds the selection until the person picks one,
  // and follows it again once the selection is cleared
  const first = await as.inputValue();
  const other = first === 'person' ? 'city' : 'person';
  await as.selectOption(other);
  await bar.getByRole('button', { name: 'Clear' }).click();
  await expect(bar).toHaveCount(0);
  await results.getByRole('tab', { name: 'Map' }).click();
  await arcmap.locator('details.list summary').click();
  await arcmap.getByRole('list', { name: 'Markers in view' }).getByRole('button').first().click();
  await expect(bar).toContainText('selected by the map');
  await expect(as).toHaveValue(first);
  await results.getByRole('tab', { name: 'Graph' }).click();
  await as.selectOption('person');
  await bar.getByRole('button', { name: 'Use in query' }).click();
  await expect(editor).toContainText('VALUES ?person {');
  await expect(page.getByText(/Added VALUES \?person with/)).toBeVisible();
  // a new run starts without a selection
  await page.getByRole('button', { name: /^Run\b/ }).click();
  await expect(results.getByRole('grid').or(results.locator('.gcanvas'))).toBeVisible();
  await expect(bar).toHaveCount(0);
});

test('a set without literal properties says so', async ({ page }) => {
  await page.goto(`/ui/explore?ds=world&iri=${encodeURIComponent(`${WORLD}dubai`)}`);
  await expect(page.getByText(/\d+ nodes, \d+ edges/)).toBeVisible();
  await expect(page.getByRole('complementary', { name: 'Resource details' })).toContainText('City');
  await page.getByRole('button', { name: 'Facets' }).click();
  const panel = page.getByTestId('facet-panel');
  // the set picker offers the class and the drawn entities; the cities have only labels
  // and geometries, which make no facets
  const sets = panel.locator('.fsub select');
  const labels = await sets.locator('option').allInnerTexts();
  await sets.selectOption({ label: labels.find((o) => o.startsWith('Instances of'))! });
  await expect(panel).toContainText('These entities have no literal properties to facet.');
  await page.keyboard.press('Shift+F');
  await expect(panel).toHaveCount(0);
});

test.describe('at 390 px', () => {
  const { defaultBrowserType: _ignored, ...iphone } = devices['iPhone 12'];
  test.use(iphone);
  test('A10. the facets are brushed with taps, without sideways scrolling', async ({ page }) => {
    await page.goto(`/ui/explore?ds=world&iri=${AMIRA}&facets=1`);
    const panel = page.getByTestId('facet-panel');
    await expect(panel.getByTestId('facet')).toHaveCount(3);
    const works = facet(panel, 'worksFor');
    const meridian = works.getByRole('button', { name: /Meridian Logistics/ });
    await meridian.scrollIntoViewIfNeeded();
    const box = (await meridian.boundingBox())!;
    expect(box.height).toBeGreaterThanOrEqual(44);
    await meridian.tap();
    await expect(panel.getByTestId('facet-count')).toContainText('24 of 41 match');
    // a handle is a 44 px target that a drag moves
    const age = facet(panel, 'age');
    const handle = age.getByRole('slider', { name: /age to/ });
    await handle.scrollIntoViewIfNeeded();
    const hb = (await handle.boundingBox())!;
    expect(hb.width).toBeGreaterThanOrEqual(22);
    const track = (await age.locator('.track').boundingBox())!;
    await page.mouse.move(hb.x + hb.width / 2, hb.y + hb.height / 2);
    await page.mouse.down();
    await page.mouse.move(track.x + track.width * 0.5, hb.y + hb.height / 2, { steps: 5 });
    await page.mouse.up();
    await expect(age.locator('.bin.in').first()).toBeVisible();
    await expect(panel.getByTestId('facet-count')).toContainText(/of 41 match/);
    await expectNoSidewaysScroll(page, 'Explore with the facets');
  });
});
