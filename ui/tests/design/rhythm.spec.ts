// The layout rules of the design review (ROADMAP item 633), measured against the server of
// `node scripts/seed-design.mjs` (SPARKLES_DESIGN_URL): the top bar's search sits in the
// middle of the viewport, every page starts at the gutter, a toolbar keeps one row on a
// large screen, panels side by side are equally tall, nothing scrolls sideways, and Tab
// reaches controls with a visible focus ring. `pnpm exec playwright test --config
// playwright.design.config.ts rhythm`.

import { expect, test, type Page } from '@playwright/test';

const BASE = process.env.SPARKLES_DESIGN_URL ?? 'http://127.0.0.1:36320';

const PAGES = [
  '/ui/query?ds=demo',
  '/ui/explore?ds=demo',
  '/ui/explore?ds=demo&tab=search',
  '/ui/similar?ds=demo',
  '/ui/memory?ds=demo',
  '/ui/datasets',
  '/ui/datasets/demo',
  '/ui/datasets/demo?tab=settings',
  '/ui/datasets/demo/review/draft-places',
  '/ui/datasets/demo/merge?source=draft-places',
  '/ui/admin/backups',
  '/ui/admin/server',
  '/ui/admin/tokens',
];

const WIDTHS = [
  [1440, 900],
  [900, 1000],
  [390, 844],
] as const;

async function load(page: Page, path: string) {
  await page.goto(BASE + path);
  await page.waitForLoadState('networkidle', { timeout: 20_000 }).catch(() => {});
  await page.waitForTimeout(400);
}

for (const theme of ['light', 'dark'] as const) {
  for (const width of [1440, 1920]) {
    test(`the search is centred at ${width} px (${theme})`, async ({ browser }) => {
      const ctx = await browser.newContext({
        viewport: { width, height: 900 },
        colorScheme: theme,
      });
      const page = await ctx.newPage();
      for (const path of ['/ui/query?ds=demo', '/ui/datasets', '/ui/explore?ds=demo']) {
        await load(page, path);
        const box = await page.locator('.scope-search .field').boundingBox();
        expect(box, path).not.toBeNull();
        const centre = box!.x + box!.width / 2;
        expect(Math.abs(centre - width / 2), `${path}: search centre`).toBeLessThanOrEqual(4);
      }
      await ctx.close();
    });
  }
}

for (const [w, h] of WIDTHS) {
  test(`pages keep the gutter, one toolbar row and equal panels at ${w} px`, async ({
    browser,
  }) => {
    test.setTimeout(180_000);
    const phone = w < 761;
    const ctx = await browser.newContext({
      viewport: { width: w, height: h },
      isMobile: phone,
      hasTouch: phone,
    });
    const page = await ctx.newPage();
    for (const path of PAGES) {
      await load(page, path);
      const m = await page.evaluate(() => {
        const main = document.querySelector('main')!;
        const gutter = parseFloat(
          getComputedStyle(document.documentElement).getPropertyValue('--gutter'),
        );
        const left = main.getBoundingClientRect().left;
        // the first piece of content: a page container's first child, or a workspace's
        // first toolbar or tab strip
        const first =
          main.querySelector('.page-container > :first-child') ??
          main.querySelector(
            '.toolbar > :first-child, .bar .tabs > :first-child, .qtabs > :first-child',
          );
        const edge = first ? first.getBoundingClientRect().left - left : null;
        // controls and text that start inside the gutter: neither full-bleed nor on it
        const inset = [
          ...main.querySelectorAll<HTMLElement>(
            'input, select, textarea, button, h1, h2, h3, p, table, .panel, .toolbar > *',
          ),
        ]
          .filter((e) => e.offsetParent && e.getBoundingClientRect().width)
          .map((e) => [e, e.getBoundingClientRect().left - left] as const)
          .filter(([, x]) => x > 0.5 && x < gutter - 0.5)
          .map(
            ([e, x]) => `${e.tagName.toLowerCase()}.${e.className.split(' ')[0]}@${Math.round(x)}`,
          )
          .slice(0, 5);
        // the rows of each toolbar's children
        const toolbars = [...main.querySelectorAll<HTMLElement>('.toolbar')]
          .filter((t) => t.offsetParent)
          .map((t) => {
            const tops = new Set(
              [...t.children]
                .filter((c) => (c as HTMLElement).offsetParent && c.getBoundingClientRect().width)
                .map((c) => Math.round(c.getBoundingClientRect().top / 8)),
            );
            return tops.size;
          });
        // panels side by side in a .panel-grid: the same height within a row
        const uneven: string[] = [];
        for (const g of main.querySelectorAll('.panel-grid')) {
          const rows = new Map<number, number[]>();
          for (const c of g.children) {
            const r = c.getBoundingClientRect();
            const k = Math.round(r.top);
            rows.set(k, [...(rows.get(k) ?? []), Math.round(r.height)]);
          }
          for (const hs of rows.values())
            if (Math.max(...hs) - Math.min(...hs) > 1) uneven.push(hs.join('/'));
        }
        const doc = document.documentElement;
        return {
          gutter,
          edge,
          inset,
          toolbars,
          uneven,
          overflow: Math.max(
            doc.scrollWidth - doc.clientWidth,
            main.scrollWidth - main.clientWidth,
          ),
        };
      });
      expect(m.overflow, `${path}: sideways scroll`).toBeLessThanOrEqual(0);
      if (m.edge != null)
        expect(Math.abs(m.edge - m.gutter), `${path}: content edge`).toBeLessThanOrEqual(1);
      expect.soft(m.inset, `${path}: inside the gutter`).toEqual([]);
      for (const rows of m.toolbars)
        expect(rows, `${path}: toolbar rows`).toBeLessThanOrEqual(phone ? 3 : 1);
      expect(m.uneven, `${path}: panel heights`).toEqual([]);
    }
    await ctx.close();
  });
}

