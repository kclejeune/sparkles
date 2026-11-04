// The Ask thread against a real server (`SPARKLES_SEED_ASK=1 node scripts/seed-design.mjs`,
// SPARKLES_DESIGN_URL), whose `demo` assistant drafts through the stand-in provider of
// scripts/ask-stub.mjs, so no paid model is called. At the large, medium and phone widths
// in both themes, a question on the Ask page answers with its rows, citations and stored
// answer, a follow-up joins the thread, the history reopens it, Show rows runs the stored
// query again without a model, and the drawer opens on a Browse page with the resource as
// context, with no sideways overflow. Shots go to SPARKLES_DESIGN_OUT. Run with
// `pnpm exec playwright test --config playwright.design.config.ts design/ask.spec`.

import { expect, test, type Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { join, resolve } from 'node:path';

const BASE = process.env.SPARKLES_DESIGN_URL ?? 'http://127.0.0.1:36320';
const OUT = resolve(
  process.env.SPARKLES_DESIGN_OUT ?? join(import.meta.dirname, '../../../.artifacts/design'),
  'ask',
);
const WIDTHS = { large: [1440, 900], medium: [900, 1000], phone: [390, 844] } as const;
const ADA = 'http://example.org/person/ada-okafor';

test.describe.configure({ timeout: 120_000 });
mkdirSync(OUT, { recursive: true });

async function noOverflow(page: Page) {
  const over = await page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
  expect(over).toBeLessThanOrEqual(0);
}

async function prepare(page: Page, theme: string) {
  await page.addInitScript(
    ([t]) => {
      localStorage.setItem('sparkles.theme', t);
      localStorage.setItem('sparkles.dataset', 'demo');
      localStorage.setItem('sparkles.askPrefs.local', JSON.stringify({ mode: 'run' }));
    },
    [theme] as const,
  );
}

for (const [size, [width, height]] of Object.entries(WIDTHS))
  for (const theme of ['light', 'dark']) {
    test(`the Ask page and its history at ${size} in ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width, height });
      await prepare(page, theme);
      await page.goto(`${BASE}/ui/ask?ds=demo`);
      const t = page.getByRole('region', { name: 'Thread' });
      const box = t.getByRole('textbox', { name: 'Question' });
      await expect(box).toBeVisible({ timeout: 30_000 });
      const q = `Who are the people? (${size} ${theme})`;
      await box.fill(q);
      await box.press('Enter');
      const a = t.getByRole('article', { name: q });
      await expect(a.getByRole('group', { name: 'Feedback' })).toBeVisible({ timeout: 60_000 });
      await expect(a).toContainText('The network lists its people');
      await a.getByRole('button', { name: /^Rows/ }).click();
      await expect(a.getByRole('grid')).toContainText('Ada Okafor');
      await a.getByRole('button', { name: 'Citation [1]' }).click();
      await expect(a.getByText('Cited rows', { exact: true })).toBeVisible();
      await noOverflow(page);
      await page.screenshot({ path: join(OUT, `page-${size}-${theme}.png`) });

      // a follow-up carries the thread
      const sent = page.waitForRequest((r) => r.url().endsWith('/demo/ask'));
      await box.fill('and their names?');
      await box.press('Enter');
      const thread = (await sent).postDataJSON().thread;
      expect(thread).toMatch(/^[0-9a-f]{16}$/);
      await expect(
        t
          .getByRole('article', { name: 'and their names?' })
          .getByRole('group', { name: 'Feedback' }),
      ).toBeVisible({ timeout: 60_000 });

      // the server keeps both turns of the thread with their answers
      const listed = await page.request.get(`${BASE}/$/asks/demo?thread=${thread}`);
      expect(listed.ok()).toBe(true);
      const records = (await listed.json()).asks;
      expect(records.length).toBe(2);
      expect(records[0].answer.summary.text).toContain('The network lists its people');

      // the history reopens it, and Show rows reruns the stored query without a model
      await page.reload();
      await expect(box).toBeVisible({ timeout: 30_000 });
      await t.getByRole('button', { name: 'New thread' }).click();
      if (size !== 'large') await page.getByRole('button', { name: 'Threads' }).click();
      await page
        .getByRole('button', { name: new RegExp(q.replace(/[()?]/g, '\\$&') + '.*2 questions') })
        .first()
        .click();
      const stored = t.getByRole('article', { name: q });
      await expect(stored).toContainText('The network lists its people');
      const asks: string[] = [];
      page.on('request', (r) => r.url().endsWith('/demo/ask') && asks.push(r.url()));
      await stored.getByRole('button', { name: /^Rows/ }).click();
      await stored.getByRole('button', { name: 'Show rows' }).click();
      await expect(stored.getByRole('grid')).toContainText('Ada Okafor', { timeout: 30_000 });
      expect(asks).toEqual([]);
      await noOverflow(page);
      await page.screenshot({ path: join(OUT, `reopened-${size}-${theme}.png`) });
    });

    test(`the drawer on a Browse page at ${size} in ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width, height });
      await prepare(page, theme);
      await page.goto(`${BASE}/ui/browse?ds=demo&iri=${encodeURIComponent(ADA)}`);
      await expect(page.locator('h1.term-title')).toContainText('Ada Okafor', { timeout: 30_000 });
      await page.locator('h1.term-title').click();
      await page.keyboard.press('Shift+A');
      const d = page
        .getByRole('complementary', { name: 'Ask' })
        .or(page.getByRole('dialog', { name: 'Ask' }));
      await expect(d.getByRole('textbox', { name: 'Question' })).toBeFocused();
      await expect(d).toContainText('Ada Okafor');
      const sent = page.waitForRequest((r) => r.url().endsWith('/demo/ask'));
      await d.getByRole('textbox', { name: 'Question' }).fill('Where does this person work?');
      await d.getByRole('textbox', { name: 'Question' }).press('Enter');
      expect((await sent).postDataJSON().about.iri).toBe(ADA);
      await expect(
        d
          .getByRole('article', { name: 'Where does this person work?' })
          .getByRole('group', { name: 'Feedback' }),
      ).toBeVisible({ timeout: 60_000 });
      await noOverflow(page);
      await page.screenshot({ path: join(OUT, `drawer-${size}-${theme}.png`) });
    });
  }
