// The README's screenshots of branches, of the agent pages and of the Explore overview
// grouped by the GraphRAG communities, in the dark theme, against
// the mock backend that playwright.screenshots-mock.config.ts starts. The `org` dataset
// gets a branch whose changes clash with main, so that the dataset page shows branches and
// the commit graph and the merge page shows conflicts. The Ask bar runs the scripted
// pipeline of mock/assistant.mjs, and a handoff link stands in for an agent's
// `share_query`. The maps of relations, the communities, the Pattern pane, the diff graph
// and the path finder on a query's graph use the `world` showcase dataset of
// mock/world.mjs. The model configuration form uses the declared configuration of
// mock/models.mjs. Run with `mise run docs:screenshots`.

import { test as base, expect, type APIRequestContext, type Page } from '@playwright/test';
import { encodeHandoff } from '../../src/lib/handoff';
import { OUT, settle, shoot } from '../shoot';
import { worldFlights } from '../../mock/world.mjs';
import { join } from 'node:path';

const DATASET = 'org';
const PREFIXES = `PREFIX ex: <http://example.org/ontology#>
PREFIX res: <http://example.org/resource/>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
`;

const test = base.extend({
  page: async ({ page }, use) => {
    await page.addInitScript((ds) => {
      localStorage.setItem('sparkles.theme', 'dark');
      localStorage.setItem('sparkles.dataset', ds);
      localStorage.removeItem('sparkles.queryTabs');
    }, DATASET);
    await use(page);
  },
});

test.describe.configure({ mode: 'serial' });

const update = (request: APIRequestContext, target: string, sparql: string, message: string) =>
  request
    .post(`/${target}/update`, {
      headers: {
        'Content-Type': 'application/sparql-update',
        'Sparkles-Commit-Message': message,
      },
      data: PREFIXES + sparql,
    })
    .then((r) => expect(r.ok(), `update on ${target}`).toBe(true));

const history = (page: Page) =>
  page.locator('section.panel', {
    has: page.getByRole('heading', { name: 'History', exact: true }),
  });

test.beforeAll(async ({ request }) => {
  // a branch that was merged back, and one whose changes clash with main's
  const branch = (name: string, note: string) =>
    request
      .post(`/$/branches/${DATASET}`, { data: { name, note } })
      .then((r) => expect(r.status()).toBe(201));
  await branch('onboarding', 'new starters for November');
  await update(
    request,
    `${DATASET}@onboarding`,
    'INSERT DATA { res:noor a ex:Person ; foaf:name "Noor Haddad"@en ; ex:memberOf res:platform }',
    'Add Noor to the platform team',
  );
  await update(
    request,
    `${DATASET}@onboarding`,
    'INSERT DATA { GRAPH <https://example.org/hr> { res:noor ex:startDate "2026-11-02"^^xsd:date } }',
    "Record Noor's start date",
  );
  const merged = await request.post(`/$/merge/${DATASET}`, { data: { source: 'onboarding' } });
  expect(merged.ok()).toBe(true);

  await branch('reorg-2027', 'team changes for next year');
  await update(
    request,
    `${DATASET}@reorg-2027`,
    'DELETE DATA { res:lea ex:memberOf res:checkout } ; INSERT DATA { res:lea ex:memberOf res:payments }',
    'Move Lea to the payments team',
  );
  await update(
    request,
    `${DATASET}@reorg-2027`,
    'INSERT DATA { res:risk a ex:Team ; rdfs:label "Risk team"@en . res:kai ex:memberOf res:risk }',
    'Start a risk team with Kai',
  );
  await update(
    request,
    `${DATASET}@reorg-2027`,
    'DELETE DATA { GRAPH <https://example.org/hr> { res:kai ex:startDate "2026-03-02"^^xsd:date } } ; INSERT DATA { GRAPH <https://example.org/hr> { res:kai ex:startDate "2026-03-16"^^xsd:date } }',
    "Correct Kai's start date",
  );
  await update(
    request,
    DATASET,
    'DELETE DATA { res:lea ex:memberOf res:checkout } ; INSERT DATA { res:lea ex:memberOf res:platform }',
    'Lea joins the platform team',
  );
  await update(
    request,
    DATASET,
    'DELETE DATA { GRAPH <https://example.org/hr> { res:kai ex:startDate "2026-03-02"^^xsd:date } } ; INSERT DATA { GRAPH <https://example.org/hr> { res:kai ex:startDate "2026-03-09"^^xsd:date } }',
    "Fix Kai's start date from the contract",
  );
});

test('branches', async ({ page }) => {
  await page.goto(`/ui/datasets/${DATASET}`);
  const panel = page.getByRole('region', { name: 'Branches' });
  await expect(panel.getByRole('row').filter({ hasText: 'reorg-2027' })).toBeVisible({
    timeout: 30_000,
  });
  const toggle = history(page).getByRole('button', { name: 'Graph', exact: true });
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-pressed', 'true');
  await expect(history(page)).toContainText('commits of');
  await panel.evaluate((el) => el.scrollIntoView({ block: 'start' }));
  await page.mouse.wheel(0, -16);
  await shoot(page, 'branches');
});

test('merge', async ({ page }) => {
  await page.goto(`/ui/datasets/${DATASET}/merge?source=reorg-2027&target=main`);
  const conflicts = page.getByRole('region', { name: 'Conflicts' });
  await expect(conflicts).toContainText('2 conflicts still open', { timeout: 30_000 });
  // one conflict resolved, so the page shows a choice and the changes it makes
  await page
    .getByRole('radiogroup', { name: /^Take for \S*lea\b/ })
    .getByRole('radio', { name: 'Theirs' })
    .click();
  await expect(conflicts).toContainText('1 conflict still open');
  await shoot(page, 'merge');
});

async function openQuery(page: Page) {
  await page.addInitScript(() =>
    localStorage.setItem('sparkles.askPrefs.local', JSON.stringify({ mode: 'run' })),
  );
  await page.goto(`/ui/query?ds=${DATASET}`);
  await expect(page.getByRole('region', { name: 'Ask bar' })).toBeVisible({ timeout: 30_000 });
}

async function ask(page: Page, question: string) {
  const bar = page.getByRole('region', { name: 'Ask bar' });
  await bar.getByRole('textbox').fill(question);
  await bar.getByRole('button', { name: 'Ask' }).click();
}

