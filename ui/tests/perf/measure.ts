// Shared measuring code of the scale measurements (ROADMAP item 725): the network recorder,
// long tasks, heap, a frame sampler and the JSON files of a run.

import type { CDPSession, Page, TestInfo } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { hostname, tmpdir } from 'node:os';
import { join } from 'node:path';

export const BASE = (process.env.PERF_URL ?? '').replace(/\/$/, '');
export const OUT = process.env.PERF_OUT ?? join(tmpdir(), 'sparkles-perf');
export const LABEL = process.env.PERF_LABEL ?? 'run';
export const REPEAT = Number(process.env.PERF_REPEAT ?? 3);
/** Waits longer than this fail a repetition, and the failure is recorded. */
export const LOAD_TIMEOUT = Number(process.env.PERF_LOAD_TIMEOUT_MS ?? 300_000);

export function need() {
  if (!BASE) throw new Error('PERF_URL names the server of scripts/perf-seed.mjs');
}

export const median = (xs: number[]) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length
    ? s.length % 2
      ? s[(s.length - 1) / 2]
      : (s[s.length / 2 - 1] + s[s.length / 2]) / 2
    : NaN;
};
export const pct = (xs: number[], p: number) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length ? s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))] : NaN;
};

/** What a request is for: the static UI, or an API route with the dataset name taken out. */
export function groupOf(url: string): string {
  const u = new URL(url);
  const p = u.pathname;
  if (p.startsWith('/ui/') || p === '/ui') return 'static';
  let m = p.match(/^\/\$\/ontology\/[^/]+\/([^/]+)/);
  if (m) return `ontology/${m[1]}`;
  m = p.match(/^\/\$\/([^/]+)/);
  if (m && !/^\/\$\/ontology\/[^/]+$/.test(p)) return `$/${m[1]}`;
  if (/^\/\$\/ontology\/[^/]+$/.test(p)) return 'ontology/summary';
  m = p.match(/^\/[^/]+\/(sparql|query|update|changes)$/);
  if (m) return m[1];
  return p.split('/').slice(0, 3).join('/');
}

/** Background polling of the page, which neither counts as load nor keeps the network busy. */
const POLLING = new Set(['$/ping', '$/datasets', 'changes', '$/auth', '$/whoami']);

export type NetGroup = { count: number; encodedBytes: number; decodedBytes: number; ms: number };

/** Records the requests of a page through the DevTools protocol. */
export class Net {
  private reqs = new Map<
    string,
    {
      url: string;
      group: string;
      poll: boolean;
      start: number;
      end?: number;
      enc: number;
      dec: number;
      status?: number;
    }
  >();
  inflight = 0;
  lastActivity = 0;
  private t0 = 0;
  constructor(private cdp: CDPSession) {}
  async start() {
    await this.cdp.send('Network.enable');
    this.t0 = Date.now();
    this.lastActivity = Date.now();
    this.cdp.on('Network.requestWillBeSent', (e) => {
      this.inflight++;
      this.lastActivity = Date.now();
      this.reqs.set(e.requestId, {
        url: e.request.url,
        group: groupOf(e.request.url),
        start: e.timestamp,
        enc: 0,
        dec: 0,
      });
    });
    this.cdp.on('Network.responseReceived', (e) => {
      const r = this.reqs.get(e.requestId);
      if (r) r.status = e.response.status;
    });
    this.cdp.on('Network.dataReceived', (e) => {
      const r = this.reqs.get(e.requestId);
      if (r) r.dec += e.dataLength;
      if (r && !r.poll) this.lastActivity = Date.now();
    });
    const done = (e: { requestId: string; timestamp: number; encodedDataLength?: number }) => {
      const r = this.reqs.get(e.requestId);
      if (r && r.end === undefined) {
        r.end = e.timestamp;
        r.enc = e.encodedDataLength ?? 0;
        if (!r.poll) {
          this.inflight--;
          this.lastActivity = Date.now();
        }
      }
    };
    this.cdp.on('Network.loadingFinished', done);
    this.cdp.on('Network.loadingFailed', done);
  }
  /** Resolves once no request has been in flight or finished for `quietMs`. */
  async quiet(quietMs = 700, timeout = LOAD_TIMEOUT) {
    const t = Date.now();
    while (Date.now() - t < timeout) {
      if (this.inflight <= 0 && Date.now() - this.lastActivity >= quietMs) return true;
      await new Promise((r) => setTimeout(r, 50));
    }
    return false;
  }
  summary(): {
    groups: Record<string, NetGroup>;
    api: NetGroup;
    total: NetGroup;
    requests: {
      url: string;
      group: string;
      status?: number;
      ms: number;
      encodedBytes: number;
      decodedBytes: number;
    }[];
  } {
    const groups: Record<string, NetGroup> = {};
    const blank = (): NetGroup => ({ count: 0, encodedBytes: 0, decodedBytes: 0, ms: 0 });
    const api = blank();
    const total = blank();
    const requests = [];
    for (const r of this.reqs.values()) {
      const g = (groups[r.group] ??= blank());
      const ms = r.end !== undefined ? (r.end - r.start) * 1000 : 0;
      for (const t of r.group === 'static' ? [g, total] : r.poll ? [g, total] : [g, total, api]) {
        t.count++;
        t.encodedBytes += r.enc;
        t.decodedBytes += r.dec;
        t.ms += ms;
      }
      requests.push({
        url: r.url.replace(BASE, ''),
        group: r.group,
        status: r.status,
        ms: Math.round(ms),
        encodedBytes: r.enc,
        decodedBytes: r.dec,
      });
    }
    return { groups, api, total, requests };
  }
}

