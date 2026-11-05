import test from 'node:test';
import assert from 'node:assert/strict';
import { native } from '../dist/native.js';

// The normal suite uses the default addon. The same tests can check a feature-free
// build through the native loader's SPARKLES_NODE_NATIVE override.
const disabled = process.env.SPARKLES_TEST_ONTOLOGY_DISABLED === '1';

function dataset(t) {
  const ds = native.NativeDataset.memory('{}');
  t.after(() => ds.close());
  return ds;
}

async function admin(ds, op, args = '{}') {
  return JSON.parse(await ds.admin(op, args, '{}', new native.Cancellation()));
}

function failure(kind, message) {
  return (error) => {
    const result = JSON.parse(error.message);
    assert.equal(result.kind, kind);
    if (message) assert.match(result.message, message);
    return true;
  };
}

test('native ontology feature builds keep JSON validation and unrelated administration', async (t) => {
  const ds = dataset(t);
  await assert.rejects(admin(ds, 'ontology.keptClassification', '{'), failure('InvalidInputError'));
  assert.equal((await admin(ds, 'info')).quads, '0');
});

test(
  'native ontology enabled builds reject duplicate selectors before decoding',
  { skip: disabled },
  async (t) => {
    const ds = dataset(t);
    for (const [op, args] of [
      ['ontology.keptClassification', '{"part":"status","part":"status"}'],
      ['ontology.classify', '{"projection":"frame","about":"urn:A","about":"urn:B"}'],
    ]) {
      await assert.rejects(admin(ds, op, args), failure('InvalidInputError', /duplicate selector/));
    }
    assert.equal((await admin(ds, 'ontology.keptClassification')).status, 'unavailable');
  },
);

test(
  'native ontology disabled builds reject reads and writes without mutation',
  { skip: !disabled },
  async (t) => {
    const ds = dataset(t);
    const before = await admin(ds, 'info');
    for (const [op, args] of [
      ['ontology.summary', '{}'],
      ['ontology.keptClassification', '{}'],
      ['ontology.classify', '{"target":"urn:taxonomy"}'],
      ['ontology.edit', '{"add":["Declaration(Class(<urn:A>))"]}'],
    ]) {
      await assert.rejects(
        admin(ds, op, args),
        failure('UnsupportedError', /unknown or disabled administration operation/),
      );
    }
    assert.deepEqual(await admin(ds, 'info'), before);
  },
);
