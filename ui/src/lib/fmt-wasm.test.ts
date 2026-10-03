import { existsSync, readFileSync } from 'node:fs';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError, type FormatResult } from './api';
import { localFormatter, LocalUnavailable, type FmtModule } from './fmt-wasm';

const result = (text: string): FormatResult => ({
  text,
  changed: true,
  language: 'sparql',
  cursorOffset: null,
  warnings: [],
});

/** A stand-in for the generated bindings: `answer` maps the request to the JSON answer. */
function fake(answer: (req: { text: string }) => unknown) {
  const m = {
    default: vi.fn(async () => undefined),
    format: vi.fn((request: string) => JSON.stringify(answer(JSON.parse(request)))),
  };
  return { m, load: vi.fn(async (): Promise<FmtModule> => m) };
}

/** Answers like the endpoint, and counts its calls. */
function endpoint() {
  const fetch = vi.fn(
    async (_url: RequestInfo | URL, _init?: RequestInit) =>
      new Response(JSON.stringify(result('from the server\n')), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }),
  );
  vi.stubGlobal('fetch', fetch);
  return fetch;
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('localFormatter', () => {
  it('loads the module once, on first use, and formats without the endpoint', async () => {
    const remote = endpoint();
    const { m, load } = fake((req) => result(req.text.toUpperCase()));
    const f = localFormatter(load);
    expect(load).not.toHaveBeenCalled();
    expect(await f.formatAny({ text: 'ask {}' })).toEqual(result('ASK {}'));
    expect(await f.formatLocal({ text: 'x', language: 'sparql', cursorOffset: 1 })).toEqual(
      result('X'),
    );
    expect(load).toHaveBeenCalledTimes(1);
    expect(m.default).toHaveBeenCalledTimes(1);
    // the request goes in as the endpoint's JSON body
    expect(JSON.parse(m.format.mock.calls[1][0])).toEqual({
      text: 'x',
      language: 'sparql',
      cursorOffset: 1,
    });
    expect(remote).not.toHaveBeenCalled();
  });

  it("turns an error answer into the endpoint's ApiError, which is final", async () => {
    const remote = endpoint();
    const { load } = fake(() => ({
      status: 400,
      error: 'SPARQL syntax error at line 2, column 3: expected one of …',
      code: 'syntax',
      line: 2,
      column: 3,
      language: 'sparql',
      detail: 'expected one of a, b, c',
    }));
    const f = localFormatter(load);
    const e = await f.formatAny({ text: 'select' }).catch((e) => e);
    expect(e).toBeInstanceOf(ApiError);
    expect(e).toMatchObject({
      status: 400,
      message: 'SPARQL syntax error at line 2, column 3: expected one of …',
      code: 'syntax',
      line: 2,
      column: 3,
      detail: 'expected one of a, b, c',
    });
    expect(remote).not.toHaveBeenCalled();

    const refused = localFormatter(
      fake(() => ({ status: 422, error: 'refused', code: 'unsafe-format' })).load,
    );
    await expect(refused.formatLocal({ text: 'x' })).rejects.toMatchObject({
      status: 422,
      code: 'unsafe-format',
    });
  });

  it('uses the endpoint when the build has no module', async () => {
    const remote = endpoint();
    const f = localFormatter(undefined);
    await expect(f.formatLocal({ text: 'x' })).rejects.toBeInstanceOf(LocalUnavailable);
    expect(await f.formatAny({ text: 'x' })).toEqual(result('from the server\n'));
    expect(remote).toHaveBeenCalledTimes(1);
    expect(String(remote.mock.calls[0][0])).toBe('/$/format');
  });

  it('lints with the module when it has a linter, else through the endpoint', async () => {
    const remote = endpoint();
    const answer = { language: 'sparql', diagnostics: [] };
    const { m, load } = fake(() => answer);
    const lint = vi.fn((request: string) =>
      JSON.stringify({ ...answer, echo: JSON.parse(request) }),
    );
    const f = localFormatter(async () => ({ ...m, lint }));
    expect(await f.lintAny({ text: 'ASK {}', fix: true })).toMatchObject({
      echo: { text: 'ASK {}', fix: true },
    });
    expect(remote).not.toHaveBeenCalled();
    // a module built before the linter: the endpoint
    const old = localFormatter(load);
    await expect(old.lintLocal({ text: 'ASK {}' })).rejects.toBeInstanceOf(LocalUnavailable);
    await old.lintAny({ text: 'ASK {}' });
    expect(String(remote.mock.calls[0][0])).toBe('/$/lint');
  });

  it('uses the endpoint when the module does not load, and does not try again', async () => {
    const remote = endpoint();
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    const load = vi.fn(async (): Promise<FmtModule> => {
      throw new TypeError('Failed to fetch dynamically imported module');
    });
    const f = localFormatter(load);
    expect(await f.formatAny({ text: 'x' })).toEqual(result('from the server\n'));
    expect(await f.formatAny({ text: 'y' })).toEqual(result('from the server\n'));
    expect(load).toHaveBeenCalledTimes(1);
    expect(remote).toHaveBeenCalledTimes(2);

    // instantiating fails (no WebAssembly, or a policy that forbids compiling it)
    const { m, load: load2 } = fake(() => result('never'));
    m.default.mockRejectedValue(new WebAssembly.CompileError('refused'));
    const g = localFormatter(load2);
    expect(await g.formatAny({ text: 'x' })).toEqual(result('from the server\n'));
    expect(m.format).not.toHaveBeenCalled();
  });

  it('uses the endpoint from the first failure of the running module on', async () => {
    const remote = endpoint();
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    const { m, load } = fake((req) => result(req.text));
    const f = localFormatter(load);
    expect(await f.formatAny({ text: 'a' })).toEqual(result('a'));
    m.format.mockImplementationOnce(() => {
      throw new WebAssembly.RuntimeError('unreachable');
    });
    expect(await f.formatAny({ text: 'b' })).toEqual(result('from the server\n'));
    // the instance may be left inconsistent: it is not used again
    expect(await f.formatAny({ text: 'c' })).toEqual(result('from the server\n'));
    expect(m.format).toHaveBeenCalledTimes(2);
    expect(remote).toHaveBeenCalledTimes(2);
  });
});

