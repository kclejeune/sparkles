import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Dataset, factory as f } from '../dist/index.js';

const n = (s) => f.namedNode(`urn:x:${s}`);
const q = (s, p, o, g = f.defaultGraph()) => f.quad(s, p, o, g);

test('terms sent by number keep their kinds across requests', async () => {
  const ds = Dataset.memory();
  const objects = [
    f.literal('plain'),
    f.literal('chat', 'fr'),
    f.literal('hello', { language: 'en', direction: 'rtl' }),
    f.literal('42', f.namedNode('http://www.w3.org/2001/XMLSchema#integer')),
    f.literal('x', f.namedNode('urn:x:custom')),
    f.literal('ünïcode ☃ 𝄞'),
    n('iri-object'),
    f.quad(n('a'), n('b'), n('c')),
  ];
  // twice, so that the second round sends every IRI and literal by number
  for (let round = 0; round < 2; round++) {
    await ds.transaction(async (tx) => {
      for (const [i, o] of objects.entries()) {
        const quad = q(n(`s${i}`), n('p'), o, round ? n('g') : f.defaultGraph());
        assert.equal(await tx.add(quad), true);
        assert.equal(await tx.add(quad), false);
      }
    });
  }
  assert.equal(await ds.count(), BigInt(2 * objects.length));
  for (const [i, o] of objects.entries()) {
    assert.equal(await ds.has(q(n(`s${i}`), n('p'), o)), true, `object ${i}`);
    assert.equal(await ds.has(q(n(`s${i}`), n('p'), o, n('g'))), true, `object ${i} in g`);
    assert.equal(await ds.has(q(n(`s${i}`), n('q'), o)), false);
    const [found] = await ds.match(n(`s${i}`), n('p'), null, f.defaultGraph()).toArray();
    assert.ok(found.object.equals(o), `object ${i} reads back`);
  }
  await ds.transaction(async (tx) => {
    assert.equal(await tx.delete(q(n('s0'), n('p'), objects[0])), true);
    assert.equal(await tx.delete(q(n('s0'), n('p'), objects[0])), false);
  });
  assert.equal(await ds.has(q(n('s0'), n('p'), objects[0])), false);
});

test('blank nodes are scoped by their transaction, not numbered', async () => {
  const ds = Dataset.memory();
  const b = f.blankNode('b');
  await ds.transaction(async (tx) => {
    await tx.add(q(b, n('p'), f.literal('1')));
    await tx.add(q(b, n('p'), f.literal('2')));
  });
  await ds.transaction(async (tx) => {
    await tx.add(q(b, n('p'), f.literal('3')));
  });
  const subjects = new Set((await ds.match(null, n('p')).toArray()).map((x) => x.subject.value));
  assert.equal(subjects.size, 2);
  const [stored] = await ds.match(null, n('p'), f.literal('1')).toArray();
  assert.equal(await ds.has(q(stored.subject, n('p'), f.literal('2'))), true);
  assert.equal(await ds.has(q(stored.subject, n('p'), f.literal('3'))), false);
});

test('a rolled back term is not found, and adding it again works', async () => {
  const ds = Dataset.memory();
  const quad = q(n('fresh'), n('p'), f.literal('rolled back'));
  await assert.rejects(
    ds.transaction(async (tx) => {
      await tx.add(quad);
      assert.equal(await ds.has(quad), false);
      throw new Error('stop');
    }),
    /stop/,
  );
  assert.equal(await ds.has(quad), false);
  await ds.add(quad);
  assert.equal(await ds.has(quad), true);
});

test('a term found missing is found once a commit adds it', async () => {
  const ds = Dataset.memory();
  await ds.add(q(n('a'), n('p'), n('o')));
  const absent = q(n('a'), n('p'), n('later'));
  // the addon remembers that the term is missing, for as long as the vocabulary is unchanged
  assert.equal(await ds.has(absent), false);
  assert.equal(await ds.has(absent), false);
  await ds.add(q(n('other'), n('p'), n('o')));
  assert.equal(await ds.has(absent), false);
  await ds.add(absent);
  assert.equal(await ds.has(absent), true);
  await ds.delete(absent);
  assert.equal(await ds.has(absent), false);
});

