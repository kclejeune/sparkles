// The pattern of a query for the visual query builder (spec U08). The browser module of
// sparkles-fmt answers `pattern(text)` with the query's graph pattern and the UTF-16 range
// of every part (§3.2). This module turns that answer into what the Pattern pane draws,
// gives every element the identity its layout is kept under (§4.4), and computes canvas
// edits as splices of the text (§4.3): lists of `{from, to, insert}` that the editor
// applies as one transaction, so every byte an edit does not name stays as it was.
//
// The text is the only source of truth. Nothing here keeps a model of the query beyond
// one answer, and an edit is a function of the answer, the text and the action.

/** UTF-16 offsets into the text, as CodeMirror counts them. */
export type Range = { from: number; to: number };

export type Term = {
  kind: 'var' | 'iri' | 'bnode' | 'literal';
  /** as written */
  text: string;
  /** a variable's name, a full IRI, a blank node label, or a literal as written */
  value: string;
  range: Range;
};

export type PathExpr =
  | { kind: 'iri'; text: string; iri: string }
  | { kind: 'seq' | 'alt'; items: PathExpr[] }
  | { kind: 'inv'; item: PathExpr }
  | { kind: 'mod'; op: string; item: PathExpr };

export type Pred = {
  kind: 'iri' | 'var' | 'path';
  text: string;
  value: string;
  path: PathExpr | null;
};

export type Triple = {
  type: 'triple';
  s: Term;
  p: Pred;
  o: Term;
  /** the whole statement's range, shared by the triples of a `;` or `,` list */
  range: Range;
  sRange: Range;
  pRange: Range;
  oRange: Range;
  /** the statement's number in the query, and the triple's place in it */
  stmt: number;
  entry: number;
  object: number;
  /** the property list entry with the `;`s after it, the object with its `,` */
  entryRange: Range;
  objectRange: Range;
};

export type FilterForm = { op: string; var: string; args: string[] };

export type Filter = {
  type: 'filter';
  /** the form the canvas understands, or null for a locked chip */
  expr: FilterForm | null;
  vars: string[];
  text: string;
  range: Range;
  /** the `.` that follows it in its group */
  dot?: Range | null;
};

export type Locked = {
  kind: string;
  vars: string[];
  /** exactly the text at `range`, which runs over a comment that ends its line */
  text: string;
  range: Range;
  /** the end of its last token, before that comment */
  end?: number;
  /** the `.` that follows it in its group (a `triples` statement keeps its own inside) */
  dot?: Range | null;
};

export type Group = {
  kind: 'group' | 'optional';
  range: Range;
  /** between the braces */
  inner: Range;
  items: Item[];
};

export type Item =
  | Triple
  | Filter
  | (Group & { type: 'group'; dot?: Range | null })
  | (Locked & { type: 'locked' });

export type PatternOk = {
  ok: true;
  form: 'select' | 'ask' | 'construct' | 'describe';
  /** the last BASE, against which IRIs and prefixes are already resolved */
  prologue: {
    base?: string | null;
    prefixes: { prefix: string; iri: string; range: Range }[];
  };
  /** the WHERE group; null for a DESCRIBE without one */
  root: Group | null;
  projection: {
    vars: string[];
    star: boolean;
    distinct: boolean;
    locked: boolean;
    range: Range;
  } | null;
  limit?: { value: number; range: Range };
  offset?: { value: number; range: Range };
  /** GROUP BY, HAVING, ORDER BY and a trailing VALUES */
  modifiers: Locked[];
  /** FROM clauses, a CONSTRUCT template, the terms of DESCRIBE */
  locked: Locked[];
};

export type PatternError = {
  ok: false;
  /** `syntax`, `update` (an update request) or `bad-request` */
  code: string;
  error: string;
  line: number;
  column: number;
};

export type PatternAnswer = PatternOk | PatternError;

/** One text change, in the coordinates of the text before the edit. */
export type Change = { from: number; to: number; insert: string };

// ------------------------------------------------------------------ the walk ------

/** A triple with the groups around it: `path[0]` is the root, the last its own group.
 * `index` is its place in its group's items, `order` its place in `triplesOf`. */
export type Placed = {
  triple: Triple;
  path: Group[];
  index: number;
  order: number;
};

/** Every triple of the pattern in text order, with its groups. */
export function triplesOf(p: PatternOk): Placed[] {
  const out: Placed[] = [];
  const walk = (g: Group, path: Group[]) => {
    g.items.forEach((item, index) => {
      if (item.type === 'triple')
        out.push({
          triple: item,
          path: [...path, g],
          index,
          order: out.length,
        });
      else if (item.type === 'group') walk(item, [...path, g]);
    });
  };
  if (p.root) walk(p.root, []);
  return out;
}

/** Every locked block of the pattern's groups in text order, with the kind of its group. */
export function lockedOf(p: PatternOk): Locked[] {
  const out: Locked[] = [];
  const walk = (g: Group) => {
    for (const item of g.items) {
      if (item.type === 'locked') out.push(item);
      else if (item.type === 'group') walk(item);
    }
  };
  if (p.root) walk(p.root);
  return out;
}

/** Every filter of the pattern in text order, with its group. */
export function filtersOf(p: PatternOk): { filter: Filter; group: Group }[] {
  const out: { filter: Filter; group: Group }[] = [];
  const walk = (g: Group) => {
    for (const item of g.items) {
      if (item.type === 'filter') out.push({ filter: item, group: g });
      else if (item.type === 'group') walk(item);
    }
  };
  if (p.root) walk(p.root);
  return out;
}

// ------------------------------------------------------------- identity (§4.4) ------

export const RDF_TYPE = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';

/** The layout key of a subject or an object that is not a literal. */
export function termKey(t: Term): string {
  switch (t.kind) {
    case 'var':
      return `v:${t.value}`;
    case 'bnode':
      return `b:${t.value}`;
    default:
      return `i:${t.value}`;
  }
}

