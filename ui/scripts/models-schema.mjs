// The schemas of the server-wide settings kinds from docs/openapi.json: `ModelsSettings`
// and the schemas it refers to, written to src/lib/models-schema.json for the form of the
// server page's Models section, and `NotificationsSettings`, written to
// src/lib/notifications-schema.json for its Notifications section. Run it after
// `mise run openapi` changes the description (`pnpm models-schema`).
// scripts/models-schema.test.ts fails while a copy is stale.

import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
export const OPENAPI = join(here, '../../docs/openapi.json');
export const OUT = join(here, '../src/lib/models-schema.json');
export const NOTIFICATIONS_OUT = join(here, '../src/lib/notifications-schema.json');
const ROOT = 'ModelsSettings';
/** Each root schema and the file its extract is written to. */
export const OUTPUTS = { ModelsSettings: OUT, NotificationsSettings: NOTIFICATIONS_OUT };

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
 * The text of the extract of `root` for the description `doc`.
 * @param {any} doc
 */
export const render = (doc, root = ROOT) => JSON.stringify(extract(doc, root), null, 2) + '\n';

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const doc = JSON.parse(readFileSync(OPENAPI, 'utf8'));
  for (const [root, out] of Object.entries(OUTPUTS)) {
    writeFileSync(out, render(doc, root));
    console.log(`wrote ${out}`);
  }
}
