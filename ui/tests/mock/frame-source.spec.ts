import { expect, test, type Route } from '@playwright/test';

const UNI = 'http://example.org/uni#';
test.describe.configure({ mode: 'serial', timeout: 60_000 });

for (const refresh of ['delayed', 'failed'] as const) {
  test(`an annotation draft cannot submit against a ${refresh} newer frame`, async ({
    page,
    request,
  }) => {
    const branch = `draft-${refresh}`;
    expect((await request.post('/$/branches/uni', { data: { name: branch } })).ok()).toBe(true);
    await page.goto(
      `/ui/workbench/ontology?ds=uni&branch=${branch}&iri=${encodeURIComponent(UNI + 'Professor')}`,
    );
    const frame = page.locator('.frame[data-iri]');
    await expect(frame).toContainText('Employee');
    await expect(page.getByText('Following changes')).toBeVisible();
    await frame.getByRole('button', { name: 'Add annotation', exact: true }).first().click();
    const form = frame.getByRole('form', { name: /^Annotation of / });
    await form.getByRole('combobox', { name: 'Property', exact: true }).fill('rdfs:comment');
    await form.getByRole('textbox', { name: 'Value', exact: true }).fill('Preserve this draft');
    const held: Route[] = [];
    await page.route('**/$/ontology/uni/{entity,axioms}?*', async (route) => {
      if (
        route.request().method() === 'GET' &&
        new URL(route.request().url()).searchParams.has('at')
      ) {
        held.push(route);
        if (refresh === 'failed')
          await route.fulfill({ status: 503, json: { error: 'deliberate frame read failure' } });
      } else await route.continue();
    });
    const posts: string[] = [];
    page.on('request', (r) => {
      if (r.method() === 'POST' && /\/axioms(?:\?|$)/.test(r.url())) posts.push(r.url());
    });
    const changed = await request.post(`/$/ontology/uni/axioms?branch=${branch}&receipt=true`, {
      data: { add: [{ type: 'SubClassOf', subClass: UNI + 'Fresh', superClass: UNI + 'Person' }] },
    });
    expect(changed.ok(), await changed.text()).toBe(true);
    await expect(page.getByText(/Displaying frame commit/)).toBeVisible();
    await expect.poll(() => held.length).toBeGreaterThan(0);
    // requestSubmit bypasses the disabled button and exercises the handler boundary too.
    await form.evaluate((el) => (el as HTMLFormElement).requestSubmit());
    await expect(form).toContainText('This frame is read-only');
    await expect(form.getByRole('button', { name: 'Preview', exact: true })).toBeDisabled();
    await expect(page.getByRole('dialog')).toBeHidden();
    expect(posts).toEqual([]);
    await expect(form.getByRole('textbox', { name: 'Value', exact: true })).toHaveValue(
      'Preserve this draft',
    );
    if (refresh === 'delayed') {
      for (const route of held) await route.continue();
      await expect(frame).toContainText('Draft based on commit');
      await expect(form.getByRole('textbox', { name: 'Value', exact: true })).toHaveValue(
        'Preserve this draft',
      );
      await expect(form.getByRole('button', { name: 'Preview', exact: true })).toBeDisabled();
      await frame
        .getByRole('button', { name: 'Discard draft and load current frame', exact: true })
        .click();
      await expect(form).toHaveCount(0);
      await frame.getByRole('button', { name: 'Add annotation', exact: true }).first().click();
      await form.getByRole('combobox', { name: 'Property', exact: true }).fill('rdfs:comment');
      await form
        .getByRole('textbox', { name: 'Value', exact: true })
        .fill('Reviewed current source');
      await form.getByRole('button', { name: 'Preview', exact: true }).click();
      await expect(page.getByRole('dialog').locator('[data-outcome]')).toHaveAttribute(
        'data-outcome',
        'commit',
      );
      expect(posts).toHaveLength(1);
    }
  });
}