/** A short, stable hash (FNV-1a, 32 bits) as 8 hex digits. */
export function hash(s: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0).toString(16).padStart(8, '0');
}

/** A locked block's text with its whitespace collapsed, for its key. */
export const normalized = (text: string) => text.replace(/\s+/g, ' ').trim();

/** The keys of the locked blocks, in order: kind, hash of the text, and the index among
 * blocks with that hash. */
export function lockedKeys(blocks: Locked[]): string[] {
  const seen = new Map<string, number>();
  return blocks.map((b) => {
    const h = `${b.kind}:${hash(normalized(b.text))}`;
    const n = seen.get(h) ?? 0;
    seen.set(h, n + 1);
    return `k:${h}:${n}`;
  });
}

// --------------------------------------------------------------- the drawing ------

export type PNodeKind = 'var' | 'iri' | 'literal' | 'bnode' | 'locked' | 'chip';

export type PNode = {
  /** the layout key */
  id: string;
  kind: PNodeKind;
  label: string;
  /** classes from `?x a C`, and filter chips, under the label */
  subtitle: string[];
  /** the full name, for the panel and the screen reader */
  title: string;
  /** an OPTIONAL hull it sits in, when only the optional part binds it */
  parent?: string;
  /** where the element starts in the text, for text order */
  at: number;
  /** a locked block's or chip's kind and text */
  locked?: Locked;
  /** a variable that only a locked block mentions */
  fromLocked?: boolean;
};

export type PEdge = {
  id: string;
  source: string;
  target: string;
  label: string;
  /** a variable predicate is dashed, a link to a locked block dotted */
  style: 'solid' | 'dashed' | 'dotted';
  optional: boolean;
  triple?: Placed;
  at: number;
};

export type PHull = { id: string; label: string; at: number };

export type Drawing = {
  nodes: PNode[];
  edges: PEdge[];
  hulls: PHull[];
  /** the number of separate parts, apart from locked blocks */
  parts: number;
  triples: number;
};

/** The shortest form of an IRI under the query's prefixes: `ex:Person`, else `<…>`. */
export function compact(iri: string, prefixes: { prefix: string; iri: string }[]): string {
  let best: { prefix: string; iri: string } | undefined;
  for (const p of prefixes)
    if (iri.startsWith(p.iri) && (!best || p.iri.length > best.iri.length)) best = p;
  if (best) {
    const local = iri.slice(best.iri.length);
    if (/^[\p{L}\p{N}_][\p{L}\p{N}_.-]*$/u.test(local) || local === '') {
      if (!local.endsWith('.')) return `${best.prefix}:${local}`;
    }
  }
  return iri === RDF_TYPE ? 'a' : `<${iri}>`;
}

/** A filter form as a chip: `starts with "A"`, `> 2020`. */
export function chipText(f: FilterForm): string {
  const a = f.args.join(', ');
  switch (f.op) {
    case 'contains':
      return `contains ${a}`;
    case 'strstarts':
      return `starts with ${a}`;
    case 'strends':
      return `ends with ${a}`;
    case 'regex':
      return `matches ${a}`;
    case 'langmatches':
      return `language ${a}`;
    case 'bound':
      return 'bound';
    case 'isIRI':
      return 'is an IRI';
    case 'isLiteral':
      return 'is a literal';
    default:
      return `${f.op} ${a}`;
  }
}

const first = (s: string, n = 60) => {
  const line = s.split('\n')[0];
  return line.length > n ? line.slice(0, n - 1) + '…' : line;
};

/**
 * The drawing of a pattern (§2.1): a node per variable, IRI, blank node label and
 * literal, an edge per triple, classes as subtitles, understood filters as chips on
 * their variable, other filters and locked blocks as boxes joined to the variables they
 * share, and a hull per OPTIONAL around the nodes only it binds.
 */
