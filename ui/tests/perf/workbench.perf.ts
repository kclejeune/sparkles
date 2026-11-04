// The Workbench measurements (ROADMAP item 725): the open of the ontology and thesaurus
// editors, selection reveal, and a scroll through 10,000 tree rows.
//
//   PERF_URL=http://127.0.0.1:PORT PERF_OUT=DIR PERF_LABEL=baseline \
//     pnpm exec playwright test --config playwright.perf.config.ts workbench

import { test } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import {
  FIRST_EXPANDABLE,
  loadedRows,
  ontologyUrl,
  rowOf,
  rowsInDom,
  SCROLLER_JS,
  SHOW_MORE,
  twistyOf,
  thesaurusUrl,
  TREE_ROW,
} from './adapter';
import {
  BASE,
  FRAME_STATS_JS,
  frameSummary,
  heap,
  LONGTASK_INIT,
  LOAD_TIMEOUT,
  longTasks,
  calm,
  Net,
  need,
  REPEAT,
  save,
} from './measure';

type Sidecar = {
  root: string;
  hubs: { iri: string; children: number }[];
  deepest: { iri: string };
  pastFirstPage: { iri: string; parent: string };
  hubPath: string[];
  hubChild: string;
};
const sidecar = (classes: number): Sidecar | null => {
  try {
    return JSON.parse(
      readFileSync(
        join(homedir(), `.cache/sparkles-ui-seed/taxon-synthetic-${classes}.json`),
        'utf8',
      ),
    ) as Sidecar;
  } catch {
    return null;
  }
};
/** The classes of each synthetic taxon dataset. */
const TAXON_SIZE: Record<string, number> = {
  taxon: 2_600_000,
  taxon1m: 1_000_000,
  taxon300k: 300_000,
};
/** The dataset of the taxon cases of an open and a reveal (PERF_TAXON_DS, default the 2.6 million). */
const TAXON_DS = process.env.PERF_TAXON_DS ?? 'taxon';
const taxon = sidecar(TAXON_SIZE[TAXON_DS] ?? 2_600_000);
/** The dataset of the scroll case: the 300,000 class one opens in the baseline too. */
const SCROLL_DS = process.env.PERF_SCROLL_DS ?? 'taxon300k';
const scrollTaxon = sidecar(TAXON_SIZE[SCROLL_DS] ?? 1_000_000);

const CHEBI_WATER = 'http://purl.obolibrary.org/obo/CHEBI_15377';

type Case = { name: string; url: string; wait: string };
const cases = (): Case[] => {
  const out: Case[] = [
    { name: 'open-chebi', url: ontologyUrl(BASE, 'chebi'), wait: TREE_ROW },
    {
      name: 'reveal-chebi-water',
      url: ontologyUrl(BASE, 'chebi', `&iri=${encodeURIComponent(CHEBI_WATER)}`),
      wait: rowOf(CHEBI_WATER),
    },
    { name: `open-${TAXON_DS}`, url: ontologyUrl(BASE, TAXON_DS), wait: TREE_ROW },
    { name: 'open-unesco-thesaurus', url: thesaurusUrl(BASE, 'unesco'), wait: TREE_ROW },
  ];
  if (taxon) {
    out.push(
      {
        name: `reveal-${TAXON_DS}-deepest`,
        url: ontologyUrl(BASE, TAXON_DS, `&iri=${encodeURIComponent(taxon.deepest.iri)}`),
        wait: rowOf(taxon.deepest.iri),
      },
      {
        name: `reveal-${TAXON_DS}-past-first-page`,
        url: ontologyUrl(BASE, TAXON_DS, `&iri=${encodeURIComponent(taxon.pastFirstPage.iri)}`),
        wait: rowOf(taxon.pastFirstPage.iri),
      },
    );
  }
  return out;
};

test.describe('workbench open', () => {
  test.beforeAll(need);
  const only = process.env.PERF_ONLY?.split(',');
  for (const c of cases()) {
    if (only && !only.includes(c.name)) continue;
    test(c.name, async ({ browser }, info) => {
      for (let rep = 1; rep <= REPEAT; rep++) {
        await calm();
        const ctx = await browser.newContext({
          bypassCSP: true,
          viewport: { width: 1440, height: 900 },
        });
        const page = await ctx.newPage();
        await page.addInitScript(LONGTASK_INIT);
        const cdp = await ctx.newCDPSession(page);
        const net = new Net(cdp);
        await net.start();
        const r: Record<string, unknown> = { url: c.url.replace(BASE, ''), status: 'ok' };
        page.on('crash', () => (r.crashed = true));
        page.on('pageerror', (e) => (r.pageError ??= String(e.message).slice(0, 200)));
        try {
          await page.goto(c.url, { waitUntil: 'commit' });
          await page.locator(c.wait).first().waitFor({ state: 'visible', timeout: LOAD_TIMEOUT });
          r.firstRowMs = Math.round((await page.evaluate('performance.now()')) as number);
          const quiet = await net.quiet();
          r.quietMs = Math.round((await page.evaluate('performance.now()')) as number);
          r.networkQuiet = quiet;
          const lt = await longTasks(page);
          r.longTasks = lt;
          // interactive: the row is drawn and the main thread has been free since
          r.interactiveMs = Math.max(r.firstRowMs as number, lt.lastEndMs);
          const n = net.summary();
          r.network = { api: n.api, total: n.total, groups: n.groups };
          r.requests = n.requests.filter((q) => q.group !== 'static');
          r.heap = await heap(cdp);
          r.rowsLoaded = await loadedRows(page);
          r.rowsInDom = await rowsInDom(page);
          // after the load: how fast a click on a twisty answers, with the row flipping
          try {
            const tw = page.locator(FIRST_EXPANDABLE).first();
            if (await tw.count()) {
              const li = await tw.locator('xpath=ancestor::li[1]').elementHandle();
              const before = await li!.getAttribute('aria-expanded');
              const t = Date.now();
              await tw.click({ timeout: 30_000 });
              await page.waitForFunction(
                ([el, b]) => (el as Element).getAttribute('aria-expanded') !== b,
                [li, before],
                { timeout: 30_000 },
              );
              r.toggleLatencyMs = Date.now() - t;
            }
          } catch (e) {
            r.toggleError = String(e).split('\n')[0];
          }
        } catch (e) {
          r.status = 'failed';
          r.error = String(e).split('\n')[0];
          r.partial = {
            firstRowMs: r.firstRowMs ?? null,
            network: net.summary().total,
            requests: net.summary().requests.filter((q) => q.group !== 'static'),
          };
        }
        await save(page, info, c.name, rep, r);
        await ctx.close();
      }
    });
  }
});

