import { describe, expect, it } from 'vitest';
import type { CursorPlan, PlanNode } from './api';
import { normalizeResult, type SparklesResult } from './api';
import { ancestors, cites, marks, planOf, type ExplainNote } from './explain';

const node = (operator: string, children: PlanNode[] = []): PlanNode => ({
  operator,
  description: '',
  columns: [],
  sortedOn: [],
  estimatedRows: 1,
  estimatedCost: 1,
  actualRows: 1,
  timeMs: 1,
  cached: false,
  children,
});

describe('planOf', () => {
  it('gives every node its path as id', () => {
    const p = planOf(node('Project', [node('Join', [node('Scan'), node('Scan')])]));
    expect(p.id).toBe('0');
    expect(p.children[0].id).toBe('0.0');
    expect(p.children[0].children[1].id).toBe('0.0.1');
  });

  it('reads a streamed plan like an eager one and keeps its streaming facts', () => {
    const c: CursorPlan = {
      id: '0',
      operator: { ...node('Limit'), id: '0' },
      materializes: false,
      fullInputBeforeOutput: false,
      growingState: false,
      complete: true,
      children: [
        {
          id: '0.0',
          operator: { ...node('Sort'), id: '0.0' },
          materializes: true,
          fullInputBeforeOutput: true,
          growingState: true,
          complete: false,
          reason: 'sorts its whole input',
          children: [],
        },
      ],
    };
    const p = planOf(c);
    expect(p.operator).toBe('Limit');
    expect(p.children[0].operator).toBe('Sort');
    expect(p.children[0].materializes).toBe(true);
    expect(p.children[0].reason).toBe('sorts its whole input');
    expect(p.children[0].complete).toBe(false);
    expect(p.children[0].id).toBe('0.0');
  });

  it('normalizes the plan of a streamed result', () => {
    const r = normalizeResult({
      queryType: 'SELECT',
      vars: ['x'],
      rows: [],
      meta: {
        totalRows: 0,
        sentRows: 0,
        timing: { parseMs: 0, planMs: 0, execMs: 0, serializeMs: 0, totalMs: 0 },
        plan: {
          operator: node('Scan'),
          materializes: false,
          fullInputBeforeOutput: false,
          growingState: false,
          complete: true,
          children: [],
        } as unknown as PlanNode,
      },
    } as SparklesResult);
    expect(r.meta.plan.operator).toBe('Scan');
    expect(r.meta.plan.id).toBe('0');
  });
});

describe('marks', () => {
  it('keeps the most severe note of each node', () => {
    const n = (node: string | null, severity: ExplainNote['severity']): ExplainNote => ({
      node,
      code: 'x',
      severity,
      text: '',
      source: 'explain',
    });
    const m = marks([n('0.1', 'info'), n('0.1', 'high'), n('0.2', 'warning'), n(null, 'high')]);
    expect(m.get('0.1')).toBe('high');
    expect(m.get('0.2')).toBe('warning');
    expect(m.size).toBe(2);
  });
});

describe('ids', () => {
  it('lists ancestors root first and tells what cites a node', () => {
    expect(ancestors('0.1.2')).toEqual(['0', '0.1']);
    expect(ancestors('0')).toEqual([]);
    expect(cites({ text: '', nodes: ['0.1', '0.2'] }, '0.2')).toBe(true);
    expect(
      cites({ node: '0.3', code: 'x', severity: 'info', text: '', source: 'explain' }, '0.2'),
    ).toBe(false);
  });
});
