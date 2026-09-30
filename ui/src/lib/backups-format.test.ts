import { describe, expect, it } from 'vitest';
import {
  dedupRatio,
  defaultRestoreName,
  fmtCommitAge,
  fmtDedup,
  fmtRatio,
  fmtTimeToken,
  globMatch,
  isoWeek,
  matchDatasets,
  parseDuration,
  parsePatterns,
  presetToSchedule,
  renderNameTemplate,
  repoLocation,
  restoreLoss,
  retentionSentence,
  scheduleToPreset,
  type NameContext,
} from './backups-format';

describe('dedup ratio', () => {
  it('formats large ratios as integers and small ones with a decimal', () => {
    expect(fmtRatio(98.2)).toBe('98×');
    expect(fmtRatio(2.44)).toBe('2.4×');
    expect(fmtRatio(1)).toBe('1.0×');
    expect(fmtRatio(9.96)).toBe('10×');
    expect(fmtRatio(1234.4)).toBe('1234×');
  });

  it('shows unknown and infinite ratios', () => {
    expect(fmtRatio(null)).toBe('—');
    expect(fmtRatio(Number.NaN)).toBe('—');
    expect(fmtRatio(0)).toBe('—');
    expect(fmtRatio(Infinity)).toBe('∞');
  });

  it('divides logical by added bytes', () => {
    expect(dedupRatio(98_000, 1_000)).toBe(98);
    expect(dedupRatio(1_000, 0)).toBe(Infinity);
    expect(dedupRatio(0, 0)).toBeNull();
    expect(fmtDedup({ logicalBytes: 310 << 20, addedBytes: 3 << 20 })).toBe('103×');
    expect(fmtDedup({ logicalBytes: 310, addedBytes: 310 })).toBe('1.0×');
  });
});

describe('durations and retention', () => {
  it('parses durations into seconds and words', () => {
    expect(parseDuration('30d')).toEqual({ seconds: 30 * 86400, words: '30 days' });
    expect(parseDuration('1d 12h')).toEqual({ seconds: 36 * 3600, words: '1 day 12 hours' });
    expect(parseDuration('1d12h')?.seconds).toBe(36 * 3600);
    expect(parseDuration('2 weeks')?.words).toBe('2 weeks');
    expect(parseDuration('90m')?.words).toBe('90 minutes');
    expect(parseDuration('1w')?.words).toBe('1 week');
    expect(parseDuration('')).toBeNull();
    expect(parseDuration('30')).toBeNull();
    expect(parseDuration('30 fortnights')).toBeNull();
    expect(parseDuration('30d x')).toBeNull();
  });

  it('explains retention in one sentence', () => {
    expect(retentionSentence({ expireAfter: '30d', minCount: 7, maxCount: 60 })).toBe(
      'Keeps at least 7; deletes backups older than 30 days, and any beyond 60.',
    );
    expect(retentionSentence({ expireAfter: '30d', minCount: 1, maxCount: null })).toBe(
      'Keeps at least 1; deletes backups older than 30 days.',
    );
    expect(retentionSentence({ expireAfter: null, minCount: 3, maxCount: 10 })).toBe(
      'Keeps at least 3; deletes any beyond 10.',
    );
    expect(retentionSentence({ expireAfter: '12h', minCount: 0, maxCount: null })).toBe(
      'Deletes backups older than 12 hours.',
    );
  });

  it('covers keeping everything, and a maximum at or below the minimum', () => {
    expect(retentionSentence({ expireAfter: null, minCount: 1, maxCount: null })).toBe(
      'Keeps every backup.',
    );
    expect(retentionSentence({ expireAfter: null, minCount: 7, maxCount: 5 })).toBe(
      'Keeps the newest 7 only.',
    );
    expect(retentionSentence({ expireAfter: '30d', minCount: 7, maxCount: 5 })).toBe(
      'Keeps at least 7; deletes backups older than 30 days, and any beyond 7.',
    );
    expect(retentionSentence({ expireAfter: null, minCount: 0, maxCount: 0 })).toBe(
      'Deletes every backup.',
    );
  });

  it('passes an unparsed duration through', () => {
    expect(retentionSentence({ expireAfter: 'P30D', minCount: 1, maxCount: null })).toBe(
      'Keeps at least 1; deletes backups older than P30D.',
    );
  });
});

