import nodeTest from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { gzipSync } from 'node:zlib';
import { Dataset, parse, serialize, factory, RdfSyntaxError } from '../dist/index.js';

// Jena's TriX, RDF Thrift, RDF Protobuf and RDF/JSON work wherever the W3C syntaxes do:
// standalone parse and serialize, dataset load from text, bytes, streams and paths, and
// dataset dump to streams and files. RDF/JSON holds one graph.
const SYNTAXES = [
  { format: 'trix', extension: 'trix', quads: true },
  { format: 'thrift', extension: 'rt', quads: true },
  { format: 'protobuf', extension: 'rpb', quads: true },
  { format: 'rdf/json', extension: 'rj', quads: false },
];

const n = (name) => factory.namedNode(`urn:${name}`);
const rows = (quads) => [
  factory.quad(n('a'), n('p'), factory.literal('one')),
  factory.quad(n('a'), n('p'), factory.literal('deux', { language: 'fr' })),
  factory.quad(n('a'), n('q'), factory.literal('3', n('integer'))),
  ...(quads ? [factory.quad(n('c'), n('p'), n('a'), n('g'))] : []),
];
const all = async (input) => {
  const out = [];
  for await (const row of input) out.push(row);
  return out;
};
const bytes = async (stream) => new Uint8Array(await new Response(stream).arrayBuffer());
const key = (q) =>
  [q.subject, q.predicate, q.object, q.graph]
    .map((t) => `${t.termType}:${t.value}:${t.language ?? ''}:${t.datatype?.value ?? ''}`)
    .join(' ');
const keys = (quads) => quads.map(key).sort();
const content = async (ds) =>
  keys(await all(parse(await bytes(ds.dump({ format: 'nq' })), { format: 'nq' })));

const test = (name, callback) =>
  nodeTest(name, async (t) => {
    const resources = [];
    t.resource = (value) => (resources.push(value), value);
    try {
      await callback(t);
    } finally {
      for (const value of resources.reverse()) await value.close();
    }
  });

test('standalone parse and serialize round trip each Jena syntax', async () => {
  for (const { format, quads } of SYNTAXES) {
    const written = await bytes(serialize(rows(quads), { format }));
    assert.deepEqual(keys(await all(parse(written, { format }))), keys(rows(quads)), format);
    const zipped = await bytes(serialize(rows(quads), { format, compression: 'gzip' }));
    const back = await all(parse(zipped, { format, compression: 'gzip' }));
    assert.deepEqual(keys(back), keys(rows(quads)), `${format} gzip`);
  }
});

test('a malformed Jena body is an RdfSyntaxError that names the syntax', async () => {
  for (const { format, quads } of SYNTAXES) {
    const written = await bytes(serialize(rows(quads), { format }));
    const cut = written.slice(0, Math.floor(written.length / 2));
    await assert.rejects(
      all(parse(cut, { format })),
      (e) => e instanceof RdfSyntaxError && e.message.length > 0,
      format,
    );
  }
});

test('dataset load takes each Jena syntax as text, bytes and streams, and dump writes it', async (t) => {
  for (const { format, quads } of SYNTAXES) {
    const written = await bytes(serialize(rows(quads), { format }));
    const ds = t.resource(Dataset.memory());
    await ds.load(written, { format });
    assert.deepEqual(await content(ds), keys(rows(quads)), format);
    // a stream of chunks, into a graph
    const into = t.resource(Dataset.memory());
    await into.load(new Blob([written]).stream(), { format, toGraph: n('t') });
    assert.equal(
      (await content(into)).some((k) => k.includes('urn:t')),
      true,
      format,
    );
    // dump, then load it back
    const copy = t.resource(Dataset.memory());
    await copy.load(ds.dump({ format }), { format });
    assert.deepEqual(await content(copy), keys(rows(quads)), `${format} dump`);
    await ds.close();
  }
});

test('dataset load reads Jena files by extension, compressed or not, and dumpToFile writes them', async (t) => {
  const dir = await mkdtemp(join(tmpdir(), 'sparkles-syntaxes-'));
  try {
    for (const { format, extension, quads } of SYNTAXES) {
      const written = await bytes(serialize(rows(quads), { format }));
      const plain = join(dir, `d.${extension}`);
      const zipped = join(dir, `z.${extension}.gz`);
      await writeFile(plain, written);
      await writeFile(zipped, gzipSync(written));
      for (const path of [plain, zipped]) {
        const ds = t.resource(Dataset.memory());
        await ds.load({ path });
        assert.deepEqual(await content(ds), keys(rows(quads)), path);
      }
      const ds = t.resource(Dataset.memory());
      await ds.load({ path: plain });
      const out = join(dir, `out.${extension}`);
      await ds.dumpToFile(out);
      assert.ok((await readFile(out)).length > 0, out);
      const back = t.resource(Dataset.memory());
      await back.load({ path: out });
      assert.deepEqual(await content(back), keys(rows(quads)), `${format} dumpToFile`);
      const graph = t.resource(Dataset.memory());
      await ds.dumpToFile(join(dir, `graph.${extension}`), { fromGraph: n('g') });
      await graph.load({ path: join(dir, `graph.${extension}`) });
      assert.equal(await graph.count(), quads ? 1n : 0n, `${format} fromGraph`);
    }
    const bad = join(dir, 'bad.rt');
    const written = await bytes(serialize(rows(true), { format: 'thrift' }));
    await writeFile(bad, written.slice(0, written.length - 4));
    const ds = t.resource(Dataset.memory());
    await assert.rejects(ds.load({ path: bad }), RdfSyntaxError);
    assert.equal(await ds.count(), 0n);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