test('ask-steps', async ({ page }) => {
  // the mock holds this ask before its summary, so the step indicator stays up
  await page.route(`**/${DATASET}/ask`, (route) =>
    route.continue({
      postData: JSON.stringify({ ...route.request().postDataJSON(), hold: 'summary' }),
    }),
  );
  await openQuery(page);
  await ask(page, 'Who is in the payments team?');
  const bar = page.getByRole('region', { name: 'Ask bar' });
  await expect(bar.getByRole('status')).toContainText('Summarizing');
  await expect(page.getByRole('grid')).toContainText('Kai Ito');
  await shoot(page, 'ask-steps', { spinners: 1 });
});

test('ask-answer', async ({ page }) => {
  await openQuery(page);
  await ask(page, 'Who is in the payments team?');
  const answer = page.getByRole('region', { name: 'Answer' });
  await expect(answer).toContainText(/cites \d+ of \d+ rows/);
  await answer.getByRole('button', { name: '[1]' }).click();
  await expect(page.getByRole('grid').locator('[aria-selected="true"]')).toHaveCount(1);
  await shoot(page, 'ask-answer');
});

test('handoff', async ({ page }) => {
  const handoff = encodeHandoff({
    dataset: DATASET,
    question: 'Who is on the payments team, and since when?',
    query: `${PREFIXES.split('\n').slice(0, 3).join('\n')}
SELECT ?person ?name ?since WHERE {
  ?person ex:memberOf res:payments ;
          foaf:name ?name .
  OPTIONAL { GRAPH <https://example.org/hr> { ?person ex:startDate ?since } }
}
ORDER BY ?since`,
    explanation:
      'Finds the members of the payments team with their names, and their start dates from the HR graph where it has one.',
    assumptions: ['"payments team" = res:payments', '"since" = the start date in the HR graph'],
  });
  await page.goto(`/ui/query?ds=${DATASET}#ask=${handoff}`);
  const header = page.getByRole('region', { name: 'Question' });
  await expect(header.getByRole('list', { name: 'Terms used' })).toContainText('ex:startDate', {
    timeout: 30_000,
  });
  await shoot(page, 'handoff');
});

test('memory', async ({ page }) => {
  await page.goto(`/ui/memory?ds=${DATASET}`);
  await expect(page.getByRole('region', { name: 'Agents' })).toContainText('agent-7', {
    timeout: 30_000,
  });
  const search = page.getByRole('region', { name: 'Search memory' });
  await search.getByRole('textbox', { name: 'Search memory' }).fill('payments team');
  await search.getByRole('button', { name: 'Recall' }).click();
  await expect(search.getByRole('region', { name: /Facts about/ }).first()).toBeVisible();
  await shoot(page, 'memory');
});

test('time-slider', async ({ page }) => {
  // the last two commits of main moved Lea to the platform team and corrected Kai's date
  const query = `PREFIX ex: <http://example.org/ontology#>
PREFIX res: <http://example.org/resource/>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
SELECT ?name ?team WHERE {
  VALUES ?person { res:lea res:kai res:noor }
  ?person foaf:name ?name ; ex:memberOf ?team .
} ORDER BY ?name`;
  await page.addInitScript(
    ([q]) =>
      localStorage.setItem(
        'sparkles.queryTabs',
        JSON.stringify({ tabs: [{ id: 't1', title: 'Team members', query: q }], active: 't1' }),
      ),
    [query],
  );
  await page.goto(`/ui/query?ds=${DATASET}`);
  await page.getByRole('button', { name: /^Run/ }).click();
  await expect(page.getByRole('grid')).toBeVisible({ timeout: 30_000 });
  await page.getByRole('button', { name: 'History', exact: true }).click();
  const slider = page.getByRole('region', { name: 'History time slider' });
  const right = slider.getByRole('slider', { name: /^(Commit|Compared to)$/ });
  await right.focus();
  await page.keyboard.press('c');
  const left = slider.getByRole('slider', { name: 'Compared from' });
  await left.focus();
  await page.keyboard.press('ArrowLeft');
  await expect(page.getByRole('grid').getByRole('gridcell', { name: 'removed' })).not.toHaveCount(
    0,
  );
  await shoot(page, 'time-slider');
});

test('time-slider-graph', async ({ page }) => {
  await page.goto(
    `/ui/explore?ds=${DATASET}&iri=${encodeURIComponent('http://example.org/resource/lea')}`,
  );
  await expect(page.locator('.wrap[data-graph]')).toBeVisible({ timeout: 30_000 });
  await page.getByRole('button', { name: 'Graph history' }).click();
  const bar = page.getByRole('region', { name: 'Graph history time slider' });
  await bar.getByRole('slider', { name: /^(Commit|Compared to)$/ }).focus();
  await page.keyboard.press('c');
  await bar.getByRole('slider', { name: 'Compared from' }).focus();
  await page.keyboard.press('ArrowLeft');
  await expect(bar.getByRole('status')).toContainText('added', { timeout: 30_000 });
  await shoot(page, 'time-slider-graph');
});

// the mock's `world` dataset (mock/world.mjs): a freight company with its head office in
// Dubai, its offices, partners and people around the world, and routes between cities
const WORLD = 'http://example.org/world/';

test('map-arcs', async ({ page }) => {
  await page.goto(`/ui/explore?ds=world&iri=${encodeURIComponent(`${WORLD}meridian`)}`);
  await expect(page.getByText(/\d+ nodes, \d+ edges/)).toBeVisible({ timeout: 30_000 });
  await page
    .getByRole('group', { name: 'Show the graph as' })
    .getByRole('button', { name: 'Map' })
    .click();
  const arcmap = page.locator('.arcmap');
  await expect(arcmap).toHaveAttribute('data-loading', 'false', { timeout: 30_000 });
  const region = page.getByRole('region', { name: 'Graph map' });
  await expect(region).toHaveAttribute('data-state', 'ready');
  await shoot(page, 'map-arcs');
});

test('result-arcs', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'world'));
  const query = `PREFIX ex: <${WORLD}>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?label ?from ?to WHERE {
  ?route a ex:Route ; rdfs:label ?label ; ex:fromGeom ?from ; ex:toGeom ?to .
} ORDER BY ?label`;
  await page.addInitScript(
    ([q]) =>
      localStorage.setItem(
        'sparkles.queryTabs',
        JSON.stringify({ tabs: [{ id: 't1', title: 'Routes', query: q }], active: 't1' }),
      ),
    [query],
  );
  await page.goto('/ui/query?ds=world');
  // a short editor, so the map has room
  const top = (await page.locator('.cm-editor').boundingBox())!.y;
  const split = (await page.getByRole('separator', { name: 'Resize editor' }).boundingBox())!;
  await page.mouse.move(split.x + split.width / 2, split.y + split.height / 2);
  await page.mouse.down();
  await page.mouse.move(split.x + split.width / 2, top + 150, { steps: 5 });
  await page.mouse.up();
  await page.getByRole('button', { name: /^Run/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('grid')).toBeVisible({ timeout: 30_000 });
  await results.getByRole('tab', { name: 'Map' }).click();
  await results.getByRole('button', { name: 'Arcs' }).click();
  await results.getByLabel('Column that labels the arcs').selectOption('label');
  const arcmap = page.locator('.arcmap');
  await expect(arcmap).toHaveAttribute('data-loading', 'false', { timeout: 30_000 });
  await expect(page.getByRole('region', { name: 'Result arcs' })).toHaveAttribute(
    'data-state',
    'ready',
  );
  await shoot(page, 'result-arcs');
});