export function drawing(p: PatternOk): Drawing {
  const prefixes = p.prologue.prefixes;
  const nodes = new Map<string, PNode>();
  const edges: PEdge[] = [];
  const hulls: PHull[] = [];
  const placed = triplesOf(p);
  const optionalOf = (t: Placed) => t.path.length > 1;
  // which nodes the required part binds: the rest of an optional's nodes go in its hull
  const required = new Set<string>();
  const literalCount = new Map<string, number>();
  const keyOf = new Map<Triple, { s: string; o: string }>();
  for (const t of placed) {
    const s = termKey(t.triple.s);
    let o: string;
    if (t.triple.o.kind === 'literal') {
      const base = `${s}|${t.triple.p.value}`;
      const n = literalCount.get(base) ?? 0;
      literalCount.set(base, n + 1);
      o = `l:${base}|${n}`;
    } else o = termKey(t.triple.o);
    keyOf.set(t.triple, { s, o });
    if (!optionalOf(t)) {
      required.add(s);
      required.add(o);
    }
  }
  const hullOf = new Map<Group, string>();
  const ensure = (key: string, term: Term, at: number, hull?: string) => {
    if (nodes.has(key)) return nodes.get(key)!;
    const label =
      term.kind === 'var'
        ? `?${term.value}`
        : term.kind === 'iri'
          ? compact(term.value, prefixes)
          : term.kind === 'bnode'
            ? `_:${term.value}`
            : term.text;
    const n: PNode = {
      id: key,
      kind: term.kind,
      label,
      subtitle: [],
      title: term.kind === 'iri' ? term.value : label,
      at,
      ...(hull && !required.has(key) ? { parent: hull } : {}),
    };
    nodes.set(key, n);
    return n;
  };
  const classes = new Map<string, string[]>();
  // the edges between the same two nodes with the same predicate, for the edge ids
  const seen = new Map<string, number>();
  for (const t of placed) {
    const { triple } = t;
    const { s, o } = keyOf.get(triple)!;
    let hull: string | undefined;
    if (optionalOf(t)) {
      const g = t.path[t.path.length - 1];
      hull = hullOf.get(g);
      if (!hull) {
        hull = `h:${hullOf.size}`;
        hullOf.set(g, hull);
        hulls.push({ id: hull, label: 'OPTIONAL', at: g.range.from });
      }
    }
    // `?x a C` in the required part: a class under the node, not an edge
    if (
      !hull &&
      triple.p.kind === 'iri' &&
      triple.p.value === RDF_TYPE &&
      triple.o.kind === 'iri'
    ) {
      ensure(s, triple.s, triple.sRange.from);
      const list = classes.get(s) ?? [];
      list.push(compact(triple.o.value, prefixes));
      classes.set(s, list);
      continue;
    }
    ensure(s, triple.s, triple.sRange.from, hull);
    ensure(o, triple.o, triple.oRange.from, hull);
    const label =
      triple.p.kind === 'iri'
        ? compact(triple.p.value, prefixes)
        : triple.p.kind === 'var'
          ? `?${triple.p.value}`
          : triple.p.text;
    const occurrence = `${s}|${triple.p.text}|${o}`;
    const n = seen.get(occurrence) ?? 0;
    seen.set(occurrence, n + 1);
    edges.push({
      id: `e:${occurrence}|${n}`,
      source: s,
      target: o,
      label,
      style: triple.p.kind === 'var' ? 'dashed' : 'solid',
      optional: !!hull,
      triple: t,
      at: triple.pRange.from,
    });
  }
  for (const [k, list] of classes) nodes.get(k)!.subtitle.push(...list);
  const varNode = (v: string) => nodes.get(`v:${v}`);
  const inner = lockedOf(p);
  // the blocks of the WHERE group, then FROM, a CONSTRUCT template and DESCRIBE's terms
  const blocks = [...inner, ...p.locked];
  lockedKeys(blocks).forEach((id, i) => {
    const b = blocks[i];
    // a block that binds variables (a BIND, a subquery, VALUES …) gets nodes for them, so
    // the reader sees where `?y` comes from; a MINUS binds nothing outside itself, and a
    // clause outside the WHERE group only uses the variables drawn already
    if (b.kind !== 'MINUS' && i < inner.length)
      for (const v of b.vars)
        if (!varNode(v))
          nodes.set(`v:${v}`, {
            id: `v:${v}`,
            kind: 'var',
            label: `?${v}`,
            subtitle: [],
            title: `?${v}`,
            at: b.range.from,
            fromLocked: true,
          });
    nodes.set(id, {
      id,
      kind: 'locked',
      label: `locked · ${b.kind}`,
      subtitle: [first(b.text)],
      title: b.text,
      at: b.range.from,
      locked: b,
    });
    for (const v of b.vars)
      if (varNode(v))
        edges.push({
          id: `d:${id}|${v}`,
          source: id,
          target: `v:${v}`,
          label: '',
          style: 'dotted',
          optional: false,
          at: b.range.from,
        });
  });
  // filters: a chip on the node, or a chip node joined to every variable it mentions. A
  // filter keeps its scope: it goes on a variable only when the variable sits in the same
  // hull (or in none, for a filter of the root), else it is a node in its OPTIONAL's hull
  const chips = new Map<string, number>();
  for (const { filter, group } of filtersOf(p)) {
    const optional = group.kind === 'optional';
    const hull = optional ? hullOf.get(group) : undefined;
    const n = filter.expr ? varNode(filter.expr.var) : undefined;
    if (filter.expr && n && !n.fromLocked && n.parent === hull && (hull || !optional)) {
      n.subtitle.push(chipText(filter.expr));
      continue;
    }
    const h = hash(normalized(filter.text));
    const k = chips.get(h) ?? 0;
    chips.set(h, k + 1);
    const id = `c:${h}:${k}`;
    const label = first(filter.text, 40);
    nodes.set(id, {
      id,
      kind: 'chip',
      label: optional && !hull ? `OPTIONAL · ${label}` : label,
      subtitle: [],
      title: filter.text,
      at: filter.range.from,
      ...(hull ? { parent: hull } : {}),
      locked: {
        kind: 'FILTER',
        vars: filter.vars,
        text: filter.text,
        range: filter.range,
      },
    });
    for (const v of filter.vars)
      if (varNode(v))
        edges.push({
          id: `d:${id}|${v}`,
          source: id,
          target: `v:${v}`,
          label: '',
          style: 'dotted',
          optional: false,
          at: filter.range.from,
        });
  }
  // an OPTIONAL whose nodes the required part binds has no hull: its edges stay dashed
  const used = new Set([...nodes.values()].map((n) => n.parent));
  return {
    nodes: [...nodes.values()],
    edges,
    hulls: hulls.filter((h) => used.has(h.id)),
    parts: parts(
      // a variable only a locked block binds is drawn beside it, not as a part of its own
      [...nodes.values()].filter((n) => n.kind !== 'locked' && n.kind !== 'chip' && !n.fromLocked),
      edges.filter((e) => e.style !== 'dotted'),
    ),
    triples: placed.length,
  };
}

/** The number of connected parts of a graph. */
function parts(nodes: PNode[], edges: PEdge[]): number {
  const up = new Map(nodes.map((n) => [n.id, n.id]));
  const root = (k: string): string => {
    let r = k;
    while (up.get(r) !== r) r = up.get(r)!;
    up.set(k, r);
    return r;
  };
  for (const e of edges)
    if (up.has(e.source) && up.has(e.target)) up.set(root(e.source), root(e.target));
  return new Set(nodes.map((n) => root(n.id))).size;
}

