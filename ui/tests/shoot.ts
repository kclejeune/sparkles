// Helpers of the README's screenshots (tests/screenshots and tests/screenshots-mock): wait
// until a page stops changing, then write docs/images/<name>.png.

import { expect, type Locator, type Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { join, resolve } from 'node:path';

export const OUT = resolve(import.meta.dirname, '../../docs/images');
mkdirSync(OUT, { recursive: true });

/** Waits for fonts, for spinners to go, and for `target` to look the same twice running. */
export async function settle(page: Page, target: Locator = page.locator('body'), spinners = 0) {
  await page.evaluate(() => document.fonts.ready.then(() => undefined));
  await expect(page.locator('.spinner')).toHaveCount(spinners, { timeout: 15_000 });
  let last: Buffer | null = null;
  for (let i = 0; i < 60; i++) {
    await page.waitForTimeout(250);
    const now = await target.screenshot({ animations: 'disabled', scale: 'css' });
    if (last && now.equals(last)) return;
    last = now;
  }
  throw new Error('the page did not stop changing');
}

/**
 * Writes docs/images/`name`.png once the page has settled, without a hover state (unless
 * `hover`) or a focus ring. `spinners` is the number of spinners the picture shows.
 */
export async function shoot(
  page: Page,
  name: string,
  o: { hover?: boolean; spinners?: number } = {},
) {
  if (!o.hover) await page.mouse.move(0, 0);
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await settle(page, page.locator('body'), o.spinners ?? 0);
  await page.screenshot({ path: join(OUT, `${name}.png`), animations: 'disabled' });
}
