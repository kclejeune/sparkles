import { afterEach, describe, expect, it, vi } from 'vitest';
import { cloneDataset, type Task } from './api';
import {
  INFERRED_GRAPH,
  cloneBody,
  cloneDetail,
  cloneMethodText,
  originSummary,
  parseGraphs,
  selectedGraphs,
} from './clone';

afterEach(() => vi.unstubAllGlobals());

describe('clone requests', () => {
  it('leaves the defaults out', () => {
    expect(cloneBody('copy')).toEqual({ name: 'copy', inferences: 'copy' });
    expect(cloneBody('copy', { type: 'persistent', mode: 'auto', graphs: [] })).toEqual({
      name: 'copy',
      inferences: 'copy',
    });
  });

  it('sends the type, the mode of a persistent clone and the graphs', () => {
    expect(
      cloneBody('copy', { inferences: 'drop', mode: 'link', graphs: ['default', 'http://g/*'] }),
    ).toEqual({
      name: 'copy',
      inferences: 'drop',
      mode: 'link',
      graphs: ['default', 'http://g/*'],
    });
    // an in-memory clone never shares index files: no mode
    expect(cloneBody('copy', { type: 'mem', mode: 'rebuild' })).toEqual({
      name: 'copy',
      inferences: 'copy',
      type: 'mem',
    });
  });

  it('posts the body as JSON', async () => {
    const calls: { url: string; init: RequestInit }[] = [];
    vi.stubGlobal('fetch', async (url: string, init: RequestInit = {}) => {
      calls.push({ url, init });
      return new Response(JSON.stringify({ id: '3', kind: 'clone' }), {
        status: 202,
        headers: { 'Content-Type': 'application/json' },
      });
    });
    await cloneDataset('my wiki', 'sandbox', { type: 'mem', graphs: ['default'] });
    expect(calls[0].url).toBe('/$/datasets/my%20wiki/clone');
    expect(calls[0].init.method).toBe('POST');
    expect(JSON.parse(String(calls[0].init.body))).toEqual({
      name: 'sandbox',
      inferences: 'copy',
      type: 'mem',
      graphs: ['default'],
    });
  });
});

describe('graph selection', () => {
  it('splits typed names and drops repeats', () => {
    expect(parseGraphs('  http://a\nhttp://b http://a\n\n')).toEqual(['http://a', 'http://b']);
    expect(parseGraphs('')).toEqual([]);
  });

  it('names the inferred graph when inferences are copied', () => {
    expect(selectedGraphs(['default'], 'http://g/*', true)).toEqual([
      'default',
      'http://g/*',
      INFERRED_GRAPH,
    ]);
    expect(selectedGraphs(['default'], '', false)).toEqual(['default']);
    // nothing chosen: nothing to add the inferred graph to
    expect(selectedGraphs([], ' ', true)).toEqual([]);
  });
});

describe('clone results', () => {
  const task = (detail: unknown, kind: Task['kind'] = 'clone') => ({ kind, detail });

  it('reads a clone task’s detail', () => {
    const d = {
      method: 'reflink',
      rebuildReason: null,
      type: 'persistent',
      quads: 10,
      graphs: 2,
      bytes: 4096,
      millis: 12,
    };
    expect(cloneDetail(task(d))).toEqual(d);
    expect(cloneDetail(task(d, 'compact'))).toBeNull();
    expect(cloneDetail(task(undefined))).toBeNull();
    expect(cloneDetail(task({ quads: 1 }))).toBeNull();
  });

  it('says how the copy was made', () => {
    expect(cloneMethodText({ method: 'link', rebuildReason: null })).toBe(
      'hard-linked the source’s index files',
    );
    expect(cloneMethodText({ method: 'reflink', rebuildReason: null })).toBe(
      'shared the source’s index files by reflink',
    );
    expect(cloneMethodText({ method: 'copy', rebuildReason: null })).toBe(
      'copied the source’s index files',
    );
    expect(cloneMethodText({ method: 'rebuild', rebuildReason: 'some graphs are left out' })).toBe(
      'rebuilt the index: some graphs are left out',
    );
    expect(cloneMethodText({ method: 'rebuild', rebuildReason: null })).toBe('rebuilt the index');
  });

  it('sums up the origin', () => {
    expect(originSummary({ inferences: 'copy' })).toBe('');
    expect(
      originSummary({ inferences: 'drop', graphs: ['default', 'http://g'], method: 'rebuild' }),
    ).toBe(', without inferences, only default, http://g, index rebuilt');
    expect(originSummary({ inferences: 'copy', method: 'link' })).toBe(', index by link');
  });
});
