// The Ask thread against the mock (C18 §6.2): the page at /ui/ask and the drawer docked
// on any page, opened with Shift+A and from the palette with the page's context as a
// chip; a turn's folds (Query, Rows, Sources, Steps), its citations and actions, feedback,
// a follow-up in the thread, Stop, the history's threads and reopening one, including a
// record kept before stored answers, keyboard-only use and the phone sheet.
//
// mock/assistant.mjs scripts the pipeline by words in the question, and starts the `org`
// history with one record of format 1.

import { expect, test, type Page } from '@playwright/test';
import { pageAction } from '../page-actions';
import { phone } from '../phone';

test.describe.configure({ timeout: 90_000 });

const UNI = 'http://example.org/uni#';

async function fresh(page: Page, ds = 'org', mode: 'preview' | 'run' = 'run') {
  await page.addInitScript(
    ([d, m]) => {
      localStorage.setItem('sparkles.dataset', d);
      localStorage.setItem('sparkles.askPrefs.local', JSON.stringify({ mode: m }));
      localStorage.removeItem('sparkles.queryTabs');
      sessionStorage.clear();
    },
    [ds, mode] as const,
  );
}

async function openAsk(page: Page) {
  await fresh(page);
  await page.goto('/ui/ask?ds=org');
  await expect(page.getByRole('textbox', { name: 'Question' })).toBeVisible({ timeout: 20_000 });
}

const thread = (page: Page) => page.getByRole('region', { name: 'Thread' });
const drawer = (page: Page) =>
  page.getByRole('complementary', { name: 'Ask' }).or(page.getByRole('dialog', { name: 'Ask' }));

async function askIn(scope: ReturnType<typeof thread>, question: string) {
  await scope.getByRole('textbox', { name: 'Question' }).fill(question);
  await scope.getByRole('textbox', { name: 'Question' }).press('Enter');
}

const turn = (scope: ReturnType<typeof thread>, question: string) =>
  scope.getByRole('article', { name: question });

test('a turn shows its answer, its folds and its citations, and its actions work', async ({
  page,
}) => {
  await openAsk(page);
  const t = thread(page);
  await askIn(t, 'Who is in the payments team?');
  const a = turn(t, 'Who is in the payments team?');
  await expect(a).toContainText('is in the answer', { timeout: 15_000 });
  // Query
  await a.getByRole('button', { name: /^Query/ }).click();
  await expect(a.locator('pre')).toContainText('ex:memberOf res:payments');
  // Rows, as a table
  await a.getByRole('button', { name: /^Rows/ }).click();
  await expect(a.getByRole('grid')).toContainText('Kai Ito');
  // a citation opens Sources and marks its row
  await a.getByRole('button', { name: 'Citation [1]' }).click();
  await expect(a.getByText('Cited rows', { exact: true })).toBeVisible();
  await expect(a.getByRole('grid').locator('[aria-selected="true"]')).toHaveCount(1);
  // memory facts about the cited entities, with their quotes
  await expect(a.getByText('Memory and documents', { exact: true })).toBeVisible();
  await expect(a.locator('blockquote').first()).toBeVisible();
  // Steps
  await a.getByRole('button', { name: /^Steps/ }).click();
  await expect(a).toContainText('mock · small, ok');
  await expect(a.getByRole('button', { name: 'Try harder' })).toBeVisible();
  // feedback goes through the feedback route
  const fb = page.waitForRequest((r) => /\/\$\/asks\/org\/[^/]+\/feedback$/.test(r.url()));
  await a.getByRole('group', { name: 'Feedback' }).getByRole('button', { name: 'Right' }).click();
  expect((await fb).postDataJSON()).toEqual({ outcome: 'accepted' });
  await expect(a.getByRole('button', { name: 'Right' })).toHaveAttribute('aria-pressed', 'true');

  // Open in Explore focuses a cited entity
  await a.getByRole('button', { name: 'Open in Explore' }).click();
  await expect(page).toHaveURL(/\/ui\/explore\?ds=org&iri=http/);
  await page.goBack();
  // Open in Query opens a tab with the question, its header and its rows
  await turn(thread(page), 'Who is in the payments team?')
    .getByRole('button', { name: 'Open in Query' })
    .click();
  await expect(page).toHaveURL(/\/ui\/query/);
  await expect(page.getByRole('tab', { name: 'Who is in the payments team?' })).toHaveAttribute(
    'aria-selected',
    'true',
  );
  await expect(page.getByRole('region', { name: 'Question' })).toContainText(
    'Who is in the payments team?',
  );
  // Save as query opens the Save as example dialog
  await page.goBack();
  await turn(thread(page), 'Who is in the payments team?')
    .getByRole('button', { name: 'Save as query' })
    .click();
  await expect(page.getByRole('dialog', { name: 'Save as example' })).toBeVisible({
    timeout: 15_000,
  });
});