/** The elements of a drawing in text order, for the keyboard and the list (§2.5). */
export function textOrder(d: Drawing): ({ node: PNode } | { edge: PEdge })[] {
  const all: { at: number; el: { node: PNode } | { edge: PEdge } }[] = [
    ...d.nodes.map((node) => ({ at: node.at, el: { node } })),
    ...d.edges.filter((e) => e.style !== 'dotted').map((edge) => ({ at: edge.at, el: { edge } })),
  ];
  return all.sort((a, b) => a.at - b.at).map((x) => x.el);
}

/** How an element is announced: "edge ex:worksFor from ?person to ?org, optional". */
export function describe(el: { node: PNode } | { edge: PEdge }, d: Drawing): string {
  if ('edge' in el) {
    const e = el.edge;
    const label = (id: string) => d.nodes.find((n) => n.id === id)?.label ?? id;
    return `edge ${e.label} from ${label(e.source)} to ${label(e.target)}${e.optional ? ', optional' : ''}`;
  }
  const n = el.node;
  switch (n.kind) {
    case 'locked':
      return `locked ${n.locked!.kind}: ${first(n.locked!.text)}`;
    case 'chip':
      return `locked filter: ${first(n.title)}`;
    case 'var':
      return `variable ${n.label}${n.subtitle.length ? `, ${n.subtitle.join(', ')}` : ''}`;
    case 'literal':
      return `literal ${n.label}`;
    case 'bnode':
      return `blank node ${n.label}`;
    default:
      return `IRI ${n.label}${n.subtitle.length ? `, ${n.subtitle.join(', ')}` : ''}`;
  }
}

/**
 * A variable renamed in the editor (§4.4): when exactly one variable key disappeared and
 * one appeared, and the two have the same incident edges (by predicate and the keys of
 * the other ends), every key that moves: the variable's, and those of the literals it is
 * the subject of, whose keys hold the subject's (`l:v:x|…`). Null when there is no rename.
 */
export function renamed(before: Drawing, after: Drawing): [string, string][] | null {
  const r = renamedVar(before, after);
  if (!r) return null;
  const [o, n] = r;
  const out: [string, string][] = [[o, n]];
  const have = new Set(after.nodes.map((x) => x.id));
  for (const x of before.nodes)
    if (x.id.startsWith(`l:${o}|`)) {
      const moved = `l:${n}|${x.id.slice(o.length + 3)}`;
      if (have.has(moved)) out.push([x.id, moved]);
    }
  return out;
}

function renamedVar(before: Drawing, after: Drawing): [string, string] | null {
  const vars = (d: Drawing) => new Set(d.nodes.filter((n) => n.kind === 'var').map((n) => n.id));
  const a = vars(before);
  const b = vars(after);
  const gone = [...a].filter((k) => !b.has(k));
  const added = [...b].filter((k) => !a.has(k));
  if (gone.length !== 1 || added.length !== 1) return null;
  const [o, n] = [gone[0], added[0]];
  const sig = (d: Drawing, k: string, swap: (x: string) => string) =>
    d.edges
      .filter((e) => e.style !== 'dotted' && (e.source === k || e.target === k))
      .map((e) =>
        e.source === k ? `out ${e.label} ${swap(e.target)}` : `in ${e.label} ${swap(e.source)}`,
      )
      .sort()
      .join('\n');
  // a literal's key holds its subject's
  const same = (x: string) =>
    x === o ? n : x.startsWith(`l:${o}|`) ? `l:${n}|${x.slice(o.length + 3)}` : x;
  return sig(before, o, same) === sig(after, n, (x) => x) ? [o, n] : null;
}

// ----------------------------------------------------------- splices (§4.3) ------

/** Apply changes (in the old text's coordinates, not overlapping) to `text`. */
export function applyChanges(text: string, changes: Change[]): string {
  const sorted = [...changes].sort((a, b) => a.from - b.from || a.to - b.to);
  let out = '';
  let at = 0;
  for (const c of sorted) {
    if (c.from < at) throw new Error('overlapping changes');
    out += text.slice(at, c.from) + c.insert;
    at = c.to;
  }
  return out + text.slice(at);
}

/** The indentation of the line `offset` is on. */
function indentAt(text: string, offset: number): string {
  const start = text.lastIndexOf('\n', offset - 1) + 1;
  return /^[ \t]*/.exec(text.slice(start))![0];
}

/** The end of the spaces and the `#` comment that end the line at `offset`, if any. */
function lineTail(text: string, offset: number): number {
  const m = /^[ \t]*#[^\r\n]*/.exec(text.slice(offset));
  return m ? offset + m[0].length : offset;
}

/** Whether only spaces precede `offset` on its line; then the offset of the line break
 * before it (or the start of the text). */
function blankBefore(text: string, offset: number): number | null {
  const nl = text.lastIndexOf('\n', offset - 1);
  const before = text.slice(nl + 1, offset);
  if (!/^[ \t]*$/.test(before)) return null;
  // the `\r` of a CRLF goes with its `\n`
  return nl > 0 && text[nl - 1] === '\r' ? nl - 1 : Math.max(nl, 0);
}

/** The indentation for an item inside `g`. */
function itemIndent(text: string, g: Group): string {
  const first = g.items[0];
  if (first) {
    const r = itemRange(first);
    if (blankBefore(text, r.from) !== null) return indentAt(text, r.from);
  }
  return indentAt(text, g.range.from) + '  ';
}

const itemRange = (i: Item): Range => i.range;

/** The variables a triple binds. */
function bindsOf(s: Term, p: { kind: string; value: string }, o: Term): Set<string> {
  const v = new Set<string>();
  if (s.kind === 'var') v.add(s.value);
  if (p.kind === 'var') v.add(p.value);
  if (o.kind === 'var') v.add(o.value);
  return v;
}

/**
 * Where a new item goes in group `g` (§4.2 rule 2): after the last editable item, and
 * after any locked block that follows it and uses a variable the new item binds. The
 * items of `skip` (being moved away) do not count. Returns the item it follows, or null
 * for the start of the group.
 */
