import { CancelledError, InvalidInputError, QueryTimeoutError } from '@sparkles-rdf/common';
import type { OperationOptions } from '@sparkles-rdf/common';

export function abortError(signal: AbortSignal): Error {
  return signal.reason?.name === 'TimeoutError' ? new QueryTimeoutError() : new CancelledError();
}
export function checkSignal(signal?: AbortSignal | null) {
  if (signal?.aborted) throw abortError(signal);
}
export async function controlled<T>(value: PromiseLike<T> | T, signal: AbortSignal): Promise<T> {
  checkSignal(signal);
  let listener: () => void = () => {};
  try {
    const result = await Promise.race([
      Promise.resolve(value),
      new Promise<never>((_, reject) => {
        listener = () => reject(abortError(signal));
        signal.addEventListener('abort', listener, { once: true });
        if (signal.aborted) listener();
      }),
    ]);
    checkSignal(signal);
    return result;
  } finally {
    signal.removeEventListener('abort', listener);
  }
}
export function operationSignal(options: OperationOptions): AbortSignal | undefined {
  if (options.timeout === undefined) return options.signal;
  if (!Number.isSafeInteger(options.timeout) || options.timeout < 0 || options.timeout > 0xffffffff)
    throw new InvalidInputError(
      'timeout must be an integer in milliseconds between 0 and 4294967295',
    );
  const deadline = AbortSignal.timeout(options.timeout);
  return options.signal ? AbortSignal.any([options.signal, deadline]) : deadline;
}
/** Enforce cancellation even when an injected fetch ignores its signal. */
export async function fetchControlled(fetcher: typeof fetch, request: Request): Promise<Response> {
  const signal = request.signal;
  checkSignal(signal);
  let aborted = false;
  let listener: () => void = () => {};
  const pending = Promise.resolve()
    .then(() => fetcher(request))
    .then((response) => {
      if (aborted || signal.aborted) {
        void response.body?.cancel().catch(() => {});
        throw abortError(signal);
      }
      return controlledResponse(response, signal);
    });
  try {
    return await Promise.race([
      pending,
      new Promise<never>((_, reject) => {
        listener = () => {
          aborted = true;
          reject(abortError(signal));
        };
        signal.addEventListener('abort', listener, { once: true });
        if (signal.aborted) listener();
      }),
    ]);
  } catch (e) {
    if (signal.aborted) throw abortError(signal);
    throw e;
  } finally {
    signal.removeEventListener('abort', listener);
  }
}

function controlledResponse(response: Response, signal: AbortSignal): Response {
  if (!response.body) return response;
  const source = response.body.getReader();
  let ended = false;
  let controller: ReadableStreamDefaultController<Uint8Array>;
  const finish = () => {
    if (ended) return;
    ended = true;
    signal.removeEventListener('abort', abort);
  };
  const abort = () => {
    if (ended) return;
    finish();
    controller.error(abortError(signal));
    void source
      .cancel()
      .catch(() => {})
      .finally(() => source.releaseLock());
  };
  const body = new ReadableStream<Uint8Array>({
    start(c) {
      controller = c;
      signal.addEventListener('abort', abort, { once: true });
      if (signal.aborted) abort();
    },
    async pull(c) {
      try {
        const row = await source.read();
        if (ended) return;
        if (row.done) {
          finish();
          source.releaseLock();
          c.close();
        } else c.enqueue(row.value);
      } catch (e) {
        if (!ended) {
          finish();
          source.releaseLock();
          c.error(e);
        }
      }
    },
    async cancel(reason) {
      if (ended) return;
      finish();
      try {
        await source.cancel(reason);
      } finally {
        source.releaseLock();
      }
    },
  });
  const wrapped = new Response(body, {
    status: response.status,
    statusText: response.statusText,
    headers: response.headers,
  });
  for (const key of ['url', 'redirected', 'type'] as const)
    Object.defineProperty(wrapped, key, { value: response[key] });
  return wrapped;
}
export async function delay(milliseconds: number, signal?: AbortSignal | null) {
  checkSignal(signal);
  await new Promise<void>((resolve, reject) => {
    const listener = () => {
      clearTimeout(timer);
      signal?.removeEventListener('abort', listener);
      reject(abortError(signal!));
    };
    const timer = setTimeout(() => {
      signal?.removeEventListener('abort', listener);
      resolve();
    }, milliseconds);
    signal?.addEventListener('abort', listener, { once: true });
    if (signal?.aborted) listener();
  });
}
/** Reader ownership extends the request's signal through all result consumption. */
export class BodyReader {
  private reader: ReadableStreamDefaultReader<Uint8Array>;
  private listener: () => void;
  private disposed = false;
  private closing?: Promise<void>;
  constructor(
    body: ReadableStream<Uint8Array>,
    private signal?: AbortSignal,
  ) {
    this.reader = body.getReader();
    this.listener = () => {
      void this.reader.cancel().catch(() => {});
    };
    signal?.addEventListener('abort', this.listener, { once: true });
    if (signal?.aborted) this.listener();
  }
  async read() {
    checkSignal(this.signal);
    const result = await this.reader.read();
    checkSignal(this.signal);
    return result;
  }
  cancel(): Promise<void> {
    return (this.closing ??= this.reader.cancel().catch(() => {}));
  }
  release() {
    if (this.disposed) return;
    this.disposed = true;
    this.signal?.removeEventListener('abort', this.listener);
    this.reader.releaseLock();
  }
}
