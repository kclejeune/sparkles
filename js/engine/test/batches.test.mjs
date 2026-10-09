// The wire batches of results: the first batch comes with the result, a drained result
// needs no native close, quads travel as four term cells, and decoded items are handed
// out in order without waiting for the queue.
import test from 'node:test';
import assert from 'node:assert/strict';
import { Dataset, configure, factory as f } from '../dist/index.js';

const ex = (l) => f.namedNode(`http://example.org/${l}`);
const XSD = 'http://www.w3.org/2001/XMLSchema#';

function quads() {
  const out = [];
  for (let i = 0; i < 40; i++) {
    const s = i % 3 ? ex(`s${i % 7}`) : f.blankNode(`b${i % 5}`);
    const g = i % 4 === 0 ? f.defaultGraph() : i % 4 === 1 ? ex('g1') : ex(`g${i % 4}`);
    const o =
      i % 5 === 0
        ? f.literal(`v${i}`, 'en-gb')
        : i % 5 === 1
          ? f.literal(String(i), f.namedNode(XSD + 'integer'))
          : i % 5 === 2
            ? f.literal(`w${i}`, { language: 'ar', direction: 'rtl' })
            : i % 5 === 3
              ? f.literal(`plain ${i}`)
              : ex(`o${i}`);
    out.push(f.quad(s, ex(`p${i % 3}`), o, g));
  }
  return out;
}

const key = (q) =>
  [q.subject, q.predicate, q.object, q.graph]
    .map(
      (t) =>
        `${t.termType}|${t.value}|${t.language ?? ''}|${t.direction ?? ''}|${t.datatype?.value ?? ''}`,
    )
    .join(' ');

async function filled() {
  const ds = Dataset.memory();
  await ds.addAll(quads());
  return ds;
}

test('match gives back every term kind across batches of any size', async () => {
  const ds = await filled();
  try {
    // blank node labels are the store's, so they are compared by kind only
    const unlabel = (k) => k.replace(/BlankNode\|[^ ]*/g, 'BlankNode');
    const want = quads().map(key).map(unlabel).sort();
    for (const size of [1, 3, 7, 1024]) {
      configure({ batchSize: size });
      const got = (await ds.match().toArray()).map(key).map(unlabel).sort();
      assert.deepEqual(got, want);
      const named = await ds.match(null, null, null, ex('g1')).toArray();
      assert(named.length > 0 && named.every((q) => q.graph.equals(ex('g1'))));
      assert(named.every((q) => q.termType === 'Quad'));
    }
  } finally {
    configure({ batchSize: 1024 });
    await ds.close();
  }
});

test('has, a first match and CONSTRUCT quads', async () => {
  const ds = await filled();
  try {
    const q = quads()[7];
    assert.equal(await ds.has(q), true);
    assert.equal(await ds.has(f.quad(q.subject, q.predicate, ex('nothing'), q.graph)), false);
    for await (const m of ds.match(ex('s1'), null, null, null)) {
      assert(m.subject.equals(ex('s1')));
      break;
    }
    const r = await ds.construct(
      'CONSTRUCT { ?s ?p ?o } WHERE { GRAPH <http://example.org/g1> { ?s ?p ?o } }',
    );
    const triples = await r.toArray();
    assert(triples.length > 0);
    assert(triples.every((t) => t.termType === 'Quad' && t.graph.termType === 'DefaultGraph'));
  } finally {
    await ds.close();
  }
});

test('next() calls made at once resolve in order', async () => {
  const ds = await filled();
  try {
    configure({ batchSize: 4 });
    const r = await ds.select('SELECT ?o WHERE { GRAPH ?g { ?s ?p ?o } } ORDER BY STR(?o)');
    const expected = (
      await (
        await ds.select('SELECT ?o WHERE { GRAPH ?g { ?s ?p ?o } } ORDER BY STR(?o)')
      ).toArray()
    ).map((b) => b.get('o').value);
    const pending = [];
    for (let i = 0; i < expected.length + 2; i++) pending.push(r.next());
    const results = await Promise.all(pending);
    assert.deepEqual(
      results.filter((x) => !x.done).map((x) => x.value.get('o').value),
      expected,
    );
    assert(results.slice(expected.length).every((x) => x.done));
  } finally {
    configure({ batchSize: 1024 });
    await ds.close();
  }
});

test('a drained result still reports its statistics and closes cleanly', async () => {
  const ds = await filled();
  try {
    for (const execution of ['eager', 'streaming']) {
      const r = await ds.select('SELECT * WHERE { ?s ?p ?o }', { execution });
      const rows = await r.toArray();
      assert.equal(rows.length, quads().filter((q) => q.graph.termType === 'DefaultGraph').length);
      assert.ok(await r.stats());
      await r.close();
    }
  } finally {
    await ds.close();
  }
});
