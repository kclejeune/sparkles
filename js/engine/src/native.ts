import { createRequire } from 'node:module';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
const require = createRequire(import.meta.url);
const libc =
  process.platform === 'linux' &&
  !(process.report?.getReport() as { header?: { glibcVersionRuntime?: string } })?.header
    ?.glibcVersionRuntime
    ? 'musl'
    : 'gnu';
const target = `${process.platform}-${process.arch}${process.platform === 'linux' ? '-' + libc : process.platform === 'win32' ? '-msvc' : ''}`;
const override = process.env.SPARKLES_NODE_NATIVE;
const local = fileURLToPath(new URL(`../native/sparkles.${target}.node`, import.meta.url));
export const native: any = override
  ? require(override)
  : existsSync(local)
    ? require(local)
    : require(`@sparkles-rdf/engine-${target}`);
