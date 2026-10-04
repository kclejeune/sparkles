import { describe, expect, it } from 'vitest';
import type { ConflictCell } from './api';
import {
  cellKey,
  choiceOf,
  graphHeading,
  groupConflicts,
  groupKey,
  guardRows,
  guardSummary,
  keepChoices,
  mergeHref,
  noChoices,
  resolutionsOf,
  resultObjects,
  unresolved,
} from './merge-page';

const int = (n: number) => `"${n}"^^<http://www.w3.org/2001/XMLSchema#integer>`;
const cell = (graph: string | null, s: string, p: string, ours: number, theirs: number) =>
  ({
    graph,
    subject: `<urn:${s}>`,
    predicate: `<urn:${p}>`,
    base: [int(1)],
    ours: [int(ours)],
    theirs: [int(theirs)],
  }) satisfies ConflictCell;

const cells = [
  cell(null, 'a', 'age', 2, 3),
  cell('<urn:g>', 'b', 'age', 4, 5),
  cell(null, 'a', 'size', 6, 7),
];

describe('groupConflicts', () => {
  it('groups by graph, the default graph first, then by subject', () => {
    const g = groupConflicts(cells);
    expect(g.map((x) => x.graph)).toEqual([null, '<urn:g>']);
    expect(g[0].subjects).toHaveLength(1);
    expect(g[0].subjects[0].cells.map((c) => c.predicate)).toEqual(['<urn:age>', '<urn:size>']);
    expect(g[0].count).toBe(2);
    expect(graphHeading(null)).toBe('Default graph');
    expect(graphHeading('<http://ex.org/g>', { ex: 'http://ex.org/' })).toBe('Graph ex:g');
  });
});

describe('choices and resolutions', () => {
  it('sends a group choice once and a row choice that overrides it', () => {
    const ch = noChoices();
    ch.groups[groupKey(null, '<urn:a>')] = 'theirs';
    ch.cells[cellKey(cells[2])] = 'ours';
    expect(choiceOf(cells[0], ch)).toBe('theirs');
    expect(choiceOf(cells[2], ch)).toBe('ours');
    expect(choiceOf(cells[1], ch)).toBeNull();
    expect(resolutionsOf(cells, ch)).toEqual([
      { graph: null, subject: '<urn:a>', take: 'theirs' },
      { graph: null, subject: '<urn:a>', predicate: '<urn:size>', take: 'ours' },
    ]);
    expect(unresolved(cells, 3, ch, 'fail')).toBe(1);
    expect(unresolved(cells, 3, ch, 'union')).toBe(0);
    // conflicts the report left out stay open without a rule
    expect(unresolved(cells, 10, ch, 'fail')).toBe(8);
  });

  it('keeps only the choices for conflicts still reported', () => {
    const ch = noChoices();
    ch.groups[groupKey('<urn:g>', '<urn:b>')] = 'base';
    ch.cells[cellKey(cells[0])] = 'ours';
    const kept = keepChoices([cells[0]], ch);
    expect(kept).toEqual({ groups: {}, cells: { [cellKey(cells[0])]: 'ours' } });
  });

  it('shows the values a choice leaves', () => {
    expect(resultObjects(cells[0], 'union')).toEqual([int(2), int(3)]);
    expect(resultObjects(cells[0], 'base')).toEqual([int(1)]);
    expect(resultObjects(cells[0], null)).toBeNull();
  });

  it('links to the page', () => {
    expect(mergeHref('/ui/datasets/prod', 'dev', 'main')).toBe(
      '/ui/datasets/prod/merge?source=dev&target=main',
    );
  });
});

describe('guard reports', () => {
  it('reads SHACL results and ShEx associations', () => {
    const rows = guardRows(
      {
        language: 'shacl',
        blocking: 1,
        results: [
          {
            focusNode: { type: 'uri', value: 'http://ex.org/p1' },
            resultPath: { type: 'uri', value: 'http://ex.org/name' },
            sourceShape: { type: 'bnode', value: 'b0' },
            sourceConstraintComponent: {
              type: 'uri',
              value: 'http://www.w3.org/ns/shacl#MinCountConstraintComponent',
            },
            messages: [],
          },
        ],
      },
      { ex: 'http://ex.org/' },
    );
    expect(rows).toEqual([
      { node: 'ex:p1', path: 'ex:name', message: 'MinCountConstraintComponent', shape: '_:b0' },
    ]);
    const shex = guardRows({
      results: [
        {
          node: { type: 'uri', value: 'urn:x' },
          shape: { type: 'uri', value: 'urn:S' },
          status: 'nonconformant',
          reason: 'no name',
        },
      ],
    });
    expect(shex[0]).toMatchObject({ node: '<urn:x>', message: 'no name', shape: '<urn:S>' });
    expect(guardSummary({ blocking: 2 })).toBe('2 blocking results');
  });
});