test('a rejected request does not leave numbers the addon lacks', async () => {
  const ds = Dataset.memory();
  await ds.add(q(n('a'), n('p'), n('o')));
  const bad = { termType: 'NamedNode', value: 'not an iri', equals: () => false };
  await assert.rejects(ds.add(q(n('b'), n('p'), bad)));
  await assert.rejects(ds.has(q(n('b'), n('p'), bad)));
  assert.equal(await ds.has(q(n('a'), n('p'), n('o'))), true);
  await ds.add(q(n('b'), n('p'), n('o')));
  assert.equal(await ds.has(q(n('b'), n('p'), n('o'))), true);
  // a variable is no stored term
  await assert.rejects(ds.add(q(n('c'), n('p'), f.variable('v'))));
  assert.equal(await ds.count(), 2n);
});

test('more distinct terms than the table keeps stay correct', async () => {
  const ds = Dataset.memory();
  const quads = Array.from({ length: 70000 }, (_, i) => q(n(`s${i}`), n('p'), f.literal(`${i}`)));
  const { inserted } = await ds.addAll(quads);
  assert.equal(inserted, 70000n);
  for (const i of [0, 1, 40000, 65535, 65536, 69999])
    assert.equal(await ds.has(quads[i]), true, `quad ${i}`);
  assert.equal(await ds.has(q(n('s70000'), n('p'), f.literal('70000'))), false);
  await ds.transaction(async (tx) => {
    for (const i of [0, 69999]) assert.equal(await tx.add(quads[i]), false);
  });
});

test('writes keep their order behind requests that are still pending', async () => {
  const ds = Dataset.memory();
  await ds.add(q(n('old'), n('p'), n('o')));
  await ds.transaction(async (tx) => {
    const cleared = tx.update('DELETE WHERE { ?s ?p ?o }');
    const added = tx.add(q(n('new'), n('p'), n('o')));
    await cleared;
    assert.equal(await added, true);
    const all = tx.addAll(Array.from({ length: 5000 }, (_, i) => q(n(`s${i}`), n('p'), n('o'))));
    assert.equal(await tx.add(q(n('s1'), n('p'), n('o'))), false);
    assert.equal(await all, 5000n);
  });
  assert.equal(await ds.has(q(n('old'), n('p'), n('o'))), false);
  assert.equal(await ds.count(), 5001n);
  // an async iterable's quads follow in order too
  await ds.transaction(async (tx) => {
    const more = tx.addAll(
      (async function* () {
        for (let i = 5000; i < 5010; i++) yield q(n(`s${i}`), n('p'), n('o'));
      })(),
    );
    assert.equal(await more, 10n);
    assert.equal(await tx.add(q(n('s5009'), n('p'), n('o'))), false);
  });
});

test('has and match read the union default graph through the pattern path', async () => {
  const ds = Dataset.memory({ unionDefaultGraph: true });
  await ds.add(q(n('s'), n('p'), n('o'), n('g')));
  assert.equal(await ds.has(q(n('s'), n('p'), n('o'))), true);
  assert.equal(await ds.has(q(n('s'), n('p'), n('o'), n('g'))), true);
  assert.equal((await ds.match(n('s'), null, null, f.defaultGraph()).toArray()).length, 1);
});

test('a match larger than its first batch keeps reading', async () => {
  const ds = Dataset.memory();
  await ds.addAll(Array.from({ length: 100 }, (_, i) => q(n('s'), n('p'), f.literal(`${i}`))));
  assert.equal((await ds.match(n('s')).toArray()).length, 100);
  let seen = 0;
  for await (const _ of ds.match(n('s'), n('p'))) if (++seen === 3) break;
  assert.equal(seen, 3);
  assert.equal((await ds.match(n('nobody')).toArray()).length, 0);
});