test('Tab reaches the controls with a visible focus ring', async ({ browser }) => {
  test.setTimeout(180_000);
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();
  for (const path of PAGES) {
    await load(page, path);
    await page.locator('body').click({ position: { x: 1, y: 1 } });
    const missing: string[] = [];
    for (let i = 0; i < 30; i++) {
      await page.keyboard.press('Tab');
      const r = await page.evaluate(() => {
        const a = document.activeElement as HTMLElement | null;
        if (!a || a === document.body) return null;
        // a ring is what focus adds: the element's or its field's shadow or outline differs
        // from the same element unfocused, so a static shadow such as the active link's
        // border does not count
        const els = [a, a.parentElement, a.parentElement?.parentElement].filter(
          (e): e is HTMLElement => !!e,
        );
        // (a skip link counts by appearing: its box moves into view on focus)
        const look = () => [
          JSON.stringify(a.getBoundingClientRect()),
          ...els.map((e) => {
            const s = getComputedStyle(e);
            return `${s.boxShadow}|${s.outlineStyle}|${s.outlineWidth}|${s.outlineColor}`;
          }),
        ];
        const focused = look();
        a.blur();
        const unfocused = look();
        a.focus({ focusVisible: true } as FocusOptions);
        const ok = focused.some((f, i) => f !== unfocused[i]) || a.closest('.cm-editor') != null;
        const label = `${a.tagName.toLowerCase()} ${a.getAttribute('aria-label') ?? a.textContent?.trim().slice(0, 30) ?? ''}`;
        return { ok, label };
      });
      if (r && !r.ok) missing.push(r.label);
    }
    expect(missing, `${path}: focused without a ring`).toEqual([]);
  }
  await ctx.close();
});

test("Explore's graph tools stay inside a narrow canvas", async ({ browser }) => {
  // 1001 px is just above the width where the details panel stacks under the canvas
  for (const width of [1001, 1100, 1440]) {
    const ctx = await browser.newContext({ viewport: { width, height: 800 } });
    const page = await ctx.newPage();
    await load(page, `/ui/explore?ds=demo&iri=${encodeURIComponent('http://example.org/ns#City')}`);
    for (const facets of [false, true]) {
      if (facets) await page.getByRole('button', { name: 'Facets', exact: true }).click();
      await page.waitForTimeout(300);
      const clipped = await page.evaluate(() => {
        const t = document.querySelector('.gtools')!.getBoundingClientRect();
        return [...document.querySelectorAll<HTMLElement>('.gtools button, .gtools .note')]
          .filter((e) => e.offsetParent && e.getBoundingClientRect().right > t.right + 0.5)
          .map((e) => e.getAttribute('aria-label') ?? e.textContent?.trim());
      });
      expect(clipped, `${width} px${facets ? ' with Facets' : ''}`).toEqual([]);
    }
    await ctx.close();
  }
});

const STRIP_PAGES = [
  ...PAGES,
  '/ui/ontology?ds=go',
  '/ui/thesaurus?ds=unesco',
  '/ui/browse?ds=go',
  `/ui/browse?ds=go&iri=${encodeURIComponent('http://purl.obolibrary.org/obo/GO_0005739')}`,
];

for (const [w, h] of WIDTHS) {
  test(`tab strips keep one row and page actions one height at ${w} px`, async ({ browser }) => {
    test.setTimeout(180_000);
    const phone = w < 761;
    const ctx = await browser.newContext({
      viewport: { width: w, height: h },
      isMobile: phone,
      hasTouch: phone,
    });
    const page = await ctx.newPage();
    for (const path of STRIP_PAGES) {
      await load(page, path);
      const m = await page.evaluate(() => {
        const main = document.querySelector('main')!;
        const wrapped = [...main.querySelectorAll<HTMLElement>('.tabs')]
          .filter((t) => t.offsetParent)
          .filter((t) => {
            const tops = new Set(
              [...t.querySelectorAll<HTMLElement>(':scope > [role="tab"], :scope > .tab')]
                .filter((c) => c.offsetParent)
                .map((c) => Math.round(c.getBoundingClientRect().top)),
            );
            return tops.size > 1;
          })
          .map((t) => t.getAttribute('aria-label') ?? t.className);
        // the controls of a page's actions row, not its menus
        const heights = [...main.querySelectorAll<HTMLElement>('.page-actions')]
          .filter((a) => a.offsetParent)
          .map((a) => [
            ...new Set(
              [...a.children]
                .filter(
                  (c) =>
                    (c as HTMLElement).offsetParent &&
                    c.matches('button, a.btn, .btn, select, span, label'),
                )
                .map((c) => Math.round(c.getBoundingClientRect().height)),
            ),
          ])
          .filter((hs) => hs.length > 1);
        return { wrapped, heights };
      });
      expect.soft(m.wrapped, `${path}: wrapped tab strips`).toEqual([]);
      expect.soft(m.heights, `${path}: page action heights`).toEqual([]);
    }
    await ctx.close();
  });
}