test('explore-facets', async ({ page }) => {
  // the facets of the people of `world` beside the graph of the chief executive, with a
  // brush on their ages and their employer, which dims the drawn people outside it
  await page.goto(
    `/ui/explore?ds=world&iri=${encodeURIComponent(`${WORLD}amira-haddad`)}&facets=1`,
  );
  const panel = page.getByTestId('facet-panel');
  await expect(panel.getByTestId('facet')).toHaveCount(3, { timeout: 30_000 });
  const facet = (local: string) =>
    panel.locator(`[data-testid="facet"][data-property="${WORLD}${local}"]`);
  await expect(facet('age').locator('.bin').first()).toBeVisible();
  await facet('worksFor')
    .getByRole('button', { name: /Meridian Logistics/ })
    .click();
  await facet('age')
    .getByRole('group', { name: /age histogram/ })
    .focus();
  await page.keyboard.press('Home');
  for (let i = 0; i < 3; i++) await page.keyboard.press('ArrowRight');
  for (let i = 0; i < 6; i++) await page.keyboard.press('Shift+ArrowRight');
  await expect(panel.getByTestId('facet-count')).toContainText(/of 41 match/);
  await expect(page.getByRole('region', { name: 'Selection' })).toContainText(
    'selected by the facets',
  );
  await shoot(page, 'explore-facets');
});

test('embedding-map', async ({ page, request }) => {
  // the 6,000 invented shipments of `world` projected by the server and coloured by cargo
  // class, with a lasso around one lobe of the largest class in the shared selection
  await page.goto('/ui/embedding?ds=world&index=shipments');
  const map = page.locator('.emap[data-ready="true"]');
  await expect(map).toHaveAttribute('data-points', '6000', { timeout: 30_000 });
  await expect(page.getByTestId('progress')).toHaveCount(0);
  await page.waitForTimeout(500);
  const r = await request.get('/$/vector/world/shipments/projection');
  const points: { x: number; y: number; class?: number }[] = (await r.json()).points;
  const [ax, bx, ay, by] = (await map.getAttribute('data-transform'))!.split(',').map(Number);
  const box = (await map.boundingBox())!;
  // the densest cell of class 0 at a coarse grid, in viewport pixels
  const cells = new Map<string, { n: number; x: number; y: number }>();
  for (const p of points) {
    if (p.class !== 0) continue;
    const vx = box.x + ax * p.x + bx;
    const vy = box.y + ay * p.y + by;
    const key = `${Math.floor(vx / 80)},${Math.floor(vy / 80)}`;
    const c = cells.get(key) ?? { n: 0, x: 0, y: 0 };
    c.n++;
    c.x += vx;
    c.y += vy;
    cells.set(key, c);
  }
  const best = [...cells.values()].sort((a, b) => b.n - a.n)[0];
  const cx = best.x / best.n;
  const cy = best.y / best.n;
  await page.getByRole('radio', { name: 'Lasso' }).click();
  await page.mouse.move(cx + 70, cy);
  await page.mouse.down();
  for (let k = 1; k <= 24; k++) {
    const a = (k / 24) * Math.PI * 2;
    await page.mouse.move(
      cx + 70 * Math.cos(a) * (1 + 0.15 * Math.sin(3 * a)),
      cy + 55 * Math.sin(a),
    );
  }
  await page.mouse.up();
  await expect(page.getByRole('region', { name: 'Selection' })).toContainText(
    'selected by the embedding map',
  );
  await shoot(page, 'embedding-map');
});

test('query-facets', async ({ page }) => {
  // the query page's facets of a variable, with the brushes Apply to query writes
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'world'));
  const query = `PREFIX ex: <${WORLD}>
SELECT ?person ?city WHERE {
  ?person a ex:Person ;
    ex:livesIn ?city .
}
ORDER BY ?person`;
  await page.addInitScript(
    ([q]) =>
      localStorage.setItem(
        'sparkles.queryTabs',
        JSON.stringify({ tabs: [{ id: 't1', title: 'People', query: q }], active: 't1' }),
      ),
    [query],
  );
  await page.goto('/ui/query?ds=world');
  await page.getByRole('button', { name: /^Run/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('grid')).toBeVisible({ timeout: 30_000 });
  await results.getByRole('tab', { name: 'Facets' }).click();
  const panel = page.getByTestId('facet-panel');
  await expect(panel.getByTestId('facet')).toHaveCount(3, { timeout: 30_000 });
  const facet = (local: string) =>
    panel.locator(`[data-testid="facet"][data-property="${WORLD}${local}"]`);
  await expect(facet('joined').locator('.bin').first()).toBeVisible();
  await facet('joined')
    .getByRole('group', { name: /joined histogram/ })
    .focus();
  await page.keyboard.press('End');
  for (let i = 0; i < 4; i++) await page.keyboard.press('Shift+ArrowLeft');
  await expect(panel.getByTestId('facet-count')).toContainText(/of 41 match/);
  await panel.getByRole('button', { name: 'Apply to query' }).click();
  await expect(page.locator('.cm-content')).toContainText('FILTER(YEAR(?joined)');
  // the toast that confirms the edit would cover the facets
  await page.getByRole('button', { name: 'Dismiss' }).click();
  await page.waitForTimeout(400);
  await shoot(page, 'query-facets');
});

test('explore-communities', async ({ page }) => {
  // the overview of `world` grouped by its GraphRAG communities (mock/world.mjs), with the
  // report of the head office, which links every region, beside it
  await page.goto(`/ui/explore?ds=world&overview=communities`);
  const ov = page.getByTestId('overview');
  await expect(ov).toHaveAttribute('data-mode', 'communities');
  await ov.getByRole('button', { name: 'List' }).click();
  const row = (name: RegExp) =>
    ov.locator('tbody tr').filter({ has: page.getByRole('button', { name }) });
  await row(/^Gulf head office/)
    .getByRole('button', { name: /^Gulf head office/ })
    .click();
  const side = page.getByTestId('community-side');
  await expect(side.getByRole('heading', { name: 'Gulf head office' })).toBeVisible();
  await ov.getByRole('button', { name: 'List' }).click();
  await expect(ov.locator('canvas').first()).toBeVisible();
  await shoot(page, 'explore-communities');
});