function anchor(g: Group, binds: Set<string>, skip: Set<Item> = new Set()): Item | null {
  const items = g.items.filter((i) => !skip.has(i));
  let last = -1;
  items.forEach((i, n) => {
    if (i.type !== 'locked') last = n;
  });
  let at = last;
  for (let n = last + 1; n < items.length; n++) {
    const i = items[n] as Locked;
    if (i.vars.some((v) => binds.has(v))) at = n;
  }
  return at >= 0 ? items[at] : null;
}

/**
 * The changes that put `fragment` (an item, such as `?s ex:p ?o .`) into group `g` on its
 * own line, after `after` (an item of `g`) or at the start of the group. A triple
 * statement before it without its `.` gets one, since triples need a `.` between them.
 */
function insertItem(
  text: string,
  g: Group,
  after: Item | null,
  fragment: string,
  isTriple: boolean,
): Change[] {
  const indent = itemIndent(text, g);
  const changes: Change[] = [];
  let at: number;
  if (after) {
    let end = itemRange(after).to;
    if (after.type === 'triple' || (after.type === 'locked' && after.kind === 'triples')) {
      // a statement that ends without its `.` (which it holds), before a triple statement
      const last = after.type === 'locked' ? (after.end ?? end) : end;
      const dotted = text.slice(after.range.from, last).trimEnd().endsWith('.');
      if (!dotted && isTriple) {
        if (last === end) changes.push({ from: end, to: end, insert: ' .' });
        // a locked statement's range runs over its trailing comment, which no byte may be
        // written into, so the `.` opens the new item's line instead
        else fragment = `. ${fragment}`;
      }
    } else if (after.dot) {
      // the `.` that follows another element, maybe after a comment or a line break
      end = after.dot.to;
    }
    at = lineTail(text, end);
  } else {
    at = lineTail(text, g.inner.from);
  }
  // what follows on the same line (another item, or the closing brace) goes on a line of
  // its own after the new item
  const line = /^[^\r\n]*/.exec(text.slice(at))![0];
  let after_ = '';
  if (line.trim() !== '') {
    const closing = at + line.length - line.trimStart().length >= g.inner.to;
    after_ = `\n${closing ? indentAt(text, g.range.from) : indent}`;
  }
  changes.push({ from: at, to: at, insert: `\n${indent}${fragment}${after_}` });
  return changes;
}

/** The range a triple's removal deletes from its statement (R2's named range). */
export function removalRange(text: string, placed: Placed): Range {
  const { triple, path } = placed;
  const g = path[path.length - 1];
  const siblings = g.items.filter(
    (i): i is Triple => i.type === 'triple' && i.stmt === triple.stmt,
  );
  if (siblings.length === 1) {
    // the whole statement, with its line when it is alone on it
    const r = triple.range;
    const end = lineTail(text, r.to);
    const start = blankBefore(text, r.from);
    if (start !== null && /^[ \t]*(\r?\n|$)/.test(text.slice(end))) return { from: start, to: end };
    return { from: r.from, to: r.to };
  }
  const entry = siblings.filter((t) => t.entry === triple.entry);
  if (entry.length > 1) {
    // one object of a `,` list
    const k = entry.indexOf(triple);
    if (k < entry.length - 1)
      return {
        from: triple.objectRange.from,
        to: entry[k + 1].objectRange.from,
      };
    return { from: entry[k - 1].oRange.to, to: triple.objectRange.to };
  }
  // one entry of a `;` list
  const entries = [...new Set(siblings.map((t) => t.entry))];
  const k = entries.indexOf(triple.entry);
  if (k < entries.length - 1) {
    const next = siblings.find((t) => t.entry === entries[k + 1])!;
    return { from: triple.entryRange.from, to: next.entryRange.from };
  }
  // the last entry: from the end of the one before (before its `;`)
  const prev = siblings.filter((t) => t.entry === entries[k - 1]);
  return {
    from: prev[prev.length - 1].objectRange.to,
    to: triple.entryRange.to,
  };
}

/** The text of a triple pattern as a statement on its own. */
export const statement = (s: string, p: string, o: string) => `${s} ${p} ${o} .`;

/** The edit that adds a triple, and the group it goes in. */
export type AddTriple = {
  kind: 'add';
  s: string;
  p: string;
  o: string;
  /** PREFIX lines to add first, for prefixed names the query does not declare */
  declare?: { prefix: string; iri: string }[];
};

/** The edit that deletes a triple, or toggles OPTIONAL on it. The triple is named by the
 * id of its edge, which is resolved against the drawing of the text being edited, so an
 * edit never lands on another triple when the text moved since the selection. */
export type TripleEdit = { kind: 'delete' | 'optional'; edge: string };

export type Edit = AddTriple | TripleEdit;

/** Why an edit cannot be made. */
export class EditError extends Error {}

const IRIREF = String.raw`<[^<>"{}|^${'`'}\\\s]*>`;
// a prefixed name as SPARQL writes it: dots inside the prefix and the local part, not at
// their ends; escapes and odd characters are left to the strict parse of the result
const PNAME = String.raw`(?:[\p{L}](?:[\p{L}\p{N}_.\-]*[\p{L}\p{N}_\-])?)?:(?:[\p{L}\p{N}_:%\-](?:[\p{L}\p{N}_:%.\-]*[\p{L}\p{N}_:%\-])?)?`;
const TAG = String.raw`(@[a-zA-Z]+(-[a-zA-Z0-9]+)*|\^\^(${IRIREF}|${PNAME}))?`;
const IRI_RE = new RegExp(`^${IRIREF}$`);
const PNAME_RE = new RegExp(`^${PNAME}$`, 'u');
const DQ_RE = new RegExp(String.raw`^"([^"\\\n\r]|\\.)*"${TAG}$`, 'u');
const SQ_RE = new RegExp(String.raw`^'([^'\\\n\r]|\\.)*'${TAG}$`, 'u');

