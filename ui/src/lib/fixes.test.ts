import { describe, expect, it } from 'vitest';
import type { ShaclResult, ShexResult } from './api';
import { hasPlaceholder, suggestFix, validLexical } from './fixes';
import { fromShacl, fromShex, type VResult } from './validation-overlay';

const EX = 'http://example.org/';
const SH = 'http://www.w3.org/ns/shacl#';
const XSD = 'http://www.w3.org/2001/XMLSchema#';
const uri = (v: string) => ({ type: 'uri' as const, value: v });
const lit = (v: string, datatype?: string) => ({ type: 'literal' as const, value: v, datatype });

function shacl(component: string, extra: Partial<ShaclResult> = {}): VResult {
  return fromShacl({
    focusNode: uri(EX + 'p17'),
    resultPath: uri(EX + 'name'),
    value: null,
    sourceShape: uri(EX + 'PersonShape'),
    sourceConstraintComponent: uri(SH + component),
    severity: uri(SH + 'Violation'),
    messages: [],
    ...extra,
  });
}

function shex(failure: NonNullable<ShexResult['appinfo']>['failures'][number]): VResult {
  return fromShex({
    node: uri(EX + 'p17'),
    shape: uri(EX + 'Person'),
    status: 'nonconformant',
    appinfo: { failures: [failure] },
  })[0];
}

