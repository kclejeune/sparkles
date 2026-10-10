import { afterEach, describe, expect, it, vi } from 'vitest';
import { badgeCount, reviewLabel } from './maintenance';
import { reviewCounts } from './review-count.svelte';

const NOW = Date.parse('2026-10-10T12:00:00Z');

describe('the review count badge', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('says how many items wait and since when', () => {
    expect(reviewLabel(1)).toBe('1 item waits for review');
    expect(reviewLabel(3, '2026-10-08T12:00:00Z', NOW)).toBe(
      '3 items wait for review, the oldest 2d ago',
    );
    expect(badgeCount(7)).toBe('7');
    expect(badgeCount(120)).toBe('99+');
  });

  it('reads the review member of the maintenance answer once per dataset at a time', async () => {
    const urls: string[] = [];
    vi.stubGlobal('fetch', async (url: string) => {
      urls.push(url);
      const body = url.includes('/org/')
        ? {
            dataset: 'org',
            consolidation: { settings: null },
            retention: { settings: null },
            review: {
              open: 3,
              oldest: '2026-10-08T12:00:00Z',
              kinds: { session: { open: 2 }, consolidation: { open: 1 } },
              branches: [],
              truncated: false,
              updated: '2026-10-10T12:00:00Z',
            },
          }
        : { dataset: 'plain', consolidation: { settings: null }, retention: { settings: null } };
      return new Response(JSON.stringify(body), { status: 200 });
    });
    await Promise.all([reviewCounts.refresh('org'), reviewCounts.refresh('org')]);
    expect(urls).toEqual(['/$/memory/org/maintenance']);
    expect(reviewCounts.of('org')?.open).toBe(3);
    // no member: no badge
    await reviewCounts.refresh('plain');
    expect(reviewCounts.of('plain')).toBeNull();
    // an error: no badge either
    vi.stubGlobal('fetch', async () => new Response('{"error":"no"}', { status: 404 }));
    await reviewCounts.refresh('org');
    expect(reviewCounts.of('org')).toBeNull();
    expect(reviewCounts.of(null)).toBeNull();
  });
});
