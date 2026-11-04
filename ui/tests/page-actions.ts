// Runs a page head's action by its name, from the head or, when PageActions folded it, from
// the More menu (ROADMAP item 633).
import type { Page } from '@playwright/test';

export async function pageAction(page: Page, name: string) {
  const head = page.locator('.page-head > .page-actions').first();
  // the head folds its actions a frame after the page changes them
  await page.evaluate(
    () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
  );
  const direct = head.locator(':scope > :is(a, button)').filter({ hasText: name });
  if (await direct.first().isVisible()) return direct.first().click();
  await head.getByRole('button', { name: 'More actions' }).click();
  await page.getByRole('menu', { name: 'More actions' }).getByRole('menuitem', { name }).click();
}