test('a removed axiom keeps its annotation draft blocked after the new frame arrives', async ({
  page,
  request,
}) => {
  const branch = 'draft-removed';
  expect((await request.post('/$/branches/uni', { data: { name: branch } })).ok()).toBe(true);
  await page.goto(
    `/ui/workbench/ontology?ds=uni&branch=${branch}&iri=${encodeURIComponent(UNI + 'Professor')}`,
  );
  const frame = page.locator('.frame[data-iri]');
  const toggle = frame.getByRole('button', { name: /Annotations of SubClassOf.*Employee/ }).first();
  await toggle.click();
  const row = toggle.locator('xpath=ancestor::li[1]');
  await row.getByRole('button', { name: 'Add annotation', exact: true }).click();
  const form = row.getByRole('form');
  await form.getByRole('combobox', { name: 'Property', exact: true }).fill('rdfs:comment');
  await form
    .getByRole('textbox', { name: 'Value', exact: true })
    .fill('Do not resurrect the removed axiom');
  const held: Route[] = [];
  await page.route('**/$/ontology/uni/{entity,axioms}?*', async (route) => {
    if (route.request().method() === 'GET' && new URL(route.request().url()).searchParams.has('at'))
      held.push(route);
    else await route.continue();
  });
  const posts: string[] = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && /\/axioms(?:\?|$)/.test(r.url())) posts.push(r.url());
  });
  const removed = await request.post(`/$/ontology/uni/axioms?branch=${branch}`, {
    data: {
      remove: [{ type: 'SubClassOf', subClass: UNI + 'Professor', superClass: UNI + 'Employee' }],
    },
  });
  expect(removed.ok(), await removed.text()).toBe(true);
  await expect(page.getByText(/Displaying frame commit/)).toBeVisible();
  await form.evaluate((el) => (el as HTMLFormElement).requestSubmit());
  await expect(form).toContainText('This frame is read-only');
  expect(posts).toEqual([]);
  for (const route of held) await route.continue();
  await expect(frame).toContainText('Draft based on commit');
  await expect(form.getByRole('textbox', { name: 'Value', exact: true })).toHaveValue(
    'Do not resurrect the removed axiom',
  );
  await form.evaluate((el) => (el as HTMLFormElement).requestSubmit());
  await expect(page.getByRole('dialog')).toBeHidden();
  expect(posts).toEqual([]);
  await frame
    .getByRole('button', { name: 'Discard draft and load current frame', exact: true })
    .click();
  await expect(
    frame.getByRole('button', { name: /Annotations of SubClassOf.*Employee/ }),
  ).toHaveCount(0);
  const current = await request.get(
    `/$/ontology/uni/axioms?branch=${branch}&about=${encodeURIComponent(UNI + 'Professor')}`,
  );
  expect(
    (await current.json()).axioms.some(
      (a: { axiom: { superClass?: string } }) => a.axiom.superClass === UNI + 'Employee',
    ),
  ).toBe(false);
});

test('an asynchronous parse cannot submit a draft after its source changes', async ({
  page,
  request,
}) => {
  const branch = 'draft-parse';
  expect((await request.post('/$/branches/uni', { data: { name: branch } })).ok()).toBe(true);
  await page.goto(
    `/ui/workbench/ontology?ds=uni&branch=${branch}&iri=${encodeURIComponent(UNI + 'Professor')}`,
  );
  const frame = page.locator('.frame[data-iri]');
  const field = frame.getByRole('textbox', { name: 'Add to SubClass Of', exact: true });
  await field.fill('Person and Course');
  let release: (() => Promise<void>) | undefined;
  await page.route('**/$/ontology/uni/parse?*', async (route) => {
    const response = await route.fetch();
    release = () => route.fulfill({ response });
  });
  const posts: string[] = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && /\/axioms(?:\?|$)/.test(r.url())) posts.push(r.url());
  });
  await frame
    .locator('[data-section="sub"]')
    .getByRole('button', { name: 'Add', exact: true })
    .click();
  await expect.poll(() => !!release).toBe(true);
  const changed = await request.post(`/$/ontology/uni/axioms?branch=${branch}`, {
    data: {
      add: [{ type: 'SubClassOf', subClass: UNI + 'NewDuringParse', superClass: UNI + 'Person' }],
    },
  });
  expect(changed.ok(), await changed.text()).toBe(true);
  const currentSummary = await request.get(`/$/ontology/uni?branch=${branch}`);
  const newCommit = (await currentSummary.json()).commit;
  await expect(page.locator('header.page-head .meta')).toContainText(`commit ${newCommit}`);
  await release!();
  await expect(frame.getByRole('alert')).toContainText('The frame source changed');
  await expect(field).toHaveText('Person and Course');
  await expect(page.getByRole('dialog')).toBeHidden();
  expect(posts).toEqual([]);
});

