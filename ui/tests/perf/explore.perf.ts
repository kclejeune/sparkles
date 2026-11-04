// The Explore measurements (ROADMAP item 725): a graph grown to 1,000, 5,000 and 20,000
// explored nodes in the `hub` dataset (a 90-ary tree, so each expansion adds about 90
// nodes), the frame times of a synthetic pan and zoom over it, the latency of an expansion
// and the heap, and the fan-out of a resource with 20,000 neighbours.
//
//   PERF_URL=http://127.0.0.1:PORT PERF_OUT=DIR PERF_LABEL=baseline \
//     pnpm exec playwright test --config playwright.perf.config.ts explore
//
// The graph is read and driven through `window.__perfHook` (tests/perf/adapter.ts). The
// baseline stops growing at its own limit of 1,500 nodes, which is recorded as `blockedAt`.

import { test, type Page } from '@playwright/test';
import { exploreUrl, GRAPH_CANVAS_JS, INSTALL_HOOK_JS, type PerfHook } from './adapter';
import {
  BASE,
  FRAME_STATS_JS,
  frameSummary,
  heap,
  LONGTASK_INIT,
  LOAD_TIMEOUT,
  longTasks,
  calm,
  median,
  need,
  Net,
  REPEAT,
  REP_BASE,
  save,
} from './measure';

const EX = 'http://example.org/perf#';
const TARGETS = (process.env.PERF_EXPLORE_TARGETS ?? '1000,5000,20000').split(',').map(Number);

declare global {
  interface Window {
    __perfHook?: PerfHook;
  }
}

async function hook(page: Page) {
  await page.waitForFunction(INSTALL_HOOK_JS, null, { timeout: LOAD_TIMEOUT });
}
const count = (page: Page) => page.evaluate(() => window.__perfHook!.nodeCount());

/** Pan by dragging and zoom with the wheel, a frame at a time, on the graph's canvas. */
const PAN_ZOOM = (cx: number, cy: number) => `(() => {
  const el = () => document.elementFromPoint(${cx}, ${cy});
  let down = false;
  const fire = (type, x, y, extra = {}) => {
    const t = el();
    if (!t) return;
    const base = { bubbles: true, cancelable: true, clientX: x, clientY: y, view: window, ...extra };
    t.dispatchEvent(type === 'wheel' ? new WheelEvent(type, base) : new MouseEvent(type, base));
  };
  return (i) => {
    const phase = Math.floor(i / 40) % 2;
    if (!down) { fire('mousedown', ${cx}, ${cy}, { button: 0, buttons: 1 }); down = true; }
    const dx = Math.sin(i / 20) * 120;
    const dy = Math.cos(i / 25) * 80;
    fire('mousemove', ${cx} + dx, ${cy} + dy, { buttons: 1 });
    fire('wheel', ${cx} + dx, ${cy} + dy, { deltaY: phase ? 100 : -100, deltaMode: 0 });
    return false;
  };
})()`;

