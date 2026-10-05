import { readFile, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { execFileSync } from 'node:child_process';
const temp = await mkdtemp(join(tmpdir(), 'sparkles-openapi-'));
try {
  const target = join(temp, 'openapi.d.ts');
  execFileSync('pnpm', ['exec', 'openapi-typescript', '../docs/openapi.json', '-o', target]);
  if ((await readFile(target, 'utf8')) !== (await readFile('client/src/openapi.d.ts', 'utf8')))
    throw new Error('Generated client types are stale: pnpm client:gen');
} finally {
  await rm(temp, { recursive: true, force: true });
}
