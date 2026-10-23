// The canvas edits of U08 Phase 2 beyond the splice core in `pattern.ts`: changing a
// predicate or its path, filters, renames, deleting a node or a locked block, several
// triples at once, and the projection and modifiers. Every edit is a function of the text,
// its pattern and the action, and gives a list of changes in the coordinates of the text
// before the edit, which the editor applies as one transaction (§4.3). `agreesWith` checks
// R3 for every kind on the parse of the result.

import { ChangeSet } from '@codemirror/state';
import {
  agrees,
  applyChanges,
  checkTerm,
  declarations,
  drawing,
  edit as coreEdit,
  EditError,
  filtersOf,
  hash,
  homeOf,
  insertItem,
  itemIndent,
  lockedKeys,
  lockedOf,
  merge,
  normalized,
  optionalRemoval,
  prefixesIn,
  removalRange,
  resolveEdit,
  resolveTerm,
  signature,
  statement,
  termKey,
  triplesOf,
  anchor,
  bindsOf,
  type Change,
  type Edit,
  type Filter,
  type Group,
  type Item,
  type Locked,
  type PatternAnswer,
  type PatternOk,
  type Placed,
  type Prefixes,
  type Range,
  type Triple,
} from './pattern';

/** A triple written by an edit, as its three terms. */
export type Words = { s: string; p: string; o: string };

export type Phase2Edit =
  /** the predicate of a triple replaced, by an IRI, a variable or a simple path */
  | { kind: 'predicate'; edge: string; p: string; declare?: Prefixes }
  /** several triples added together, such as `?x a C` and the edge that uses `?x` */
  | { kind: 'add-many'; triples: Words[]; declare?: Prefixes }
  /** `FILTER(expr)` after the last triple that binds `var` */
  | { kind: 'filter-add'; var: string; expr: string; declare?: Prefixes }
  | { kind: 'filter-edit'; filter: string; expr: string; declare?: Prefixes }
  | { kind: 'filter-delete'; filter: string }
  /** every occurrence of `?from` in the editable subset, and in locked blocks with `locked` */
  | { kind: 'rename'; from: string; to: string; locked: boolean }
  /** every triple that touches a node, and the filters left without a bound variable */
  | { kind: 'delete-node'; node: string }
  /** a locked block, after the person confirmed its text */
  | { kind: 'delete-locked'; block: string }
  | { kind: 'projection'; var: string; on: boolean }
  | { kind: 'distinct'; on: boolean }
  | { kind: 'limit' | 'offset'; value: number | null };

export type AnyEdit = Edit | Phase2Edit;

/** Parse a text, as the browser module does. */
export type Parse = (text: string) => PatternAnswer;

// ----------------------------------------------------------------- identities ------

/** The keys of the filters of a pattern in text order: `f:` a hash of the text and the
 * index among filters with that hash. */
export function filterKeys(p: PatternOk): { key: string; filter: Filter; group: Group }[] {
  const seen = new Map<string, number>();
  return filtersOf(p).map(({ filter, group }) => {
    const h = hash(normalized(filter.text));
    const n = seen.get(h) ?? 0;
    seen.set(h, n + 1);
    return { key: `f:${h}:${n}`, filter, group };
  });
}

/** The locked blocks of the WHERE group with their keys, as the drawing keys them. */
export function lockedBlocks(p: PatternOk): { key: string; block: Locked }[] {
  const inner = lockedOf(p);
  const keys = lockedKeys([...inner, ...p.locked]);
  return inner.map((block, i) => ({ key: keys[i], block }));
}

/** The variables of the pattern in the order they first appear: in triples, then those
 * locked blocks bind. */
export function patternVars(p: PatternOk): string[] {
  const out: string[] = [];
  const add = (v: string) => {
    if (!out.includes(v)) out.push(v);
  };
  const at: { at: number; v: string }[] = [];
  for (const { triple: t } of triplesOf(p)) {
    if (t.s.kind === 'var') at.push({ at: t.sRange.from, v: t.s.value });
    if (t.p.kind === 'var') at.push({ at: t.pRange.from, v: t.p.value });
    if (t.o.kind === 'var') at.push({ at: t.oRange.from, v: t.o.value });
  }
  for (const b of lockedOf(p)) for (const v of b.binds ?? b.vars) at.push({ at: b.range.from, v });
  at.sort((a, b) => a.at - b.at).forEach((x) => add(x.v));
  return out;
}

