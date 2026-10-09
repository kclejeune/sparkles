import { mkdir, writeFile, copyFile, readdir, readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
const engine = fileURLToPath(new URL('../engine/', import.meta.url));
const version = JSON.parse(await readFile(join(engine, 'package.json'), 'utf8')).version;
// Release targets: Linux on x64 and arm64 (glibc and musl) and macOS on Apple silicon.
const targets = [
  'linux-x64-gnu',
  'linux-arm64-gnu',
  'linux-x64-musl',
  'linux-arm64-musl',
  'darwin-arm64',
];
for (const target of targets) {
  const [os, cpu, libc] = target.split('-');
  const path = join(engine, 'npm', target);
  await mkdir(path, { recursive: true });
  const pkg = {
    name: `@sparkles-rdf/engine-${target}`,
    version,
    license: 'Apache-2.0',
    // npm checks the provenance statement against this repository
    repository: {
      type: 'git',
      url: 'git+https://github.com/kclejeune/sparkles.git',
      directory: `js/engine/npm/${target}`,
    },
    main: `sparkles.${target}.node`,
    files: [`sparkles.${target}.node`, 'LICENSE', 'THIRD_PARTY_LICENSES.md'],
    os: [os],
    cpu: [cpu],
    ...(os === 'linux' ? { libc: [libc === 'gnu' ? 'glibc' : libc] } : {}),
  };
  await writeFile(join(path, 'package.json'), JSON.stringify(pkg, null, 2) + '\n');
  for (const name of ['LICENSE', 'THIRD_PARTY_LICENSES.md']) {
    await copyFile(join(engine, name), join(path, name));
  }
}
for (const binary of await readdir(join(engine, 'native'))) {
  const match = /^sparkles\.(.*)\.node$/.exec(binary);
  if (match && targets.includes(match[1]))
    await copyFile(join(engine, 'native', binary), join(engine, 'npm', match[1], binary));
}
