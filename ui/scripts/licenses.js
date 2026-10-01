// A Vite plugin that writes licenses.txt into the client build: the license and notice
// files of every npm package with code or assets in the bundle (devDependencies included:
// the Svelte runtime and SvelteKit's client ship, the compilers and test tools do not).
//
// The packages come from the bundle itself: the modules of the output chunks (JavaScript
// that survived tree shaking, and stylesheets) and the source files of the emitted assets
// (fonts), each attributed to the package under the last `node_modules/` of its path.
// Everything is read from node_modules, so the output depends only on the lockfile and
// is reproducible; `mise run licenses` copies it to THIRD_PARTY_LICENSES-UI.md and
// `mise run licenses:check` fails when the copy is out of date.
//
// The build fails when a shipped package has no license that PERMISSIVE allows.
import { createHash } from 'node:crypto';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';

/** @typedef {{ dir: string, name: string, version: string, license: string }} Package */

/**
 * SPDX identifiers a shipped package may be used under. All of them only ask for the
 * copyright and license text to travel with the code. OFL-1.1 is the fonts' license
 * (@fontsource-variable/*): it allows bundling the fonts with any software.
 */
export const PERMISSIVE = new Set([
  '0BSD',
  'Apache-2.0',
  'BlueOak-1.0.0',
  'BSD-2-Clause',
  'BSD-3-Clause',
  'CC0-1.0',
  'ISC',
  'MIT',
  'MIT-0',
  'OFL-1.1',
  'Unlicense',
  'Zlib',
]);

/** File names (case-insensitive) that carry a license or a notice, not scripts. */
const LICENSE_FILE = /^(licen[cs]e|copying|copyright|notice|unlicense)([-._].*)?$/i;
const SCRIPT = /\.([cm]?[jt]s|json)$/i;

/**
 * Where a license file goes on to list the licenses of the code that the package's own
 * published files bundle (Vite's LICENSE.md): that code is not in the UI bundle.
 */
const BUNDLED = /^# Licenses of bundled dependencies$/m;

/**
 * Packages behind the bundler's virtual modules (`\0name/...`): runtime helpers that
 * Vite and Rolldown write into the output.
 */
const VIRTUAL = ['vite', 'rolldown'];

/**
 * Whether an SPDX expression can be satisfied with PERMISSIVE licenses alone (`OR`: one
 * side, `AND`: both, `WITH`: the exception's base license).
 * @param {string} expr
 */
export function permissive(expr) {
  const tokens = expr.match(/\(|\)|[^\s()]+/g) ?? [];
  let i = 0;
  /** @returns {boolean} */
  const or = () => {
    let ok = and();
    while (tokens[i] === 'OR') {
      i++;
      ok = and() || ok;
    }
    return ok;
  };
  /** @returns {boolean} */
  const and = () => {
    let ok = atom();
    while (tokens[i] === 'AND') {
      i++;
      ok = atom() && ok;
    }
    return ok;
  };
  /** @returns {boolean} */
  const atom = () => {
    const t = tokens[i++];
    if (t === '(') {
      const ok = or();
      if (tokens[i++] !== ')') throw new Error(`unbalanced SPDX expression ${expr}`);
      return ok;
    }
    if (tokens[i] === 'WITH') i += 2;
    return t !== undefined && PERMISSIVE.has(t.replace(/\+$/, ''));
  };
  const ok = or();
  return ok && i === tokens.length;
}

/**
 * The package directory of a file under node_modules (the last `node_modules/` of its
 * path, then the scope and the name), or null.
 * @param {string} file
 */
function packageDir(file) {
  const m = /^(.*\/node_modules\/(?:@[^/]+\/)?[^/]+)\//.exec(file.replaceAll('\\', '/'));
  return m ? m[1] : null;
}

/**
 * @param {string} dir
 * @returns {Package}
 */
function readPackage(dir) {
  const pkg = JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8'));
  // the old `licenses: [{ type }]` form means any of them
  const license =
    typeof pkg.license === 'string'
      ? pkg.license
      : Array.isArray(pkg.licenses)
        ? pkg.licenses.map((/** @type {{ type: string }} */ l) => l.type).join(' OR ')
        : '';
  return { dir, name: pkg.name, version: pkg.version, license };
}

/**
 * (name, text) of the license and notice files at the top of a package.
 * @param {string} dir
 * @returns {[string, string][]}
 */
function licenseFiles(dir) {
  return readdirSync(dir)
    .filter((f) => LICENSE_FILE.test(f) && !SCRIPT.test(f) && statSync(join(dir, f)).isFile())
    .sort()
    .map((f) => [
      f,
      readFileSync(join(dir, f), 'utf8')
        .replaceAll('\r\n', '\n')
        .split(BUNDLED)[0]
        .replace(/^\n+|\s+$/g, ''),
    ]);
}