/** The variables triples or locked blocks bind. */
function boundVars(p: PatternOk): Set<string> {
  const out = new Set<string>();
  for (const { triple: t } of triplesOf(p))
    for (const term of [t.s, t.o]) if (term.kind === 'var') out.add(term.value);
  for (const { triple: t } of triplesOf(p)) if (t.p.kind === 'var') out.add(t.p.value);
  for (const b of lockedOf(p)) for (const v of b.binds ?? b.vars) out.add(v);
  return out;
}

/**
 * The variables a locked block uses that nothing binds (§4.2 rule 4): mentioned by the
 * block, not bound by it, by a triple or by another block. A deletion on the canvas may
 * leave a block in this state, which the drawing marks.
 */
export function unboundUses(p: PatternOk, block: Locked): string[] {
  const own = new Set(block.binds ?? block.vars);
  const others = new Set<string>();
  for (const { triple: t } of triplesOf(p)) {
    for (const term of [t.s, t.o]) if (term.kind === 'var') others.add(term.value);
    if (t.p.kind === 'var') others.add(t.p.value);
  }
  for (const b of [...lockedOf(p), ...p.modifiers, ...p.locked])
    if (b !== block) for (const v of b.binds ?? []) others.add(v);
  for (const item of p.projection?.items ?? []) if (item.expression) others.add(item.var);
  return block.vars.filter((v) => !own.has(v) && !others.has(v));
}

// ---------------------------------------------------------------- checks ------

const VARNAME = /^[\p{L}\p{N}_][\p{L}\p{N}_·̀-ͯ‿-⁀]*$/u;

/** A variable name as a person types it, with or without `?`, checked. */
export function checkVarName(name: string): string {
  const n = name.trim().replace(/^[?$]/, '');
  if (!VARNAME.test(n)) throw new EditError(`"${name.trim()}" is not a variable name`);
  return n;
}

const PATH_STEP = String.raw`\^?(?:<[^<>"{}|^\x60\\\s]*>|(?:[\p{L}](?:[\p{L}\p{N}_.\-]*[\p{L}\p{N}_\-])?)?:(?:[\p{L}\p{N}_:%\-](?:[\p{L}\p{N}_:%.\-]*[\p{L}\p{N}_:%\-])?)?|a)(?:[+*?]|\{\d+(?:,\d*)?\})?`;
const PATH_RE = new RegExp(`^${PATH_STEP}(?:\\s*[/|]\\s*${PATH_STEP})*$`, 'u');

/** A predicate as the person types it: a term of `checkTerm`, or a simple path of IRIs
 * with `^`, `+`, `*`, `?`, `{n,m}`, `/` and `|`. Whether the canvas still edits the path
 * is left to the parse of the result (R3). */
export function checkPredicate(text: string): string {
  const t = text.trim();
  try {
    return checkTerm(t, 'p');
  } catch (e) {
    if (PATH_RE.test(t)) return t;
    throw e;
  }
}

/** The path forms of an IRI predicate the panel offers (§2.2 Path). */
export function pathForms(p: string): { label: string; text: string }[] {
  const base = p.replace(/^\^/, '').replace(/(?:[+*?]|\{\d+(?:,\d*)?\})$/, '');
  return [
    { label: 'one step', text: base },
    { label: 'one or more', text: `${base}+` },
    { label: 'zero or more', text: `${base}*` },
    { label: 'zero or one', text: `${base}?` },
    { label: 'inverse', text: `^${base}` },
  ];
}

/** The prefixes the terms of an edit use that neither the query nor its declarations
 * declare, refused with the first. */
function checkPrefixes(p: PatternOk, terms: string[], declare: Prefixes) {
  for (const prefix of terms.flatMap(prefixesIn).concat(terms.flatMap(pathPrefixes)))
    if (
      !p.prologue.prefixes.some((x) => x.prefix === prefix) &&
      !declare.some((x) => x.prefix === prefix)
    )
      throw new EditError(`The prefix "${prefix}:" is not declared`);
}

