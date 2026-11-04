import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { LABEL_CHUNK, LabelBatch } from './label-batch';
import type { Lit } from './ontology';

const lit = (v: string): Lit => ({ '@value': v });

describe('LabelBatch', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  test('gathers the IRIs asked for in a moment into one read', async () => {
    const read = vi.fn(async (iris: string[]) => new Map(iris.map((i) => [i, [lit(`L ${i}`)]])));
    const applied: Map<string, Lit[]>[] = [];
    const b = new LabelBatch(read, (m) => applied.push(m));
    b.want('a');
    b.want('b');
    b.want('a');
    expect(read).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(30);
    expect(read).toHaveBeenCalledTimes(1);
    expect(read.mock.calls[0][0]).toEqual(['a', 'b']);
    expect(applied[0].get('a')).toEqual([lit('L a')]);
  });

  test('does not ask twice for an IRI, and records an IRI with no label as having none', async () => {
    const read = vi.fn(async () => new Map<string, Lit[]>());
    const applied: Map<string, Lit[]>[] = [];
    const b = new LabelBatch(read, (m) => applied.push(m));
    b.want('a');
    await vi.advanceTimersByTimeAsync(30);
    b.want('a');
    await vi.advanceTimersByTimeAsync(30);
    expect(read).toHaveBeenCalledTimes(1);
    expect(applied[0].get('a')).toEqual([]);
  });

  test('reads a long queue in chunks', async () => {
    const read = vi.fn(async (_iris: string[]) => new Map<string, Lit[]>());
    const b = new LabelBatch(read, () => {});
    for (let i = 0; i < LABEL_CHUNK * 2 + 5; i++) b.want(`i${i}`);
    await vi.advanceTimersByTimeAsync(30);
    expect(read).toHaveBeenCalledTimes(3);
    expect(read.mock.calls.map((c) => (c[0] as string[]).length)).toEqual([
      LABEL_CHUNK,
      LABEL_CHUNK,
      5,
    ]);
  });

  test('ignores blank nodes and known IRIs', async () => {
    const read = vi.fn(async () => new Map<string, Lit[]>());
    const b = new LabelBatch(read, () => {});
    b.know(['k']);
    b.want('k');
    b.want('_:b1');
    await vi.advanceTimersByTimeAsync(30);
    expect(read).not.toHaveBeenCalled();
  });

  test('a reset drops an answer in flight and asks again afterwards', async () => {
    let release: (m: Map<string, Lit[]>) => void = () => {};
    const read = vi.fn(() => new Promise<Map<string, Lit[]>>((r) => (release = r)));
    const applied: Map<string, Lit[]>[] = [];
    const b = new LabelBatch(read, (m) => applied.push(m));
    b.want('a');
    await vi.advanceTimersByTimeAsync(30);
    b.reset();
    release(new Map([['a', [lit('old')]]]));
    await vi.advanceTimersByTimeAsync(0);
    expect(applied).toEqual([]);
    b.want('a');
    expect(b.has('a')).toBe(true);
  });

  test('a failed read leaves the IRIs to be asked for again', async () => {
    const read = vi.fn(async () => {
      throw new Error('down');
    });
    const b = new LabelBatch(read, () => {});
    b.want('a');
    await vi.advanceTimersByTimeAsync(30);
    expect(b.has('a')).toBe(false);
  });
});
