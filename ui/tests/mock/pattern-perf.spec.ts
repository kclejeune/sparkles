// U08 A10, the speed of the Pattern pane: for a generated query of 200 triple patterns,
// the time from a keystroke to a settled redraw, less the 150 ms debounce, at the median of
// 50 runs. It is recorded rather than gating ordinary runs: set PATTERN_PERF=1 to run it
// (`PATTERN_PERF=1 pnpm e2e:mock tests/mock/pattern-perf.spec.ts`).

import { expect, test } from '@playwright/test';
import { selectAll } from '../phone';

test.skip(!process.env.PATTERN_PERF, 'set PATTERN_PERF=1 to measure');
test.describe.configure({ timeout: 300_000 });

/** 200 triple patterns over 100 variables in a chain, with literals and a few classes. */
function query(): string {
  const lines = ['PREFIX ex: <http://example.org/>', 'SELECT * WHERE {'];
  for (let i = 0; i < 100; i++) {
    lines.push(`  ?v${i} ex:p${i % 7} ?v${i + 1} .`);
    lines.push(i % 10 === 0 ? `  ?v${i} a ex:C${i % 3} .` : `  ?v${i} ex:name "n${i}" .`);
  }
  lines.push('  ?v100 ex:last "end" .', '}');
  return lines.join('\n');
}

test('A10: a keystroke redraws a 200-pattern query', async ({ page }) => {
  await page.goto('/ui/query');
  await page.locator('.cm-content').click();
  await selectAll(page);
  await page.keyboard.press('Delete');
  await page.keyboard.insertText(query());
  await page.getByRole('button', { name: 'Pattern', exact: true }).click();
  const pane = page.getByRole('region', { name: 'Pattern', exact: true });
  await expect(pane).toHaveAttribute('data-state', 'ok');
  await expect(pane.getByTestId('pattern-counts')).toHaveText('201 triples · 0 locked');
  const c = pane.locator('.pcanvas');
  await expect.poll(async () => Number(await c.getAttribute('data-settled'))).toBeGreaterThan(0);

  // in the page: the time of each keydown in the editor and of each settle after it
  await page.evaluate(() => {
    const w = window as unknown as { __t: { down: number; settle: number[] } };
    w.__t = { down: 0, settle: [] };
    document.addEventListener('keydown', () => (w.__t.down = performance.now()), true);
    const el = document.querySelector('.pcanvas')!;
    new MutationObserver(() => w.__t.settle.push(performance.now() - w.__t.down)).observe(el, {
      attributes: true,
      attributeFilter: ['data-settled'],
    });
  });

  async function measure(key: string): Promise<{ total: number; parse: number; draw: number }> {
    await page.evaluate(() => {
      (window as unknown as { __t: { settle: number[] } }).__t.settle = [];
    });
    await page.keyboard.press(key);
    await expect
      .poll(() =>
        page.evaluate(() => (window as unknown as { __t: { settle: number[] } }).__t.settle.length),
      )
      .toBeGreaterThan(0);
    const total = await page.evaluate(
      () => (window as unknown as { __t: { settle: number[] } }).__t.settle[0],
    );
    const parse = Number(await pane.getAttribute('data-parse-ms'));
    const draw = Number(await pane.getAttribute('data-draw-ms'));
    return { total: total - 150, parse, draw };
  }

  const median = (xs: number[]) => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)];
  const report: Record<string, { median: number; parse: number; draw: number; max: number }> = {};

  // (a) a character typed into the last literal: the same nodes, a changed label
  await page.locator('.cm-line', { hasText: 'ex:last "end"' }).click();
  await page.keyboard.press('End');
  for (let i = 0; i < 5; i++) await page.keyboard.press('ArrowLeft');
  let runs: { total: number; parse: number; draw: number }[] = [];
  for (let i = 0; i < 50; i++) runs.push(await measure(i % 2 ? 'Backspace' : 'x'));
  report.literal = {
    median: median(runs.map((r) => r.total)),
    parse: median(runs.map((r) => r.parse)),
    draw: median(runs.map((r) => r.draw)),
    max: Math.max(...runs.map((r) => r.total)),
  };

  // (b) a character typed into a variable: one node goes, a new one is placed
  // `?v99 ex:p1 ?v100 .`, three lines above the closing brace: typing after `?v100`
  await page.keyboard.press('ControlOrMeta+End');
  for (let i = 0; i < 3; i++) await page.keyboard.press('ArrowUp');
  await page.keyboard.press('End');
  for (let i = 0; i < 2; i++) await page.keyboard.press('ArrowLeft');
  runs = [];
  for (let i = 0; i < 50; i++) runs.push(await measure(i % 2 ? 'Backspace' : 'z'));
  report.variable = {
    median: median(runs.map((r) => r.total)),
    parse: median(runs.map((r) => r.parse)),
    draw: median(runs.map((r) => r.draw)),
    max: Math.max(...runs.map((r) => r.total)),
  };
  console.log(`A10 ${JSON.stringify(report)}`);
  test.info().annotations.push({ type: 'A10', description: JSON.stringify(report) });
});
