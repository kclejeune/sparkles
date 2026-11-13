// Watches on entities (ROADMAP item 619) against a real server without auth, where watches
// belong to the local principal: a subtree watch on Professor made from the entity frame
// on a branch, a class put below Professor and then labelled, which the server's follower
// matches, the Watches page listing both commits with links to the History view, marking
// them read, and removing the watch.

import { expect, open } from './fixtures';

open.describe.configure({ mode: 'serial', timeout: 120_000 });

const DS = 'watches-e2e';
const BRANCH = 'watched';
const UNI = 'http://example.org/uni#';

const TURTLE = `
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix uni: <${UNI}> .

<http://example.org/uni> a owl:Ontology .
uni:Employee a owl:Class ; rdfs:label "Employee"@en .
uni:Professor a owl:Class ; rdfs:subClassOf uni:Employee ; rdfs:label "Professor"@en .
uni:Course a owl:Class ; rdfs:label "Course"@en .
`;

open.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
  expect(r.ok(), await r.text()).toBe(true);
  const loaded = await request.post(`/${DS}/data`, {
    headers: { 'Content-Type': 'text/turtle' },
    data: TURTLE,
  });
  expect(loaded.ok(), await loaded.text()).toBe(true);
  const b = await request.post(`/$/branches/${DS}`, { data: { name: BRANCH } });
  expect(b.ok(), await b.text()).toBe(true);
});

open.afterAll(async ({ request }) => {
  await request.delete(`/$/datasets/${DS}`);
});

open(
  'a subtree watch made in the entity frame lists the commits below it',
  async ({ page, request }) => {
    await page.goto(
      `/ui/workbench/ontology?${new URLSearchParams({ ds: DS, branch: BRANCH, iri: `${UNI}Professor` })}`,
    );
    await page.getByRole('button', { name: 'Watch', exact: true }).click({ timeout: 30_000 });
    const dialog = page.getByRole('dialog', { name: 'Watch this entity' });
    await dialog.getByLabel('Entity and subtree').check();
    await dialog.getByRole('button', { name: 'Watch', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Watching', exact: true })).toHaveAttribute(
      'aria-pressed',
      'true',
    );

    // a commit that puts a class below Professor, one that labels it, and one elsewhere
    for (const [message, u] of [
      [
        'Emeritus under Professor',
        `<${UNI}Emeritus> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <${UNI}Professor>`,
      ],
      [
        'Label Emeritus',
        `<${UNI}Emeritus> <http://www.w3.org/2000/01/rdf-schema#label> "Emeritus"@en`,
      ],
      [
        'Label Course',
        `<${UNI}Course> <http://www.w3.org/2000/01/rdf-schema#comment> "Taught."@en`,
      ],
    ]) {
      const r = await request.post(`/${DS}@${BRANCH}/update`, {
        headers: {
          'Content-Type': 'application/sparql-update',
          'Sparkles-Commit-Message': message,
        },
        data: `INSERT DATA { ${u} }`,
      });
      expect(r.ok(), await r.text()).toBe(true);
    }

    // the follower runs within a second or so of a commit
    await expect
      .poll(
        async () => {
          const l = await (await request.get(`/$/watches?dataset=${DS}`)).json();
          return l.watches[0]?.unread;
        },
        { timeout: 30_000 },
      )
      .toBe(2);

    await page.goto('/ui/watches');
    await expect(page.getByRole('link', { name: 'Watches, 2 unread' })).toBeVisible();
    const row = page.getByRole('row').filter({ hasText: `${UNI}Professor` });
    await expect(row).toContainText('Subtree');
    await row.getByRole('button', { name: `${UNI}Professor` }).click();
    const commits = page.getByRole('list', { name: 'Matched commits' });
    await expect(commits.getByRole('listitem')).toHaveCount(2);
    await expect(commits).not.toContainText(`${UNI}Course`);
    const member = commits.getByRole('link', { name: `${UNI}Emeritus` }).first();
    await expect(member).toHaveAttribute('href', /view=history/);

    await row.getByRole('button', { name: 'Mark read' }).click();
    await expect(row.getByRole('button', { name: 'Mark read' })).toBeDisabled();
    await row.getByRole('button', { name: 'Remove' }).click();
    await expect(page.getByText('You watch no entities.')).toBeVisible();
  },
);
