// The Explore overview grouped by the GraphRAG communities (U06 with C22) against the
// mock: the group-by picker, opening a community into the level below and into its
// members, the report in the side panel, a finding that marks its entities, the links
// between Explore and the Communities tab, the unavailable and off-main states, and a
// phone. The mock's `org` dataset has Engineering at level 0 with the payments and
// platform teams below it. The assertions read the list view, the canvas's accessible
// companion.

import { expect, test, type Page } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

async function communities(page: Page, url = '/ui/explore?ds=org') {
  await page.goto(url);
  const ov = page.getByTestId('overview');
  await expect(ov).toBeVisible();
  if ((await ov.getAttribute('data-mode')) !== 'communities')
    await ov.getByRole('combobox', { name: 'Group by' }).selectOption('mode:communities');
  await expect(ov.getByRole('navigation', { name: 'Overview levels' })).toContainText(
    'Communities',
  );
  await ov.getByRole('button', { name: 'List' }).click();
  return ov;
}

const row = (ov: ReturnType<Page['getByTestId']>, name: string) =>
  ov.locator('tbody tr').filter({
    has: ov.page().getByRole('button', { name: new RegExp(`^${name}\\s*(,\\s*marked)?$`) }),
  });

test('the picker draws the communities, which open into the level below', async ({ page }) => {
  const ov = await communities(page);
  await expect(ov).toHaveAttribute('data-mode', 'communities');
  // the overview's history reads the classes, so the communities do not offer it
  await expect(ov.getByRole('button', { name: 'Overview history' })).toHaveCount(0);
  await page.keyboard.press('Shift+H');
  await expect(ov.locator('.ov-time')).toHaveCount(0);
  await expect(row(ov, 'Engineering')).toContainText('unreviewed');
  await expect(ov.getByRole('combobox', { name: 'Colour by' })).toHaveValue('status');
  await expect(ov).toContainText('1 community at level 0');

  await row(ov, 'Engineering').getByRole('button', { name: 'Open' }).click();
  const crumbs = ov.getByRole('navigation', { name: 'Overview levels' });
  await expect(crumbs).toContainText('Communities');
  await expect(crumbs).toContainText('Engineering');
  // (the Communities tab's spec renames the payments team in the shared mock)
  await expect(ov.locator('tbody tr')).toHaveCount(2);
  await expect(row(ov, 'The platform team')).toBeVisible();
  // the opened community's report is beside the canvas
  const side = page.getByTestId('community-side');
  await expect(side.getByRole('heading', { name: 'Engineering' })).toBeVisible();
  await expect(side).toContainText('The payments and platform teams');

  // selecting a community shows its report instead
  await row(ov, 'The platform team').getByRole('button', { name: 'The platform team' }).click();
  await expect(side.getByRole('heading', { name: 'The platform team' })).toBeVisible();
  await expect(page).toHaveURL(/overview=communities&community=/);

  // the finest level opens into its members
  await row(ov, 'The platform team').getByRole('button', { name: 'Open' }).click();
  await expect(crumbs).toContainText('The platform team');
  await expect(row(ov, 'Lee')).toBeVisible();
  await expect(row(ov, 'Mo')).toBeVisible();
  // Backspace closes the level
  await page.keyboard.press('Backspace');
  await expect(row(ov, 'The platform team')).toBeVisible();
  // and the picker goes back to the classes
  await ov.getByRole('button', { name: 'Communities' }).click();
  await ov.getByRole('combobox', { name: 'Group by' }).selectOption('mode:classes');
  await expect(ov).toHaveAttribute('data-mode', 'classes');
  await expect(side).toHaveCount(0);
});

test('a finding marks the entities it is about', async ({ page }) => {
  const ov = await communities(page);
  await row(ov, 'Engineering').getByRole('button', { name: 'Open' }).click();
  await row(ov, 'The platform team').getByRole('button', { name: 'The platform team' }).click();
  const side = page.getByTestId('community-side');
  await expect(side.getByRole('heading', { name: 'The platform team' })).toBeVisible();
  const finding = side.getByRole('button', { name: /Lee works with the rest/ });
  await finding.click();
  await expect(finding).toHaveAttribute('aria-pressed', 'true');
  // Lee is in the team, which is drawn as one group: the members are drawn on request
  const marked = ov.getByTestId('overview-marked');
  await expect(marked).toContainText('0 of the 2 entities');
  await marked.getByRole('button', { name: 'Draw the members' }).click();
  await expect(row(ov, 'Lee')).toHaveClass(/marked/);
  await expect(row(ov, 'Mo')).not.toHaveClass(/marked/);
  await expect(marked).toContainText('1 of the 2 entities');
  // choosing it again clears the marks
  await finding.click();
  await expect(marked).toHaveCount(0);
});

