import { EventEmitter } from 'node:events';
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { captureFiniteReads } from './finite-reads.mjs';

// Execute the real harness orchestration with deterministic browser/network events.
// Fixture setup/API work preceding browser launch is deliberately outside this seam.
async function run(
  mode: 'primary' | 'ancillary' | 'success',
  cleanupFails = false,
  outputExists = false,
) {
  const source = readFileSync(new URL('./perf-individuals.mjs', import.meta.url), 'utf8');
  const body = source.slice(source.indexOf('const browser = await chromium.launch'));
  const primary = new Error('First member absent after failed primary read');
  const cleanup = new Error('Context cleanup failed');
  const collision = Object.assign(new Error('Existing raw output'), { code: 'EEXIST' });
  let capture: ReturnType<typeof captureFiniteReads>;
  let contextClosed = false;
  let browserClosed = false;
  let serverClosed = false;
  const records: any[] = [];
  const page = new EventEmitter() as any;
  const request = (path: string, failed: boolean) => ({
    resourceType: () => 'fetch',
    headers: () => ({ accept: 'application/json' }),
    url: () => `http://localhost/$/ontology/example/${path}`,
    method: () => 'GET',
    postData: () => null,
    failure: () => (failed ? { errorText: 'net::ERR_ABORTED' } : null),
    response: async () =>
      failed ? null : { status: () => 200, body: async () => Buffer.from('okay') },
    sizes: async () => {
      if (failed) throw new Error('Unavailable sizes');
      return {
        requestBodySize: 0,
        requestHeadersSize: 20,
        responseBodySize: 4,
        responseHeadersSize: 10,
      };
    },
  });
  page.goto = async (url: string) => {
    const reads: [string, boolean][] = url.includes(':45231/')
      ? [
          ['individuals', false],
          ['individuals?type=People', false],
        ]
      : [
          ['entities?kind=NamedIndividual', mode === 'primary'],
          ['axioms?kind=ClassAssertion', false],
          ['metrics', mode === 'ancillary'],
        ];
    for (const [path, failed] of reads) {
      const r = request(path, failed);
      page.emit('request', r);
      page.emit(failed ? 'requestfailed' : 'requestfinished', r);
    }
  };
  const locator = { locator: true, click: async () => {}, count: async () => 20 };
  page.getByRole = () => ({ getByRole: () => locator });
  const check = Object.assign(
    (value: any) =>
      value?.locator
        ? {
            toBeVisible: async () => {
              if (mode === 'primary') throw primary;
            },
          }
        : expect(value),
    {
      poll: (fn: () => unknown) => ({
        toBe: async (value: unknown) => {
          await capture.settle();
          expect(fn()).toBe(value);
        },
      }),
    },
  );
  const browser = {
    version: () => 'orchestration-fixture',
    newContext: async () => {
      // Each real context has its own page and request listeners.
      page.removeAllListeners();
      return {
        newPage: async () => page,
        close: async () => {
          contextClosed = true;
          if (cleanupFails) throw cleanup;
        },
      };
    },
    close: async () => {
      browserClosed = true;
    },
  };
  const execute = new (Object.getPrototypeOf(async function () {}).constructor)(
    'chromium',
    'servers',
    'result',
    'repeats',
    'captureFiniteReads',
    'performance',
    'expect',
    'ds',
    'writeFileSync',
    'output',
    'count',
    'console',
    body,
  );
  let error: any;
  try {
    await execute(
      { launch: async () => browser },
      [
        {
          close: (done: () => void) => {
            serverClosed = true;
            done();
          },
        },
      ],
      { browser: [] },
      1,
      (...args: Parameters<typeof captureFiniteReads>) => (capture = captureFiniteReads(...args)),
      performance,
      check,
      'example',
      (_path: string, data: string, options: unknown) => {
        expect(options).toEqual({ flag: 'wx' });
        if (outputExists) throw collision;
        records.push(JSON.parse(data));
      },
      'fixture.json',
      200,
      { log() {}, error() {} },
    );
  } catch (e) {
    error = e;
  }
  return {
    records,
    error,
    primary,
    cleanup,
    collision,
    contextClosed,
    browserClosed,
    serverClosed,
  };
}

describe('Individuals measurement failure persistence', () => {
  it('certifies complete runs only after both trials and cleanup finish', async () => {
    const r = await run('success');
    expect(r.error).toBeUndefined();
    expect(r.records).toHaveLength(1);
    expect(r.contextClosed && r.browserClosed && r.serverClosed).toBe(true);
    expect(r.records[0]).toMatchObject({
      complete: true,
      error: null,
      cleanupErrors: [],
      correctness: expect.any(String),
    });
    expect(r.records[0].browser).toHaveLength(2);
  });

  it('persists a failed primary read and partial milestones before preserving the original failure', async () => {
    const r = await run('primary');
    expect(r.records).toHaveLength(1);
    expect(r.error).toBe(r.primary);
    expect(r.contextClosed && r.browserClosed && r.serverClosed).toBe(true);
    expect(r.records[0]).toMatchObject({ complete: false, correctness: null });
    expect(r.records[0].browser[0]).toMatchObject({
      complete: false,
      firstBucketsMs: null,
      firstMemberMs: null,
      rows: null,
      error: { message: r.primary.message },
      failedCount: 1,
    });
    expect(r.records[0].browser[0].requests.find((r: any) => r.outcome === 'failed')).toMatchObject(
      {
        failure: 'net::ERR_ABORTED',
        decodedBytes: null,
        responseBodySize: null,
      },
    );
  });

  it('persists successful UI milestones with a failed ancillary read before rejecting', async () => {
    const r = await run('ancillary');
    expect(r.records).toHaveLength(1);
    expect(r.error.message).toMatch(/Incomplete finite-read/);
    expect(r.records[0]).toMatchObject({ complete: false, correctness: null });
    expect(r.records[0].browser[0]).toMatchObject({
      complete: false,
      failedCount: 1,
      rows: 20,
      firstMemberMs: expect.any(Number),
    });
  });

  it('retains cleanup failures without replacing the original primary error', async () => {
    const r = await run('primary', true);
    expect(r.records).toHaveLength(1);
    expect(r.error).toBe(r.primary);
    expect(r.browserClosed && r.serverClosed).toBe(true);
    expect(r.records[0].browser[0].cleanupErrors).toEqual([
      expect.objectContaining({ message: r.cleanup.message }),
    ]);
  });

  it('uses exclusive output and preserves the original error when persistence also fails', async () => {
    const r = await run('primary', false, true);
    expect(r.records).toHaveLength(0);
    expect(r.error).toBe(r.primary);
    expect(r.error.persistenceError).toBe(r.collision);
    expect(r.browserClosed && r.serverClosed).toBe(true);
  });
});
