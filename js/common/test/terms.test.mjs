import test from 'node:test';
import assert from 'node:assert/strict';
import { DataFactory as N3 } from 'n3';
import {
  factory,
  Bindings,
  decodeTerm,
  encodeTerm,
  InvalidInputError,
  receiptOf,
} from '../dist/index.js';

test('RDF/JS factories accept foreign terms and preserve directional/triple literals', () => {
  const s = factory.namedNode('urn:s');
  const p = factory.namedNode('urn:p');
  const literal = factory.literal('مرحبا', { language: 'AR', direction: 'rtl' });
  assert.equal(literal.language, 'ar');
  assert.equal(literal.direction, 'rtl');
  assert(factory.fromTerm(literal).equals(literal));
  assert(
    decodeTerm(encodeTerm(factory.quad(s, p, factory.quad(s, p, literal)))).equals(
      factory.quad(s, p, factory.quad(s, p, literal)),
    ),
  );
  assert(
    factory
      .fromQuad(N3.quad(N3.namedNode('urn:s'), N3.namedNode('urn:p'), N3.literal('x')))
      .equals(factory.quad(s, p, factory.literal('x'))),
  );
});
test('bindings support variables, iteration and mapping', () => {
  const b = new Bindings([
    ['x', factory.literal('one')],
    ['y', factory.literal('two')],
  ]);
  assert.equal(b.get(factory.variable('x')).value, 'one');
  assert.equal(b.get('?x').value, 'one');
  assert.equal(b.size, 2);
  assert.deepEqual(
    [...b.keys()].map((v) => v.value),
    ['x', 'y'],
  );
  assert.equal(b.filter((t) => t.value === 'one').size, 1);
  assert.equal(b.map((t) => factory.literal(t.value + '!')).get('x').value, 'one!');
});
test('literal conversion is lossless and validates factory inputs', () => {
  for (const v of [true, false, 1, 1.5, 9007199254740993n, -9007199254740993n, 'x'])
    assert.equal(factory.fromJs(v).toJs(), v);
  assert.deepEqual(factory.fromJs(new Uint8Array([0, 1, 255])).toJs(), new Uint8Array([0, 1, 255]));
  assert.throws(() => factory.fromJs(9007199254740992), InvalidInputError);
  assert.throws(() => factory.namedNode('relative'), InvalidInputError);
  assert.throws(() => factory.literal('x', 'bad tag'), InvalidInputError);
});

test('Unicode blank labels follow Turtle grammar and receipt counts fit u64', () => {
  for (const label of ['é', 'مرحبا', '𝒙', 'a·b', 'a\u0301', '0', 'a.b', 'a-b'])
    assert.equal(factory.blankNode(label).value, label);
  for (const label of ['', '.a', 'a.', '-a', 'a:b', '\u0301a', 'a b'])
    assert.throws(() => factory.blankNode(label), InvalidInputError);
  for (const value of [-1, '-1', '18446744073709551616', 9007199254740992])
    assert.throws(() => receiptOf({ datasetId: 'id', commit: { seq: value } }), InvalidInputError);
  assert.equal(
    receiptOf({ datasetId: 'id', commit: { seq: '18446744073709551615' } }).commit.seq,
    18446744073709551615n,
  );
});

test('literal and IRI validation follows RDF lexical forms', () => {
  assert.equal(factory.literal('x', '').datatype.value, 'http://www.w3.org/2001/XMLSchema#string');
  assert.throws(() => factory.namedNode('urn:a\u0000'), InvalidInputError);
  assert.throws(() => factory.namedNode('urn:a\uD800'), InvalidInputError);
  assert.equal(
    factory.literal('0x10', factory.namedNode('http://www.w3.org/2001/XMLSchema#double')).toJs(),
    '0x10',
  );
});