describe('suggestFix', () => {
  it('sh:minCount inserts a placeholder value', () => {
    const q = suggestFix(
      shacl('MinCountConstraintComponent', {
        messages: ['minCount[1]: Invalid cardinality: expected min 1: Got count = 0'],
      }),
    )!;
    expect(q).toContain(`INSERT DATA {\n  <${EX}p17> <${EX}name> ?value .\n}`);
    expect(q).toMatch(/^# Suggested fix for /);
    expect(hasPlaceholder(q)).toBe(true);
    expect(hasPlaceholder(q.replace('?value .', '"Ana" .'))).toBe(false);
  });

  it('sh:maxCount deletes a chosen value and lists the values', () => {
    const q = suggestFix(shacl('MaxCountConstraintComponent', { messages: ['maxCount[1]: x'] }), {
      values: [lit('a@x'), lit('b@x')],
    })!;
    expect(q).toContain('#   "a@x"   (kept)');
    expect(q).toContain('#   "b@x"\n');
    expect(q).toContain(`DELETE DATA {\n  <${EX}p17> <${EX}name> ?value .\n}`);
  });

  it('sh:datatype casts a valid lexical form and nothing else', () => {
    const msg = [`DatatypeConstraint[<${XSD}integer>]: Expected <${XSD}integer> : Actual "30"`];
    const ok = suggestFix(
      shacl('DatatypeConstraintComponent', { value: lit('30'), messages: msg }),
    )!;
    expect(ok).toContain(`INSERT { <${EX}p17> <${EX}name> "30"^^<${XSD}integer> }`);
    expect(ok).toContain(`DELETE { <${EX}p17> <${EX}name> "30" }`);
    expect(
      suggestFix(shacl('DatatypeConstraintComponent', { value: lit('thirty'), messages: msg })),
    ).toBeNull();
    // a custom message hides the datatype: no cast
    expect(
      suggestFix(shacl('DatatypeConstraintComponent', { value: lit('30'), messages: ['bad'] })),
    ).toBeNull();
  });

  it('sh:class types the value, with a placeholder when the class is unknown', () => {
    const q = suggestFix(
      shacl('ClassConstraintComponent', {
        value: uri(EX + 'acme'),
        messages: [`ClassConstraint[<${EX}Org>]: Expected class <${EX}Org> for <${EX}acme>`],
      }),
    )!;
    expect(q).toContain(
      `<${EX}acme> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <${EX}Org> .`,
    );
    expect(q).toContain('The other choice is to change the');
    const p = suggestFix(shacl('ClassConstraintComponent', { value: uri(EX + 'acme') }))!;
    expect(p).toContain('?class');
    expect(hasPlaceholder(p)).toBe(true);
  });

  it('sh:in and sh:hasValue list the allowed values', () => {
    const q = suggestFix(
      shacl('InConstraintComponent', {
        value: lit('blue'),
        messages: ['InConstraint["red", "green"] : Expected one of the list : "blue"'],
      }),
    )!;
    expect(q).toContain('# The allowed values are: "red", "green"');
    expect(q).toContain(`INSERT { <${EX}p17> <${EX}name> ?allowed }`);
    expect(hasPlaceholder(q)).toBe(true);
    const h = suggestFix(
      shacl('HasValueConstraintComponent', { messages: [`HasValue[<${EX}x>]`] }),
    )!;
    expect(h).toContain(`<${EX}p17> <${EX}name> <${EX}x> .`);
    expect(hasPlaceholder(h)).toBe(false);
  });

  it('sh:closed deletes the triple', () => {
    const q = suggestFix(
      shacl('ClosedConstraintComponent', { resultPath: uri(EX + 'nick'), value: lit('Al') }),
    )!;
    expect(q).toContain(`DELETE DATA {\n  <${EX}p17> <${EX}nick> "Al" .\n}`);
  });

  it('ShEx cardinality, datatype and valueSet take the same forms', () => {
    const min = suggestFix(
      shex({
        kind: 'cardinality',
        predicate: EX + 'name',
        inverse: false,
        min: 1,
        max: 1,
        count: 0,
      }),
    )!;
    expect(min).toContain('INSERT DATA');
    expect(min).toContain('ShEx cardinality');
    const max = suggestFix(
      shex({
        kind: 'cardinality',
        predicate: EX + 'name',
        inverse: false,
        min: 0,
        max: 1,
        count: 2,
      }),
    )!;
    expect(max).toContain('DELETE DATA');
    const inv = suggestFix(
      shex({
        kind: 'cardinality',
        predicate: EX + 'knows',
        inverse: true,
        min: 1,
        max: null,
        count: 0,
      }),
    )!;
    expect(inv).toContain(`?value <${EX}knows> <${EX}p17> .`);
    const dt = suggestFix(shex({ kind: 'datatype', value: lit('7'), constraint: 'xsd:integer' }))!;
    expect(dt).toContain(`INSERT { <${EX}p17> ?p "7"^^<${XSD}integer> }`);
    const vs = suggestFix(
      shex({ kind: 'valueSet', value: uri(EX + 'z'), constraint: '[ex:a ex:b]' }),
    )!;
    expect(vs).toContain('# The allowed values are: [ex:a ex:b]');
    expect(suggestFix(shex({ kind: 'closed', predicate: EX + 'q', value: lit('v') }))).toContain(
      'DELETE DATA',
    );
  });

  it('gives no suggestion for the other components, paths and blank nodes', () => {
    for (const c of [
      'PatternConstraintComponent',
      'SPARQLConstraintComponent',
      'NodeConstraintComponent',
      'QualifiedValueShapeConstraintComponent',
      'MinLengthConstraintComponent',
    ])
      expect(suggestFix(shacl(c, { value: lit('x') })), c).toBeNull();
    expect(
      suggestFix(
        shacl('MinCountConstraintComponent', {
          resultPath: { type: 'path', value: `<${EX}a>/<${EX}b>` },
        }),
      ),
    ).toBeNull();
    expect(
      suggestFix(
        shacl('MinCountConstraintComponent', { focusNode: { type: 'bnode', value: 'b1' } }),
      ),
    ).toBeNull();
    expect(suggestFix(shex({ kind: 'noMatch', detail: 'x' }))).toBeNull();
    expect(
      suggestFix(shex({ kind: 'reference', shape: EX + 'S', value: uri(EX + 'v') })),
    ).toBeNull();
  });
});

describe('validLexical', () => {
  it('checks the common XSD datatypes', () => {
    expect(validLexical('42', XSD + 'integer')).toBe(true);
    expect(validLexical('4.2', XSD + 'integer')).toBe(false);
    expect(validLexical('300', XSD + 'byte')).toBe(false);
    expect(validLexical('2026-10-23', XSD + 'date')).toBe(true);
    expect(validLexical('2026-13-01', XSD + 'date')).toBe(false);
    expect(validLexical('true', XSD + 'boolean')).toBe(true);
    expect(validLexical('1e3', XSD + 'double')).toBe(true);
    expect(validLexical('x', EX + 'custom')).toBeNull();
  });
});
