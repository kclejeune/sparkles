// The plan drawn on the query's pattern (spec U04 §11). The server names, on each plan
// node that reads them, the triple patterns and paths of the query (`patterns`), the
// variables of a filter (`filterVars`) and those of a join (`joinVars`). This module
// matches them with the triples, filters and variables of U08's pattern graph and turns
// the plan's numbers into what the canvas draws: a label and a width per edge, the rows
// that reached and passed each filter chip, numbered join badges on variables, and the
// plan's patterns that match nothing on the canvas, so that nothing is hidden.
//
// Matching is by the three terms after prefix expansion and by the occurrence index among
// equal patterns. The parser writes a sequence as its steps joined by a blank node and an
// outer inverse as the path inside it with its ends swapped, so an edge with such a path
// is matched step by step.

import { planOf, type CursorPlan, type PlanNode } from './api';
import { fmtCompact, fmtInt, fmtMs } from './format';
import {
  filtersOf,
  hash,
  normalized,
  resolveIri,
  termKey,
  triplesOf,
  type Drawing,
  type PathExpr,
  type PatternOk,
  type Placed,
  type Term,
  type Triple,
} from './pattern';

/** A pattern of the query as a plan node names it (`PlanPattern` of the API). */
export type PlanPattern = { s: string; p: string; o: string; graph?: string; occurrence: number };

/** A plan node with the members of §11.3. */
type Refs = { patterns?: PlanPattern[]; filterVars?: string[]; joinVars?: string[] };

/** One operator of the plan, flattened. */
export type Op = {
  id: string;
  node: PlanNode & Refs;
  depth: number;
  /** its time without its children's */
  self: number;
  /** its place in a walk that visits children first: the order the plan runs in */
  post: number;
};

/** A plan in preorder, with ids (a plan in format 2 has none, so they are derived). */
export function flatten(plan: PlanNode | CursorPlan): Op[] {
  const out: Op[] = [];
  let post = 0;
  // iterative, since a plan can nest thousands of levels deep
  const stack: { n: PlanNode; id: string; depth: number; at: number; i: number }[] = [];
  const root = planOf(plan);
  const push = (n: PlanNode, id: string, depth: number) => {
    const self = Math.max(0, n.timeMs - n.children.reduce((a, c) => a + (c.timeMs || 0), 0));
    out.push({ id, node: n as PlanNode & Refs, depth, self, post: -1 });
    stack.push({ n, id, depth, at: out.length - 1, i: 0 });
  };
  push(root, root.id ?? '0', 0);
  while (stack.length) {
    const top = stack[stack.length - 1];
    if (top.i < top.n.children.length) {
      const c = top.n.children[top.i];
      const id = c.id ?? `${top.id}.${top.i}`;
      top.i++;
      push(c, id, top.depth + 1);
    } else {
      out[top.at].post = post++;
      stack.pop();
    }
  }
  return out;
}

/** Whether `id` is `of` or below it. */
export const under = (id: string, of: string) => id === of || id.startsWith(of + '.');

/**
 * The ratio of actual to estimated rows when one is over ten times the other, else null.
 * A hidden estimate (`-1`), a node that stopped early or did not run, and partial counts
 * are not misestimates. PlanView uses the same rule.
 */
export function misestimate(n: PlanNode, executed: boolean): number | null {
  if (!executed || n.actualRows < 0 || n.estimatedRows < 0) return null;
  if (n.stoppedEarly || n.skipped || n.complete === false) return null;
  const a = Math.max(n.actualRows, 1);
  const e = Math.max(n.estimatedRows, 1);
  const r = a > e ? a / e : e / a;
  return r >= 10 ? r : null;
}

/** A misestimate as text: `×1,338` when the rows were more, `÷40` when fewer. */
export function ratioText(n: PlanNode, executed: boolean): string | null {
  const r = misestimate(n, executed);
  if (r == null) return null;
  return `${n.actualRows > n.estimatedRows ? '×' : '÷'}${fmtInt(r)}`;
}

