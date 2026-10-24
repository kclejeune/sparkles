// The schemas of the model configuration (`ModelsSettings` and the schemas it refers to)
// from docs/openapi.json, written to src/lib/models-schema.json for the form of the
// server page's Models section. Run it after `mise run openapi` changes the description
// (`pnpm models-schema`); scripts/models-schema.test.ts fails while the copy is stale.

import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
export const OPENAPI = join(here, '../../docs/openapi.json');
export const OUT = join(here, '../src/lib/models-schema.json');
const ROOT = 'ModelsSettings';

/**
 * The schema named `root` and every schema it refers to, without `externalDocs`.
 * @param {any} doc
 * @returns {Record<string, any>}
 */
export function extract(doc, root = ROOT) {
  const all = doc.components.schemas;
  /** @type {Record<string, any>} */
  const out = {};
  /** @param {string} name */
  const visit = (name) => {
    if (out[name]) return;
    if (!all[name]) throw new Error(`docs/openapi.json has no schema ${name}`);
    const { externalDocs: _, ...s } = all[name];
    out[name] = s;
    walk(s);
  };
  /** @param {unknown} v */
  const walk = (v) => {
    if (Array.isArray(v)) v.forEach(walk);
    else if (v && typeof v === 'object')
      for (const [k, x] of Object.entries(v)) {
        if (k === '$ref' && typeof x === 'string') visit(String(x.split('/').pop()));
        else walk(x);
      }
  };
  visit(root);
  return Object.fromEntries(
    Object.keys(out)
      .sort()
      .map((k) => [k, out[k]]),
  );
}

/**
 * The text of src/lib/models-schema.json for the description `doc`.
 * @param {any} doc
 */
export const render = (doc) => JSON.stringify(extract(doc), null, 2) + '\n';

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  writeFileSync(OUT, render(JSON.parse(readFileSync(OPENAPI, 'utf8'))));
  console.log(`wrote ${OUT}`);
}
