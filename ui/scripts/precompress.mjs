// Writes brotli (.br) and gzip (.gz) siblings of the built text assets and of the
// formatter's WebAssembly module (when the build has it), which the server embeds and
// serves by Accept-Encoding instead of compressing per request.
// A sibling is kept only when it is at least 10% smaller than the asset.
import { readdir, readFile, writeFile, stat, unlink } from 'node:fs/promises';
import { join, extname } from 'node:path';
import { brotliCompressSync, gzipSync, constants } from 'node:zlib';

const root = new URL('../build/', import.meta.url).pathname;
const COMPRESSIBLE = new Set(['.js', '.css', '.html', '.svg', '.json', '.map', '.txt', '.wasm']);
const MIN = 1024;

async function* files(dir) {
  for (const e of await readdir(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) yield* files(p);
    else yield p;
  }
}

let raw = 0;
let br = 0;
let n = 0;
for await (const p of files(root)) {
  if (p.endsWith('.br') || p.endsWith('.gz')) {
    await unlink(p); // stale sibling of an earlier build
    continue;
  }
  if (!COMPRESSIBLE.has(extname(p)) || (await stat(p)).size < MIN) continue;
  const data = await readFile(p);
  const variants = [
    [
      '.br',
      brotliCompressSync(data, {
        params: {
          [constants.BROTLI_PARAM_QUALITY]: 11,
          [constants.BROTLI_PARAM_SIZE_HINT]: data.length,
        },
      }),
    ],
    ['.gz', gzipSync(data, { level: 9 })],
  ];
  for (const [ext, out] of variants) {
    if (out.length <= data.length * 0.9) await writeFile(p + ext, out);
    if (ext === '.br') br += Math.min(out.length, data.length);
  }
  raw += data.length;
  n++;
}
console.log(`precompressed ${n} assets: ${raw} bytes, ${br} with brotli`);