test('query-paths', async ({ page, request }) => {
  // the freight routes of `world` as a result graph on the query page, with the three
  // shortest ways from Vancouver to Auckland by distance, which the reifiers of the flights
  // carry in ex:km, drawn over it
  await update(request, 'world', `INSERT DATA {\n${worldFlights()}\n}`, 'Add the flight distances');
  const query = `PREFIX ex: <${WORLD}>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>

# The flights between the cities, named by their labels
CONSTRUCT { ?a ex:flight ?b . ?a rdfs:label ?from . ?b rdfs:label ?to }
WHERE { ?a ex:flight ?b ; rdfs:label ?from . ?b rdfs:label ?to }`;
  await page.addInitScript(
    ([q]) => {
      localStorage.setItem(
        'sparkles.queryTabs',
        JSON.stringify({ tabs: [{ id: 't1', title: 'Flights', query: q }], active: 't1' }),
      );
      localStorage.setItem('sparkles.dataset', 'world');
    },
    [query],
  );
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.goto('/ui/query?ds=world');
  // a short editor, so the graph has room
  const top = (await page.locator('.cm-editor').boundingBox())!.y;
  const split = (await page.getByRole('separator', { name: 'Resize editor' }).boundingBox())!;
  await page.mouse.move(split.x + split.width / 2, split.y + split.height / 2);
  await page.mouse.down();
  await page.mouse.move(split.x + split.width / 2, top + 150, { steps: 5 });
  await page.mouse.up();
  await page.getByRole('button', { name: /^Run/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await results.getByRole('tab', { name: 'Graph' }).click();
  await expect(results.locator('.wrap[data-graph]')).toBeAttached({ timeout: 30_000 });
  await results.getByRole('button', { name: 'Paths', exact: true }).click();
  const panel = page.getByRole('region', { name: 'Paths' });
  for (const [end, text, name] of [
    ['From', 'Vanc', /^Vancouver\b/],
    ['To', 'Auck', /^Auckland\b/],
  ] as const) {
    await panel.getByRole('combobox', { name: end }).fill(text);
    await panel.getByRole('option', { name }).first().click();
  }
  await panel.getByLabel('k shortest').check();
  await panel.getByLabel('only', { exact: true }).check();
  await panel.getByRole('combobox', { name: 'Add a predicate' }).fill('ex:flight');
  await panel.getByRole('combobox', { name: 'Add a predicate' }).press('Enter');
  const weight = panel.getByRole('combobox', { name: 'Weight' });
  await weight.focus();
  await expect(weight.locator(`option[value="${WORLD}km"]`)).toHaveCount(1);
  await weight.selectOption(`${WORLD}km`);
  await panel.getByRole('button', { name: 'Find paths' }).click();
  await expect(panel.getByRole('listbox', { name: 'Paths found' }).getByRole('option')).toHaveCount(
    3,
  );
  // the filters folded away, so the list shows the three paths with their costs
  await panel.locator('summary', { hasText: 'Filters' }).click();
  await page.waitForTimeout(1000);
  await results.getByRole('button', { name: 'Fit to view' }).click();
  await shoot(page, 'query-paths');
});

test('pattern', async ({ page }) => {
  // the Pattern pane beside the editor: required triples, an optional one in its hull, a
  // filter as a chip on its variable and a BIND as a locked block
  const query = `PREFIX ex: <${WORLD}>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>

# The heads of the regions, and the company they work for
SELECT ?name ?region ?company ?home WHERE {
  ?person a ex:Person ;
          rdfs:label ?name ;
          ex:jobTitle ?title ;
          ex:worksFor ?org .
  ?org rdfs:label ?company .
  OPTIONAL { ?person ex:livesIn ?home }
  FILTER(STRSTARTS(?title, "Head of"))
  BIND(STRAFTER(?title, "Head of ") AS ?region) # the region they lead
}
ORDER BY ?region`;
  await page.addInitScript(
    ([q]) => {
      localStorage.setItem(
        'sparkles.queryTabs',
        JSON.stringify({ tabs: [{ id: 't1', title: 'Heads of regions', query: q }], active: 't1' }),
      );
      localStorage.removeItem('sparkles.patternLayouts');
      localStorage.setItem('sparkles.patternPane', JSON.stringify({ open: true, w: 620 }));
      localStorage.setItem('sparkles.dataset', 'world');
    },
    [query],
  );
  await page.goto('/ui/query?ds=world');
  const pane = page.getByRole('region', { name: 'Pattern', exact: true });
  await expect(pane).toHaveAttribute('data-state', 'ok');
  // a taller editor, so the drawing has room
  const top = (await page.locator('.cm-editor').boundingBox())!.y;
  const split = (await page.getByRole('separator', { name: 'Resize editor' }).boundingBox())!;
  await page.mouse.move(split.x + split.width / 2, split.y + split.height / 2);
  await page.mouse.down();
  await page.mouse.move(split.x + split.width / 2, top + 470, { steps: 5 });
  await page.mouse.up();
  await page.getByRole('button', { name: /^Run/ }).click();
  await expect(page.getByRole('grid')).toBeVisible({ timeout: 30_000 });
  const canvas = pane.locator('.pcanvas');
  await expect
    .poll(async () => Number(await canvas.getAttribute('data-settled')))
    .toBeGreaterThan(0);
  // the strip's count of the pattern's matches
  await pane.getByRole('button', { name: 'Count matches' }).click();
  // the seven heads of regions, which the README's alt text names
  await expect(pane.getByTestId('pattern-count')).toContainText('7 matches');
  await pane.getByRole('button', { name: 'Fit to view' }).click();
  await shoot(page, 'pattern');
});

test('plan-pattern', async ({ page }) => {
  // the plan drawn on the pattern of the query that ran (U04 §11): estimated and actual
  // rows on each edge, ex:memberOf marked as a misestimate, the joins as numbered badges
  // and the filter's rows on its chip, with the plan tree beside it
  const query = `PREFIX ex: <http://example.org/ontology#>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>

SELECT ?name ?teamName WHERE {
  ?person a ex:Person ;
          foaf:name ?name ;
          ex:memberOf ?team .
  ?team rdfs:label ?teamName .
  FILTER(STRLEN(?name) > 3)
}`;
  await page.addInitScript(
    ([q]) => {
      localStorage.setItem(
        'sparkles.queryTabs',
        JSON.stringify({ tabs: [{ id: 't1', title: 'Teams', query: q }], active: 't1' }),
      );
      localStorage.removeItem('sparkles.patternLayouts');
      localStorage.setItem('sparkles.patternPane', JSON.stringify({ open: false }));
      localStorage.setItem('sparkles.planOnPattern', 'true');
    },
    [query],
  );
  await page.goto(`/ui/query?ds=${DATASET}`);
  await expect(page.locator('.cm-content')).toContainText('SELECT', { timeout: 30_000 });
  // a shorter editor, so the drawing and the tree have room
  const top = (await page.locator('.cm-editor').boundingBox())!.y;
  const split = (await page.getByRole('separator', { name: 'Resize editor' }).boundingBox())!;
  await page.mouse.move(split.x + split.width / 2, split.y + split.height / 2);
  await page.mouse.down();
  await page.mouse.move(split.x + split.width / 2, top + 250, { steps: 5 });
  await page.mouse.up();
  await page.getByRole('button', { name: /^Run/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await results.getByRole('tab', { name: /Pattern/ }).click();
  const ran = results.getByRole('region', { name: 'Pattern of the query that ran' });
  const canvas = ran.locator('.pcanvas');
  await expect(canvas).toHaveAttribute('data-overlay', /memberOf/, { timeout: 30_000 });
  await expect
    .poll(async () => Number(await canvas.getAttribute('data-settled')))
    .toBeGreaterThan(0);
  await ran.getByRole('button', { name: 'Fit to view' }).click();
  await page.mouse.move(0, 0);
  await shoot(page, 'plan-pattern');
});

test('diff-graph', async ({ page, request }) => {
  // a commit of `world` that adds and removes triples, drawn as a graph of its own
  await update(
    request,
    'world',
    `PREFIX w: <${WORLD}>
DELETE DATA {
  w:marcus-hill w:jobTitle "Terminal manager"@en .
  w:olivia-chen w:livesIn w:vancouver .
  w:diego-alvarez w:knows w:sam-ortiz .
} ;
INSERT DATA {
  w:marcus-hill w:jobTitle "Head of terminals"@en .
  w:olivia-chen w:livesIn w:losangeles .
  w:nora-quinn a w:Person ; rdfs:label "Nora Quinn"@en ; w:jobTitle "Terminal manager"@en ;
    w:worksFor w:northgate ; w:livesIn w:toronto ; w:knows w:marcus-hill .
}`,
    'Northgate Rail reorganises its terminals',
  );
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'world'));
  await page.goto('/ui/explore?ds=world&diff=head');
  const view = page.locator('.dg');
  await expect(view.locator('.wrap[data-graph]')).toBeAttached({ timeout: 30_000 });
  await page.waitForTimeout(1500);
  await view.getByRole('button', { name: 'Fit to view' }).click();
  await shoot(page, 'diff-graph');
});

test('diff-lapse', async ({ page }) => {
  // a time-lapse over main's history, caught on a step that removes a triple
  await page.goto(`/ui/explore?ds=${DATASET}&diff=head&diffFrom=commit%3A0`);
  const view = page.locator('.dg');
  await expect(view.locator('.wrap[data-graph]')).toBeAttached({ timeout: 30_000 });
  await view.getByRole('checkbox', { name: 'Keep ghosts' }).check();
  await view.getByRole('combobox', { name: 'Steps a second' }).selectOption('2');
  await view.getByRole('button', { name: 'Play the time-lapse' }).click();
  await expect(view.getByRole('button', { name: 'Play the time-lapse' })).toBeVisible({
    timeout: 60_000,
  });
  await page.waitForTimeout(1000);
  await shoot(page, 'diff-lapse');
});

test('explore-lenses', async ({ page, request }) => {
  // the `foaf` dataset with OWL 2 RL materialized by the mock's reasoner: the symmetric
  // foaf:knows and the domains and ranges add edges, which the inferred-edge lens dashes
  const t = await request.post('/$/reason/foaf', { form: { profile: 'owl-rl' } });
  expect(t.ok()).toBe(true);
  await expect
    .poll(async () => (await (await request.get('/$/reason/foaf')).json()).profile, {
      timeout: 20_000,
    })
    .toBe('owl-rl');
  const rows = await request.post('/foaf/sparql', {
    headers: {
      'Content-Type': 'application/sparql-query',
      Accept: 'application/sparql-results+json',
    },
    data: `PREFIX foaf: <http://xmlns.com/foaf/0.1/>
SELECT ?p (COUNT(?o) AS ?n) WHERE { ?p foaf:knows ?o FILTER(STRSTARTS(STR(?p), "http://example.org/resource/")) }
GROUP BY ?p ORDER BY DESC(?n) ?p LIMIT 1`,
  });
  const person = (await rows.json()).results.bindings[0].p.value as string;
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'foaf'));
  await page.goto(`/ui/explore?ds=foaf&iri=${encodeURIComponent(person)}`);
  await expect(page.locator('.wrap[data-graph]')).toBeVisible({ timeout: 30_000 });
  await page.getByRole('button', { name: /^Lenses/ }).click();
  const menu = page.getByRole('dialog', { name: 'Lenses' });
  await menu.getByRole('combobox', { name: 'Size' }).selectOption('degree-dataset');
  await menu.getByRole('combobox', { name: 'Colour' }).selectOption('class');
  await menu.getByRole('combobox', { name: 'Badges' }).selectOption('inferred');
  await expect(page.getByTestId('lens-status-colour')).toContainText('categor', {
    timeout: 30_000,
  });
  await expect(page.getByTestId('lens-status-edge')).toContainText('inferred');
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: 'Fit to view' }).click();
  await settle(page, page.locator('.wrap[data-graph]'));
  // the drawing fitted into the room right of the legend
  await page
    .locator('.wrap[data-graph] canvas')
    .first()
    .evaluate((canvas) => {
      type Box = { x1: number; y1: number; w: number; h: number };
      type Cy = {
        zoom(z: number): void;
        pan(p: { x: number; y: number }): void;
        nodes(): { boundingBox(o: object): Box };
        width(): number;
        height(): number;
      };
      let el: HTMLElement | null = canvas.parentElement;
      while (el && !('_cyreg' in el)) el = el.parentElement;
      const cy = (el as unknown as { _cyreg: { cy: Cy } })._cyreg.cy;
      const bb = cy.nodes().boundingBox({ includeLabels: false, includeOverlays: false });
      const left = 250;
      // room for the labels around the nodes
      const [w, h] = [cy.width() - left - 110, cy.height() - 90];
      const z = Math.min(w / bb.w, h / bb.h);
      cy.zoom(z);
      cy.pan({
        x: left + 50 + w / 2 - z * (bb.x1 + bb.w / 2),
        y: 45 + h / 2 - z * (bb.y1 + bb.h / 2),
      });
    });
  await shoot(page, 'explore-lenses');
});

