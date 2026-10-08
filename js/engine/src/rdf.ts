import type * as RDF from '@rdfjs/types';
import { createReadStream } from 'node:fs';
import type { Readable } from 'node:stream';
import {
  CancelledError,
  InvalidInputError,
  QueryTimeoutError,
  decodeTerm,
  encodeTerm,
  nativeError,
} from '@sparkles-rdf/common';
import { native, type NativeRdfParser, type NativeRdfSerializer } from './native.js';

export type RdfInput =
  | string
  | Uint8Array
  | Readable
  | ReadableStream<Uint8Array>
  | AsyncIterable<string | Uint8Array>
  | { path: string };
export type SerializeOptions = {
  format?: string;
  compression?: string;
  signal?: AbortSignal;
  /** Milliseconds, covering the whole stream including stalled input/output. */
  timeout?: number;
};
export type ParseOptions = SerializeOptions & {
  baseIri?: string;
  lenient?: boolean;
  factory?: RDF.DataFactory;
};

const encoder = new TextEncoder();
const CHUNK = 64 << 10;
type Stopped = { stopped: true };
const STOPPED: Stopped = { stopped: true };

/** Every wait, including arbitrary user iterators, races the stream lifetime. */
class Lifetime {
  readonly token = new native.Cancellation();
  readonly stopped: Promise<Stopped>;
  private resolve!: (v: Stopped) => void;
  private timer?: ReturnType<typeof setTimeout>;
  private abort?: () => void;
  ended = false;
  reason?: unknown;
  cleanup: () => void = () => {};
  constructor(private options: SerializeOptions) {
    if (
      options.timeout !== undefined &&
      (!Number.isSafeInteger(options.timeout) ||
        options.timeout <= 0 ||
        options.timeout > 2_147_483_647)
    )
      throw new InvalidInputError(
        'Timeout must be an integer from 1 to 2,147,483,647 milliseconds',
      );
    this.stopped = new Promise((resolve) => {
      this.resolve = resolve;
    });
    this.abort = () => this.stop(options.signal?.reason ?? new CancelledError());
    options.signal?.addEventListener('abort', this.abort, { once: true });
    if (options.signal?.aborted) this.abort();
    if (!this.ended && options.timeout !== undefined)
      this.timer = setTimeout(() => this.stop(new QueryTimeoutError()), options.timeout);
  }
  stop(reason?: unknown) {
    if (this.ended) return;
    this.ended = true;
    this.reason = reason;
    this.token.cancel();
    if (this.timer) clearTimeout(this.timer);
    if (this.abort) this.options.signal?.removeEventListener('abort', this.abort);
    this.cleanup();
    this.resolve(STOPPED);
  }
  async wait<T>(request: PromiseLike<T>): Promise<T | Stopped> {
    const result = await Promise.race([Promise.resolve(request), this.stopped]);
    if (this.ended) {
      if (this.reason !== undefined) throw this.reason;
      return STOPPED;
    }
    return result;
  }
}
function isStopped(value: unknown): value is Stopped {
  return value === STOPPED;
}
function quietReturn(iterator: AsyncIterator<unknown> | Iterator<unknown>) {
  // A user producer may itself be stalled forever. Invoke cleanup, but stream
  // cancellation must not wait for that producer to acknowledge it.
  try {
    void Promise.resolve(iterator.return?.()).catch(() => {});
  } catch {
    /* cleanup best effort */
  }
}
function inputIterator(
  input: RdfInput,
): AsyncIterator<string | Uint8Array> | Iterator<string | Uint8Array> {
  if (typeof input === 'string' || input instanceof Uint8Array) return [input][Symbol.iterator]();
  if (input && typeof input === 'object' && 'path' in input)
    return createReadStream(input.path)[Symbol.asyncIterator]();
  if (input instanceof ReadableStream) {
    const reader = input.getReader();
    let released = false;
    const release = () => {
      if (!released) {
        released = true;
        reader.releaseLock();
      }
    };
    return {
      async next() {
        try {
          const r = await reader.read();
          if (r.done) release();
          return r as IteratorResult<Uint8Array>;
        } catch (e) {
          release();
          throw e;
        }
      },
      return() {
        const pending = reader.cancel();
        // cancel closes the reader synchronously even if the underlying source's
        // cancellation callback returns a never-settling promise.
        release();
        void pending.catch(() => {});
        return Promise.resolve({ done: true, value: undefined });
      },
    };
  }
  if (input && typeof input[Symbol.asyncIterator] === 'function')
    return input[Symbol.asyncIterator]();
  throw new InvalidInputError('Expected RDF text, bytes, a stream, an async iterable, or {path}');
}
const wire = (options: SerializeOptions) =>
  JSON.stringify({ ...options, signal: undefined, factory: undefined });