/** An edge's width: it grows with the logarithm of its rows. */
export function width(rows: number): number {
  if (!(rows >= 0)) return 1.4;
  return Math.min(9, 1.4 + 1.25 * Math.log10(1 + rows));
}

// ------------------------------------------------------------ terms as N-Triples ------

const XSD = 'http://www.w3.org/2001/XMLSchema#';

function unescape(s: string): string {
  return s.replace(/\\(u[0-9a-fA-F]{4}|U[0-9a-fA-F]{8}|.)/g, (_, e: string) => {
    if (e[0] === 'u' || e[0] === 'U') return String.fromCodePoint(parseInt(e.slice(1), 16));
    return { t: '\t', n: '\n', r: '\r', b: '\b', f: '\f' }[e] ?? e;
  });
}

const ESCAPES: Record<string, string> = {
  '\b': '\\b',
  '\t': '\\t',
  '\n': '\\n',
  '\f': '\\f',
  '\r': '\\r',
  '"': '\\"',
  '\\': '\\\\',
};

/** A string in quotes as OxRDF writes it in N-Triples (`print_quoted_str`). */
function quote(s: string): string {
  // eslint-disable-next-line no-control-regex
  const body = s.replace(/[\u0000-\u001f"\\\u007f\ufffe\uffff]/g, (c) => {
    return ESCAPES[c] ?? `\\u${c.charCodeAt(0).toString(16).toUpperCase().padStart(4, '0')}`;
  });
  return `"${body}"`;
}

/** The IRI a prefixed name or `<iri>` stands for under the query's prefixes and BASE. */
function expand(t: string, p: PatternOk): string | null {
  if (t.startsWith('<') && t.endsWith('>')) {
    const iri = t
      .slice(1, -1)
      .replace(/\\u([0-9a-fA-F]{4})|\\U([0-9a-fA-F]{8})/g, (_, a?: string, b?: string) =>
        String.fromCodePoint(parseInt(a ?? b!, 16)),
      );
    return resolveIri(iri, p.prologue.base);
  }
  const i = t.indexOf(':');
  if (i < 0) return null;
  // a later declaration of the same prefix wins, as in the parser
  const pre = p.prologue.prefixes.filter((x) => x.prefix === t.slice(0, i)).pop();
  // a local name's backslash escapes stand for the character after them
  return pre ? pre.iri + t.slice(i + 1).replace(/\\(.)/g, '$1') : null;
}