/** The prefixes of the prefixed names in a path or a filter's expression. */
export function pathPrefixes(text: string): string[] {
  const out: string[] = [];
  // strings and IRIs hold no prefixed names
  const bare = text.replace(/"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|<[^<>\s]*>/g, ' ');
  for (const m of bare.matchAll(
    /(?<![\p{L}\p{N}_?$:.-])([\p{L}](?:[\p{L}\p{N}_.-]*[\p{L}\p{N}_-])?)?:(?=[\p{L}\p{N}_:%]|\s|$)/gu,
  ))
    if (!out.includes(m[1] ?? '')) out.push(m[1] ?? '');
  return out;
}

const prefixLines = (declare: Prefixes): Change[] =>
  declare.map(({ prefix, iri }) => ({ from: 0, to: 0, insert: `PREFIX ${prefix}: <${iri}>\n` }));

// ------------------------------------------------------------------ edits ------

/**
 * The changes an edit makes to `text`, whose pattern is `p`. The kinds of the splice core
 * go to `edit` in `pattern.ts`. Deleting a node needs `parse`, since it deletes one triple
 * at a time and reads the pattern again between them, and the steps are composed into one
 * list of changes in the coordinates of `text`.
 */
export function editAny(text: string, p: PatternOk, e: AnyEdit, parse?: Parse): Change[] {
  switch (e.kind) {
    case 'add':
    case 'delete':
    case 'optional':
      return coreEdit(text, p, e);
    case 'add-many':
      return addMany(text, p, e);
    case 'predicate':
      return predicate(text, p, e);
    case 'filter-add':
      return filterAdd(text, p, e);
    case 'filter-edit': {
      const f = filterKeys(p).find((x) => x.key === e.filter);
      if (!f) throw new EditError('The pattern changed, select the filter again');
      const declare = declarations(p, e);
      for (const prefix of pathPrefixes(e.expr)) checkDeclared(p, prefix, declare);
      return [
        { from: f.filter.range.from, to: f.filter.range.to, insert: `FILTER(${e.expr.trim()})` },
        ...prefixLines(declare),
      ];
    }
    case 'filter-delete': {
      const f = filterKeys(p).find((x) => x.key === e.filter);
      if (!f) throw new EditError('The pattern changed, select the filter again');
      return optionalRemoval(text, f.filter.range, f.filter.dot);
    }
    case 'rename':
      return rename(p, e);
    case 'delete-locked': {
      const b = lockedBlocks(p).find((x) => x.key === e.block);
      if (!b) throw new EditError('The pattern changed, select the block again');
      return optionalRemoval(text, b.block.range, b.block.dot);
    }
    case 'delete-node':
      if (!parse) throw new Error('deleting a node needs the parser');
      return deleteNode(text, p, e.node, parse);
    case 'projection':
      return projection(text, p, e.var, e.on);
    case 'distinct':
      return distinct(text, p, e.on);
    case 'limit':
    case 'offset':
      return modifier(text, p, e.kind, e.value);
  }
}

function checkDeclared(p: PatternOk, prefix: string, declare: Prefixes) {
  if (
    !p.prologue.prefixes.some((x) => x.prefix === prefix) &&
    !declare.some((x) => x.prefix === prefix)
  )
    throw new EditError(`The prefix "${prefix}:" is not declared`);
}

function addMany(
  text: string,
  p: PatternOk,
  e: { triples: Words[]; declare?: Prefixes },
): Change[] {
  if (!p.root) throw new EditError('this query has no WHERE group to edit');
  if (e.triples.length === 0) throw new EditError('nothing to add');
  const declare = declarations(p, e);
  const words = e.triples.map((t) => ({
    s: checkTerm(t.s, 's'),
    p: checkPredicate(t.p),
    o: checkTerm(t.o, 'o'),
  }));
  checkPrefixes(
    p,
    words.flatMap((w) => [w.s, w.o]),
    declare,
  );
  for (const w of words) for (const x of pathPrefixes(w.p)) checkDeclared(p, x, declare);
  // the group of the first subject's patterns, as one add would choose
  const home = homeOf(p, words[0].s, declare);
  const g = home ? home.path[home.path.length - 1] : p.root;
  const binds = new Set<string>();
  for (const w of words)
    for (const v of bindsOf(
      resolveTerm(w.s, p, declare),
      w.p.startsWith('?') || w.p.startsWith('$')
        ? { kind: 'var', value: w.p.slice(1) }
        : { kind: 'iri', value: w.p },
      resolveTerm(w.o, p, declare),
    ))
      binds.add(v);
  const lines = words.map((w) => statement(w.s, w.p, w.o));
  const fragment = lines.join(`\n${itemIndent(text, g)}`);
  return [...insertItem(text, g, anchor(g, binds), fragment, true), ...prefixLines(declare)];
}

function predicate(
  text: string,
  p: PatternOk,
  e: { edge: string; p: string; declare?: Prefixes },
): Change[] {
  const placed = resolveEdit(p, e.edge);
  if (!placed) throw new EditError('The pattern changed, select the edge again');
  const np = checkPredicate(e.p);
  const declare = declarations(p, e);
  for (const x of [...prefixesIn(np), ...pathPrefixes(np)]) checkDeclared(p, x, declare);
  const { triple, path } = placed;
  if (np === triple.p.text) throw new EditError(`The predicate is ${np} already`);
  const g = path[path.length - 1];
  const entry = g.items.filter(
    (i): i is Triple => i.type === 'triple' && i.stmt === triple.stmt && i.entry === triple.entry,
  );
  if (entry.length === 1)
    return [
      { from: triple.pRange.from, to: triple.pRange.to, insert: np },
      ...prefixLines(declare),
    ];
  // one object of a `,` list: it leaves the list and gets an entry of its own after it
  const k = entry.indexOf(triple);
  const del: Change =
    k < entry.length - 1
      ? { from: triple.objectRange.from, to: entry[k + 1].objectRange.from, insert: '' }
      : { from: entry[k - 1].oRange.to, to: triple.objectRange.to, insert: '' };
  const end = entry[entry.length - 1].oRange.to;
  return merge(del, [
    { from: end, to: end, insert: ` ; ${np} ${triple.o.text}` },
    ...prefixLines(declare),
  ]);
}

function filterAdd(
  text: string,
  p: PatternOk,
  e: { var: string; expr: string; declare?: Prefixes },
): Change[] {
  const v = checkVarName(e.var);
  const declare = declarations(p, e);
  for (const x of pathPrefixes(e.expr)) checkDeclared(p, x, declare);
  // after the last triple that binds the variable, in that triple's group
  let last: Placed | undefined;
  for (const t of triplesOf(p)) {
    const { s, p: pr, o } = t.triple;
    if (
      (s.kind === 'var' && s.value === v) ||
      (o.kind === 'var' && o.value === v) ||
      (pr.kind === 'var' && pr.value === v)
    )
      last = t;
  }
  if (!last) throw new EditError(`No triple binds ?${v}, so a filter on it has nothing to test`);
  const g = last.path[last.path.length - 1];
  // the last triple of that statement, whose range is the statement's
  const item = g.items
    .filter((i): i is Triple => i.type === 'triple' && i.stmt === last!.triple.stmt)
    .at(-1)!;
  return [
    ...insertItem(text, g, item as Item, `FILTER(${e.expr.trim()})`, false),
    ...prefixLines(declare),
  ];
}

/** The occurrences a rename changes: those of the editable subset, and with `locked` those
 * in locked blocks, modifiers, the clauses outside WHERE and projection expressions. */
export function renameReach(
  p: PatternOk,
  from: string,
): { editable: Range[]; locked: { text: string; ranges: Range[] }[] } {
  const editable: Range[] = [];
  const add = (r: Range) => {
    if (!editable.some((x) => x.from === r.from)) editable.push(r);
  };
  for (const { triple: t } of triplesOf(p)) {
    if (t.s.kind === 'var' && t.s.value === from) add(t.sRange);
    if (t.o.kind === 'var' && t.o.value === from) add(t.oRange);
    if (t.p.kind === 'var' && t.p.value === from) add(t.pRange);
  }
  for (const { filter } of filtersOf(p))
    if (filter.expr) for (const r of filter.varRefs ?? []) if (r.name === from) add(r.range);
  for (const item of p.projection?.items ?? [])
    if (!item.expression && item.var === from) add(item.range);
  const locked: { text: string; ranges: Range[] }[] = [];
  const blocks: { text: string; refs: { name: string; range: Range }[] }[] = [
    ...filtersOf(p)
      .filter(({ filter }) => !filter.expr)
      .map(({ filter }) => ({ text: filter.text, refs: filter.varRefs ?? [] })),
    ...[...lockedOf(p), ...p.modifiers, ...p.locked].map((b) => ({
      text: b.text,
      refs: b.varRefs ?? [],
    })),
    ...(p.projection?.items ?? [])
      .filter((i) => i.expression)
      .map((i) => ({ text: '', refs: i.varRefs })),
  ];
  for (const b of blocks) {
    const ranges = b.refs.filter((r) => r.name === from).map((r) => r.range);
    if (ranges.length) locked.push({ text: b.text, ranges });
  }
  return { editable, locked };
}

function rename(p: PatternOk, e: { from: string; to: string; locked: boolean }): Change[] {
  const from = checkVarName(e.from);
  const to = checkVarName(e.to);
  if (from === to) throw new EditError('That is the same name');
  const all = new Set([...patternVars(p), ...lockedOf(p).flatMap((b) => b.vars)]);
  for (const { filter } of filtersOf(p)) for (const v of filter.vars) all.add(v);
  if (all.has(to)) throw new EditError(`?${to} is already in the query`);
  const reach = renameReach(p, from);
  const ranges = [...reach.editable, ...(e.locked ? reach.locked.flatMap((b) => b.ranges) : [])];
  if (ranges.length === 0) throw new EditError(`?${from} is not in the pattern`);
  // a token keeps its sigil, `?` or `$`
  return ranges.map((r) => ({ from: r.from + 1, to: r.to, insert: to }));
}

/** The node keys of a triple's ends, as the drawing keys them. */
function touches(t: Placed, node: string, p: PatternOk): boolean {
  if (termKey(t.triple.s) === node) return true;
  if (t.triple.o.kind !== 'literal') return termKey(t.triple.o) === node;
  // a literal's key holds its occurrence: find it through the drawing
  return drawing(p).edges.some((x) => x.triple?.triple === t.triple && x.target === node);
}

function deleteNode(text: string, p: PatternOk, node: string, parse: Parse): Change[] {
  let cur = text;
  let q = p;
  let set: ChangeSet | null = null;
  const step = (changes: Change[]) => {
    const cs = ChangeSet.of(
      changes.map((c) => ({ from: c.from, to: c.to, insert: c.insert })),
      cur.length,
    );
    set = set ? set.compose(cs) : cs;
    cur = applyChanges(cur, changes);
    const a = parse(cur);
    if (!a.ok) throw new EditError(`Deleting the node would break the query: ${a.error}`);
    q = a;
  };
  const touched = triplesOf(p).filter((t) => touches(t, node, p)).length;
  if (touched === 0) throw new EditError('No triple uses that node');
  for (let i = 0; i < 1000; i++) {
    const t = triplesOf(q).find((x) => touches(x, node, q));
    if (!t) break;
    // the same removal as an edge's, for a class subtitle too
    step([{ ...removalRange(cur, t), insert: '' }]);
  }
  // the filters left testing a variable that the deleted triples bound and nothing
  // binds now
  const was = boundVars(p);
  for (let i = 0; i < 1000; i++) {
    const bound = boundVars(q);
    const f = filterKeys(q).find(({ filter }) =>
      filter.vars.some((x) => was.has(x) && !bound.has(x)),
    );
    if (!f) break;
    step(optionalRemoval(cur, f.filter.range, f.filter.dot));
  }
  const out: Change[] = [];
  (set as ChangeSet | null)?.iterChanges((fromA, toA, _fromB, _toB, inserted) =>
    out.push({ from: fromA, to: toA, insert: inserted.toString() }),
  );
  return out;
}

function projection(text: string, p: PatternOk, v: string, on: boolean): Change[] {
  const pr = p.projection;
  if (!pr || !pr.keyword) throw new EditError('Only a SELECT query has a projection to change');
  const name = checkVarName(v);
  if (pr.star) {
    if (on) throw new EditError(`?${name} is already projected by *`);
    const rest = patternVars(p).filter((x) => x !== name);
    if (rest.length === 0) throw new EditError('A query projects at least one variable');
    const star = pr.starRange!;
    return [{ from: star.from, to: star.to, insert: rest.map((x) => `?${x}`).join(' ') }];
  }
  const items = pr.items ?? [];
  if (on) {
    if (items.some((i) => i.var === name)) throw new EditError(`?${name} is already projected`);
    const end = items.length
      ? items[items.length - 1].range.to
      : (pr.distinctRange ?? pr.keyword).to;
    return [{ from: end, to: end, insert: ` ?${name}` }];
  }
  const k = items.findIndex((i) => i.var === name && !i.expression);
  if (k < 0) throw new EditError(`?${name} is not projected as a plain variable`);
  if (items.length === 1) throw new EditError('A query projects at least one variable');
  // the item with the space before it
  const from = k > 0 ? items[k - 1].range.to : (pr.distinctRange ?? pr.keyword).to;
  const to = items[k].range.to;
  if (k === 0) return [{ from: items[0].range.from, to: items[1].range.from, insert: '' }];
  return [{ from, to, insert: '' }];
}

function distinct(text: string, p: PatternOk, on: boolean): Change[] {
  const pr = p.projection;
  if (!pr || !pr.keyword) throw new EditError('Only a SELECT query has DISTINCT');
  if (on) {
    if (pr.distinctRange) throw new EditError('The query is DISTINCT already');
    return [{ from: pr.keyword.to, to: pr.keyword.to, insert: ' DISTINCT' }];
  }
  const r = pr.distinctRange;
  if (!r || !pr.distinct) throw new EditError('The query is not DISTINCT');
  const spaces = /^[ \t]*/.exec(text.slice(r.to))![0].length;
  return [{ from: r.from, to: r.to + spaces, insert: '' }];
}

function modifier(
  text: string,
  p: PatternOk,
  kind: 'limit' | 'offset',
  value: number | null,
): Change[] {
  const word = kind.toUpperCase();
  if (value !== null && (!Number.isSafeInteger(value) || value < 0))
    throw new EditError(`${word} takes a whole number`);
  const cur = p[kind];
  if (cur) {
    if (value === null) {
      // the clause with the spaces or line break before it
      const before = /[ \t]*(\r?\n)?[ \t]*$/.exec(text.slice(0, cur.range.from))![0].length;
      return [{ from: cur.range.from - before, to: cur.range.to, insert: '' }];
    }
    return [{ from: cur.range.from, to: cur.range.to, insert: `${word} ${value}` }];
  }
  if (value === null) throw new EditError(`The query has no ${word}`);
  if (!p.root) throw new EditError('This query has no WHERE group');
  // after the group, GROUP BY, HAVING, ORDER BY and the other number, before a VALUES
  let at = p.root.range.to;
  for (const m of p.modifiers) if (m.kind !== 'VALUES') at = Math.max(at, m.range.to);
  for (const x of [p.limit, p.offset]) if (x) at = Math.max(at, x.range.to);
  return [{ from: at, to: at, insert: `\n${word} ${value}` }];
}

// -------------------------------------------------------------- agreement ------

/**
 * Whether the parse after an edit agrees with it (R3) for every kind: the triples, the
 * filters and the locked blocks are those of the parse before, changed exactly as the
 * edit says, and the projection and modifiers likewise.
 */
export function agreesWith(before: PatternOk, e: AnyEdit, after: PatternOk): boolean {
  if (e.kind === 'add' || e.kind === 'delete' || e.kind === 'optional')
    return agrees(before, e, after) && sameLocked(before, after);
  const triples = (q: PatternOk) =>
    triplesOf(q)
      .map((t) => `${signature(t.triple.s, t.triple.p, t.triple.o)}|${t.path.length > 1}`)
      .sort();
  const filters = (q: PatternOk) =>
    filtersOf(q)
      .map(({ filter }) => normalized(filter.text))
      .sort();
  const locked = (q: PatternOk) =>
    lockedOf(q)
      .map((b) => b.text)
      .sort();
  const eq = (a: string[], b: string[]) => a.length === b.length && a.every((x, i) => x === b[i]);
  const same = (pick: (q: PatternOk) => string[]) => eq(pick(before), pick(after));
  const sameMods = () =>
    eq(
      [before.limit?.value, before.offset?.value].map(String),
      [after.limit?.value, after.offset?.value].map(String),
    ) &&
    eq(
      before.modifiers.map((m) => m.text),
      after.modifiers.map((m) => m.text),
    );
  const sameProj = () =>
    JSON.stringify([
      before.projection?.vars,
      before.projection?.star,
      before.projection?.distinct,
    ]) ===
    JSON.stringify([after.projection?.vars, after.projection?.star, after.projection?.distinct]);
  const rest = () => sameMods() && sameProj();
  switch (e.kind) {
    case 'add-many': {
      const want = e.triples.map((w) =>
        signature({ text: w.s.trim() }, { text: w.p.trim() }, { text: w.o.trim() }),
      );
      const b = triples(before);
      const a = triples(after);
      const added = minus(a, b)
        .map((x) => x.replace(/\|(true|false)$/, ''))
        .sort();
      return (
        minus(b, a).length === 0 &&
        eq(added, [...want].sort()) &&
        same(filters) &&
        same(locked) &&
        rest()
      );
    }
    case 'predicate': {
      const t = resolveEdit(before, e.edge);
      if (!t) return false;
      const opt = t.path.length > 1;
      const old = `${signature(t.triple.s, t.triple.p, t.triple.o)}|${opt}`;
      const now = `${signature(t.triple.s, { text: e.p.trim() }, t.triple.o)}|${opt}`;
      return (
        eq(minus(triples(before), triples(after)), [old]) &&
        eq(minus(triples(after), triples(before)), [now]) &&
        same(filters) &&
        same(locked) &&
        rest()
      );
    }
    case 'filter-add':
    case 'filter-edit':
    case 'filter-delete': {
      const b = filters(before);
      const a = filters(after);
      const text = e.kind === 'filter-delete' ? null : normalized(`FILTER(${e.expr.trim()})`);
      const old =
        e.kind === 'filter-add'
          ? null
          : (filterKeys(before).find((x) => x.key === e.filter)?.filter.text ?? '');
      const removed = minus(b, a);
      const added = minus(a, b);
      const ok =
        eq(removed, old === null ? [] : [normalized(old)]) &&
        eq(added, text === null ? [] : [text]);
      return (
        (ok ||
          // an edit that writes the same text back changes nothing
          (e.kind === 'filter-edit' && text === normalized(old ?? '') && eq(a, b))) &&
        same(triples) &&
        same(locked) &&
        rest()
      );
    }
    case 'rename': {
      const swap = (s: string) =>
        s.replace(/[?$]([\p{L}\p{N}_·̀-ͯ‿-⁀]+)/gu, (m, n) =>
          n === e.from.replace(/^[?$]/, '') ? `${m[0]}${e.to.replace(/^[?$]/, '')}` : m,
        );
      const lockedNow = e.locked ? locked(before).map(swap).sort() : locked(before);
      return (
        eq(triples(before).map(swap).sort(), triples(after)) &&
        eq(lockedNow, locked(after)) &&
        filtersOf(before).length === filtersOf(after).length
      );
    }
    case 'delete-node': {
      const gone = triplesOf(before)
        .filter((t) => touches(t, e.node, before))
        .map((t) => `${signature(t.triple.s, t.triple.p, t.triple.o)}|${t.path.length > 1}`)
        .sort();
      return (
        eq(minus(triples(before), triples(after)).sort(), gone) &&
        minus(triples(after), triples(before)).length === 0 &&
        same(locked) &&
        minus(filters(after), filters(before)).length === 0
      );
    }
    case 'delete-locked': {
      const b = lockedBlocks(before).find((x) => x.key === e.block);
      return (
        !!b &&
        eq(minus(locked(before), locked(after)), [b.block.text]) &&
        minus(locked(after), locked(before)).length === 0 &&
        same(triples) &&
        same(filters)
      );
    }
    case 'projection': {
      const name = e.var.replace(/^[?$]/, '');
      const was = before.projection?.star ? patternVars(before) : (before.projection?.vars ?? []);
      const want = e.on ? [...was, name] : was.filter((x) => x !== name);
      const now = after.projection?.star ? patternVars(after) : (after.projection?.vars ?? []);
      return eq(want, now) && same(triples) && same(filters) && same(locked) && sameMods();
    }
    case 'distinct':
      return (
        after.projection?.distinct === e.on &&
        same(triples) &&
        same(filters) &&
        same(locked) &&
        sameMods()
      );
    case 'limit':
    case 'offset': {
      const other = e.kind === 'limit' ? 'offset' : 'limit';
      return (
        (after[e.kind]?.value ?? null) === e.value &&
        before[other]?.value === after[other]?.value &&
        same(triples) &&
        same(filters) &&
        same(locked) &&
        sameProj()
      );
    }
  }
}

/** The locked blocks are byte-identical. */
function sameLocked(a: PatternOk, b: PatternOk): boolean {
  const x = lockedOf(a).map((l) => l.text);
  const y = lockedOf(b).map((l) => l.text);
  return x.length === y.length && x.every((t, i) => t === y[i]);
}

/** The items of `a` beyond those of `b`, counting repeats. */
function minus(a: string[], b: string[]): string[] {
  const left = new Map<string, number>();
  for (const x of b) left.set(x, (left.get(x) ?? 0) + 1);
  const out: string[] = [];
  for (const x of a) {
    const n = left.get(x) ?? 0;
    if (n > 0) left.set(x, n - 1);
    else out.push(x);
  }
  return out;
}

/** What an edit of Phase 2 is announced as. */
export function describeEdit(e: Phase2Edit): string {
  switch (e.kind) {
    case 'predicate':
      return `Changed the predicate to ${e.p.trim()}`;
    case 'add-many':
      return `Added ${e.triples.map((t) => `${t.s} ${t.p} ${t.o}`).join(' and ')}`;
    case 'filter-add':
      return `Added FILTER(${e.expr.trim()})`;
    case 'filter-edit':
      return `Changed the filter to FILTER(${e.expr.trim()})`;
    case 'filter-delete':
      return 'Deleted the filter';
    case 'rename':
      return `Renamed ?${e.from.replace(/^[?$]/, '')} to ?${e.to.replace(/^[?$]/, '')}`;
    case 'delete-node':
      return 'Deleted the node and its triples';
    case 'delete-locked':
      return 'Deleted the locked block';
    case 'projection':
      return `${e.on ? 'Projected' : 'Stopped projecting'} ?${e.var.replace(/^[?$]/, '')}`;
    case 'distinct':
      return e.on ? 'Made the query DISTINCT' : 'Removed DISTINCT';
    case 'limit':
    case 'offset':
      return e.value === null
        ? `Removed ${e.kind.toUpperCase()}`
        : `Set ${e.kind.toUpperCase()} ${e.value}`;
  }
}

/**
 * The query **Count matches** runs (§2.2): the prologue, `SELECT (COUNT(*) AS ?n)`, the
 * dataset clauses and the WHERE group, as written.
 */
export function countQuery(text: string, p: PatternOk): string {
  if (!p.root) throw new EditError('This query has no WHERE group to count');
  const prologue = text.slice(0, p.queryStart ?? 0);
  const from = p.locked
    .filter((b) => b.kind === 'FROM')
    .map((b) => `${b.text}\n`)
    .join('');
  const where = text.slice(p.root.range.from, p.root.range.to);
  return `${prologue}${prologue && !prologue.endsWith('\n') ? '\n' : ''}SELECT (COUNT(*) AS ?n)\n${from}WHERE ${where}`;
}