// The real module, when this checkout built it (`mise run ui:wasm`).
const built = new URL('./wasm/sparkles_fmt_wasm_bg.wasm', import.meta.url);

describe.skipIf(!existsSync(built))('the built module', () => {
  it('formats every language and answers errors like the endpoint', async () => {
    const bindings = await import(
      /* @vite-ignore */ new URL('./wasm/sparkles_fmt_wasm.js', import.meta.url).href
    );
    bindings.initSync({ module: readFileSync(built) });
    const f = localFormatter(async () => ({
      default: async () => undefined,
      format: bindings.format,
    }));
    const r = await f.formatLocal({ text: 'select * where{?s ?p ?o}', cursorOffset: 0 });
    expect(r).toEqual({
      text: 'SELECT *\nWHERE {\n  ?s ?p ?o .\n}\n',
      changed: true,
      language: 'sparql',
      cursorOffset: 0,
      warnings: [],
    });
    for (const [language, text] of [
      ['turtle', '<http://a> <http://b> <http://c>.'],
      ['trig', '<http://g> { <http://a> <http://b> 1 }'],
      ['ntriples', '<http://a>  <http://b> <http://c> .'],
      ['nquads', '<http://a> <http://b> <http://c>  <http://g> .'],
      ['jsonld', '{"@id":"http://a","http://b":1}'],
    ] as const) {
      const r = await f.formatLocal({ text, language });
      expect(r.language).toBe(language);
      expect((await f.formatLocal({ text: r.text, language })).changed).toBe(false);
    }
    await expect(f.formatLocal({ text: 'select * {', language: 'sparql' })).rejects.toMatchObject({
      status: 400,
      code: 'syntax',
      line: 1,
      column: 11,
    });
    await expect(
      f.formatLocal({ text: 'ASK {}', options: { lineWidth: 1000 } }),
    ).rejects.toMatchObject({ status: 400, code: 'bad-request' });
  });
});
