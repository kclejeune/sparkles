// Branches and merges (docs/specs/F09-branches-and-merges.md): how a request names a
// branch, and the text the branch selector, the Branches panel and the merge dialog show.
import type { Branch, ConflictCell, MergeOutcome } from './api';
import { fmtBytes, fmtInt } from './format';
import { shorten, type PrefixMap } from './rdf';

/** The branch every dataset has. */
export const MAIN = 'main';

/**
 * A branch name: a letter or digit, then letters, digits, `.`, `_` or `-` (64 at most),
 * with at least one letter, and not `head` (so it never reads as a commit selector).
 */
export function validBranchName(n: string): boolean {
  return /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(n) && /[A-Za-z]/.test(n) && n !== 'head';
}

/** The branch a `?branch=` value chooses: null for `main`, a missing or an empty value. */
export function branchParam(v: string | null | undefined): string | null {
  const b = v?.trim();
  return b && b !== MAIN ? b : null;
}

/**
 * The dataset target the API client takes: `name` for `main`, `name@branch` for another
 * branch. Dataset names cannot contain `@`, so this is the server's own path form.
 */
export function onBranch(ds: string, branch?: string | null): string {
  return branch && branch !== MAIN ? `${ds}@${branch}` : ds;
}

/** The dataset and the branch of a target (`main` when it names none). */
export function splitTarget(target: string): { name: string; branch: string } {
  const i = target.indexOf('@');
  return i < 0
    ? { name: target, branch: MAIN }
    : { name: target.slice(0, i), branch: target.slice(i + 1) || MAIN };
}

/** Admin routes that take `branch=`; the server refuses a branch on the others. */
const BRANCH_ROUTES = new Set([
  'commits',
  'snapshots',
  'history',
  'compaction',
  'compact',
  'stats',
  'schema',
  'reason',
  'validation',
  'prefixes',
  'describe',
  'text',
  'geo',
  'vector',
  'rdfs',
  'cache',
]);

/**
 * The request path for a path whose dataset is a target `name@branch` (as built with
 * `encodeURIComponent`, so `name%40branch`). A dataset route keeps the path form
 * `/{ds}@{branch}/…`. An admin route that takes a branch, `/$/{route}/{ds}@{branch}…`,
 * becomes `/$/{route}/{ds}…?branch=…`. Every other path is returned unchanged, so a route
 * that has no branches answers for the dataset `name@branch`, which does not exist.
 */
export function branchPath(path: string): string {
  const q = path.indexOf('?');
  const pathname = q < 0 ? path : path.slice(0, q);
  const query = q < 0 ? '' : path.slice(q + 1);
  const segs = pathname.split('/');
  if (segs[1] !== '$') {
    if (!segs[1] || !/%40/i.test(segs[1])) return path;
    segs[1] = segs[1].replace(/%40/gi, '@');
    return segs.join('/') + (q < 0 ? '' : `?${query}`);
  }
  const route = segs[2];
  // `/$/cache/clear/{ds}`; `/$/datasets/{ds}/clone` copies a branch
  const at = route === 'cache' ? 4 : 3;
  const takes = BRANCH_ROUTES.has(route) || (route === 'datasets' && segs[4] === 'clone');
  const seg = segs[at];
  if (!takes || !seg) return path;
  const i = seg.replace(/%40/gi, '@').indexOf('@');
  if (i < 0) return path;
  const plain = seg.replace(/%40/gi, '@');
  segs[at] = plain.slice(0, i);
  const branch = decodeURIComponent(plain.slice(i + 1));
  const rest = segs.join('/');
  if (!branch || branch === MAIN) return rest + (q < 0 ? '' : `?${query}`);
  const param = `branch=${encodeURIComponent(branch)}`;
  return `${rest}?${query ? `${query}&` : ''}${param}`;
}

/** "main@42": the commit a branch started from; "deleted@42" when that branch is gone. */
export function fromLabel(b: Pick<Branch, 'from'>): string {
  if (!b.from) return '';
  return `${b.from.branch ?? 'deleted'}@${b.from.seq}`;
}

