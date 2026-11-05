// Account only finite dataset reads. A terminal event never erases an observed read,
// and unavailable byte counts never certify a zero-byte transfer.
/** @typedef {import('@playwright/test').Request} Request */
/** @typedef {{path: string, query: string, method: string, body: string | null,
 * outcome: 'pending' | 'finished' | 'failed', failure: string | null,
 * status: number | null, decodedBytes: number | null, requestBodySize: number | null,
 * requestHeadersSize: number | null, responseBodySize: number | null,
 * responseHeadersSize: number | null, metadataErrors: string[]}} Read */

/** @param {import('@playwright/test').Page} page @param {string} ds */
export function captureFiniteReads(page, ds) {
  /** @type {Read[]} */
  const requests = [];
  /** @type {Map<Request, Read>} */
  const rows = new Map();
  /** @type {Set<Request>} */
  const pending = new Set();
  /** @type {Promise<void>[]} */
  const jobs = [];
  /** @param {Request} r */
  const tracks = (r) =>
    r.resourceType() !== 'eventsource' &&
    !(r.headers().accept ?? '').includes('text/event-stream') &&
    (new URL(r.url()).pathname.startsWith('/$/ontology/' + ds) ||
      new URL(r.url()).pathname.startsWith('/' + ds + '/'));
  /** @param {Request} r */
  function observe(r) {
    if (!tracks(r)) return null;
    let row = rows.get(r);
    if (!row) {
      const url = new URL(r.url());
      row = {
        path: url.pathname,
        query: url.search,
        method: r.method(),
        body: r.postData(),
        outcome: 'pending',
        failure: null,
        status: null,
        decodedBytes: null,
        requestBodySize: null,
        requestHeadersSize: null,
        responseBodySize: null,
        responseHeadersSize: null,
        metadataErrors: [],
      };
      rows.set(r, row);
      requests.push(row);
      pending.add(r);
    }
    return row;
  }
  /** @param {Request} r @param {'finished' | 'failed'} outcome */
  function finish(r, outcome) {
    const row = observe(r);
    if (!row || row.outcome !== 'pending') return;
    row.outcome = outcome;
    row.failure =
      outcome === 'failed' ? (r.failure()?.errorText ?? 'Unknown transport failure') : null;
    jobs.push(
      (async () => {
        // Fetch these independently: a missing body must not discard an available status
        // or wire size. A partial failed body is never a complete decoded body.
        try {
          const response = await r.response();
          if (response) {
            row.status = response.status();
            if (outcome === 'finished') row.decodedBytes = (await response.body()).length;
          } else if (outcome === 'finished') row.metadataErrors.push('Missing response');
        } catch (error) {
          row.metadataErrors.push('Response: ' + String(error));
        }
        try {
          const sizes = await r.sizes();
          for (const key of /** @type {const} */ ([
            'requestBodySize',
            'requestHeadersSize',
            'responseBodySize',
            'responseHeadersSize',
          ])) {
            const value = sizes[key];
            if (Number.isFinite(value) && value >= 0) row[key] = value;
            else row.metadataErrors.push('Unavailable ' + key);
          }
        } catch (error) {
          row.metadataErrors.push('Sizes: ' + String(error));
        }
        pending.delete(r);
      })(),
    );
  }
  page.on('request', observe);
  page.on('requestfailed', (r) => finish(r, 'failed'));
  page.on('requestfinished', (r) => finish(r, 'finished'));
  async function settle() {
    let done = 0;
    while (done < jobs.length) {
      const batch = jobs.slice(done);
      done = jobs.length;
      await Promise.all(batch);
    }
  }
  function summary() {
    const selectors = {
      /** @param {Read} r */
      decodedBytes: (r) => [r.decodedBytes],
      /** @param {Read} r */
      requestBodyBytes: (r) => [r.requestBodySize],
      /** @param {Read} r */
      requestBytes: (r) => [r.requestBodySize, r.requestHeadersSize],
      /** @param {Read} r */
      responseWireBytes: (r) => [r.responseBodySize, r.responseHeadersSize],
    };
    const totals = Object.fromEntries(
      Object.entries(selectors).map(([key, select]) => {
        const values = requests.flatMap(select);
        return [
          key,
          values.every((n) => n !== null) ? values.reduce((sum, n) => sum + (n ?? 0), 0) : null,
        ];
      }),
    );
    const observedBytes = Object.fromEntries(
      Object.entries(selectors).map(([key, select]) => [
        key,
        requests.flatMap(select).reduce((sum, n) => sum + (n ?? 0), 0),
      ]),
    );
    const failedCount = requests.filter((r) => r.outcome === 'failed').length;
    const httpErrorCount = requests.filter((r) => r.status !== null && r.status >= 400).length;
    const metadataErrorCount = requests.reduce((sum, r) => sum + r.metadataErrors.length, 0);
    return {
      ...totals,
      observedBytes,
      failedCount,
      httpErrorCount,
      metadataErrorCount,
      pendingCount: pending.size,
      complete:
        requests.length > 0 &&
        pending.size === 0 &&
        failedCount === 0 &&
        httpErrorCount === 0 &&
        metadataErrorCount === 0 &&
        Object.values(totals).every((n) => n !== null),
    };
  }
  return { requests, pending, settle, summary };
}