describe('name templates', () => {
  // 2026-09-30 14:03:11 UTC is 16:03:11 in Berlin (CEST)
  const ctx: NameContext = {
    policy: 'nightly',
    dataset: 'wiki',
    seq: 42,
    run: '7f3a9c21-5b6e-4d0a-9f1e-0c2b3a4d5e6f',
    time: new Date('2026-09-30T14:03:11Z'),
    timezone: 'Europe/Berlin',
  };

  it('renders the default template with the UTC time token', () => {
    expect(renderNameTemplate('{policy}-{dataset}-{time}', ctx)).toEqual({
      name: 'nightly-wiki-20260930t140311z',
    });
    expect(fmtTimeToken(new Date('2027-01-02T03:04:05Z'))).toBe('20270102t030405z');
  });

  it('renders seq, run and dates in the policy time zone', () => {
    expect(renderNameTemplate('{dataset}-{seq}-{run}', ctx).name).toBe('wiki-42-7f3a9c21');
    expect(renderNameTemplate('{policy}-{dataset}-{date:%Y%m%d-%H%M%S}', ctx).name).toBe(
      'nightly-wiki-20260930-160311',
    );
    expect(renderNameTemplate('{dataset}.{date:%Y}-w{date:%V}.d{date:%j}', ctx).name).toBe(
      'wiki.2026-w40.d273',
    );
    // after midnight in Tokyo it is already the next day
    const tokyo = { ...ctx, time: new Date('2026-09-30T16:00:00Z'), timezone: 'Asia/Tokyo' };
    expect(renderNameTemplate('{date:%Y-%m-%d}', tokyo).name).toBe('2026-10-01');
  });

  it('reports template errors', () => {
    expect(renderNameTemplate('{policy}-{nope}', ctx).error).toBe('unknown placeholder {nope}');
    expect(renderNameTemplate('{policy', ctx).error).toContain('not closed');
    expect(renderNameTemplate('{date:%A}', ctx).error).toContain('“%A” is not supported');
    expect(renderNameTemplate('{date:%}', ctx).error).toContain('ends with “%”');
    expect(renderNameTemplate('{date:%Y}', { ...ctx, timezone: 'Mars/Olympus' }).error).toBe(
      'unknown time zone “Mars/Olympus”',
    );
  });

  it('checks the result against the backup-name grammar', () => {
    expect(renderNameTemplate('-{dataset}', ctx).error).toContain('not a valid backup name');
    expect(renderNameTemplate('{dataset} {seq}', ctx).error).toContain('not a valid backup name');
    expect(renderNameTemplate('x'.repeat(65), ctx).error).toContain('not a valid backup name');
    expect(renderNameTemplate('x'.repeat(64), ctx).name).toHaveLength(64);
  });

  it('numbers ISO weeks across year boundaries', () => {
    expect(isoWeek(2026, 1, 1)).toBe(1); // a Thursday
    expect(isoWeek(2027, 1, 1)).toBe(53); // a Friday, still in 2026's last week
    expect(isoWeek(2024, 12, 30)).toBe(1); // a Monday, the first week of 2025
    expect(isoWeek(2026, 9, 30)).toBe(40);
  });
});

