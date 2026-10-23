// The README's screenshots of branches, of the agent pages and of the Explore overview
// grouped by the GraphRAG communities, in the dark theme, against
// the mock backend that playwright.screenshots-mock.config.ts starts. The `org` dataset
// gets a branch whose changes clash with main, so that the dataset page shows branches and
// the commit graph and the merge page shows conflicts. The Ask bar runs the scripted
// pipeline of mock/assistant.mjs, and a handoff link stands in for an agent's
// `share_query`. Run with `mise run docs:screenshots`.

import { test as base, expect, type APIRequestContext, type Page } from '@playwright/test';
import { encodeHandoff } from '../../src/lib/handoff';
import { shoot } from '../shoot';

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
  const toggle = history(page).getByRole('button', { name: 'Graph' });
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

// the mock's `flows` dataset (mock/geo.mjs): people in Lyon and Paris who work for an
// organization in Paris, and routes between airports
const FLOWS = 'http://example.org/flows/';

test('map-arcs', async ({ page }) => {
  await page.goto(`/ui/explore?ds=flows&iri=${encodeURIComponent(`${FLOWS}acme`)}`);
  await expect(page.getByText(/\d+ nodes, \d+ edges/)).toBeVisible({ timeout: 30_000 });
  await page
    .getByRole('group', { name: 'Show the graph as' })
    .getByRole('button', { name: 'Map' })
    .click();
  const arcmap = page.locator('.arcmap');
  await expect(arcmap).toHaveAttribute('data-loading', 'false', { timeout: 30_000 });
  const region = page.getByRole('region', { name: 'Graph map' });
  await expect(region).toHaveAttribute('data-state', 'ready');
  // the ten people of Lyon, selected with the keyboard
  await region.locator('canvas.maplibregl-canvas').focus();
  await page.keyboard.press('Tab');
  await page.keyboard.press('Enter');
  await expect(page.getByRole('region', { name: 'Selection' })).toContainText('selected');
  await shoot(page, 'map-arcs');
});

test('result-arcs', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'flows'));
  const query = `PREFIX ex: <${FLOWS}>
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
  await page.goto('/ui/query?ds=flows');
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

test('explore-communities', async ({ page }) => {
  // the overview grouped by the GraphRAG communities of mock/graphrag.mjs, opened at
  // Engineering, with the payments team's report beside it and a finding's entities marked
  await page.goto(`/ui/explore?ds=${DATASET}&overview=communities`);
  const ov = page.getByTestId('overview');
  await expect(ov).toHaveAttribute('data-mode', 'communities');
  await ov.getByRole('button', { name: 'List' }).click();
  const row = (name: RegExp) =>
    ov.locator('tbody tr').filter({ has: page.getByRole('button', { name }) });
  await row(/^Engineering/)
    .getByRole('button', { name: 'Open' })
    .click();
  await row(/^The payments team/)
    .getByRole('button', { name: /^The payments team/ })
    .click();
  const side = page.getByTestId('community-side');
  await expect(side.getByRole('heading', { name: 'The payments team' })).toBeVisible();
  await side.getByRole('button', { name: /Kai works with the rest/ }).click();
  await ov.getByRole('button', { name: 'List' }).click();
  await expect(ov.locator('canvas').first()).toBeVisible();
  await shoot(page, 'explore-communities');
});
