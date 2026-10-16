import { beforeEach, describe, expect, it, vi } from 'vitest';

const request = vi.fn();
vi.mock('./api', () => ({
  ApiError: class ApiError extends Error {},
  request: (...args: unknown[]) => request(...args),
}));

import { followCommits } from './changes-feed';

/** A response whose body delivers the given server-sent events. */
function stream(seqs: number[]) {
  const text = seqs
    .map((s) => `event: commit\nid: ${s}\ndata: ${JSON.stringify({ commit: { seq: s } })}\n\n`)
    .join('');
  return { body: new Response(text).body };
}

beforeEach(() => request.mockReset());

describe('followCommits', () => {
  it('asks for the commits after the one the view has read', async () => {
    request.mockResolvedValueOnce(stream([]));
    await followCommits('ds', 7, () => {}, new AbortController().signal);
    const url = String(request.mock.calls[0][0]);
    expect(new URL(url, 'http://x').searchParams.get('after')).toBe('commit:7');
  });

  it('returns the last commit it delivered, which the next connection resumes from', async () => {
    request.mockResolvedValueOnce(stream([8, 9]));
    request.mockResolvedValueOnce(stream([10]));
    const seen: number[] = [];
    const signal = new AbortController().signal;
    const last = await followCommits('ds', 7, (c) => seen.push(c.seq), signal);
    expect(last).toBe(9);
    const next = await followCommits('ds', last, (c) => seen.push(c.seq), signal);
    expect(next).toBe(10);
    expect(seen).toEqual([8, 9, 10]);
    expect(new URL(String(request.mock.calls[1][0]), 'http://x').searchParams.get('after')).toBe(
      'commit:9',
    );
  });

  it('keeps its starting point when the stream ends without a commit', async () => {
    request.mockResolvedValueOnce(stream([]));
    expect(await followCommits('ds', 12, () => {}, new AbortController().signal)).toBe(12);
  });
});