test('explore-algo-lenses', async ({ page, request }) => {
  // algorithm lenses over the drawn graph: nodes sized by PageRank and coloured by Leiden
  // communities, with the menu's parameters and the legend's community sizes
  const rows = await request.post('/foaf/sparql', {
    headers: {
      'Content-Type': 'application/sparql-query',
      Accept: 'application/sparql-results+json',
    },
    data: `PREFIX foaf: <http://xmlns.com/foaf/0.1/>
SELECT ?p (COUNT(?o) AS ?n) WHERE { ?p foaf:knows ?o FILTER(STRSTARTS(STR(?p), "http://example.org/resource/")) }
GROUP BY ?p ORDER BY DESC(?n) ?p LIMIT 1`,
  });
  const person = (await rows.json()).results.bindings[0].p.value as string;
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'foaf'));
  await page.goto(`/ui/explore?ds=foaf&iri=${encodeURIComponent(person)}`);
  await expect(page.locator('.wrap[data-graph]')).toBeVisible({ timeout: 30_000 });
  await page.getByRole('button', { name: /^Lenses/ }).click();
  const menu = page.getByRole('dialog', { name: 'Lenses' });
  await menu.getByRole('combobox', { name: 'Size' }).selectOption('pageRank');
  await menu.getByRole('combobox', { name: 'Colour' }).selectOption('leiden');
  await expect(page.getByTestId('lens-status-size')).toContainText('iteration', {
    timeout: 30_000,
  });
  await expect(page.getByTestId('lens-status-colour')).toContainText('communit', {
    timeout: 30_000,
  });
  await page.getByRole('button', { name: 'Fit to view' }).click();
  await shoot(page, 'explore-algo-lenses');
});