/** A literal as written in SPARQL, as the server writes it in N-Triples, or null. */
export function literalNt(text: string, p: PatternOk): string | null {
  const t = text.trim();
  if (/^[+-]?\d+$/.test(t)) return `"${t}"^^<${XSD}integer>`;
  if (/^[+-]?\d*\.\d+$/.test(t)) return `"${t}"^^<${XSD}decimal>`;
  if (/^[+-]?(\d+\.?\d*|\.\d+)[eE][+-]?\d+$/.test(t)) return `"${t}"^^<${XSD}double>`;
  if (/^(true|false)$/i.test(t)) return `"${t.toLowerCase()}"^^<${XSD}boolean>`;
  const m = /^("""[\s\S]*"""|'''[\s\S]*'''|"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')(.*)$/.exec(t);
  if (!m) return null;
  const q = m[1];
  const long = q.startsWith('"""') || q.startsWith("'''");
  const body = unescape(long ? q.slice(3, -3) : q.slice(1, -1));
  const rest = m[2].trim();
  if (!rest) return quote(body);
  if (rest.startsWith('@')) return `${quote(body)}${rest.toLowerCase()}`;
  if (rest.startsWith('^^')) {
    const dt = expand(rest.slice(2).trim(), p);
    if (!dt) return null;
    return dt === `${XSD}string` ? quote(body) : `${quote(body)}^^<${dt}>`;
  }
  return null;
}

/** A subject or object of the pattern as the server writes it, or null. */
export function termNt(t: Term, p: PatternOk): string | null {
  switch (t.kind) {
    case 'var':
      return `?${t.value}`;
    case 'iri':
      return `<${t.value}>`;
    case 'bnode':
      return '?_';
    default:
      return literalNt(t.text, p);
  }
}

/** A range modifier in the server's form: `{2}`, `{1,3}` or `{0,}`. */
export function rangeOp(op: string): string {
  const m = /^\{\s*(\d*)\s*(,?)\s*(\d*|\*|\+)?\s*\}$/.exec(op);
  if (!m) return op.replace(/\s+/g, '');
  const [, a, comma, b] = m;
  if (b === '*' && !a) return '{0,}';
  if (b === '+' && !a) return '{1,}';
  // the bounds as numbers, which have no leading zeros and may pass 2^53
  const num = (x: string) => BigInt(x).toString();
  const min = num(a || '0');
  if (!comma) return `{${min}}`;
  if (!b) return `{${min},}`;
  const max = num(b);
  return max === min ? `{${min}}` : `{${min},${max}}`;
}

/** A path as the server writes it in `patterns` (see `path_text` in the engine). */
export function pathNt(e: PathExpr): string {
  const wrap = (x: PathExpr) => (x.kind === 'iri' ? pathNt(x) : `(${pathNt(x)})`);
  switch (e.kind) {
    case 'iri':
      return `<${e.iri}>`;
    case 'seq':
      return e.items
        .map((x) => (x.kind === 'seq' || x.kind === 'alt' ? `(${pathNt(x)})` : pathNt(x)))
        .join('/');
    case 'alt':
      return e.items
        .map((x) => (x.kind === 'seq' || x.kind === 'alt' ? `(${pathNt(x)})` : pathNt(x)))
        .join('|');
    case 'inv':
      return `^${wrap(e.item)}`;
    case 'mod':
      return `${wrap(e.item)}${/^[?*+]$/.test(e.op) ? e.op : rangeOp(e.op)}`;
  }
}

/** The patterns the server names for one triple of the pattern, as `s p o` keys. */
export function stepsOf(t: Placed, p: PatternOk): string[] | null {
  const { triple } = t;
  const s = termNt(triple.s, p);
  const o = termNt(triple.o, p);
  if (s == null || o == null) return null;
  const pr = triple.p;
  if (pr.kind === 'iri') return [`${s} <${pr.value}> ${o}`];
  if (pr.kind === 'var') return [`${s} ?${pr.value} ${o}`];
  const path = pr.path;
  if (!path) return null;
  // as the parser splits a path: an IRI is a triple, an inverse swaps the ends of the path
  // inside it, a sequence is its steps joined by blank nodes, and the rest stay whole
  const out: string[] = [];
  const split = (a: string, e: PathExpr, b: string) => {
    if (e.kind === 'iri') out.push(`${a} <${e.iri}> ${b}`);
    else if (e.kind === 'inv') split(b, e.item, a);
    else if (e.kind === 'seq')
      e.items.forEach((x, i) => split(i === 0 ? a : '?_', x, i === e.items.length - 1 ? b : '?_'));
    else out.push(`${a} ${pathNt(e)} ${b}`);
  };
  split(s, path, o);
  return out;
}

// ----------------------------------------------------------------- the overlay ------

/** The numbers of one operator as the canvas shows them. */
export type Numbers = {
  op: Op;
  est: number;
  rows: number;
  timeMs: number;
  self: number;
  ratio: string | null;
};

export type EdgeStat = Numbers & {
  /** every operator that reads the edge's triple, the shown one first */
  ops: Op[];
  /** the shown operator also reads other triples of the canvas (a star join) */
  shared: number;
};

export type FilterStat = { op: Op; reached: number; passed: number };

export type JoinBadge = { op: Op; n: number; vars: string[]; drawnVars: string[] };

export type Unmatched = { op: Op; pattern: PlanPattern };

export type Overlay = {
  executed: boolean;
  /** by edge id */
  edges: Map<string, EdgeStat>;
  /** by the key of a `?x a C` triple drawn as a class under its node: the node id */
  classes: Map<string, EdgeStat[]>;
  /** by filter key (`f:hash:n`) */
  filters: Map<string, FilterStat>;
  joins: JoinBadge[];
  /** by the id of a locked block's node, when the plan maps the block to one operator */
  locked: Map<string, Numbers>;
  unmatched: Unmatched[];
  /** the operator with the largest share of the time, when the plan ran */
  hot: string | null;
  ops: Op[];
  /** the estimates are hidden from this caller (`-1`) */
  hiddenEstimates: boolean;
};

const numbers = (op: Op, executed: boolean): Numbers => ({
  op,
  est: op.node.estimatedRows,
  rows: executed ? op.node.actualRows : -1,
  timeMs: op.node.timeMs,
  self: op.self,
  ratio: ratioText(op.node, executed),
});

const keyOf = (p: PlanPattern) => `${p.s} ${p.p} ${p.o}`;

/** The operator that evaluates a locked block of each kind (U04 §11.2). */
const LOCKED_OPERATOR: Record<string, string> = {
  BIND: 'Bind',
  MINUS: 'Minus',
  UNION: 'Union',
  SERVICE: 'Service',
  LATERAL: 'Lateral',
  LET: 'Let',
  UNFOLD: 'Unfold',
  SEMIJOIN: 'SemiJoin',
  ANTIJOIN: 'AntiJoin',
};

/** The plan's numbers matched with a drawing of the same query text. */
export function overlay(
  a: PatternOk,
  d: Drawing,
  plan: PlanNode | CursorPlan,
  executed: boolean,
): Overlay {
  const ops = flatten(plan);
  // the operators that name each pattern, by `s p o #occurrence`
  const byPattern = new Map<string, { op: Op; pattern: PlanPattern }[]>();
  for (const op of ops)
    for (const pattern of op.node.patterns ?? []) {
      // a pattern inside GRAPH is in a locked block, which draws no edge for it: its key
      // names the graph so that it never matches, and it is listed with the unmatched
      const g = pattern.graph ? ` @${pattern.graph}` : '';
      const k = `${keyOf(pattern)}${g} #${pattern.occurrence}`;
      const list = byPattern.get(k) ?? [];
      list.push({ op, pattern });
      byPattern.set(k, list);
    }
  const matched = new Set<string>();
  // by the triple's place in the text: the drawing may hold another copy of the answer
  // (the pane keeps it in reactive state), so the objects themselves do not compare
  const opsOfTriple = new Map<string, Op[]>();
  const placeOf = (t: Triple) => `${t.stmt}:${t.entry}:${t.object}`;
  const seen = new Map<string, number>();
  for (const t of triplesOf(a)) {
    const steps = stepsOf(t, a);
    if (!steps) continue;
    const found: Op[] = [];
    for (const step of steps) {
      const n = seen.get(step) ?? 0;
      seen.set(step, n + 1);
      const k = `${step} #${n}`;
      for (const x of byPattern.get(k) ?? []) {
        matched.add(k);
        if (!found.includes(x.op)) found.push(x.op);
      }
    }
    if (found.length) opsOfTriple.set(placeOf(t.triple), found);
  }
  // how many triples of the canvas each operator reads
  const reads = new Map<Op, number>();
  for (const list of opsOfTriple.values())
    for (const op of list) reads.set(op, (reads.get(op) ?? 0) + 1);
  const stat = (list: Op[]): EdgeStat => {
    // the operator nearest the root shows, then the first in the plan
    const sorted = [...list].sort((x, y) => x.depth - y.depth || ops.indexOf(x) - ops.indexOf(y));
    const op = sorted[0];
    return { ...numbers(op, executed), ops: sorted, shared: (reads.get(op) ?? 1) - 1 };
  };
  const edges = new Map<string, EdgeStat>();
  const classes = new Map<string, EdgeStat[]>();
  for (const e of d.edges) {
    const list = e.triple && opsOfTriple.get(placeOf(e.triple.triple));
    if (list) edges.set(e.id, stat(list));
  }
  const drawnPlaces = new Set(d.edges.flatMap((e) => (e.triple ? [placeOf(e.triple.triple)] : [])));
  for (const t of triplesOf(a)) {
    const list = opsOfTriple.get(placeOf(t.triple));
    if (!list || drawnPlaces.has(placeOf(t.triple))) continue;
    // a class drawn under its subject
    const id = termKey(t.triple.s);
    const have = classes.get(id) ?? [];
    have.push(stat(list));
    classes.set(id, have);
  }
  // filters: a chip of the root group takes a filter operator that reads exactly its
  // variables, and then a chip left over takes the one with the fewest variables that holds
  // all of its own, but never one that reads exactly another chip's. A filter in an
  // OPTIONAL becomes its join's condition, and one in a nested group may be planned
  // anywhere, so their chips, and a chip with no operator left, show no numbers rather
  // than another filter's. Each operator serves one chip.
  const filterOps = ops.filter((o) => o.node.filterVars?.length && o.node.operator === 'Filter');
  const varsKey = (vs: string[]) => [...new Set(vs)].sort().join(' ');
  const chips: { key: string; vars: string[] }[] = [];
  const fseen = new Map<string, number>();
  for (const { filter, group } of filtersOf(a)) {
    const fh = hash(normalized(filter.text));
    const fn = fseen.get(fh) ?? 0;
    fseen.set(fh, fn + 1);
    if (!filter.vars.length || group.range.from !== a.root?.range.from) continue;
    chips.push({ key: `f:${fh}:${fn}`, vars: filter.vars });
  }
  const reserved = new Set(chips.map((c) => varsKey(c.vars)));
  const used = new Set<Op>();
  const chosen = new Map<string, Op>();
  for (const c of chips) {
    const op = filterOps.find(
      (o) => !used.has(o) && varsKey(o.node.filterVars!) === varsKey(c.vars),
    );
    if (op) {
      used.add(op);
      chosen.set(c.key, op);
    }
  }
  for (const c of chips) {
    if (chosen.has(c.key)) continue;
    const op = filterOps
      .filter(
        (o) =>
          !used.has(o) &&
          !reserved.has(varsKey(o.node.filterVars!)) &&
          c.vars.every((v) => o.node.filterVars!.includes(v)),
      )
      .sort(
        (x, y) =>
          x.node.filterVars!.length - y.node.filterVars!.length || ops.indexOf(x) - ops.indexOf(y),
      )[0];
    if (op) {
      used.add(op);
      chosen.set(c.key, op);
    }
  }
  const filters = new Map<string, FilterStat>();
  for (const c of chips) {
    const op = chosen.get(c.key);
    if (!op) continue;
    const reached = executed
      ? op.node.children.reduce((s, c) => s + Math.max(0, c.actualRows), 0)
      : -1;
    filters.set(c.key, {
      op,
      reached,
      passed: executed ? op.node.actualRows : -1,
    });
  }
  // joins in the order the plan runs them
  const varIds = new Set(d.nodes.filter((n) => n.kind === 'var').map((n) => n.id));
  const joins: JoinBadge[] = ops
    .filter((o) => o.node.joinVars?.length)
    .sort((x, y) => x.post - y.post)
    .map((op, i) => ({
      op,
      n: i + 1,
      vars: op.node.joinVars!,
      drawnVars: op.node.joinVars!.filter((v) => varIds.has(`v:${v}`)),
    }));
  // a locked block maps to one operator when it is the only block of its kind and the
  // plan has one operator of the matching name
  const locked = new Map<string, Numbers>();
  const blocks = d.nodes.filter((n) => n.kind === 'locked' && n.locked);
  for (const n of blocks) {
    const name = LOCKED_OPERATOR[n.locked!.kind];
    if (!name) continue;
    if (blocks.filter((b) => b.locked!.kind === n.locked!.kind).length !== 1) continue;
    const named = ops.filter((o) => o.node.operator === name);
    if (named.length === 1) locked.set(n.id, numbers(named[0], executed));
  }
  const unmatched: Unmatched[] = [];
  for (const [k, list] of byPattern) if (!matched.has(k)) unmatched.push(...list);
  unmatched.sort((x, y) => ops.indexOf(x.op) - ops.indexOf(y.op));
  let hot: string | null = null;
  if (executed && ops.length) {
    const top = ops.reduce((b, o) => (o.self > b.self ? o : b));
    if (top.self > 0) hot = top.id;
  }
  return {
    executed,
    edges,
    classes,
    filters,
    joins,
    locked,
    unmatched,
    hot,
    ops,
    hiddenEstimates: ops.some((o) => o.node.estimatedRows < 0),
  };
}

// ---------------------------------------------------------------- as text ------

/** An edge's label line: `est 214 · rows 214 ⚠ ×12`, or `214 / 1.2M ⚠` on a phone. */
export function label(s: Numbers, o: { time: boolean; phone: boolean }): string {
  if (o.time) return `${fmtMs(s.self)}${o.phone ? '' : ' own'}`;
  const warn = s.ratio ? ` ⚠${o.phone ? '' : ` ${s.ratio}`}` : '';
  const est = s.est >= 0 ? s.est : null;
  const rows = s.rows >= 0 ? s.rows : null;
  if (o.phone) {
    const parts = [est != null ? fmtCompact(est) : null, rows != null ? fmtCompact(rows) : null];
    return parts.filter((x) => x != null).join(' / ') + warn;
  }
  const parts = [
    est != null ? `est ${fmtInt(est)}` : null,
    rows != null ? `rows ${fmtInt(rows)}` : null,
  ];
  return parts.filter((x) => x != null).join(' · ') + warn;
}

/** What a screen reader hears after an element's own description. */
export function spoken(s: Numbers, time: boolean): string {
  const parts: string[] = [];
  if (time) parts.push(`own time ${fmtMs(s.self)}`);
  if (s.est >= 0) parts.push(`estimated ${fmtInt(s.est)} rows`);
  if (s.rows >= 0) parts.push(`actual ${fmtInt(s.rows)}`);
  if (s.ratio)
    parts.push(
      s.rows > s.est ? 'more than ten times the estimate' : 'under a tenth of the estimate',
    );
  return parts.join(', ');
}

/** A filter chip's line: `14 of 900 passed`. */
export function filterLabel(f: FilterStat, o: { time: boolean; phone: boolean }): string {
  if (o.time) return fmtMs(f.op.self);
  if (f.passed < 0)
    return f.op.node.estimatedRows >= 0 ? `est ${fmtInt(f.op.node.estimatedRows)}` : '';
  return o.phone
    ? `${fmtCompact(f.passed)} / ${fmtCompact(f.reached)}`
    : `${fmtInt(f.passed)} of ${fmtInt(f.reached)} passed`;
}

/** A join badge: `① HashJoin · 214`. */
export function badge(
  j: JoinBadge,
  o: { time: boolean; phone: boolean },
  executed: boolean,
): string {
  const circled = j.n <= 20 ? String.fromCodePoint(0x2460 + j.n - 1) : `(${j.n})`;
  const n = j.op.node;
  const value = o.time
    ? fmtMs(j.op.self)
    : executed && n.actualRows >= 0
      ? fmtCompact(n.actualRows)
      : n.estimatedRows >= 0
        ? `est ${fmtCompact(n.estimatedRows)}`
        : '';
  return o.phone ? `${circled} ${value}` : `${circled} ${n.operator}${value ? ` · ${value}` : ''}`;
}

/** The rule the legend states. */
export const LEGEND =
  '⚠ marks an operator whose actual rows are over ten times its estimate or under a tenth of it.';
