// Package the complete published runtime/type dependency closure without fetching.
import {
  existsSync,
  readFileSync,
  realpathSync,
  mkdirSync,
  mkdtempSync,
  cpSync,
  rmSync,
} from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
const root = fileURLToPath(new URL('../', import.meta.url));
const out = resolve(process.argv[2] ?? join(root, 'packed'));
mkdirSync(out, { recursive: true });
const seen = new Set();
function pack(path) {
  path = realpathSync(path);
  const pkg = JSON.parse(readFileSync(join(path, 'package.json')));
  if (seen.has(pkg.name)) return;
  if (pkg.name.startsWith('@sparkles-rdf/engine'))
    for (const name of ['LICENSE', 'THIRD_PARTY_LICENSES.md'])
      if (!existsSync(join(path, name))) throw Error(`Missing ${name} for ${pkg.name}`);
  seen.add(pkg.name);
  if (pkg.name.startsWith('@sparkles-rdf/'))
    execFileSync('pnpm', ['--dir', path, 'pack', '--pack-destination', out], {
      stdio: ['ignore', 'ignore', 'inherit'],
    });
  else {
    // npm extraction ignores hard-link entries. pnpm stores identical declaration
    // files as hard links, so copy each file independently before repacking.
    const copy = mkdtempSync(join(tmpdir(), 'sparkles-node-dependency-'));
    try {
      cpSync(path, copy, { recursive: true, dereference: true });
      execFileSync('npm', ['pack', copy, '--pack-destination', out, '--ignore-scripts'], {
        stdio: ['ignore', 'ignore', 'inherit'],
      });
    } finally {
      rmSync(copy, { recursive: true, force: true });
    }
  }
  const require = createRequire(join(path, 'package.json'));
  for (const name of Object.keys(pkg.dependencies ?? {})) {
    if (name.startsWith('@sparkles-rdf/')) continue;
    const dirs = require.resolve.paths(name) ?? [];
    const dependency = dirs
      .map((dir) => join(dir, name))
      .find((dir) => existsSync(join(dir, 'package.json')));
    if (!dependency) throw Error(`Missing installed dependency ${name} of ${pkg.name}`);
    pack(dependency);
  }
}
for (const pkg of ['common', 'client', 'engine']) pack(join(root, pkg));
pack(join(root, 'node_modules', 'typescript'));
const native = (await import('node:fs/promises')).readdir;
for (const target of await native(join(root, 'engine', 'npm'))) {
  const path = join(root, 'engine', 'npm', target);
  if ((await native(path)).some((f) => f.endsWith('.node'))) pack(path);
}