test('explore-why', async ({ page, request }) => {
  // why is this inferred? the dashed edge's derivation tree beside the graph, with its
  // premises drawn and highlighted by Show on graph
  const ds = 'staff';
  const r = await request.post('/$/datasets', { data: { dbName: ds, dbType: 'persistent' } });
  expect(r.ok() || r.status() === 409).toBe(true);
  if (r.ok()) {
    const u = await request.post(`/${ds}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: `PREFIX ex: <http://example.org/ontology#>
PREFIX res: <http://example.org/resource/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
INSERT DATA {
  ex:Manager rdfs:subClassOf ex:Employee . ex:Employee rdfs:subClassOf ex:Person .
  ex:manages rdfs:domain ex:Manager . ex:manages rdfs:range ex:Team .
  res:alice a ex:Manager ; ex:manages res:team7 ; ex:knows res:bob , res:carol ; rdfs:label "Alice Ng" .
  res:bob ex:memberOf res:team7 ; rdfs:label "Bob Diaz" . res:carol rdfs:label "Carol Ito" .
  res:team7 rdfs:label "Team 7" .
}`,
    });
    expect(u.ok()).toBe(true);
    const t = await request.post(`/$/reason/${ds}`, { form: { profile: 'rdfs' } });
    expect(t.ok()).toBe(true);
    await expect
      .poll(async () => (await (await request.get(`/$/reason/${ds}`)).json()).profile, {
        timeout: 20_000,
      })
      .toBe('rdfs');
  }
  await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), ds);
  await page.goto(
    `/ui/explore?ds=${ds}&iri=${encodeURIComponent('http://example.org/resource/alice')}`,
  );
  const graph = page.locator('.wrap[data-graph]');
  await expect(graph).toBeVisible({ timeout: 30_000 });
  await expect
    .poll(async () => {
      const g = JSON.parse((await graph.getAttribute('data-graph')) ?? '{"edges":[]}');
      return (g.edges as [string, string, string][]).some(([, , i]) => i === 'inferred');
    })
    .toBe(true);
  await expect
    .poll(async () => {
      const g = JSON.parse((await graph.getAttribute('data-graph')) ?? '{"nodes":[]}');
      return (g.nodes as [string][]).map(([l]) => l);
    })
    .toEqual(expect.arrayContaining(['Alice Ng', 'Person']));
  // the dashed edge from alice to Person, two subclass steps above her asserted type
  const at = await page
    .locator('.wrap canvas')
    .first()
    .evaluate((canvas) => {
      type Node = { data(k: string): unknown };
      type Edge = { source(): Node; target(): Node; renderedMidpoint(): { x: number; y: number } };
      let el: HTMLElement | null = canvas.parentElement;
      while (el && !('_cyreg' in el)) el = el.parentElement;
      const cy = (el as unknown as { _cyreg: { cy: { edges(): Edge[] } } })._cyreg.cy;
      const e = [...cy.edges()].find(
        (x) => x.source().data('label') === 'Alice Ng' && x.target().data('label') === 'Person',
      )!;
      const p = e.renderedMidpoint();
      const box = el!.getBoundingClientRect();
      return { x: box.left + p.x, y: box.top + p.y };
    });
  await page.mouse.click(at.x, at.y);
  const panel = page.getByTestId('why-panel');
  await expect(panel.getByRole('tree')).toBeVisible();
  await panel.getByRole('button', { name: 'Show on graph' }).click();
  await expect
    .poll(async () => Number(await page.locator('.wrap').first().getAttribute('data-whymarked')))
    .toBeGreaterThan(8);
  await page.getByRole('button', { name: 'Fit to view' }).click();
  await settle(page, graph);
  await shoot(page, 'explore-why');
});

test('explore-why-stale', async ({ page, request }) => {
  // a derivation from stale inferences: the note above the tree, and Next on the second
  // of two derivations whose top rules differ
  const ds = 'staff-history';
  const r = await request.post('/$/datasets', { data: { dbName: ds, dbType: 'persistent' } });
  expect(r.ok() || r.status() === 409).toBe(true);
  if (r.ok()) {
    const u = await request.post(`/${ds}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: `PREFIX ex: <http://example.org/ontology#>
PREFIX res: <http://example.org/resource/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
INSERT DATA {
  ex:Manager rdfs:subClassOf ex:Employee . ex:reportsTo rdfs:range ex:Employee .
  res:alice a ex:Manager ; rdfs:label "Alice Ng" .
  res:bob ex:reportsTo res:alice ; rdfs:label "Bob Diaz" .
  res:carol ex:reportsTo res:alice ; rdfs:label "Carol Ito" .
}`,
    });
    expect(u.ok()).toBe(true);
    const t = await request.post(`/$/reason/${ds}`, { form: { profile: 'rdfs' } });
    expect(t.ok()).toBe(true);
    await expect
      .poll(async () => (await (await request.get(`/$/reason/${ds}`)).json()).profile, {
        timeout: 20_000,
      })
      .toBe('rdfs');
    // two commits after the run leave the inferences stale
    for (const who of ['dan', 'erin']) {
      const w = await request.post(`/${ds}/update`, {
        headers: { 'Content-Type': 'application/sparql-update' },
        data: `INSERT DATA { <http://example.org/resource/${who}> <http://www.w3.org/2000/01/rdf-schema#label> "${who}" }`,
      });
      expect(w.ok()).toBe(true);
    }
  }
  await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), ds);
  await page.goto(
    `/ui/explore?ds=${ds}&iri=${encodeURIComponent('http://example.org/resource/alice')}`,
  );
  const graph = page.locator('.wrap[data-graph]');
  await expect(graph).toBeVisible({ timeout: 30_000 });
  await expect
    .poll(async () => {
      const g = JSON.parse((await graph.getAttribute('data-graph')) ?? '{"nodes":[]}');
      return (g.nodes as [string][]).map(([l]) => l);
    })
    .toEqual(expect.arrayContaining(['Alice Ng', 'Employee']));
  const why = page.getByRole('button', { name: /Why is rdf:type inferred\?/ }).first();
  await why.click();
  const panel = page.getByTestId('why-panel');
  await expect(panel.getByRole('tree')).toBeVisible();
  await expect(panel.getByRole('note')).toContainText('stale');
  await panel.getByRole('button', { name: 'Next' }).click();
  await expect(panel).toContainText('2 of 2 derivations');
  await page.getByRole('button', { name: 'Fit to view' }).click();
  await settle(page, graph);
  await shoot(page, 'explore-why-stale');
});

