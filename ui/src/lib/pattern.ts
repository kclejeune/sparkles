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
};

export type Locked = {
  kind: string;
  vars: string[];
  /** exactly the text at `range` */
  text: string;
  range: Range;
};

export type Group = {
  kind: 'group' | 'optional';
  range: Range;
  /** between the braces */
  inner: Range;
  items: Item[];
};

export type Item = Triple | Filter | (Group & { type: 'group' }) | (Locked & { type: 'locked' });

export type PatternOk = {
  ok: true;
  form: 'select' | 'ask' | 'construct' | 'describe';
  prologue: { prefixes: { prefix: string; iri: string; range: Range }[] };
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
export type Placed = { triple: Triple; path: Group[]; index: number; order: number };

/** Every triple of the pattern in text order, with its groups. */
export function triplesOf(p: PatternOk): Placed[] {
  const out: Placed[] = [];
  const walk = (g: Group, path: Group[]) => {
    g.items.forEach((item, index) => {
      if (item.type === 'triple')
        out.push({ triple: item, path: [...path, g], index, order: out.length });
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

/** Every filter of the pattern in text order, with whether it is inside an OPTIONAL. */
export function filtersOf(p: PatternOk): { filter: Filter; optional: boolean }[] {
  const out: { filter: Filter; optional: boolean }[] = [];
  const walk = (g: Group, optional: boolean) => {
    for (const item of g.items) {
      if (item.type === 'filter') out.push({ filter: item, optional });
      else if (item.type === 'group') walk(item, true);
    }
  };
  if (p.root) walk(p.root, false);
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
    edges.push({
      id: `e:${s}|${triple.p.text}|${o}|${edges.filter((e) => e.source === s && e.target === o && e.label === label).length}`,
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
  const blocks = lockedOf(p);
  lockedKeys(blocks).forEach((id, i) => {
    const b = blocks[i];
    // a block that binds variables (a BIND, a subquery, VALUES …) gets nodes for them, so
    // the reader sees where `?y` comes from; a MINUS binds nothing outside itself
    if (b.kind !== 'MINUS')
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
  // filters: a chip on the node, or a locked chip joined to every variable it mentions
  const chips = new Map<string, number>();
  for (const { filter } of filtersOf(p)) {
    const n = filter.expr ? varNode(filter.expr.var) : undefined;
    if (filter.expr && n) {
      n.subtitle.push(chipText(filter.expr));
      continue;
    }
    const h = hash(normalized(filter.text));
    const k = chips.get(h) ?? 0;
    chips.set(h, k + 1);
    const id = `c:${h}:${k}`;
    nodes.set(id, {
      id,
      kind: 'chip',
      label: first(filter.text, 40),
      subtitle: [],
      title: filter.text,
      at: filter.range.from,
      locked: { kind: 'FILTER', vars: filter.vars, text: filter.text, range: filter.range },
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
  return {
    nodes: [...nodes.values()],
    edges,
    hulls,
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
 * the other ends), the old key and the new one.
 */
export function renamed(before: Drawing, after: Drawing): [string, string] | null {
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
  const same = (x: string) => (x === o ? n : x);
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
    // the `.` that may follow an element that is not a triple statement
    const dot = after.type === 'triple' ? null : /^[ \t]*\./.exec(text.slice(end));
    if (dot) end += dot[0].length;
    // a statement that ends without its `.`, followed by a triple statement
    const stmtEnd = after.type === 'triple' && !text.slice(0, end).trimEnd().endsWith('.');
    if (stmtEnd && isTriple) changes.push({ from: end, to: end, insert: ' .' });
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
      return { from: triple.objectRange.from, to: entry[k + 1].objectRange.from };
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
  return { from: prev[prev.length - 1].objectRange.to, to: triple.entryRange.to };
}

/** The text of a triple pattern as a statement on its own. */
export const statement = (s: string, p: string, o: string) => `${s} ${p} ${o} .`;

/** The edit that adds a triple, and the group it goes in. */
export type AddTriple = {
  kind: 'add';
  s: string;
  p: string;
  o: string;
  /** a PREFIX line to add first, for a prefixed name the query does not declare */
  declare?: { prefix: string; iri: string };
};

/** The edit that deletes a triple, or toggles OPTIONAL on it (by its index in `triplesOf`). */
export type TripleEdit = { kind: 'delete' | 'optional'; triple: number };

export type Edit = AddTriple | TripleEdit;

/** Why an edit cannot be made. */
export class EditError extends Error {}

/** A term as it is written in a triple, checked: a variable, an IRI, a prefixed name, a
 * labelled blank node, `a` for a predicate, or a literal for an object. */
export function checkTerm(text: string, role: 's' | 'p' | 'o'): string {
  const t = text.trim();
  const ok =
    /^[?$][\p{L}\p{N}_]+$/u.test(t) ||
    /^<[^<>"{}|^`\\\s]*>$/.test(t) ||
    /^[\p{L}][\p{L}\p{N}_.-]*:[\p{L}\p{N}_:%-]*$/u.test(t) ||
    /^:[\p{L}\p{N}_:%-]*$/u.test(t) ||
    (role !== 'p' && /^_:[\p{L}\p{N}_]+$/u.test(t)) ||
    (role === 'p' && t === 'a') ||
    (role === 'o' &&
      (/^"([^"\\\n]|\\.)*"(@[a-zA-Z]+(-[a-zA-Z0-9]+)*|\^\^\S+)?$/.test(t) ||
        /^'([^'\\\n]|\\.)*'(@[a-zA-Z]+(-[a-zA-Z0-9]+)*|\^\^\S+)?$/.test(t) ||
        /^[+-]?(\d+\.?\d*([eE][+-]?\d+)?|\.\d+([eE][+-]?\d+)?)$/.test(t) ||
        t === 'true' ||
        t === 'false'));
  if (!ok) {
    const what = role === 's' ? 'subject' : role === 'p' ? 'predicate' : 'object';
    throw new EditError(`"${t}" is not a ${what} the builder writes`);
  }
  return t;
}

const asTerm = (t: string, kind: Term['kind'] = 'iri'): Term => ({
  kind: /^[?$]/.test(t) ? 'var' : t.startsWith('_:') ? 'bnode' : kind,
  text: t,
  value: /^[?$]/.test(t) ? t.slice(1) : t.startsWith('_:') ? t.slice(2) : t,
  range: { from: 0, to: 0 },
});

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
    const subject = asTerm(s);
    const binds = bindsOf(subject, asTerm(pr), asTerm(o, 'literal'));
    // the group of the subject's patterns
    const key = termKey(subject);
    const home = triplesOf(p).find(
      (t) => termKey(t.triple.s) === key || termKey(t.triple.o) === key,
    );
    const g = home ? home.path[home.path.length - 1] : p.root;
    const changes = insertItem(text, g, anchor(g, binds), statement(s, pr, o), true);
    if (e.declare) {
      const { prefix, iri } = e.declare;
      if (!p.prologue.prefixes.some((x) => x.prefix === prefix))
        changes.push({ from: 0, to: 0, insert: `PREFIX ${prefix}: <${iri}>\n` });
    }
    return changes;
  }
  const all = triplesOf(p);
  const placed = all[e.triple];
  if (!placed) throw new EditError('no such triple');
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
    const r = optionalRemoval(text, g.range);
    return merge({ from: r.from, to: r.to, insert: '' }, changes);
  }
  return merge(del, changes);
}

/** The range an OPTIONAL's removal deletes: with its line when alone on it, and the `.`
 * after it in its group. */
function optionalRemoval(text: string, r: Range): Range {
  let to = r.to;
  const dot = /^[ \t]*\./.exec(text.slice(to));
  if (dot) to += dot[0].length;
  to = lineTail(text, to);
  const start = blankBefore(text, r.from);
  if (start !== null && /^[ \t]*(\r?\n|$)/.test(text.slice(to))) return { from: start, to };
  return { from: r.from, to };
}

/**
 * A deletion and insertions as one list of non-overlapping changes: an insertion inside
 * the deleted range moves to its end, where the deleted text was.
 */
function merge(del: Change, inserts: Change[]): Change[] {
  return [
    del,
    ...inserts.map((c) =>
      c.from > del.from && c.from < del.to ? { ...c, from: del.to, to: del.to } : c,
    ),
  ];
}

/** What an edit is announced as: "Added ?person ex:worksFor ?org, 3 nodes, 2 edges". */
export function announcement(e: Edit, p: PatternOk, after: Drawing): string {
  const counts = `${after.nodes.filter((n) => n.kind !== 'chip').length} nodes, ${after.edges.filter((x) => x.style !== 'dotted').length} edges`;
  if (e.kind === 'add') return `Added ${e.s} ${e.p} ${e.o}, ${counts}`;
  const t = triplesOf(p)[e.triple]?.triple;
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
