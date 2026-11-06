import { expect, open } from './fixtures';

open.describe.configure({ timeout: 120_000 });

open(
  'materialized property pages and individual frames share exact read-only sources',
  async ({ page, request }) => {
    const ds = 'ontology-inferred-reads';
    const created = await request.post('/$/datasets', {
      data: { dbName: ds, dbType: 'persistent' },
    });
    expect(created.ok(), await created.text()).toBe(true);
    try {
      const wide = Array.from(
        { length: 105 },
        (_, i) =>
          `<urn:p${String(i).padStart(3, '0')}> a owl:ObjectProperty; rdfs:subPropertyOf <urn:root> .`,
      ).join('\n');
      const loaded = await request.post(`/${ds}/data`, {
        headers: { 'Content-Type': 'text/turtle' },
        data: `@prefix owl: <http://www.w3.org/2002/07/owl#> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        <urn:root> a owl:ObjectProperty; rdfs:label "root" .
        <urn:dp> a owl:DatatypeProperty; rdfs:subPropertyOf <urn:dq> .
        <urn:dq> a owl:DatatypeProperty; rdfs:subPropertyOf <urn:dr> .
        <urn:dr> a owl:DatatypeProperty .
        <urn:A> a owl:Class; rdfs:subClassOf <urn:B> .
        <urn:B> a owl:Class; rdfs:subClassOf <urn:C> . <urn:C> a owl:Class .
        <urn:i> a owl:NamedIndividual, <urn:A> . ${wide}`,
      });
      expect(loaded.ok(), await loaded.text()).toBe(true);
      const reasoned = await request.post(`/$/reason/${ds}`, { data: { profile: 'rdfs' } });
      expect(reasoned.ok(), await reasoned.text()).toBe(true);
      const task = await reasoned.json();
      await expect
        .poll(async () => (await (await request.get(`/$/tasks/${task.id}`)).json()).state)
        .toBe('done');
      const posts: string[] = [];
      const reads: {
        sourceId: string;
        commit: number;
        url: string;
        noStore: string | undefined;
      }[] = [];
      const pending: Promise<void>[] = [];
      page.on('request', (r) => {
        if (r.method() === 'POST' && /\/(?:classify|reason)(?:\/|\?|$)/.test(r.url()))
          posts.push(r.url());
      });
      page.on('response', (r) => {
        const url = new URL(r.url());
        if (url.searchParams.get('inferences') !== 'true' || !r.ok()) return;
        pending.push(
          (async () => {
            const body = await r.json();
            if (body.inferenceSource)
              reads.push({
                sourceId: body.inferenceSource.sourceId,
                commit: body.commit,
                url: r.url(),
                noStore: r.headers()['cache-control'],
              });
          })(),
        );
      });
      await page.goto(`/ui/workbench/ontology?ds=${ds}&kind=ObjectProperty&inferred=1`);
      const properties = page.getByRole('tree', {
        name: 'Object properties (materialized inferred)',
        exact: true,
      });
      await expect(properties.getByRole('button', { name: 'root', exact: true })).toBeVisible();
      await properties.getByRole('button', { name: 'Expand root', exact: true }).click();
      await expect(properties.getByRole('button', { name: 'urn:p000', exact: true })).toBeVisible();
      await properties.focus();
      await page.keyboard.press('End');
      await expect(properties.getByRole('button', { name: /Show more/ })).toBeVisible();
      await properties.getByRole('button', { name: /Show more/ }).click();
      await expect(
        properties.getByRole('treeitem', { name: 'Show more, 100 of 105 shown' }),
      ).not.toBeAttached();
      await properties.focus();
      await page.keyboard.press('End');
      await expect(properties.getByRole('button', { name: 'urn:p104', exact: true })).toBeVisible();
      await page.getByRole('combobox', { name: 'Entity kind' }).selectOption('DataProperty');
      const data = page.getByRole('tree', {
        name: 'Data properties (materialized inferred)',
        exact: true,
      });
      await expect(data.getByRole('button', { name: 'urn:dr', exact: true })).toBeVisible();
      await data.getByRole('button', { name: 'Expand urn:dr', exact: true }).click();
      await expect(data.getByRole('button', { name: 'urn:dq', exact: true })).toBeVisible();
      // Transitive closure edges do not make dp a direct child of dr.
      await expect(data.getByRole('button', { name: 'urn:dp', exact: true })).toHaveCount(0);
      await page.goto(`/ui/browse?ds=${ds}&iri=urn%3Ai`);
      const frame = page.getByRole('region', { name: 'Materialized inferred axioms', exact: true });
      await expect(frame).toContainText('ClassAssertion');
      await expect(frame).toContainText('urn:C');
      await expect(frame).toContainText('Completeness and consistency are unknown');
      await expect(
        frame.getByRole('button', { name: /Edit|Explain|Classify|Materialize/ }),
      ).toHaveCount(0);
      await Promise.all(pending);
      expect(posts).toEqual([]);
      expect(reads.length).toBeGreaterThan(3);
      expect(new Set(reads.map((r) => r.sourceId)).size).toBe(1);
      expect(new Set(reads.map((r) => r.commit)).size).toBe(1);
      for (const read of reads) {
        expect(read.noStore).toBe('no-store');
        expect(new URL(read.url).searchParams.get('at')).toBe(`commit:${read.commit}`);
      }
      const cleared = await request.delete(`/$/reason/${ds}`);
      expect(cleared.ok(), await cleared.text()).toBe(true);
      await frame.getByRole('button', { name: 'Read again', exact: true }).click();
      await expect(frame.getByRole('alert')).toContainText('Could not read materialized facts');
      await expect(frame.locator('li code')).toHaveCount(0);
      expect(posts).toEqual([]);
    } finally {
      const removed = await request.delete(`/$/datasets/${ds}`);
      expect(removed.ok(), await removed.text()).toBe(true);
    }
  },
);