/**
 * Texts that differ only in white space are the same text.
 * @param {string} text
 */
const key = (text) =>
  createHash('sha256').update(text.split(/\s+/).filter(Boolean).join(' ')).digest('hex');

/**
 * A code fence longer than any backtick run in `text`.
 * @param {string} text
 */
const fence = (text) =>
  '`'.repeat(Math.max(3, ...(text.match(/`+/g) ?? []).map((m) => m.length + 1)));

/**
 * The notices, as Markdown (THIRD_PARTY_LICENSES-UI.md).
 * @param {Package[]} pkgs
 */
export function render(pkgs) {
  /** @type {Map<string, { text: string, file: string, users: string[] }>} */
  const byText = new Map();
  /** @type {[Package, string[]][]} */
  const rows = [];
  for (const p of pkgs) {
    const files = licenseFiles(p.dir);
    if (files.length === 0) throw new Error(`${p.name} ${p.version}: no license file`);
    rows.push([p, files.map(([f]) => f)]);
    for (const [file, text] of files) {
      const k = key(text);
      if (!byText.has(k)) byText.set(k, { text, file, users: [] });
      byText.get(k)?.users.push(`${p.name} ${p.version}`);
    }
  }
  const o = [
    '# Third-party licenses: web UI\n',
    'The web UI embedded in the `sparkles` binary (served under `/ui/`) bundles code and ' +
      'fonts from the npm packages below. Their licenses and notices follow, each text once, ' +
      'with the packages that ship it. The running server serves this file at ' +
      '`/ui/licenses.txt`.\n',
    'Generated by the UI build (`ui/scripts/licenses.js`) from the packages in the bundle; ' +
      '`mise run licenses` copies it here. Do not edit.\n',
    '## Packages\n',
    '| Package | Version | License | Files |',
    '|---|---|---|---|',
    ...rows.map(
      ([p, files]) =>
        `| ${p.name} | ${p.version} | ${p.license.replaceAll('|', '\\|')} | ${files.join(', ')} |`,
    ),
    '',
    '## Texts\n',
  ];
  // notices first: Apache-2.0 asks for them to travel with the code
  const isNotice = (/** @type {string} */ f) => f.toLowerCase().startsWith('notice');
  const texts = [...byText.values()].sort(
    (a, b) =>
      Number(isNotice(b.file)) - Number(isNotice(a.file)) ||
      cmp(a.users[0], b.users[0]) ||
      cmp(a.file, b.file),
  );
  for (const { text, file, users } of texts) {
    const f = fence(text);
    o.push(`### ${file}: ${users.join(', ')}\n`, `${f}text\n${text}\n${f}\n`);
  }
  return o.join('\n');
}

/** Code point order, independent of the locale. */
const cmp = (/** @type {string} */ a, /** @type {string} */ b) => (a < b ? -1 : a > b ? 1 : 0);

/** @returns {import('vite').Plugin} */
export function licenses() {
  return {
    name: 'sparkles-licenses',
    apply: 'build',
    applyToEnvironment: (env) => env.name === 'client',
    generateBundle(_options, bundle) {
      const root = this.environment.config.root;
      const require = createRequire(join(root, 'package.json'));
      /** @type {Set<string>} */
      const dirs = new Set();
      /** @param {string} file */
      const add = (file) => {
        const dir = packageDir(file);
        if (dir) dirs.add(dir);
      };
      for (const out of Object.values(bundle)) {
        if (out.type === 'asset') {
          for (const f of out.originalFileNames) add(resolve(root, f));
          continue;
        }
        for (const [id, m] of Object.entries(out.modules)) {
          if (id.startsWith('\0')) {
            const name = VIRTUAL.find((n) => id.startsWith(`\0${n}/`));
            if (!name) this.error(`licenses: no package known for the virtual module ${id}`);
            // rolldown is Vite's dependency
            const from =
              name === 'vite' ? require : createRequire(require.resolve('vite/package.json'));
            dirs.add(dirname(from.resolve(`${name}/package.json`)));
          } else if (m.renderedLength > 0 || /\.css($|\?)/.test(id)) {
            add(id);
          }
        }
      }
      const pkgs = [...dirs]
        .map(readPackage)
        .sort((a, b) => cmp(a.name, b.name) || cmp(a.version, b.version));
      const bad = pkgs.filter((p) => !permissive(p.license));
      if (bad.length > 0) {
        this.error(
          'licenses: packages in the bundle without a license that PERMISSIVE allows: ' +
            bad.map((p) => `${p.name} ${p.version} (${p.license || 'none'})`).join(', '),
        );
      }
      this.emitFile({ type: 'asset', fileName: 'licenses.txt', source: render(pkgs) });
    },
  };
}