test('the Communities tab and Explore link to each other', async ({ page }) => {
  await page.goto('/ui/datasets/org?tab=communities');
  const panel = page.getByTestId('communities');
  const tree = panel.getByRole('navigation', { name: 'Community hierarchy' });
  await tree.getByRole('button', { name: /The platform team/ }).click();
  await expect(panel.getByRole('heading', { name: 'The platform team' })).toBeVisible();
  await panel.getByRole('link', { name: 'Show in Explore' }).click();
  await expect(page).toHaveURL(/\/explore\?ds=org&overview=communities&community=/);
  const ov = page.getByTestId('overview');
  await expect(ov.getByRole('navigation', { name: 'Overview levels' })).toContainText(
    'Engineering',
  );
  const side = page.getByTestId('community-side');
  await expect(side.getByRole('heading', { name: 'The platform team' })).toBeVisible();
  await side.getByRole('link', { name: 'Open report' }).click();
  await expect(page).toHaveURL(/\/datasets\/org\?tab=communities&community=/);
  await expect(
    page.getByTestId('communities').getByRole('heading', { name: 'The platform team' }),
  ).toBeVisible();
});

test('a caller who may not read the reports learns that only', async ({ page }) => {
  await page.route(/\/\$\/graph\/org\/groups$/, (route) =>
    route.fulfill({
      json: {
        commit: 1,
        available: false,
        unavailable: 'access',
        reason:
          'The GraphRAG communities are not available to you, because your access does not cover everything their build read.',
        within: { reports: { graph: 'urn:x-sparkles:graphrag:reports', level: 0 }, size: 0 },
        grouping: { kind: 'reports', predicate: null, level: 0 },
        communities: false,
        groups: [],
        edges: [],
        truncated: false,
      },
    }),
  );
  await page.goto('/ui/explore?ds=org&overview=communities');
  const empty = page.getByTestId('overview-communities-unavailable');
  await expect(empty).toContainText('not available to you');
  await expect(page.getByTestId('community-side')).toHaveCount(0);
  await empty.getByRole('button', { name: 'Show the classes' }).click();
  await expect(page.getByTestId('overview')).toHaveAttribute('data-mode', 'classes');
});

test('a dataset without reports says so', async ({ page }) => {
  await page.goto('/ui/explore?ds=foaf&overview=communities');
  const empty = page.getByTestId('overview-communities-unavailable');
  await expect(empty).toContainText('No GraphRAG communities');
  await expect(empty.getByRole('link', { name: 'Communities tab' })).toBeVisible();
});

test('off main the communities are not read', async ({ page }) => {
  const groups: string[] = [];
  page.on('request', (r) => {
    if (r.url().includes('/$/graph/') || r.url().includes('/$/graphrag/')) groups.push(r.url());
  });
  await page.goto('/ui/explore?ds=org&overview=communities&branch=scratch-s2');
  await expect(page.getByTestId('overview-communities-off-main')).toContainText('scratch-s2');
  expect(groups).toEqual([]);
});

// review round 1: the report of the dataset left stayed beside the next dataset's overview,
// and a read of it still in flight could land there after the switch
test('switching datasets drops the report, even one still being read', async ({ page }) => {
  let release: () => void = () => {};
  const held = new Promise<void>((r) => (release = r));
  await page.route(/\/\$\/graphrag\/org\/community\?/, async (route) => {
    if (route.request().url().includes(encodeURIComponent('urn:x-sparkles:graphrag:org:1:plat')))
      await held;
    await route.continue();
  });
  const ov = await communities(page);
  await row(ov, 'Engineering').getByRole('button', { name: 'Open' }).click();
  const side = page.getByTestId('community-side');
  await expect(side.getByRole('heading', { name: 'Engineering' })).toBeVisible();
  // the platform team's report is held while the dataset changes
  await row(ov, 'The platform team').getByRole('button', { name: 'The platform team' }).click();
  await page.getByRole('button', { name: /^org/ }).first().click();
  await page
    .getByRole('listbox', { name: 'Datasets' })
    .getByRole('option', { name: /^foaf persistent/ })
    .click();
  await expect(page).toHaveURL(/ds=foaf/);
  release();
  await expect(page.getByTestId('overview-communities-unavailable')).toContainText(
    'No GraphRAG communities',
  );
  await expect(side).toHaveCount(0);
  await expect(page).not.toHaveURL(/community=/);
});

// review round 1: a root answer that the server cut said nothing about it
test('a cut list of communities says so', async ({ page }) => {
  await page.route(/\/\$\/graph\/org\/groups$/, async (route) => {
    const body = route.request().postDataJSON() as { within?: unknown };
    if (body.within) return route.continue();
    const res = await route.fetch();
    const json = await res.json();
    await route.fulfill({ response: res, json: { ...json, truncated: true } });
  });
  await page.goto('/ui/explore?ds=org&overview=communities');
  await expect(page.getByTestId('overview')).toContainText(
    'Some communities or links were left out.',
  );
});

test.describe('on a phone', () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

  test('the communities and a report fit the screen', async ({ page }) => {
    const ov = await communities(page);
    await row(ov, 'Engineering').getByRole('button', { name: 'Open' }).click();
    await expect(
      page.getByTestId('community-side').getByRole('heading', { name: 'Engineering' }),
    ).toBeVisible();
    await expectNoSidewaysScroll(page, 'the Explore communities');
    // the canvas too
    await ov.getByRole('button', { name: 'List' }).click();
    await expect(ov.getByRole('combobox', { name: 'Group by' })).toBeVisible();
    await expectNoSidewaysScroll(page, 'the Explore communities canvas');
  });
});