test('pattern-edit', async ({ page }) => {
  // building on the canvas: + Edge from ?person, with the predicate picker ranked by the
  // profile of ex:Person, and a waiting class node beside the drawing
  const query = `PREFIX ex: <http://example.org/ontology#>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>

SELECT ?person ?name WHERE {
  ?person a ex:Person ;
          foaf:name ?name .
}`;
  await page.addInitScript(
    ([q]) => {
      localStorage.setItem(
        'sparkles.queryTabs',
        JSON.stringify({ tabs: [{ id: 't1', title: 'People', query: q }], active: 't1' }),
      );
      localStorage.removeItem('sparkles.patternLayouts');
      localStorage.setItem('sparkles.patternPane', JSON.stringify({ open: true, w: 620 }));
    },
    [query],
  );
  await page.goto(`/ui/query?ds=${DATASET}`);
  const pane = page.getByRole('region', { name: 'Pattern', exact: true });
  await expect(pane).toHaveAttribute('data-state', 'ok');
  // a taller editor, so the drawing and the strip have room
  const top = (await page.locator('.cm-editor').boundingBox())!.y;
  const split = (await page.getByRole('separator', { name: 'Resize editor' }).boundingBox())!;
  await page.mouse.move(split.x + split.width / 2, split.y + split.height / 2);
  await page.mouse.down();
  await page.mouse.move(split.x + split.width / 2, top + 470, { steps: 5 });
  await page.mouse.up();
  const canvas = pane.locator('.pcanvas');
  await expect
    .poll(async () => Number(await canvas.getAttribute('data-settled')))
    .toBeGreaterThan(0);
  // a waiting class node, added with N
  await canvas.focus();
  await page.keyboard.press('n');
  await page.keyboard.type('ex:Team');
  await page.keyboard.press('Enter');
  await expect(pane.getByTestId('pattern-live')).toContainText('waiting for an edge');
  await pane.getByRole('button', { name: 'Fit to view' }).click();
  // E on ?person opens the predicate picker
  await canvas.focus();
  await expect(pane.getByTestId('pattern-focus')).toContainText('?person');
  await page.keyboard.press('e');
  await pane.getByLabel('To', { exact: true }).fill('?team');
  await pane.getByLabel('Predicate', { exact: true }).focus();
  await expect(pane.getByRole('option').first()).toContainText('used by the class');
  // the settling of `shoot` takes screenshots of the page, which closes the list: the
  // page is already still once the ranked options are drawn
  await page.mouse.move(0, 0);
  await page.evaluate(() => document.fonts.ready.then(() => undefined));
  await page.waitForTimeout(500);
  await page.screenshot({ path: join(OUT, 'pattern-edit.png'), animations: 'disabled' });
});

// the validation overlay (docs/specs/U07-data-quality-and-schema-diagram.md §11): a team
// whose SHACL guard wants a name for every person and a valid email address
const Q = 'http://example.org/quality/';

test('validation-overlay', async ({ page, request }) => {
  const made = await request.post('/$/datasets', { data: { dbName: 'team', dbType: 'mem' } });
  expect(made.ok() || made.status() === 409).toBe(true);
  const u = await request.post('/team/update', {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: `PREFIX q: <${Q}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> INSERT DATA {
      q:acme a q:Organization ; rdfs:label "Acme Robotics" .
      q:lea a q:Person ; q:name "Léa Martin" ; q:email "lea@acme.example" ; q:worksFor q:acme .
      q:omar a q:Person ; q:name "Omar Haddad" ; q:email "omar@acme" ; q:worksFor q:acme .
      q:p17 a q:Person ; q:email "p17@acme.example" ; q:worksFor q:acme .
      q:p18 a q:Person ; q:worksFor q:acme .
      q:sara a q:Person ; q:name "Sara Lind" ; q:email "sara@acme.example" ; q:worksFor q:acme . }`,
  });
  expect(u.ok()).toBe(true);
  const shapes = `@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix q: <${Q}> .
q:PersonShape a sh:NodeShape ; sh:targetClass q:Person ;
  sh:property [ sh:path q:name ; sh:minCount 1 ] ;
  sh:property [ sh:path q:email ; sh:pattern "^[^@]+@[^@]+[.][^@]+$" ; sh:message "not an email address" ] .`;
  const g = await request.put('/$/validation/team', {
    data: { mode: 'warn', shapes: { inline: shapes } },
  });
  expect(g.ok()).toBe(true);
  await page.goto(`/ui/explore?ds=team&iri=${encodeURIComponent(`${Q}acme`)}`);
  await expect(page.getByRole('button', { name: /p17, 1 violation/ })).toBeVisible({
    timeout: 30_000,
  });
  await page.getByRole('button', { name: /omar, 1 violation/i }).click();
  const panel = page.getByRole('dialog', { name: /Validation results of/ });
  await panel.getByRole('button', { name: 'Show triples' }).click();
  await page.getByRole('button', { name: /p17, 1 violation/ }).click();
  await panel.getByRole('button', { name: 'Suggest fix' }).click();
  const editor = panel.getByRole('textbox', { name: 'Suggested update' });
  await editor.fill((await editor.inputValue()).replace('?value .', '"Priya Shah" .'));
  await panel.getByRole('button', { name: 'Preview' }).click();
  await expect(panel).toContainText('The guard would resolve this result.');
  await shoot(page, 'validation-overlay');
});