/** Parse RDF through bounded native channels; no dataset is materialized. */
export function parse(
  input: RdfInput,
  options: ParseOptions = {},
): AsyncIterableIterator<RDF.Quad> {
  let life: Lifetime | undefined;
  let handle: NativeRdfParser | undefined;
  let ended = false;
  let serial: Promise<unknown> = Promise.resolve();
  const start = () => {
    if (life) return;
    life = new Lifetime(options);
    try {
      if (life.ended) throw life.reason;
      const parser = new native.NativeRdfParser(wire(options), life.token);
      handle = parser;
      const source = inputIterator(input);
      const owner = life;
      owner.cleanup = () => {
        parser.close();
        quietReturn(source);
      };
      if (owner.ended) owner.cleanup();
      void (async () => {
        try {
          while (!owner.ended) {
            const item = await owner.wait(Promise.resolve(source.next()));
            if (isStopped(item) || item.done) break;
            if (typeof item.value !== 'string' && !(item.value instanceof Uint8Array))
              throw new InvalidInputError('RDF input chunks must be strings or Uint8Array');
            const bytes = typeof item.value === 'string' ? encoder.encode(item.value) : item.value;
            if (!bytes.length) {
              // Empty synchronous chunks still yield the event loop so abort
              // listeners and timeout callbacks cannot be starved indefinitely.
              await owner.wait(new Promise<void>((resolve) => setImmediate(resolve)));
            }
            for (let i = 0; i < bytes.length && !owner.ended; i += CHUNK) {
              try {
                await owner.wait(parser.push(Buffer.from(bytes.subarray(i, i + CHUNK))));
              } catch (e) {
                // Native parser errors arrive on its output channel. A closed
                // input channel must not replace a syntax error with a push error.
                if (!owner.ended && nativeError(e).message.includes('RDF worker ended')) return;
                throw e;
              }
            }
          }
          if (!owner.ended) parser.end();
        } catch (e) {
          owner.stop(nativeError(e));
        }
      })();
    } catch (e) {
      life.stop(nativeError(e));
      throw nativeError(e);
    }
  };
  const iterator: AsyncIterableIterator<RDF.Quad> = {
    [Symbol.asyncIterator]() {
      return this;
    },
    next() {
      const request = serial.then(async (): Promise<IteratorResult<RDF.Quad>> => {
        if (ended) return { done: true, value: undefined };
        try {
          start();
          const owner = life!;
          const row = await owner.wait(handle!.nextQuad());
          if (isStopped(row) || row === null || row === undefined) {
            ended = true;
            owner.stop();
            return { done: true, value: undefined };
          }
          return {
            done: false,
            value: decodeTerm(JSON.parse(row as string), options.factory) as RDF.Quad,
          };
        } catch (e) {
          ended = true;
          life?.stop(nativeError(e));
          throw nativeError(e);
        }
      });
      serial = request.catch(() => {});
      return request;
    },
    async return() {
      ended = true;
      life?.stop();
      return { done: true, value: undefined };
    },
    async throw(e?: unknown) {
      ended = true;
      life?.stop(e);
      throw e;
    },
  };
  return iterator;
}

/** Serialize quads incrementally, retaining at most bounded native channel data. */
export function serialize(
  quads: Iterable<RDF.Quad> | AsyncIterable<RDF.Quad>,
  options: SerializeOptions = {},
): ReadableStream<Uint8Array> {
  const life = new Lifetime(options);
  let handle: NativeRdfSerializer | undefined;
  try {
    if (life.ended) throw life.reason;
    handle = new native.NativeRdfSerializer(wire(options), life.token);
  } catch (e) {
    life.stop(nativeError(e));
    throw nativeError(e);
  }
  let source: Iterator<RDF.Quad> | AsyncIterator<RDF.Quad>;
  try {
    source =
      Symbol.asyncIterator in quads ? quads[Symbol.asyncIterator]() : quads[Symbol.iterator]();
  } catch (e) {
    life.stop(e);
    handle.close();
    throw new InvalidInputError('Expected an iterable of RDF quads');
  }
  life.cleanup = () => {
    handle.close();
    quietReturn(source);
  };
  if (life.ended) life.cleanup();
  void (async () => {
    try {
      while (!life.ended) {
        const item = await life.wait(Promise.resolve(source.next()));
        if (isStopped(item) || item.done) break;
        if (item.value?.termType !== 'Quad') throw new InvalidInputError('Expected an RDF quad');
        try {
          await life.wait(handle.push(JSON.stringify(encodeTerm(item.value))));
        } catch (e) {
          if (!life.ended && nativeError(e).message.includes('RDF worker ended')) return;
          throw e;
        }
      }
      if (!life.ended) handle.end();
    } catch (e) {
      life.stop(nativeError(e));
    }
  })();
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        const bytes = await life.wait(handle.nextBytes());
        if (isStopped(bytes) || bytes === null || bytes === undefined) {
          life.stop();
          controller.close();
        } else controller.enqueue(bytes as Uint8Array);
      } catch (e) {
        life.stop(nativeError(e));
        controller.error(nativeError(e));
      }
    },
    cancel() {
      life.stop();
    },
  });
}
