import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
const expected = process.argv[2]?.replace(/^node-v/, '');
if (!expected) throw Error('Pass a node-vVERSION release tag');
const root = fileURLToPath(new URL('../', import.meta.url));
for (const path of [
  'common',
  'client',
  'engine',
  ...readdirSync(root + 'engine/npm').map((p) => 'engine/npm/' + p),
]) {
  const pkg = JSON.parse(readFileSync(root + path + '/package.json'));
  if (pkg.version !== expected)
    throw Error(`${pkg.name}: version ${pkg.version} differs from ${expected}`);
}
const cargo = readFileSync(
  fileURLToPath(new URL('../../crates/sparkles-node/Cargo.toml', import.meta.url)),
  'utf8',
);
const native = /^version\s*=\s*"([^"]+)"/m.exec(cargo)?.[1];
if (native !== expected) throw Error(`Native crate version ${native} differs from ${expected}`);