test('a frame preview and commit keep the original If-Match through a summary refresh', async ({
  page,
  request,
}) => {
  const branch = 'draft-preview-pin';
  expect((await request.post('/$/branches/uni', { data: { name: branch } })).ok()).toBe(true);
  await page.goto(
    `/ui/workbench/ontology?ds=uni&branch=${branch}&iri=${encodeURIComponent(UNI + 'Professor')}`,
  );
  const frame = page.locator('.frame[data-iri]');
  await frame.getByRole('button', { name: 'Add annotation', exact: true }).first().click();
  const form = frame.getByRole('form', { name: /^Annotation of / });
  await form.getByRole('combobox', { name: 'Property', exact: true }).fill('rdfs:comment');
  await form
    .getByRole('textbox', { name: 'Value', exact: true })
    .fill('Must not be written at the newer commit');
  const initial = await request.get(`/$/ontology/uni?branch=${branch}`);
  const tag = initial.headers().etag;
  expect(tag).toBeTruthy();
  const tags: (string | undefined)[] = [];
  let release: (() => Promise<void>) | undefined;
  await page.route('**/$/ontology/uni/axioms?*', async (route) => {
    if (route.request().method() !== 'POST') return route.continue();
    tags.push(route.request().headers()['if-match']);
    if (new URL(route.request().url()).searchParams.has('dryRun')) {
      const response = await route.fetch();
      release = () => route.fulfill({ response });
    } else await route.continue();
  });
  await form.getByRole('button', { name: 'Preview', exact: true }).click();
  await expect.poll(() => !!release).toBe(true);
  const changed = await request.post(`/$/ontology/uni/axioms?branch=${branch}`, {
    data: {
      add: [{ type: 'SubClassOf', subClass: UNI + 'DuringPreview', superClass: UNI + 'Person' }],
    },
  });
  expect(changed.ok(), await changed.text()).toBe(true);
  const current = await request.get(`/$/ontology/uni?branch=${branch}`);
  await expect(page.locator('header.page-head .meta')).toContainText(
    `commit ${(await current.json()).commit}`,
  );
  await release!();
  const dialog = page.getByRole('dialog');
  await expect(dialog.locator('[data-outcome]')).toHaveAttribute('data-outcome', 'commit');
  await dialog.getByRole('button', { name: 'Commit', exact: true }).click();
  await expect(dialog).toBeHidden();
  expect(tags).toEqual([tag, tag]);
  const after = await request.get(
    `/$/ontology/uni/axioms?branch=${branch}&about=${encodeURIComponent(UNI + 'Professor')}`,
  );
  expect(JSON.stringify(await after.json())).not.toContain('Must not be written');
});

test('a delayed frame preview closes on entity departure and cannot reopen', async ({
  page,
  request,
}) => {
  const branch = 'draft-preview-context';
  expect((await request.post('/$/branches/uni', { data: { name: branch } })).ok()).toBe(true);
  await page.goto(
    `/ui/workbench/ontology?ds=uni&branch=${branch}&iri=${encodeURIComponent(UNI + 'Professor')}`,
  );
  const frame = page.locator('.frame[data-iri]');
  await frame.getByRole('button', { name: 'Employee', exact: true }).click();
  await expect(frame).toHaveAttribute('data-iri', UNI + 'Employee');
  await frame.getByRole('button', { name: 'Add annotation', exact: true }).first().click();
  const form = frame.getByRole('form', { name: /^Annotation of / });
  await form.getByRole('combobox', { name: 'Property', exact: true }).fill('rdfs:comment');
  await form.getByRole('textbox', { name: 'Value', exact: true }).fill('Employee-only draft');
  let release: (() => Promise<void>) | undefined;
  await page.route('**/$/ontology/uni/axioms?*', async (route) => {
    if (route.request().method() !== 'POST') return route.continue();
    const response = await route.fetch();
    release = () => route.fulfill({ response });
  });
  await form.getByRole('button', { name: 'Preview', exact: true }).click();
  await expect.poll(() => !!release).toBe(true);
  await page.goBack();
  await expect(page).toHaveURL(/iri=http%3A%2F%2Fexample.org%2Funi%23Professor/);
  await expect(page.getByRole('dialog')).toBeHidden();
  await release!();
  await expect(frame).toHaveAttribute('data-iri', UNI + 'Professor');
  await expect(page.getByRole('dialog')).toBeHidden();
});
