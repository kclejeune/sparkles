import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
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

test('quad positions reject incompatible terms after those terms were cached', async (t) => {
  const ds = Dataset.memory();
  t.after(() => ds.close());
  const valid = q(n('validation-subject'), n('validation-predicate'), f.literal('kept'));
  await ds.add(valid);
  const triple = f.quad(n('quoted-subject'), n('quoted-predicate'), f.literal('quoted object'));
  const invalid = [
    ['subject', f.literal('literal subject'), /subject and graph/],
    ['subject', f.defaultGraph(), /subject and graph/],
    ['subject', triple, /subject and graph/],
    ['predicate', f.literal('literal predicate'), /predicate must be an IRI/],
    ['predicate', f.blankNode('predicate'), /predicate must be an IRI/],
    ['predicate', f.defaultGraph(), /predicate must be an IRI/],
    ['predicate', triple, /predicate must be an IRI/],
    ['object', f.defaultGraph(), /default graph is not a term/],
    ['graph', f.literal('literal graph'), /subject and graph/],
    ['graph', triple, /subject and graph/],
  ];
  for (const [position, term, error] of invalid) {
    const bad = { ...valid, [position]: term };
    for (const write of [false, true]) {
      // Literals and quoted terms are valid objects. Send them first in that role;
      // default-graph terms are already cached by every ordinary probe.
      if (term.termType !== 'DefaultGraph')
        assert.equal(await ds.has(q(n('missing'), n('p'), term)), false);
      if (write)
        await assert.rejects(
          ds.transaction((tx) => tx.add(bad)),
          error,
        );
      else await assert.rejects(ds.has(bad), error);
      assert.equal(await ds.count(), 1n);
      assert.equal(await ds.has(valid), true);
    }
  }
});

/** Quads that are there and quads that are not, for a dataset that holds `stored`. */
function probes(stored) {
  const out = [];
  for (const s of stored) {
    out.push(s);
    out.push(q(n('unknown'), s.predicate, s.object, s.graph));
    out.push(q(s.subject, n('unknown'), s.object, s.graph));
    out.push(q(s.subject, s.predicate, n('unknown'), s.graph));
    out.push(q(s.subject, s.predicate, s.object, n('unknown')));
    out.push(
      q(
        s.subject,
        s.predicate,
        s.object,
        s.graph.termType === 'DefaultGraph' ? n('g') : f.defaultGraph(),
      ),
    );
    out.push(s);
  }
  return out;
}

async function fillForProbes(ds) {
  const objects = [
    f.literal('plain'),
    f.literal('chat', 'fr'),
    f.literal('42', f.namedNode('http://www.w3.org/2001/XMLSchema#integer')),
    n('iri-object'),
    f.quad(n('a'), n('b'), n('c')),
  ];
  await ds.transaction(async (tx) => {
    for (const [i, o] of objects.entries()) {
      await tx.add(q(n(`s${i}`), n('p'), o));
      await tx.add(q(n(`s${i}`), n('q'), o, n('g')));
      await tx.add(q(f.blankNode(`b${i}`), n('p'), o, n('h')));
    }
  });
  return ds.match().toArray();
}

async function assertHasAllMatchesHas(ds, quads, label) {
  const one = [];
  for (const quad of quads) one.push(await ds.has(quad));
  assert.deepEqual(await ds.hasAll(quads), one, label);
  return one;
}

test('hasAll answers each quad as has does, in batches short and long', async () => {
  for (const options of [{}, { unionDefaultGraph: true }]) {
    const ds = Dataset.memory(options);
    const stored = await fillForProbes(ds);
    assert.equal(stored.length, 15);
    const short = probes(stored);
    const label = JSON.stringify(options);
    const answers = await assertHasAllMatchesHas(ds, short, label);
    // Where the default graph is the union of the named graphs, a quad of the default
    // graph is not read and a quad of a named graph is also found in the default graph.
    assert.equal(answers.filter(Boolean).length, 30, label);
    assert.equal(answers.length, 105);
    // a batch too long for the calling thread is answered on the pool
    const long = Array.from({ length: 12 }, () => short).flat();
    assert.deepEqual(
      await ds.hasAll(long),
      Array.from({ length: 12 }, () => answers).flat(),
      label,
    );
    // every length around a word of the answer's bit array
    for (const length of [1, 2, 31, 32, 33, 63, 64, 65])
      assert.deepEqual(
        await ds.hasAll(short.slice(0, length)),
        answers.slice(0, length),
        `${length}`,
      );
    // any iterable is read, and an empty one gives an empty answer
    const each = [];
    for (const quad of stored) each.push(await ds.has(quad));
    assert.deepEqual(await ds.hasAll(new Set(stored)), each);
    assert.equal(each.filter(Boolean).length, options.unionDefaultGraph ? 10 : 15, label);
    assert.deepEqual(
      await ds.hasAll(
        (function* () {
          yield* short;
        })(),
      ),
      answers,
    );
    assert.deepEqual(await ds.hasAll([]), []);
    await ds.close();
  }
});