/** A term as it is written in a triple, checked: a variable, an IRI, a prefixed name, a
 * labelled blank node, `a` for a predicate, or a literal for an object, whose datatype is
 * exactly one IRI or prefixed name. */
export function checkTerm(text: string, role: 's' | 'p' | 'o'): string {
  const t = text.trim();
  const ok =
    /^[?$][\p{L}\p{N}_]+$/u.test(t) ||
    IRI_RE.test(t) ||
    PNAME_RE.test(t) ||
    (role !== 'p' && /^_:[\p{L}\p{N}_]+$/u.test(t)) ||
    (role === 'p' && t === 'a') ||
    (role === 'o' &&
      (DQ_RE.test(t) ||
        SQ_RE.test(t) ||
        /^[+-]?(\d+\.?\d*([eE][+-]?\d+)?|\.\d+([eE][+-]?\d+)?)$/.test(t) ||
        t === 'true' ||
        t === 'false'));
  if (!ok) {
    const what = role === 's' ? 'subject' : role === 'p' ? 'predicate' : 'object';
    throw new EditError(`"${t}" is not a ${what} the builder writes`);
  }
  return t;
}

type Prefixes = { prefix: string; iri: string }[];

/** The prefix of a prefixed name, or null for another term. */
const prefixOf = (t: string): string | null =>
  !/^[?$_<"'+\-.\d]/.test(t) && PNAME_RE.test(t) ? t.slice(0, t.indexOf(':')) : null;

/** The prefixes a checked term uses: its own, or its datatype's (`"1"^^xsd:int`). */
export function prefixesIn(term: string): string[] {
  const own = prefixOf(term);
  if (own !== null) return [own];
  const dt = /\^\^([^<].*)$/u.exec(term)?.[1];
  const p = dt !== undefined ? prefixOf(dt) : null;
  return p !== null ? [p] : [];
}

/**
 * The PREFIX lines an add needs for its terms: each prefix they use that the query does
 * not declare and the dataset does, the empty prefix of `:p` included, once.
 */
export function missingPrefixes(
  terms: string[],
  p: PatternOk,
  dataset: Record<string, string>,
): Prefixes {
  const declared = new Set(p.prologue.prefixes.map((x) => x.prefix));
  const out: Prefixes = [];
  for (const t of terms)
    for (const prefix of prefixesIn(t.trim()))
      if (
        !declared.has(prefix) &&
        Object.hasOwn(dataset, prefix) &&
        !out.some((x) => x.prefix === prefix)
      )
        out.push({ prefix, iri: dataset[prefix] });
  return out;
}

/** The parts of an IRI reference (RFC 3986 §3), without normalization. */
function iriParts(iri: string) {
  const m =
    /^(?:([a-zA-Z][a-zA-Z0-9+.-]*):)?(?:\/\/([^/?#]*))?([^?#]*)(?:\?([^#]*))?(?:#(.*))?$/u.exec(
      iri,
    )!;
  return { scheme: m[1], authority: m[2], path: m[3], query: m[4], fragment: m[5] };
}

/** RFC 3986 §5.2.4. */
function removeDots(path: string): string {
  let input = path;
  let out = '';
  while (input) {
    if (input.startsWith('../')) input = input.slice(3);
    else if (input.startsWith('./')) input = input.slice(2);
    else if (input.startsWith('/./')) input = input.slice(2);
    else if (input === '/.') input = '/';
    else if (input.startsWith('/../') || input === '/..') {
      input = '/' + input.slice(input === '/..' ? 3 : 4);
      out = out.slice(0, Math.max(out.lastIndexOf('/'), 0));
    } else if (input === '.' || input === '..') input = '';
    else {
      const m = /^\/?[^/]*/.exec(input)![0];
      out += m;
      input = input.slice(m.length);
    }
  }
  return out;
}

/**
 * An IRI resolved against the query's BASE as the parser resolves it (RFC 3986 §5.2, as
 * oxiri does): no percent-encoding and no normalization of the host or port, which the
 * WHATWG `URL` would add.
 */
export function resolveIri(iri: string, base: string | null | undefined): string {
  if (!base) return iri;
  const r = iriParts(iri);
  if (r.scheme !== undefined) return iri;
  const b = iriParts(base);
  // a BASE that is not absolute resolves nothing, as in the reader
  if (b.scheme === undefined) return iri;
  let authority = b.authority;
  let path: string;
  let query = r.query;
  if (r.authority !== undefined) {
    authority = r.authority;
    path = removeDots(r.path);
  } else if (r.path === '') {
    path = b.path;
    if (query === undefined) query = b.query;
  } else if (r.path.startsWith('/')) path = removeDots(r.path);
  else {
    const merged =
      b.authority !== undefined && b.path === ''
        ? `/${r.path}`
        : b.path.slice(0, b.path.lastIndexOf('/') + 1) + r.path;
    path = removeDots(merged);
  }
  return (
    `${b.scheme}:` +
    (authority !== undefined ? `//${authority}` : '') +
    path +
    (query !== undefined ? `?${query}` : '') +
    (r.fragment !== undefined ? `#${r.fragment}` : '')
  );
}

/**
 * A checked term as the parse gives it: a variable's name, a blank node's label, a full
 * IRI (a prefixed name expanded under the query's prefixes and then `extra`, an IRI
 * resolved against BASE, `a` as rdf:type), or a literal as written.
 */
export function resolveTerm(t: string, p: PatternOk, extra: Prefixes = []): Term {
  const range = { from: 0, to: 0 };
  if (/^[?$]/.test(t)) return { kind: 'var', text: t, value: t.slice(1), range };
  if (t.startsWith('_:')) return { kind: 'bnode', text: t, value: t.slice(2), range };
  if (t === 'a') return { kind: 'iri', text: t, value: RDF_TYPE, range };
  if (t.startsWith('<'))
    return {
      kind: 'iri',
      text: t,
      value: resolveIri(t.slice(1, -1), p.prologue.base),
      range,
    };
  const prefix = prefixOf(t);
  if (prefix !== null) {
    // a later declaration of the same prefix wins, as in the parser
    const ns = [...p.prologue.prefixes, ...extra].filter((x) => x.prefix === prefix).pop();
    const local = t.slice(prefix.length + 1);
    return {
      kind: 'iri',
      text: t,
      value: (ns?.iri ?? `${prefix}:`) + local,
      range,
    };
  }
  return { kind: 'literal', text: t, value: t, range };
}

/**
 * The changes an edit makes to `text`, whose pattern is `p`:
 *
 * - `add` puts `s p o .` on a new line in the group that holds the subject's patterns
 *   (the root when none does), after the last editable item and after a following locked
 *   block that uses a variable it binds (§4.2 rule 2);
 * - `delete` removes the triple from its statement: the object of a `,` list, the entry
 *   of a `;` list, or the statement with its line;
 * - `optional` moves the triple into `OPTIONAL { s p o . }` in its group, placed like a
 *   new triple, or back out of its OPTIONAL into the group around it, deleting the
 *   OPTIONAL when nothing else is left in it.
 */
export function edit(text: string, p: PatternOk, e: Edit): Change[] {
  if (!p.root) throw new EditError('this query has no WHERE group to edit');
  if (e.kind === 'add') {
    const s = checkTerm(e.s, 's');
    const pr = checkTerm(e.p, 'p');
    const o = checkTerm(e.o, 'o');
    const declare = declarations(p, e);
    // a prefixed name needs its prefix declared by the query or by the edit
    for (const prefix of [s, pr, o].flatMap(prefixesIn))
      if (
        !p.prologue.prefixes.some((x) => x.prefix === prefix) &&
        !declare.some((x) => x.prefix === prefix)
      )
        throw new EditError(`The prefix "${prefix}:" is not declared`);
    const subject = resolveTerm(s, p, declare);
    const binds = bindsOf(subject, resolveTerm(pr, p, declare), resolveTerm(o, p, declare));
    const home = homeOf(p, s, declare);
    const g = home ? home.path[home.path.length - 1] : p.root;
    const changes = insertItem(text, g, anchor(g, binds), statement(s, pr, o), true);
    for (const { prefix, iri } of declare)
      changes.push({ from: 0, to: 0, insert: `PREFIX ${prefix}: <${iri}>\n` });
    return changes;
  }
  const placed = resolveEdit(p, e.edge);
  if (!placed) throw new EditError('The pattern changed, select the edge again');
  const { triple, path } = placed;
  const g = path[path.length - 1];
  const removal = removalRange(text, placed);
  const del: Change = { from: removal.from, to: removal.to, insert: '' };
  if (e.kind === 'delete') return [del];
  const binds = bindsOf(triple.s, triple.p, triple.o);
  const words = statement(triple.s.text, triple.p.text, triple.o.text);
  if (g.kind === 'group') {
    // into an OPTIONAL at the end of its group; the statement may be the anchor
    const alone = g.items.filter((i) => i.type === 'triple' && i.stmt === triple.stmt).length === 1;
    const skip = new Set<Item>(alone ? [triple] : []);
    // a statement that keeps other triples can be the anchor: the OPTIONAL follows it
    const after = anchor(g, binds, skip);
    const changes = insertItem(text, g, after, `OPTIONAL { ${words} }`, false);
    return merge(del, changes);
  }
  // out of its OPTIONAL into the group around it
  const parent = path[path.length - 2];
  const optItem = parent.items.find((i) => i.type === 'group' && i.range.from === g.range.from)!;
  const empty = g.items.length === 1;
  const skip = new Set<Item>(empty ? [optItem] : []);
  const after = anchor(parent, binds, skip);
  const changes = insertItem(text, parent, after, words, true);
  if (empty) {
    const dot = optItem.type === 'group' ? optItem.dot : null;
    return merge(optionalRemoval(text, optItem.range, dot), changes);
  }
  return merge(del, changes);
}

/** The PREFIX lines an add makes: its declarations the query lacks, each prefix once. */
function declarations(p: PatternOk, e: AddTriple): Prefixes {
  const out: Prefixes = [];
  for (const d of e.declare ?? [])
    if (
      !p.prologue.prefixes.some((x) => x.prefix === d.prefix) &&
      !out.some((x) => x.prefix === d.prefix)
    )
      out.push(d);
  return out;
}

/**
 * Whether an edge id taken from drawing `before` still names the same triple in `after`,
 * the drawing of the text after typing: the id must be in both, unambiguous in both (its
 * occurrence number moves when another copy of the same triple changes), and on the same
 * side of an OPTIONAL. Otherwise the selection is dropped and an edit is refused.
 */
export function stillNames(before: Drawing, after: Drawing, edge: string): boolean {
  const base = edge.slice(0, edge.lastIndexOf('|') + 1);
  const copies = (d: Drawing) => d.edges.filter((x) => x.id.startsWith(base) && x.triple);
  const a = copies(before);
  const b = copies(after);
  if (a.length !== 1 || b.length !== 1) return false;
  return a[0].id === edge && b[0].id === edge && a[0].optional === b[0].optional;
}

/**
 * The first triple that mentions the subject of an add, whose group the add goes in. A
 * term matches when it is written the same way, which needs no resolution and so holds
 * even for a prefix only the add declares, or when it resolves to the same term.
 */
function homeOf(p: PatternOk, s: string, declare: Prefixes): Placed | undefined {
  const key = termKey(resolveTerm(s, p, declare));
  const same = (t: Term) => (t.kind !== 'literal' && t.text === s) || termKey(t) === key;
  return triplesOf(p).find((t) => same(t.triple.s) || same(t.triple.o));
}

/** The triple whose edge has this id in the drawing of `p`. */
export function resolveEdit(p: PatternOk, edge: string): Placed | undefined {
  return drawing(p).edges.find((x) => x.id === edge)?.triple;
}

/** The deletions that remove an OPTIONAL: its range, with its line when alone on it and no
 * `.` follows on that line, and the `.` after it in its group as a deletion of its own,
 * which keeps a comment or line break between the two. */
function optionalRemoval(text: string, r: Range, dot: Range | null | undefined): Change[] {
  const out: Change[] = [];
  const to = lineTail(text, r.to);
  const start = blankBefore(text, r.from);
  const dotOnLine = !!dot && !/[\r\n]/.test(text.slice(r.to, dot.from));
  if (dotOnLine) {
    // `OPTIONAL { … } .` with its line, or just the two
    const end = lineTail(text, dot!.to);
    if (start !== null && /^[ \t]*(\r?\n|$)/.test(text.slice(end)))
      out.push({ from: start, to: end, insert: '' });
    else out.push({ from: r.from, to: dot!.to, insert: '' });
    return out;
  }
  if (start !== null && /^[ \t]*(\r?\n|$)/.test(text.slice(to)))
    out.push({ from: start, to, insert: '' });
  else out.push({ from: r.from, to: r.to, insert: '' });
  if (dot) out.push({ from: dot.from, to: dot.to, insert: '' });
  return out;
}

/**
 * Deletions and insertions as one list of non-overlapping changes: an insertion inside a
 * deleted range moves to its end, where the deleted text was.
 */
function merge(dels: Change | Change[], inserts: Change[]): Change[] {
  const list = Array.isArray(dels) ? dels : [dels];
  return [
    ...list,
    ...inserts.map((c) => {
      const d = list.find((d) => c.from > d.from && c.from < d.to);
      return d ? { ...c, from: d.to, to: d.to } : c;
    }),
  ];
}

/**
 * A triple by its terms as written, for comparing two parses (R3). The splices never
 * rewrite a term, and the only prologue change is a PREFIX line for a prefix the query did
 * not declare, so the same text means the same term on both sides, while resolved values
 * would differ for exactly the names that line declares.
 */
function signature(s: { text: string }, p: { text: string }, o: { text: string }): string {
  return `${s.text} ${p.text} ${o.text}`;
}

/**
 * Whether the parse after an edit agrees with the edit (R3): after `add` exactly one more
 * triple, with the edit's terms; after `delete` exactly that triple fewer; after
 * `optional` the same triples with that one moved in or out of an OPTIONAL. The pane
 * refuses an edit whose result does not agree.
 */
export function agrees(before: PatternOk, e: Edit, after: PatternOk): boolean {
  const bag = (q: PatternOk) => {
    const m = new Map<string, number>();
    for (const t of triplesOf(q)) {
      const k = `${signature(t.triple.s, t.triple.p, t.triple.o)}|${t.path.length > 1}`;
      m.set(k, (m.get(k) ?? 0) + 1);
    }
    return m;
  };
  const a = bag(before);
  const b = bag(after);
  const diff = (x: Map<string, number>, y: Map<string, number>) => {
    const out: string[] = [];
    for (const [k, n] of x) for (let i = y.get(k) ?? 0; i < n; i++) out.push(k);
    return out;
  };
  const added = diff(b, a);
  const removed = diff(a, b);
  if (e.kind === 'add') {
    const s = e.s.trim();
    const want = signature({ text: s }, { text: e.p.trim() }, { text: e.o.trim() });
    // on the side of an OPTIONAL its subject's patterns are on (§2.2)
    const home = homeOf(before, s, declarations(before, e));
    const optional = !!home && home.path.length > 1;
    return removed.length === 0 && added.length === 1 && added[0] === `${want}|${optional}`;
  }
  const placed = resolveEdit(before, e.edge);
  if (!placed) return false;
  const sig = signature(placed.triple.s, placed.triple.p, placed.triple.o);
  const opt = placed.path.length > 1;
  if (e.kind === 'delete')
    return added.length === 0 && removed.length === 1 && removed[0] === `${sig}|${opt}`;
  return (
    removed.length === 1 &&
    added.length === 1 &&
    removed[0] === `${sig}|${opt}` &&
    added[0] === `${sig}|${!opt}`
  );
}

/** What an edit is announced as: "Added ?person ex:worksFor ?org, 3 nodes, 2 edges". */
export function announcement(e: Edit, p: PatternOk, after: Drawing): string {
  const counts = `${after.nodes.filter((n) => n.kind !== 'chip').length} nodes, ${after.edges.filter((x) => x.style !== 'dotted').length} edges`;
  if (e.kind === 'add') return `Added ${e.s.trim()} ${e.p.trim()} ${e.o.trim()}, ${counts}`;
  const t = resolveEdit(p, e.edge)?.triple;
  const words = t ? `${t.s.text} ${t.p.text} ${t.o.text}` : 'the triple';
  if (e.kind === 'delete') return `Deleted ${words}, ${counts}`;
  return `Toggled optional on ${words}, ${counts}`;
}

/** The drawing caps of §4.6: above them the pane lists the patterns instead. */
export const CAPS = { triples: 300, locked: 100, nodes: 400 };

/** The texts the pane parses at most on every change (§4.6). */
export const MAX_TEXT = 256 * 1024;

/** Whether a drawing is over the caps. */
export function overCaps(p: PatternOk, d: Drawing): boolean {
  return (
    d.triples > CAPS.triples || lockedOf(p).length > CAPS.locked || d.nodes.length > CAPS.nodes
  );
}
