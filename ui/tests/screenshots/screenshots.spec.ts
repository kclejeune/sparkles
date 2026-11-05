// The README's screenshots of the UI, in dark mode, against the demo dataset that
// global-setup.ts serves: each test drives one page to an interesting state, waits until
// nothing moves any more (fonts, spinners, map and graph layouts), and writes
// docs/images/<name>.png. Run with `mise run docs:screenshots`.

import { test as base, expect, type Locator, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { settle, shoot } from '../shoot';
import { DATASET, DEMO } from './global-setup';
import { BROWSE_DATASET, BROWSE_TURTLE, CELLO } from '../browse-fixture';

const test = base.extend({
  baseURL: async ({}, use) => {
    const url = process.env.SPARKLES_SCREENSHOTS_URL;
    if (!url)
      throw new Error('SPARKLES_SCREENSHOTS_URL is not set: run `mise run docs:screenshots`');
    await use(url);
  },
  page: async ({ page }, use) => {
    // the stored theme wins over the browser's preference, so set both; and the dataset the
    // query page starts on
    await page.addInitScript((ds) => {
      localStorage.setItem('sparkles.theme', 'dark');
      localStorage.setItem('sparkles.dataset', ds);
      // a seeded Math.random, so that the graph layouts come out the same on every run
      let seed = 0x5eed;
      Math.random = () => {
        seed = (seed + 0x6d2b79f5) | 0;
        let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
        t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
        return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
      };
    }, DATASET);
    await use(page);
  },
});

const PREFIX = {
  ex: 'PREFIX ex: <http://example.org/ns#>',
  foaf: 'PREFIX foaf: <http://xmlns.com/foaf/0.1/>',
  geo: 'PREFIX geo: <http://www.opengis.net/ont/geosparql#>',
  rdfs: 'PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>',
};

/**
 * Where the node labelled `label` of the graph drawn in `region` is on the page; with
 * `zoom`, first zooms the graph to that level around the node (a larger graph is laid out at
 * a zoom where its labels are hard to read). Cytoscape keeps its instance on the container
 * element.
 */
async function nodeAt(region: Locator, label: string, zoom?: number) {
  return await region
    .locator('canvas')
    .first()
    .evaluate(
      (canvas, [label, zoom]) => {
        type Node = { data(k: string): unknown; renderedPosition(): { x: number; y: number } };
        type Cy = { zoom(z: number): void; center(n: Node): void; nodes(): Node[] };
        let el: HTMLElement | null = canvas.parentElement;
        while (el && !('_cyreg' in el)) el = el.parentElement;
        if (!el) throw new Error('no graph');
        const cy = (el as unknown as { _cyreg: { cy: Cy } })._cyreg.cy;
        const node = [...cy.nodes()].find((n) => n.data('label') === label);
        if (!node) throw new Error(`no node ${label}`);
        if (zoom) {
          cy.zoom(zoom);
          cy.center(node);
        }
        const p = node.renderedPosition();
        const box = el.getBoundingClientRect();
        return { x: box.left + p.x, y: box.top + p.y };
      },
      [label, zoom] as const,
    );
}

/**
 * Opens the query page, types `query` into the editor, drags the splitter so the editor is
 * `editor` pixels high, and runs the query.
 */
async function run(page: Page, query: string, editor: number) {
  await page.goto('/ui/query');
  await page.locator('.cm-content').click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText(query);
  // the caret at the start, where no bracket is highlighted
  await page.keyboard.press('ControlOrMeta+Home');
  const top = (await page.locator('.cm-editor').boundingBox())!.y;
  const split = (await page.getByRole('separator', { name: 'Resize editor' }).boundingBox())!;
  await page.mouse.move(split.x + split.width / 2, split.y + split.height / 2);
  await page.mouse.down();
  await page.mouse.move(split.x + split.width / 2, top + editor, { steps: 5 });
  await page.mouse.up();
  await page.getByRole('button', { name: /^Run\b/ }).click();
  return page.getByRole('region', { name: 'Results' });
}

test('query', async ({ page }) => {
  const results = await run(
    page,
    `${PREFIX.ex}
${PREFIX.foaf}
${PREFIX.rdfs}

# Who works where, and how well connected they are
SELECT ?person ?name ?born ?employer ?city (COUNT(?contact) AS ?contacts)
WHERE {
  ?person a ex:Person ;
          rdfs:label ?name ;
          ex:worksFor ?employer .
  OPTIONAL { ?person ex:birthDate ?born }
  ?employer ex:headquarters/rdfs:label ?city .
  FILTER(LANG(?city) = "en")
  OPTIONAL { ?person foaf:knows ?contact }
}
GROUP BY ?person ?name ?born ?employer ?city
ORDER BY DESC(?contacts) ?name`,
    375,
  );
  await expect(results.getByRole('row').nth(12)).toBeVisible();
  await shoot(page, 'query');
});

test('graph', async ({ page }) => {
  const results = await run(
    page,
    `${PREFIX.ex}
${PREFIX.foaf}
${PREFIX.rdfs}
# The research network: who knows whom, and where they work
CONSTRUCT { ?a rdfs:label ?name ; foaf:knows ?b ; ex:worksFor ?org . ?org rdfs:label ?orgName }
WHERE {
  ?a ex:worksFor ?org ; rdfs:label ?name ; foaf:knows ?b .
  ?org rdfs:label ?orgName FILTER(LANG(?orgName) = "en")
}`,
    195,
  );
  await results.getByRole('tab', { name: 'Graph' }).click();
  await expect(results.locator('canvas').first()).toBeVisible();
  await settle(page, results);
  // one person's neighbourhood highlighted
  const priya = await nodeAt(results, 'Priya Raman', 0.85);
  await page.mouse.move(priya.x, priya.y);
  await shoot(page, 'graph', { hover: true });
});

test('explore', async ({ page }) => {
  await page.goto(
    `/ui/explore?ds=${DATASET}&iri=${encodeURIComponent('http://example.org/org/atlas-open-data')}`,
  );
  await expect(page.locator('canvas').first()).toBeVisible();
  await expect(page.getByText('Referenced by')).toBeVisible();
  await shoot(page, 'explore');
});

test('path-finder', async ({ page }) => {
  // without the step-by-step animation, so the picture shows every path at once; along
  // foaf:knows only, in its direction, the three shortest paths share no one in between
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.goto(
    `/ui/explore?ds=${DATASET}&iri=${encodeURIComponent('http://example.org/person/freya-lindqvist')}`,
  );
  await expect(page.locator('canvas').first()).toBeVisible();
  await page.getByRole('button', { name: 'Paths', exact: true }).click();
  const panel = page.getByRole('region', { name: 'Paths' });
  await expect(panel.locator('#path-from').locator('..')).toContainText('Freya Lindqvist');
  await panel.getByRole('combobox', { name: 'To' }).fill('Luk');
  await panel
    .getByRole('option', { name: /Lukáš Novák/ })
    .first()
    .click();
  await panel.getByLabel('k shortest').check();
  await panel.getByLabel('Direction').selectOption('forward');
  await panel.getByLabel('only', { exact: true }).check();
  await panel.getByRole('combobox', { name: 'Add a predicate' }).fill('foaf:knows');
  await panel.getByRole('combobox', { name: 'Add a predicate' }).press('Enter');
  await panel.getByRole('button', { name: 'Find paths' }).click();
  await expect(panel.getByRole('listbox', { name: 'Paths found' }).getByRole('option')).toHaveCount(
    3,
  );
  await shoot(page, 'path-finder');
});

test('dataset', async ({ page }) => {
  await page.goto(`/ui/datasets/${DATASET}`);
  await expect(page.getByRole('heading', { name: 'Top classes' })).toBeVisible();
  await shoot(page, 'dataset');
});

test('map', async ({ page }) => {
  const results = await run(
    page,
    `${PREFIX.geo}
${PREFIX.rdfs}

# Cities, areas and rail links, drawn from their WKT geometries
SELECT ?place (STR(?label) AS ?name) ?wkt
WHERE {
  ?place rdfs:label ?label ;
         geo:hasDefaultGeometry/geo:asWKT ?wkt .
  FILTER(LANG(?label) = "en")
}
ORDER BY ?name`,
    240,
  );
  await results.getByRole('tab', { name: 'Map' }).click();
  const map = page.getByRole('region', { name: 'Result map' });
  await expect(map).toHaveAttribute('data-state', 'ready');
  await settle(page, map);
  await shoot(page, 'map');
});

test('validate', async ({ page }) => {
  await page.goto(`/ui/datasets/${DATASET}`);
  const panel = page.locator('section.panel').filter({
    has: page.getByRole('heading', { name: 'Validate', exact: true }),
  });
  await panel.getByRole('button', { name: 'ShEx', exact: true }).click();
  await page.getByRole('textbox', { name: 'ShEx schema' }).click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.press('Delete');
  // the schema without the file's header comment
  const schema = readFileSync(join(DEMO, 'network.shex'), 'utf8');
  await page.keyboard.insertText(schema.slice(schema.indexOf('PREFIX')));
  await page.keyboard.press('ControlOrMeta+Home');
  await page
    .getByRole('textbox', { name: 'Shape map' })
    .fill(readFileSync(join(DEMO, 'network.smap'), 'utf8').trim());
  await panel.getByLabel('Only nonconformant').check();
  await panel.getByRole('button', { name: 'Validate', exact: true }).click();
  await expect(panel.locator('.panel-head .badge')).toHaveText(/Does not conform/);
  const rows = panel.locator('.shex-results > table > tbody > tr');
  for (const node of ['tomasz-kowalski', 'corvid-labs'])
    await rows.filter({ hasText: node }).getByRole('button', { name: 'Show the failures' }).click();
  await expect(panel.locator('table.failures')).toHaveCount(2);
  await panel.evaluate((el) => el.scrollIntoView({ block: 'start' }));
  await page.mouse.wheel(0, -16);
  await shoot(page, 'validate');
});

test('schema', async ({ page }) => {
  await page.goto(`/ui/explore?ds=${DATASET}&tab=schema`);
  for (const c of ['Company', 'Region'])
    await page
      .locator('[role=treeitem] > .node')
      .filter({ has: page.getByText(c, { exact: true }) })
      .getByRole('button', { name: 'Expand' })
      .click();
  await page.getByText('Person', { exact: true }).first().click();
  await expect(page.getByText('ex:worksFor').first()).toBeVisible();
  await shoot(page, 'schema');
});

// The app shell: the top bar's search at 1440 pixels, the icon rail at 900 and the drawer
// on a phone.
test('shell-search', async ({ page }) => {
  await page.goto(`/ui/datasets/${DATASET}`);
  await expect(page.getByRole('heading', { name: 'Top classes' })).toBeVisible();
  await page.keyboard.press('ControlOrMeta+k');
  await page.keyboard.type('org');
  await expect(
    page.getByRole('listbox', { name: 'Search results' }).getByRole('option', { name: /^Organi/ }),
  ).toBeVisible();
  await shoot(page, 'shell-search', { focus: true });
});

// The command palette (item 631): its commands with nothing typed on the Query page, the
// `>` mode, and the phone's full-width sheet.
test('palette', async ({ page }) => {
  await page.goto('/ui/query');
  await expect(page.locator('.cm-content')).toBeVisible();
  await page.locator('.cm-content').click();
  await page.keyboard.press('ControlOrMeta+k');
  const list = page.getByRole('listbox', { name: 'Search results' });
  await expect(list.getByRole('group', { name: 'This page' })).toBeVisible();
  await shoot(page, 'palette', { focus: true });
  await page.keyboard.type('>form');
  await expect(list.getByRole('option', { name: /^Format query/ })).toBeVisible();
  await shoot(page, 'palette-commands', { focus: true });
});

test('palette-phone', async ({ browser }) => {
  const ctx = await browser.newContext({
    viewport: { width: 390, height: 844 },
    deviceScaleFactor: 2,
    isMobile: true,
    hasTouch: true,
    colorScheme: 'dark',
    baseURL: process.env.SPARKLES_SCREENSHOTS_URL,
  });
  await ctx.addInitScript((ds) => {
    localStorage.setItem('sparkles.theme', 'dark');
    localStorage.setItem('sparkles.dataset', ds);
  }, DATASET);
  const page = await ctx.newPage();
  await page.goto(`/ui/workbench/ontology?ds=${DATASET}`);
  await expect(page.getByRole('heading', { name: 'Ontology', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Open search' }).click();
  await expect(
    page.getByRole('listbox', { name: 'Search results' }).getByRole('group', { name: 'This page' }),
  ).toBeVisible();
  await shoot(page, 'palette-phone');
  await ctx.close();
});

test('shell-medium', async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 900 });
  await page.goto(
    `/ui/workbench/ontology?ds=${DATASET}&iri=${encodeURIComponent('http://example.org/ns#Person')}`,
  );
  await expect(page.getByRole('heading', { name: 'Person', exact: true })).toBeVisible();
  await shoot(page, 'shell-medium');
});

test('shell-phone', async ({ browser }) => {
  const ctx = await browser.newContext({
    viewport: { width: 390, height: 844 },
    deviceScaleFactor: 2,
    isMobile: true,
    hasTouch: true,
    colorScheme: 'dark',
    baseURL: process.env.SPARKLES_SCREENSHOTS_URL,
  });
  await ctx.addInitScript((ds) => {
    localStorage.setItem('sparkles.theme', 'dark');
    localStorage.setItem('sparkles.dataset', ds);
  }, DATASET);
  const page = await ctx.newPage();
  await page.goto(`/ui/datasets/${DATASET}`);
  await expect(page.getByRole('heading', { name: 'Top classes' })).toBeVisible();
  await page.getByRole('button', { name: 'Menu' }).click();
  await expect(page.getByRole('dialog', { name: 'Menu' })).toBeVisible();
  await shoot(page, 'shell-phone');
  await ctx.close();
});

// The ontology browser on a small OBO-style ontology of its own (tests/browse-fixture.ts):
// an entity's page at 1440 pixels, the landing page, and the entity's page on a phone.
test.describe('browse', () => {
  test.beforeAll(async ({ request }) => {
    const made = await request.post('/$/datasets', {
      data: { dbName: BROWSE_DATASET, dbType: 'mem' },
    });
    expect(made.ok() || made.status() === 409, await made.text()).toBe(true);
    const loaded = await request.put(`/${BROWSE_DATASET}/data`, {
      headers: { 'Content-Type': 'text/turtle' },
      data: BROWSE_TURTLE,
    });
    expect(loaded.ok(), await loaded.text()).toBe(true);
  });

  const term = `/ui/browse?ds=${BROWSE_DATASET}&iri=${encodeURIComponent(CELLO(5))}`;

  test('browse-term', async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 1000 });
    await page.goto(term);
    await expect(page.getByRole('heading', { level: 1, name: 'mitochondrion' })).toBeVisible();
    await expect(page.getByRole('link', { name: 'mitochondrial matrix' }).first()).toBeVisible();
    await expect(page.getByRole('region', { name: 'In other datasets' })).not.toContainText(
      'Looking in',
    );
    await shoot(page, 'browse-term');
  });

  test('browse-landing', async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(`/ui/browse?ds=${BROWSE_DATASET}`);
    await expect(page.getByRole('region', { name: 'Top classes' })).toContainText(
      'A location in or around a cell',
    );
    await page.getByRole('searchbox').fill('mitoch');
    await expect(page.locator('.hits li')).not.toHaveCount(0);
    await shoot(page, 'browse-landing');
  });

  test('ontology-metrics', async ({ page }) => {
    await page.goto(`/ui/workbench/ontology?ds=${BROWSE_DATASET}&tab=header`);
    const metrics = page.getByRole('region', { name: 'Ontology metrics', exact: true });
    await metrics.getByRole('button', { name: 'Read metrics', exact: true }).click();
    await expect(metrics).toContainText('DL expressivity bound');
    await metrics.scrollIntoViewIfNeeded();
    await shoot(page, 'ontology-metrics');
  });

  test('browse-phone', async ({ browser }) => {
    const ctx = await browser.newContext({
      viewport: { width: 390, height: 844 },
      deviceScaleFactor: 2,
      isMobile: true,
      hasTouch: true,
      colorScheme: 'dark',
      baseURL: process.env.SPARKLES_SCREENSHOTS_URL,
    });
    await ctx.addInitScript(() => localStorage.setItem('sparkles.theme', 'dark'));
    const page = await ctx.newPage();
    await page.goto(term);
    await expect(page.getByRole('heading', { level: 1, name: 'mitochondrion' })).toBeVisible();
    await expect(page.getByRole('complementary', { name: 'Hierarchy' })).toContainText(
      'cellular component',
    );
    await shoot(page, 'browse-phone');
    await ctx.close();
  });

  // edit mode (item 630): the page's sections as the editors' forms, a changed definition
  // ready to preview, its preview, and the forms on a phone with the bottom edit bar
  test('browse-edit', async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 1000 });
    await page.goto(`${term}&edit=1`);
    const def = page.getByRole('textbox', { name: 'Definition', exact: true });
    await expect(def).toHaveValue(/self-replicating organelle/);
    await def.fill(
      "A self-replicating organelle of the cytoplasm of eukaryotic cells, where most of the cell's respiration and ATP synthesis take place.",
    );
    await page
      .getByRole('textbox', { name: 'New synonym', exact: true })
      .fill('mitochondrial organelle');
    await shoot(page, 'browse-edit');
  });

  test('browse-edit-preview', async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(`${term}&edit=1`);
    const def = page.getByRole('textbox', { name: 'Definition', exact: true });
    await expect(def).toHaveValue(/self-replicating organelle/);
    await def.fill(
      "A self-replicating organelle of the cytoplasm of eukaryotic cells, where most of the cell's respiration and ATP synthesis take place.",
    );
    await page
      .getByRole('group', { name: /^Definition/ })
      .getByRole('button', { name: 'Preview change' })
      .click();
    await expect(page.getByRole('dialog').locator('[data-outcome]')).toHaveAttribute(
      'data-outcome',
      'commit',
    );
    await shoot(page, 'browse-edit-preview');
  });

  test('browse-edit-phone', async ({ browser }) => {
    const ctx = await browser.newContext({
      viewport: { width: 390, height: 844 },
      deviceScaleFactor: 2,
      isMobile: true,
      hasTouch: true,
      colorScheme: 'dark',
      baseURL: process.env.SPARKLES_SCREENSHOTS_URL,
    });
    await ctx.addInitScript(() => localStorage.setItem('sparkles.theme', 'dark'));
    const page = await ctx.newPage();
    await page.goto(`${term}&edit=1`);
    await expect(page.getByRole('toolbar', { name: 'Edit mode' })).toBeVisible();
    await expect(page.getByRole('textbox', { name: 'Definition', exact: true })).toHaveValue(
      /self-replicating/,
    );
    await shoot(page, 'browse-edit-phone');
    await ctx.close();
  });
});
