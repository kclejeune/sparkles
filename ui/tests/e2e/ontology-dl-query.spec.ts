// The DL Query tab and the Manchester add fields against a real server: the EL
// classifier answers an expression inside OWL 2 EL, the RL rules answer one outside it
// with the label that says so, a name the reader does not know is underlined, and a frame
// section takes a Manchester expression that the server reads.

import type { Page } from '@playwright/test';
import { expect, open } from './fixtures';

open.describe.configure({ mode: 'serial', timeout: 120_000 });

const DS = 'dl-query-e2e';
const UNI = 'http://example.org/uni#';

const TURTLE = `
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix uni: <${UNI}> .

<http://example.org/uni> a owl:Ontology ; rdfs:label "University ontology"@en .
uni:Agent a owl:Class ; rdfs:label "Agent"@en .
uni:Person a owl:Class ; rdfs:subClassOf uni:Agent ; rdfs:label "Person"@en .
uni:Employee a owl:Class ; rdfs:subClassOf uni:Person ; rdfs:label "Employee"@en .
uni:Course a owl:Class ; rdfs:label "Course"@en .
uni:GraduateCourse a owl:Class ; rdfs:subClassOf uni:Course ; rdfs:label "Graduate course"@en .
uni:teaches a owl:ObjectProperty ; rdfs:label "teaches"@en .
uni:Professor a owl:Class ; rdfs:subClassOf uni:Employee ;
  rdfs:subClassOf [ a owl:Restriction ; owl:onProperty uni:teaches ; owl:someValuesFrom uni:Course ] ;
  rdfs:label "Professor"@en .
uni:Teacher a owl:Class ; rdfs:label "Teacher"@en ;
  owl:equivalentClass [ owl:intersectionOf ( uni:Person
    [ a owl:Restriction ; owl:onProperty uni:teaches ; owl:someValuesFrom uni:Course ] ) ] .
uni:ada a owl:NamedIndividual , uni:Professor ; rdfs:label "Ada Lovelace"@en .
`;

const editor = (page: Page) => page.getByRole('textbox', { name: 'Class expression' });
const answers = (page: Page) => page.getByRole('region', { name: 'DL query answers' });

async function run(page: Page, text: string) {
  await editor(page).click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.press('Delete');
  await page.keyboard.type(text);
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: 'Execute' }).click();
}

open.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
  expect(r.ok(), await r.text()).toBe(true);
  const loaded = await request.post(`/${DS}/data`, {
    headers: { 'Content-Type': 'text/turtle' },
    data: TURTLE,
  });
  expect(loaded.ok(), await loaded.text()).toBe(true);
});

open('the EL classifier answers a query inside OWL 2 EL', async ({ page }) => {
  await page.goto(`/ui/workbench/ontology?ds=${DS}&tab=dlquery`);
  await run(page, 'Person and (teaches some Course)');
  const a = answers(page);
  await expect(a.getByRole('status')).toContainText('OWL 2 EL classifier');
  await expect(a.getByRole('status')).toContainText('Complete');
  await expect(a.getByRole('region', { name: 'Equivalent classes' })).toContainText('Teacher');
  await expect(a.getByRole('region', { name: 'Direct superclasses' })).toContainText('Person');
  await expect(a.getByRole('region', { name: 'Direct subclasses' })).toContainText('Professor');
  await expect(a.getByRole('region', { name: 'Instances' })).toContainText('Ada Lovelace');
  await a
    .getByRole('region', { name: 'Direct subclasses' })
    .getByRole('button', { name: 'Professor' })
    .click();
  await expect(page.getByRole('heading', { name: 'Professor', level: 2 })).toBeVisible();
});

open('outside OWL 2 EL the RL rules answer, labelled as incomplete', async ({ page }) => {
  await page.goto(`/ui/workbench/ontology?ds=${DS}&tab=dlquery`);
  await run(page, "teaches only 'Graduate course'");
  const a = answers(page);
  await expect(a.getByRole('status')).toContainText('OWL 2 RL');
  await expect(a.getByRole('status')).toContainText('Possibly incomplete');
  // the classifier can be named, and the label says what it left out
  await page.getByRole('combobox', { name: 'Reasoning' }).selectOption('el');
  await run(page, "teaches some 'Graduate course'");
  await expect(a.getByRole('status')).toContainText('OWL 2 EL classifier');
});

open('a name the ontology does not have is underlined', async ({ page }) => {
  await page.goto(`/ui/workbench/ontology?ds=${DS}&tab=dlquery`);
  await run(page, 'Person and Robot');
  await expect(page.getByRole('alert')).toContainText('Robot');
  await expect(page.locator('.cm-lint-error')).toHaveText('Robot');
});

open('a frame section reads a Manchester expression on the server', async ({ page }) => {
  await page.goto(`/ui/workbench/ontology?ds=${DS}&iri=${encodeURIComponent(UNI + 'Employee')}`);
  const sub = page.locator('.frame [data-section="sub"]');
  await sub.getByRole('textbox', { name: /^Add to/ }).click();
  await page.keyboard.type("teaches only 'Graduate course'");
  await page.keyboard.press('Escape');
  await page.keyboard.press('Enter');
  const d = page.getByRole('dialog');
  await expect(d).toContainText("Employee SubClassOf teaches only 'Graduate course'");
  await d.getByRole('button', { name: 'Cancel' }).click();
  await expect(d).toBeHidden();
});
