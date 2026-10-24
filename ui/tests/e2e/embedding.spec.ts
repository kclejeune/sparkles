// The embedding map against a real server (docs/specs/U11-linked-views.md, acceptance
// examples A6 and A8). A6 builds an index of 10,000 vectors in three separated clusters,
// one class each, and checks that the PCA projection keeps each class in its own region and
// that the map colours them by class. A8 gives a caller a grant on one graph of a dataset
// with an index, and checks that the projection routes answer 403 and that the UI offers no
// map while the facets still work.

import type { APIRequestContext } from '@playwright/test';
import { anonymous, expect, open, server } from './fixtures';

const EX = 'http://example.org/embedding/';
const DIM = 16;
const CLASSES = ['Alpha', 'Beta', 'Gamma'];

/** A unit-ish vector near the centre of `cluster`, with deterministic noise. */
function vector(cluster: number, i: number): number[] {
  return Array.from({ length: DIM }, (_, d) => {
    const centre = d % 3 === cluster ? 1 : 0;
    const noise = 0.08 * Math.sin(i * 12.9898 + d * 78.233);
    return Math.round((centre + noise) * 10000) / 10000;
  });
}

function turtle(n: number): string {
  const lines = [`@prefix ex: <${EX}> .`];
  for (let i = 0; i < n; i++) {
    const c = i % 3;
    lines.push(
      `ex:e${i} a ex:${CLASSES[c]} ; ex:rank ${i} ; ex:vec "[${vector(c, i).join(',')}]"^^<urn:x-sparkles:vector> .`,
    );
  }
  return lines.join('\n');
}

async function makeIndexed(
  request: APIRequestContext,
  name: string,
  n: number,
  headers: Record<string, string> = {},
) {
  const made = await request.post('/$/datasets', {
    data: { dbName: name, dbType: 'mem' },
    headers,
  });
  expect(made.ok() || made.status() === 409, await made.text()).toBe(true);
  const up = await request.post(`/${name}/data`, {
    headers: { ...headers, 'Content-Type': 'text/turtle' },
    data: turtle(n),
  });
  expect(up.ok(), await up.text()).toBe(true);
  const put = await request.put(`/$/vector/${name}/vec`, {
    headers,
    data: { predicate: `${EX}vec`, dimension: DIM, metric: 'cosine' },
  });
  expect(put.ok(), await put.text()).toBe(true);
  await expect
    .poll(
      async () => {
        const r = await request.get(`/$/vector/${name}/vec`, { headers });
        return r.ok() ? (await r.json()).state : String(r.status());
      },
      { timeout: 60_000 },
    )
    .toBe('ready');
}

open.describe.configure({ mode: 'serial' });

open(
  'A6. the PCA projection keeps each class in its own region, and the map colours by class',
  async ({ page, request }) => {
    open.setTimeout(120_000);
    const NAME = 'embedding-clusters';
    await makeIndexed(request, NAME, 10_000);
    const made = await request.post(`/$/vector/${NAME}/vec/projection?method=pca`, { data: {} });
    expect(made.status(), await made.text()).toBe(200);
    const r = await request.get(`/$/vector/${NAME}/vec/projection`);
    expect(r.ok(), await r.text()).toBe(true);
    const p = await r.json();
    expect(p.entities).toBe(10_000);
    expect(p.points).toHaveLength(10_000);
    expect(p.region.sampled).toBe(false);
    const classes: string[] = p.classes.map((c: { iri: string }) => c.iri);
    expect([...classes].sort()).toEqual(CLASSES.map((c) => EX + c).sort());
    // the class of each point is its entity's class
    type P = { entity: string; x: number; y: number; class: number };
    for (const q of p.points as P[]) {
      const i = Number(q.entity.slice(EX.length + 1));
      expect(classes[q.class]).toBe(EX + CLASSES[i % 3]);
    }
    // every point is nearer its own class's centre than any other's, so a lasso around each
    // centre separates the classes
    const centres = classes.map((_, c) => {
      const own = (p.points as P[]).filter((q) => q.class === c);
      return {
        x: own.reduce((s, q) => s + q.x, 0) / own.length,
        y: own.reduce((s, q) => s + q.y, 0) / own.length,
      };
    });
    const nearest = (q: P) =>
      centres
        .map((m, c) => ({ c, d: (q.x - m.x) ** 2 + (q.y - m.y) ** 2 }))
        .sort((a, b) => a.d - b.d)[0].c;
    const wrong = (p.points as P[]).filter((q) => nearest(q) !== q.class);
    expect(wrong).toHaveLength(0);

    await page.goto(`/ui/embedding?ds=${NAME}&index=vec`);
    const map = page.locator('.emap[data-ready="true"]');
    await expect(map).toHaveAttribute('data-points', '10000', { timeout: 30_000 });
    await expect(page.getByTestId('facts')).toContainText('10,000 entities');
    const legend = page.getByRole('complementary', { name: 'Legend' });
    await expect(legend.locator('li')).toHaveCount(3);
    for (const c of CLASSES) await expect(legend).toContainText(c);
  },
);

anonymous(
  'A8. a caller limited to one graph gets 403 from the projection routes and no map, while the facets work',
  async ({ browser, request }) => {
    anonymous.setTimeout(120_000);
    const NAME = 'embedding-limited';
    const admin = { Authorization: `Bearer ${server.token}` };
    await makeIndexed(request, NAME, 300, admin);
    const ctx = await browser.newContext({
      baseURL: server.url,
      extraHTTPHeaders: { Authorization: `Bearer ${server.limitedToken}` },
    });
    try {
      const page = await ctx.newPage();
      expect((await page.request.get(`/$/vector/${NAME}/vec/projection`)).status()).toBe(403);
      expect(
        (await page.request.post(`/$/vector/${NAME}/vec/projection`, { data: {} })).status(),
      ).toBe(403);
      // the facets read under the caller's view and still answer
      const f = await page.request.post(`/$/graph/${NAME}/facets`, {
        data: { within: { class: `${EX}Alpha` }, facets: [{ property: `${EX}rank` }] },
      });
      expect(f.ok(), await f.text()).toBe(true);
      expect((await f.json()).size).toBe(100);
      // Explore offers no map, and the page says why
      await page.goto(`/ui/explore?ds=${NAME}&iri=${encodeURIComponent(`${EX}e0`)}`);
      await expect(page.getByText(/\d+ nodes, \d+ edges/)).toBeVisible({ timeout: 20_000 });
      await expect(page.getByRole('link', { name: 'Embedding map' })).toHaveCount(0);
      await page.goto(`/ui/embedding?ds=${NAME}&index=vec`);
      await expect(page.locator('[data-state="forbidden"]')).toContainText(
        'Embedding maps need access to every graph',
      );
    } finally {
      await ctx.close();
    }
  },
);