/** "3 ahead, 1 behind", "up to date", or "" for a branch without an upstream. */
export function aheadBehind(b: Pick<Branch, 'upstream' | 'ahead' | 'behind'>): string {
  if (!b.upstream) return '';
  if (!b.ahead && !b.behind) return 'up to date';
  return `${fmtInt(b.ahead)} ahead, ${fmtInt(b.behind)} behind`;
}

/** The disk a branch holds: "12 KiB own, 2.0 MiB held". */
export function storageText(b: Pick<Branch, 'storage'>): string {
  const s = b.storage;
  if (!s) return '';
  const own = `${fmtBytes(s.ownBytes)} own`;
  return s.heldBytes ? `${own}, ${fmtBytes(s.heldBytes)} held` : own;
}

/** The selector's text for a branch: "dev · commit 57". */
export function branchOption(b: Pick<Branch, 'name' | 'head'>): string {
  return `${b.name} · commit ${fmtInt(b.head)}`;
}

/** A result label with the branch it was read from, when that is not main: "dev · at commit 57". */
export function onBranchLabel(branch: string | null | undefined, label: string): string {
  return branch && branch !== MAIN ? `${branch} · ${label}` : label;
}

/** Why deleting a branch needs a second look, or null when it does not. */
export function deleteWarning(b: Pick<Branch, 'name' | 'upstream' | 'ahead'>): string | null {
  if (!b.ahead) return null;
  const n = `${fmtInt(b.ahead)} commit${b.ahead === 1 ? '' : 's'}`;
  return `${b.name} has ${n} that ${b.upstream ?? 'its upstream'} does not have. Deleting it loses them.`;
}

/**
 * The conflicts a merge left unresolved: `conflictCount` of a preview, or the number
 * `conflicts` of a conflict report (a preview's `conflicts` is `{found, resolved}`).
 */
export function remainingConflicts(r: Pick<MergeOutcome, 'conflicts' | 'conflictCount'>): number {
  if (typeof r.conflictCount === 'number') return r.conflictCount;
  return typeof r.conflicts === 'number' ? r.conflicts : 0;
}

/** The merge's net change: "+3 −1". */
export function mergeChanges(r: Pick<MergeOutcome, 'changes'>): string {
  const c = r.changes ?? { inserted: 0, deleted: 0 };
  return `+${fmtInt(c.inserted)} −${fmtInt(c.deleted)}`;
}

/**
 * An N-Triples term with its IRI, or a literal's datatype, as a prefixed name where
 * `prefixes` has the namespace: `"31"^^xsd:integer`, `ex:age`.
 */
export function ntShort(t: string, prefixes: PrefixMap = {}): string {
  const iri = /^<([^>]*)>$/.exec(t);
  if (iri) return shorten(iri[1], prefixes) ?? t;
  const typed = /^(".*")\^\^<([^>]*)>$/s.exec(t);
  const dt = typed && shorten(typed[2], prefixes);
  return typed && dt ? `${typed[1]}^^${dt}` : t;
}

/** A conflict's place: "<s> <p> (default graph)", or with the graph's IRI. */
export function cellPlace(c: ConflictCell, prefixes: PrefixMap = {}): string {
  const at = c.graph == null ? '(default graph)' : `in ${ntShort(c.graph, prefixes)}`;
  return [c.subject, c.predicate]
    .filter((t): t is string => !!t)
    .map((t) => ntShort(t, prefixes))
    .concat(at)
    .join(' ');
}

/** The same merge from the command line, resolving every conflict one way or with a file. */
export function mergeCommands(o: {
  server: string;
  dataset: string;
  source: string;
  target: string;
}): string[] {
  const base = `sparkles merge --server ${o.server} --dataset ${o.dataset} ${o.source} --into ${o.target}`;
  return [`${base} --on-conflict theirs`, `${base} --resolve FILE.json`];
}