test('hasAll answers a dataset on disk as has does', async () => {
  const path = await mkdtemp(join(tmpdir(), 'sparkles-has-all-'));
  try {
    const ds = await Dataset.open(path);
    const stored = await fillForProbes(ds);
    const short = probes(stored);
    const answers = await assertHasAllMatchesHas(ds, short, 'on disk');
    assert.equal(answers.filter(Boolean).length, 30);
    const long = Array.from({ length: 5 }, () => short).flat();
    assert.deepEqual(await ds.hasAll(long), Array.from({ length: 5 }, () => answers).flat());
    assert.deepEqual(await ds.hasAll([]), []);
    await ds.close();
  } finally {
    await rm(path, { recursive: true, force: true });
  }
});

test('hasAll reads one state of the dataset and sees later commits', async () => {
  const ds = Dataset.memory();
  const quads = Array.from({ length: 600 }, (_, i) => q(n(`s${i}`), n('p'), f.literal(`${i}`)));
  assert.deepEqual(
    await ds.hasAll(quads),
    quads.map(() => false),
  );
  await ds.addAll(quads.filter((_, i) => i % 3 === 0));
  assert.deepEqual(
    await ds.hasAll(quads),
    quads.map((_, i) => i % 3 === 0),
  );
  // answers asked for before a commit do not see it, on this thread or on the pool
  const before = [ds.hasAll(quads.slice(0, 10)), ds.hasAll(quads)];
  await ds.addAll(quads);
  assert.deepEqual(
    await before[0],
    quads.slice(0, 10).map((_, i) => i % 3 === 0),
  );
  assert.deepEqual(
    await before[1],
    quads.map((_, i) => i % 3 === 0),
  );
  assert.deepEqual(
    await ds.hasAll(quads),
    quads.map(() => true),
  );
  await ds.delete(quads[5]);
  assert.deepEqual(
    await ds.hasAll(quads.slice(0, 10)),
    quads.slice(0, 10).map((_, i) => i !== 5),
  );
});

test('hasAll rejects what is not a quad, and a closed dataset', async () => {
  const ds = Dataset.memory();
  const there = q(n('a'), n('p'), n('o'));
  await ds.add(there);
  const bad = { termType: 'NamedNode', value: 'not an iri', equals: () => false };
  await assert.rejects(ds.hasAll([there, q(n('b'), n('p'), bad)]));
  await assert.rejects(ds.hasAll([there, n('a')]), /Expected an RDF quad/);
  await assert.rejects(ds.hasAll([there, null]), /Expected an RDF quad/);
  await assert.rejects(ds.hasAll(42));
  // the rejected requests left no numbers that the addon lacks
  assert.deepEqual(await ds.hasAll([there, q(n('b'), n('p'), n('o'))]), [true, false]);
  await ds.close();
  await assert.rejects(ds.hasAll([there]), /closed/i);
  await assert.rejects(ds.hasAll([]), /closed/i);
});

test('hasAll stays correct with more distinct terms than the table keeps', async () => {
  const ds = Dataset.memory();
  const quads = Array.from({ length: 70000 }, (_, i) => q(n(`s${i}`), n('p'), f.literal(`${i}`)));
  await ds.addAll(quads.filter((_, i) => i % 2 === 0));
  const answers = await ds.hasAll(quads);
  assert.equal(answers.length, 70000);
  assert.ok(answers.every((found, i) => found === (i % 2 === 0)));
  // and again, with the terms the table kept sent by number
  assert.deepEqual(await ds.hasAll(quads), answers);
  assert.equal(await ds.has(quads[69998]), true);
  assert.equal(await ds.has(quads[69999]), false);
});