export const LONGTASK_INIT = `
  window.__long = [];
  try {
    new PerformanceObserver((l) => { for (const e of l.getEntries()) window.__long.push([e.startTime, e.duration]); })
      .observe({ type: 'longtask', buffered: true });
  } catch {}
`;

export async function longTasks(page: Page) {
  const l: [number, number][] = await page.evaluate('window.__long || []');
  return {
    count: l.length,
    totalMs: Math.round(l.reduce((a, [, d]) => a + d, 0)),
    maxMs: Math.round(Math.max(0, ...l.map(([, d]) => d))),
    lastEndMs: Math.round(Math.max(0, ...l.map(([s, d]) => s + d))),
  };
}

/** Collects garbage and reads the JS heap and the DOM nodes. */
export async function heap(cdp: CDPSession) {
  await cdp.send('HeapProfiler.enable');
  await cdp.send('HeapProfiler.collectGarbage');
  await cdp.send('HeapProfiler.collectGarbage');
  await cdp.send('Performance.enable');
  const { metrics } = await cdp.send('Performance.getMetrics');
  const m = Object.fromEntries(metrics.map((x) => [x.name, x.value]));
  return {
    jsHeapUsedMB: +(m.JSHeapUsedSize / 1048576).toFixed(1),
    jsHeapTotalMB: +(m.JSHeapTotalSize / 1048576).toFixed(1),
    domNodes: m.Nodes,
    layoutCount: m.LayoutCount,
  };
}

/** Frame intervals from `requestAnimationFrame` while `driver` runs in the page. */
export const FRAME_STATS_JS = (driver: string, ms: number) => `(async () => {
  const dts = [];
  let last = performance.now();
  const start = last;
  let i = 0;
  const drive = ${driver};
  await new Promise((done) => {
    const tick = (t) => {
      dts.push(t - last);
      last = t;
      const stop = drive(i++, t - start);
      if (stop || t - start > ${ms}) done();
      else requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
  return dts.slice(1);
})()`;

export function frameSummary(dts: number[]) {
  return {
    frames: dts.length,
    p50: +median(dts).toFixed(1),
    p95: +pct(dts, 95).toFixed(1),
    p99: +pct(dts, 99).toFixed(1),
    max: +Math.max(0, ...dts).toFixed(1),
    over50ms: dts.filter((d) => d > 50).length,
    meanFps: +(1000 / (dts.reduce((a, b) => a + b, 0) / Math.max(1, dts.length))).toFixed(1),
  };
}

let metaCache: Record<string, unknown> | undefined;
export async function meta(page: Page) {
  metaCache ??= {
    label: LABEL,
    base: BASE,
    host: hostname(),
    date: new Date().toISOString(),
    uiDir: process.env.PERF_UI_DIR ?? process.env.SPARKLES_UI_DIR ?? '(embedded)',
    commit:
      process.env.PERF_COMMIT ??
      safe(() => execFileSync('git', ['rev-parse', '--short', 'HEAD']).toString().trim()),
    server: process.env.PERF_SERVER ?? '',
    note: process.env.PERF_NOTE ?? '',
  };
  const gl = await page
    .evaluate(() => {
      const c = document.createElement('canvas');
      const g = c.getContext('webgl2') || c.getContext('webgl');
      if (!g) return 'none';
      const x = g.getExtension('WEBGL_debug_renderer_info');
      return x ? String(g.getParameter(x.UNMASKED_RENDERER_WEBGL)) : 'unknown';
    })
    .catch(() => 'unknown');
  return {
    ...metaCache,
    browser: page.context().browser()?.version(),
    viewport: page.viewportSize(),
    webgl: gl,
  };
}
const safe = (f: () => string) => {
  try {
    return f();
  } catch {
    return '';
  }
};

export async function save(
  page: Page,
  info: TestInfo,
  name: string,
  rep: number,
  data: Record<string, unknown>,
) {
  const dir = join(OUT, LABEL);
  mkdirSync(dir, { recursive: true });
  const file = join(dir, `${name}.rep${rep}.json`);
  writeFileSync(file, JSON.stringify({ name, rep, meta: await meta(page), ...data }, null, 1));
  info.annotations.push({ type: 'result', description: file });
  return file;
}
