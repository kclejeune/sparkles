import { EventEmitter } from 'node:events';
import type { Page, Request } from '@playwright/test';
import { describe, expect, it } from 'vitest';
import { captureFiniteReads } from './finite-reads.mjs';

function request(
  path: string,
  options: { failed?: boolean; status?: number; feed?: string; noSizes?: boolean } = {},
) {
  return {
    resourceType: () => (options.feed === 'native' ? 'eventsource' : 'fetch'),
    headers: () => ({
      accept: options.feed === 'fetch' ? 'text/event-stream' : 'application/json',
    }),
    url: () => 'http://localhost/$/ontology/example/' + path + '?at=pinned',
    method: () => 'GET',
    postData: () => null,
    failure: () => (options.failed ? { errorText: 'net::ERR_ABORTED' } : null),
    response: async () =>
      options.failed
        ? null
        : { status: () => options.status ?? 200, body: async () => Buffer.from('okay') },
    sizes: async () => {
      if (options.failed || options.noSizes) throw new Error('Size unavailable');
      return {
        requestBodySize: 0,
        requestHeadersSize: 20,
        responseBodySize: 4,
        responseHeadersSize: 10,
      };
    },
  } as unknown as Request;
}

describe('finite browser read accounting', () => {
  it('certifies successful finite transfers after their metadata settles', async () => {
    const page = new EventEmitter();
    const capture = captureFiniteReads(page as unknown as Page, 'example');
    const r = request('individuals');
    page.emit('request', r);
    expect(capture.summary()).toMatchObject({
      complete: false,
      pendingCount: 1,
      decodedBytes: null,
    });
    page.emit('requestfinished', r);
    await capture.settle();
    expect(capture.summary()).toMatchObject({
      complete: true,
      failedCount: 0,
      httpErrorCount: 0,
      metadataErrorCount: 0,
      pendingCount: 0,
      decodedBytes: 4,
      requestBytes: 20,
      responseWireBytes: 14,
    });
  });

  it('retains available response metadata when a finite transport fails', async () => {
    const page = new EventEmitter();
    const capture = captureFiniteReads(page as unknown as Page, 'example');
    const r = Object.assign(request('metrics', { failed: true }), {
      response: async () => ({
        status: () => 200,
        body: async () => {
          throw new Error('Partial body');
        },
      }),
      sizes: async () => ({
        requestBodySize: 0,
        requestHeadersSize: 20,
        responseBodySize: 2,
        responseHeadersSize: 10,
      }),
    });
    page.emit('request', r);
    page.emit('requestfailed', r);
    await capture.settle();
    expect(capture.requests[0]).toMatchObject({
      outcome: 'failed',
      status: 200,
      decodedBytes: null,
      responseBodySize: 2,
    });
    expect(capture.summary()).toMatchObject({
      complete: false,
      failedCount: 1,
      decodedBytes: null,
      responseWireBytes: 12,
    });
  });

  it('keeps successes and transport failures, excludes both feed kinds, and refuses incomplete totals', async () => {
    const page = new EventEmitter();
    const capture = captureFiniteReads(page as unknown as Page, 'example');
    const success = request('individuals');
    const failed = request('metrics', { failed: true });
    const nativeFeed = request('changes', { feed: 'native' });
    const fetchFeed = request('changes', { feed: 'fetch' });
    for (const r of [success, failed, nativeFeed, fetchFeed]) page.emit('request', r);
    page.emit('requestfinished', success);
    page.emit('requestfailed', failed);
    page.emit('requestfinished', nativeFeed);
    page.emit('requestfailed', fetchFeed);
    page.emit('requestfailed', nativeFeed);
    page.emit('requestfinished', fetchFeed);
    await capture.settle();
    expect(capture.requests).toHaveLength(2);
    expect(capture.pending.size).toBe(0);
    const rows = JSON.parse(JSON.stringify(capture.requests));
    expect(rows.find((r: { path: string }) => r.path.endsWith('/metrics'))).toMatchObject({
      method: 'GET',
      query: '?at=pinned',
      body: null,
      outcome: 'failed',
      failure: 'net::ERR_ABORTED',
      status: null,
      decodedBytes: null,
      responseBodySize: null,
    });
    expect(capture.summary()).toMatchObject({
      complete: false,
      failedCount: 1,
      pendingCount: 0,
      decodedBytes: null,
      responseWireBytes: null,
      observedBytes: { decodedBytes: 4, responseWireBytes: 14 },
    });
  });

  it('records HTTP errors once and preserves available bytes', async () => {
    const page = new EventEmitter();
    const capture = captureFiniteReads(page as unknown as Page, 'example');
    const r = request('individuals', { status: 503 });
    page.emit('request', r);
    page.emit('requestfinished', r);
    page.emit('requestfinished', r);
    page.emit('requestfailed', r);
    await capture.settle();
    expect(capture.requests).toHaveLength(1);
    expect(capture.requests[0]).toMatchObject({
      outcome: 'finished',
      status: 503,
      decodedBytes: 4,
    });
    expect(capture.summary()).toMatchObject({
      complete: false,
      failedCount: 0,
      httpErrorCount: 1,
      decodedBytes: 4,
    });
  });

  it('does not turn unavailable successful-response sizes into zero', async () => {
    const page = new EventEmitter();
    const capture = captureFiniteReads(page as unknown as Page, 'example');
    const r = request('individuals', { noSizes: true });
    page.emit('request', r);
    page.emit('requestfinished', r);
    await capture.settle();
    expect(capture.summary()).toMatchObject({
      complete: false,
      decodedBytes: 4,
      requestBytes: null,
      responseWireBytes: null,
      metadataErrorCount: 1,
    });
    expect(capture.requests[0]).toMatchObject({
      status: 200,
      decodedBytes: 4,
      responseBodySize: null,
    });
  });
});
