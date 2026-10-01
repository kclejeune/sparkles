// The formatter in the browser: the WebAssembly build of sparkles-fmt
// (crates/sparkles-fmt-wasm, built into src/lib/wasm by `mise run ui:wasm`), loaded the
// first time something is formatted. It takes the JSON body of `POST /$/format` and
// answers with the endpoint's JSON, so a result or an error reads the same from either.
//
// The module is optional. A UI built without it, a module that does not load (a network
// error, a browser without WebAssembly) and one that fails while running all format
// through the endpoint instead. An error the formatter reports (a syntax error, a
// refusal) is final: the endpoint would answer the same.

import { ApiError, format as formatRemote, type FormatRequest, type FormatResult } from './api';

/** What the generated bindings (wasm-bindgen `--target web`) export. */
export type FmtModule = {
  /** instantiate the module, fetched from next to the bindings */
  default: () => Promise<unknown>;
  /** a `POST /$/format` JSON body in, the endpoint's JSON answer out */
  format: (request: string) => string;
};

/** The browser formatter is not there: not in this build, not loaded, or broken. */
export class LocalUnavailable extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = 'LocalUnavailable';
  }
}

/** The endpoint's answer: the result, or the `ApiError` the endpoint's status would make. */
function answerOf(text: string): FormatResult {
  const body = JSON.parse(text);
  if (typeof body.status !== 'number') return body as FormatResult;
  throw new ApiError(body.status, String(body.error), {
    detail: body.detail,
    line: body.line,
    column: body.column,
    code: typeof body.code === 'string' ? body.code : undefined,
  });
}

/**
 * The formatting functions over the bindings `load` imports (`undefined` when the build
 * has none): `formatLocal` formats in the browser or throws `LocalUnavailable`;
 * `formatAny` formats in the browser when it can and through the endpoint otherwise.
 */
export function localFormatter(load: (() => Promise<FmtModule>) | undefined) {
  let ready: Promise<FmtModule | null> | undefined;
  let broken = false;

  // loaded once; a failure is remembered (the browser caches a failed import anyway)
  const module = () =>
    (ready ??= load
      ? load()
          .then(async (m) => {
            await m.default();
            return m;
          })
          .catch((e) => {
            console.warn('The browser formatter did not load; formatting on the server', e);
            return null;
          })
      : Promise.resolve(null));

  async function formatLocal(req: FormatRequest): Promise<FormatResult> {
    const m = broken ? null : await module();
    if (!m) throw new LocalUnavailable('the browser formatter is not available');
    let answer: string;
    try {
      answer = m.format(JSON.stringify(req));
    } catch (e) {
      // a trap (a panic aborts) leaves the instance unusable
      broken = true;
      console.warn('The browser formatter failed; formatting on the server from now on', e);
      throw new LocalUnavailable('the browser formatter failed', { cause: e });
    }
    return answerOf(answer);
  }

  async function formatAny(req: FormatRequest, signal?: AbortSignal): Promise<FormatResult> {
    try {
      return await formatLocal(req);
    } catch (e) {
      if (!(e instanceof LocalUnavailable)) throw e;
    }
    return formatRemote(req, signal);
  }

  return { formatLocal, formatAny };
}

// The bindings when the UI was built with them. `VITE_FMT_WASM=off` leaves them out (the
// mock UI tests, whose stand-in formatter is the mock's endpoint).
const bindings: Record<string, () => Promise<FmtModule>> =
  import.meta.env.VITE_FMT_WASM === 'off'
    ? {}
    : import.meta.glob<FmtModule>('./wasm/sparkles_fmt_wasm.js');

const shared = localFormatter(bindings['./wasm/sparkles_fmt_wasm.js']);

/** Format in the browser; throws `LocalUnavailable` without the module, else like `api.format`. */
export const formatLocal = shared.formatLocal;

/** Format in the browser when the module is there and works, else through `POST /$/format`. */
export const formatAny = shared.formatAny;
