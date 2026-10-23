// The map of relations at its caps (docs/specs/U11-linked-views.md, section 4): a result of
// 20,000 rows with a geometry in each of two columns draws 20,000 arcs before aggregation.
// The test checks that the caps hold and records the layout time and the frame times of a
// pan, at the world view (arcs merged by clustering) and zoomed in until none merge. The
// frame times come from headless Chromium's software GL, so they are an upper bound for a
// machine with a GPU. The numbers are written to target/audits/u11-phase1/frame-times.json.

import { expect as baseExpect, test, type Locator } from '@playwright/test';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

const expect = baseExpect.configure({ timeout: 60_000 });
const OUT = resolve(import.meta.dirname, '../../../target/audits/u11-phase1');
mkdirSync(OUT, { recursive: true });

test.describe.configure({ timeout: 240_000 });

const A = Array.from({ length: 100 }, (_, i) => i + 1).join(' ');
const B = Array.from({ length: 200 }, (_, i) => i + 1).join(' ');
// 100 origins and 200 destinations: 20,000 distinct arcs between 300 places
const QUERY = `PREFIX geo: <http://www.opengis.net/ont/geosparql#>
SELECT ?from ?to WHERE {
  VALUES ?a { ${A} }
  VALUES ?b { ${B} }
  BIND(STRDT(CONCAT("POINT(", STR(-170 + ?a * 3.3), " ", STR(-60 + ?a * 1.1), ")"), geo:wktLiteral) AS ?from)
  BIND(STRDT(CONCAT("POINT(", STR(170 - ?b * 1.6), " ", STR(70 - ?b * 0.6), ")"), geo:wktLiteral) AS ?to)
}`;

type Stats = {
  frames: number;
  meanMs: number;
  p50Ms: number;
  p95Ms: number;
  maxMs: number;
  fps: number;
};

/** Pan the map for `frames` frames and report the time between rendered frames. */
function pan(region: Locator, frames: number): Promise<Stats> {
  return region.evaluate(async (el, n) => {
    const m = (
      el as unknown as {
        __map: {
          panBy(d: number[], o: object): void;
          once(e: string, f: () => void): void;
        };
      }
    ).__map;
    const times: number[] = [];
    let last = performance.now();
    for (let i = 0; i < n; i++) {
      const next = new Promise<void>((r) => m.once('render', () => r()));
      m.panBy([i % 2 ? 14 : -14, 6], { duration: 0 });
      await next;
      const now = performance.now();
      times.push(now - last);
      last = now;
    }
    // the first frame pays for the tiles of the new view
    const t = times.slice(1).sort((a, b) => a - b);
    const q = (p: number) => t[Math.min(t.length - 1, Math.floor(p * t.length))];
    const mean = t.reduce((s, x) => s + x, 0) / t.length;
    return {
      frames: t.length,
      meanMs: mean,
      p50Ms: q(0.5),
      p95Ms: q(0.95),
      maxMs: t[t.length - 1],
      fps: 1000 / mean,
    };
  }, frames);
}

test('20,000 arcs: the caps hold, and the frame times of a pan are recorded', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'flows'));
  await page.goto('/ui/query');
  const editor = page.locator('.cm-content');
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText(QUERY);
  await page.locator('label.limit select').selectOption({ index: 2 });
  await page.getByRole('button', { name: /^Run\b/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('grid')).toBeVisible();
  await results.getByRole('tab', { name: 'Map' }).click();
  await results.getByRole('button', { name: 'Arcs' }).click();
  const arcmap = page.locator('.arcmap');
  const region = page.getByRole('region', { name: 'Result arcs' });
  await expect(arcmap).toHaveAttribute('data-loading', 'false');
  await expect(region).toHaveAttribute('data-state', 'ready');

  // all 20,000 rows are read as links, and the map says nothing was cut
  await expect(results.getByText(/20,000 of 20,000 rows drawn/)).toBeVisible();
  expect(await arcmap.getAttribute('data-places')).toBe('300');
  expect(Number(await arcmap.getAttribute('data-arcs'))).toBeLessThanOrEqual(20_000);

  const world = {
    arcs: Number(await arcmap.getAttribute('data-arcs')),
    clusters: Number(await arcmap.getAttribute('data-clusters')),
    layoutMs: Number(await arcmap.getAttribute('data-layout-ms')),
    pan: await pan(region, 90),
  };
  // zoomed in until no two places merge: every one of the 20,000 arcs is in the source. At
  // zoom 3.5 thousands of them cross the view, at zoom 8 they are mostly off screen.
  const zoomed = async (zoom: number, center: [number, number], minArcs: number) => {
    await region.evaluate(
      (el, o) => (el as unknown as { __map: { jumpTo(o: object): void } }).__map.jumpTo(o),
      { center, zoom },
    );
    await expect(arcmap).toHaveAttribute('data-zoom', zoom.toFixed(2));
    await expect
      .poll(async () => Number(await arcmap.getAttribute('data-arcs')))
      .toBeGreaterThan(minArcs);
    return {
      zoom,
      arcs: Number(await arcmap.getAttribute('data-arcs')),
      clusters: Number(await arcmap.getAttribute('data-clusters')),
      layoutMs: Number(await arcmap.getAttribute('data-layout-ms')),
      pan: await pan(region, 90),
    };
  };
  const mid = await zoomed(3.5, [0, 10], 15_000);
  const deep = await zoomed(8, [0, 0], 19_000);
  writeFileSync(`${OUT}/frame-times.json`, JSON.stringify({ world, mid, deep }, null, 2));
  for (const [k, v] of Object.entries({ world, mid, deep }))
    test.info().annotations.push({ type: k, description: JSON.stringify(v) });
  // software GL is slow: only a hang or a collapse fails the test
  for (const v of [world, mid, deep]) expect(v.pan.p95Ms).toBeLessThan(1000);
});

test('more than 20,000 rows draw the first 20,000 and say so', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'flows'));
  await page.goto('/ui/query');
  const editor = page.locator('.cm-content');
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  // 100 x 250 rows: the first 20,000 are read
  const C = Array.from({ length: 250 }, (_, i) => i + 1).join(' ');
  await page.keyboard.insertText(QUERY.replace(`VALUES ?b { ${B} }`, `VALUES ?b { ${C} }`));
  await page.locator('label.limit select').selectOption({ index: 2 });
  await page.getByRole('button', { name: /^Run\b/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('grid')).toBeVisible();
  await results.getByRole('tab', { name: 'Map' }).click();
  await results.getByRole('button', { name: 'Arcs' }).click();
  const arcmap = page.locator('.arcmap');
  await expect(arcmap).toHaveAttribute('data-loading', 'false');
  await expect(results.getByText(/20,000 of 25,000 rows drawn, the first 20,000/)).toBeVisible();
  await expect(results.getByText('The map draws the first 20,000 of 25,000 rows.')).toBeVisible();
});