test.describe('explore scale', () => {
  test.beforeAll(need);
  for (const target of TARGETS) {
    test(`explore-${target}`, async ({ browser }, info) => {
      for (let rep = 1; rep <= REPEAT; rep++) {
        await calm();
        const ctx = await browser.newContext({
          bypassCSP: true,
          viewport: { width: 1440, height: 900 },
        });
        const page = await ctx.newPage();
        await page.addInitScript(LONGTASK_INIT);
        // the changed page installs window.__perfHook only when asked
        await page.addInitScript("localStorage.setItem('sparkles.explore.perfHook', '1')");
        const cdp = await ctx.newCDPSession(page);
        const net = new Net(cdp);
        await net.start();
        const r: Record<string, unknown> = { target, status: 'ok' };
        try {
          const t0 = Date.now();
          await page.goto(exploreUrl(BASE, 'hub', `${EX}tree`));
          await hook(page);
          await page.waitForFunction(() => window.__perfHook!.nodeCount() > 1, null, {
            timeout: LOAD_TIMEOUT,
          });
          r.firstGraphMs = Date.now() - t0;
          r.renderer = await page.evaluate(() => window.__perfHook!.renderer());
          // grow the graph breadth first by expanding the nodes it holds, one at a time
          const latencies: number[] = [];
          const done = new Set<string>([`<${EX}tree>`]);
          let stuck = 0;
          const grow = Date.now();
          while ((await count(page)) < target && stuck < 3) {
            const ids = await page.evaluate(() => window.__perfHook!.nodeIds());
            const next = ids.find(
              (id) => !done.has(id) && /perf#t\d+>$/.test(id) && !id.includes('#big'),
            );
            if (!next) break;
            done.add(next);
            const before = await count(page);
            const t = Date.now();
            await page.evaluate((id) => window.__perfHook!.expand(id), next);
            latencies.push(Date.now() - t);
            stuck = (await count(page)) > before ? 0 : stuck + 1;
          }
          r.growMs = Date.now() - grow;
          r.nodes = await count(page);
          r.drawn = await page.evaluate(() => window.__perfHook!.drawnCount());
          r.reached = (r.nodes as number) >= target;
          if (!r.reached) r.blockedAt = r.nodes;
          r.expansions = latencies.length;
          r.expandLatencyMs = { median: median(latencies), max: Math.max(0, ...latencies) };
          await page.waitForTimeout(1500);
          // pan and zoom
          const c = (await page.evaluate(GRAPH_CANVAS_JS)) as { x: number; y: number } | null;
          if (c) {
            r.viewportBefore = await page.evaluate(() => window.__perfHook!.viewport?.());
            const dts = (await page.evaluate(FRAME_STATS_JS(PAN_ZOOM(c.x, c.y), 5000))) as number[];
            r.panZoom = frameSummary(dts);
            r.viewportAfter = await page.evaluate(() => window.__perfHook!.viewport?.());
          }
          // an idle page: how fast frames come with nothing moving
          r.idle = frameSummary(
            (await page.evaluate(FRAME_STATS_JS('() => false', 2000))) as number[],
          );
          r.longTasks = await longTasks(page);
          r.network = net.summary().api;
          r.heap = await heap(cdp);
          await page.screenshot({
            path: `${process.env.PERF_OUT ?? '/tmp'}/${process.env.PERF_LABEL ?? 'run'}/explore-${target}.rep${rep + REP_BASE}.png`,
          });
        } catch (e) {
          r.status = 'failed';
          r.error = String(e).split('\n')[0];
        }
        await save(page, info, `explore-${target}`, rep, r);
        await ctx.close();
      }
    });
  }

  test('explore-fanout-20000', async ({ browser }, info) => {
    for (let rep = 1; rep <= REPEAT; rep++) {
      await calm();
      const ctx = await browser.newContext({
        bypassCSP: true,
        viewport: { width: 1440, height: 900 },
      });
      const page = await ctx.newPage();
      await page.addInitScript(LONGTASK_INIT);
      // the changed page installs window.__perfHook only when asked
      await page.addInitScript("localStorage.setItem('sparkles.explore.perfHook', '1')");
      const cdp = await ctx.newCDPSession(page);
      const net = new Net(cdp);
      await net.start();
      const r: Record<string, unknown> = { status: 'ok' };
      try {
        const t0 = Date.now();
        await page.goto(exploreUrl(BASE, 'hub', `${EX}big`));
        await hook(page);
        await page.waitForFunction(() => window.__perfHook!.nodeCount() > 1, null, {
          timeout: LOAD_TIMEOUT,
        });
        r.firstGraphMs = Date.now() - t0;
        await net.quiet();
        r.nodes = await count(page);
        r.drawn = await page.evaluate(() => window.__perfHook!.drawnCount());
        r.renderer = await page.evaluate(() => window.__perfHook!.renderer());
        r.network = net.summary().api;
        r.requests = net.summary().requests.filter((q) => q.group !== 'static');
        r.heap = await heap(cdp);
        r.longTasks = await longTasks(page);
      } catch (e) {
        r.status = 'failed';
        r.error = String(e).split('\n')[0];
      }
      await save(page, info, 'explore-fanout-20000', rep, r);
      await ctx.close();
    }
  });
});