describe('dataset globs', () => {
  it('matches like the server’s grants', () => {
    expect(globMatch('*', 'anything')).toBe(true);
    expect(globMatch('*', '')).toBe(true);
    expect(globMatch('wiki', 'wiki')).toBe(true);
    expect(globMatch('wiki', 'wiki2')).toBe(false);
    expect(globMatch('wiki-*', 'wiki-')).toBe(true);
    expect(globMatch('wiki-*', 'wiki-sandbox')).toBe(true);
    expect(globMatch('wiki-*', 'wiki')).toBe(false);
    expect(globMatch('team-*-prod', 'team-a-prod')).toBe(true);
    expect(globMatch('team-*-prod', 'team--prod')).toBe(true);
    expect(globMatch('team-*-prod', 'team-a-dev')).toBe(false);
    expect(globMatch('*a*b*', 'xxaxxbxx')).toBe(true);
    expect(globMatch('*a*b*', 'xxbxxaxx')).toBe(false);
    expect(globMatch('Wiki', 'wiki')).toBe(false);
    expect(globMatch('a**', 'abc')).toBe(true);
    // `?` and brackets are literal
    expect(globMatch('wiki?', 'wikis')).toBe(false);
    expect(globMatch('wiki?', 'wiki?')).toBe(true);
  });

  it('lists the datasets a pattern list selects', () => {
    const names = ['foaf', 'wiki', 'wiki-staging', 'scratch'];
    expect(matchDatasets(['wiki*'], names)).toEqual(['wiki', 'wiki-staging']);
    expect(matchDatasets(['foaf', '*-staging'], names)).toEqual(['foaf', 'wiki-staging']);
    expect(matchDatasets([], names)).toEqual([]);
    expect(parsePatterns(' wiki-*, foaf  team-*,')).toEqual(['wiki-*', 'foaf', 'team-*']);
  });
});

describe('schedule presets', () => {
  it('turns presets into cron and back', () => {
    const cases = [
      { kind: 'hourly', minute: 15 },
      { kind: 'daily', hour: 2, minute: 30 },
      { kind: 'weekly', day: 0, hour: 23, minute: 5 },
    ] as const;
    for (const p of cases) expect(scheduleToPreset(presetToSchedule(p))).toEqual(p);
    expect(presetToSchedule({ kind: 'daily', hour: 2, minute: 30 })).toBe('30 2 * * *');
    expect(presetToSchedule({ kind: 'custom', expr: ' every 6h ' })).toBe('every 6h');
  });

  it('keeps anything else custom', () => {
    expect(scheduleToPreset('30 2 * * 7')).toEqual({ kind: 'weekly', day: 0, hour: 2, minute: 30 });
    expect(scheduleToPreset('*/15 * * * *').kind).toBe('custom');
    expect(scheduleToPreset('30 2 1 * *').kind).toBe('custom');
    expect(scheduleToPreset('0 30 2 * * *').kind).toBe('custom');
    expect(scheduleToPreset('30 2 * * 1-5').kind).toBe('custom');
    expect(scheduleToPreset('every 6h')).toEqual({ kind: 'custom', expr: 'every 6h' });
  });
});

describe('restore', () => {
  it('says which commits replacing a dataset loses', () => {
    expect(restoreLoss(57, 41, true)).toEqual({ lost: 16, text: 'Commits 42–57 will be lost' });
    expect(restoreLoss(42, 41, true)?.text).toBe('Commit 42 will be lost');
    expect(restoreLoss(41, 41, true)).toEqual({ lost: 0, text: 'No commits will be lost' });
    expect(restoreLoss(57, 41, false)?.text).toContain('different lineage');
    expect(restoreLoss(undefined, 41, true)).toBeNull();
  });

  it('proposes a free dataset name', () => {
    const now = new Date(2026, 8, 30, 12);
    expect(defaultRestoreName('wiki', [], now)).toBe('wiki-restored-20260930');
    expect(defaultRestoreName('wiki', ['wiki-restored-20260930'], now)).toBe(
      'wiki-restored-20260930-2',
    );
    expect(defaultRestoreName('x'.repeat(80), [], now).length).toBeLessThanOrEqual(64);
  });
});

describe('labels', () => {
  it('shows repository locations', () => {
    expect(repoLocation({ type: 'fs', path: '/srv/backups' })).toBe('/srv/backups');
    expect(repoLocation({ type: 's3', bucket: 'kg', prefix: '/prod/sparkles/' })).toBe(
      's3://kg/prod/sparkles',
    );
    expect(repoLocation({ type: 's3', bucket: 'kg' })).toBe('s3://kg');
    expect(repoLocation({ type: 'gcs', bucket: 'kg', prefix: 'x' })).toBe('gs://kg/x');
  });

  it('shows a commit with its age', () => {
    const now = Date.parse('2026-09-30T17:00:00Z');
    expect(fmtCommitAge({ seq: 42, timestamp: '2026-09-30T14:00:00Z' }, now)).toBe('#42 · 3h ago');
  });
});