test('forms', async ({ page }) => {
  // the editing form of a person of `world` in the explorer's panel, generated from the
  // shapes (mock/forms.mjs): a mistyped email that the browser checks at once, and a
  // previewed move to a city in Works for, which the server's check reports under the field
  await page.goto(`/ui/explore?ds=world&iri=${encodeURIComponent(`${WORLD}priya-nair`)}`);
  const edit = page.getByRole('button', { name: 'Edit', exact: true });
  await expect(edit).toBeVisible({ timeout: 30_000 });
  await edit.click();
  const form = page.getByRole('region', { name: /^Edit Priya Nair/ });
  await form.getByLabel('Email', { exact: true }).fill('priya.nair@meridian');
  await form.getByRole('button', { name: 'Clear Lives in' }).click();
  await form.getByRole('combobox', { name: 'Lives in' }).fill(`<${WORLD}delhi>`);
  await page.getByRole('option', { name: /Use/ }).click();
  await form.getByRole('button', { name: 'Clear Works for' }).click();
  await form.getByRole('combobox', { name: 'Works for' }).fill(`<${WORLD}mumbai>`);
  await page.getByRole('option', { name: /Use/ }).click();
  await form.getByRole('button', { name: 'Preview' }).click();
  await expect(form.locator(`[data-field="<${WORLD}worksFor>"]`)).toContainText('server check');
  await form
    .locator(`[data-field="<${WORLD}worksFor>"]`)
    .evaluate((e) => e.scrollIntoView({ block: 'center' }));
  await shoot(page, 'forms');
});

test('forms-branch', async ({ page, request }) => {
  // the form on its own page after a save while main is protected: the save went to the
  // editor's session branch, which the banner links to the merge page
  const p = await request.patch('/$/branches/world/main', { data: { protected: true } });
  expect(p.ok()).toBe(true);
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'world'));
  try {
    await page.goto(`/ui/datasets/world/edit?iri=${encodeURIComponent(`${WORLD}wei-lin-tan`)}`);
    const form = page.getByRole('region', { name: /^Edit / });
    await expect(form.getByRole('note').filter({ hasText: /main is protected/ })).toBeVisible({
      timeout: 30_000,
    });
    await form.getByLabel('Email', { exact: true }).fill('weilin.tan@meridian.example');
    await form.getByRole('radio', { name: 'Active' }).check();
    await form
      .getByLabel('Message', { exact: true })
      .fill('Add the email and status of Wei Lin Tan');
    await form.getByRole('button', { name: 'Save', exact: true }).click();
    await expect(form.getByRole('status').filter({ hasText: 'Editing on branch' })).toBeVisible();
    await form.getByLabel('Age', { exact: true }).fill('17');
    // the save's toast goes before the picture
    await expect(page.getByText(/^Saved commit/)).toHaveCount(0, { timeout: 15_000 });
    await shoot(page, 'forms-branch');
  } finally {
    await request.patch('/$/branches/world/main', { data: { protected: false } });
  }
});

/** The server page's model configuration form, scrolled to the row of `field`. */
async function modelsForm(page: Page, request: APIRequestContext, field: string) {
  await request.delete('/$/server/settings/models');
  await page.goto('/ui/server');
  const panel = page.getByTestId('settings-model-configuration');
  const row = (f: string) => panel.locator(`[data-field="${f}"]`);
  await expect(row(field)).toBeVisible();
  return { panel, row };
}

test('models-settings', async ({ page, request }) => {
  // claude's allowed models as a list, and its per-model options as cards, one open with
  // its typed fields and one being added from a suggestion
  const { row } = await modelsForm(page, request, 'providers.claude.allowedModels');
  const allowed = row('providers.claude.allowedModels');
  await allowed.getByPlaceholder('Add a model').fill('claude-opus-5-5');
  await allowed.getByPlaceholder('Add a model').press('Enter');
  const models = row('providers.claude.models');
  await models.locator('[data-entry="claude-haiku-5-5"] summary').click();
  await models.getByPlaceholder('Add options for a model').fill('claude-sonnet-5-5');
  await models.getByRole('button', { name: 'Add model' }).click();
  const sonnet = models.locator('[data-entry="claude-sonnet-5-5"]');
  await sonnet.getByLabel('Context tokens').fill('200000');
  await sonnet.getByLabel('Output tokens').fill('8192');
  await allowed.evaluate((e) => e.scrollIntoView({ block: 'start' }));
  await page.evaluate(() => document.querySelector('main')?.scrollBy(0, -40));
  await shoot(page, 'models-settings');
});

test('models-settings-errors', async ({ page, request }) => {
  // errors beside the fields as they are typed: a per-model option out of range, text in
  // a token field, and a role entry whose model the provider's allowed models leave out
  const { panel, row } = await modelsForm(page, request, 'providers.local.models');
  const models = row('providers.local.models');
  await models.getByPlaceholder('Add options for a model').fill('llama3.3:70b');
  await models.getByRole('button', { name: 'Add model' }).click();
  const card = models.locator('[data-entry="llama3.3:70b"]');
  await card.getByLabel('Temperature').fill('2.5');
  await card.getByLabel('Context tokens').fill('128k');
  await card.getByLabel('Request timeout').fill('120');
  await expect(card.getByText('Temperature is at most 2')).toBeVisible();
  await expect(panel.getByText('1 field needs a fix')).toBeVisible();
  await row('providers.local.allowedModels').evaluate((e) => e.scrollIntoView({ block: 'start' }));
  await page.evaluate(() => document.querySelector('main')?.scrollBy(0, -40));
  await shoot(page, 'models-settings-errors');
});
