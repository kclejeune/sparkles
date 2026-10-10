// The README's screenshots of the UI, in dark mode, against the demo dataset that
// global-setup.ts serves: each test drives one page to an interesting state, waits until
// nothing moves any more (fonts, spinners, map and graph layouts), and writes
// docs/images/<name>.png. Run with `mise run docs:screenshots`.

import { test as base, expect, type Locator, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { settle, shoot } from '../shoot';
import { DATASET, DEMO } from './global-setup';

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
  await panel.getByRole('tab', { name: 'ShEx' }).click();
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
