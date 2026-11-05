import { afterEach, describe, expect, it, vi } from 'vitest';
import { classifyOntology } from './ontology';

const source = {
  boundStoreId: 'store-a',
  familyId: 'family-a',
  commit: 7,
  inputFingerprint: 'input-a',
  contentFingerprint: 'content-a',
};
function page(commit: number, offset: number, overrides: Record<string, unknown> = {}) {
  return {
    commit,
    nodes: [
      {
        node: offset,
        classes: [`http://example.org/C${offset}`],
        parents: [],
        parentClasses: [],
        children: 0,
      },
    ],
    complete: false,
    partial: false,
    excludedCount: 1,
    consistency: 'consistent',
    classificationSource: { ...source, commit },
    ...(offset === 0 ? { next: 1 } : {}),
    ...overrides,
  };
}
function mock(pages: unknown[]) {
  const calls: { url: string; body: Record<string, unknown> }[] = [];
  vi.stubGlobal('fetch', async (url: string, init: RequestInit) => {
    calls.push({ url, body: JSON.parse(String(init.body)) });
    return new Response(JSON.stringify(pages[calls.length - 1]), {
      headers: { 'Content-Type': 'application/json' },
    });
  });
  return calls;
}
afterEach(() => vi.unstubAllGlobals());
describe('explicit legacy classification tree pages', () => {
  it('never appends a page from a different commit', async () => {
    mock([page(7, 0), page(8, 1)]);
    await expect(classifyOntology('ds', { graph: null, imports: true })).rejects.toThrow(
      /changed|commit/i,
    );
  });
  it('pins subsequent requests and accepts identical content with distinct run IDs and exclusions', async () => {
    const calls = mock([
      page(7, 0, { resultId: 'first-run' }),
      page(7, 1, { resultId: 'second-run' }),
    ]);
    const result = await classifyOntology('ds', { graph: null, imports: true });
    expect(result.nodes).toHaveLength(2);
    expect(result.complete).toBe(false);
    expect(calls[1].url).toContain('at=commit%3A7');
  });
  it('rejects changed taxonomy content under the same exact input', async () => {
    mock([
      page(7, 0),
      page(7, 1, { classificationSource: { ...source, contentFingerprint: 'different' } }),
    ]);
    await expect(classifyOntology('ds', { graph: null, imports: true })).rejects.toThrow(
      /changed|content/i,
    );
  });
  it('rejects unexpected partial answers rather than assembling a tree', async () => {
    mock([page(7, 0, { partial: true }), page(7, 1, { partial: true })]);
    await expect(classifyOntology('ds', { graph: null, imports: true })).rejects.toThrow(
      /partial/i,
    );
  });
});