test('a follow-up continues the thread, which the history lists and reopens', async ({ page }) => {
  await openAsk(page);
  const t = thread(page);
  await askIn(t, 'Who is in the payments team?');
  // a turn has its feedback once it ends
  await expect(
    turn(t, 'Who is in the payments team?').getByRole('group', { name: 'Feedback' }),
  ).toBeVisible({ timeout: 15_000 });
  const sent = page.waitForRequest((r) => r.url().endsWith('/org/ask'));
  await askIn(t, 'and their start dates?');
  const body = (await sent).postDataJSON();
  expect(body.thread).toMatch(/^[0-9a-f]{16}$/);
  expect(body.context).toEqual([
    { question: 'Who is in the payments team?', query: expect.stringContaining('res:payments') },
  ]);
  await expect(turn(t, 'and their start dates?')).toContainText('is in the answer', {
    timeout: 15_000,
  });
  // the history lists the thread with its two questions
  const list = page.getByRole('complementary', { name: 'Threads' });
  await expect(
    list.getByRole('button', { name: /Who is in the payments team\?.*2 questions/ }),
  ).toBeVisible();
  // New thread starts over, and the history reopens the thread from its stored answers
  await t.getByRole('button', { name: 'New thread' }).click();
  await expect(t.getByRole('article')).toHaveCount(0);
  await list.getByRole('button', { name: /Who is in the payments team\?.*2 questions/ }).click();
  await expect(t.getByRole('article')).toHaveCount(2);
  const stored = turn(t, 'Who is in the payments team?');
  await expect(stored).toContainText('is in the answer');
  // rows are not stored: Show rows runs the stored query again without a model
  const asks: string[] = [];
  page.on('request', (r) => r.url().endsWith('/org/ask') && asks.push(r.url()));
  await stored.getByRole('button', { name: /^Rows/ }).click();
  await stored.getByRole('button', { name: 'Show rows' }).click();
  await expect(stored.getByRole('grid')).toContainText('Kai Ito');
  expect(asks).toEqual([]);
});

test('a record kept before stored answers reopens as a thread of its own', async ({ page }) => {
  await openAsk(page);
  const list = page.getByRole('complementary', { name: 'Threads' });
  await list.getByRole('button', { name: /Which teams are there\?.*1 question, right/ }).click();
  const t = thread(page);
  const old = turn(t, 'Which teams are there?');
  await expect(old).toContainText('This answer was kept before answers were stored.');
  await expect(old.getByRole('button', { name: 'Right' })).toHaveAttribute('aria-pressed', 'true');
  await old.getByRole('button', { name: /^Query/ }).click();
  await expect(old.locator('pre')).toContainText('ex:Team');
  await old.getByRole('button', { name: /^Rows/ }).click();
  await old.getByRole('button', { name: 'Show rows' }).click();
  await expect(old.getByRole('grid')).toBeVisible();
});

test('Stop ends a running turn', async ({ page }) => {
  // the mock holds the stream before the summary for `hold: "summary"`
  await page.route('**/org/ask', async (route) => {
    const body = route.request().postDataJSON();
    await route.continue({ postData: JSON.stringify({ ...body, hold: 'summary' }) });
  });
  await openAsk(page);
  const t = thread(page);
  await askIn(t, 'Who is in the payments team?');
  const a = turn(t, 'Who is in the payments team?');
  await expect(a.getByRole('status')).toContainText('Summarizing', { timeout: 15_000 });
  await expect(a.getByRole('list', { name: 'Steps so far' })).toContainText('Rows');
  await t.getByRole('button', { name: 'Stop' }).click();
  await expect(a).toContainText('Stopped.');
  await expect(t.getByRole('button', { name: 'Ask', exact: true })).toBeVisible();
});

test('a clarification is answered in the turn', async ({ page }) => {
  await openAsk(page);
  const t = thread(page);
  await askIn(t, 'What does Ana work on?');
  const a = turn(t, 'What does Ana work on?');
  const choices = a.getByRole('group', { name: 'Which one is meant' });
  await expect(choices).toContainText('Which "Ana" do you mean?', { timeout: 15_000 });
  const sent = page.waitForRequest((r) => r.url().endsWith('/org/ask'));
  await choices.getByRole('button', { name: /Ana Lima/ }).click();
  expect((await sent).postDataJSON().clarification).toEqual({
    id: 'mention',
    value: 'http://example.org/resource/ana',
  });
  await expect(a.getByRole('group', { name: 'Which one is meant' })).toHaveCount(0, {
    timeout: 15_000,
  });
});

