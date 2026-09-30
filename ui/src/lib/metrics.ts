// Derived views of the server's metrics snapshot (`GET /$/metrics?format=json`).
import type { MetricsSnapshot, Outcome, RequestSeries } from './api';

/**
 * The `q` quantile (0..1) of a histogram given as cumulative bucket counts (the last
 * one is `+Inf`), interpolating linearly inside the bucket that holds the rank, like
 * Prometheus' `histogram_quantile`. Observations above the highest finite bound are
 * reported at that bound. `null` when there are no observations.
 */
export function histogramQuantile(
  q: number,
  bounds: number[],
  cumulative: number[],
): number | null {
  const total = cumulative[cumulative.length - 1] ?? 0;
  if (!total || !bounds.length) return null;
  const rank = Math.min(Math.max(q, 0), 1) * total;
  const i = cumulative.findIndex((c) => c >= rank && c > 0);
  if (i < 0 || i >= bounds.length) return bounds[bounds.length - 1];
  const below = i === 0 ? 0 : cumulative[i - 1];
  const lo = i === 0 ? 0 : bounds[i - 1];
  const inBucket = cumulative[i] - below;
  if (inBucket <= 0) return bounds[i];
  return lo + (bounds[i] - lo) * ((rank - below) / inBucket);
}

/** Cumulative counts observed between two snapshots of the same histogram (a counter
 * that went down was reset, e.g. a recreated dataset: the newer counts stand alone). */
export function bucketDelta(now: number[], before: number[] | undefined): number[] {
  if (!before || before.length !== now.length || now.some((n, i) => n < before[i])) return now;
  return now.map((n, i) => n - before[i]);
}

export type RequestRow = {
  key: string;
  dataset: string;
  operation: RequestSeries['operation'];
  count: number;
  /** Requests per second between the two snapshots; `null` for the first one. */
  rate: number | null;
  /** Share of requests (since start) that did not end `ok`, in percent. */
  failedPct: number;
  outcomes: Record<Outcome, number>;
  /** Latency percentiles in seconds. */
  p50: number | null;
  p95: number | null;
  /** Whether the percentiles cover only the last polling interval (else: since start). */
  windowed: boolean;
};

/** One row per (dataset, operation) with traffic. `prev` is the previous snapshot and
 * `seconds` the time between the two. */
export function requestRows(
  cur: MetricsSnapshot,
  prev: MetricsSnapshot | null,
  seconds: number,
): RequestRow[] {
  const key = (r: RequestSeries) => `${r.dataset}\u0000${r.operation}`;
  const before = new Map((prev?.requests ?? []).map((r) => [key(r), r]));
  return cur.requests
    .filter((r) => r.count > 0)
    .map((r) => {
      const old = before.get(key(r));
      const window = bucketDelta(r.buckets, old?.buckets);
      const recent = window[window.length - 1] > 0 && old !== undefined;
      const hist = recent ? window : r.buckets;
      const delta = old && r.count >= old.count ? r.count - old.count : r.count;
      return {
        key: key(r),
        dataset: r.dataset,
        operation: r.operation,
        count: r.count,
        rate: prev && seconds > 0 ? delta / seconds : null,
        failedPct: r.count ? (100 * (r.count - (r.outcomes.ok ?? 0))) / r.count : 0,
        outcomes: r.outcomes,
        p50: histogramQuantile(0.5, cur.bucketBounds, hist),
        p95: histogramQuantile(0.95, cur.bucketBounds, hist),
        windowed: recent,
      };
    })
    .sort((a, b) => b.count - a.count || a.key.localeCompare(b.key));
}

/** Hits as a share of lookups, in percent; `null` before the first lookup. */
export function hitRatio(hits: number, misses: number): number | null {
  const n = hits + misses;
  return n ? (100 * hits) / n : null;
}

/** Seconds as a compact latency (`850 µs`, `12.5 ms`, `3.2 s`). */
export function fmtSeconds(s: number | null): string {
  if (s == null || Number.isNaN(s)) return '—';
  if (s < 0.001) return `${Math.round(s * 1e6)} µs`;
  if (s < 1) return `${(s * 1000).toFixed(s < 0.01 ? 2 : 1)} ms`;
  return `${s.toFixed(s < 10 ? 2 : 1)} s`;
}
