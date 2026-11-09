// The entity frame's History view (ROADMAP item 620) against the mock's `uni` dataset: the
// commits of a branch that changed an entity, newest first, each opening to the axioms it
// changed, a revert of one of them through the editor's preview, an entity without recorded
// commits, a reader who cannot revert and the layout at phone width. Each test works on its
// own branch.

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

test.describe.configure({ timeout: 90_000 });

const UNI = 'http://example.org/uni#';
const PREFIXES = `PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX uni: <${UNI}>`;

async function branchWithEdits(request: APIRequestContext, name: string) {
  const made = await request.post('/$/branches/uni', { data: { name } });
  expect(made.status()).toBe(201);
  const update = async (message: string, body: string) => {
    const r = await request.post(`/uni@${name}/update`, {
      headers: { 'Content-Type': 'application/sparql-update', 'Sparkles-Commit-Message': message },
      data: `${PREFIXES}\n${body}`,
    });
    expect(r.ok(), await r.text()).toBe(true);
  };
  await update(
    'Comment on Professor',
    'INSERT DATA { uni:Professor rdfs:comment "Teaches courses."@en }',
  );
  await update('Label a course', 'INSERT DATA { uni:Course rdfs:label "Kurs"@de }');
  await update(
    'Lecturer under Professor',
    'INSERT DATA { uni:Lecturer rdfs:subClassOf uni:Professor }',
  );
}

const historyOf = (page: Page) =>
  page.getByRole('list', { name: 'Commits that changed the entity' });

async function openHistory(page: Page, branch: string, iri: string) {
  await page.goto(
    `/ui/workbench/ontology?${new URLSearchParams({ ds: 'uni', branch, iri, view: 'history' })}`,
  );
  await expect(page.getByRole('button', { name: 'History', exact: true })).toHaveAttribute(
    'aria-pressed',
    'true',
    { timeout: 30_000 },
  );
}