test.describe('the drawer on a Browse page', () => {
  test.beforeEach(async ({ request }) => {
    const r = await request.put('/$/assistant/uni', { data: { enabled: true, send: 'rows' } });
    expect(r.ok()).toBe(true);
  });
  test.afterEach(async ({ request }) => {
    await request.put('/$/assistant/uni', { data: {} });
  });

  test("a term page's Ask about this opens the drawer about the term", async ({ page }) => {
    await fresh(page, 'uni');
    await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Professor')}`);
    await expect(page.locator('h1.term-title')).toBeVisible({ timeout: 30_000 });
    await pageAction(page, 'Ask about this');
    const d = drawer(page);
    await expect(d).toBeVisible();
    await expect(d).toContainText('About Professor');
    await expect(d.getByRole('textbox', { name: 'Question' })).toBeFocused();
  });

  test('opens with Shift+A and the class as context, and asks about it', async ({ page }) => {
    await fresh(page, 'uni');
    await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Professor')}`);
    await expect(page.locator('h1.term-title')).toBeVisible({ timeout: 30_000 });
    await page.locator('h1.term-title').click();
    await page.keyboard.press('Shift+A');
    const d = drawer(page);
    await expect(d).toBeVisible();
    await expect(d.getByRole('textbox', { name: 'Question' })).toBeFocused();
    await expect(d).toContainText('About Professor');
    const sugg = d.getByRole('group', { name: 'Suggested questions' });
    await expect(sugg.getByRole('button', { name: 'What are its subclasses?' })).toBeVisible();
    const sent = page.waitForRequest((r) => r.url().endsWith('/uni/ask'));
    await sugg.getByRole('button', { name: 'What are its subclasses?' }).click();
    const body = (await sent).postDataJSON();
    expect(body.about).toEqual({ iri: UNI + 'Professor', label: 'Professor', kind: 'class' });
    expect(body.question).toBe('What are its subclasses?');
    await expect(
      d
        .getByRole('article', { name: 'What are its subclasses?' })
        .getByRole('group', { name: 'Feedback' }),
    ).toBeVisible({ timeout: 15_000 });
    // the chip can be removed, and the next question carries no resource
    await d.getByRole('button', { name: 'Remove the context About Professor' }).click();
    const next = page.waitForRequest((r) => r.url().endsWith('/uni/ask'));
    await d.getByRole('textbox', { name: 'Question' }).fill('How many people are there?');
    await d.getByRole('textbox', { name: 'Question' }).press('Enter');
    expect((await next).postDataJSON().about).toBeUndefined();
    // Open as page keeps the thread
    await d.getByRole('button', { name: 'Open as page' }).click();
    await expect(page).toHaveURL(/\/ui\/ask\?ds=uni/);
    await expect(thread(page).getByRole('article')).toHaveCount(2);
  });

  test('is usable with the keyboard alone', async ({ page }) => {
    await fresh(page, 'uni');
    await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Professor')}`);
    await expect(page.locator('h1.term-title')).toBeVisible({ timeout: 30_000 });
    await page.locator('h1.term-title').click();
    await page.keyboard.press('Shift+A');
    await page.keyboard.type('Who teaches?');
    await page.keyboard.press('Enter');
    const d = drawer(page);
    const a = d.getByRole('article', { name: 'Who teaches?' });
    await expect(a.getByRole('button', { name: /^Steps/ })).toBeVisible({ timeout: 15_000 });
    // the folds are buttons in the tab order
    await a.getByRole('button', { name: /^Query/ }).focus();
    await page.keyboard.press('Enter');
    await expect(a.getByRole('button', { name: /^Query/ })).toHaveAttribute(
      'aria-expanded',
      'true',
    );
    // Escape closes the drawer
    await d.getByRole('textbox', { name: 'Question' }).focus();
    await page.keyboard.press('Escape');
    await expect(drawer(page)).toHaveCount(0);
  });
});

test.describe('on a phone', () => {
  test.use({ ...phone, viewport: { width: 390, height: 844 } });

  test('the drawer is a bottom sheet that fits the screen', async ({ page }) => {
    await fresh(page);
    await page.goto('/ui/explore?ds=org');
    await page
      .getByRole('button', { name: /search/i })
      .first()
      .click();
    await page.keyboard.type('who leads payments');
    await page.getByRole('option', { name: /Ask a question/ }).click();
    const sheet = page.getByRole('dialog', { name: 'Ask' });
    await expect(sheet).toBeVisible();
    await expect(sheet.getByRole('heading', { name: 'who leads payments' })).toBeVisible();
    await expect(sheet.getByRole('group', { name: 'Feedback' })).toBeVisible({ timeout: 15_000 });
    const box = await sheet.boundingBox();
    expect(box!.x).toBeGreaterThanOrEqual(0);
    expect(box!.x + box!.width).toBeLessThanOrEqual(391);
    expect(box!.y).toBeGreaterThan(50);
    const wide = await sheet.evaluate((el) => {
      const r = el.getBoundingClientRect();
      return [...el.querySelectorAll('*')]
        .filter((c) => c.getBoundingClientRect().right > r.right + 1)
        .map((c) => c.className || c.tagName);
    });
    expect(wide).toEqual([]);
    await page.keyboard.press('Escape');
    await expect(sheet).toHaveCount(0);
  });
});
