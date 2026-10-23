// A15 of docs/specs/U01-time-slider.md, Phase 4: over a synthetic history of 200 commits of
// 50 changes each, the time-lapse at 10 steps a second keeps each step's frame time under
// 50 ms after its changes arrive. Each commit adds 25 links among 180 resources and removes
// the 25 the commit before added, so the canvas stays well under its node cap while every
// step moves edges and ghosts.
//
// A frame is timed from the step's changes to the animation frame after the graph drew
// them, so it moves in steps of one display refresh and depends on what else the machine
// runs. As U06 does for its frame rate, ordinary runs record the numbers and check only that
// the lapse ran, and `SPARKLES_PERF=1` turns on the 50 ms gate for a run on a quiet host.

import { expect, test, type APIRequestContext } from '@playwright/test';

test.describe.configure({ mode: 'serial', timeout: 240_000 });

const EX = 'http://example.org/';

const links = (i: number) =>
  Array.from({ length: 25 }, (_, k) => `ex:s${(i * 7 + k) % 120} ex:p ex:o${(i + k * 3) % 60} .`);

async function build(request: APIRequestContext) {
  const r = await request.post('/$/datasets', {
    data: { dbName: 'lapse200', dbType: 'persistent' },
  });
  if (r.status() === 409) return;
  expect(r.ok()).toBe(true);
  for (let i = 1; i <= 200; i++) {
    const del = i > 1 ? `DELETE DATA { ${links(i - 1).join(' ')} } ;` : '';
    const res = await request.post('/lapse200/update', {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: `PREFIX ex: <${EX}> ${del} INSERT DATA { ${links(i).join(' ')} }`,
    });
    expect(res.ok()).toBe(true);
  }
}

test('A15: each step of a 200-commit time-lapse draws in under 50 ms', async ({
  page,
  request,
}) => {
  await build(request);
  await page.goto('/ui/explore?ds=lapse200&diff=commit%3A200&diffFrom=commit%3A0');
  const view = page.locator('.dg');
  await expect(view.locator('.wrap[data-graph]')).toBeAttached({ timeout: 60_000 });
  await view.getByRole('combobox', { name: 'Steps a second' }).selectOption('10');
  const started = Date.now();
  await view.getByRole('button', { name: 'Play the time-lapse' }).click();
  await expect(view.getByText(/^step \d+\/200$/)).toHaveText('step 200/200', { timeout: 200_000 });
  const seconds = (Date.now() - started) / 1000;
  const max = Number(await view.getAttribute('data-frame-max'));
  const p50 = await view.getAttribute('data-frame-p50');
  const p95 = await view.getAttribute('data-frame-p95');
  const firstFrame = await view.getAttribute('data-frame-first');
  console.log(`A15: frames p50 ${p50} ms, p95 ${p95} ms, first ${firstFrame} ms`);
  const inFlight = Number(await view.getAttribute('data-max-in-flight'));
  test.info().annotations.push({
    type: 'A15',
    description: `200 steps in ${seconds.toFixed(1)} s, the slowest frame ${max} ms`,
  });
  console.log(`A15: 200 steps in ${seconds.toFixed(1)} s, slowest frame ${max} ms`);
  expect(inFlight).toBe(1);
  expect(max).toBeGreaterThan(0);
  if (process.env.SPARKLES_PERF === '1') expect(max).toBeLessThan(50);
});
