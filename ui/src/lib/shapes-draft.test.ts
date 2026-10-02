import { describe, expect, it } from 'vitest';
import { draftShapesPath, type DraftShape, type ShapesDraft } from './api';
import { draftText, exclusions, parseSupport, summaryLine } from './shapes-draft';

const shape = (over: Partial<DraftShape> = {}): DraftShape => ({
  shape: 'urn:x-sparkles:shape:t:PersonShape',
  class: 'http://ex.org/Person',
  instances: 4,
  closed: false,
  properties: [
    {
      path: 'http://ex.org/name',
      instances: 4,
      maxValues: 2,
      constraints: [
        { component: 'minCount', value: 1, applicable: 4, satisfied: 4, excluded: 0 },
        { component: 'maxCount', value: 1, applicable: 4, satisfied: 3, excluded: 1 },
      ],
      rejected: [],
    },
    {
      path: 'http://ex.org/status',
      instances: 4,
      maxValues: 1,
      constraints: [{ component: 'in', value: ['"a"'], applicable: 4, satisfied: 2, excluded: 2 }],
      rejected: [],
    },
  ],
  ...over,
});

const draft = (shapes: DraftShape[]): ShapesDraft => ({
  draftFormat: 1,
  dataset: 't',
  snapshot: { version: 1, generation: 'mem', computedAt: '2026-10-02T00:00:00Z' },
  selection: { graph: 'default', reasoning: false },
  options: {
    support: 0.5,
    minInstances: 1,
    maxIn: 10,
    maxCount: 1,
    closed: false,
    base: 'urn:x-sparkles:shape:t:',
    classes: [],
  },
  totals: {
    shapes: shapes.length,
    propertyShapes: 2,
    constraints: 3,
    rejected: 0,
    skippedClasses: 0,
  },
  shapes,
  shacl: '@prefix sh: <http://www.w3.org/ns/shacl#> .\n',
  shex: 'PREFIX ex: <http://ex.org/>\n',
  shapeMap: '{FOCUS a <A>}@<AS>,\n{FOCUS a <B>}@<BS>',
});

describe('parseSupport', () => {
  it('accepts (0, 1]', () => {
    expect(parseSupport('1')).toBe(1);
    expect(parseSupport(' 0.95 ')).toBe(0.95);
    expect(parseSupport('0')).toBeNull();
    expect(parseSupport('1.5')).toBeNull();
    expect(parseSupport('x')).toBeNull();
    expect(parseSupport('')).toBeNull();
  });
});

describe('exclusions', () => {
  it('lists drafted constraints that exclude instances, most first', () => {
    expect(exclusions(shape())).toEqual([
      { path: 'http://ex.org/status', component: 'in', excluded: 2, applicable: 4 },
      { path: 'http://ex.org/name', component: 'maxCount', excluded: 1, applicable: 4 },
    ]);
    expect(exclusions(shape({ properties: [] }))).toEqual([]);
  });
});

describe('summaryLine', () => {
  it('counts the shapes and the constraints that reject data', () => {
    expect(summaryLine(draft([shape()]))).toBe(
      '1 shape · 2 property shapes · 3 constraints · 2 constraints would reject current data',
    );
    expect(summaryLine(draft([shape({ properties: [] })]))).toContain('the current data conforms');
  });
});

describe('draftText', () => {
  it('gives the Turtle, or the ShExC with its shape map as comments', () => {
    const d = draft([]);
    expect(draftText(d, 'shacl')).toBe(d.shacl);
    expect(draftText(d, 'shex')).toBe(
      'PREFIX ex: <http://ex.org/>\n\n# Shape map:\n# {FOCUS a <A>}@<AS>,\n# {FOCUS a <B>}@<BS>\n',
    );
  });
});

describe('draftShapesPath', () => {
  it('sends only the options that differ from the defaults', () => {
    expect(draftShapesPath('t')).toBe('/$/schema/t/shapes');
    expect(
      draftShapesPath('my ds', {
        support: 0.9,
        closed: true,
        graph: 'http://ex.org/g',
        classes: ['http://ex.org/A'],
      }),
    ).toBe(
      '/$/schema/my%20ds/shapes?support=0.9&closed=true&graph=http%3A%2F%2Fex.org%2Fg&class=http%3A%2F%2Fex.org%2FA',
    );
    expect(draftShapesPath('t', { support: 1, graph: 'default' })).toBe('/$/schema/t/shapes');
  });
});