test('lists the commits that changed the entity, opens their axioms and reverts one', async ({
  page,
  request,
}) => {
  const branch = 'eh-revert';
  await branchWithEdits(request, branch);
  try {
    await openHistory(page, branch, `${UNI}Professor`);
    const commits = historyOf(page);
    // the course's label names another entity, so its commit is left out
    await expect(commits.getByRole('button', { name: /Lecturer under Professor/ })).toBeVisible();
    await expect(commits.getByRole('button', { name: /Comment on Professor/ })).toBeVisible();
    await expect(commits.getByRole('button', { name: /Label a course/ })).toHaveCount(0);
    await expect(commits.getByRole('listitem').first()).toContainText('Lecturer under Professor');

    await commits.getByRole('button', { name: /Comment on Professor/ }).click();
    const changed = page.getByRole('list', { name: /Axioms commit \d+ changed/ });
    await expect(changed).toContainText('AnnotationAssertion(rdfs:comment uni:Professor');
    await expect(changed.locator('li.added')).toHaveCount(1);

    // the revert is previewed and committed, and the new commit tops the list
    await page.getByRole('button', { name: /^Revert commit \d+$/ }).click();
    const d = page.getByRole('dialog');
    await expect(d).toContainText('Revert commit');
    await expect(d.locator('[data-outcome]')).toHaveAttribute('data-outcome', 'commit');
    await d.getByRole('button', { name: 'Commit' }).click();
    await expect(d).toBeHidden();
    await expect(commits.getByRole('listitem').first()).toContainText(/revert/i, {
      timeout: 10_000,
    });
    await commits.getByRole('listitem').first().getByRole('button').first().click();
    await expect(page.locator('li.removed').first()).toContainText('rdfs:comment uni:Professor');
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('an entity that no recorded commit changed has an empty history', async ({
  page,
  request,
}) => {
  const branch = 'eh-empty';
  await branchWithEdits(request, branch);
  try {
    await openHistory(page, branch, `${UNI}Department`);
    await expect(
      page.getByText('No recorded commit changed this entity in this graph.'),
    ).toBeVisible();
    await expect(historyOf(page).getByRole('listitem')).toHaveCount(0);
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('a reader sees the history without a revert, and it fits a phone', async ({
  page,
  request,
}) => {
  const branch = 'eh-reader';
  await branchWithEdits(request, branch);
  try {
    await page.route('**/$/whoami', (route) =>
      route.fulfill({
        json: {
          authEnabled: true,
          principal: { kind: 'token', name: 'reader' },
          method: 'bearer',
          server: [],
          datasets: { uni: 'read' },
          canMintTokens: false,
          logout: false,
        },
      }),
    );
    await page.setViewportSize({ width: 360, height: 760 });
    await openHistory(page, branch, `${UNI}Professor`);
    await historyOf(page)
      .getByRole('button', { name: /Comment on Professor/ })
      .click();
    await expect(page.getByRole('list', { name: /Axioms commit \d+ changed/ })).toContainText(
      'rdfs:comment',
    );
    await expect(page.getByRole('link', { name: /Full diff of commit/ })).toBeVisible();
    await expect(page.getByRole('button', { name: /^Revert commit \d+$/ })).toHaveCount(0);
    await expectNoSidewaysScroll(page, 'the entity history at phone width');
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('a late answer for the entity the view left is not shown for the next one', async ({
  page,
  request,
}) => {
  const branch = 'eh-late';
  const made = await request.post('/$/branches/uni', { data: { name: branch } });
  expect(made.status()).toBe(201);
  try {
    // one commit that changes both entities, with an axiom only Professor's diff lists
    const r = await request.post(`/uni@${branch}/update`, {
      headers: {
        'Content-Type': 'application/sparql-update',
        'Sparkles-Commit-Message': 'Both entities',
      },
      data: `${PREFIXES}
INSERT DATA { uni:Employee rdfs:comment "staff"@en . uni:Professor rdfs:comment "late"@en }`,
    });
    expect(r.ok(), await r.text()).toBe(true);
    let release!: () => void;
    const held = new Promise<void>((done) => (release = done));
    await page.route(
      (u) => u.pathname.endsWith('/diff') && u.searchParams.get('entity') === `${UNI}Professor`,
      async (route) => {
        await held;
        await route.continue().catch(() => {});
      },
    );
    await openHistory(page, branch, `${UNI}Professor`);
    await historyOf(page)
      .getByRole('button', { name: /Both entities/ })
      .click();
    // Professor's answer is held while the view moves to Employee through the breadcrumb,
    // which keeps the History view
    await page.locator('.crumbs').getByRole('button', { name: 'Employee', exact: true }).click();
    await expect(page).toHaveURL(/Employee/);
    await historyOf(page)
      .getByRole('button', { name: /Both entities/ })
      .click();
    const changed = page.getByRole('list', { name: /Axioms commit \d+ changed/ });
    await expect(changed).toContainText('"staff"@en');
    release();
    // the held answer, if it still arrives, must not replace Employee's
    await page.waitForTimeout(1000);
    await expect(changed).toContainText('"staff"@en');
    await expect(changed).not.toContainText('late');
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('a writer whose grants cover only some graphs gets no revert', async ({ page, request }) => {
  const branch = 'eh-limited';
  await branchWithEdits(request, branch);
  try {
    await page.route('**/$/whoami', (route) =>
      route.fulfill({
        json: {
          authEnabled: true,
          principal: { kind: 'token', name: 'limited' },
          method: 'bearer',
          server: [],
          datasets: { uni: 'write' },
          restricted: { uni: { graphs: true } },
          canMintTokens: false,
          logout: false,
        },
      }),
    );
    await openHistory(page, branch, `${UNI}Professor`);
    await historyOf(page)
      .getByRole('button', { name: /Comment on Professor/ })
      .click();
    await expect(page.getByRole('link', { name: /Full diff of commit/ })).toBeVisible();
    await expect(page.getByRole('button', { name: /^Revert commit \d+$/ })).toHaveCount(0);
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});