test.describe('workbench scroll', () => {
  test.beforeAll(need);
  test.skip(!scrollTaxon, 'the synthetic taxon sidecar is missing');
  test(`scroll-${SCROLL_DS}-10k`, async ({ browser }, info) => {
    const hub = scrollTaxon!.hubs[0].iri;
    for (let rep = 1; rep <= REPEAT; rep++) {
      await calm();
      const ctx = await browser.newContext({
        bypassCSP: true,
        viewport: { width: 1440, height: 900 },
      });
      const page = await ctx.newPage();
      await page.addInitScript(LONGTASK_INIT);
      const cdp = await ctx.newCDPSession(page);
      const r: Record<string, unknown> = { status: 'ok', hub };
      try {
        // select the parent of the largest family, so that the tree opens down to it, open it, scroll
        // to the family and open that. Selecting the family itself would read its entity frame,
        // which lists its subclass axioms.
        const path = scrollTaxon!.hubPath;
        const parent = path[path.length - 2];
        await page.goto(ontologyUrl(BASE, SCROLL_DS, `&iri=${encodeURIComponent(parent)}`));
        await page.locator(rowOf(parent)).first().waitFor({ timeout: LOAD_TIMEOUT });
        if ((await page.locator(rowOf(parent)).first().getAttribute('aria-expanded')) !== 'true')
          await page.locator(twistyOf(parent)).first().click();
        for (let i = 0; i < 400 && !(await page.locator(rowOf(hub)).count()); i++) {
          await page.evaluate(`(() => { const s = ${SCROLLER_JS}; s.scrollTop += 300; })()`);
          await page.waitForTimeout(30);
        }
        await page.locator(twistyOf(hub)).first().click({ timeout: 30_000 });
        const pages: number[] = [];
        // read pages of the hub's children until 10,000 rows are loaded
        for (let i = 0; i < 14; i++) {
          // let the level that is being read finish, so that the click is not lost
          let rows = await loadedRows(page);
          for (let k = 0; k < 100; k++) {
            await page.waitForTimeout(500);
            const now = await loadedRows(page);
            if (now === rows) break;
            rows = now;
          }
          if (rows >= 10_000) break;
          // a virtual tree draws the "Show more" row only when it is in view, and a level
          // that is still being read grows under the scroll, so keep going to the bottom
          const more = page.locator(SHOW_MORE).first();
          for (let k = 0; k < 300 && !(await more.count()); k++) {
            await page.evaluate(
              `(() => { const s = ${SCROLLER_JS}; s.scrollTop = s.scrollHeight; })()`,
            );
            await page.waitForTimeout(200);
          }
          if (!(await more.count())) break;
          const t = Date.now();
          await more.click();
          const until = Date.now() + 20_000;
          while ((await loadedRows(page)) <= rows && Date.now() < until)
            await page.waitForTimeout(50);
          if ((await loadedRows(page)) <= rows) break;
          pages.push(Date.now() - t);
          if (process.env.PERF_DEBUG)
            console.log('show more', i, pages.at(-1), await loadedRows(page));
        }
        r.showMoreMs = pages;
        r.rowsLoaded = await loadedRows(page);
        r.rowsInDomBefore = await rowsInDom(page);
        r.heapBefore = await heap(cdp);
        // scroll the tree from top to bottom and back, 700 px a frame
        await page.evaluate(`(() => { const s = ${SCROLLER_JS}; s.scrollTop = 0; })()`);
        await page.waitForTimeout(500);
        const down = (await page.evaluate(
          FRAME_STATS_JS(
            `(() => { const s = ${SCROLLER_JS}; return () => { s.scrollTop += 700; return s.scrollTop + s.clientHeight >= s.scrollHeight - 1; }; })()`,
            120_000,
          ),
        )) as number[];
        const up = (await page.evaluate(
          FRAME_STATS_JS(
            `(() => { const s = ${SCROLLER_JS}; return () => { s.scrollTop -= 700; return s.scrollTop <= 0; }; })()`,
            120_000,
          ),
        )) as number[];
        r.scrollDown = frameSummary(down);
        r.scrollUp = frameSummary(up);
        r.scrollHeightPx = await page.evaluate(`${SCROLLER_JS}.scrollHeight`);
        r.rowsInDomAfter = await rowsInDom(page);
        r.heapAfter = await heap(cdp);
        r.longTasks = await longTasks(page);
      } catch (e) {
        r.status = 'failed';
        r.error = String(e).split('\n')[0];
      }
      await save(page, info, `scroll-${SCROLL_DS}-10k`, rep, r);
      await ctx.close();
    }
  });
});
